use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedSpec};
use crate::fingerprint::SpecFingerprint;
use crate::spec_hash;
use crate::Target;

/// Placeholder string spliced into the `hash = "..."` field of the
/// `#[qed(verified, ...)]` attribute during scaffold rendering. The
/// fixup pass at the end of `render_handler_scaffold` parses the
/// rendered impl method, computes the real body hash via
/// `body_hash_for_impl_fn`, and string-replaces this placeholder.
/// Picked to be obviously not a SHA-hex value so a missed fixup is
/// caught by the macro's "expected hash format" error rather than
/// silently shipping a placeholder.
const BODY_HASH_PLACEHOLDER: &str = "QEDGEN_FIXUP_BODY_HASH";

/// Per-framework strings for the surface that differs between Anchor
/// and Quasar codegen (imports, ctx type, return type, lifetime,
/// program-mod visibility, discriminator attribute).
///
/// All other generated content (`#[derive(Accounts)]` shape, account
/// constraints, `ctx.accounts.handler(...)` forwarder pattern, guard
/// module shape) is identical across the two — both frameworks support
/// the accounts-method forwarder idiom that the rest of the emitter
/// produces.
#[derive(Clone, Copy)]
struct FrameworkSurface {
    /// Crate-root attributes line, e.g. `"#![no_std]\n\n"`. Empty for
    /// targets that build against std.
    crate_attrs: &'static str,
    /// `"use anchor_lang::prelude::*;\n"` or
    /// `"use quasar_lang::prelude::*;\n"`. Caller appends the trailing
    /// blank line (some generators add additional imports first).
    prelude_import: &'static str,
    /// Type written as `<context_type>::<X>` in handler signatures —
    /// `"Context"` (Anchor) or `"Ctx"` (Quasar).
    context_type: &'static str,
    /// Handler return type — `"Result<()>"` (Anchor; the `Result`
    /// alias from `anchor_lang::prelude` defaults the error to
    /// `anchor_lang::error::Error`) or `"Result<(), ProgramError>"`
    /// (Quasar).
    handler_result_type: &'static str,
    /// Lifetime threaded into `#[derive(Accounts)]` structs and impl
    /// blocks. Anchor uses `"'info"`; Quasar's `Account<()>` doesn't
    /// need one and uses `""`.
    accounts_lifetime: &'static str,
    /// Visibility keyword for the `#[program]` mod — Anchor convention
    /// is `pub mod`, Quasar is bare `mod`.
    program_mod_vis: &'static str,
    /// True when each handler in the `#[program]` mod needs an
    /// `#[instruction(discriminator = N)]` attribute. Quasar requires
    /// it; Anchor auto-derives.
    explicit_handler_discriminator: bool,
    /// True when each `#[account]` struct in `state.rs` needs an
    /// explicit `discriminator = N` parameter (Quasar) vs Anchor's
    /// auto-derived form.
    explicit_account_discriminator: bool,
}

impl FrameworkSurface {
    fn for_target(target: Target) -> Self {
        match target {
            Target::Anchor => FrameworkSurface {
                crate_attrs: "",
                prelude_import: "use anchor_lang::prelude::*;\n",
                context_type: "Context",
                handler_result_type: "Result<()>",
                accounts_lifetime: "'info",
                program_mod_vis: "pub mod",
                explicit_handler_discriminator: false,
                explicit_account_discriminator: false,
            },
            Target::Quasar => FrameworkSurface {
                // `no_std` only for the on-chain (Solana/BPF) build. Host
                // builds (`cargo check`/`cargo test`) keep std so the host
                // gets a panic_handler / global_allocator from the standard
                // library. Quasar provides solana-target panic_handler /
                // global_allocator below via `panic_handler!()` / `no_alloc!()`.
                crate_attrs: "#![cfg_attr(any(target_os = \"solana\", target_arch = \"bpf\"), no_std)]\n\n",
                prelude_import: "use quasar_lang::prelude::*;\n",
                context_type: "Ctx",
                handler_result_type: "Result<(), ProgramError>",
                // Quasar's `#[derive(Accounts)]` expands to
                // `impl<'info> ParseAccounts<'info> for #name<'info>`,
                // so the user struct must carry `<'info>`. Field types
                // are references to wrappers (e.g. `&'info Signer`,
                // `&'info mut Account<T>`) per the canonical pattern in
                // `quasar_lang/tests/compile_fail/*.rs`.
                accounts_lifetime: "'info",
                program_mod_vis: "mod",
                explicit_handler_discriminator: true,
                explicit_account_discriminator: true,
            },
            Target::Pinocchio => {
                unreachable!("Pinocchio is rejected at the init dispatcher")
            }
        }
    }

    /// Render the lifetime parameter list for a `#[derive(Accounts)]`
    /// struct or impl block — e.g. `"<'info>"` (Anchor) or `""`
    /// (Quasar).
    fn lifetime_params(&self) -> String {
        if self.accounts_lifetime.is_empty() {
            String::new()
        } else {
            format!("<{}>", self.accounts_lifetime)
        }
    }
}

/// Render the Rust type for a `#[derive(Accounts)]` field for the
/// given target framework.
///
/// `is_state_account` is true when this account is the handler's
/// writable state holder (per `find_state_account`); in that case we
/// emit `Account<{state_name}>` (Quasar) or `Account<'info,
/// {state_name}>` (Anchor) so the field-access path
/// `self.<acct>.<field>` resolves through the typed inner data. For
/// non-state accounts we fall back to the framework's neutral
/// placeholder — `Account<()>` / `Signer` / `Program<()>` for Quasar,
/// `AccountInfo<'info>` / `Signer<'info>` / `Program<'info, System>`
/// for Anchor.
fn render_account_field_type(
    acct: &crate::check::ParsedHandlerAccount,
    surface: &FrameworkSurface,
    is_state_account: bool,
    state_name: &str,
) -> String {
    // Both Anchor and Quasar carry `'info` on field types; the divergence
    // is just inner-shape (Quasar uses `&'info` references to the wrapper,
    // Anchor uses bare typed wrappers parameterized by `'info`).
    if surface.context_type == "Ctx" {
        // Quasar branch — fields are references to wrappers.
        // Pattern from `quasar_lang/tests/compile_fail/*.rs`:
        //   `pub signer: &'info Signer` (read-only)
        //   `pub vault:  &'info mut Account<MyState>` (writable)
        // Writability mirrors the spec's `writable` flag.
        let lt = surface.accounts_lifetime;
        let mut_kw = if acct.is_writable { "mut " } else { "" };
        if acct.is_signer {
            format!("&{} {}Signer", lt, mut_kw)
        } else if acct.is_program {
            format!("&{} {}Program<System>", lt, mut_kw)
        } else if acct.account_type.as_deref() == Some("token") {
            format!("&{} {}Account<Token>", lt, mut_kw)
        } else if acct.account_type.as_deref() == Some("mint") {
            format!("&{} {}Account<Mint>", lt, mut_kw)
        } else if is_state_account {
            format!("&{} {}Account<{}>", lt, mut_kw, state_name)
        } else {
            format!("&{} {}UncheckedAccount", lt, mut_kw)
        }
    } else {
        // Anchor branch — every type carries `'info`.
        let lt = surface.accounts_lifetime;
        if acct.is_signer {
            format!("Signer<{}>", lt)
        } else if acct.is_program {
            format!("Program<{}, System>", lt)
        } else if acct.account_type.as_deref() == Some("token") {
            format!("Account<{}, TokenAccount>", lt)
        } else if acct.account_type.as_deref() == Some("mint") {
            format!("Account<{}, Mint>", lt)
        } else if is_state_account {
            format!("Account<{}, {}>", lt, state_name)
        } else {
            format!("AccountInfo<{}>", lt)
        }
    }
}

/// Compute a path, as a string, from a program `Cargo.toml` directory to the
/// spec file. This value is embedded verbatim in the `#[qed(spec = "...")]`
/// attribute and resolved at compile time relative to `CARGO_MANIFEST_DIR`.
///
/// Best-effort: if the spec isn't under a path we can express relatively,
/// fall back to the absolute path (works as long as the repo doesn't move).
fn relative_spec_path(spec_path: &Path, manifest_dir: &Path) -> String {
    // Canonicalize both; fall back to the raw paths on failure.
    let spec = spec_path
        .canonicalize()
        .unwrap_or_else(|_| spec_path.to_path_buf());
    let manifest = manifest_dir
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.to_path_buf());
    let spec_components: Vec<_> = spec.components().collect();
    let manifest_components: Vec<_> = manifest.components().collect();

    // Find common prefix length.
    let common = spec_components
        .iter()
        .zip(manifest_components.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = std::path::PathBuf::new();
    for _ in 0..(manifest_components.len().saturating_sub(common)) {
        out.push("..");
    }
    for comp in &spec_components[common..] {
        out.push(comp.as_os_str());
    }
    if out.as_os_str().is_empty() {
        spec.display().to_string()
    } else {
        out.to_string_lossy().replace('\\', "/")
    }
}

