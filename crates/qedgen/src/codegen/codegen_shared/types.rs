use super::*;

/// Per-framework strings for the surface that differs between targets
/// (imports, ctx type, return type, lifetime, program-mod visibility,
/// discriminator attribute). All other generated content is identical —
/// the frameworks share the accounts-method forwarder idiom.
#[derive(Clone, Copy)]
pub(crate) struct FrameworkSurface {
    pub(crate) target: Target,
    /// Crate-root attributes line, e.g. `"#![no_std]\n\n"`. Empty for
    /// targets that build against std.
    pub(crate) crate_attrs: &'static str,
    /// `"use anchor_lang::prelude::*;\n"` or
    /// `"use quasar_lang::prelude::*;\n"`. Caller appends the trailing
    /// blank line (some generators add additional imports first).
    pub(crate) prelude_import: &'static str,
    /// Type written as `<context_type>::<X>` in handler signatures —
    /// `"Context"` (Anchor) or `"Ctx"` (Quasar).
    pub(crate) context_type: &'static str,
    /// Handler return type — `"Result<()>"` (Anchor; the `Result`
    /// alias from `anchor_lang::prelude` defaults the error to
    /// `anchor_lang::error::Error`) or `"Result<(), ProgramError>"`
    /// (Quasar).
    pub(crate) handler_result_type: &'static str,
    /// Lifetime threaded into `#[derive(Accounts)]` structs and impl
    /// blocks. Anchor uses `"'info"`; Quasar's `Account<()>` doesn't
    /// need one and uses `""`.
    pub(crate) accounts_lifetime: &'static str,
    /// Visibility keyword for the `#[program]` mod — Anchor convention
    /// is `pub mod`, Quasar is bare `mod`.
    pub(crate) program_mod_vis: &'static str,
    /// True when each handler in the `#[program]` mod needs an
    /// `#[instruction(discriminator = N)]` attribute. Quasar requires
    /// it; Anchor auto-derives.
    pub(crate) explicit_handler_discriminator: bool,
    /// True when each `#[account]` struct in `state.rs` needs an
    /// explicit `discriminator = N` parameter (Quasar) vs Anchor's
    /// auto-derived form.
    pub(crate) explicit_account_discriminator: bool,
}

