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
    target: Target,
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
                target,
                // Anchor's `#[program]` macro expands to references to
                // unstable `cfg(feature = "anchor-debug")` etc. that
                // aren't declared in the generated `Cargo.toml`. The
                // warnings come from anchor itself, not qedgen, and
                // they drown out actual diagnostics on the rendered
                // scaffold. Suppress at the crate root.
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
                // `no_std` only for the on-chain (Solana/BPF) build. Host
                // builds (`cargo check`/`cargo test`) keep std so the host
                // gets a panic_handler / global_allocator from the standard
                // library. Quasar provides solana-target panic_handler /
                // global_allocator below via `panic_handler!()` / `no_alloc!()`.
                //
                // The `unexpected_cfgs` allow suppresses cfg warnings
                // from quasar's `no_alloc` / `panic_handler` macros,
                // which gate on `target_os = "solana"` / `feature =
                // "alloc"` — values that aren't declared in the
                // generated Cargo.toml. Same shape as Anchor's
                // anchor-debug noise; treat both as external framework
                // diagnostics so genuine warnings on the rendered
                // scaffold stay visible.
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

    fn is_quasar(&self) -> bool {
        matches!(self.target, Target::Quasar)
    }

    /// Per-target import line for SPL token / mint types. Selects only
    /// the names the caller has flagged as needed so unused-import
    /// warnings don't pile up on the rendered scaffold:
    ///
    /// - `has_token`: any handler has a token account or a `token_program`
    ///   account (needs `Token` for the program type; needs `TokenAccount`
    ///   on Anchor for the typed account wrapper).
    /// - `has_mint`: any handler has a mint account (needs `Mint`).
    ///
    /// Returns `String` rather than `&'static str` because the import
    /// list is composed at call time. Empty when neither flag is set.
    fn token_imports(&self, has_token: bool, has_mint: bool) -> String {
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
    fn needs_bumps_import(&self, handler: &ParsedHandler) -> bool {
        matches!(self.target, Target::Anchor) && handler.has_bumps()
    }

    fn signer_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_quasar() {
            format!("&{} {}Signer", lt, mut_prefix(mutable))
        } else {
            format!("Signer<{}>", lt)
        }
    }

    fn program_type(&self, name: &str, account_type: Option<&str>, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        // Token-program detection is shared between targets: a `program`
        // account named `token_program` (the convention) or carrying the
        // `type token` annotation (explicit) needs `Program<Token>` so the
        // generated handler can call `.transfer()` / `.mint_to()` etc.
        // Anything else stays `Program<System>`.
        let is_token = name == "token_program" || account_type == Some("token");
        if self.is_quasar() {
            let inner = if is_token { "Token" } else { "System" };
            format!("&{} {}Program<{}>", lt, mut_prefix(mutable), inner)
        } else if is_token {
            format!("Program<{}, Token>", lt)
        } else {
            format!("Program<{}, System>", lt)
        }
    }

    fn token_account_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_quasar() {
            format!("&{} {}Account<Token>", lt, mut_prefix(mutable))
        } else {
            format!("Account<{}, TokenAccount>", lt)
        }
    }

    fn mint_account_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_quasar() {
            format!("&{} {}Account<Mint>", lt, mut_prefix(mutable))
        } else {
            format!("Account<{}, Mint>", lt)
        }
    }

    fn state_account_type(&self, state_name: &str, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_quasar() {
            format!("&{} {}Account<{}>", lt, mut_prefix(mutable), state_name)
        } else {
            format!("Account<{}, {}>", lt, state_name)
        }
    }

    fn unchecked_account_type(&self, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_quasar() {
            format!("&{} {}UncheckedAccount", lt, mut_prefix(mutable))
        } else {
            format!("AccountInfo<{}>", lt)
        }
    }

    fn error_expr(&self, enum_name: &str, variant: &str) -> String {
        match self.target {
            Target::Anchor => format!("{}::{}.into()", enum_name, variant),
            Target::Quasar => format!("ProgramError::from({}::{})", enum_name, variant),
            Target::Pinocchio => unreachable!(),
        }
    }

    /// Generic "predicate violated, no specific error code" expression for
    /// bare `requires` clauses (no `else <Error>`). Pre-v2.14 emitted
    /// `debug_assert!` (silent no-op in release); v2.14+ emits a real
    /// runtime check that returns this error. Each surface needs the
    /// type-correct form for its `Result<(), _>` return shape.
    fn generic_error_expr(&self) -> &'static str {
        match self.target {
            Target::Anchor => "anchor_lang::error::Error::from(ProgramError::Custom(0xFF))",
            Target::Quasar => "ProgramError::Custom(0xFF)",
            Target::Pinocchio => unreachable!(),
        }
    }

    fn guard_accounts_import(&self) -> &'static str {
        match self.target {
            Target::Anchor => "use crate::*;\n\n",
            Target::Quasar => "use crate::instructions::*;\n\n",
            Target::Pinocchio => unreachable!(),
        }
    }

    fn account_key_expr(&self, account_name: &str) -> String {
        match self.target {
            Target::Anchor => format!("ctx.{}.key()", account_name),
            Target::Quasar => format!("(*ctx.{}.to_account_view().address())", account_name),
            Target::Pinocchio => unreachable!(),
        }
    }

    fn token_owner_expr(&self, token_account_name: &str) -> String {
        match self.target {
            Target::Anchor => format!("ctx.{}.owner", token_account_name),
            Target::Quasar => format!("(*ctx.{}.owner())", token_account_name),
            Target::Pinocchio => unreachable!(),
        }
    }

    fn authority_check_expr(&self, token_account: &str, authority_account: &str) -> String {
        format!(
            "{} != {}",
            self.token_owner_expr(token_account),
            self.account_key_expr(authority_account)
        )
    }
}