/// Map a DSL type to its Rust equivalent.
///
/// Handles:
///   - primitives (U8..U128, I8..I128, Bool, Pubkey),
///   - `Map[N] T` fixed-size containers (N = numeric literal or declared
///     constant; inner T recurses through this function) → `[T; N]`,
///   - `Fin[N]` → `usize` (index type with a bound; bound is informational),
///   - type aliases declared via `type Name = RHS` — resolved transitively,
///   - record type names (`type Foo = { ... }`) — returned as-is; the
///     generated Rust emits a corresponding `struct Foo { ... }` declaration
///     (see `emit_record_decls` in rust_codegen_util.rs),
///   - sum type names (`type Error | A | B | C`) — returned as-is; the
///     generated Rust emits a corresponding Rust enum (unit variants only;
///     payload variants are S3 narrow: name resolves but enum is flattened).
///
/// Returns an error for anything else, rather than silently passing it
/// through — the fall-through in v2.6.1 was the root cause of the codegen-
/// bug class where types like `U16` or `Map[N] UserAccount` leaked verbatim
/// into generated Rust (see docs/prds/PRD-v2.6.2.md G1).
pub fn map_type(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    let dsl_type = dsl_type.trim();

    // Compound type: Map[BOUND] T → [T; N]
    if let Some(rest) = dsl_type.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound_src = rest[..close].trim();
                let inner_src = rest[close + 1..].trim();
                let n = resolve_map_bound(bound_src, &spec.constants)?;
                let inner_rust = map_type(inner_src, spec)?;
                return Ok(format!("[{inner_rust}; {n}]"));
            }
        }
        anyhow::bail!(
            "malformed Map type `{}` — expected `Map[BOUND] T`",
            dsl_type
        );
    }

    // Fin[N] → usize. N is informational (bound for index-type safety in
    // the DSL); in Rust we just use usize.
    if let Some(rest) = dsl_type.strip_prefix("Fin") {
        let rest = rest.trim_start();
        if rest.starts_with('[') {
            return Ok("usize".to_string());
        }
    }

    // Primitive match — check first so `U8` etc. never hit the alias path.
    if let Some(rust) = primitive_map(dsl_type) {
        return Ok(rust.to_string());
    }

    // Type alias: `type Foo = Bar` — recurse on the RHS. Transitive.
    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return map_type(rhs, spec);
    }

    // Record type declared in the spec — return the name as-is. The generator
    // is responsible for emitting a `struct <Name> { ... }` alongside the
    // State struct.
    if spec.records.iter().any(|r| r.name == dsl_type) {
        return Ok(dsl_type.to_string());
    }

    // Sum type declared in the spec — return the name as-is. For S3 narrow,
    // only no-payload sums (Error-like enums) are fully supported; sums with
    // payload variants resolve by name but the generator flattens to a
    // primary variant's fields (see `resolve_state_fields`).
    if spec.sum_types.iter().any(|s| s.name == dsl_type) {
        return Ok(dsl_type.to_string());
    }

    anyhow::bail!(
        "unsupported DSL type `{}` — expected a primitive (U8/U16/U32/U64/U128, I8/I16/I32/I64/I128, Bool, Pubkey), a compound (Map[N] T, Fin[N]), or a user-defined type declared with `type` in the spec",
        dsl_type
    );
}

/// Map a DSL primitive name to its Rust equivalent, if one exists. Factored
/// out of `map_type` so both the primitive fast-path and the alias-recursion
/// base case can share it.
fn primitive_map(dsl_type: &str) -> Option<&'static str> {
    Some(match dsl_type {
        "Pubkey" => "Address",
        "U8" => "u8",
        "U16" => "u16",
        "U32" => "u32",
        "U64" => "u64",
        "U128" => "u128",
        "I8" => "i8",
        "I16" => "i16",
        "I32" => "i32",
        "I64" => "i64",
        "I128" => "i128",
        "Bool" => "bool",
        _ => return None,
    })
}

/// Resolve the bound expression inside `Map[BOUND] T`. Accepts either a
/// numeric literal (e.g. `Map[16] U64`) or a constant declared in the spec
/// (e.g. `Map[MAX_ACCOUNTS] U64`).
fn resolve_map_bound(bound: &str, constants: &[(String, String)]) -> Result<String> {
    let bound = bound.trim();
    if bound.chars().all(|c| c.is_ascii_digit()) && !bound.is_empty() {
        return Ok(bound.to_string());
    }
    match constants.iter().find(|(n, _)| n == bound) {
        Some((_, value)) => Ok(value.clone()),
        None => anyhow::bail!(
            "Map bound `{}` is not a numeric literal and not declared as a `const` in the spec",
            bound
        ),
    }
}

/// Sanitize a field-path string (e.g. `accounts[i].active`) into a legal
/// Rust identifier stem suitable for interpolation into `fn verify_*` names
/// and similar. Non-identifier characters become `_`; consecutive and
/// trailing `_` are collapsed.
///
/// Motivated by the v2.6.1 eval (percolator-prog, qedgen-bug-report §2):
/// subscripted effect targets like `accounts[i].active` landed verbatim
/// inside `format!("fn verify_{}_effect_{}", op.name, field)`, producing
/// Rust-illegal identifiers such as `verify_init_user_effect_accounts[i].active`.
pub fn sanitize_ident(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_underscore = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            prev_underscore = c == '_';
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Convert a snake_case operation name to PascalCase for struct names.
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + &chars.collect::<String>(),
            }
        })
        .collect()
}

/// Format the "GENERATED BY QEDGEN" marker with the per-file spec hash.
/// Thin wrapper around `crate::banner::banner` that resolves the hash from
/// the fingerprint table by file_key.
fn marker(label: &str, fp: &SpecFingerprint, file_key: &str) -> String {
    let hash = fp
        .file_hashes
        .get(file_key)
        .map(String::as_str)
        .unwrap_or("");
    crate::banner::banner(Some(label), hash)
}

// ============================================================================
// File generators
// ============================================================================

/// Generate src/lib.rs. Skip if the file already exists — once the user has
/// stamped custom imports or extra modules onto the crate shell, regenerating
/// it would silently clobber that edit. Paired with the per-handler
/// `instructions/<name>.rs` skip, this keeps `qedgen codegen` idempotent.
fn generate_lib(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let lib_path = src_dir.join("lib.rs");
    if lib_path.exists() {
        eprintln!(
            "programs/{}/src/lib.rs already exists — skipping (user-owned). guards.rs regenerated.",
            output_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<program>")
        );
        return Ok(());
    }

    let program_name = spec.program_name.to_lowercase();
    let program_id = spec
        .program_id
        .as_deref()
        .unwrap_or("11111111111111111111111111111111");

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/lib.rs"));
    out.push_str(surface.crate_attrs);
    out.push_str(surface.prelude_import);
    out.push('\n');
    out.push_str("mod instructions;\nuse instructions::*;\n");

    if !spec.events.is_empty() {
        out.push_str("pub mod events;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("pub mod errors;\n");
    }
    out.push_str("pub mod state;\n");
    out.push_str("pub mod guards;\n\n");

    out.push_str(&format!("declare_id!(\"{}\");\n\n", program_id));

    out.push_str("#[program]\n");
    out.push_str(&format!(
        "{} {} {{\n",
        surface.program_mod_vis, program_name
    ));
    out.push_str("    use super::*;\n\n");

    for (i, handler) in spec.handlers.iter().enumerate() {
        let pascal = to_pascal_case(&handler.name);

        if let Some(ref doc) = handler.doc {
            out.push_str(&format!("    /// {}\n", doc));
        }
        if surface.explicit_handler_discriminator {
            out.push_str(&format!("    #[instruction(discriminator = {})]\n", i));
        }

        let mut params = format!("ctx: {}<{}>", surface.context_type, pascal);

        for (pname, ptype) in &handler.takes_params {
            params.push_str(&format!(", {}: {}", pname, map_type(ptype, spec)?));
        }

        out.push_str(&format!(
            "    pub fn {}({}) -> {} {{\n",
            handler.name, params, surface.handler_result_type
        ));

        if handler.has_bumps() {
            out.push_str(&format!(
                "        ctx.accounts.handler({}&ctx.bumps)\n",
                handler
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!("{}, ", n))
                    .collect::<String>()
            ));
        } else {
            out.push_str(&format!(
                "        ctx.accounts.handler({})\n",
                handler
                    .takes_params
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push_str("    }\n\n");
    }

    out.push_str("}\n");

    // Anchor: emit `#[derive(Accounts)]` structs at crate root so the
    // `#[program]` macro can find them via `crate::<Pascal>`. Quasar
    // keeps structs in `instructions/<name>.rs` (handled by
    // `render_handler_scaffold`).
    if matches!(target, Target::Anchor) {
        let is_multi = spec.account_types.len() > 1;
        let default_state_name = format!("{}Account", to_pascal_case(&spec.program_name));
        out.push('\n');
        out.push_str("// `#[derive(Accounts)]` structs live at the crate root so the\n");
        out.push_str("// Anchor `#[program]` macro can resolve them via `crate::*`.\n");
        out.push_str("// The handler impl blocks live next to the (always-regenerated)\n");
        out.push_str("// guard module in `instructions/<name>.rs`.\n");
        out.push_str("use crate::state::*;\n");
        for handler in &spec.handlers {
            out.push('\n');
            out.push_str(&render_handler_accounts_struct(
                handler,
                spec,
                is_multi,
                &default_state_name,
                &surface,
                target,
            ));
        }
    }

    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("lib.rs"), &out)?;
    Ok(())
}

/// Generate src/state.rs
fn generate_state(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let is_multi = spec.account_types.len() > 1;

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/state.rs"));
    out.push_str(surface.prelude_import);
    out.push('\n');

    if is_multi {
        for (idx, acct) in spec.account_types.iter().enumerate() {
            let struct_name = format!("{}Account", acct.name);

            // Note: a previous pass emitted a `#[seeds(...)]` attribute on
            // the state struct from `gen_pda_seeds_attr`, but neither
            // Anchor nor Quasar recognize it (PDA seeds live on the
            // per-handler `#[account]` attribute, not the state struct).
            // Suppressed to avoid E0658 from an unknown attribute.

            let account_attr = if surface.explicit_account_discriminator {
                format!("#[account(discriminator = {})]\n", idx + 1)
            } else {
                "#[account]\n".to_string()
            };
            out.push_str(&format!(
                "{}pub struct {} {{\n",
                account_attr, struct_name
            ));

            for (fname, ftype) in &acct.fields {
                out.push_str(&format!("    pub {}: {},\n", fname, map_type(ftype, spec)?));
            }

            if acct.pda_ref.is_some() && !acct.fields.iter().any(|(n, _)| n == "bump") {
                out.push_str("    pub bump: u8,\n");
            }

            out.push_str("}\n\n");

            if !acct.lifecycle.is_empty() {
                out.push_str(&format!("/// {} lifecycle states.\n", acct.name));
                out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
                out.push_str("#[repr(u8)]\n");
                out.push_str(&format!("pub enum {}Status {{\n", acct.name));
                for (i, state) in acct.lifecycle.iter().enumerate() {
                    out.push_str(&format!("    {} = {},\n", state, i));
                }
                out.push_str("}\n\n");
            }
        }
    } else {
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));

        // No `#[seeds(...)]` on the state struct — see the multi-account
        // branch above. Per-handler PDA seeds are emitted on the
        // `#[account(seeds = [...], bump)]` attribute on the handler's
        // Accounts struct field.

        let account_attr = if surface.explicit_account_discriminator {
            "#[account(discriminator = 1)]\n"
        } else {
            "#[account]\n"
        };
        out.push_str(&format!(
            "{}pub struct {} {{\n",
            account_attr, state_name
        ));

        for (fname, ftype) in &spec.state_fields {
            out.push_str(&format!("    pub {}: {},\n", fname, map_type(ftype, spec)?));
        }

        if !spec.pdas.is_empty() && !spec.state_fields.iter().any(|(n, _)| n == "bump") {
            out.push_str("    pub bump: u8,\n");
        }

        out.push_str("}\n");

        if !spec.lifecycle_states.is_empty() {
            out.push_str("\n/// Program lifecycle states.\n");
            out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
            out.push_str("#[repr(u8)]\n");
            out.push_str("pub enum Status {\n");
            for (i, state) in spec.lifecycle_states.iter().enumerate() {
                out.push_str(&format!("    {} = {},\n", state, i));
            }
            out.push_str("}\n");
        }
    }

    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("state.rs"), &out)?;
    Ok(())
}