impl FrameworkSurface {
    pub(crate) fn for_target(target: Target) -> Self {
        match target {
            Target::Anchor => FrameworkSurface {
                target,
                // Anchor's `#[program]` macro references cfgs (e.g.
                // `anchor-debug`) undeclared in the generated Cargo.toml;
                // the warnings are Anchor's, not qedgen's, and drown out
                // real diagnostics. Suppress at the crate root.
                crate_attrs: "#![allow(unexpected_cfgs)]\n\n",
                prelude_import: "use anchor_lang::prelude::*;\n",
                context_type: "Context",
                handler_result_type: "Result<()>",
                accounts_lifetime: "'info",
                program_mod_vis: "pub mod",
                explicit_handler_discriminator: false,
                explicit_account_discriminator: false,
            },
            Target::Quasar => FrameworkSurface {
                target,
                // `no_std` only on-chain; host builds keep std for the
                // panic_handler / global_allocator (Quasar supplies the
                // solana-target ones via `panic_handler!()` / `no_alloc!()`).
                // `unexpected_cfgs` suppresses cfg warnings from quasar's
                // macros (undeclared `target_os = "solana"` / `feature =
                // "alloc"`) — external framework noise, same as Anchor's.
                crate_attrs:
                    "#![allow(unexpected_cfgs)]\n#![cfg_attr(any(target_os = \"solana\", target_arch = \"bpf\"), no_std)]\n\n",
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
            Target::Pinocchio => FrameworkSurface {
                target,
                // Same host-std / on-chain-no_std split as Quasar;
                // pinocchio's `entrypoint!` supplies the on-chain panic
                // handler + allocator.
                crate_attrs:
                    "#![allow(unexpected_cfgs)]\n#![cfg_attr(any(target_os = \"solana\", target_arch = \"bpf\"), no_std)]\n\n",
                prelude_import:
                    "use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};\n",
                // Pinocchio has no `Context` wrapper and no `#[program]`
                // mod (free `process_instruction` byte-dispatch instead);
                // these two fields are unused and gated on the target.
                context_type: "",
                handler_result_type: "Result<(), ProgramError>",
                accounts_lifetime: "'a",
                program_mod_vis: "",
                // 1-byte leading discriminant dispatch in process_instruction.
                explicit_handler_discriminator: true,
                explicit_account_discriminator: true,
            },
        }
    }

    /// Render the lifetime parameter list for a `#[derive(Accounts)]`
    /// struct or impl block — e.g. `"<'info>"` (Anchor) or `""`
    /// (Quasar).
    pub(crate) fn lifetime_params(&self) -> String {
        if self.accounts_lifetime.is_empty() {
            String::new()
        } else {
            format!("<{}>", self.accounts_lifetime)
        }
    }

    pub(crate) fn is_quasar(&self) -> bool {
        matches!(self.target, Target::Quasar)
    }

    pub(crate) fn is_pinocchio(&self) -> bool {
        matches!(self.target, Target::Pinocchio)
    }

    /// Pinocchio account-field type: every field is a raw `&'a AccountInfo`
    /// (no typed wrapper); typing happens via `zeropod` decode inside
    /// `.handler()`. Shared by all the `*_type` helpers below.
    pub(crate) fn pinocchio_account_type(&self) -> String {
        format!("&{} AccountInfo", self.accounts_lifetime)
    }

    /// Per-target import line for SPL token / mint types. Selects only the
    /// names the caller flagged as needed (`has_token` → `Token` +
    /// Anchor's `TokenAccount`; `has_mint` → `Mint`) so unused-import
    /// warnings don't pile up on the rendered scaffold.
    pub(crate) fn token_imports(&self, has_token: bool, has_mint: bool) -> String {
        if !has_token && !has_mint {
            return String::new();
        }
        match self.target {
            Target::Anchor => {
                let mut names: Vec<&str> = Vec::with_capacity(3);
                if has_mint {
                    names.push("Mint");
                }
                if has_token {
                    names.push("Token");
                    names.push("TokenAccount");
                }
                if names.len() == 1 {
                    format!("use anchor_spl::token::{};\n", names[0])
                } else {
                    format!("use anchor_spl::token::{{{}}};\n", names.join(", "))
                }
            }
            Target::Quasar => {
                let mut names: Vec<&str> = Vec::with_capacity(2);
                if has_token {
                    names.push("Token");
                }
                if has_mint {
                    names.push("Mint");
                }
                if names.len() == 1 {
                    format!("use quasar_spl::{};\n", names[0])
                } else {
                    format!("use quasar_spl::{{{}}};\n", names.join(", "))
                }
            }
            Target::Pinocchio => String::new(),
        }
    }

    /// True when the per-handler scaffold needs to import the bumps
    /// struct from the crate root. Anchor places the `<Pascal>Bumps`
    /// struct alongside the `<Pascal>` accounts struct in `lib.rs`, so
    /// handler files reach back into the crate root for both. Quasar
    /// keeps the accounts struct (and bumps, when present) inside
    /// `instructions/<name>.rs`, so no cross-module import is needed.
    pub(crate) fn needs_bumps_import(&self, handler: &ParsedHandler) -> bool {
        matches!(self.target, Target::Anchor) && handler.has_bumps()
    }

    pub(crate) fn signer_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            format!("&{} {}Signer", lt, mut_prefix(mutable))
        } else {
            format!("Signer<{}>", lt)
        }
    }

    pub(crate) fn program_type(
        &self,
        name: &str,
        account_type: Option<&str>,
        mutable: bool,
    ) -> String {
        let lt = self.accounts_lifetime;
        // A `program` account named `token_program` (convention) or marked
        // `type token` (explicit) needs `Program<Token>` so the handler can
        // call `.transfer()` etc.; anything else stays `Program<System>`.
        let is_token = name == "token_program" || account_type == Some("token");
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            let inner = if is_token { "Token" } else { "System" };
            format!("&{} {}Program<{}>", lt, mut_prefix(mutable), inner)
        } else if is_token {
            format!("Program<{}, Token>", lt)
        } else {
            format!("Program<{}, System>", lt)
        }
    }

    pub(crate) fn token_account_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            format!("&{} {}Account<Token>", lt, mut_prefix(mutable))
        } else {
            format!("Account<{}, TokenAccount>", lt)
        }
    }

    pub(crate) fn mint_account_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            format!("&{} {}Account<Mint>", lt, mut_prefix(mutable))
        } else {
            format!("Account<{}, Mint>", lt)
        }
    }

    pub(crate) fn state_account_type(&self, state_name: &str, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            // Raw &AccountInfo; state decoded via zeropod inside .handler().
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            format!("&{} {}Account<{}>", lt, mut_prefix(mutable), state_name)
        } else {
            format!("Account<{}, {}>", lt, state_name)
        }
    }

    /// Imported account type via the local mirror at
    /// `crate::imported::<ns>::<source_type>`. Anchor target only.
    pub(crate) fn imported_account_type(
        &self,
        ns: &str,
        source_type: &str,
        _mutable: bool,
    ) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        format!("Account<{}, crate::imported::{}::{}>", lt, ns, source_type)
    }

    pub(crate) fn unchecked_account_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            format!("&{} {}UncheckedAccount", lt, mut_prefix(mutable))
        } else {
            format!("AccountInfo<{}>", lt)
        }
    }

    pub(crate) fn error_expr(&self, enum_name: &str, variant: &str) -> String {
        match self.target {
            Target::Anchor => format!("{}::{}.into()", enum_name, variant),
            // Both Quasar and Pinocchio return bare `ProgramError`; the
            // generated error enum impls `From<Enum> for ProgramError`.
            Target::Quasar | Target::Pinocchio => {
                format!("ProgramError::from({}::{})", enum_name, variant)
            }
        }
    }

    /// Generic "predicate violated, no specific error code" expression for
    /// bare `requires` clauses (no `else <Error>`); each surface needs the
    /// type-correct form for its `Result<(), _>` return shape.
    pub(crate) fn generic_error_expr(&self) -> &'static str {
        match self.target {
            Target::Anchor => "anchor_lang::error::Error::from(ProgramError::Custom(0xFF))",
            Target::Quasar | Target::Pinocchio => "ProgramError::Custom(0xFF)",
        }
    }

    pub(crate) fn guard_accounts_import(&self) -> &'static str {
        match self.target {
            Target::Anchor => "use crate::*;\n\n",
            // Pinocchio keeps the per-handler accounts struct in
            // `instructions/<name>.rs` like Quasar.
            Target::Quasar | Target::Pinocchio => "use crate::instructions::*;\n\n",
        }
    }

    pub(crate) fn account_key_expr(&self, account_name: &str) -> String {
        match self.target {
            Target::Anchor => format!("ctx.{}.key()", account_name),
            Target::Quasar => format!("(*ctx.{}.to_account_view().address())", account_name),
            // pinocchio's AccountInfo::key() returns &Pubkey ([u8; 32]).
            Target::Pinocchio => format!("ctx.{}.key()", account_name),
        }
    }

    pub(crate) fn token_owner_expr(&self, token_account_name: &str) -> String {
        match self.target {
            Target::Anchor => format!("ctx.{}.owner", token_account_name),
            Target::Quasar => format!("(*ctx.{}.owner())", token_account_name),
            // Pinocchio must read the SPL token-account owner from the
            // account DATA (not AccountInfo::owner, which is the owning
            // program); the zeropod-decode form isn't emitted yet.
            Target::Pinocchio => {
                unreachable!("pinocchio token-owner read lands with guard codegen — slice 6 step 4")
            }
        }
    }

    pub(crate) fn authority_check_expr(
        &self,
        token_account: &str,
        authority_account: &str,
    ) -> String {
        format!(
            "{} != {}",
            self.token_owner_expr(token_account),
            self.account_key_expr(authority_account)
        )
    }
}