fn mut_prefix(mutable: bool) -> &'static str {
    if mutable {
        "mut "
    } else {
        ""
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
    if acct.is_signer {
        surface.signer_type(acct.is_writable)
    } else if acct.is_program {
        surface.program_type(&acct.name, acct.account_type.as_deref(), acct.is_writable)
    } else if acct.account_type.as_deref() == Some("token") {
        surface.token_account_type(acct.is_writable)
    } else if acct.account_type.as_deref() == Some("mint") {
        surface.mint_account_type(acct.is_writable)
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

#[derive(Clone, Copy)]
enum TypeMapContext {
    Standalone,
    Anchor,
    Quasar,
}

/// Map a DSL type to its standalone Rust equivalent.
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
    map_type_standalone(dsl_type, spec)
}

pub fn map_type_standalone(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_with_context(dsl_type, spec, TypeMapContext::Standalone)
}

fn map_type_anchor(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_with_context(dsl_type, spec, TypeMapContext::Anchor)
}

fn map_type_quasar(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    map_type_with_context(dsl_type, spec, TypeMapContext::Quasar)
}

fn map_type_for_target(dsl_type: &str, spec: &ParsedSpec, target: Target) -> Result<String> {
    match target {
        Target::Anchor => map_type_anchor(dsl_type, spec),
        Target::Quasar => map_type_quasar(dsl_type, spec),
        Target::Pinocchio => unreachable!(),
    }
}

fn map_type_with_context(
    dsl_type: &str,
    spec: &ParsedSpec,
    context: TypeMapContext,
) -> Result<String> {
    let dsl_type = dsl_type.trim();

    // Compound type: Map[BOUND] T → [T; N]
    if let Some(rest) = dsl_type.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound_src = rest[..close].trim();
                let inner_src = rest[close + 1..].trim();
                let n = resolve_map_bound(bound_src, spec)?;
                let inner_rust = map_type_with_context(inner_src, spec, context)?;
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

fn primitive_pod_map(dsl_type: &str) -> Option<&'static str> {
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
fn primitive_map(dsl_type: &str, context: TypeMapContext) -> Option<&'static str> {
    Some(match dsl_type {
        // v2.21 Slice 3: lower Pubkey to `[u8; 32]` for Standalone
        // harnesses (proptest, kani, unit tests). This is "Option B"
        // from PRD-v2.20 §S1.3 / PRD-v2.21 §"Slice 3" — the in-state
        // workaround the P6 lint used to recommend, now applied
        // automatically. The 32-byte array is structurally compatible
        // with Solana's Pubkey (which is a `[u8; 32]` newtype), and
        // proptest's existing `prop::array::uniform32(0u8..)` strategy
        // already produces this shape.
        //
        // The Anchor user-facing program target keeps the real
        // `solana_program::Pubkey` so on-chain accounts work normally.
        // Quasar uses `Pubkey` from `quasar-lang::prelude` for the same
        // reason — both are 32-byte newtypes downstream of `[u8; 32]`.
        // The `Address` alias that v2.20 emitted for Quasar/Standalone
        // contexts is retired; unit-test scaffolds drop the
        // `type Address = [u8; 32];` line.
        "Pubkey" => match context {
            TypeMapContext::Anchor | TypeMapContext::Quasar => "Pubkey",
            TypeMapContext::Standalone => "[u8; 32]",
        },
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
/// numeric literal (e.g. `Map[16] U64`), a constant declared in the spec
/// (e.g. `Map[MAX_ACCOUNTS] U64`), or — v2.24 #20 — a unit-only sum type
/// (e.g. `Map[AddressField] ProposalSlot` where every variant has no
/// payload). The enum-bounded form lowers to a fixed-size array whose
/// length equals the variant count; downstream readers index into it
/// using the variant's source-declared ordinal (Owner = 0, Manager = 1,
/// …). Mixed-variant sums (some unit, some payload) are still rejected
/// at the lint side; the codegen never sees them.
fn resolve_map_bound(bound: &str, spec: &ParsedSpec) -> Result<String> {
    let bound = bound.trim();
    if bound.chars().all(|c| c.is_ascii_digit()) && !bound.is_empty() {
        return Ok(bound.to_string());
    }
    if let Some((_, value)) = spec.constants.iter().find(|(n, _)| n == bound) {
        return Ok(value.clone());
    }
    // v2.24 #20 — enum-typed Map bound. Use the variant count as the
    // array size. Unit-only check mirrors the lint at check.rs so the
    // codegen never silently widens what the lint accepts.
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
    out.push_str("mod instructions;\n");
    // Quasar's Accounts structs live inside instructions/<name>.rs, so
    // lib.rs needs the glob to reference them in `Context<X>`. Anchor
    // defines the structs further down in this same file, so the glob
    // would just produce an unused-import warning.
    if matches!(target, Target::Quasar) {
        out.push_str("use instructions::*;\n");
    }

    if !spec.events.is_empty() {
        out.push_str("pub mod events;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("pub mod errors;\n");
    }
    out.push_str("pub mod state;\n");
    out.push_str("pub mod guards;\n");
    if guards_use_math_helpers(spec) {
        out.push_str("pub mod math;\n");
    }
    out.push('\n');

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

        // Quasar's `#[instruction]` macro auto-converts native integers
        // (`u64` → `PodU64`, …) inside the `InstructionDataZc` struct, so
        // we can keep the user-facing handler signature in native types.
        // `usize`, however, isn't recognized — it falls through unchanged
        // and the ZC struct fails the alignment-1 assertion. Resolve
        // `Fin[N]` (and its aliases) to `u32` on Quasar so the auto-Pod
        // conversion picks it up as `PodU32`. The inner impl still takes
        // `usize` for indexing, which we cast at the dispatch boundary.
        let needs_fin_cast = |ptype: &str| -> bool {
            if !matches!(target, Target::Quasar) {
                return false;
            }
            let mut resolved = ptype.trim().to_string();
            while let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == &resolved) {
                resolved = rhs.trim().to_string();
            }
            resolved.starts_with("Fin")
        };
        for (pname, ptype) in &handler.takes_params {
            let rust_ty = if needs_fin_cast(ptype) {
                "u32".to_string()
            } else {
                map_type_for_target(ptype, spec, target)?
            };
            params.push_str(&format!(", {}: {}", pname, rust_ty));
        }

        out.push_str(&format!(
            "    pub fn {}({}) -> {} {{\n",
            handler.name, params, surface.handler_result_type
        ));

        let cast_arg = |pname: &str, ptype: &str| -> String {
            if needs_fin_cast(ptype) {
                format!("{} as usize", pname)
            } else {
                pname.to_string()
            }
        };

        if handler.has_bumps() {
            out.push_str(&format!(
                "        ctx.accounts.handler({}&ctx.bumps)\n",
                handler
                    .takes_params
                    .iter()
                    .map(|(n, t)| format!("{}, ", cast_arg(n, t)))
                    .collect::<String>()
            ));
        } else {
            out.push_str(&format!(
                "        ctx.accounts.handler({})\n",
                handler
                    .takes_params
                    .iter()
                    .map(|(n, t)| cast_arg(n, t))
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
        let has_token = spec.handlers.iter().any(|h| {
            h.accounts
                .iter()
                .any(|a| a.account_type.as_deref() == Some("token") || a.name == "token_program")
        });
        let has_mint = spec.handlers.iter().any(|h| {
            h.accounts
                .iter()
                .any(|a| a.account_type.as_deref() == Some("mint"))
        });
        let imports = surface.token_imports(has_token, has_mint);
        if !imports.is_empty() {
            out.push_str(&imports);
        }
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

/// v2.24 S5b — `true` when the spec's state is a multi-variant ADT
/// (two or more variants in a single account type) AND the spec has
/// declared the `WrongState` error variant that the new emission
/// path needs for its variant-mismatch fallthroughs.
///
/// `WrongState` works as the **migration signal**: a spec author
/// opts into the wrapper-struct + inner-enum emission by declaring
/// it in `type Error`, alongside flipping bare-field effect LHS to
/// `Variant.field` syntax. Without that declaration, codegen falls
/// back to the legacy flat-fields struct + parallel `Status` enum
/// emission — which keeps unmigrated bundled examples (escrow,
/// multisig, lending, percolator) compiling on Anchor target until
/// they migrate at their own pace.
///
/// v2.24.x follow-up: pre-fix the predicate only checked variant
/// count, which meant every multi-variant ADT spec routed through
/// the wrapper+enum path regardless of whether it had been migrated.
/// That exposed downstream emission gaps (lib.rs pda seeds
/// referencing variant-payload fields, guards.rs requires bodies
/// reaching `wrapper.X` where X moved into the inner enum) that
/// don't show up on Quasar (which stays on the flat path) but
/// break Anchor cargo check.
///
/// Single-record account types, single-variant ADTs, multi-account
/// specs, and any spec lacking `WrongState` all stay on the flat path.
fn is_multi_variant_adt_state(spec: &ParsedSpec) -> bool {
    let has_wrong_state = spec.error_codes.iter().any(|c| c == "WrongState");
    has_wrong_state
        && spec.account_types.len() == 1
        && spec
            .account_types
            .first()
            .map(|a| a.variants.len() > 1)
            .unwrap_or(false)
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
    // User-declared record types (`type T = { field : Type, ... }`) get
    // emitted as plain `#[repr(C)]` structs ahead of the account structs
    // that reference them. Without this, a state field like
    // `accounts: Map[N] Account` lowers to `[Account; N]` where `Account`
    // resolves to whatever the prelude exports (e.g. quasar's
    // `Account<T>`), shadowing the user's intended record type.
    //
    // For Quasar these records are nested inside zero-copy `#[account]`
    // structs, so all integer fields must use Pod companions (PodU64,
    // PodU128, …) so the whole struct keeps alignment 1.
    for record in &spec.records {
        out.push_str("#[repr(C)]\n");
        // Anchor: when a record is nested inside an `#[account]` struct
        // (e.g. `accounts: Map[N] Account` lowers to `[Account; N]`),
        // the `#[account]` macro derives AnchorSerialize/Deserialize
        // for the outer struct and recursively requires every field
        // type to implement them. Add the derives here so the inner
        // record satisfies that bound. Quasar nests records inside
        // zero-copy structs whose serialization comes from `#[repr(C)]`
        // alignment, not from Borsh, so the extra derives only fire
        // for the Anchor target.
        let derives = match target {
            Target::Anchor => "#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]\n",
            _ => "#[derive(Clone, Copy)]\n",
        };
        out.push_str(derives);
        out.push_str(&format!("pub struct {} {{\n", record.name));
        for (fname, ftype) in &record.fields {
            let rust_ty = match target {
                Target::Quasar => map_type_pod(ftype, spec)?,
                _ => map_type_for_target(ftype, spec, target)?,
            };
            out.push_str(&format!("    pub {}: {},\n", fname, rust_ty));
        }
        out.push_str("}\n\n");
    }

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
            out.push_str(&format!("{}pub struct {} {{\n", account_attr, struct_name));

            for (fname, ftype) in &acct.fields {
                out.push_str(&format!(
                    "    pub {}: {},\n",
                    fname,
                    map_type_for_target(ftype, spec, target)?
                ));
            }

            if acct.pda_ref.is_some() && !acct.fields.iter().any(|(n, _)| n == "bump") {
                out.push_str("    pub bump: u8,\n");
            }

            // R26: lifecycle status field. Stored as `u8` (matches the
            // `#[repr(u8)]` enum below; alignment 1 so it's safe inside a
            // Quasar zero-copy struct). Handlers `require!(status == Pre)`
            // / `status = Post` via guards.rs to enforce state-machine
            // transitions at runtime, closing the propose-erasure CRIT and
            // the broader lifecycle gap surfaced in audit-20260427.
            if !acct.lifecycle.is_empty() && !acct.fields.iter().any(|(n, _)| n == "status") {
                out.push_str("    pub status: u8,\n");
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
    } else if is_multi_variant_adt_state(spec) && matches!(target, Target::Anchor) {
        // v2.24 S5b — multi-variant ADT state lowers to a wrapper-struct
        // + inner-enum pair. Anchor 0.32.1's `#[account]` hard-requires
        // a struct (anchor-attribute-account-0.32.1/src/lib.rs:106), so
        // the wrapper carries the discriminator + AccountSerialize
        // while the inner enum carries the actual variant payloads.
        // Borsh discriminates variants by declaration-order tag (the
        // role the legacy `status: u8` byte used to play, now handled
        // by the enum's own representation). See the smoke fixture at
        // `/tmp/anchor_enum_test/` for the validated pattern, and
        // [[reference_anchor_account_struct_only]] in user memory.
        //
        // Quasar's zero-copy `#[account]` is incompatible with enum
        // payloads (alignment / `#[repr(C)]` constraints); this branch
        // is intentionally Anchor-only. Quasar multi-variant ADT specs
        // still go through the flat-struct branch below until that
        // target gets its own enum emission story.
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));
        let inner_name = format!("{}Inner", state_name);
        let acct = &spec.account_types[0];

        out.push_str("#[account]\n");
        out.push_str("#[derive(InitSpace)]\n");
        out.push_str(&format!("pub struct {} {{\n", state_name));
        out.push_str(&format!("    pub inner: {},\n", inner_name));
        if !spec.pdas.is_empty() && !spec.state_fields.iter().any(|(n, _)| n == "bump") {
            out.push_str("    pub bump: u8,\n");
        }
        out.push_str("}\n\n");

        out.push_str(&format!(
            "/// Variant-payload state for {0}. The Anchor wrapper above\n\
             /// carries the account discriminator; this enum carries the\n\
             /// state-machine variant + per-variant payload fields.\n",
            state_name
        ));
        out.push_str(
            "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Debug, PartialEq)]\n",
        );
        out.push_str(&format!("pub enum {} {{\n", inner_name));
        for variant in &acct.variants {
            if variant.fields.is_empty() {
                // Unit-style variant (no payload). `Uninitialized`,
                // `Closed`, `HasProposal` shapes all go through here.
                out.push_str(&format!("    {},\n", variant.name));
            } else {
                out.push_str(&format!("    {} {{\n", variant.name));
                for (fname, ftype) in &variant.fields {
                    out.push_str(&format!(
                        "        {}: {},\n",
                        fname,
                        map_type_for_target(ftype, spec, target)?
                    ));
                }
                out.push_str("    },\n");
            }
        }
        out.push_str("}\n");
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
        out.push_str(&format!("{}pub struct {} {{\n", account_attr, state_name));

        for (fname, ftype) in &spec.state_fields {
            out.push_str(&format!(
                "    pub {}: {},\n",
                fname,
                map_type_for_target(ftype, spec, target)?
            ));
        }

        if !spec.pdas.is_empty() && !spec.state_fields.iter().any(|(n, _)| n == "bump") {
            out.push_str("    pub bump: u8,\n");
        }

        // R26: see the multi-account branch above for rationale.
        if !spec.lifecycle_states.is_empty()
            && !spec.state_fields.iter().any(|(n, _)| n == "status")
        {
            out.push_str("    pub status: u8,\n");
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
            out.push_str(&format!(
                "    pub {}: {},\n",
                fname,
                map_type_for_target(ftype, spec, target)?
            ));
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

    // R26: when any handler has a non-init lifecycle transition, the
    // generated guards.rs raises `<Program>Error::InvalidLifecycle` on
    // pre-status mismatch. Auto-add the variant if the spec doesn't
    // already declare one — this is a purely operational error, not a
    // spec-level concept the user reasons about.
    let needs_lifecycle = spec.handlers.iter().any(|h| {
        let pre = h.pre_status.as_deref().unwrap_or("");
        let is_init = matches!(pre, "Uninitialized" | "Empty");
        !pre.is_empty() && !is_init
    });
    // R28: same shape — when guards.rs emits a runtime PDA verification
    // (driven by R13 suppression on Quasar non-init handlers), it raises
    // `<Program>Error::InvalidPda` on mismatch. Auto-add similarly.
    let needs_invalid_pda = matches!(target, Target::Quasar)
        && spec.handlers.iter().any(|h| {
            let bound: std::collections::HashSet<&str> =
                h.accounts.iter().map(|a| a.name.as_str()).collect();
            let is_init_handler = matches!(
                h.pre_status.as_deref(),
                Some("Uninitialized") | Some("Empty")
            );
            h.accounts.iter().any(|acct| {
                let Some(seeds) = &acct.pda_seeds else {
                    return false;
                };
                if acct.is_signer {
                    return false;
                }
                // Skip the init target — its seeds are macro-verified.
                let on_account_matches = match h.on_account.as_deref() {
                    Some(adt) => {
                        let lower = adt.to_lowercase();
                        acct.name == lower || acct.name.starts_with(&lower)
                    }
                    None => true,
                };
                if is_init_handler && on_account_matches {
                    return false;
                }
                seeds.iter().any(|seed| {
                    let is_literal = seed.starts_with('"') && seed.ends_with('"');
                    !is_literal && !bound.contains(seed.as_str())
                })
            })
        });
    let mut codes: Vec<String> = spec.error_codes.clone();
    if needs_lifecycle && !codes.iter().any(|c| c == "InvalidLifecycle") {
        codes.push("InvalidLifecycle".to_string());
    }
    if needs_invalid_pda && !codes.iter().any(|c| c == "InvalidPda") {
        codes.push("InvalidPda".to_string());
    }

    out.push_str("#[error_code]\n");
    out.push_str(&format!("pub enum {} {{\n", error_name));
    for (i, code) in codes.iter().enumerate() {
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
/// Identifier-character predicate for the `bind_state` word-bounded
/// rewrite: ASCII alphanumerics plus underscore mark the inside of a
/// Rust identifier.
fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Rewrite each `[<idx>]` substring to `[(<idx>) as usize]`. Used by
/// `mechanize_effect` (Rust output) to keep the field-string Lean-clean
/// while still satisfying Rust's `usize`-only array indexing. Same
/// transform as `path_to_rust`'s Index emission, applied at codegen
/// time instead of at expr-render time so both Lean and Rust read the
/// same `(field, op_kind, value)` tuple.
fn rewrite_index_to_usize(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = String::with_capacity(field.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            // Find matching `]`.
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b']' {
                end += 1;
            }
            if end >= bytes.len() {
                // Unbalanced — give up and emit verbatim.
                out.push_str(&field[i..]);
                break;
            }
            let idx_expr = &field[start..end];
            // Don't double-wrap if already cast.
            if idx_expr.contains("as usize") {
                out.push_str(&field[i..=end]);
            } else {
                out.push_str(&format!("[({}) as usize]", idx_expr));
            }
            i = end + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Render the pre-status check (when `write` is false) or the post-status
/// write (when `write` is true) for R26 lifecycle enforcement. Returns an
/// empty string when the lifecycle clause doesn't require a runtime
/// emission (init handlers skip the pre-check; pre==post handlers skip
/// the post-write; specs without lifecycle declarations skip everything).
fn lifecycle_check_line(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    write: bool,
    surface: &FrameworkSurface,
) -> String {
    // Find the state-bearing account name and its `<ADT>Status` enum.
    let state_acct = find_state_account(handler);
    let Some(sa) = state_acct else {
        return String::new();
    };

    // v2.24 S5c — multi-variant ADT state has no `status: u8` byte; the
    // variant IS the discriminator. Pre-check rewrites to a
    // `matches!(inner, Inner::<pre> { .. })` test against the wrapper's
    // `inner` field. Post-write is a no-op — the effect lowering's
    // match arm (or set_inner for cross-variant promotion) is what
    // moves the variant.
    //
    // v2.24.0 follow-up: gate on Anchor target. Quasar emits the
    // legacy flat-struct `pub struct X { … status: u8, … }` shape
    // even for multi-variant ADT specs (the wrapper-struct + inner-
    // enum rewrite is Anchor-only — Quasar's zero-copy `#[account]`
    // is incompatible with enum payloads). Emitting the `matches!
    // (ctx.X.inner, …)` check on Quasar produces uncompilable code:
    // there's no `inner` field on the Quasar wrapper. Quasar specs
    // keep the legacy `status` byte check below.
    if is_multi_variant_adt_state(spec) && matches!(surface.target, Target::Anchor) {
        if write {
            // The effect lowering handles variant transitions inline;
            // no separate post-write needed.
            return String::new();
        }
        let pre = handler.pre_status.as_deref().unwrap_or("");
        if pre.is_empty() || matches!(pre, "Uninitialized" | "Empty") {
            return String::new();
        }
        let Some(acct) = spec.account_types.first() else {
            return String::new();
        };
        let Some(variant) = acct.variants.iter().find(|v| v.name == pre) else {
            return String::new();
        };
        let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
        let err_enum = format!("crate::errors::{}Error", to_pascal_case(&spec.program_name));
        let err_ctor = surface.error_expr(&err_enum, "InvalidLifecycle");
        // Payload variants need `{ .. }`; unit variants don't.
        let pattern = if variant.fields.is_empty() {
            format!("{}::{}", inner_name, pre)
        } else {
            format!("{}::{} {{ .. }}", inner_name, pre)
        };
        return format!(
            "    // lifecycle: require inner == {pre}\n    if !matches!(ctx.{acct}.inner, {pattern}) {{ return Err({err_ctor}); }}\n",
            pre = pre,
            acct = sa.name,
            pattern = pattern,
            err_ctor = err_ctor,
        );
    }

    // Resolve the Status enum name. Mirrors `generate_state`'s naming:
    //   - `is_multi` (account_types.len() > 1): emit `<ADT>Status` per
    //     lifecycle (lending: `PoolStatus`, `LoanStatus`).
    //   - Otherwise: emit a single `Status` enum.
    // Important: `account_types` can contain ONE entry (e.g. multisig's
    // `type State | …`) and still be "single-state" for naming purposes.
    let is_multi = spec.account_types.len() > 1;
    let (enum_name, lifecycle): (String, &Vec<String>) = if is_multi {
        let Some(adt) = handler.on_account.as_deref() else {
            return String::new();
        };
        let Some(at) = spec.account_types.iter().find(|a| a.name == adt) else {
            return String::new();
        };
        if at.lifecycle.is_empty() {
            return String::new();
        }
        (format!("{}Status", at.name), &at.lifecycle)
    } else {
        // Single-state: the spec may declare its lifecycle either via a
        // single ADT (then `account_types[0].lifecycle` carries the
        // variants) or via the legacy flat `state {}` form (then they
        // live on `spec.lifecycle_states`). Prefer the ADT slot.
        let lifecycle: &Vec<String> = spec
            .account_types
            .first()
            .map(|at| &at.lifecycle)
            .filter(|v| !v.is_empty())
            .unwrap_or(&spec.lifecycle_states);
        if lifecycle.is_empty() {
            return String::new();
        }
        ("Status".to_string(), lifecycle)
    };

    let pre = handler.pre_status.as_deref().unwrap_or("");
    let post = handler.post_status.as_deref().unwrap_or("");
    if pre.is_empty() && post.is_empty() {
        return String::new();
    }

    let is_init_pre = matches!(pre, "Uninitialized" | "Empty");

    let err_enum = format!("crate::errors::{}Error", to_pascal_case(&spec.program_name));

    if write {
        // Post-status write: only when post is set and differs from pre.
        if post.is_empty() || pre == post {
            return String::new();
        }
        if !lifecycle.iter().any(|s| s == post) {
            return String::new();
        }
        format!(
            "    // lifecycle: status := {post}\n    ctx.{acct}.status = {enum_name}::{post} as u8;\n",
            post = post,
            acct = sa.name,
            enum_name = enum_name,
        )
    } else {
        // Pre-status check: skip on init transitions (init zeros the
        // account) and when there's no pre to check.
        if is_init_pre || pre.is_empty() {
            return String::new();
        }
        if !lifecycle.iter().any(|s| s == pre) {
            return String::new();
        }
        let err_ctor = surface.error_expr(&err_enum, "InvalidLifecycle");
        format!(
            "    // lifecycle: require status == {pre}\n    if ctx.{acct}.status != {enum_name}::{pre} as u8 {{ return Err({err_ctor}); }}\n",
            pre = pre,
            acct = sa.name,
            enum_name = enum_name,
            err_ctor = err_ctor,
        )
    }
}

fn find_state_account(handler: &ParsedHandler) -> Option<&crate::check::ParsedHandlerAccount> {
    // Try writable-only first — matches lifecycle-mutation handlers and is
    // the original behavior. If the writable-filtered search comes up empty,
    // fall back to all non-signer/non-program/non-token candidates so
    // read-only handlers (view-style reads, pre-flight checks, claim
    // handlers that mutate a sibling account) still get `s.field` rewritten
    // to `ctx.<acct>.field` in guards.rs. Without the fallback the guard
    // body emits bare `s.field` references that don't compile.
    if let Some(found) = find_state_account_filtered(handler, true) {
        return Some(found);
    }
    find_state_account_filtered(handler, false)
}

fn find_state_account_filtered(
    handler: &ParsedHandler,
    require_writable: bool,
) -> Option<&crate::check::ParsedHandlerAccount> {
    let mut candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| (!require_writable || a.is_writable) && !a.is_signer && !a.is_program)
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
        return Some(candidates[0]);
    }
    // Multi-state spec disambiguator: when the handler declares
    // `on_account = "Loan"` (parsed from `: Loan.Pre -> Loan.Post`), pick
    // the handler-account whose name matches the ADT (lowercase). Without
    // this, lending::liquidate has both `loan` and `pool` as writable
    // PDA candidates and `find_state_account` returned None, leaving
    // `s.amount > s.collateral` un-rewritten in guards.rs.
    if let Some(adt) = handler.on_account.as_deref() {
        let lower = adt.to_lowercase();
        if let Some(matched) = candidates
            .iter()
            .copied()
            .find(|a| a.name == lower || a.name.starts_with(&lower))
        {
            return Some(matched);
        }
    }
    None
}

/// Canonical SPL Token program ID. Calls into an interface whose
/// `program_id "..."` matches this constant get the `anchor_spl::token::*`
/// CPI shape; other program IDs route through the generic
/// `solana_program::program::invoke` builder.
const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Try to emit a real Anchor CPI invocation for one `call Interface.handler(...)`
/// site. Returns `None` when the interface isn't recognized (caller falls
/// back to a comment + `todo!()` so the user / an LLM fills the body).
///
/// All five SPL Token handlers — `transfer`, `mint_to`, `burn`,
/// `initialize_account`, `close_account` — get an `anchor_spl::token::*`
/// shape; non-SPL-Token interfaces ship a generic
/// `solana_program::program::invoke` shape. The canonical SPL handlers
/// cover the bulk of CPI traffic in deployed programs, which is what
/// keeps `todo!()` out of the typical escrow / lending / vault shape.
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
    emit_generic_anchor_cpi(call, handler, iface, spec)
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
    spec: &ParsedSpec,
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
    // v2.24.0 follow-up: use `Pubkey::from_str` instead of the
    // `pubkey!` macro. The macro lives at different paths across
    // Anchor versions (and isn't reexported under
    // `anchor_lang::solana_program` in 0.32.x); `from_str` is
    // stable on the `Pubkey` type itself.
    out.push_str(&format!(
        "                program_id: <anchor_lang::prelude::Pubkey as std::str::FromStr>::from_str(\"{}\").unwrap(),\n",
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

    // v2.24 #11 — when the caller wrote `let X = call …` and the
    // interface declares a return type, capture the callee's return
    // data via Solana's `get_return_data` syscall + Borsh decode.
    // The block opens with `let X = { … }` instead of bare `{ … }`,
    // and the closing emits the binding's value as the block's
    // result. Without a declared return type, the binding name is
    // dropped (matched at the caller side via lint warning at
    // adapt time — TODO v2.25.1).
    if let (Some(bind), Some(ret_dsl_type)) = (&call.result_binding, &iface_handler.return_type) {
        let ret_rust = match map_type(ret_dsl_type, spec) {
            Ok(t) => t,
            Err(_) => return None,
        };
        out.push_str("            use anchor_lang::AnchorDeserialize;\n");
        out.push_str("            use anchor_lang::solana_program::program::get_return_data;\n");
        out.push_str("            use anchor_lang::solana_program::program_error::ProgramError;\n");
        out.push_str(
            "            let (_, ret_bytes) = get_return_data().ok_or(ProgramError::InvalidAccountData)?;\n",
        );
        out.push_str(&format!(
            "            <{ret} as AnchorDeserialize>::deserialize(&mut ret_bytes.as_slice()).map_err(|_| ProgramError::InvalidAccountData)?\n",
            ret = ret_rust,
        ));
        // Close the block and use it as the RHS of the let-binding.
        out.push_str("        };\n");
        // Prepend the `let <bind> = ` to the front of `out` (after
        // the leading indent), since we opened with `        {` and
        // the binding wants `let X = {`.
        let prefix = format!("        let {} = {{\n", bind);
        let body_without_open = out
            .strip_prefix("        {\n")
            .map(String::from)
            .unwrap_or(out.clone());
        return Some(format!("{}{}", prefix, body_without_open));
    }

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
///
/// `on_error` is the v2.24 §S1a per-site override (`pool += amount or X`).
/// When `Some(name)`, the generated `checked_add` / `checked_sub` uses
/// `Name` as the error variant. When `None`, the lowering falls back to
/// (in priority order) the `pragma checked_overflow_error =` /
/// `pragma checked_underflow_error =` default, then the built-in
/// `MathOverflow` / `MathUnderflow`. Always `None` for non-checked ops.
fn mechanize_effect(
    effect: &(String, String, String),
    on_error: Option<&str>,
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
    // v2.24 S5c — under multi-variant ADT state on the Anchor
    // target, the flat `self.<acct>.<field>` lowering doesn't apply.
    // The wrapper-struct + inner-enum emission from S5b means the
    // wrapper carries only `inner` (the variant payload) and `bump`
    // — there's no top-level `<field>` to write. Bail so the
    // per-effect path surfaces a `// Spec effect (needs fill)` line
    // + a trailing `todo!()` rather than a silent miscompile like
    // `self.escrow.initializer_amount = …`.
    //
    // Two cases trigger:
    //   1. Variant-prefixed LHS (`Active.balance := …`): cross-
    //      variant emitter `emit_variant_state_handler_body` handles
    //      same-variant + cross-variant when it can; this path is
    //      the bail-out for cases it can't (missing post-variant
    //      fields, payload-carrying pre, etc.).
    //   2. Bare-field LHS (`balance := …`): a pre-v2.24 spec
    //      hasn't been migrated to `Variant.field` syntax yet. The
    //      `bare_field_on_multi_variant_state` lint (S5c) surfaces
    //      this as guidance; the codegen bails so the migration
    //      need is unmissable.
    if is_multi_variant_adt_state(spec) && matches!(target, Target::Anchor) {
        return None;
    }
    // v2.24.0 follow-up: single-variant ADT specs also accept the
    // `Variant.field` LHS form for forward-compat. The flat-struct
    // emission has no `Variant.` prefix in the actual struct, so
    // strip it here. Without this, `Active.total_burned := X`
    // would lower to `self.acct.Active.total_burned = X` which
    // doesn't compile (Active isn't a field on the wrapper).
    let field = strip_variant_prefix(field, spec);
    let field = field.as_str();

    // Anchor / Quasar handler bodies bind state as `self.<acct>.<field>`,
    // so a bare state-field RHS (e.g. `bid_buyer := state.rfp_buyer` after
    // upstream strips `state.`) needs to resolve to `self.<acct>.rfp_buyer`.
    let acct = &state_acct.name;
    let acct_binder = format!("self.{}.", acct);
    let rhs = crate::rust_codegen_util::resolve_value(value, handler, spec, Some(&acct_binder));
    // Cast index expressions in the LHS path to `usize`. `render_effect`
    // emits the field as `voted[member_index]` (Lean-friendly); on the
    // Rust side, indexing `[u8; N]` with `u8`/`u16`/Fin fails — Rust
    // requires `usize`. Same shape as `path_to_rust`'s Index emission;
    // applied here so the Lean output stays untouched.
    let field = rewrite_index_to_usize(field);
    let field = field.as_str();
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
    //
    // v2.24 §S1a/b/c: variant-name resolution becomes three-tiered:
    //   1. Per-site override:    `pool += amount or MintOverflow`
    //   2. Pragma default:       `pragma checked_overflow_error = …`
    //   3. Built-in default:     `MathOverflow` (for `+=`),
    //                            `MathUnderflow` (for `-=`).
    // S1c back-compat: specs declaring `MathOverflow` but not
    // `MathUnderflow` keep the pre-v2.24 behavior of `-=` raising
    // `MathOverflow`. The lint at check.rs surfaces missing declarations.
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let pragma_overflow = spec.pragma_value("checked_overflow_error");
    let pragma_underflow = spec.pragma_value("checked_underflow_error");
    // Built-in underflow default with back-compat: if MathOverflow is
    // declared but MathUnderflow isn't, treat `-=` as raising MathOverflow
    // (matches pre-v2.24 behavior). This keeps existing specs building.
    let builtin_underflow = if has_decl("MathUnderflow") || !has_decl("MathOverflow") {
        "MathUnderflow"
    } else {
        "MathOverflow"
    };
    let overflow_variant = on_error.or(pragma_overflow).unwrap_or("MathOverflow");
    let underflow_variant = on_error.or(pragma_underflow).unwrap_or(builtin_underflow);
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
            "        self.{acct}.{field} = self.{acct}.{field}.checked_add({rhs}).ok_or({err_enum}::{overflow_variant})?;\n"
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
            "        self.{acct}.{field} = self.{acct}.{field}.checked_sub({rhs}).ok_or({err_enum}::{underflow_variant})?;\n"
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

/// v2.24 S5c — strip a leading `Variant.` prefix from an effect LHS
/// when the root matches a known variant on the spec's single state
/// account. `Active.pool` → `pool`; `accounts[i].cap` → unchanged;
/// `pool` → unchanged. Pure string transform — the lint side keeps
/// `variant_fields` indexed for validation; this just normalizes
/// for downstream emission.
fn strip_variant_prefix(lhs: &str, spec: &ParsedSpec) -> String {
    if let Some(dot) = lhs.find('.') {
        let head = &lhs[..dot];
        let is_variant = spec
            .account_types
            .iter()
            .any(|a| a.variants.iter().any(|v| v.name == head));
        if is_variant {
            return lhs[dot + 1..].to_string();
        }
    }
    lhs.to_string()
}

/// v2.24 S5c — emit one effect line in destructured-variant context.
/// `mechanize_effect`'s sibling for the multi-variant ADT path: the
/// state binder is a destructured local (e.g. `pool: &mut u64` from
/// `match &mut self.<acct>.inner { Inner::Active { pool, .. } => …`),
/// not `self.<acct>.<field>`. Emit `*pool = …` or `pool[i] = …`
/// instead of `self.<acct>.pool = …`. Falls back to `None` for
/// non-scalar / non-indexed shapes the caller can route to a fresh
/// per-effect `todo!()`.
fn mechanize_effect_destructured(
    effect: &(String, String, String),
    on_error: Option<&str>,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let (field_raw, op_kind, value) = effect;
    let simple_rhs = value
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !simple_rhs {
        return None;
    }
    // Strip variant prefix (`Active.pool` → `pool`) and normalize
    // indexed-access form. The destructured binding is the bare
    // field root — `pool[i]` keeps the indexing in place; scalar
    // bindings carry an extra `*` deref to write through `&mut T`.
    let field = strip_variant_prefix(field_raw, spec);
    let field = rewrite_index_to_usize(&field);
    let is_indexed = field.contains('[');
    // The destructure binding shadows the field name in scope, so a
    // bare-identifier RHS that names a sibling state field resolves
    // directly (no `self.<acct>.` prefix). For params, the resolver
    // already returns the bare name.
    let rhs = crate::rust_codegen_util::resolve_value(value, handler, spec, None);

    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let pragma_overflow = spec.pragma_value("checked_overflow_error");
    let pragma_underflow = spec.pragma_value("checked_underflow_error");
    let builtin_underflow = if has_decl("MathUnderflow") || !has_decl("MathOverflow") {
        "MathUnderflow"
    } else {
        "MathOverflow"
    };
    let overflow_variant = on_error.or(pragma_overflow).unwrap_or("MathOverflow");
    let underflow_variant = on_error.or(pragma_underflow).unwrap_or(builtin_underflow);

    // Scalars need an explicit deref to write through `&mut T`;
    // indexed access through `&mut [T; N]` works as-is.
    let lhs = if is_indexed {
        field.clone()
    } else {
        format!("*{}", field)
    };
    // Method calls auto-deref `&mut T`, so the RHS-side read can
    // stay as the bare binding name even for scalar bindings.
    let read = strip_array_index_suffix(&field);

    let line = match op_kind.as_str() {
        "set" => format!("            {} = {};\n", lhs, rhs),
        "add" => format!(
            "            {} = {}.checked_add({}).ok_or({}::{})?;\n",
            lhs, read, rhs, err_enum, overflow_variant
        ),
        "add_sat" => format!("            {} = {}.saturating_add({});\n", lhs, read, rhs),
        "add_wrap" => format!("            {} = {}.wrapping_add({});\n", lhs, read, rhs),
        "sub" => format!(
            "            {} = {}.checked_sub({}).ok_or({}::{})?;\n",
            lhs, read, rhs, err_enum, underflow_variant
        ),
        "sub_sat" => format!("            {} = {}.saturating_sub({});\n", lhs, read, rhs),
        "sub_wrap" => format!("            {} = {}.wrapping_sub({});\n", lhs, read, rhs),
        _ => return None,
    };
    Some(line)
}

/// v2.24 S5c — drop `[<idx>]` from a destructured field reference so
/// the RHS-side read uses the bare binding name. `voted[i as usize]`
/// → `voted`. Pure substring chop; safe for the simple shapes the
/// destructured emitter accepts.
fn strip_array_index_suffix(field: &str) -> String {
    match field.find('[') {
        Some(i) => field[..i].to_string(),
        None => field.to_string(),
    }
}

/// v2.24 S5c — emit the complete handler body block for a
/// multi-variant ADT state. Wraps the per-effect lines in a
/// destructure-and-mutate (same-variant) or destructure-and-promote
/// (cross-variant) match block. Returns `None` when the handler
/// shape isn't yet supported by this lowering pass — callers fall
/// back to the per-effect loop (which emits `todo!()` for any
/// unmechanized effect).
///
/// Same-variant pattern:
///
/// ```ignore
/// match &mut self.<acct>.inner {
///     <Inner>::<Variant> { f1, f2, .. } => {
///         *f1 = …;
///         f2[i as usize] = …;
///     }
///     _ => return Err(<Err>::WrongState.into()),
/// }
/// ```
///
/// Cross-variant (init / promote) is deferred — the wrapping
/// `set_inner(Inner::Post { … })` requires picking a payload for
/// every field of the post variant, which often needs spec data
/// the agent fills in (CPI return values, event-binding sources).
/// Today that lands as a `todo!()` line; v2.24.1 / v2.25 may grow
/// a richer codegen path.
fn emit_variant_state_handler_body(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
    state_acct: &crate::check::ParsedHandlerAccount,
) -> Option<String> {
    if !matches!(target, Target::Anchor) {
        return None;
    }
    if !is_multi_variant_adt_state(spec) {
        return None;
    }
    let pre = handler.pre_status.as_deref()?;
    let post = handler.post_status.as_deref()?;
    // WrongState is the error variant returned on a variant
    // mismatch. Demand it's declared so codegen doesn't emit a
    // dangling reference to an undeclared error variant.
    let has_wrong_state = spec.error_codes.iter().any(|c| c == "WrongState");
    if !has_wrong_state {
        return None;
    }

    let acct = spec.account_types.first()?;
    let post_variant = acct.variants.iter().find(|v| v.name == post)?;
    let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    let acct_binder = &state_acct.name;

    // Cross-variant promotion (init / promote handlers,
    // `pre != post`) routes through a separate emitter: destructure
    // the pre (if it carries payload) + assemble the post-variant
    // payload + assign `self.<acct>.inner = <Inner>::<Post>{ … }`.
    // Same-variant continues with the in-place match destructure
    // below.
    if pre != post {
        return emit_cross_variant_promotion(
            handler,
            spec,
            acct_binder,
            pre,
            post_variant,
            &inner_name,
            &err_enum,
        );
    }

    // Collect the unique set of bare field names referenced on the
    // LHS of any effect (after stripping `<Variant>.` prefix and
    // any `[…]` indexing). These go into the match destructure
    // pattern so the inner block can rebind them.
    let mut mutated_fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (lhs, _, _) in &handler.effects {
        let stripped = strip_variant_prefix(lhs, spec);
        let bare = strip_array_index_suffix(&stripped);
        // Only fields that actually live on the post variant get
        // destructured. References that don't match are a spec
        // bug — fall back so the per-effect path emits a clear
        // `todo!()` with the offending line surfaced verbatim.
        if !post_variant.fields.iter().any(|(n, _)| n == &bare) {
            return None;
        }
        mutated_fields.insert(bare);
    }
    if mutated_fields.is_empty() {
        // Pure read-only handler (no effects on state). Emit a
        // skip-style match so the runtime still rejects the wrong
        // variant — the guard layer doesn't catch variant drift on
        // its own.
        let mut out = String::new();
        out.push_str(&format!("        match self.{}.inner {{\n", acct_binder));
        out.push_str(&format!(
            "            {}::{} {{ .. }} => {{}}\n",
            inner_name, post
        ));
        out.push_str(&format!(
            "            _ => return Err({}::WrongState.into()),\n",
            err_enum
        ));
        out.push_str("        }\n");
        return Some(out);
    }

    let destructure = mutated_fields
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::new();
    out.push_str(&format!(
        "        match &mut self.{}.inner {{\n",
        acct_binder
    ));
    out.push_str(&format!(
        "            {}::{} {{ {}, .. }} => {{\n",
        inner_name, post, destructure
    ));
    for (idx, effect) in handler.effects.iter().enumerate() {
        let on_error = handler.effect_on_error.get(idx).and_then(|o| o.as_deref());
        let line = mechanize_effect_destructured(effect, on_error, handler, spec)?;
        out.push_str(&line);
    }
    out.push_str("            }\n");
    out.push_str(&format!(
        "            _ => return Err({}::WrongState.into()),\n",
        err_enum
    ));
    out.push_str("        }\n");
    Some(out)
}

/// v2.24 S5c — render an effect RHS for the cross-variant promotion
/// emitter. Pre-variant fields aren't in scope here (no destructure
/// has happened yet), so the legal RHS shapes are restricted:
///
///   - bare param (handler `takes_params`) → bare identifier
///   - bare const (spec `constants`) → bare identifier (Rust resolves)
///   - integer literal → bare integer
///   - `<account>.pubkey` where `<account>` is bound on the handler
///     and is a signer / writable account → `self.<account>.key()`
///
/// Anything else returns `None`, which bails the whole cross-variant
/// emitter back to the per-effect `todo!()` path so the agent fills
/// the gap manually instead of getting a silent miscompile.
fn resolve_cross_variant_rhs(
    raw: &str,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    // Integer literal — render_effect emits `Expr::Int(v)` as its
    // decimal form.
    if raw.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Some(raw.to_string());
    }
    // Bare param or const — no dot, no bracket, matches a known name.
    if !raw.contains('.') && !raw.contains('[') {
        if handler.takes_params.iter().any(|(n, _)| n == raw) {
            return Some(raw.to_string());
        }
        if spec.constants.iter().any(|(n, _)| n == raw) {
            return Some(raw.to_string());
        }
        // Unknown bare ident — could be a param shadowed by a state
        // field of the same name; that's a spec smell. Bail loud
        // (via `None`) rather than guess.
        return None;
    }
    // `<account>.pubkey` shape — map to the Anchor key() accessor on
    // the handler's account binding.
    if let Some(account_name) = raw.strip_suffix(".pubkey") {
        if handler.accounts.iter().any(|a| a.name == account_name) {
            return Some(format!("self.{}.key()", account_name));
        }
    }
    None
}

/// v2.24 S5c — emit cross-variant promotion (init / promote) for a
/// multi-variant ADT state. Assembles every post-variant field
/// from the handler's effect lines and assigns the new variant via
/// `self.<acct>.inner = <Inner>::<Post> { … };`.
///
/// Bail-out conditions (each falls back to the per-effect
/// `todo!()` path):
///   - Pre is a payload-carrying variant (the destructure would
///     need to capture pre fields that may flow into post; for
///     v2.24 we keep cross-variant scoped to unit-style pre).
///   - Any post-variant field has no matching effect line (we don't
///     guess defaults for unspecified fields).
///   - Any effect RHS can't be resolved by `resolve_cross_variant_rhs`
///     (complex shapes — match / record / arith — fall through).
fn emit_cross_variant_promotion(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    acct_binder: &str,
    pre: &str,
    post_variant: &crate::check::ParsedVariant,
    inner_name: &str,
    err_enum: &str,
) -> Option<String> {
    // v2.24.0 follow-up: lift the unit-pre-only restriction. Three
    // cross-variant promotion patterns are now valid:
    //   1. unit pre → payload post  (init: Uninitialized → Active)
    //   2. payload pre → unit post  (terminate: Open → Closed; the cancel case)
    //   3. unit pre → unit post     (rare; harmless: just set the variant)
    // Payload-pre → payload-post is still rejected — it would need
    // a destructure-then-rebuild pass to capture pre's fields, and
    // the current RHS resolver doesn't see the pre as a binding.
    let pre_variant = spec
        .account_types
        .first()?
        .variants
        .iter()
        .find(|v| v.name == pre)?;
    if !pre_variant.fields.is_empty() && !post_variant.fields.is_empty() {
        return None;
    }

    // Build a {field → RHS-rust} map from the handler's effects.
    // For cross-variant we only accept the `:= <rhs>` form (effect
    // op kind "set"); checked-arith ops don't make sense in a
    // promotion (no pre value to read).
    let mut field_rhs: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (lhs, op_kind, rhs) in &handler.effects {
        if op_kind != "set" {
            return None;
        }
        let stripped = strip_variant_prefix(lhs, spec);
        let bare = strip_array_index_suffix(&stripped);
        // Field must live on the post variant.
        if !post_variant.fields.iter().any(|(n, _)| n == &bare) {
            return None;
        }
        let resolved = resolve_cross_variant_rhs(rhs, handler, spec)?;
        field_rhs.insert(bare, resolved);
    }

    // Every post-variant field must have an RHS. We don't fill
    // defaults — silent defaults hide bugs (a zeroed pubkey is a
    // famously bad bug). For unit-style post variants, this loop
    // iterates zero times — the assignment lands as a bare
    // variant constructor with no braces.
    for (fname, _) in &post_variant.fields {
        if !field_rhs.contains_key(fname) {
            return None;
        }
    }

    // Assemble the assignment. Pre-check: ensure the wrapper is in
    // the pre variant. Init handlers (`Uninitialized`/`Empty`) skip
    // this since the `#[account(init, …)]` constraint zeroes the
    // account before our code runs — there's no prior payload to
    // mismatch on. Other pres still get the gate. Payload pres
    // use `Inner::Pre { .. }` to match without binding fields.
    let is_init_pre = matches!(pre, "Uninitialized" | "Empty");
    let mut out = String::new();
    if !is_init_pre {
        let pre_pattern = if pre_variant.fields.is_empty() {
            format!("{}::{}", inner_name, pre)
        } else {
            format!("{}::{} {{ .. }}", inner_name, pre)
        };
        out.push_str(&format!(
            "        if !matches!(self.{}.inner, {}) {{ return Err({}::WrongState.into()); }}\n",
            acct_binder, pre_pattern, err_enum
        ));
    }
    // Unit-style post: emit `... = Inner::Closed;` without the
    // empty `{}` braces. Payload post: emit the brace-wrapped
    // field-by-field initializer in declared order.
    if post_variant.fields.is_empty() {
        out.push_str(&format!(
            "        self.{}.inner = {}::{};\n",
            acct_binder, inner_name, post_variant.name
        ));
    } else {
        out.push_str(&format!(
            "        self.{}.inner = {}::{} {{\n",
            acct_binder, inner_name, post_variant.name
        ));
        // Emit fields in the variant's declared order so the
        // assembled initializer reads the way a user would write
        // it. The map is sorted by name for lookup; iterating the
        // variant's `fields` here preserves source-declared order.
        for (fname, _) in &post_variant.fields {
            let rhs = &field_rhs[fname];
            out.push_str(&format!("            {}: {},\n", fname, rhs));
        }
        out.push_str("        };\n");
    }
    Some(out)
}

/// v2.24 S5c — emit a destructure-then-compare auth guard for a
/// handler whose `auth X` field lives in a variant payload. Returns
/// the empty string when the conditions don't apply (single-variant
/// state, no `auth X`, X is on the wrapper not in a variant payload,
/// no matching signer account, or `Unauthorized` not declared in
/// `type Error`). The guard fires after the lifecycle pre-check, so
/// the destructure is guaranteed to bind.
fn emit_variant_auth_guard(handler: &ParsedHandler, spec: &ParsedSpec, target: Target) -> String {
    // v2.24.0 follow-up: gate on Anchor target. Quasar keeps the
    // flat-struct shape even for multi-variant ADT specs, so
    // emitting a `let <Inner>::<Pre> { auth: auth_field, .. } = …`
    // destructure references a type that doesn't exist on Quasar.
    // The `has_one = X` suppression in check.rs::account_attr is
    // also target-gated below — Quasar specs keep firing `has_one`
    // (the flat-struct field IS accessible) and don't need the
    // replacement destructure-check guard.
    if !matches!(target, Target::Anchor) {
        return String::new();
    }
    let Some(ref who) = handler.who else {
        return String::new();
    };
    if !crate::check::is_multi_variant_adt_with_field_in_variant(spec, who) {
        return String::new();
    }
    let Some(ref pre) = handler.pre_status else {
        return String::new();
    };
    if matches!(pre.as_str(), "Uninitialized" | "Empty") {
        return String::new();
    }
    let Some(acct) = spec.account_types.first() else {
        return String::new();
    };
    let Some(variant) = acct.variants.iter().find(|v| v.name == *pre) else {
        return String::new();
    };
    if !variant.fields.iter().any(|(n, _)| n == who) {
        return String::new();
    }
    // Need a signer account in the handler with the same name as
    // the auth field, so the auth comparison can resolve
    // `ctx.<signer>.key()` without renaming dance.
    let Some(signer_acct) = handler
        .accounts
        .iter()
        .find(|a| a.is_signer && a.name == *who)
    else {
        return String::new();
    };
    if !spec.error_codes.iter().any(|c| c == "Unauthorized") {
        return String::new();
    }
    let Some(state_acct) = find_state_account(handler) else {
        return String::new();
    };

    let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    // Pin the auth-field destructure binding to a stable local name
    // (`auth_field`) to avoid shadowing param names that might match
    // the field's name in user code. The rest of the variant payload
    // gets ignored via `..`.
    format!(
        "    // auth: variant payload {pre}.{who} == ctx.{signer}.key()\n\
         \x20   let {inner}::{pre} {{ {who}: auth_field, .. }} = &ctx.{acct}.inner\n\
         \x20   else {{ return Err({err}::InvalidLifecycle.into()); }};\n\
         \x20   if auth_field != &ctx.{signer}.key() {{ return Err({err}::Unauthorized.into()); }}\n",
        inner = inner_name,
        pre = pre,
        who = who,
        signer = signer_acct.name,
        acct = state_acct.name,
        err = err_enum,
    )
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
            let inferred_match = is_multi && inferred_name != default_state_name;
            let is_state =
                state_acct.map(|sa| sa.name == acct.name).unwrap_or(false) || inferred_match;
            let attr = acct.quasar_account_attr(handler, &inferred_name, target, spec, is_state);
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
    // Token / Mint live in a separate crate per framework. Only Quasar
    // handler files need a per-handler SPL import — the local Accounts
    // struct references `Account<Token>` / `Account<Mint>` directly.
    // Anchor handler files re-export the struct from lib.rs, which
    // already imports SPL types at crate root.
    if matches!(target, Target::Quasar) {
        let has_token = handler
            .accounts
            .iter()
            .any(|a| a.account_type.as_deref() == Some("token") || a.name == "token_program");
        let has_mint = handler
            .accounts
            .iter()
            .any(|a| a.account_type.as_deref() == Some("mint"));
        let imports = surface.token_imports(has_token, has_mint);
        if !imports.is_empty() {
            out.push_str(&imports);
        }
    }
    // Quasar's Accounts struct is defined locally in this file, so its
    // fields (`Account<MyState>`) need state types in scope. Anchor's
    // struct lives in lib.rs (already imports state); the handler
    // scaffold body only references guards + bumps, so the import would
    // be flagged unused until the agent fills the body.
    if render_struct {
        out.push_str("use crate::state::*;\n");
    }
    out.push_str("use crate::guards;\n");
    out.push_str("use qedgen_macros::qed;\n");
    // Checked-arith effects (`+=` / `-=`) lower to
    // `<Pascal>Error::MathOverflow`. Bring the error enum into scope so
    // the rendered scaffold body compiles. Saturating / wrapping
    // (`+=!` / `+=?`) don't reference the enum.
    //
    // v2.24.0 follow-up: variant-state cross-variant promotion also
    // emits `MiniEscrowError::WrongState` (the pre-variant gate)
    // even when the handler has no checked-arith effects. Bring the
    // enum in whenever the spec is multi-variant ADT on Anchor and
    // the handler has a non-init pre — the same condition under
    // which `emit_cross_variant_promotion` emits the `matches!`
    // check that references the error variant.
    let uses_wrong_state_check = is_multi_variant_adt_state(spec)
        && matches!(target, Target::Anchor)
        && handler
            .pre_status
            .as_deref()
            .is_some_and(|p| !matches!(p, "Uninitialized" | "Empty"));
    let body_uses_error_enum = !spec.error_codes.is_empty()
        && (uses_wrong_state_check
            || handler
                .effects
                .iter()
                .any(|(_, op_kind, _)| op_kind == "add" || op_kind == "sub"));
    if body_uses_error_enum {
        out.push_str("use crate::errors::*;\n");
    }
    // v2.24 S5c — variant-state lowering references `<Name>AccountInner`
    // directly inside the handler body (`match &mut self.<acct>.inner {
    // <Name>AccountInner::<post> { … } => … }`). Without an explicit
    // `use`, Rust's name resolution can't find the inner enum from
    // inside the per-handler module — `state::*` is already imported
    // when `render_struct` is true, but Anchor's handler scaffold
    // (the `!render_struct` branch) skips that import to keep the
    // common case lint-clean. Bring the inner enum in by name when
    // the spec actually uses variant-state lowering.
    // v2.24.0 follow-up: only emit the inner-enum import when the
    // wrapper-struct + inner-enum emission actually fires (Anchor
    // target). Quasar specs stay on the flat-struct path; no
    // `<Name>AccountInner` type exists, so importing it would
    // fail to resolve.
    if is_multi_variant_adt_state(spec) && matches!(target, Target::Anchor) {
        let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
        out.push_str(&format!("use crate::state::{};\n", inner_name));
    }
    if !render_struct {
        // Anchor: bring the Accounts struct (defined in lib.rs) into
        // scope so the impl block can reference it bare.
        if surface.needs_bumps_import(handler) {
            out.push_str(&format!("use crate::{{{}, {}}};\n", pascal, bumps_name));
        } else {
            out.push_str(&format!("use crate::{};\n", pascal));
        }
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
    // Match-arm-derived handlers (`liquidate_case_0`, `..._case_1`,
    // `..._otherwise`) don't appear in the source by their split name —
    // look them up under the parent handler's name. Both the `handler`
    // attribute and the `spec_hash` reference the parent so the qedgen
    // macro can resolve the block at compile time and every arm shares
    // the same drift-tracking key. (The split is purely a codegen
    // artifact; the spec contract is one block.)
    let parent_name: &str = if let Some(stripped) = handler.name.strip_suffix("_otherwise") {
        stripped.strip_suffix('_').unwrap_or(stripped)
    } else if let Some(idx) = handler.name.rfind("_case_") {
        &handler.name[..idx]
    } else {
        handler.name.as_str()
    };
    let parent_exists = spec_hash::spec_hash_for_handler(spec_src, parent_name).is_some();
    let attr_handler_name = if parent_exists {
        parent_name
    } else {
        handler.name.as_str()
    };
    let spec_h = spec_hash::spec_hash_for_handler(spec_src, attr_handler_name).unwrap_or_default();
    out.push_str(&format!(
        "    #[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\")]\n",
        spec_attr, attr_handler_name, BODY_HASH_PLACEHOLDER, spec_h
    ));

    out.push_str("    #[inline(always)]\n");

    let self_ref = if any_mut { "&mut self" } else { "&self" };
    let mut handler_params = vec![self_ref.to_string()];
    let mut param_names: Vec<String> = Vec::new();
    for (pname, ptype) in &handler.takes_params {
        handler_params.push(format!(
            "{}: {}",
            pname,
            map_type_for_target(ptype, spec, target)?
        ));
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
    if handler.has_bumps() {
        out.push_str("        let _ = bumps;\n");
    }

    // Spec-level `let` bindings (e.g. `let total_fee = amount * 125 / 10000`)
    // must be emitted BEFORE the effect block — effect RHSs reference them.
    // Pre-fix: they were dropped on the Rust side, leaving undefined-variable
    // errors on `cargo build`.
    for (binding_name, _lean_expr, rust_expr) in &handler.let_bindings {
        out.push_str(&format!("        let {} = {};\n", binding_name, rust_expr));
    }

    // v2.24 #11 — `let X = call …` bindings must be in scope when
    // subsequent effects / requires reference them. Emit bound calls
    // BEFORE the effect block. Unbound calls keep firing at the
    // tail (after effects) per the pre-v2.24 convention. Track
    // emitted-here calls so the tail emission skips them.
    let mut emitted_call_indices = std::collections::HashSet::new();
    let mut any_unmechanized_call_pre = false;
    for (idx, c) in handler.calls.iter().enumerate() {
        if c.result_binding.is_none() {
            continue;
        }
        match try_emit_anchor_cpi(c, handler, spec) {
            Some(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{} (binding: {})\n",
                    c.target_interface,
                    c.target_handler,
                    c.result_binding.as_deref().unwrap_or("_")
                ));
                out.push_str(&rendered);
                emitted_call_indices.insert(idx);
            }
            None => {
                let args = c
                    .args
                    .iter()
                    .map(|a| format!("{}={}", a.name, a.rust_expr))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "        // Spec call: {}.{}({}) (binding: {}) — needs fill\n",
                    c.target_interface,
                    c.target_handler,
                    args,
                    c.result_binding.as_deref().unwrap_or("_")
                ));
                any_unmechanized_call_pre = true;
                emitted_call_indices.insert(idx);
            }
        }
    }

    // Mechanical-effect expansion (v2.4-M3). For each spec effect we try to
    // emit a real Rust statement; anything non-mechanical stays as a comment
    // and forces a trailing `todo!()` so the user / an LLM (M4) fills it in.
    let state_acct = find_state_account(handler);
    let mut any_unmechanized = false;
    // v2.24 S5c — multi-variant ADT specs need a different lowering
    // shape: the wrapper-struct + inner-enum emission from S5b means
    // `self.<acct>.<field>` no longer resolves; effects must run
    // inside a `match &mut self.<acct>.inner { Inner::<post> { … }
    // => …, _ => Err(WrongState) }` block. Try the variant-aware
    // emitter first; on `None`, fall through to the per-effect
    // path (which will emit `// Spec effect (needs fill)` + the
    // trailing `todo!()` for cross-variant / non-mechanical shapes).
    let variant_body =
        state_acct.and_then(|sa| emit_variant_state_handler_body(handler, spec, target, sa));
    if let Some(body) = variant_body {
        out.push_str(&body);
    } else {
        for (idx, effect) in handler.effects.iter().enumerate() {
            // v2.24 §S1a — per-site error-variant override, indexed parallel
            // to `effects`. Missing entry = `None` (silent fallback to pragma
            // / built-in default inside mechanize_effect).
            let on_error = handler.effect_on_error.get(idx).and_then(|o| o.as_deref());
            let mechanized = state_acct
                .and_then(|sa| mechanize_effect(effect, on_error, sa, handler, spec, target));
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
    // surface. SPL Token calls get a real `anchor_spl::token::*` builder;
    // other interfaces fall through to a generic `invoke` shape, with
    // unmechanized cases emitting a structured comment + `todo!()` so an
    // LLM / human fills the body. The boolean tracks whether any call
    // site remained unmechanized so the tail `todo!()` only fires for
    // those.
    let mut any_unmechanized_call = false;
    for (idx, c) in handler.calls.iter().enumerate() {
        // v2.24 #11 — bound calls (result_binding = Some(_)) were
        // already emitted before the effect block so the binding
        // would be in scope for subsequent effects / requires.
        // Skip them here so we don't double-emit.
        if emitted_call_indices.contains(&idx) {
            continue;
        }
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

    let needs_fill = any_unmechanized
        || has_events
        || has_transfers
        || any_unmechanized_call
        || any_unmechanized_call_pre;
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

/// True if any rendered Rust expression in the spec references one of the
/// fixed-point helpers in `src/math.rs`. Used to gate the `use crate::math::*;`
/// import in `guards.rs` so legacy programs whose user-owned `lib.rs` doesn't
/// declare `pub mod math;` keep compiling.
pub(crate) fn guards_use_math_helpers(spec: &ParsedSpec) -> bool {
    let mut any = false;
    let probe = |s: &str| s.contains("mul_div_floor_u128") || s.contains("mul_div_ceil_u128");
    for h in &spec.handlers {
        if h.requires.iter().any(|r| probe(&r.rust_expr)) {
            any = true;
        }
        if h.aborts_if.iter().any(|a| probe(&a.rust_expr)) {
            any = true;
        }
        if h.ensures.iter().any(|e| probe(&e.rust_expr)) {
            any = true;
        }
        // Handler-level `let bindings: (lean_expr, rust_expr)` also lower to
        // `let X = mul_div_floor_u128(...)` in the emitted Rust handler body.
        // Without this, specs that compute fee math via a `let` (a common
        // pattern for splitting amounts before the effect block) wouldn't
        // pick up the math.rs import / inline helpers.
        if h.let_bindings.iter().any(|(_, _, r)| probe(r)) {
            any = true;
        }
    }
    for prop in &spec.properties {
        if let Some(ref r) = prop.rust_expression {
            if probe(r) {
                any = true;
            }
        }
    }
    any
}

/// Generate `src/math.rs` — small helper module with the fixed-point
/// `mul_div_*` primitives that property guards / handler bodies emit when
/// the spec uses `Expr::MulDivFloor` / `Expr::MulDivCeil`. Always emitted
/// because any non-trivial DeFi spec eventually wants them and the cost is
/// a few inlined functions; suppressing them would just create a
/// "generated-vs-not" coupling between the parser and codegen.
fn generate_math(fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/math.rs"));
    out.push_str("//! Fixed-point math helpers used by spec-derived guards and properties.\n\n");
    out.push_str("#![allow(dead_code)]\n\n");
    out.push_str(
        "/// Floor of `(a * b) / d`. Returns `0` if `d == 0` (caller must guard).\n\
/// Uses saturating multiplication as a safe approximation; specs that need\n\
/// exact u256-width fixed-point math should pin a checked widening crate\n\
/// once the spec language exposes one.\n\
#[inline]\n\
pub fn mul_div_floor_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 {\n\
        return 0;\n\
    }\n\
    a.saturating_mul(b) / d\n\
}\n\n",
    );
    out.push_str(
        "/// Ceiling of `(a * b) / d`. Same caveats as `mul_div_floor_u128`.\n\
#[inline]\n\
pub fn mul_div_ceil_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 {\n\
        return 0;\n\
    }\n\
    let prod = a.saturating_mul(b);\n\
    if prod % d == 0 {\n\
        prod / d\n\
    } else {\n\
        (prod / d).saturating_add(1)\n\
    }\n\
}\n",
    );
    out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(src_dir.join("math.rs"), &out)?;
    Ok(())
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
    // R26: `<ADT>Status` / `Status` enums live in `crate::state`. Pull
    // them in unconditionally — guards.rs always emits the enum-typed
    // pre-check / post-write when lifecycle is present, and a
    // never-used import is harmless under `#![allow(unused_imports)]`.
    out.push_str("use crate::state::*;\n");
    // `crate::math` carries `mul_div_floor_u128` / `mul_div_ceil_u128`.
    // Only import when a spec expression actually uses them, otherwise
    // existing `pub mod math;`-less lib.rs (user-owned, skip-if-exists)
    // would fail to resolve the path.
    if guards_use_math_helpers(spec) {
        out.push_str("use crate::math::*;\n");
    }
    // Pick up the per-handler `Accounts` structs. Anchor places them
    // at crate root (lib.rs); Quasar places them in
    // `instructions/<name>.rs` and re-exports via `instructions::*`.
    out.push_str(surface.guard_accounts_import());

    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        let any_mut = handler.accounts.iter().any(|a| a.is_writable);
        let self_ref = if any_mut { "&mut " } else { "&" };
        let mut params = vec![format!("ctx: {}{}{}", self_ref, pascal, lifetime_params)];
        for (pname, ptype) in &handler.takes_params {
            params.push(format!(
                "{}: {}",
                pname,
                map_type_for_target(ptype, spec, target)?
            ));
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

        // R26: lifecycle pre-status check. The spec's `: State.Pre ->
        // State.Post` expresses a state-machine transition; without a
        // runtime guard, every handler is reachable in every state
        // (which is how the multisig::propose proposal-erasure CRIT
        // surfaced — calling `propose` again from `HasProposal` zeroes
        // approval/rejection counts). The pre-check uses the `status:
        // u8` field added by `generate_state` and the `<ADT>Status`
        // enum's discriminator. We elide the check on init handlers
        // (Quasar's `init` zeroes the account, so `status == 0` is the
        // default; we just write the post variant). We also elide when
        // the spec doesn't declare lifecycle states for the relevant
        // ADT.
        let lifecycle_pre_check = lifecycle_check_line(handler, spec, false, &surface);
        let lifecycle_post_write = lifecycle_check_line(handler, spec, true, &surface);
        if !lifecycle_pre_check.is_empty() {
            out.push_str(&lifecycle_pre_check);
        }

        // v2.24 S5c — auth guard for fields that live in a variant
        // payload. R25's `auth X → has_one = X` suppresses the
        // Anchor `has_one` attribute under multi-variant ADT
        // because the macro can't reach `wrapper.inner.<variant>.X`
        // (see `is_multi_variant_adt_with_field_in_variant` in
        // check.rs). Replace it with an explicit destructure-then-
        // compare guard so the auth check still fires at runtime.
        // Requires:
        //   - multi-variant ADT spec
        //   - handler declares `auth X` where X is a variant-payload
        //     field on the pre-variant
        //   - handler binds a signer account named `X`
        //   - the spec declares `Unauthorized` in `type Error`
        // Missing any condition: silently skip — the auth gap shows
        // up as a `qedgen check` warning (`no_access_control` / R25
        // friend) rather than a compile error.
        let auth_guard = emit_variant_auth_guard(handler, spec, target);
        if !auth_guard.is_empty() {
            out.push_str(&auth_guard);
        }

        let err_enum_name_r28 = format!("{}Error", to_pascal_case(&spec.program_name));
        let _ = &err_enum_name_r28;
        // R28: per-handler PDA verification. R13 suppresses
        // `seeds = [...]` on Quasar non-init handlers when seeds
        // reference state fields (the macro's `Bumps::seeds()` method
        // can't auto-capture `self.<state-field>`). Owner+discriminator
        // protects against type confusion but not wrong-PDA passing —
        // the audit's MED-tier finding. Emit a runtime
        // `verify_program_address` check using the stored bump for
        // every account whose `seeds = [...]` would have been
        // suppressed. The cost is one syscall (~544 CU on first-try
        // bump 255) per affected handler load.
        for acct in &handler.accounts {
            let Some(ref seeds) = acct.pda_seeds else {
                continue;
            };
            let is_init_target = matches!(
                handler.pre_status.as_deref(),
                Some("Uninitialized") | Some("Empty")
            ) && match handler.on_account.as_deref() {
                Some(adt) => {
                    let lower = adt.to_lowercase();
                    acct.name == lower || acct.name.starts_with(&lower)
                }
                None => true,
            } && !acct.is_signer;
            if is_init_target {
                continue; // init flow already verifies via #[account(seeds=…, bump)]
            }
            // Was R13 going to suppress on this handler? Mirror the
            // detection logic from `quasar_account_attr`.
            let bound_account_names: std::collections::HashSet<&str> =
                handler.accounts.iter().map(|a| a.name.as_str()).collect();
            let needs_state_field_seed = seeds.iter().any(|seed| {
                let is_literal = seed.starts_with('"') && seed.ends_with('"');
                !is_literal && !bound_account_names.contains(seed.as_str())
            });
            if !matches!(target, Target::Quasar) || !needs_state_field_seed {
                continue;
            }

            let mut seed_exprs: Vec<String> = Vec::with_capacity(seeds.len() + 1);
            for seed in seeds {
                if let Some(inner) = seed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    seed_exprs.push(format!("b\"{}\"", inner));
                } else if bound_account_names.contains(seed.as_str()) {
                    // Handler-bound account: read its address.
                    seed_exprs.push(format!("ctx.{}.to_account_view().address().as_ref()", seed));
                } else {
                    // State-field seed: read off the same PDA's stored
                    // value (the field that R13 couldn't pass through
                    // the macro's seeds method).
                    seed_exprs.push(format!("ctx.{}.{}.as_ref()", acct.name, seed));
                }
            }
            seed_exprs.push(format!("&[ctx.{}.bump]", acct.name));

            out.push_str(&format!(
                "    // R28 PDA check: ctx.{acct} matches its declared seeds\n    {{\n        let __seeds: &[&[u8]] = &[{seeds}];\n        if quasar_lang::pda::verify_program_address(__seeds, &crate::ID, ctx.{acct}.to_account_view().address()).is_err() {{\n            return Err(ProgramError::from({err_enum}::InvalidPda));\n        }}\n    }}\n",
                acct = acct.name,
                seeds = seed_exprs.join(", "),
                err_enum = err_enum_name_r28,
            ));
        }

        // R27: token-vault authority binding. The spec declares
        // `pool_vault : token, authority pool` — meaning the SPL token
        // account's `owner` field (i.e. the entity that can sign
        // transfers from it) must equal the `pool` PDA's address. R6
        // dropped Quasar's `token::authority = X` constraint on
        // non-init accounts (the macro rejects it without `init`), so
        // the static check is gone for every load after init. Without
        // a runtime equivalent the pool_vault parameter could be any
        // SPL-Token-program-owned account, breaking the deposit/repay/
        // liquidate transfer routing intent (audit HIGH 5).
        //
        // Emit a runtime owner check on every non-init token account
        // that declares `authority X` — the token account's `owner()`
        // accessor returns the authority address, compared against the
        // bound account's address.
        let err_enum_name = format!("{}Error", to_pascal_case(&spec.program_name));
        for acct in &handler.accounts {
            let is_init_target = matches!(
                handler.pre_status.as_deref(),
                Some("Uninitialized") | Some("Empty")
            ) && match handler.on_account.as_deref() {
                Some(adt) => {
                    let lower = adt.to_lowercase();
                    acct.name == lower || acct.name.starts_with(&lower)
                }
                None => true,
            } && acct.pda_seeds.is_some()
                && !acct.is_signer;
            let is_token = acct.account_type.as_deref() == Some("token");
            if !is_token || is_init_target {
                continue;
            }
            let Some(ref auth_name) = acct.authority else {
                continue;
            };
            let unauthorized = if spec.error_codes.iter().any(|c| c == "Unauthorized") {
                "Unauthorized"
            } else {
                "InvalidLifecycle"
            };
            let err_expr = surface.error_expr(&err_enum_name, unauthorized);
            let check_expr = surface.authority_check_expr(&acct.name, auth_name);
            out.push_str(&format!(
                "    // authority: {}\n    if {} {{ return Err({}); }}\n",
                check_expr, check_expr, err_expr,
            ));
        }

        if handler.requires.is_empty()
            && handler.aborts_if.is_empty()
            && lifecycle_pre_check.is_empty()
            && lifecycle_post_write.is_empty()
        {
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
        // Bare handler-account idents in spec expressions (e.g. the
        // `approver` in `state.members[i] == approver`) need to be
        // lowered to the runtime pubkey load `*ctx.<name>.to_account_view().address()`.
        // Without this, the spec's signer-binding compiles to `... ==
        // approver` where `approver` resolves to nothing in scope.
        let handler_account_names: Vec<String> =
            handler.accounts.iter().map(|a| a.name.clone()).collect();
        let bind_state = |expr: &str| -> String {
            // Step 1: rewrite handler-account idents to address loads.
            let mut after_accounts = String::with_capacity(expr.len() + 32);
            let bytes = expr.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
                let mut matched = false;
                if prev_ok {
                    for name in &handler_account_names {
                        let nbytes = name.as_bytes();
                        if i + nbytes.len() <= bytes.len() && &bytes[i..i + nbytes.len()] == nbytes
                        {
                            // Boundary check on the trailing edge: don't
                            // match `approver_x` when looking for `approver`.
                            let after = i + nbytes.len();
                            if after >= bytes.len() || !is_ident_char(bytes[after]) {
                                // `<acct>.pubkey` is the spec-author's
                                // way of saying "this account's address"
                                // — lower to the same address-load form
                                // we use for bare `<acct>` so a
                                // `requires acct.pubkey == state.field`
                                // clause compiles.
                                let pubkey_marker = b".pubkey";
                                let after_dot_end = after + pubkey_marker.len();
                                if after_dot_end <= bytes.len()
                                    && &bytes[after..after_dot_end] == pubkey_marker
                                    && (after_dot_end == bytes.len()
                                        || !is_ident_char(bytes[after_dot_end]))
                                {
                                    after_accounts.push_str(&surface.account_key_expr(name));
                                    i = after_dot_end;
                                    matched = true;
                                    break;
                                }
                                // Don't rewrite `name.` (field access on
                                // the handler-account is a different
                                // expression — keep the `.` access path).
                                if after >= bytes.len() || bytes[after] != b'.' {
                                    after_accounts.push_str(&surface.account_key_expr(name));
                                    i = after;
                                    matched = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if !matched {
                    after_accounts.push(bytes[i] as char);
                    i += 1;
                }
            }

            // Step 2: rewrite `s.` to `ctx.<state>.` if we have a state
            // account. Word-bounded so `accounts[i].fee_credits.get()`
            // doesn't get corrupted to `fee_creditctx.vault.get()`.
            let Some(sa) = state_acct else {
                return after_accounts;
            };
            let target = format!("ctx.{}.", sa.name);
            let bytes = after_accounts.as_bytes();
            let mut out = String::with_capacity(after_accounts.len());
            let mut i = 0;
            while i < bytes.len() {
                let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
                if prev_ok && i + 1 < bytes.len() && bytes[i] == b's' && bytes[i + 1] == b'.' {
                    out.push_str(&target);
                    i += 2;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            out
        };

        // Pick the Pod-aware rust expression on Quasar so Pod field
        // accesses carry `.get()` and mixed-kind binops add `as i128`
        // casts — without it `state.foo.x + state.foo.y` fails when
        // `x: PodU128` and `y: PodI128`.
        let pod_target = matches!(target, Target::Quasar);

        for req in &handler.requires {
            // Emit as a comment for human readers + an executable check.
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
            let raw = if pod_target {
                req.rust_expr_pod.trim()
            } else {
                req.rust_expr.trim()
            };
            let rust = bind_state(raw);
            if let Some(err) = &req.error_name {
                out.push_str(&format!(
                    "    if !({}) {{ return Err({}); }}\n",
                    rust,
                    surface.error_expr(&err_enum, err),
                ));
            } else {
                // Bare `requires` (no `else <ErrorCode>`). Pre-v2.14 emitted
                // `debug_assert!`, which silently no-ops in release builds —
                // every bare requires would skip its check in production.
                // Emit a real runtime check with `ProgramError::Custom(0xFF)`
                // (sentinel "predicate violated, no specific error code").
                // The auditor's `bounty_intent_drift` predicate flags
                // bare requires as P3 — users should still add an explicit
                // `else <Error>` for diagnostic clarity, but the check now
                // runs either way.
                out.push_str(&format!(
                    "    if !({}) {{ return Err({}); }}\n",
                    rust,
                    surface.generic_error_expr()
                ));
            }
        }

        let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
        for ab in &handler.aborts_if {
            let raw = if pod_target {
                ab.rust_expr_pod.trim()
            } else {
                ab.rust_expr.trim()
            };
            let rust = bind_state(raw);
            out.push_str(&format!(
                "    if ({}) {{ return Err({}); }}\n",
                rust,
                surface.error_expr(&err_enum, &ab.error_name),
            ));
        }

        // R26: lifecycle post-status write — runs after all guards have
        // passed so a failed guard doesn't half-transition. Only emitted
        // when the post variant differs from the pre variant.
        if !lifecycle_post_write.is_empty() {
            out.push_str(&lifecycle_post_write);
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

/// Sections of `Cargo.toml` that qedgen owns and rewrites on every
/// `qedgen codegen` run. Sections outside this set (e.g.,
/// `[dev-dependencies]`, `[profile.release]`, custom feature flags) are
/// preserved verbatim when the file already exists — see
/// [`merge_cargo_toml`] / PRD-v2.21 §S2.3.
///
/// `[dependencies]` is qedgen-owned but with a sub-table preserve pass
/// inside [`merge_cargo_toml`] (any user-added crate stays; qedgen-owned
/// crates are upserted).
const QEDGEN_OWNED_SECTIONS: &[&str] = &["package", "lib", "features", "dependencies", "workspace"];

/// Crates qedgen manages inside `[dependencies]`. Other crates the user
/// adds to that section are preserved by [`merge_cargo_toml`].
const QEDGEN_OWNED_DEPS: &[&str] = &[
    "anchor-lang",
    "anchor-spl",
    "quasar-lang",
    "quasar-spl",
    "qedgen-macros",
];

/// Generate Cargo.toml.
///
/// v2.21 S2.3: preserves user-added sections (`[dev-dependencies]`,
/// `[profile.*]`, custom `[features.X]` arms, etc.) when the file
/// already exists. The qedgen-owned set (`QEDGEN_OWNED_SECTIONS`) is
/// rewritten on every run. Inside `[dependencies]`, qedgen upserts its
/// owned crates (`QEDGEN_OWNED_DEPS`) and leaves any other dep lines
/// untouched. Greenfield runs (no existing file) emit a fresh skeleton.
fn generate_cargo_toml(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let fresh = render_qedgen_cargo_toml(spec, fp, target);
    let path = output_dir.join("Cargo.toml");
    let final_toml = match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => merge_cargo_toml(&existing, &fresh),
        _ => fresh,
    };
    std::fs::write(path, final_toml)?;
    Ok(())
}

fn render_qedgen_cargo_toml(spec: &ParsedSpec, fp: &SpecFingerprint, target: Target) -> String {
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

    out
}

/// Merge a freshly rendered qedgen Cargo.toml (`fresh`) with the on-disk
/// content (`existing`) so user-added sections + deps survive.
///
/// Algorithm:
/// 1. Parse both files into a list of `(section_header, body_lines)` plus
///    a preamble (comments / docs before the first section).
/// 2. Walk the existing file's sections. For each section:
///    - If the section is in `QEDGEN_OWNED_SECTIONS` and isn't
///      `dependencies`: replace its body with the fresh body.
///    - If it's `dependencies`: upsert each qedgen-owned dep line from
///      the fresh body, preserve all other deps from the existing body.
///    - Otherwise: keep the existing body verbatim.
/// 3. Append any qedgen-owned section that's missing from the existing
///    file (greenfield-style fallback).
///
/// The preamble is replaced with the fresh preamble (carries the
/// `GENERATED BY QEDGEN` marker + spec-hash); user comments above the
/// first section don't survive a regen. Trade-off documented in
/// PRD-v2.21 §S2.3.
fn merge_cargo_toml(existing: &str, fresh: &str) -> String {
    let fresh_sections = parse_toml_sections(fresh);
    let existing_sections = parse_toml_sections(existing);

    let mut out = String::new();
    out.push_str(&fresh_sections.preamble);

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (name, existing_body) in &existing_sections.sections {
        let body = if QEDGEN_OWNED_SECTIONS.contains(&name.as_str()) {
            if name == "dependencies" {
                merge_dependencies_section(existing_body, lookup_section(&fresh_sections, name))
            } else {
                lookup_section(&fresh_sections, name).to_string()
            }
        } else {
            existing_body.clone()
        };
        push_section(&mut out, name, &body);
        seen.insert(name.clone());
    }
    // Append qedgen-owned sections not present in `existing`.
    for (name, body) in &fresh_sections.sections {
        if seen.contains(name) {
            continue;
        }
        push_section(&mut out, name, body);
    }
    out
}

fn lookup_section<'a>(parsed: &'a ParsedToml, name: &str) -> &'a str {
    parsed
        .sections
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.as_str())
        .unwrap_or("")
}

fn push_section(out: &mut String, name: &str, body: &str) {
    out.push_str(&format!("[{name}]\n"));
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

fn merge_dependencies_section(existing: &str, fresh: &str) -> String {
    let fresh_lines: Vec<&str> = fresh.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = String::new();
    let mut managed_emitted: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();

    // Pass 1: walk existing lines. Replace qedgen-owned deps with the
    // fresh value; keep everything else.
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let dep_name = trimmed
            .split('=')
            .next()
            .map(|s| s.trim())
            .unwrap_or("")
            .trim_matches('"');
        if let Some(owned) = QEDGEN_OWNED_DEPS.iter().find(|d| **d == dep_name) {
            if let Some(fresh_line) = fresh_lines
                .iter()
                .find(|fl| fl.trim_start().starts_with(&format!("{owned} =")))
            {
                out.push_str(fresh_line);
                out.push('\n');
                managed_emitted.insert(*owned);
            } else {
                // Fresh render dropped this dep (e.g. needs_spl=false now)
                // — also drop it from the merged output.
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    // Pass 2: append qedgen-owned deps that didn't appear in the existing
    // file (greenfield deps).
    for owned in QEDGEN_OWNED_DEPS {
        if managed_emitted.contains(*owned) {
            continue;
        }
        if let Some(fresh_line) = fresh_lines
            .iter()
            .find(|fl| fl.trim_start().starts_with(&format!("{owned} =")))
        {
            out.push_str(fresh_line);
            out.push('\n');
        }
    }
    out
}

struct ParsedToml {
    preamble: String,
    sections: Vec<(String, String)>,
}

/// Split a TOML string into (preamble, [(section_name, body)]). Section
/// names are normalized to their canonical bracket form sans whitespace —
/// `[ workspace ]` and `[workspace]` both register as `"workspace"`.
/// Sub-tables (`[package.metadata.foo]`) are preserved as their full
/// path string. Hand-rolled (no `toml` crate dep) because we only need
/// section-level granularity and the input is well-formed by
/// construction (qedgen emits + user edits).
fn parse_toml_sections(text: &str) -> ParsedToml {
    let mut preamble = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                let name = rest[..end].trim().to_string();
                if let Some(prev) = current.take() {
                    sections.push(prev);
                }
                current = Some((name, String::new()));
                continue;
            }
        }
        match current.as_mut() {
            Some((_, body)) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {
                preamble.push_str(line);
                preamble.push('\n');
            }
        }
    }
    if let Some(prev) = current.take() {
        sections.push(prev);
    }
    ParsedToml { preamble, sections }
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
/// `Target::Pinocchio` is rejected at the `init` dispatcher and never
/// reaches this function.
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
    if guards_use_math_helpers(&spec) {
        generate_math(&fp, output_dir)?;
    }
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
        // v2.21 Slice 3: Standalone Pubkey lowers to [u8; 32] (was
        // "Address" pre-v2.21; the alias is retired).
        assert_eq!(map_type("Pubkey", &spec).unwrap(), "[u8; 32]");
    }

    #[test]
    fn map_type_anchor_uses_native_pubkey() {
        let spec = empty_spec();

        assert_eq!(map_type_anchor("Pubkey", &spec).unwrap(), "Pubkey");
        assert_eq!(
            map_type_anchor("Map[2] Pubkey", &spec).unwrap(),
            "[Pubkey; 2]"
        );
    }

    #[test]
    fn framework_surface_centralizes_target_snippets() {
        let anchor = FrameworkSurface::for_target(Target::Anchor);
        assert_eq!(
            anchor.token_account_type(true),
            "Account<'info, TokenAccount>"
        );
        assert_eq!(
            anchor.program_type("token_program", None, false),
            "Program<'info, Token>"
        );
        assert_eq!(
            anchor.error_expr("EscrowError", "Unauthorized"),
            "EscrowError::Unauthorized.into()"
        );
        assert_eq!(
            anchor.authority_check_expr("escrow_ta", "escrow"),
            "ctx.escrow_ta.owner != ctx.escrow.key()"
        );

        let quasar = FrameworkSurface::for_target(Target::Quasar);
        assert_eq!(quasar.token_account_type(true), "&'info mut Account<Token>");
        assert_eq!(
            quasar.program_type("token_program", None, false),
            "&'info Program<Token>"
        );
        assert_eq!(
            quasar.program_type("system_program", None, false),
            "&'info Program<System>"
        );
        assert_eq!(
            quasar.error_expr("EscrowError", "Unauthorized"),
            "ProgramError::from(EscrowError::Unauthorized)"
        );
        assert_eq!(
            quasar.authority_check_expr("escrow_ta", "escrow"),
            "(*ctx.escrow_ta.owner()) != (*ctx.escrow.to_account_view().address())"
        );
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
        // v2.21 Slice 3: nested Pubkey lowers through `[u8; 32]` too.
        assert_eq!(map_type("Map[32] Pubkey", &spec).unwrap(), "[[u8; 32]; 32]");
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
    fn anchor_scaffold_imports_compile_support_for_tokens_bumps_and_guards() {
        let src = r#"spec Escrow

type State
  | Uninitialized
  | Open of {
      initializer : Pubkey,
    }
  | Closed

pda escrow ["escrow", initializer]

type Error
  | Unauthorized

handler initialize (amount : U64) : State.Uninitialized -> State.Open {
  auth initializer
  accounts {
    initializer   : signer, writable
    escrow        : writable, pda ["escrow", initializer]
    escrow_ta     : writable, type token, authority escrow
    token_program : program
  }
  requires amount > 0 else Unauthorized
  effect {
    initializer := initializer.pubkey
  }
}

handler cancel : State.Open -> State.Closed {
  auth initializer
  accounts {
    initializer   : signer, writable
    escrow        : writable, pda ["escrow", initializer]
    escrow_ta     : writable, type token, authority escrow
    token_program : program
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("escrow.qedspec");
        let out_dir = dir.path().join("programs");
        std::fs::write(&spec_path, src).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_state(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_errors(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_instructions(&spec, &fp, &spec_path, &out_dir, Target::Anchor).unwrap();
        generate_guards(&spec, &fp, &out_dir, Target::Anchor).unwrap();

        let lib = std::fs::read_to_string(out_dir.join("src/lib.rs")).unwrap();
        // No mint accounts in this spec, so `Mint` should be omitted to
        // keep the rendered scaffold warning-clean. See Workstream F in
        // `docs/prds/PRD-v2.11-codegen-simplification.md`.
        assert!(lib.contains("use anchor_spl::token::{Token, TokenAccount};"));
        assert!(!lib.contains("Mint, Token, TokenAccount"));
        assert!(lib.contains("pub token_program: Program<'info, Token>"));

        let state = std::fs::read_to_string(out_dir.join("src/state.rs")).unwrap();
        assert!(state.contains("initializer: Pubkey"));
        assert!(!state.contains("pub type Address = Pubkey;"));

        let init = std::fs::read_to_string(out_dir.join("src/instructions/initialize.rs")).unwrap();
        assert!(init.contains("use crate::{Initialize, InitializeBumps};"));

        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();
        assert!(guards.contains("ctx.escrow_ta.owner != ctx.escrow.key()"));
        assert!(guards.contains("EscrowError::Unauthorized.into()"));
        assert!(guards.contains("EscrowError::InvalidLifecycle.into()"));
        assert!(!guards.contains("to_account_view"));
    }

    /// v2.24 S5b — multi-variant ADT state lowers to the
    /// `#[account] pub struct Wrapper { pub inner: WrapperInner }` +
    /// `pub enum WrapperInner { … }` pattern. Smoke fixture at
    /// `/tmp/anchor_enum_test/` validated against Anchor 0.32.1
    /// (see [[reference_anchor_account_struct_only]]). This test
    /// pins the codegen output shape so a future regression that
    /// flips back to flat-struct + `status: u8` fails fast.
    #[test]
    fn multi_variant_adt_emits_wrapper_struct_plus_inner_enum() {
        let src = r#"spec Escrow

type State
  | Uninitialized
  | Open of {
      initializer        : Pubkey,
      taker              : Pubkey,
      initializer_amount : U64,
      taker_amount       : U64,
    }
  | Closed

type Error
  | InvalidAmount
  | WrongState

handler initialize (amount : U64) : State.Uninitialized -> State.Open {
  auth initializer
  accounts {
    initializer : signer, writable
  }
  requires amount > 0 else InvalidAmount
  effect {
    Open.initializer := initializer.pubkey
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_state(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        let state = std::fs::read_to_string(out_dir.join("src/state.rs")).unwrap();

        // Wrapper struct carries `#[account]` + `InitSpace` and owns
        // the discriminator. The smoke fixture proved this is the
        // only pattern Anchor 0.32.1's struct-only `#[account]`
        // macro accepts.
        assert!(
            state.contains("#[account]\n#[derive(InitSpace)]\npub struct EscrowAccount"),
            "missing wrapper struct shape; got:\n{state}"
        );
        assert!(
            state.contains("pub inner: EscrowAccountInner"),
            "wrapper struct should hold the inner enum as `pub inner: …`; got:\n{state}"
        );

        // Inner enum carries the actual variants. No-payload variants
        // stay unit-style; payload variants get struct-style fields.
        assert!(
            state.contains(
                "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Debug, PartialEq)]"
            ),
            "inner enum should carry Borsh + Clone + Debug + PartialEq derives; got:\n{state}"
        );
        assert!(
            state.contains("pub enum EscrowAccountInner"),
            "missing inner enum; got:\n{state}"
        );
        assert!(
            state.contains("Uninitialized,"),
            "unit-style variant missing; got:\n{state}"
        );
        assert!(
            state.contains("Closed,"),
            "second unit-style variant missing; got:\n{state}"
        );
        assert!(
            state.contains("Open {")
                && state.contains("initializer: Pubkey,")
                && state.contains("initializer_amount: u64,"),
            "payload variant missing fields; got:\n{state}"
        );

        // The legacy flat-fields shape must NOT appear: no
        // `status: u8` discriminator field on the wrapper, no
        // parallel `pub enum Status` enum, and no top-level
        // `pub initializer:` on the wrapper struct.
        assert!(
            !state.contains("pub status: u8"),
            "legacy `status: u8` discriminator should not appear for multi-variant ADT; got:\n{state}"
        );
        assert!(
            !state.contains("pub enum Status {"),
            "legacy `Status` enum should not appear for multi-variant ADT; got:\n{state}"
        );
        assert!(
            !state.contains("pub initializer: Pubkey,\n    pub taker: Pubkey,"),
            "wrapper should not carry flattened fields directly; got:\n{state}"
        );
    }

    /// v2.24 S5c — same-variant handler bodies lower to a
    /// `match &mut self.<acct>.inner { Inner::<post> { … } => { … },
    ///  _ => Err(WrongState.into()) }` block when the spec is a
    /// multi-variant ADT. Locks the new emission shape so a future
    /// regression that emits the legacy `self.<acct>.<field> = …`
    /// (which no longer compiles against the S5b wrapper+enum) fails
    /// fast.
    #[test]
    fn variant_state_lowers_same_variant_handler_to_match_block() {
        let src = r#"spec Vault

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow
  | WrongState

handler deposit (amount : U64) : State.Active -> State.Active {
  auth owner
  accounts {
    vault : writable
    owner : signer
  }
  requires amount > 0 else MathOverflow
  effect {
    Active.balance += amount
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        let out_dir = dir.path().join("programs");
        std::fs::write(&spec_path, src).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_instructions(&spec, &fp, &spec_path, &out_dir, Target::Anchor).unwrap();

        let deposit = std::fs::read_to_string(out_dir.join("src/instructions/deposit.rs")).unwrap();

        // Match-wrapped body lands.
        assert!(
            deposit.contains("match &mut self.vault.inner"),
            "expected variant match destructure; got:\n{deposit}"
        );
        assert!(
            deposit.contains("VaultAccountInner::Active { balance, .. } =>"),
            "expected destructure pattern naming only the mutated field; got:\n{deposit}"
        );

        // Effect line uses the destructured binding (no `self.vault.balance`).
        assert!(
            deposit.contains(
                "*balance = balance.checked_add(amount).ok_or(VaultError::MathOverflow)?;"
            ),
            "expected destructured checked_add line; got:\n{deposit}"
        );

        // Wrong-variant fallthrough.
        assert!(
            deposit.contains("_ => return Err(VaultError::WrongState.into()),"),
            "expected WrongState fallthrough arm; got:\n{deposit}"
        );

        // Legacy `self.vault.balance` shape must NOT appear.
        assert!(
            !deposit.contains("self.vault.balance"),
            "legacy flat-field handler body should not appear under variant-state lowering; got:\n{deposit}"
        );
    }

    /// v2.24 S5c — guards module emits a `matches!(inner, Inner::<pre>
    /// { .. })` lifecycle check instead of the legacy
    /// `ctx.<acct>.status != Status::<pre> as u8` byte compare when the
    /// state is a multi-variant ADT. Wrapper has no `status` field
    /// under the new emission, so the byte compare would no longer
    /// compile. Also locks the `has_one` suppression for fields that
    /// live in variant payloads — without it the wrapper-side
    /// `has_one = X` macro tries to read `wrapper.X` and fails with
    /// "no field X on Account<…, Wrapper>". Both gaps were caught by
    /// the end-to-end `cargo check` smoke on the Vault fixture.
    #[test]
    fn variant_state_guards_use_matches_and_skip_has_one() {
        let src = r#"spec Vault

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow
  | WrongState
  | InvalidLifecycle
  | Unauthorized

handler deposit (amount : U64) : State.Active -> State.Active {
  auth owner
  accounts {
    vault : writable
    owner : signer
  }
  requires amount > 0 else MathOverflow
  effect {
    Active.balance += amount
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        let out_dir = dir.path().join("programs");
        std::fs::write(&spec_path, src).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_guards(&spec, &fp, &out_dir, Target::Anchor).unwrap();

        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();
        assert!(
            guards.contains("if !matches!(ctx.vault.inner, VaultAccountInner::Active { .. })"),
            "expected `matches!` lifecycle check; got:\n{guards}"
        );
        assert!(
            !guards.contains("Status::Active as u8"),
            "legacy status byte compare must not appear under variant-state lowering; got:\n{guards}"
        );

        let lib = std::fs::read_to_string(out_dir.join("src/lib.rs")).unwrap();
        assert!(
            !lib.contains("has_one = owner"),
            "has_one suppression failed — wrapper-side `has_one = owner` should not appear because owner lives in variant payload; got:\n{lib}"
        );
    }

    /// v2.24 S5c-auth — when `has_one` is suppressed under multi-variant
    /// ADT (the auth field lives in a variant payload, see
    /// 01dc057), guards.rs picks up a destructure-then-compare
    /// auth check so the security gap is closed at runtime.
    /// Without this the wrapper-side `has_one = owner` is silently
    /// dropped and any signer can call the handler.
    #[test]
    fn variant_state_guards_emit_auth_check_when_has_one_suppressed() {
        let src = r#"spec Vault

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow
  | WrongState
  | InvalidLifecycle
  | Unauthorized

handler deposit (amount : U64) : State.Active -> State.Active {
  auth owner
  accounts {
    vault : writable
    owner : signer
  }
  requires amount > 0 else MathOverflow
  effect {
    Active.balance += amount
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_guards(&spec, &fp, &out_dir, Target::Anchor).unwrap();

        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();

        // Destructure-then-compare guard fires.
        assert!(
            guards.contains(
                "let VaultAccountInner::Active { owner: auth_field, .. } = &ctx.vault.inner"
            ),
            "expected destructure binding of variant-payload auth field; got:\n{guards}"
        );
        assert!(
            guards.contains("if auth_field != &ctx.owner.key()"),
            "expected key comparison against signer account; got:\n{guards}"
        );
        assert!(
            guards.contains("return Err(VaultError::Unauthorized.into())"),
            "expected Unauthorized error on auth mismatch; got:\n{guards}"
        );
    }

    /// v2.24 S5c-auth — when `Unauthorized` isn't declared in
    /// `type Error`, the auth guard suppresses itself rather than
    /// emitting a dangling reference. The `has_one` is already
    /// suppressed too (separate gate), so the spec needs a manual
    /// auth check elsewhere — which the lint side surfaces via
    /// `no_access_control` / R25-friend warnings. Locks the
    /// silent-skip behavior.
    #[test]
    fn variant_state_auth_guard_skips_when_unauthorized_undeclared() {
        let src = r#"spec Vault

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow
  | WrongState
  | InvalidLifecycle

handler deposit (amount : U64) : State.Active -> State.Active {
  auth owner
  accounts {
    vault : writable
    owner : signer
  }
  requires amount > 0 else MathOverflow
  effect {
    Active.balance += amount
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_guards(&spec, &fp, &out_dir, Target::Anchor).unwrap();

        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();
        assert!(
            !guards.contains("Unauthorized"),
            "auth guard should not emit reference to undeclared Unauthorized variant; got:\n{guards}"
        );
        assert!(
            !guards.contains("owner: auth_field"),
            "auth destructure should not appear when Unauthorized is undeclared; got:\n{guards}"
        );
    }

    /// v2.24 S5c — cross-variant (init / promote) handlers now lower
    /// to an explicit `self.<acct>.inner = <Inner>::<Post> { … };`
    /// assignment. Pre-variant must be unit-style (no payload); every
    /// post-variant field must have a matching `Active.field := <rhs>`
    /// effect. Bail-outs (payload-carrying pre, missing fields,
    /// complex RHS) fall through to the per-effect `todo!()` path —
    /// a separate test covers those.
    #[test]
    fn variant_state_lowers_cross_variant_init_to_set_inner() {
        let src = r#"spec Vault

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow
  | WrongState
  | InvalidLifecycle
  | Unauthorized

handler create (initial : U64) : State.Uninitialized -> State.Active {
  auth owner
  accounts {
    vault : writable
    owner : signer
  }
  requires initial > 0 else MathOverflow
  effect {
    Active.balance := initial
    Active.owner := owner.pubkey
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        let out_dir = dir.path().join("programs");
        std::fs::write(&spec_path, src).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_instructions(&spec, &fp, &spec_path, &out_dir, Target::Anchor).unwrap();

        let create = std::fs::read_to_string(out_dir.join("src/instructions/create.rs")).unwrap();

        // Assignment shape lands. Note: declared field order matters
        // — `owner` first, then `balance` — to match the variant
        // declaration in the spec.
        assert!(
            create.contains("self.vault.inner = VaultAccountInner::Active {"),
            "expected variant assignment; got:\n{create}"
        );
        assert!(
            create.contains("owner: self.owner.key(),"),
            "expected `.pubkey` RHS to lower to `self.<acct>.key()`; got:\n{create}"
        );
        assert!(
            create.contains("balance: initial,"),
            "expected param RHS to lower as bare ident; got:\n{create}"
        );

        // No leftover `todo!()` — fully mechanized.
        assert!(
            !create.contains("todo!("),
            "cross-variant init should be fully mechanized, no todo!(); got:\n{create}"
        );

        // Init handlers (`Uninitialized` pre) elide the
        // `matches!(.., Pre)` pre-check since `#[account(init, …)]`
        // zeroes the account before the body runs.
        assert!(
            !create.contains("if !matches!(self.vault.inner, VaultAccountInner::Uninitialized"),
            "init handler should skip the pre-variant check; got:\n{create}"
        );
    }

    /// v2.24 S5c — cross-variant emitter bails (returns `None`) when
    /// any post-variant field has no corresponding effect line. Silent
    /// defaults would hide bugs (e.g. a zeroed Pubkey owner). The
    /// fallback per-effect path then surfaces a clear `todo!()`.
    #[test]
    fn variant_state_bails_on_cross_variant_missing_field() {
        let src = r#"spec Vault

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow
  | WrongState

handler create (initial : U64) : State.Uninitialized -> State.Active {
  auth owner
  accounts {
    vault : writable
    owner : signer
  }
  requires initial > 0 else MathOverflow
  effect {
    Active.balance := initial
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        let out_dir = dir.path().join("programs");
        std::fs::write(&spec_path, src).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Anchor).unwrap();
        generate_instructions(&spec, &fp, &spec_path, &out_dir, Target::Anchor).unwrap();

        let create = std::fs::read_to_string(out_dir.join("src/instructions/create.rs")).unwrap();

        // Missing `owner` field → cross-variant emitter bails.
        assert!(
            !create.contains("self.vault.inner = VaultAccountInner::Active {"),
            "cross-variant emitter should bail on missing post-variant field; got:\n{create}"
        );
        assert!(
            create.contains("todo!("),
            "fallback should surface a todo!() for the user to fill; got:\n{create}"
        );
    }

    /// Quasar twin of the Anchor scaffold-imports test. Workstreams A + B
    /// (target-aware type mappers + `FrameworkSurface` boundary) and F
    /// (conditional imports + warning gating) reshaped both targets'
    /// emission. The Anchor side is covered above; this test pins the
    /// Quasar side so a regression in the shared `FrameworkSurface`
    /// surface fails fast at the unit level — without depending on the
    /// drift gate (which can hide changes if bundled examples are
    /// regenerated in the same commit) or on a `cargo check` smoke (slow
    /// and pulls quasar-lang at build time).
    #[test]
    fn quasar_scaffold_emits_target_specific_surface() {
        let src = r#"spec Escrow

type State
  | Uninitialized
  | Open of {
      initializer : Pubkey,
    }
  | Closed

pda escrow ["escrow", initializer]

type Error
  | Unauthorized

handler initialize (amount : U64) : State.Uninitialized -> State.Open {
  auth initializer
  accounts {
    initializer   : signer, writable
    escrow        : writable, pda ["escrow", initializer]
    escrow_ta     : writable, type token, authority escrow
    token_program : program
  }
  requires amount > 0 else Unauthorized
  effect {
    initializer := initializer.pubkey
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("escrow.qedspec");
        let out_dir = dir.path().join("programs");
        std::fs::write(&spec_path, src).unwrap();

        generate_lib(&spec, &fp, &out_dir, Target::Quasar).unwrap();
        generate_state(&spec, &fp, &out_dir, Target::Quasar).unwrap();
        generate_errors(&spec, &fp, &out_dir, Target::Quasar).unwrap();
        generate_instructions(&spec, &fp, &spec_path, &out_dir, Target::Quasar).unwrap();
        generate_guards(&spec, &fp, &out_dir, Target::Quasar).unwrap();

        let lib = std::fs::read_to_string(out_dir.join("src/lib.rs")).unwrap();
        // Quasar's `#[program]` mod uses `Ctx<X>` — Anchor uses
        // `Context<X>`. Pin the difference so a target-flip in
        // `FrameworkSurface::context_type` fails the test.
        assert!(
            lib.contains("Ctx<Initialize>"),
            "Quasar uses Ctx, not Context"
        );
        // Quasar lib.rs needs `use instructions::*;` because the Accounts
        // struct lives in `instructions/<name>.rs`. Anchor doesn't need
        // it; Workstream F made this conditional.
        assert!(lib.contains("use instructions::*;"));
        // Quasar emits `#![cfg_attr(... no_std)]` at the crate root so
        // the on-chain build has no_std but the host build keeps std.
        assert!(lib.contains("#![cfg_attr"));
        // Day-2 sidecar: Quasar's `no_alloc` / `panic_handler` macros
        // emit `cfg(target_os = "solana")` / `feature = "alloc"`
        // references that aren't declared, same shape as Anchor's
        // anchor-debug noise. The cfg-allow is now target-agnostic so
        // both scaffolds compile warning-clean.
        assert!(lib.contains("#![allow(unexpected_cfgs)]"));

        let init = std::fs::read_to_string(out_dir.join("src/instructions/initialize.rs")).unwrap();
        // Quasar handler files import quasar_spl, not anchor_spl.
        // Workstream B's `token_imports(has_token, has_mint)` filters
        // to actually-used names — escrow has tokens but no mint, so
        // emit `quasar_spl::Token` only.
        assert!(init.contains("use quasar_spl::Token;"));
        assert!(!init.contains("Mint"));
        // Quasar handlers define the Accounts struct locally, not from
        // crate root, so they need `use crate::state::*;`.
        assert!(init.contains("use crate::state::*;"));

        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();
        // Quasar uses `ProgramError::from(EscrowError::*)` for error
        // exprs — Anchor uses `EscrowError::*.into()`. Workstream B's
        // `error_expr` centralizes the difference.
        assert!(guards.contains("ProgramError::from(EscrowError::"));
        assert!(!guards.contains("EscrowError::Unauthorized.into()"));
        // Quasar's account-key expression: `(*ctx.X.to_account_view().address())`.
        // Anchor's is `ctx.X.key()`. Pin the difference.
        assert!(guards.contains(".to_account_view().address()"));
        assert!(!guards.contains("ctx.escrow.key()"));
    }

    /// Records nested inside `#[account]` Anchor structs need
    /// `AnchorSerialize` + `AnchorDeserialize` derives or the outer
    /// struct fails its trait bound (see Workstream 9 Borsh fix on
    /// percolator). Lock the derive emission for record types with
    /// mixed-Borshable field types — Pubkey, integers, signed ints —
    /// so a future regression in `generate_state` fails fast at the
    /// unit level. Day-2 sidecar test.
    #[test]
    fn anchor_records_with_mixed_field_types_get_borsh_derives() {
        let src = r#"spec MixedRecord

type Holding = {
  owner       : Pubkey,
  capital     : U128,
  pnl         : I128,
  active      : U8,
  duration    : U16,
}

type State
  | Uninitialized
  | Active of {
      authority : Pubkey,
      holdings  : Holding,
    }
  | Closed

type Error
  | Unauthorized

handler initialize : State.Uninitialized -> State.Active {
  auth authority
  accounts {
    authority : signer, writable
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        generate_state(&spec, &fp, &out_dir, Target::Anchor).unwrap();

        let state = std::fs::read_to_string(out_dir.join("src/state.rs")).unwrap();
        assert!(
            state.contains("#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]\npub struct Holding"),
            "Holding record should carry AnchorSerialize/AnchorDeserialize derives so the outer #[account] struct's recursive Borsh bound is satisfied; got:\n{state}"
        );
        // Field types should be the native Anchor mappings, not the
        // standalone harness aliases (Address, etc.).
        assert!(state.contains("pub owner: Pubkey,"));
        assert!(state.contains("pub capital: u128,"));
        assert!(state.contains("pub pnl: i128,"));
        assert!(state.contains("pub active: u8,"));
        assert!(state.contains("pub duration: u16,"));
    }

    /// Quasar nests records inside `#[repr(C)]` zero-copy structs whose
    /// serialization comes from layout, not from Borsh. Confirm we
    /// don't accidentally drop AnchorSerialize/AnchorDeserialize on
    /// the Quasar path (where it would pull in unwanted deps).
    #[test]
    fn quasar_records_skip_anchor_borsh_derives() {
        let src = r#"spec QuasarRecord

type Holding = {
  active  : U8,
  capital : U128,
}

type State
  | Active of {
      holdings : Holding,
    }

type Error
  | Unauthorized

handler initialize : State.Active -> State.Active {
  auth authority
  accounts {
    authority : signer, writable
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        generate_state(&spec, &fp, &out_dir, Target::Quasar).unwrap();

        let state = std::fs::read_to_string(out_dir.join("src/state.rs")).unwrap();
        assert!(state.contains("#[derive(Clone, Copy)]\npub struct Holding"));
        assert!(!state.contains("AnchorSerialize"));
        assert!(!state.contains("AnchorDeserialize"));
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
            .expect("must emit a generic CPI shape for non-SPL Anchor programs");

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

    /// v2.24 #11 — `let X = call Foo.handler(...)` lowers to a Rust
    /// let-binding capturing the callee's return value via Solana's
    /// `get_return_data` syscall, when the interface declares a
    /// return type (`-> U64` etc.).
    #[test]
    fn cpi_emits_let_binding_when_interface_declares_return_type() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "22222222222222222222222222222222"
  handler absorb_loss (loss : U64) -> U64 {
    accounts { vault : writable }
  }
}

type State | Active of { total_burned : U64 }
type Error | MathOverflow | E

handler liquidate (loss : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    vault         : writable
    pool_program  : program
  }
  let burned = call Pool.absorb_loss(vault = vault, loss = loss)
}
"#,
        )
        .unwrap();
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "liquidate")
            .unwrap();
        let call = handler.calls.first().expect("call site");
        assert_eq!(
            call.result_binding.as_deref(),
            Some("burned"),
            "result_binding should land in ParsedCall"
        );
        let rendered = try_emit_anchor_cpi(call, handler, &spec)
            .expect("must emit a generic CPI with let-binding capture");
        // Block opens as a `let burned = { … }` expression.
        assert!(
            rendered.starts_with("        let burned = {\n"),
            "expected let-binding open; got prefix:\n{}",
            &rendered[..200.min(rendered.len())]
        );
        // The CPI invoke happens inside.
        assert!(rendered.contains("invoke(&ix"));
        // get_return_data captures the callee's return.
        assert!(rendered.contains("get_return_data"));
        // The return type maps to u64; deserialize is typed.
        assert!(
            rendered.contains("<u64 as AnchorDeserialize>"),
            "expected typed deserialize for U64 return; got:\n{rendered}"
        );
        // Block closes with `};` (let-binding terminator).
        assert!(
            rendered.ends_with("        };\n"),
            "expected let-binding close; got suffix:\n{}",
            &rendered[rendered.len().saturating_sub(200)..]
        );
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
        let rendered = mechanize_effect(effect, None, state_acct, handler, &spec, Target::Anchor)
            .expect("mechanized");
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

    // ----- v2.24 §S1a/b/c: per-site override + pragma + underflow default -----

    fn mechanize_first_effect(src: &str, handler_name: &str) -> String {
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == handler_name)
            .expect("handler not found");
        let state_acct = find_state_account(handler).expect("state account");
        let effect = handler.effects.first().expect("at least one effect");
        let on_error = handler.effect_on_error.first().and_then(|o| o.as_deref());
        mechanize_effect(effect, on_error, state_acct, handler, &spec, Target::Anchor)
            .expect("mechanized")
    }

    #[test]
    fn per_site_else_overrides_default_variant_for_checked_add() {
        let rendered = mechanize_first_effect(
            r#"spec Mint
program_id "11111111111111111111111111111111"
type State | Active of { pool : U64 }
type Error | MathOverflow | MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { pool += n else MintOverflow }
}
"#,
            "deposit",
        );
        assert!(
            rendered.contains("MintError::MintOverflow"),
            "v2.24 §S1a: `else MintOverflow` should lower to MintError::MintOverflow; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("MintError::MathOverflow"),
            "should not use the default; got:\n{rendered}"
        );
    }

    #[test]
    fn pragma_overrides_default_variant_when_no_per_site_override() {
        let rendered = mechanize_first_effect(
            r#"spec Mint
program_id "11111111111111111111111111111111"
type State | Active of { pool : U64 }
type Error | MathOverflow | MintOverflow

pragma checked_overflow_error = MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { pool += n }
}
"#,
            "deposit",
        );
        assert!(
            rendered.contains("MintError::MintOverflow"),
            "v2.24 §S1b: pragma checked_overflow_error should override the default; got:\n{rendered}"
        );
    }

    #[test]
    fn checked_sub_defaults_to_math_underflow_when_declared() {
        let rendered = mechanize_first_effect(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MathUnderflow

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { balance -= n }
}
"#,
            "withdraw",
        );
        assert!(
            rendered.contains("PoolError::MathUnderflow"),
            "v2.24 §S1c: -= should default to MathUnderflow when declared; got:\n{rendered}"
        );
    }

    #[test]
    fn checked_sub_falls_back_to_math_overflow_for_legacy_specs() {
        // S1c back-compat: only MathOverflow declared, no MathUnderflow.
        // `-=` keeps raising MathOverflow (pre-v2.24 behavior) so existing
        // specs continue to build without spec edits.
        let rendered = mechanize_first_effect(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  accounts { state : writable }
  effect { balance -= n }
}
"#,
            "withdraw",
        );
        assert!(
            rendered.contains("PoolError::MathOverflow"),
            "v2.24 §S1c back-compat: legacy spec falls back to MathOverflow; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("MathUnderflow"),
            "back-compat path should not reference MathUnderflow; got:\n{rendered}"
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

    // ── S2.3: Cargo.toml section + dep preservation ───────────────────────

    #[test]
    fn parse_toml_sections_splits_correctly() {
        let toml = r#"# preamble

[package]
name = "foo"

[dependencies]
anchor-lang = "0.30"

[dev-dependencies]
proptest = "1"
"#;
        let parsed = parse_toml_sections(toml);
        assert!(parsed.preamble.contains("preamble"));
        assert_eq!(parsed.sections.len(), 3);
        assert_eq!(parsed.sections[0].0, "package");
        assert!(parsed.sections[0].1.contains("name = \"foo\""));
        assert_eq!(parsed.sections[1].0, "dependencies");
        assert_eq!(parsed.sections[2].0, "dev-dependencies");
    }

    #[test]
    fn merge_cargo_toml_preserves_user_sections() {
        let existing = r#"# generated by qedgen older spec-hash

[package]
name = "user-renamed"
version = "0.2.0"
edition = "2021"

[dependencies]
anchor-lang = "0.30"
anyhow = "1"

[dev-dependencies]
proptest = "1"

[profile.release]
opt-level = 3
"#;
        let fresh = r#"# ---- GENERATED BY QEDGEN ---- spec-hash:abc123

[package]
name = "buggy"
version = "0.1.0"
edition = "2021"

[dependencies]
anchor-lang = "0.32.1"
qedgen-macros = { git = "https://example.com" }

[workspace]
"#;
        let merged = merge_cargo_toml(existing, fresh);
        // Preamble comes from fresh (qedgen marker).
        assert!(merged.contains("GENERATED BY QEDGEN"));
        // qedgen-owned `[package]` is fully replaced — user's renamed
        // `name` is overwritten back to the spec's program name. (PRD
        // trade-off: `[package]` is qedgen-owned; users wanting a
        // different crate name should change the spec's `program_name`.)
        assert!(merged.contains("name = \"buggy\""));
        // qedgen-managed deps are upserted.
        assert!(merged.contains("anchor-lang = \"0.32.1\""));
        assert!(merged.contains("qedgen-macros"));
        // User-added `anyhow` dep is preserved.
        assert!(merged.contains("anyhow = \"1\""), "got:\n{merged}");
        // User-added sections are preserved verbatim.
        assert!(merged.contains("[dev-dependencies]"));
        assert!(merged.contains("proptest = \"1\""));
        assert!(merged.contains("[profile.release]"));
        assert!(merged.contains("opt-level = 3"));
    }

    #[test]
    fn merge_cargo_toml_handles_greenfield_existing() {
        // Existing file has no qedgen sections — merge should still
        // produce a working file (qedgen sections appended).
        let existing = r#"[dev-dependencies]
proptest = "1"
"#;
        let fresh = r#"# ---- GENERATED BY QEDGEN ----

[package]
name = "foo"

[dependencies]
anchor-lang = "0.32"

[workspace]
"#;
        let merged = merge_cargo_toml(existing, fresh);
        assert!(merged.contains("[dev-dependencies]"));
        assert!(merged.contains("[package]"));
        assert!(merged.contains("[dependencies]"));
        assert!(merged.contains("[workspace]"));
    }
}