/// Generate PDA seeds attribute for a PDA declaration.
fn gen_pda_seeds_attr(
    pda: &crate::check::ParsedPda,
    fields: &[(String, String)],
    spec: &ParsedSpec,
) -> Result<String> {
    let mut seed_parts = Vec::new();
    for seed in &pda.seeds {
        let trimmed = seed.trim_matches('"');
        if seed.starts_with('"') || seed.starts_with('\"') {
            seed_parts.push(format!("b\"{}\"", trimmed));
        } else {
            let field_type = match fields.iter().find(|(n, _)| n == trimmed) {
                Some((_, t)) => map_type(t, spec)?,
                None => "Address".to_string(),
            };
            seed_parts.push(format!("{}: {}", trimmed, field_type));
        }
    }
    Ok(format!("#[seeds({})]\n", seed_parts.join(", ")))
}

/// Generate src/events.rs (only if events are declared)
fn generate_events(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    if spec.events.is_empty() {
        return Ok(());
    }

    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/events.rs"));
    out.push_str(surface.prelude_import);
    out.push('\n');

    for (i, event) in spec.events.iter().enumerate() {
        if surface.explicit_account_discriminator {
            // Quasar uses the same explicit-discriminator convention
            // for events as for accounts.
            out.push_str(&format!("#[event(discriminator = {})]\n", i + 1));
        } else {
            out.push_str("#[event]\n");
        }
        out.push_str(&format!("pub struct {} {{\n", event.name));
        for (fname, ftype) in &event.fields {
            out.push_str(&format!("    pub {}: {},\n", fname, map_type(ftype, spec)?));
        }
        out.push_str("}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("events.rs"), &out)?;
    Ok(())
}

/// Generate src/errors.rs (only if error codes are declared)
fn generate_errors(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    if spec.error_codes.is_empty() {
        return Ok(());
    }

    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let error_name = format!("{}Error", to_pascal_case(&spec.program_name));

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/errors.rs"));
    out.push_str(surface.prelude_import);
    out.push('\n');

    out.push_str("#[error_code]\n");
    out.push_str(&format!("pub enum {} {{\n", error_name));
    for (i, code) in spec.error_codes.iter().enumerate() {
        out.push_str(&format!("    {} = {},\n", code, i));
    }
    out.push_str("}\n");
    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("errors.rs"), &out)?;
    Ok(())
}

/// Generate src/instructions/mod.rs and per-handler files.
///
/// `mod.rs` is always regenerated (pure scaffold: `pub mod` declarations).
/// Per-handler `src/instructions/<name>.rs` files are USER-OWNED: emitted
/// only when missing. Each scaffolded handler body calls
/// `crate::guards::<name>(...)?` then falls through to `todo!()` for the
/// agent to fill in business logic. The `#[qed(verified, spec, handler,
/// hash, spec_hash)]` attribute ties the body and the spec contract
/// together at compile time.
fn generate_instructions(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    spec_path: &Path,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let instr_dir = output_dir.join("src").join("instructions");
    std::fs::create_dir_all(&instr_dir)?;

    let is_multi = spec.account_types.len() > 1;
    let default_state_name = format!("{}Account", to_pascal_case(&spec.program_name));

    // mod.rs — always regenerated, pure scaffold.
    let mut mod_out = String::new();
    mod_out.push_str(&marker("DO NOT EDIT", fp, "src/instructions/mod.rs"));
    for handler in &spec.handlers {
        mod_out.push_str(&format!("pub mod {};\n", handler.name));
    }
    // Quasar: re-export the `#[derive(Accounts)]` structs that live in
    // `instructions/<name>.rs` so the `#[program]` mod's
    // `use super::*;` brings them into scope. Anchor: structs live in
    // lib.rs at crate root, so no re-export is needed (and emitting
    // one would fail because the module no longer defines them).
    if matches!(target, Target::Quasar) {
        mod_out.push('\n');
        for handler in &spec.handlers {
            let pascal = to_pascal_case(&handler.name);
            mod_out.push_str(&format!("pub use {}::{};\n", handler.name, pascal));
        }
    }
    mod_out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(instr_dir.join("mod.rs"), &mod_out)?;

    // Read spec source once — used for spec_hash attributes.
    // `read_spec_source` handles both single-file and multi-file (directory)
    // specs, concatenating fragments in the same order the loader merges them.
    let spec_src = crate::check::read_spec_source(spec_path).unwrap_or_default();
    let spec_attr = relative_spec_path(spec_path, output_dir);

    // Per-handler instruction files — skip if existing (user-owned).
    for handler in &spec.handlers {
        let handler_path = instr_dir.join(format!("{}.rs", handler.name));
        if handler_path.exists() {
            eprintln!(
                "programs/{}/src/instructions/{}.rs already exists — skipping (user-owned). guards.rs regenerated.",
                output_dir.file_name().and_then(|n| n.to_str()).unwrap_or("<program>"),
                handler.name
            );
            continue;
        }

        let out = render_handler_scaffold(
            handler,
            spec,
            is_multi,
            &default_state_name,
            &spec_src,
            &spec_attr,
            target,
        )?;
        std::fs::write(&handler_path, &out)?;
    }

    Ok(())
}

/// Render the initial scaffold for a single user-owned handler file.
/// Identify the writable state-holding account in a handler. A handler's
/// accounts include user signers, token/mint accounts, programs, and
/// PDA-derived state holders; only the last category can receive a `self.X.field = ...`
/// effect expansion. Returns None when the handler has zero or multiple
/// plausible state accounts — in which case the caller must fall back to
/// `todo!()` and let a human (or M4 agent) disambiguate.
fn find_state_account(handler: &ParsedHandler) -> Option<&crate::check::ParsedHandlerAccount> {
    let mut candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| a.is_writable && !a.is_signer && !a.is_program)
        .filter(|a| {
            // Drop token/mint accounts — they hold balances, not program state.
            !matches!(a.account_type.as_deref(), Some("token") | Some("mint"))
        })
        .collect();

    // Prefer PDA-derived candidates when available.
    let pda_candidates: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|a| a.pda_seeds.is_some())
        .collect();
    if !pda_candidates.is_empty() {
        candidates = pda_candidates;
    }

    if candidates.len() == 1 {
        Some(candidates[0])
    } else {
        None
    }
}

/// Canonical SPL Token program ID. Calls into an interface whose
/// `program_id "..."` matches this constant get the `anchor_spl::token::*`
/// CPI shape (v2.8 G4). Other program IDs fall through to the
/// agent-fill comment for now — generic `invoke` codegen lands in v2.9.
const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Try to emit a real Anchor CPI invocation for one `call Interface.handler(...)`
/// site. Returns `None` when the interface isn't recognized (caller falls
/// back to a comment + `todo!()` so the user / an LLM fills the body).
///
/// v2.8 covers all five SPL Token handlers — `transfer`, `mint_to`,
/// `burn`, `initialize_account`, `close_account` — via `anchor_spl::token::*`.
/// Non-SPL-Token interfaces ship a generic `solana_program::program::invoke`
/// shape in v2.9. The canonical SPL handlers are the bulk of CPI traffic
/// in deployed programs, so this scope is enough to remove `todo!()` from
/// the typical escrow / lending / vault shape.
fn try_emit_anchor_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let iface = spec
        .interfaces
        .iter()
        .find(|i| i.name == call.target_interface)?;

    // SPL Token gets the special-case `anchor_spl::token::*` shapes
    // (typed accounts structs + the existing token::transfer / mint_to /
    // burn / initialize_account / close_account helpers — fewer lines of
    // generated code, idiomatic for the bulk of CPI traffic).
    if iface.program_id.as_deref() == Some(SPL_TOKEN_PROGRAM_ID) {
        return emit_spl_token_cpi(call, handler);
    }

    // Every other Anchor program gets the generic `invoke` shape
    // (v2.9 G3): sighash discriminator + Borsh-serialized args +
    // AccountMeta synthesis from the interface's accounts block.
    emit_generic_anchor_cpi(call, handler, iface)
}

/// SPL Token dispatcher. Routes to the right `anchor_spl::token` helper
/// per the called handler's name. Returns None on unrecognized handlers
/// (the caller falls back to comment + `todo!()`).
fn emit_spl_token_cpi(call: &crate::check::ParsedCall, handler: &ParsedHandler) -> Option<String> {
    let token_program_acct = find_token_program_account(handler)?;
    let prog_name = &token_program_acct.name;

    match call.target_handler.as_str() {
        "transfer" => emit_spl(
            call,
            handler,
            prog_name,
            "Transfer",
            &[("from", "from"), ("to", "to"), ("authority", "authority")],
            Some("amount"),
            "transfer",
        ),
        "mint_to" => emit_spl(
            call,
            handler,
            prog_name,
            "MintTo",
            &[
                ("mint", "mint"),
                ("to", "to"),
                // anchor_spl's MintTo uses `authority`; the canonical
                // qedspec interface names it `mint_authority` to match the
                // SPL Token instruction docs. Map between them at the
                // codegen boundary.
                ("authority", "mint_authority"),
            ],
            Some("amount"),
            "mint_to",
        ),
        "burn" => emit_spl(
            call,
            handler,
            prog_name,
            "Burn",
            &[
                ("mint", "mint"),
                ("from", "from"),
                ("authority", "authority"),
            ],
            Some("amount"),
            "burn",
        ),
        "initialize_account" => emit_spl(
            call,
            handler,
            prog_name,
            "InitializeAccount",
            &[
                ("account", "account"),
                ("mint", "mint"),
                // anchor_spl's InitializeAccount uses `authority` for the
                // owner slot; the canonical qedspec interface names it
                // `owner` to match SPL Token instruction docs.
                ("authority", "owner"),
                ("rent", "rent"),
            ],
            None,
            "initialize_account",
        ),
        "close_account" => emit_spl(
            call,
            handler,
            prog_name,
            "CloseAccount",
            &[
                ("account", "account"),
                ("destination", "destination"),
                ("authority", "authority"),
            ],
            None,
            "close_account",
        ),
        _ => None,
    }
}