pub(crate) fn mut_prefix(mutable: bool) -> &'static str {
    if mutable {
        "mut "
    } else {
        ""
    }
}

/// Render the Rust type for a `#[derive(Accounts)]` field. When
/// `is_state_account` (the handler's writable state holder per
/// `find_state_account`) emit the typed `Account<…, {state_name}>` so
/// `self.<acct>.<field>` resolves through the inner data; other accounts
/// get the framework's neutral placeholder types.
pub(crate) fn render_account_field_type(
    acct: &crate::check::ParsedHandlerAccount,
    surface: &FrameworkSurface,
    is_state_account: bool,
    state_name: &str,
) -> String {
    if acct.is_signer {
        surface.signer_type(acct.is_writable)
    } else if acct.is_program {
        surface.program_type(&acct.name, acct.account_type.as_deref(), acct.is_writable)
    } else if acct.account_type.as_deref() == Some("token") {
        surface.token_account_type(acct.is_writable)
    } else if acct.account_type.as_deref() == Some("mint") {
        surface.mint_account_type(acct.is_writable)
    } else if let (Some(ns), Some(ty)) = (&acct.imported_namespace, &acct.account_type) {
        // Imported account type — routes through the local mirror at
        // `src/imported/<ns>.rs` so the wrapper layout matches the foreign
        // program. Writability is driven by `#[account(mut)]`, not the
        // wrapper choice.
        surface.imported_account_type(ns, ty, acct.is_writable)
    } else if is_state_account {
        surface.state_account_type(state_name, acct.is_writable)
    } else {
        surface.unchecked_account_type(acct.is_writable)
    }
}

/// Compute a path, as a string, from a program `Cargo.toml` directory to the
/// spec file. This value is embedded verbatim in the `#[qed(spec = "...")]`
/// attribute and resolved at compile time relative to `CARGO_MANIFEST_DIR`.
///
/// Best-effort: if the spec isn't under a path we can express relatively,
/// fall back to the absolute path (works as long as the repo doesn't move).
pub(crate) fn relative_spec_path(spec_path: &Path, manifest_dir: &Path) -> String {
    // Canonicalize both; fall back to the raw paths on failure.
    let spec = spec_path
        .canonicalize()
        .unwrap_or_else(|_| spec_path.to_path_buf());
    let manifest = manifest_dir
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.to_path_buf());
    let spec_components: Vec<_> = spec.components().collect();
    let manifest_components: Vec<_> = manifest.components().collect();

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

#[derive(Clone, Copy)]
pub(crate) enum TypeMapContext {
    Standalone,
    Anchor,
    Quasar,
}

/// Split a `Map[BOUND] T` DSL compound into `(bound_src, inner_src)`,
/// both trimmed. Tolerates whitespace between `Map` and `[`. `None` when
/// the string isn't a well-formed Map compound — callers own their
/// malformed-input policy (bail / return-None / fall through), which is
/// why this returns an Option instead of erroring.
pub(crate) fn split_map_type(dsl_type: &str) -> Option<(&str, &str)> {
    let rest = dsl_type.strip_prefix("Map")?;
    let rest = rest.trim_start().strip_prefix('[')?;
    let close = rest.find(']')?;
    Some((rest[..close].trim(), rest[close + 1..].trim()))
}

/// Map a DSL type to its standalone Rust equivalent: primitives,
/// `Map[N] T` → `[T; N]` (N = literal or declared constant; inner T
/// recurses), `Fin[N]` → `usize`, transitive type aliases, and record /
/// sum-type names returned as-is (the generator emits the matching
/// struct/enum declarations).
///
/// Errors on anything else rather than silently passing it through —
/// fall-through was the root cause of the bug class where types like
/// `U16` or `Map[N] UserAccount` leaked verbatim into generated Rust.
pub fn map_type(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_standalone(dsl_type, spec)
}

/// Shared type-shape queries used by the Rust harness emitters. Keeping
/// these on `ParsedSpec` avoids making one backend (for example proptest)
/// the accidental owner of generic DSL type behavior.
pub(crate) trait DslTypeExt {
    fn resolve_map_bound(&self, bound: &str) -> Result<String>;
    fn default_value_for_type(&self, dsl_type: &str) -> Option<String>;
}

impl DslTypeExt for ParsedSpec {
    fn resolve_map_bound(&self, bound: &str) -> Result<String> {
        resolve_map_bound(bound, self)
    }

    fn default_value_for_type(&self, dsl_type: &str) -> Option<String> {
        default_value_for_type(dsl_type, self)
    }
}