/// Find the handler-side `<name> : program` account that points at the
/// token program. Convention: any `is_program` account named
/// `token_program`, or the unique `is_program` account otherwise.
fn find_token_program_account(
    handler: &ParsedHandler,
) -> Option<&crate::check::ParsedHandlerAccount> {
    handler
        .accounts
        .iter()
        .find(|a| a.is_program && a.name == "token_program")
        .or_else(|| {
            let programs: Vec<_> = handler.accounts.iter().filter(|a| a.is_program).collect();
            // .then(...) is lazy; .then_some(programs[0]) would evaluate
            // the index even when len is 0 and panic.
            (programs.len() == 1).then(|| programs[0])
        })
}

// ----------------------------------------------------------------------------
// v2.9 G3 — generic Anchor CPI codegen
// ----------------------------------------------------------------------------

/// Compute Anchor's instruction discriminator for a handler:
/// `Sha256("global:<handler_name>")[..8]`. This is the on-the-wire byte
/// prefix every Anchor instruction starts with — matches `anchor-lang`'s
/// `Discriminator` derive macro.
fn anchor_sighash(handler_name: &str) -> [u8; 8] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{}", handler_name).as_bytes());
    let result = hasher.finalize();
    let mut sighash = [0u8; 8];
    sighash.copy_from_slice(&result[..8]);
    sighash
}

/// Find the handler-side `<name> : program` account that points at a
/// non-SPL-Token target. Convention (mirrors `find_token_program_account`):
///   1. Prefer an account named `<interface_name_snake>_program`
///      (e.g. interface `MyAmm` → handler account `my_amm_program`).
///   2. Fall back to the unique `is_program` account if exactly one
///      exists (excluding any account named `token_program`, which is
///      reserved for SPL Token interactions and would only confuse a
///      generic-CPI dispatch).
///   3. Otherwise None — caller emits comment + `todo!()`.
fn find_program_account_for_interface<'a>(
    handler: &'a ParsedHandler,
    iface_name: &str,
) -> Option<&'a crate::check::ParsedHandlerAccount> {
    let expected_name = format!("{}_program", to_snake_case(iface_name));
    handler
        .accounts
        .iter()
        .find(|a| a.is_program && a.name == expected_name)
        .or_else(|| {
            let programs: Vec<_> = handler
                .accounts
                .iter()
                .filter(|a| a.is_program && a.name != "token_program")
                .collect();
            // .then(...) is lazy; .then_some(programs[0]) would evaluate
            // the index even when len is 0 and panic.
            (programs.len() == 1).then(|| programs[0])
        })
}

/// Convert PascalCase to snake_case. Used to map an interface name
/// (`MyAmm`) to its conventional handler-side program account name
/// (`my_amm_program`). Single-pass — adds an underscore before each
/// uppercase letter (except the first) and lowercases the result.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Emit a generic `solana_program::program::invoke` CPI shape for any
/// Anchor program that isn't SPL Token. Returns None when:
/// - the called handler isn't declared in the interface (unknown name);
/// - no program account is reachable in the calling handler (caller
///   falls back to comment + `todo!()` so the user can wire it manually).
///
/// Emitted shape:
///
/// ```rust
/// {
///     let mut ix_data: Vec<u8> = vec![<sighash bytes>];
///     <BorshSerialize each value arg>::serialize(&mut ix_data)?;
///     let ix = solana_program::instruction::Instruction {
///         program_id: solana_program::pubkey!("<iface_program_id>"),
///         accounts: vec![
///             AccountMeta::new(self.<acct>.key(), <is_signer>),
///             AccountMeta::new_readonly(self.<acct>.key(), <is_signer>),
///             // ... per the interface's accounts block, in declared order
///         ],
///         data: ix_data,
///     };
///     solana_program::program::invoke(&ix, &[
///         self.<acct>.to_account_info(),
///         // ... + the program account
///     ])?;
/// }
/// ```
fn emit_generic_anchor_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    iface: &crate::check::ParsedInterface,
) -> Option<String> {
    let program_id = iface.program_id.as_deref()?;
    let iface_handler = iface
        .handlers
        .iter()
        .find(|h| h.name == call.target_handler)?;
    let program_acct = find_program_account_for_interface(handler, &iface.name)?;

    let sighash = anchor_sighash(&call.target_handler);
    let sighash_literal = sighash
        .iter()
        .map(|b| format!("0x{:02x}", b))
        .collect::<Vec<_>>()
        .join(", ");

    // Collect (interface account name → caller's rust_expr at the call
    // site) so each AccountMeta and AccountInfo entry can address the
    // caller-side handler account.
    let arg_account_lookup: std::collections::HashMap<&str, &str> = call
        .args
        .iter()
        .filter(|a| iface_handler.accounts.iter().any(|ia| ia.name == a.name))
        .map(|a| (a.name.as_str(), a.rust_expr.as_str()))
        .collect();

    let mut out = String::new();
    out.push_str("        {\n");
    out.push_str(&format!(
        "            // Generic Anchor CPI to {}.{} (v2.9 G3).\n",
        iface.name, call.target_handler,
    ));
    out.push_str("            use anchor_lang::prelude::*;\n");
    out.push_str("            use anchor_lang::solana_program::program::invoke;\n");
    out.push_str(
        "            use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};\n",
    );
    out.push_str("            use anchor_lang::AnchorSerialize;\n\n");

    // Discriminator + Borsh-serialized handler params.
    out.push_str(&format!(
        "            let mut ix_data: Vec<u8> = vec![{}];\n",
        sighash_literal,
    ));
    for (param_name, _) in &iface_handler.params {
        let arg = call.args.iter().find(|a| &a.name == param_name)?;
        let resolved = resolve_call_arg_for_amount(&arg.rust_expr, handler);
        out.push_str(&format!(
            "            AnchorSerialize::serialize(&{}, &mut ix_data).map_err(|_| ProgramError::Custom(0))?;\n",
            resolved,
        ));
    }
    out.push('\n');

    // AccountMeta vec, in interface-declared order. Match writable / signer
    // role flags from the interface declaration.
    out.push_str("            let accounts = vec![\n");
    for ia in &iface_handler.accounts {
        let caller_acct = arg_account_lookup.get(ia.name.as_str())?;
        let constructor = if ia.is_writable {
            "AccountMeta::new"
        } else {
            "AccountMeta::new_readonly"
        };
        out.push_str(&format!(
            "                {}(self.{}.key(), {}),\n",
            constructor, caller_acct, ia.is_signer,
        ));
    }
    out.push_str("            ];\n\n");

    out.push_str("            let ix = Instruction {\n");
    out.push_str(&format!(
        "                program_id: anchor_lang::solana_program::pubkey!(\"{}\"),\n",
        program_id,
    ));
    out.push_str("                accounts,\n");
    out.push_str("                data: ix_data,\n");
    out.push_str("            };\n\n");

    out.push_str("            invoke(&ix, &[\n");
    for ia in &iface_handler.accounts {
        let caller_acct = arg_account_lookup.get(ia.name.as_str())?;
        out.push_str(&format!(
            "                self.{}.to_account_info(),\n",
            caller_acct,
        ));
    }
    out.push_str(&format!(
        "                self.{}.to_account_info(),\n",
        program_acct.name,
    ));
    out.push_str("            ])?;\n");
    out.push_str("        }\n");
    Some(out)
}

/// Emit one `anchor_spl::token::<fn>` CPI body. Generic over which SPL
/// Token handler is being called — the differences are the Anchor accounts
/// struct name, the call-arg → struct-field name map, the optional
/// scalar argument (e.g. `amount` for transfer / mint_to / burn; absent
/// for initialize_account / close_account), and the function name.
///
/// `field_to_arg` is `(anchor_field_name, call_arg_name)` pairs. The arg
/// name is the call-site identifier (matches the qedspec interface's
/// account block); the anchor field name is what `anchor_spl::token`'s
/// accounts struct expects. Most are identity (`("from", "from")`) but
/// some interfaces expose a more semantic name than anchor_spl uses
/// (e.g. `mint_authority` vs `authority`).
fn emit_spl(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    token_program: &str,
    accounts_struct: &str,
    field_to_arg: &[(&str, &str)],
    scalar_arg: Option<&str>,
    fn_name: &str,
) -> Option<String> {
    // Resolve every account argument via the call site.
    let mut acct_lines: Vec<String> = Vec::with_capacity(field_to_arg.len());
    let max_field = field_to_arg.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
    for (anchor_field, call_arg) in field_to_arg {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let pad = " ".repeat(max_field - anchor_field.len());
        acct_lines.push(format!(
            "                {}:{} self.{}.to_account_info(),\n",
            anchor_field, pad, arg.rust_expr
        ));
    }

    // Resolve the optional scalar arg (e.g. `amount`).
    let scalar_rhs = match scalar_arg {
        Some(name) => {
            let arg = call.args.iter().find(|a| a.name == name)?;
            Some(resolve_call_arg_for_amount(&arg.rust_expr, handler))
        }
        None => None,
    };

    let mut out = String::new();
    out.push_str("        {\n");
    out.push_str(&format!(
        "            use anchor_spl::token::{{self, {}}};\n",
        accounts_struct
    ));
    out.push_str(&format!(
        "            let cpi_accounts = {} {{\n",
        accounts_struct
    ));
    for line in &acct_lines {
        out.push_str(line);
    }
    out.push_str("            };\n");
    out.push_str(&format!(
        "            let cpi_program = self.{}.to_account_info();\n",
        token_program
    ));
    let invocation = match scalar_rhs {
        Some(rhs) => format!(
            "            token::{}(CpiContext::new(cpi_program, cpi_accounts), {})?;\n",
            fn_name, rhs
        ),
        None => format!(
            "            token::{}(CpiContext::new(cpi_program, cpi_accounts))?;\n",
            fn_name
        ),
    };
    out.push_str(&invocation);
    out.push_str("        }\n");
    Some(out)
}

/// Resolve a numeric / value argument's rust_expr to a form that's in
/// scope inside the handler `impl` block. Bare identifiers that match a
/// state field get the `self.<state_acct>.` prefix; handler params and
/// literals pass through unchanged.
fn resolve_call_arg_for_amount(rust_expr: &str, handler: &ParsedHandler) -> String {
    let is_simple_ident = rust_expr
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !is_simple_ident {
        return rust_expr.to_string();
    }
    if handler.takes_params.iter().any(|(n, _)| n == rust_expr) {
        return rust_expr.to_string();
    }
    if let Some(sa) = find_state_account(handler) {
        return format!("self.{}.{}", sa.name, rust_expr);
    }
    rust_expr.to_string()
}