pub(crate) fn ensure_parent_dir(output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

pub(crate) fn write_generated_file(output_path: &Path, content: &str) -> Result<()> {
    ensure_parent_dir(output_path)?;
    if output_path.extension().is_some_and(|ext| ext == "rs") {
        std::fs::write(output_path, format_rust_source(content))?;
    } else {
        std::fs::write(output_path, content)?;
    }
    Ok(())
}

/// Best-effort `rustfmt` pass over generated Rust source. Returns the input
/// unchanged when rustfmt is unavailable or rejects the source, warning once
/// per process — formatting is presentation; the snapshot/smoke suites own
/// correctness. Runs at every `.rs` write AND before body-hash stamping
/// ([`crate::codegen_shared::precompute_body_hash`] callers): the
/// `#[qed(verified, hash = …)]` leg hashes the canonical token stream, and
/// rustfmt is not token-neutral (trailing commas), so the stamp must be
/// computed over the formatted text. rustfmt is idempotent, so the double
/// pass is safe.
pub(crate) fn format_rust_source(source: &str) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn warn_once(msg: &str) {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| eprintln!("warning: {msg}"));
    }

    let child = Command::new("rustfmt")
        .args(["--edition", "2021", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        warn_once("rustfmt not found on PATH; generated Rust left unformatted");
        return source.to_string();
    };
    // rustfmt consumes all of stdin before emitting, so a straight
    // write-then-wait cannot deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(source.as_bytes()).is_err() {
            let _ = child.wait();
            return source.to_string();
        }
    }
    match child.wait_with_output() {
        Ok(output) if output.status.success() => match String::from_utf8(output.stdout) {
            Ok(formatted) if !formatted.is_empty() => formatted,
            _ => source.to_string(),
        },
        _ => {
            // Parse-rejected source is a codegen bug the compile gates will
            // surface; keep the bytes so the user sees what was generated.
            warn_once("rustfmt rejected generated source; leaving it unformatted");
            source.to_string()
        }
    }
}

pub fn map_type_standalone(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_with_context(dsl_type, spec, TypeMapContext::Standalone)
}

pub(crate) fn map_type_anchor(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_with_context(dsl_type, spec, TypeMapContext::Anchor)
}

pub(crate) fn map_type_quasar(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_with_context(dsl_type, spec, TypeMapContext::Quasar)
}

pub(crate) fn map_type_for_target(
    dsl_type: &str,
    spec: &ParsedSpec,
    target: Target,
) -> Result<String> {
    match target {
        Target::Anchor => map_type_anchor(dsl_type, spec),
        Target::Quasar => map_type_quasar(dsl_type, spec),
        // Instruction params decode from raw bytes into plain Rust scalars —
        // the standalone mapping. State-field pod types are a separate
        // state-emission concern.
        Target::Pinocchio => map_type_standalone(dsl_type, spec),
    }
}

pub(crate) fn map_type_with_context(
    dsl_type: &str,
    spec: &ParsedSpec,
    context: TypeMapContext,
) -> Result<String> {
    let dsl_type = dsl_type.trim();

    // Compound type: Map[BOUND] T → [T; N]
    if dsl_type.starts_with("Map") {
        let Some((bound_src, inner_src)) = split_map_type(dsl_type) else {
            anyhow::bail!(
                "malformed Map type `{}` — expected `Map[BOUND] T`",
                dsl_type
            );
        };
        let n = resolve_map_bound(bound_src, spec)?;
        let inner_rust = map_type_with_context(inner_src, spec, context)?;
        return Ok(format!("[{inner_rust}; {n}]"));
    }

    // Fin[N] → usize. N is informational (bound for index-type safety in
    // the DSL); in Rust we just use usize.
    if let Some(rest) = dsl_type.strip_prefix("Fin") {
        let rest = rest.trim_start();
        if rest.starts_with('[') {
            return Ok("usize".to_string());
        }
    }

    // `Vec T` → `Vec<T>`; `Option T` → `Option<T>` (from `TypeRef::Param`,
    // rendered as a space-separated string). Recurse on the inner so it maps
    // per-context (e.g. `Option Pubkey` → `Option<Pubkey>` on Anchor,
    // `Option<[u8; 32]>` standalone). The whitespace boundary keeps `Vector` /
    // `Optional` (named types) from matching.
    for (kw, wrap) in [("Vec", "Vec"), ("Option", "Option")] {
        if let Some(rest) = dsl_type.strip_prefix(kw) {
            if rest.starts_with(char::is_whitespace) {
                let inner = rest.trim();
                if !inner.is_empty() {
                    let inner_rust = map_type_with_context(inner, spec, context)?;
                    return Ok(format!("{wrap}<{inner_rust}>"));
                }
            }
        }
    }

    // Primitive match — check first so `U8` etc. never hit the alias path.
    if let Some(rust) = primitive_map(dsl_type, context) {
        return Ok(rust.to_string());
    }

    // Type alias: `type Foo = Bar` — recurse on the RHS. Transitive.
    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return map_type_with_context(rhs, spec, context);
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

/// Map a DSL type to its Quasar-Pod Rust equivalent. Used inside Quasar's
/// zero-copy `#[account]` and nested record structs where every field must
/// have alignment 1. `u64` becomes `PodU64`, etc. Non-integer types fall
/// through to `map_type`.
pub fn map_type_pod(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    let dsl_type = dsl_type.trim();
    // Fin[N] is a bounded index type; usize has 8-byte alignment on most
    // targets, so pack it as PodU32 for the alignment-1 constraint. Wider
    // bounds would need PodU64 — the bound itself is informational here.
    if dsl_type.starts_with("Fin") {
        return Ok("PodU32".to_string());
    }
    if let Some(pod) = primitive_pod_map(dsl_type) {
        return Ok(pod.to_string());
    }
    if let Some(rust) = primitive_map(dsl_type, TypeMapContext::Quasar) {
        return Ok(rust.to_string());
    }
    // Type alias: `type Foo = Bar` — recurse on the RHS so an alias like
    // `AccountIdx = Fin[N]` ends up as `PodU32` instead of `usize`.
    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return map_type_pod(rhs, spec);
    }
    // Fall back to map_type for compound / user-defined types — those
    // don't need (and can't take) the pod conversion.
    map_type_quasar(dsl_type, spec)
}

pub(crate) fn primitive_pod_map(dsl_type: &str) -> Option<&'static str> {
    Some(match dsl_type {
        "U16" => "PodU16",
        "U32" => "PodU32",
        "U64" => "PodU64",
        "U128" => "PodU128",
        "I16" => "PodI16",
        "I32" => "PodI32",
        "I64" => "PodI64",
        "I128" => "PodI128",
        "Bool" => "PodBool",
        // u8, i8 already alignment 1; no Pod wrapper needed.
        _ => return None,
    })
}