/// Try to translate a single effect tuple to a real Rust statement. Returns
/// None when the RHS is too complex for mechanical expansion (match/arith/
/// pre-rendered Lean form); the caller falls through to a `todo!()` so an
/// LLM or human fills the body.
fn mechanize_effect(
    effect: &(String, String, String),
    state_acct: &crate::check::ParsedHandlerAccount,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
) -> Option<String> {
    let (field, op_kind, value) = effect;

    // Refuse complex RHS. `render_effect` pre-renders match/record/arith into
    // Lean string form; those start looking nothing like Rust identifiers.
    // A simple param / literal / constant is what's always safe.
    let simple_rhs = value
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !simple_rhs {
        return None;
    }

    let rhs = crate::rust_codegen_util::resolve_value(value, handler, spec);
    let acct = &state_acct.name;
    // v2.7 G3: `+=` default lowers to `checked_add(...).ok_or(err)?` — the
    // pattern deployed Anchor programs use. Pre-v2.7 this lowered to
    // `wrapping_add` which produced Kani false-positives and didn't match
    // production behavior. Explicit `+=!` / `+=?` opt into saturating /
    // wrapping.
    //
    // v2.8 F8: thread the user-declared Error sum through. Pre-F8 the
    // generated code referenced a non-existent `ErrorCode::MathOverflow`,
    // which only worked when no effect actually exercised checked
    // arithmetic. Now we emit `<ProgramName>Error::MathOverflow`, which
    // matches the Anchor `#[error_code]` enum generated alongside.
    // Specs that use `+=` / `-=` / `*=` should declare a `MathOverflow`
    // variant in their `type Error | ...` block; the
    // `effect_uses_checked_arith_without_math_overflow` lint surfaces
    // missing declarations.
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    // Quasar's `#[account]` macro auto-wraps integer state fields in their
    // Pod companions (u64 → PodU64). Plain `=` and `wrapping_*` between a
    // `u64` rhs and a `PodU64` lhs fail to type-check, so on Quasar:
    //   - `set` lhs gets `.into()` on the rhs (PodU64: From<u64>).
    //   - `checked_*` / `saturating_*` work as-is — PodU64 ships them.
    //   - `wrapping_*` is unwound to `<lhs>.get().wrapping_*(rhs).into()`
    //     because PodU64 doesn't expose `wrapping_*` directly.
    // Anchor uses native ints, so its branch matches the previous output.
    let is_quasar = matches!(target, Target::Quasar);
    let line = match op_kind.as_str() {
        "set" => {
            if is_quasar {
                format!("        self.{}.{} = ({}).into();\n", acct, field, rhs)
            } else {
                format!("        self.{}.{} = {};\n", acct, field, rhs)
            }
        }
        "add" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.checked_add({rhs}).ok_or({err_enum}::MathOverflow)?;\n"
        ),
        "add_sat" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.saturating_add({rhs});\n"
        ),
        "add_wrap" => {
            if is_quasar {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.get().wrapping_add({rhs}).into();\n"
                )
            } else {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.wrapping_add({rhs});\n"
                )
            }
        }
        "sub" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.checked_sub({rhs}).ok_or({err_enum}::MathOverflow)?;\n"
        ),
        "sub_sat" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.saturating_sub({rhs});\n"
        ),
        "sub_wrap" => {
            if is_quasar {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.get().wrapping_sub({rhs}).into();\n"
                )
            } else {
                format!(
                    "        self.{acct}.{field} = self.{acct}.{field}.wrapping_sub({rhs});\n"
                )
            }
        }
        _ => return None,
    };
    Some(line)
}

/// Render the `#[derive(Accounts)] pub struct X<'info>? { fields }`
/// block for one handler. Used by `generate_lib` (Anchor target —
/// structs live at crate root so `#[program]` can find them) and by
/// `render_handler_scaffold` (Quasar target — struct + impl together
/// in `instructions/<name>.rs`).
fn render_handler_accounts_struct(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    is_multi: bool,
    default_state_name: &str,
    surface: &FrameworkSurface,
    target: Target,
) -> String {
    let pascal = to_pascal_case(&handler.name);
    let lifetime_params = surface.lifetime_params();
    let mut out = String::new();
    out.push_str("#[derive(Accounts)]\n");
    out.push_str(&format!("pub struct {}{} {{\n", pascal, lifetime_params));

    if !handler.accounts.is_empty() {
        let state_acct = find_state_account(handler);
        for acct in &handler.accounts {
            let inferred_name = if is_multi {
                infer_state_name(acct, spec, default_state_name)
            } else {
                default_state_name.to_string()
            };
            // An account is "state-bearing" if either:
            //   1. `find_state_account` picked it as the unique writable
            //      non-token PDA (single-state-ADT specs), or
            //   2. `infer_state_name` matched its name to a declared state
            //      ADT in this multi-state spec (e.g., `loan` ↔ `Loan` ADT
            //      → `LoanAccount`). Without this, a multi-PDA handler like
            //      lending's `borrow` (loan + pool both writable PDAs)
            //      drops `loan` to `UncheckedAccount` even though it's the
            //      lifecycle target.
            let inferred_match =
                is_multi && inferred_name != default_state_name;
            let is_state =
                state_acct.map(|sa| sa.name == acct.name).unwrap_or(false) || inferred_match;
            let attr = acct.quasar_account_attr(handler, &inferred_name, target);
            let field_type = render_account_field_type(acct, surface, is_state, &inferred_name);
            out.push_str(&format!("{}    pub {}: {},\n", attr, acct.name, field_type));
        }
    } else if handler.who.is_some() {
        let signer_ty = if surface.accounts_lifetime.is_empty() {
            "Signer".to_string()
        } else {
            format!("Signer<{}>", surface.accounts_lifetime)
        };
        out.push_str(&format!("    pub signer: {},\n", signer_ty));
    }

    out.push_str("}\n");
    out
}

fn render_handler_scaffold(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    is_multi: bool,
    default_state_name: &str,
    spec_src: &str,
    spec_attr: &str,
    target: Target,
) -> Result<String> {
    let surface = FrameworkSurface::for_target(target);
    let pascal = to_pascal_case(&handler.name);
    let bumps_name = format!("{}Bumps", pascal);
    let any_mut = handler.accounts.iter().any(|a| a.is_writable);
    let lifetime_params = surface.lifetime_params();
    // Anchor puts the `#[derive(Accounts)]` struct at crate root (in
    // lib.rs) so the `#[program]` macro can find it; Quasar keeps
    // struct + impl together in `instructions/<name>.rs`. The flag
    // also flips the imports — Anchor's instructions file pulls the
    // struct in via `use crate::<Pascal>;`.
    let render_struct = matches!(target, Target::Quasar);

    let mut out = String::new();
    out.push_str("// User-owned. Regenerating the spec does NOT overwrite this file.\n");
    out.push_str("// Guard checks live in the sibling `crate::guards` module and ARE\n");
    out.push_str("// regenerated on every `qedgen codegen`. Drift between the spec\n");
    out.push_str("// handler block and the `spec_hash` below fires a compile_error!\n");
    out.push_str("// via the `#[qed(verified, ...)]` macro.\n\n");
    out.push_str(surface.prelude_import);
    // Token / Mint live in a separate crate per framework; only pull
    // them in when the spec actually declares token accounts.
    let needs_spl = handler.has_token_accounts();
    if needs_spl {
        match target {
            Target::Anchor => out.push_str("use anchor_spl::token::{Token, Mint, TokenAccount};\n"),
            Target::Quasar => out.push_str("use quasar_spl::{Token, Mint};\n"),
            Target::Pinocchio => unreachable!(),
        }
    }
    out.push_str("use crate::state::*;\n");
    out.push_str("use crate::guards;\n");
    out.push_str("use qedgen_macros::qed;\n");
    if !render_struct {
        // Anchor: bring the Accounts struct (defined in lib.rs) into
        // scope so the impl block can reference it bare.
        out.push_str(&format!("use crate::{};\n", pascal));
    }
    if !spec.events.is_empty() && !handler.emits.is_empty() {
        out.push_str("use crate::events::*;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    out.push('\n');

    if render_struct {
        out.push_str(&render_handler_accounts_struct(
            handler,
            spec,
            is_multi,
            default_state_name,
            &surface,
            target,
        ));
        out.push('\n');
    }

    // impl block with handler — lifetime threaded for Anchor.
    out.push_str(&format!(
        "impl{} {}{} {{\n",
        lifetime_params, pascal, lifetime_params
    ));
    if let Some(ref doc) = handler.doc {
        out.push_str(&format!("    /// {}\n", doc));
    }

    // Emit the spec-bound #[qed(...)] attribute with a body-hash
    // sentinel. The fixup pass at the bottom of this function parses
    // the rendered impl method, computes the real body hash, and
    // splices it into the placeholder. Both `qedgen::spec_hash` and
    // `qedgen-macros::FnLike::content_hash` normalize via
    // `proc_macro2::TokenStream::from_str` before hashing, so the
    // codegen-emitted `hash` agrees with the macro's compile-time
    // recomputation.
    let spec_h = spec_hash::spec_hash_for_handler(spec_src, &handler.name).unwrap_or_default();
    out.push_str(&format!(
        "    #[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\")]\n",
        spec_attr, handler.name, BODY_HASH_PLACEHOLDER, spec_h
    ));

    out.push_str("    #[inline(always)]\n");

    let self_ref = if any_mut { "&mut self" } else { "&self" };
    let mut handler_params = vec![self_ref.to_string()];
    let mut param_names: Vec<String> = Vec::new();
    for (pname, ptype) in &handler.takes_params {
        handler_params.push(format!("{}: {}", pname, map_type(ptype, spec)?));
        param_names.push(pname.clone());
    }
    if handler.has_bumps() {
        handler_params.push(format!("bumps: &{}", bumps_name));
    }

    out.push_str(&format!(
        "    pub fn handler({}) -> {} {{\n",
        handler_params.join(", "),
        surface.handler_result_type
    ));

    // Call the always-regenerated guards module. Signature: takes `&Self`
    // plus every handler-level parameter, returns `Result<(), ProgramError>`.
    let guard_args = if param_names.is_empty() {
        "self".to_string()
    } else {
        format!("self, {}", param_names.join(", "))
    };
    out.push_str(&format!(
        "        guards::{}({})?;\n",
        handler.name, guard_args
    ));

    // Spec-level `let` bindings (e.g. `let total_fee = amount * 125 / 10000`)
    // must be emitted BEFORE the effect block — effect RHSs reference them.
    // Pre-fix: they were dropped on the Rust side, leaving undefined-variable
    // errors on `cargo build`.
    for (binding_name, _lean_expr, rust_expr) in &handler.let_bindings {
        out.push_str(&format!("        let {} = {};\n", binding_name, rust_expr));
    }

    // Mechanical-effect expansion (v2.4-M3). For each spec effect we try to
    // emit a real Rust statement; anything non-mechanical stays as a comment
    // and forces a trailing `todo!()` so the user / an LLM (M4) fills it in.
    let state_acct = find_state_account(handler);
    let mut any_unmechanized = false;
    for effect in &handler.effects {
        let mechanized =
            state_acct.and_then(|sa| mechanize_effect(effect, sa, handler, spec, target));
        match mechanized {
            Some(line) => out.push_str(&line),
            None => {
                let (field, op_kind, value) = effect;
                out.push_str(&format!(
                    "        // Spec effect (needs fill): {} {} {}\n",
                    field, op_kind, value
                ));
                any_unmechanized = true;
            }
        }
    }

    // Events are always agent-fill for now (M4): the spec declares the event
    // name but not the payload binding.
    for emit in &handler.emits {
        out.push_str(&format!("        // Spec: emit!({})\n", emit));
    }
    let has_events = !handler.emits.is_empty();

    // Token transfers (CPI calls) are also agent-fill: building the CPI
    // context from the handler accounts is mechanical-ish but involves
    // framework-specific helpers that differ per Quasar/Anchor/raw.
    let has_transfers = !handler.transfers.is_empty();
    for t in &handler.transfers {
        out.push_str(&format!(
            "        // Spec transfer: {} -> {} amount={}\n",
            t.from,
            t.to,
            t.amount.as_deref().unwrap_or("?")
        ));
    }

    // `call Interface.handler(name = expr, ...)` sites — the uniform CPI
    // surface introduced in v2.5 (slice 2). v2.8 G4 lands real Anchor CPI
    // codegen for the canonical SPL Token transfer; other interfaces and
    // handlers still emit a structured comment + todo!() so an LLM /
    // human fills the body. The boolean tracks whether any call site
    // remained unmechanized so the tail `todo!()` only fires for those.
    let mut any_unmechanized_call = false;
    for c in &handler.calls {
        match try_emit_anchor_cpi(c, handler, spec) {
            Some(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{} (Anchor CPI emitted by v2.8 G4)\n",
                    c.target_interface, c.target_handler
                ));
                out.push_str(&rendered);
            }
            None => {
                let args = c
                    .args
                    .iter()
                    .map(|a| format!("{}={}", a.name, a.rust_expr))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "        // Spec call: {}.{}({}) — v2.9 will emit a generic Anchor CPI\n",
                    c.target_interface, c.target_handler, args
                ));
                any_unmechanized_call = true;
            }
        }
    }

    let needs_fill = any_unmechanized || has_events || has_transfers || any_unmechanized_call;
    if needs_fill {
        out.push_str("        todo!(\"fill non-mechanical effects, events, transfers, calls\")\n");
    } else {
        out.push_str("        Ok(())\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n");

    // Fixup: parse the rendered scaffold, find the impl method,
    // compute the body hash, and splice it into the
    // `hash = "QEDGEN_FIXUP_BODY_HASH"` placeholder.
    // `qedgen::spec_hash::body_hash_for_*` and
    // `qedgen-macros::FnLike::content_hash` both normalize via
    // `proc_macro2::TokenStream::from_str` so codegen-time and
    // compile-time agree on the hash; first `cargo build` is clean.
    if let Some(body_hash) = precompute_body_hash(&out) {
        out = out.replace(BODY_HASH_PLACEHOLDER, &body_hash);
    }
    Ok(out)
}