/// Map a DSL primitive name to its Rust equivalent, if one exists. Factored
/// out of `map_type` so both the primitive fast-path and the alias-recursion
/// base case can share it.
pub(crate) fn primitive_map(dsl_type: &str, context: TypeMapContext) -> Option<&'static str> {
    Some(match dsl_type {
        // Standalone harnesses (proptest/kani/unit tests) lower Pubkey to
        // `[u8; 32]` — structurally compatible with Solana's Pubkey
        // newtype, and proptest's uniform32 strategy already produces it.
        // Anchor/Quasar program targets keep the real `Pubkey` type.
        "Pubkey" => match context {
            TypeMapContext::Anchor | TypeMapContext::Quasar => "Pubkey",
            TypeMapContext::Standalone => "[u8; 32]",
        },
        // Opaque byte tokens (#191): hashes/digests (`Bytes32`) and
        // signatures / recovered secp keys (`Bytes64`). No framework newtype
        // exists, so they lower to raw arrays in EVERY context.
        "Bytes32" => "[u8; 32]",
        "Bytes64" => "[u8; 64]",
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

/// Resolve the bound inside `Map[BOUND] T`: a numeric literal, a declared
/// constant, or a unit-only sum type (array length = variant count;
/// readers index by the variant's source-declared ordinal). Mixed-variant
/// sums are rejected at the lint side; codegen never sees them.
pub(crate) fn resolve_map_bound(bound: &str, spec: &ParsedSpec) -> Result<String> {
    let bound = bound.trim();
    if bound.chars().all(|c| c.is_ascii_digit()) && !bound.is_empty() {
        return Ok(bound.to_string());
    }
    if let Some((_, value)) = spec.constants.iter().find(|(n, _)| n == bound) {
        return Ok(value.clone());
    }
    // Enum-typed bound: variant count = array size. Unit-only check
    // mirrors the lint at check.rs so codegen never silently widens what
    // the lint accepts.
    if let Some(sum) = spec.sum_types.iter().find(|s| s.name == bound) {
        if sum.variants.iter().all(|v| v.fields.is_empty()) {
            return Ok(sum.variants.len().to_string());
        }
    }
    anyhow::bail!(
        "Map bound `{}` is not a numeric literal, not declared as a `const`, and not a unit-only enum type",
        bound
    )
}

/// Type-aware default for a generated `State { ... }` literal.
///
/// `Map[N] T` becomes `[<default of T>; N]`, aliases recurse, records use
/// `<Name>::default()`, and payload-variant sums intentionally return
/// `None` so rustc reports the exact missing field instead of accepting an
/// arbitrary placeholder.
pub(crate) fn default_value_for_type(dsl_type: &str, spec: &ParsedSpec) -> Option<String> {
    let dsl_type = dsl_type.trim();

    if dsl_type.starts_with("Map") {
        if let Some((bound_src, inner_src)) = split_map_type(dsl_type) {
            if let Ok(n) = resolve_map_bound(bound_src, spec) {
                let inner_default = default_value_for_type(inner_src, spec)?;
                return Some(format!("[{}; {}]", inner_default, n));
            }
        }
        return None;
    }

    // #327 — structured parameterized forms. Before the canonical type
    // IR these fell to the trailing `"0"` fallback, seeding
    // `field: 0` against a `Vec<T>` / `Option<T>` slot (E0308).
    if dsl_type.strip_prefix("Vec ").is_some() {
        return Some("Vec::new()".to_string());
    }
    if dsl_type.strip_prefix("Option ").is_some() {
        return Some("None".to_string());
    }

    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return default_value_for_type(rhs, spec);
    }

    if spec.records.iter().any(|r| r.name == dsl_type) {
        return Some(format!("{}::default()", dsl_type));
    }

    if spec
        .sum_types
        .iter()
        .any(|s| s.name == dsl_type && s.variants.iter().any(|v| !v.fields.is_empty()))
    {
        return None;
    }

    if dsl_type.starts_with("Fin[") {
        return Some("0".to_string());
    }

    if dsl_type == "Pubkey" || dsl_type == "Bytes32" {
        return Some("[0u8; 32]".to_string());
    }
    if dsl_type == "Bytes64" {
        return Some("[0u8; 64]".to_string());
    }
    // `Bool` lowers to a Rust `bool`, so its default is `false` — the
    // numeric `"0"` fallback below produces `field: 0` which fails to
    // compile (E0308). The unit-test seeder special-cased this locally;
    // the proptest/kani seeders route through here, so fix it at the
    // shared source.
    if dsl_type == "Bool" {
        return Some("false".to_string());
    }

    Some("0".to_string())
}

/// Sanitize a field-path string (e.g. `accounts[i].active`) into a legal
/// Rust identifier stem for `fn verify_*` names: non-identifier characters
/// become `_`; consecutive and trailing `_` collapse.
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

/// Per-file spec hash from the fingerprint table; empty when the key was
/// never inserted (the banner then renders hash-less).
pub(crate) fn fingerprint_hash(fp: &SpecFingerprint, file_key: &str) -> String {
    fp.file_hashes.get(file_key).cloned().unwrap_or_default()
}

/// Format the "GENERATED BY QEDGEN" marker with the per-file spec hash.
/// Thin wrapper around `crate::banner::banner` that resolves the hash from
/// the fingerprint table by file_key.
pub(crate) fn marker(label: &str, fp: &SpecFingerprint, file_key: &str) -> String {
    crate::banner::banner(Some(label), &fingerprint_hash(fp, file_key))
}

/// Unlabeled variant of [`marker`] — the harness banners (kani, proptest,
/// crucible) carry no label text.
pub(crate) fn marker_unlabeled(fp: &SpecFingerprint, file_key: &str) -> String {
    crate::banner::banner(None, &fingerprint_hash(fp, file_key))
}