/// Re-parse a rendered handler scaffold (with `BODY_HASH_PLACEHOLDER`
/// still in the `#[qed]` attribute), find the impl method named
/// `handler`, and compute its body hash. MUST mirror
/// `qedgen-macros::FnLike::from_tokens`'s parse order (try `ItemFn`
/// first, fall back to `ImplItemFn`) so we hit the same arm — both
/// produce the same canonical bytes after the `from_str`
/// normalization in `body_hash_for_*`, but only when fed equivalent
/// inputs.
fn precompute_body_hash(scaffold_source: &str) -> Option<String> {
    use quote::ToTokens;
    let file: syn::File = syn::parse_str(scaffold_source).ok()?;
    for item in &file.items {
        if let syn::Item::Impl(item_impl) = item {
            for impl_item in &item_impl.items {
                if let syn::ImplItem::Fn(impl_fn) = impl_item {
                    if impl_fn.sig.ident == "handler" {
                        let tokens = impl_fn.to_token_stream();
                        if let Ok(item_fn) = syn::parse2::<syn::ItemFn>(tokens.clone()) {
                            return Some(spec_hash::body_hash_for_fn(&item_fn));
                        }
                        if let Ok(impl_fn2) = syn::parse2::<syn::ImplItemFn>(tokens) {
                            return Some(spec_hash::body_hash_for_impl_fn(&impl_fn2));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Generate src/guards.rs — one function per handler containing all the
/// spec-declared guard checks. This file is always regenerated; any edit
/// is clobbered on the next `qedgen codegen` (by design).
fn generate_guards(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(target);
    let lifetime_params = surface.lifetime_params();
    // Anchor errors flow through `Result<()>` (the `Result` alias from
    // `anchor_lang::prelude` defaults the error to
    // `anchor_lang::error::Error`); Anchor error enums implement
    // `Into<anchor_lang::error::Error>`, so `Err(MyError::Foo.into())`
    // is the idiomatic return. Quasar uses `ProgramError::from(...)`
    // because its error enums implement `Into<ProgramError>` instead.
    let err_ctor: fn(&str, &str) -> String = match target {
        Target::Anchor => |enum_name, variant| format!("{}::{}.into()", enum_name, variant),
        Target::Quasar => {
            |enum_name, variant| format!("ProgramError::from({}::{})", enum_name, variant)
        }
        Target::Pinocchio => unreachable!(),
    };
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let mut out = String::new();
    out.push_str(&marker(
        "DO NOT EDIT — regenerated from .qedspec",
        fp,
        "src/guards.rs",
    ));
    out.push_str("//! Per-handler guard checks derived from the `.qedspec`.\n");
    out.push_str("//! Called from user-owned `instructions/<name>::handler` before\n");
    out.push_str("//! business logic; keep guard logic here, policy-free logic there.\n\n");
    out.push_str(
        "#![allow(unused_variables, unused_imports, dead_code, clippy::too_many_arguments)]\n\n",
    );
    out.push_str(surface.prelude_import);
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    // Pick up the per-handler `Accounts` structs. Anchor places them
    // at crate root (lib.rs); Quasar places them in
    // `instructions/<name>.rs` and re-exports via `instructions::*`.
    match target {
        Target::Anchor => out.push_str("use crate::*;\n\n"),
        Target::Quasar => out.push_str("use crate::instructions::*;\n\n"),
        Target::Pinocchio => unreachable!(),
    }

    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        let any_mut = handler.accounts.iter().any(|a| a.is_writable);
        let self_ref = if any_mut { "&mut " } else { "&" };
        let mut params = vec![format!("ctx: {}{}{}", self_ref, pascal, lifetime_params)];
        for (pname, ptype) in &handler.takes_params {
            params.push(format!("{}: {}", pname, map_type(ptype, spec)?));
        }
        out.push_str(&format!(
            "/// Guards for `{}`.  \n/// Generated from the `requires` clauses of the spec handler block.\n",
            handler.name
        ));
        out.push_str(&format!(
            "pub fn {}{}({}) -> {} {{\n",
            handler.name,
            lifetime_params,
            params.join(", "),
            surface.handler_result_type
        ));

        if handler.requires.is_empty() && handler.aborts_if.is_empty() {
            out.push_str("    // No guards declared in spec — nothing to check.\n");
        }

        // `rust_expr` references state fields as `s.<field>` (lowered from
        // `state.<field>` in the spec). Inside guards.rs the state-bearing
        // account is reached via `ctx.<state_account>.<field>` (Anchor's
        // `Account<T>` and Quasar's typed account both auto-deref to T).
        // When we can identify a single state account, rewrite `s.` to that
        // path so the guards compile. Multi-state handlers fall through with
        // the raw `s.` form — caller must hand-edit. R12 fix.
        let state_acct = find_state_account(handler);
        let bind_state = |expr: &str| -> String {
            match state_acct {
                Some(sa) => expr.replace("s.", &format!("ctx.{}.", sa.name)),
                None => expr.to_string(),
            }
        };

        for req in &handler.requires {
            // Emit as a comment for human readers + an executable check.
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
            let rust = bind_state(req.rust_expr.trim());
            if let Some(err) = &req.error_name {
                out.push_str(&format!(
                    "    if !({}) {{ return Err({}); }}\n",
                    rust,
                    err_ctor(&err_enum, err),
                ));
            } else {
                out.push_str(&format!("    debug_assert!({});\n", rust));
            }
        }

        let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
        for ab in &handler.aborts_if {
            let rust = bind_state(ab.rust_expr.trim());
            out.push_str(&format!(
                "    if ({}) {{ return Err({}); }}\n",
                rust,
                err_ctor(&err_enum, &ab.error_name),
            ));
        }

        out.push_str("    Ok(())\n");
        out.push_str("}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(src_dir.join("guards.rs"), &out)?;
    Ok(())
}

/// Infer the state struct name for a handler account in multi-account specs.
fn infer_state_name(
    acct: &crate::check::ParsedHandlerAccount,
    spec: &ParsedSpec,
    default: &str,
) -> String {
    // Check if this account name matches any account type name (lowercase match)
    for at in &spec.account_types {
        if acct.name == at.name.to_lowercase() || acct.name.starts_with(&at.name.to_lowercase()) {
            return format!("{}Account", at.name);
        }
    }
    default.to_string()
}

/// Generate Cargo.toml
fn generate_cargo_toml(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let program_name = spec.program_name.to_lowercase().replace('_', "-");
    let needs_spl = spec.handlers.iter().any(|h| h.has_token_accounts());
    let hash = fp
        .file_hashes
        .get("Cargo.toml")
        .cloned()
        .unwrap_or_default();
    let qedgen_version = env!("CARGO_PKG_VERSION");

    let mut out = String::new();
    out.push_str(&format!(
        "# ---- GENERATED BY QEDGEN ---- spec-hash:{}\n\n",
        hash
    ));
    out.push_str("[package]\n");
    out.push_str(&format!("name = \"{}\"\n", program_name));
    out.push_str("version = \"0.1.0\"\n");
    out.push_str("edition = \"2021\"\n\n");
    out.push_str("[lib]\n");
    out.push_str("crate-type = [\"cdylib\", \"lib\"]\n\n");
    out.push_str("[features]\n");
    out.push_str("client = []\n");
    out.push_str("debug = []\n\n");
    out.push_str("[dependencies]\n");
    match target {
        Target::Anchor => {
            out.push_str("anchor-lang = \"0.32.1\"\n");
            if needs_spl {
                out.push_str("anchor-spl = \"0.32.1\"\n");
            }
        }
        Target::Quasar => {
            out.push_str("quasar-lang = { version = \"0.0.0\" }\n");
            if needs_spl {
                // Token / Mint live in `quasar-spl`, not the core
                // `quasar-lang` prelude. Pull it in whenever the spec
                // declares token accounts or transfers.
                out.push_str("quasar-spl = { version = \"0.0.0\" }\n");
            }
        }
        Target::Pinocchio => unreachable!("Pinocchio is rejected at the init dispatcher"),
    }
    out.push_str(&format!(
        "qedgen-macros = {{ git = \"https://github.com/qedgen/solana-skills\", tag = \"v{}\" }}\n",
        qedgen_version
    ));

    // Stand the generated crate up as its own workspace root. Without this,
    // when the spec lives inside a parent crate that has its own `[package]`
    // (e.g. percolator's pure-no_std host library), cargo tries to read the
    // parent as a workspace root and fails with "current package believes
    // it's in a workspace when it's not". Empty `[workspace]` makes the
    // generated crate self-contained.
    out.push_str("\n[workspace]\n");

    std::fs::write(output_dir.join("Cargo.toml"), &out)?;
    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Generate a framework-flavored Rust program skeleton from a `.qedspec`.
///
/// `target` selects which framework's idioms the emitter uses
/// (`Target::Anchor` → `anchor_lang::prelude::*`, `Context<X>`,
/// `Result<()>`, auto-derived discriminators; `Target::Quasar` →
/// `quasar_lang::prelude::*`, `#![no_std]`, `Ctx<X>`, `Result<(),
/// ProgramError>`, explicit `#[instruction(discriminator = N)]`).
/// `Target::Pinocchio` is rejected at the `init` dispatcher and won't
/// reach this function in v2.9.
pub fn generate(spec_path: &Path, output_dir: &Path, target: crate::Target) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No handlers found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    crate::rust_codegen_util::check_effect_targets(&spec)?;

    // Check that the project is initialized (.qed/ next to the spec file)
    if crate::init::find_qed_dir(spec_path).is_none() {
        anyhow::bail!(
            "No .qed/ directory found next to {} — run `qedgen init` first.",
            spec_path.display()
        );
    }

    std::fs::create_dir_all(output_dir)?;

    let fp = crate::fingerprint::compute_fingerprint(&spec);

    generate_lib(&spec, &fp, output_dir, target)?;
    generate_state(&spec, &fp, output_dir, target)?;
    generate_events(&spec, &fp, output_dir, target)?;
    generate_errors(&spec, &fp, output_dir, target)?;
    generate_instructions(&spec, &fp, spec_path, output_dir, target)?;
    generate_guards(&spec, &fp, output_dir, target)?;
    generate_cargo_toml(&spec, &fp, output_dir, target)?;

    let file_count = 4
        + spec.handlers.len()
        + if spec.events.is_empty() { 0 } else { 1 }
        + if spec.error_codes.is_empty() { 0 } else { 1 };

    eprintln!("Generated {} files in {}", file_count, output_dir.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_spec() -> ParsedSpec {
        ParsedSpec::default()
    }

    fn spec_with_constants(pairs: &[(&str, &str)]) -> ParsedSpec {
        ParsedSpec {
            constants: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..ParsedSpec::default()
        }
    }

    #[test]
    fn map_type_covers_all_primitives() {
        let spec = empty_spec();

        // Integer primitives
        assert_eq!(map_type("U8", &spec).unwrap(), "u8");
        assert_eq!(map_type("U16", &spec).unwrap(), "u16");
        assert_eq!(map_type("U32", &spec).unwrap(), "u32");
        assert_eq!(map_type("U64", &spec).unwrap(), "u64");
        assert_eq!(map_type("U128", &spec).unwrap(), "u128");
        assert_eq!(map_type("I8", &spec).unwrap(), "i8");
        assert_eq!(map_type("I16", &spec).unwrap(), "i16");
        assert_eq!(map_type("I32", &spec).unwrap(), "i32");
        assert_eq!(map_type("I64", &spec).unwrap(), "i64");
        assert_eq!(map_type("I128", &spec).unwrap(), "i128");

        // Non-integer primitives
        assert_eq!(map_type("Bool", &spec).unwrap(), "bool");
        assert_eq!(map_type("Pubkey", &spec).unwrap(), "Address");
    }

    #[test]
    fn map_type_errors_on_unknown_type() {
        // v2.6.1 bug: DSL types not in the four-item allowlist (U8/U64/U128/I128)
        // fell through as-is, leaking `U16` verbatim into Rust. v2.6.2: unknown
        // types must surface as errors at codegen time.
        let spec = empty_spec();
        let err = map_type("Blorb", &spec).unwrap_err().to_string();
        assert!(
            err.contains("Blorb"),
            "error should name the bad type: {err}"
        );
        assert!(
            err.contains("unsupported DSL type"),
            "error should call it out as unsupported: {err}"
        );
    }

    #[test]
    fn map_type_renders_map_with_literal_bound() {
        let spec = empty_spec();
        assert_eq!(map_type("Map[4] U64", &spec).unwrap(), "[u64; 4]");
        assert_eq!(map_type("Map[16] U8", &spec).unwrap(), "[u8; 16]");
        assert_eq!(map_type("Map[32] Pubkey", &spec).unwrap(), "[Address; 32]");
    }

    #[test]
    fn map_type_resolves_map_bound_via_constants() {
        // Mirrors the percolator eval case: `Map[MAX_ACCOUNTS] U64` where
        // MAX_ACCOUNTS is declared as a spec constant.
        let spec = spec_with_constants(&[("MAX_ACCOUNTS", "256"), ("UNRELATED", "99")]);
        assert_eq!(
            map_type("Map[MAX_ACCOUNTS] U64", &spec).unwrap(),
            "[u64; 256]"
        );
    }

    #[test]
    fn map_type_errors_when_map_bound_is_unknown_symbol() {
        // Bound is neither a literal nor a declared constant → clear error
        // naming the unresolved symbol.
        let spec = empty_spec();
        let err = map_type("Map[MISSING] U64", &spec).unwrap_err().to_string();
        assert!(
            err.contains("MISSING"),
            "error should name the bound: {err}"
        );
        assert!(
            err.contains("not a numeric literal") || err.contains("not declared"),
            "error should explain why the bound didn't resolve: {err}"
        );
    }

    #[test]
    fn map_type_resolves_fin_to_usize() {
        // `Fin[N]` → `usize`. Used for index types like `Fin[MAX_ACCOUNTS]`.
        let spec = spec_with_constants(&[("MAX_ACCOUNTS", "256")]);
        assert_eq!(map_type("Fin[MAX_ACCOUNTS]", &spec).unwrap(), "usize");
        assert_eq!(map_type("Fin[4]", &spec).unwrap(), "usize");
    }

    #[test]
    fn map_type_resolves_type_aliases_transitively() {
        // The percolator pattern: `type AccountIdx = Fin[MAX_ACCOUNTS]`.
        // `map_type("AccountIdx")` must resolve through the alias to `usize`.
        use crate::check::ParsedRecordType;
        let mut spec = ParsedSpec {
            type_aliases: vec![
                ("AccountIdx".to_string(), "Fin[MAX_ACCOUNTS]".to_string()),
                ("MyAlias".to_string(), "U64".to_string()),
            ],
            ..ParsedSpec::default()
        };
        assert_eq!(map_type("AccountIdx", &spec).unwrap(), "usize");
        assert_eq!(map_type("MyAlias", &spec).unwrap(), "u64");

        // Record name stays as-is for struct emission downstream.
        spec.records.push(ParsedRecordType {
            name: "UserAccount".to_string(),
            fields: vec![
                ("active".to_string(), "U8".to_string()),
                ("capital".to_string(), "U128".to_string()),
            ],
        });
        assert_eq!(map_type("UserAccount", &spec).unwrap(), "UserAccount");
        // `Map[N] UserAccount` → `[UserAccount; N]`.
        spec.constants = vec![("MAX_ACCOUNTS".to_string(), "4".to_string())];
        assert_eq!(
            map_type("Map[MAX_ACCOUNTS] UserAccount", &spec).unwrap(),
            "[UserAccount; 4]"
        );
    }

    #[test]
    fn sanitize_ident_replaces_subscripts_and_dots() {
        // The eval's actual output:
        //   fn verify_init_user_effect_accounts[i].active()
        // must become a legal Rust identifier.
        assert_eq!(sanitize_ident("accounts[i].active"), "accounts_i_active");
        assert_eq!(sanitize_ident("s.foo.bar"), "s_foo_bar");
        assert_eq!(sanitize_ident("plain_field"), "plain_field");
    }

    #[test]
    fn sanitize_ident_collapses_consecutive_and_trailing_underscores() {
        // Repeated non-ident chars should not pile up as `___`.
        assert_eq!(sanitize_ident("foo[ ].bar"), "foo_bar");
        // Leading non-ident chars produce a leading `_` that stays (doesn't
        // collapse to empty) — this keeps the resulting string non-empty.
        assert_eq!(sanitize_ident("[i]"), "_i");
        // Trailing non-ident chars drop cleanly.
        assert_eq!(sanitize_ident("foo."), "foo");
    }

    #[test]
    fn map_type_errors_on_undeclared_user_type() {
        // `Map[N] UserAccount` where UserAccount is neither a primitive nor
        // declared via `type UserAccount = …` / `type UserAccount { … }` /
        // `type UserAccount | …`. Must surface as an error naming the bad
        // inner type rather than silently emitting broken Rust.
        let spec = spec_with_constants(&[("MAX_ACCOUNTS", "8")]);
        let err = map_type("Map[MAX_ACCOUNTS] UserAccount", &spec)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("UserAccount"),
            "error should name the unsupported inner type: {err}"
        );
    }

    // ----- v2.8 G4: Anchor CPI codegen for SPL Token transfer -----

    /// Exercise try_emit_anchor_cpi against an end-to-end-parsed spec.
    /// Hits the resolver pipeline (no need to construct ParsedSpec by
    /// hand) and confirms the SPL Token transfer shape lands.
    #[test]
    fn cpi_emits_anchor_spl_transfer_for_canonical_program_id() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state         : writable
    src           : writable
    dst           : writable
    auth          : signer
    token_program : program
  }
  call Token.transfer(from = src, to = dst, amount = n, authority = auth)
}
"#,
        )
        .unwrap();
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "send")
            .expect("send handler");
        let call = handler.calls.first().expect("call site");
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit Anchor CPI");
        assert!(
            rendered.contains("anchor_spl::token::{self, Transfer}"),
            "must use anchor_spl::token::Transfer; got:\n{rendered}"
        );
        assert!(
            rendered.contains("from:      self.src.to_account_info()"),
            "from arg must resolve to self.src; got:\n{rendered}"
        );
        assert!(
            rendered.contains("token::transfer(CpiContext::new(cpi_program, cpi_accounts), n)"),
            "amount arg `n` is a handler param and should pass through bare; got:\n{rendered}"
        );
    }

    #[test]
    fn anchor_sighash_matches_known_discriminators() {
        // Anchor's discriminator = sha256("global:<handler>")[..8].
        // Verify the function uses the right input format by computing
        // the expected value via sha2 directly, confirming both prefix
        // and slice-length are correct. If `anchor_sighash` ever drifts
        // (e.g. wrong prefix, different hash, wrong slice), this test
        // catches it independently of what value the function produces.
        use sha2::{Digest, Sha256};
        for handler in ["initialize", "transfer", "swap", "do_nothing"] {
            let mut hasher = Sha256::new();
            hasher.update(format!("global:{}", handler).as_bytes());
            let full = hasher.finalize();
            let mut expected = [0u8; 8];
            expected.copy_from_slice(&full[..8]);
            assert_eq!(
                anchor_sighash(handler),
                expected,
                "sighash for `{handler}` should be sha256(\"global:{handler}\")[..8]"
            );
        }
        // Sanity: different handler names produce different sighashes.
        assert_ne!(anchor_sighash("a"), anchor_sighash("b"));
    }

    #[test]
    fn to_snake_case_handles_pascal_and_camel() {
        assert_eq!(to_snake_case("MyAmm"), "my_amm");
        assert_eq!(to_snake_case("SPLToken"), "s_p_l_token");
        assert_eq!(to_snake_case("Token"), "token");
        assert_eq!(to_snake_case("simple"), "simple");
        assert_eq!(to_snake_case("FooBarBaz"), "foo_bar_baz");
    }

    #[test]
    fn cpi_generic_returns_none_when_program_account_is_missing() {
        // No `<iface>_program` account, no unique non-token-program
        // account either. Caller falls back to comment + todo!().
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface MyAmm {
  program_id "MyAmm22222222222222222222222222222222222222"
  handler swap (amount : U64) {
    discriminant "0x01"
    accounts { src : writable }
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    src : writable
  }
  call MyAmm.swap(src = src, amount = balance)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        assert!(
            try_emit_anchor_cpi(call, handler, &spec).is_none(),
            "missing program account should defer to comment + todo!()"
        );
    }

    #[test]
    fn cpi_emits_generic_invoke_shape_for_non_spl_token_interface() {
        // v2.9 G3: non-SPL-Token interfaces get the generic
        // `solana_program::program::invoke` shape rather than v2.8's
        // None / comment-only fallback.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface MyAmm {
  program_id "MyAmm22222222222222222222222222222222222222"
  handler swap (amount : U64) {
    discriminant "0x01"
    accounts {
      src : writable
      dst : writable
    }
    ensures amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    src          : writable
    dst          : writable
    my_amm_program : program
  }
  call MyAmm.swap(src = src, dst = dst, amount = balance)
}
"#,
        )
        .unwrap();
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "send")
            .expect("send handler");
        let call = handler.calls.first().expect("call site");
        let rendered = try_emit_anchor_cpi(call, handler, &spec)
            .expect("v2.9 must emit a generic CPI shape for non-SPL Anchor programs");

        // Sanity-check the emitted shape:
        assert!(rendered.contains("solana_program::program::invoke"));
        assert!(rendered.contains("Instruction"));
        assert!(rendered.contains("AccountMeta::new(self.src.key(), false)"));
        assert!(rendered.contains("AccountMeta::new(self.dst.key(), false)"));
        // The program account ends up in the AccountInfo array passed to
        // invoke (so the runtime can validate it).
        assert!(rendered.contains("self.my_amm_program.to_account_info()"));
        // Discriminator: first byte of sha256("global:swap") is 0xf8.
        assert!(
            rendered.contains("0xf8"),
            "expected sighash for `swap` to start with 0xf8; got:\n{rendered}"
        );
        // Borsh-serialized amount arg.
        assert!(rendered.contains("AnchorSerialize::serialize"));
    }

    // ----- v2.8 F8: Error-sum threading via mechanize_effect -----

    #[test]
    fn mechanize_effect_references_program_error_enum_for_checked_add() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec MyProgram
program_id "11111111111111111111111111111111"
type State | Active of { pool : U64 }
type Error | MathOverflow

handler bump (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state : writable
  }
  effect { pool += n }
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "bump").unwrap();
        let state_acct = find_state_account(handler).expect("state account");
        let effect = handler.effects.first().unwrap();
        let rendered =
            mechanize_effect(effect, state_acct, handler, &spec, Target::Anchor).expect("mechanized");
        // Pre-F8 this said `ErrorCode::MathOverflow` (a non-existent enum).
        // F8: it now says `<ProgramName>Error::MathOverflow`, matching the
        // user's declared Error sum.
        assert!(
            rendered.contains("MyProgramError::MathOverflow"),
            "expected program-specific Error enum; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("ErrorCode::MathOverflow"),
            "should not reference the legacy non-existent ErrorCode enum; got:\n{rendered}"
        );
    }

    #[test]
    fn cpi_emits_anchor_spl_mint_to_with_authority_renaming() {
        // Spec exposes `mint_authority` per SPL Token docs; anchor_spl's
        // MintTo struct calls the same slot `authority`. The codegen
        // boundary maps the names.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler mint_to (amount : U64) {
    discriminant "0x07"
    accounts {
      mint            : writable
      to              : writable, type token
      mint_authority  : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { stash : U64 }
type Error | E

handler do_mint (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    the_mint       : writable
    holder_ta      : writable, type token
    minter         : signer
    token_program  : program
  }
  call Token.mint_to(mint = the_mint, to = holder_ta, mint_authority = minter, amount = n)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(
            rendered.contains("anchor_spl::token::{self, MintTo}"),
            "should use MintTo struct; got:\n{rendered}"
        );
        // anchor_spl uses `authority`; spec uses `mint_authority` — the
        // mapping should land the call-site `minter` value at the
        // `authority` field. Padding may insert extra whitespace before
        // `self`, so we check the substring on each side independently.
        assert!(
            rendered.contains("self.minter.to_account_info()"),
            "minter should be wired into the cpi_accounts struct; got:\n{rendered}"
        );
        assert!(
            rendered.contains("authority:"),
            "MintTo struct should use field name `authority`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("token::mint_to(CpiContext::new(cpi_program, cpi_accounts), n)"),
            "should invoke token::mint_to with the amount; got:\n{rendered}"
        );
    }

    #[test]
    fn cpi_emits_anchor_spl_burn() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler burn (amount : U64) {
    discriminant "0x08"
    accounts {
      from      : writable, type token
      mint      : writable
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_burn (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    holder_ta      : writable, type token
    the_mint       : writable
    holder         : signer
    token_program  : program
  }
  call Token.burn(from = holder_ta, mint = the_mint, authority = holder, amount = n)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_burn").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(rendered.contains("anchor_spl::token::{self, Burn}"));
        assert!(rendered.contains("token::burn(CpiContext::new"));
        // Padding aligns colons across fields; use a substring that's
        // independent of whitespace.
        assert!(
            rendered.contains("self.holder_ta.to_account_info()"),
            "burn's `from` should resolve to self.holder_ta; got:\n{rendered}"
        );
        assert!(rendered.contains("authority: self.holder.to_account_info()"));
    }

    #[test]
    fn cpi_emits_anchor_spl_initialize_account_no_amount() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler initialize_account {
    discriminant "0x01"
    accounts {
      account : writable
      mint    : readonly
      owner   : readonly
      rent    : readonly
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_init : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    new_acct       : writable
    the_mint       : writable
    the_owner      : writable
    rent_sysvar    : writable
    token_program  : program
  }
  call Token.initialize_account(account = new_acct, mint = the_mint, owner = the_owner, rent = rent_sysvar)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(rendered.contains("InitializeAccount"));
        // No scalar arg — the invocation has no second positional parameter.
        assert!(
            rendered.contains(
                "token::initialize_account(CpiContext::new(cpi_program, cpi_accounts))?;"
            ),
            "no-amount handler should not get a trailing argument; got:\n{rendered}"
        );
        // Owner-as-authority renaming.
        assert!(
            rendered.contains("self.the_owner.to_account_info()"),
            "the_owner should be wired in; got:\n{rendered}"
        );
        assert!(
            rendered.contains("authority:"),
            "InitializeAccount should use field name `authority` for the owner slot; got:\n{rendered}"
        );
    }

    #[test]
    fn cpi_emits_anchor_spl_close_account_no_amount() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler close_account {
    discriminant "0x09"
    accounts {
      account     : writable, type token
      destination : writable
      authority   : signer
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_close : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    target_acct    : writable, type token
    sweep_target   : writable
    closer         : signer
    token_program  : program
  }
  call Token.close_account(account = target_acct, destination = sweep_target, authority = closer)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(rendered.contains("CloseAccount"));
        assert!(
            rendered.contains("token::close_account(CpiContext::new(cpi_program, cpi_accounts))?;")
        );
    }

    #[test]
    fn cpi_resolves_state_field_amount_to_self_state_field() {
        // The amount arg references a state field — the emitted code should
        // bind it as self.<state_acct>.<field>, not bare.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

type State | Active of { stash : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    state         : writable
    src           : writable, type token
    dst           : writable, type token
    auth          : signer
    token_program : program
  }
  call Token.transfer(from = src, to = dst, amount = stash, authority = auth)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(
            rendered.contains("self.state.stash"),
            "state-field amount must resolve to self.<state_acct>.<field>; got:\n{rendered}"
        );
    }
}
