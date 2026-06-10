//! Shared Rust-codegen helper library for `codegen_mir`. Was `codegen.rs`
//! until v2.32 deleted the legacy `generate()` orchestration and renamed
//! the remainder. Holds the per-target `FrameworkSurface`, `generate_guards`,
//! the Pinocchio scaffold emitters, the SPL CPI dispatch (`try_emit_cpi` /
//! `emit_spl_*`), and helpers (`map_type`, `to_pascal_case`, `mechanize_effect`,
//! …). These read `ParsedSpec` directly — they emit account-constraint /
//! guard-predicate / framework-scaffold surface, not effect-body `Stmt` IR,
//! so they're correctly `ParsedSpec`-based.

use anyhow::Result;
use std::path::Path;

use crate::check::{ParsedHandler, ParsedSpec};
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
            Target::Pinocchio => FrameworkSurface {
                target,
                // Same host-std / on-chain-no_std split as Quasar: host
                // builds (cargo test / cargo kani) keep std; the BPF build
                // is no_std. pinocchio's `entrypoint!` macro supplies the
                // on-chain panic handler + allocator. (slice 6, §12)
                crate_attrs:
                    "#![allow(unexpected_cfgs)]\n#![cfg_attr(any(target_os = \"solana\", target_arch = \"bpf\"), no_std)]\n\n",
                prelude_import:
                    "use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};\n",
                // Pinocchio has no `Context`/`Ctx` wrapper and no
                // `#[program]` mod — it uses a free `process_instruction`
                // entrypoint that byte-dispatches to per-handler
                // `process_<name>` fns. These two fields are unused for
                // Pinocchio; generators gate their use on the target.
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

    /// Pinocchio account-field type: every `#[derive(Accounts)]`-equivalent
    /// field is a raw `&'a AccountInfo` (interior-mutable; no typed wrapper).
    /// Typing happens via `zeropod` decode inside `.handler()` (slice 6, §12).
    /// Shared by all the `*_type` helpers below.
    fn pinocchio_account_type(&self) -> String {
        format!("&{} AccountInfo", self.accounts_lifetime)
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
    fn needs_bumps_import(&self, handler: &ParsedHandler) -> bool {
        matches!(self.target, Target::Anchor) && handler.has_bumps()
    }

    fn signer_type(&self, mutable: bool) -> String {
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

    fn program_type(&self, name: &str, account_type: Option<&str>, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        // Token-program detection is shared between targets: a `program`
        // account named `token_program` (the convention) or carrying the
        // `type token` annotation (explicit) needs `Program<Token>` so the
        // generated handler can call `.transfer()` / `.mint_to()` etc.
        // Anything else stays `Program<System>`.
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

    fn token_account_type(&self, mutable: bool) -> String {
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

    fn mint_account_type(&self, mutable: bool) -> String {
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

    fn state_account_type(&self, state_name: &str, mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            // Raw &AccountInfo; state decoded from the data bytes via
            // zeropod inside .handler() (slice 6 step 3/4).
            return self.pinocchio_account_type();
        }
        if self.is_quasar() {
            format!("&{} {}Account<{}>", lt, mut_prefix(mutable), state_name)
        } else {
            format!("Account<{}, {}>", lt, state_name)
        }
    }

    /// v2.29 Slice G — imported account type via the local mirror at
    /// `crate::imported::<ns>::<source_type>`. Anchor target only;
    /// Quasar imported-namespace support is reserved for v2.30.
    fn imported_account_type(&self, ns: &str, source_type: &str, _mutable: bool) -> String {
        let lt = self.accounts_lifetime;
        if self.is_pinocchio() {
            return self.pinocchio_account_type();
        }
        format!("Account<{}, crate::imported::{}::{}>", lt, ns, source_type)
    }

    fn unchecked_account_type(&self, mutable: bool) -> String {
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

    fn error_expr(&self, enum_name: &str, variant: &str) -> String {
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
    /// bare `requires` clauses (no `else <Error>`). Pre-v2.14 emitted
    /// `debug_assert!` (silent no-op in release); v2.14+ emits a real
    /// runtime check that returns this error. Each surface needs the
    /// type-correct form for its `Result<(), _>` return shape.
    fn generic_error_expr(&self) -> &'static str {
        match self.target {
            Target::Anchor => "anchor_lang::error::Error::from(ProgramError::Custom(0xFF))",
            Target::Quasar | Target::Pinocchio => "ProgramError::Custom(0xFF)",
        }
    }

    fn guard_accounts_import(&self) -> &'static str {
        match self.target {
            Target::Anchor => "use crate::*;\n\n",
            // Pinocchio keeps the per-handler accounts struct in
            // `instructions/<name>.rs` like Quasar (slice 6, §12).
            Target::Quasar | Target::Pinocchio => "use crate::instructions::*;\n\n",
        }
    }

    fn account_key_expr(&self, account_name: &str) -> String {
        match self.target {
            Target::Anchor => format!("ctx.{}.key()", account_name),
            Target::Quasar => format!("(*ctx.{}.to_account_view().address())", account_name),
            // pinocchio's AccountInfo::key() returns &Pubkey ([u8; 32]).
            Target::Pinocchio => format!("ctx.{}.key()", account_name),
        }
    }

    fn token_owner_expr(&self, token_account_name: &str) -> String {
        match self.target {
            Target::Anchor => format!("ctx.{}.owner", token_account_name),
            Target::Quasar => format!("(*ctx.{}.owner())", token_account_name),
            // Pinocchio reads the SPL token-account owner from the account
            // DATA (not AccountInfo::owner, which is the owning program).
            // The zeropod-decode form lands with guard codegen (slice 6
            // step 4); not reached until then (init bails until step 2).
            Target::Pinocchio => {
                unreachable!("pinocchio token-owner read lands with guard codegen — slice 6 step 4")
            }
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
    } else if let (Some(ns), Some(ty)) = (&acct.imported_namespace, &acct.account_type) {
        // v2.29 Slice G — imported account type. Routes through the
        // local mirror at `src/imported/<ns>.rs` so the wrapper's
        // field layout matches the foreign program's on-chain
        // representation. `is_writable` flips between `Account<'info,
        // T>` (read-only) and the same — Anchor doesn't have a
        // separate read-only Account type; the macro's writability
        // is driven by the `#[account(mut)]` attribute, not the
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

pub(crate) fn map_type_for_target(
    dsl_type: &str,
    spec: &ParsedSpec,
    target: Target,
) -> Result<String> {
    match target {
        Target::Anchor => map_type_anchor(dsl_type, spec),
        Target::Quasar => map_type_quasar(dsl_type, spec),
        // Instruction params decode from raw bytes into plain Rust scalars
        // (u64, etc.) — the same standalone mapping. State-field pod types
        // (PodU64, …) are a separate generate_state concern (slice 6 step 3).
        Target::Pinocchio => map_type_standalone(dsl_type, spec),
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
pub(crate) fn marker(label: &str, fp: &SpecFingerprint, file_key: &str) -> String {
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

/// Self-contained Pinocchio `src/lib.rs` emitter — a shared helper called
/// by `codegen_mir::emit_lib` for the Pinocchio target (slice 6, §12b).
/// Pinocchio is MIR-only: the legacy `generate_lib` pipeline does NOT route
/// Pinocchio. This helper survives the v3.0 legacy-pipeline deletion.
///
/// Emits the no_std crate root + module decls + `declare_id!` +
/// `entrypoint!` + the byte-dispatch `process_instruction`. Idempotent: a
/// pre-existing `src/lib.rs` is left untouched (user-owned).
pub(crate) fn emit_pinocchio_program_lib(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(Target::Pinocchio);
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
    if !spec.events.is_empty() {
        out.push_str("pub mod events;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("pub mod errors;\n");
    }
    out.push_str("pub mod state;\n");
    out.push_str("pub mod guards;\n");
    out.push_str("#[cfg(kani)]\n");
    out.push_str("extern crate kani;\n");
    out.push_str("#[cfg(kani)]\n");
    out.push_str("mod kani_impl;\n");
    if guards_use_math_helpers(spec) {
        out.push_str("pub mod math;\n");
    }
    if !spec.ref_impls.is_empty() {
        out.push_str("pub mod ref_impls;\n");
    }
    if spec
        .imported_namespaces
        .values()
        .any(|ns| !ns.account_types.is_empty())
    {
        out.push_str("pub mod imported;\n");
    }
    out.push('\n');
    emit_pinocchio_lib_tail(&mut out, spec, program_id);
    out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(src_dir.join("lib.rs"), &out)?;
    Ok(())
}

/// Emit the Pinocchio `lib.rs` tail: program ID, the `entrypoint!` macro,
/// and a `process_instruction` dispatcher that reads the leading
/// discriminant byte and routes to each handler's `process_<name>`
/// wrapper (declared in `instructions/<name>.rs`, slice 6 step 4).
/// Replaces the Anchor/Quasar `#[program]` mod entirely (§12b).
///
/// `entrypoint!` expands to `program_entrypoint!` + `default_allocator!` +
/// `default_panic_handler!`; all three are internally `target_os =
/// "solana"`-gated (host builds link std's allocator/panic handler), so
/// the invocation is emitted unconditionally.
fn emit_pinocchio_lib_tail(out: &mut String, spec: &ParsedSpec, program_id: &str) {
    out.push_str(&format!(
        "pinocchio_pubkey::declare_id!(\"{}\");\n\n",
        program_id
    ));
    // `entrypoint!`'s single-arg arm recursively calls `entrypoint!`
    // *unqualified*, so it must be imported (a `pinocchio::entrypoint!`
    // path call fails to resolve the inner recursion).
    out.push_str("use pinocchio::entrypoint;\n");
    out.push_str("entrypoint!(process_instruction);\n\n");
    out.push_str("/// Instruction dispatch — the leading byte of `instruction_data`\n");
    out.push_str("/// selects the handler (discriminant = declaration order).\n");
    out.push_str("pub fn process_instruction(\n");
    out.push_str("    _program_id: &pinocchio::pubkey::Pubkey,\n");
    out.push_str("    accounts: &[AccountInfo],\n");
    out.push_str("    instruction_data: &[u8],\n");
    out.push_str(") -> ProgramResult {\n");
    out.push_str("    let (discriminant, data) = instruction_data\n");
    out.push_str("        .split_first()\n");
    out.push_str("        .ok_or(ProgramError::InvalidInstructionData)?;\n");
    out.push_str("    match *discriminant {\n");
    for (i, handler) in spec.handlers.iter().enumerate() {
        out.push_str(&format!(
            "        {} => instructions::{}::process_{}(accounts, data),\n",
            i, handler.name, handler.name
        ));
    }
    out.push_str("        _ => Err(ProgramError::InvalidInstructionData),\n");
    out.push_str("    }\n");
    out.push_str("}\n");
}

/// Emit `src/state.rs` for the Pinocchio target (slice 6 step 3, §12a).
///
/// zeropod zero-copy: each persisted struct is the *schema* — declared
/// with plain Rust field types (`u64`, `[u8; 32]`, nested records) — and
/// `#[derive(ZeroPod)]` generates the alignment-1 `<Struct>Zc` companion
/// that handlers mutate in place via `from_bytes_mut`. Lifecycle / sum-type
/// State lowers to a `u8` discriminant field + a `#[repr(u8)]` enum of
/// named constants (the same `status: u8` + `enum` shape the Anchor/Quasar
/// path uses, so the delegated guard codegen stays consistent). Sum-type
/// variant payloads are flattened into one superset struct; the tag byte
/// selects the live variant.
pub(crate) fn emit_pinocchio_state(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    out: &mut String,
) -> Result<()> {
    out.push_str(&marker("DO NOT EDIT", fp, "src/state.rs"));
    out.push_str("use zeropod::ZeroPod;\n\n");

    // Record types referenced by state fields → ZeroPod structs (emitted
    // ahead of the account structs that nest them).
    for record in &spec.records {
        out.push_str("#[derive(ZeroPod)]\n");
        out.push_str(&format!("pub struct {} {{\n", record.name));
        for (fname, ftype) in &record.fields {
            out.push_str(&format!(
                "    pub {}: {},\n",
                fname,
                map_type_standalone(ftype, spec)?
            ));
        }
        out.push_str("}\n\n");
    }

    if spec.account_types.len() > 1 {
        // Multi-account: one ZeroPod struct per account type.
        for acct in &spec.account_types {
            let struct_name = format!("{}Account", acct.name);
            let enum_name = format!("{}Status", acct.name);
            emit_pinocchio_state_struct(
                out,
                &struct_name,
                &acct.fields,
                acct.pda_ref.is_some(),
                &acct.lifecycle,
                &enum_name,
                spec,
            )?;
        }
    } else if spec
        .account_types
        .first()
        .map(|a| a.variants.len() > 1)
        .unwrap_or(false)
    {
        // Sum-type State → discriminant tag byte + flat superset struct.
        let acct = &spec.account_types[0];
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));
        let tag_name = format!("{}Tag", state_name);
        out.push_str(&format!(
            "/// Discriminant tag for `{}`. Variant payloads are stored\n\
             /// flattened in the struct below; the `tag` byte selects the\n\
             /// live variant.\n",
            state_name
        ));
        out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n#[repr(u8)]\n");
        out.push_str(&format!("pub enum {} {{\n", tag_name));
        for (i, v) in acct.variants.iter().enumerate() {
            out.push_str(&format!("    {} = {},\n", v.name, i));
        }
        out.push_str("}\n\n");

        out.push_str("#[derive(ZeroPod)]\n");
        out.push_str(&format!("pub struct {} {{\n", state_name));
        out.push_str("    pub tag: u8,\n");
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for v in &acct.variants {
            for (fname, ftype) in &v.fields {
                if seen.insert(fname.clone()) {
                    out.push_str(&format!(
                        "    pub {}: {},\n",
                        fname,
                        map_type_standalone(ftype, spec)?
                    ));
                }
            }
        }
        if !spec.pdas.is_empty() && !seen.contains("bump") {
            out.push_str("    pub bump: u8,\n");
        }
        out.push_str("}\n\n");
    } else {
        // Single flat record state.
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));
        emit_pinocchio_state_struct(
            out,
            &state_name,
            &spec.state_fields,
            !spec.pdas.is_empty(),
            &spec.lifecycle_states,
            "Status",
            spec,
        )?;
    }

    out.push_str("// ---- END GENERATED ----\n");
    Ok(())
}

/// Emit one Pinocchio `#[derive(ZeroPod)]` state struct (+ its optional
/// `#[repr(u8)]` lifecycle enum). `has_pda` appends a `bump: u8`;
/// `lifecycle` (when non-empty) emits the named-constant enum + a
/// `status: u8` field. Shared by the single-account + multi-account
/// branches of `emit_pinocchio_state`.
fn emit_pinocchio_state_struct(
    out: &mut String,
    struct_name: &str,
    fields: &[(String, String)],
    has_pda: bool,
    lifecycle: &[String],
    enum_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    if !lifecycle.is_empty() {
        out.push_str(&format!("/// {} lifecycle states.\n", enum_name));
        out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n#[repr(u8)]\n");
        out.push_str(&format!("pub enum {} {{\n", enum_name));
        for (i, s) in lifecycle.iter().enumerate() {
            out.push_str(&format!("    {} = {},\n", s, i));
        }
        out.push_str("}\n\n");
    }
    out.push_str("#[derive(ZeroPod)]\n");
    out.push_str(&format!("pub struct {} {{\n", struct_name));
    for (fname, ftype) in fields {
        out.push_str(&format!(
            "    pub {}: {},\n",
            fname,
            map_type_standalone(ftype, spec)?
        ));
    }
    if has_pda && !fields.iter().any(|(n, _)| n == "bump") {
        out.push_str("    pub bump: u8,\n");
    }
    if !lifecycle.is_empty() && !fields.iter().any(|(n, _)| n == "status") {
        out.push_str("    pub status: u8,\n");
    }
    out.push_str("}\n\n");
    Ok(())
}

/// DSL integer type → (Rust primitive, byte width) for little-endian
/// (de)serialization of instruction-data params. None for non-integer
/// types (the Pinocchio wrapper emits a `todo!()` for those).
fn numeric_param_width(dsl_type: &str) -> Option<(&'static str, usize)> {
    match dsl_type.trim() {
        "U8" => Some(("u8", 1)),
        "I8" => Some(("i8", 1)),
        "U16" => Some(("u16", 2)),
        "I16" => Some(("i16", 2)),
        "U32" => Some(("u32", 4)),
        "I32" => Some(("i32", 4)),
        "U64" => Some(("u64", 8)),
        "I64" => Some(("i64", 8)),
        "U128" => Some(("u128", 16)),
        "I128" => Some(("i128", 16)),
        _ => None,
    }
}

/// Emit one Pinocchio `instructions/<name>.rs` scaffold (slice 6 step 4a).
/// Shared helper; `codegen_mir::emit_instructions` calls it for the
/// Pinocchio target. USER-OWNED (emitted only when the file is missing).
///
/// Shape: a `struct <Pascal><'a>` of `&AccountInfo` fields + an
/// `impl { fn handler(&mut self, …) -> ProgramResult }` (calls
/// `crate::guards::<name>`, then applies effects) + a free
/// `process_<name>(accounts, data)` wrapper the entrypoint dispatcher
/// calls — it binds the account slice positionally, parses params from
/// `instruction_data` (LE, offset-tracked), builds the struct, and calls
/// `.handler()`.
///
/// step 4a scope: the account-binding + param-parse + dispatch shape. The
/// `.handler()` effect body (zeropod state read/write + SPL CPI) + the
/// Pinocchio `guards.rs` path are step 4b — the body is a `todo!()`
/// breadcrumb for now.
pub(crate) fn render_pinocchio_handler_scaffold(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Result<String> {
    let pascal = to_pascal_case(&handler.name);
    let mut out = String::new();

    out.push_str("// User-owned. Regenerating the spec does NOT overwrite this file.\n");
    out.push_str("// Guard checks live in the sibling `crate::guards` module and ARE\n");
    out.push_str("// regenerated on every `qedgen codegen`.\n\n");
    out.push_str(
        "use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};\n",
    );
    out.push_str("use zeropod::ZeroPodFixed;\n");
    out.push_str("use crate::state::*;\n");
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    out.push_str("use crate::guards;\n");
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    out.push('\n');

    // Accounts struct — every field is a raw &AccountInfo (zeropod decode
    // happens inside .handler()).
    out.push_str(&format!("pub struct {}<'a> {{\n", pascal));
    for acct in &handler.accounts {
        out.push_str(&format!("    pub {}: &'a AccountInfo,\n", acct.name));
    }
    out.push_str("}\n\n");

    let params_sig: String = handler
        .takes_params
        .iter()
        .map(|(n, t)| map_type_standalone(t, spec).map(|ty| format!(", {}: {}", n, ty)))
        .collect::<Result<Vec<_>>>()?
        .join("");
    let param_names: Vec<&str> = handler
        .takes_params
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();

    out.push_str(&format!("impl {}<'_> {{\n", pascal));
    out.push_str(&format!(
        "    pub fn handler(&mut self{}) -> ProgramResult {{\n",
        params_sig
    ));
    if param_names.is_empty() {
        out.push_str(&format!("        guards::{}(self)?;\n", handler.name));
    } else {
        out.push_str(&format!(
            "        guards::{}(self, {})?;\n",
            handler.name,
            param_names.join(", ")
        ));
    }
    emit_pinocchio_effect_body(&mut out, handler, spec);
    out.push_str("        Ok(())\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "/// Entrypoint wrapper — binds the account slice + parses params, then\n\
         /// calls `{}::handler`. Invoked by `process_instruction` in lib.rs.\n",
        pascal
    ));
    out.push_str(&format!(
        "pub fn process_{}(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {{\n",
        handler.name
    ));
    if handler.accounts.is_empty() {
        out.push_str("    let _ = accounts;\n");
    } else {
        let names: Vec<&str> = handler.accounts.iter().map(|a| a.name.as_str()).collect();
        out.push_str(&format!(
            "    let [{}, ..] = accounts else {{\n",
            names.join(", ")
        ));
        out.push_str("        return Err(ProgramError::NotEnoughAccountKeys);\n");
        out.push_str("    };\n");
    }
    if handler.takes_params.is_empty() {
        out.push_str("    let _ = instruction_data;\n");
    } else {
        let mut offset = 0usize;
        for (pname, ptype) in &handler.takes_params {
            match numeric_param_width(ptype) {
                Some((rust_ty, width)) => {
                    out.push_str(&format!(
                        "    let {} = {}::from_le_bytes(\n        instruction_data\n            .get({}..{})\n            .ok_or(ProgramError::InvalidInstructionData)?\n            .try_into()\n            .map_err(|_| ProgramError::InvalidInstructionData)?,\n    );\n",
                        pname,
                        rust_ty,
                        offset,
                        offset + width
                    ));
                    offset += width;
                }
                None => {
                    out.push_str(&format!(
                        "    // TODO: parse non-numeric param `{}` (spec type {}) from instruction_data\n",
                        pname, ptype
                    ));
                    out.push_str(&format!(
                        "    let {}: {} = todo!(\"parse {} from instruction_data\");\n",
                        pname,
                        map_type_standalone(ptype, spec)?,
                        pname
                    ));
                }
            }
        }
    }
    let field_init: String = handler
        .accounts
        .iter()
        .map(|a| a.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "    let mut ctx = {} {{ {} }};\n",
        pascal, field_init
    ));
    out.push_str(&format!("    ctx.handler({})\n", param_names.join(", ")));
    out.push_str("}\n");

    Ok(out)
}

/// Emit the `.handler()` effect body for a Pinocchio handler (slice 6 4b).
/// Mechanical SCALAR state effects (`field := v`, `field += / -= / +=! /
/// +=? n`) lower to a one-time mutable zeropod decode of the state account
/// plus per-field `.get()`-based arithmetic (universally safe — native int
/// op then `.into()` back to the Pod field). RHS expressions route through
/// `bind_pinocchio_expr` so param + scalar-state reads resolve.
///
/// SPL Token CPIs from explicit `call Interface.handler(...)` sites lower
/// via `try_emit_cpi(_, Target::Pinocchio)` (slice 6 step 5b) — the handler
/// struct's `&'a AccountInfo` fields match `pinocchio_token`'s CPI struct
/// fields directly.
///
/// Deferred (emit a documented breadcrumb): non-scalar effects (array /
/// nested / variant-payload writes), events, `transfers { … }` sugar
/// (agent-fill on every target), and generic non-SPL CPI (slice 7).
fn emit_pinocchio_effect_body(out: &mut String, handler: &ParsedHandler, spec: &ParsedSpec) {
    let prog = to_pascal_case(&spec.program_name);
    let err = format!("{}Error", prog);
    let has = |n: &str| spec.error_codes.iter().any(|c| c == n);
    let overflow = if has("MathOverflow") {
        format!("ProgramError::from({}::MathOverflow)", err)
    } else {
        "ProgramError::ArithmeticOverflow".to_string()
    };
    let underflow = if has("MathUnderflow") {
        format!("ProgramError::from({}::MathUnderflow)", err)
    } else if has("MathOverflow") {
        format!("ProgramError::from({}::MathOverflow)", err)
    } else {
        "ProgramError::ArithmeticOverflow".to_string()
    };

    // Classify scalar state effects (lhs is a simple field after stripping
    // any `Variant.` prefix — no `.` / `[` remaining).
    let scalar: Vec<(String, &str, &str)> = handler
        .effects
        .iter()
        .filter_map(|(lhs, op, rhs)| {
            let field = strip_variant_prefix(lhs, spec);
            if field.contains('.') || field.contains('[') {
                None
            } else {
                Some((field, op.as_str(), rhs.as_str()))
            }
        })
        .collect();

    // Single-account decode only (multi-account state typing is deferred).
    let single_state = spec.account_types.len() <= 1;
    if !scalar.is_empty() && single_state {
        if let Some(acct) = resolve_handler_state_account(handler, spec) {
            out.push_str(&format!(
                "        let __state = {}Account::from_bytes_mut(unsafe {{ self.{}.borrow_mut_data_unchecked() }})\n            .map_err(|_| ProgramError::InvalidAccountData)?;\n",
                prog, acct.name
            ));
            for (field, op, rhs) in &scalar {
                // Effect body lives in the handler method, so account refs
                // bind through `self` (not the guard fn's `ctx`).
                let r = bind_pinocchio_expr(rhs, handler, "__state", "self", spec);
                let line = match *op {
                    // Pubkey fields are raw `[u8; 32]` (no Pod wrapper), so
                    // assign the deref'd value directly — `.into()` is for
                    // the native-int → Pod-scalar conversion only.
                    "set" if state_field_is_pubkey(spec, field) => {
                        format!("        __state.{field} = {r};\n")
                    }
                    "set" => format!("        __state.{field} = ({r}).into();\n"),
                    "add" => format!(
                        "        __state.{field} = __state.{field}.get().checked_add({r}).ok_or({overflow})?.into();\n"
                    ),
                    "sub" => format!(
                        "        __state.{field} = __state.{field}.get().checked_sub({r}).ok_or({underflow})?.into();\n"
                    ),
                    "add_sat" => format!(
                        "        __state.{field} = __state.{field}.get().saturating_add({r}).into();\n"
                    ),
                    "sub_sat" => format!(
                        "        __state.{field} = __state.{field}.get().saturating_sub({r}).into();\n"
                    ),
                    "add_wrap" => format!(
                        "        __state.{field} = __state.{field}.get().wrapping_add({r}).into();\n"
                    ),
                    "sub_wrap" => format!(
                        "        __state.{field} = __state.{field}.get().wrapping_sub({r}).into();\n"
                    ),
                    other => format!("        // TODO: effect op `{other}` on `{field}` not mechanized\n"),
                };
                out.push_str(&line);
            }
        } else {
            out.push_str(
                "        // TODO(slice 6 4b): could not resolve the state account for effects\n",
            );
        }
    }

    // Deferred surfaces — documented breadcrumbs (not silently dropped).
    let complex_effects =
        handler.effects.len() > scalar.len() || (!scalar.is_empty() && !single_state);
    if complex_effects {
        out.push_str("        // TODO(slice 6 4b-cont): non-scalar effects (array / nested /\n        // variant-payload writes) + multi-account state.\n");
    }
    // SPL Token CPIs from explicit `call Interface.handler(...)` sites
    // (slice 6 step 5b). The handler struct's `&'a AccountInfo` fields
    // are exactly what `pinocchio_token::instructions::*` takes, so the
    // emitter's `self.<acct>` resolves directly. Non-SPL (generic invoke)
    // call sites return `None` (slice 7) and fall through to a breadcrumb.
    let mut any_unmechanized_call = false;
    for c in &handler.calls {
        match try_emit_cpi(c, handler, spec, Target::Pinocchio) {
            Some(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{}\n",
                    c.target_interface, c.target_handler
                ));
                out.push_str(&rendered);
            }
            None => any_unmechanized_call = true,
        }
    }
    if any_unmechanized_call {
        out.push_str("        // TODO(slice 7): generic (non-SPL) CPI call sites are not yet\n        // mechanized for Pinocchio (raw invoke_signed + Borsh).\n");
    }

    // `transfers { … }` stays agent-fill on every target — codegen owns
    // deterministic translation, the agent owns the CPI/authority business
    // logic. Events likewise carry no payload binding in the spec.
    if !handler.emits.is_empty() || !handler.transfers.is_empty() {
        out.push_str("        // TODO(slice 6 4b-cont): events / transfers.\n");
    }
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
/// Public re-export of `is_multi_variant_adt_state` for callers in
/// other modules (check.rs's seeds-suppression logic).
pub fn is_multi_variant_adt_state_pub(spec: &ParsedSpec) -> bool {
    is_multi_variant_adt_state(spec)
}

fn is_multi_variant_adt_state(spec: &ParsedSpec) -> bool {
    spec.state_repr_is_adt()
        && spec.account_types.len() == 1
        && spec
            .account_types
            .first()
            .map(|a| a.variants.len() > 1)
            .unwrap_or(false)
}

/// v2.29 Slice H — generate `src/imported/<ns>.rs` per imported
/// namespace plus a `src/imported/mod.rs` re-exporter. The handler's
/// accounts block can then name `<ns>::<Type>` as the account type
/// without depending on the foreign crate at compile time: the
/// mirror struct carries the same field layout, so an `Account<'info,
/// <ns>::<Type>>` deserializes the foreign bytes exactly the way the
/// foreign program would.
///
/// The mirror is fully regenerated on every `qedgen codegen`; users
/// should not hand-edit anything under `src/imported/`. Drift
/// detection (`qedgen check --regen-drift`) treats these files as
/// generated, same shape as `state.rs` / `errors.rs` / `events.rs`.
///
/// Single-variant account types lower to plain `pub struct`s.
/// Multi-variant ADTs (the `WrongState`-gated wrapper + inner enum
/// pattern) reuse the wrapper-struct + inner-enum shape so an
/// `Account<'info, Wrapper>` matches the foreign program's
/// account discriminator. The same accessor-method emission from
/// `generate_state` runs here so consumer-side reads via
/// `imported_acct.inner.<field>()` work identically to local
/// multi-variant state.
///
/// Plain record types referenced by the imported account types are
/// emitted in the same file so the mirror is self-contained — no
/// cross-file `use crate::imported::<other>::*;` chasing.
#[allow(dead_code)]
pub(crate) fn generate_imported_mirror(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    // v2.30 (mir / unified imports): every imported source registers
    // in `imported_namespaces`, but only those with non-empty
    // `account_types` produce a mirror file. Tier-0 interface stubs
    // (SPL Token / System Program / Metaplex) carry no `type`
    // declarations — skip them.
    if !spec
        .imported_namespaces
        .values()
        .any(|ns| !ns.account_types.is_empty())
    {
        return Ok(());
    }
    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    let imported_dir = src_dir.join("imported");
    std::fs::create_dir_all(&imported_dir)?;

    // Per-namespace file emission. The BTreeMap iteration order is
    // sorted by local name so the generated mod.rs re-exports stay
    // deterministic across runs.
    for (local_name, ns) in &spec.imported_namespaces {
        // Skip Tier-0 stubs whose `account_types` is empty —
        // no Rust mirror to emit.
        if ns.account_types.is_empty() {
            continue;
        }
        let mut out = String::new();
        let file_rel = format!("src/imported/{}.rs", local_name);
        out.push_str(&marker("DO NOT EDIT", fp, &file_rel));
        out.push_str(&format!(
            "//! v2.29 Slice H mirror of `{0}`'s account types\n\
             //! (sourced from dep `{1}`).\n\
             //!\n\
             //! Hand-editing is unsafe: every `qedgen codegen` regenerates\n\
             //! this file from the imported `.qedspec`'s `type` declarations.\n\
             //! To change a field, change the imported spec and re-resolve.\n\n",
            local_name, ns.dep_key,
        ));
        out.push_str(surface.prelude_import);
        out.push('\n');

        // Plain record types first (account types may reference them).
        // Anchor-only: Quasar's zero-copy/Pod path needs a different
        // derive set + `#[repr(C)]` discipline that v2.29 doesn't
        // wire for imports — flagged as future work alongside the
        // larger Quasar import story.
        for record in &ns.records {
            out.push_str("#[repr(C)]\n");
            let derives = match target {
                Target::Anchor => "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq)]\n",
                _ => "#[derive(Clone, Copy)]\n",
            };
            out.push_str(derives);
            out.push_str(&format!("pub struct {} {{\n", record.name));
            for (fname, ftype) in &record.fields {
                let rust_ty = map_type_for_target(ftype, spec, target)?;
                out.push_str(&format!("    pub {}: {},\n", fname, rust_ty));
            }
            out.push_str("}\n\n");
        }

        // Account types. Two shapes mirror `generate_state`:
        //   1. Single-variant (no `WrongState`): plain `#[account]`
        //      struct with fields at the top level.
        //   2. Multi-variant ADT (wrapper + inner enum): only emit
        //      when there's more than one variant. Inner enum gets
        //      the accessor-impl block from `generate_state`'s
        //      Slice B work so consumer reads through
        //      `imported_acct.inner.<field>()` resolve.
        for (idx, acct) in ns.account_types.iter().enumerate() {
            let is_multi_variant = acct.variants.len() > 1;
            let account_attr = if surface.explicit_account_discriminator {
                format!("#[account(discriminator = {})]\n", idx + 1)
            } else {
                "#[account]\n".to_string()
            };
            if !is_multi_variant {
                // Flat struct path. Lifecycle status field follows the
                // same convention as `generate_state` — without it the
                // consumer's `requires` clauses that reference
                // `<acct>.status` wouldn't compile against the mirror.
                out.push_str(&format!("{}pub struct {} {{\n", account_attr, acct.name));
                for (fname, ftype) in &acct.fields {
                    let rust_ty = map_type_for_target(ftype, spec, target)?;
                    out.push_str(&format!("    pub {}: {},\n", fname, rust_ty));
                }
                if !acct.lifecycle.is_empty() && !acct.fields.iter().any(|(n, _)| n == "status") {
                    out.push_str("    pub status: u8,\n");
                }
                out.push_str("}\n\n");

                if !acct.lifecycle.is_empty() {
                    out.push_str(&format!(
                        "/// {} lifecycle states (mirrored from `{}`).\n",
                        acct.name, ns.dep_key
                    ));
                    out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
                    out.push_str("#[repr(u8)]\n");
                    out.push_str(&format!("pub enum {}Status {{\n", acct.name));
                    for (i, state) in acct.lifecycle.iter().enumerate() {
                        out.push_str(&format!("    {} = {},\n", state, i));
                    }
                    out.push_str("}\n\n");
                }
                continue;
            }

            // Multi-variant ADT — same wrapper + inner enum shape as
            // generate_state's `is_multi_variant_adt_state` path so
            // the mirror deserializes the foreign account exactly
            // the way the foreign program does.
            let inner_name = format!("{}Inner", acct.name);
            out.push_str(&format!("{}pub struct {} {{\n", account_attr, acct.name));
            out.push_str(&format!("    pub inner: {},\n", inner_name));
            out.push_str("}\n\n");

            out.push_str(&format!(
                "/// Variant-payload state for `{0}` (mirrored from `{1}`).\n",
                acct.name, ns.dep_key
            ));
            out.push_str(
                "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Debug, PartialEq)]\n",
            );
            out.push_str(&format!("pub enum {} {{\n", inner_name));
            for variant in &acct.variants {
                if variant.fields.is_empty() {
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
            out.push_str("}\n\n");

            // Reuse the Slice B accessor pattern (fields that appear
            // across variants with a consistent type get an accessor
            // method on the inner enum). Consumer-side reads like
            // `imported_acct.inner.balance()` then resolve through
            // the same shape used for locally-declared multi-variant
            // ADT state.
            let mut field_index: std::collections::BTreeMap<String, Vec<(String, String)>> =
                std::collections::BTreeMap::new();
            for variant in &acct.variants {
                for (fname, ftype) in &variant.fields {
                    field_index
                        .entry(fname.clone())
                        .or_default()
                        .push((variant.name.clone(), ftype.clone()));
                }
            }
            if !field_index.is_empty() {
                out.push_str(&format!("impl {} {{\n", inner_name));
                for (fname, occurrences) in &field_index {
                    let first_ty = &occurrences[0].1;
                    if occurrences.iter().any(|(_, t)| t != first_ty) {
                        continue;
                    }
                    let rust_ty = map_type_for_target(first_ty, spec, target)?;
                    out.push_str(&format!(
                        "    /// v2.29 Slice H accessor for `{0}`. Panics on variants\n\
                         /// that don't carry the field — the per-handler lifecycle\n\
                         /// check at the top of each `crate::guards::*` fn prevents\n\
                         /// the panic arm from being reached at runtime.\n",
                        fname
                    ));
                    out.push_str(&format!(
                        "    pub fn {}(&self) -> &{} {{\n        match self {{\n",
                        fname, rust_ty
                    ));
                    for (variant_name, _) in occurrences {
                        out.push_str(&format!(
                            "            Self::{} {{ {}, .. }} => {},\n",
                            variant_name, fname, fname
                        ));
                    }
                    if occurrences.len() < acct.variants.len() {
                        out.push_str(&format!(
                            "            _ => panic!(\"{}::{}() called on a variant without `{}`\"),\n",
                            inner_name, fname, fname
                        ));
                    }
                    out.push_str("        }\n    }\n");
                }
                out.push_str("}\n\n");
            }
        }

        out.push_str("// ---- END GENERATED ----\n");
        std::fs::write(imported_dir.join(format!("{}.rs", local_name)), &out)?;
    }

    // `mod.rs` re-exports each namespace as `pub mod <ns>;`. Slice G
    // (the parser+codegen for `Ident.Ident` type refs) consumes this
    // to resolve `<ns>::<Type>` references in the consumer's spec
    // against the local mirror. `#![allow(non_snake_case)]` covers
    // the common case where the imported spec's bound name is
    // PascalCase (e.g. `import Config from "config_program"` keeps
    // the module name `Config`); rustc otherwise warns on every
    // module-decl line.
    let mut mod_out = String::new();
    mod_out.push_str(&marker("DO NOT EDIT", fp, "src/imported/mod.rs"));
    mod_out.push_str("//! v2.29 Slice H — re-exports for imported namespace mirrors.\n\n");
    mod_out.push_str("#![allow(non_snake_case)]\n\n");
    // v2.30: only re-export namespaces that actually produced a
    // mirror file (account_types non-empty). Tier-0 stubs registered
    // in `imported_namespaces` are silent on the codegen side.
    for (local_name, ns) in &spec.imported_namespaces {
        if ns.account_types.is_empty() {
            continue;
        }
        mod_out.push_str(&format!("pub mod {};\n", local_name));
    }
    mod_out.push_str("\n// ---- END GENERATED ----\n");
    std::fs::write(imported_dir.join("mod.rs"), mod_out)?;

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

/// v2.29 Slice B — word-boundary substring search. Returns `true`
/// when `needle` appears in `haystack` as a complete identifier
/// (i.e. neither neighbouring byte is an identifier character). Used
/// to detect whether a `requires` rust expression actually references
/// an `abstract` binder by name without false-matching on
/// `<binder>_x` / `prefix<binder>` substrings.
fn contains_word_boundary(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.len() > hb.len() {
        return false;
    }
    let mut i = 0;
    while i + nb.len() <= hb.len() {
        if &hb[i..i + nb.len()] == nb {
            let prev_ok = i == 0 || !is_ident_char(hb[i - 1]);
            let next_idx = i + nb.len();
            let next_ok = next_idx >= hb.len() || !is_ident_char(hb[next_idx]);
            if prev_ok && next_ok {
                return true;
            }
        }
        i += 1;
    }
    false
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

/// Canonical System Program ID (all-ones base58 → the all-zero pubkey).
/// Calls into the bundled `System` interface (`import System from "system"`)
/// match this constant and get the `pinocchio_system::instructions::*` CPI
/// shape on the Pinocchio target. The System Program is a native built-in,
/// so unlike SPL Token there is no deployed binary to hash — the upstream
/// pin's `binary_hash` stays all-zero (see `data/interfaces/system.qedspec`).
const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";

/// Try to emit a real Anchor CPI invocation for one `call Interface.handler(...)`
/// site. Returns `None` when the interface isn't recognized OR when the
/// target framework isn't Anchor (caller falls back to a comment +
/// `todo!()` so the user / an LLM fills the body).
///
/// All five SPL Token handlers — `transfer`, `mint_to`, `burn`,
/// `initialize_account`, `close_account` — get an `anchor_spl::token::*`
/// shape; non-SPL-Token interfaces ship a generic
/// `solana_program::program::invoke` shape. The canonical SPL handlers
/// cover the bulk of CPI traffic in deployed programs, which is what
/// keeps `todo!()` out of the typical escrow / lending / vault shape.
///
/// Per-target dispatch: emits the right CPI shape per
/// `(target, is_spl_token)`.
///
/// - Anchor    + SPL Token → `anchor_spl::token::*` builder shape
/// - Anchor    + generic   → `solana_program::program::invoke` shape
/// - Quasar    + SPL Token → `quasar_spl::TokenCpi` method chain
/// - Quasar    + generic   → not implemented (spike scope; §8 slice 3)
/// - Pinocchio + SPL Token → `pinocchio_token::instructions::*` struct + invoke
/// - Pinocchio + generic   → not implemented (spike scope; §8 slice 7)
///
/// Returns `None` for any branch that isn't implemented yet — the
/// caller falls back to a structured comment + `todo!()` so the agent
/// fills the body.
fn try_emit_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
) -> Option<String> {
    let iface = spec
        .interfaces
        .iter()
        .find(|i| i.name == call.target_interface)?;
    let is_spl_token = iface.program_id.as_deref() == Some(SPL_TOKEN_PROGRAM_ID);
    let is_system = iface.program_id.as_deref() == Some(SYSTEM_PROGRAM_ID);

    match (target, is_spl_token, is_system) {
        (Target::Anchor, true, _) => emit_spl_token_cpi_anchor(call, handler, spec),
        (Target::Anchor, false, true) => emit_system_cpi_anchor(call, handler, iface, spec),
        (Target::Anchor, false, false) => emit_generic_cpi_anchor(call, handler, iface, spec),
        (Target::Quasar, true, _) => emit_spl_token_cpi_quasar(call, handler, spec),
        // Quasar generic / System CPI — follow-on slice (uses `BufCpiCall`).
        (Target::Quasar, false, _) => None,
        (Target::Pinocchio, true, _) => emit_spl_token_cpi_pinocchio(call, handler, spec),
        (Target::Pinocchio, false, true) => emit_system_cpi_pinocchio(call, handler, spec),
        // Pinocchio generic (non-SPL, non-System) CPI — follow-on slice
        // (raw invoke_signed).
        (Target::Pinocchio, false, false) => None,
    }
}

/// SPL Token dispatcher. Routes to the right `anchor_spl::token` helper
/// per the called handler's name. Returns None on unrecognized handlers
/// (the caller falls back to comment + `todo!()`).
fn emit_spl_token_cpi_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let token_program_acct = find_token_program_account(handler)?;
    let prog_name = &token_program_acct.name;

    match call.target_handler.as_str() {
        "transfer" => emit_spl_anchor(
            call,
            handler,
            spec,
            prog_name,
            "Transfer",
            &[("from", "from"), ("to", "to"), ("authority", "authority")],
            Some("amount"),
            "transfer",
        ),
        "mint_to" => emit_spl_anchor(
            call,
            handler,
            spec,
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
        "burn" => emit_spl_anchor(
            call,
            handler,
            spec,
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
        "initialize_account" => emit_spl_anchor(
            call,
            handler,
            spec,
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
        "close_account" => emit_spl_anchor(
            call,
            handler,
            spec,
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
pub(crate) fn to_snake_case(s: &str) -> String {
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
fn emit_generic_cpi_anchor(
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
        let resolved = resolve_call_arg_for_amount(&arg.rust_expr, handler, spec);
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
#[allow(clippy::too_many_arguments)]
fn emit_spl_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
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
            Some(resolve_call_arg_for_amount(&arg.rust_expr, handler, spec))
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

/// Anchor System Program dispatcher. `transfer` gets the idiomatic
/// `anchor_lang::system_program::transfer(CpiContext::new(...), lamports)`
/// shape (mirroring the SPL Token path); `create_account` / `assign`
/// fall back to the generic `solana_program::program::invoke` builder —
/// PDA creation needs `invoke_signed` + seed assembly, which is CPI
/// authority business logic the agent fills, not deterministic codegen
/// (the same boundary that keeps `transfers {}` agent-filled).
fn emit_system_cpi_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    iface: &crate::check::ParsedInterface,
    spec: &ParsedSpec,
) -> Option<String> {
    if call.target_handler == "transfer" {
        // The CpiContext needs the System program account info; resolve it
        // by the `system_program` convention. If it isn't reachable, fall
        // through to the generic invoke shape (which builds the program id
        // inline) rather than fail.
        if let Some(sp) = find_program_account_for_interface(handler, &call.target_interface) {
            let name = sp.name.clone();
            return emit_system_anchor(
                call,
                handler,
                spec,
                &name,
                "Transfer",
                &[("from", "from"), ("to", "to")],
                Some("amount"),
                "transfer",
            );
        }
    }
    emit_generic_cpi_anchor(call, handler, iface, spec)
}

/// Emit an idiomatic `anchor_lang::system_program::<fn>(CpiContext::new(
/// cpi_program, <Struct> { … }), <scalar>)?` CPI. Sibling to
/// `emit_spl_anchor` — identical context-builder shape, but the `use`
/// path is `anchor_lang::system_program` and the program account is the
/// System program (`system_program`), not `anchor_spl::token` /
/// `token_program`.
#[allow(clippy::too_many_arguments)]
fn emit_system_anchor(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    system_program: &str,
    accounts_struct: &str,
    field_to_arg: &[(&str, &str)],
    scalar_arg: Option<&str>,
    fn_name: &str,
) -> Option<String> {
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

    let scalar_rhs = match scalar_arg {
        Some(name) => {
            let arg = call.args.iter().find(|a| a.name == name)?;
            Some(resolve_call_arg_for_amount(&arg.rust_expr, handler, spec))
        }
        None => None,
    };

    let mut out = String::new();
    out.push_str("        {\n");
    out.push_str(&format!(
        "            use anchor_lang::system_program::{{self, {}}};\n",
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
        system_program
    ));
    let invocation = match scalar_rhs {
        Some(rhs) => format!(
            "            system_program::{}(CpiContext::new(cpi_program, cpi_accounts), {})?;\n",
            fn_name, rhs
        ),
        None => format!(
            "            system_program::{}(CpiContext::new(cpi_program, cpi_accounts))?;\n",
            fn_name
        ),
    };
    out.push_str(&invocation);
    out.push_str("        }\n");
    Some(out)
}

/// Quasar SPL Token dispatcher. Routes to the right `quasar_spl::TokenCpi`
/// method per the called handler's name. Returns None on unrecognized
/// handlers so the caller falls back to comment + `todo!()`.
///
/// Coverage (slice 2): `transfer`, `mint_to`, `burn`, `close_account`.
/// `initialize_account` stays `None` — `quasar-spl` exposes only
/// `initialize_account3`, whose `owner: &Address` positional is a raw key
/// (not an account view) and which omits the rent sysvar, so it doesn't
/// fit the uniform `emit_spl_quasar` helper (see the match arm).
fn emit_spl_token_cpi_quasar(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let token_program_acct = find_token_program_account(handler)?;
    let prog_name = &token_program_acct.name;

    match call.target_handler.as_str() {
        "transfer" => emit_spl_quasar(
            call,
            handler,
            spec,
            prog_name,
            "transfer",
            // Quasar's TokenCpi::transfer signature is
            // (from, to, authority, amount). Spec arg names must match
            // the canonical SPL Token interface declared in the spec.
            &["from", "to", "authority"],
            Some("amount"),
        ),
        "mint_to" => emit_spl_quasar(
            call,
            handler,
            spec,
            prog_name,
            "mint_to",
            // TokenCpi::mint_to(mint, to, authority, amount). The spec's
            // canonical SPL interface names the authority slot
            // `mint_authority`; it resolves positionally into the trait's
            // `authority` parameter.
            &["mint", "to", "mint_authority"],
            Some("amount"),
        ),
        "burn" => emit_spl_quasar(
            call,
            handler,
            spec,
            prog_name,
            "burn",
            // TokenCpi::burn(from, mint, authority, amount).
            &["from", "mint", "authority"],
            Some("amount"),
        ),
        "close_account" => emit_spl_quasar(
            call,
            handler,
            spec,
            prog_name,
            "close_account",
            // TokenCpi::close_account(account, destination, authority) —
            // no scalar.
            &["account", "destination", "authority"],
            None,
        ),
        // `initialize_account` has no uniform Quasar shape: `quasar-spl`
        // exposes only `initialize_account3`, whose third positional is
        // `owner: &Address` (a raw key, not an account view) and which
        // omits the rent sysvar. That divergent signature doesn't fit the
        // positional-account-view `emit_spl_quasar` helper, so it falls
        // through to the caller's `todo!()`.
        _ => None,
    }
}

/// Emit one Quasar `quasar_spl::*` CPI as a single-line method chain on
/// the token-program account: `self.<prog>.<method>(&self.<a>, …,
/// scalar).invoke()?;`. The shape comes from `quasar-spl-0.0.0`'s
/// `TokenCpi` trait (sibling shape to the Anchor builder).
///
/// `account_args_in_order` carries the call-site arg names in the order
/// the Quasar trait method expects (e.g. `["from", "to", "authority"]`
/// for `transfer`). The function looks each one up in `call.args` and
/// resolves it to `&self.<rust_expr>`. Unrecognized arg names short-
/// circuit to `None`.
fn emit_spl_quasar(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    token_program: &str,
    method_name: &str,
    account_args_in_order: &[&str],
    scalar_arg: Option<&str>,
) -> Option<String> {
    let mut args: Vec<String> = Vec::with_capacity(account_args_in_order.len() + 1);
    for call_arg in account_args_in_order {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        args.push(format!("&self.{}", arg.rust_expr));
    }
    if let Some(name) = scalar_arg {
        let arg = call.args.iter().find(|a| a.name == name)?;
        args.push(resolve_call_arg_for_amount(&arg.rust_expr, handler, spec));
    }
    Some(format!(
        "        self.{}.{}({}).invoke()?;\n",
        token_program,
        method_name,
        args.join(", ")
    ))
}

/// Pinocchio SPL Token dispatcher. Routes to the right
/// `pinocchio_token::instructions::*` struct per the called handler's
/// name. Returns None on unrecognized handlers so the caller falls
/// back to comment + `todo!()`.
///
/// Coverage (slice 2b): all five canonical SPL handlers — `transfer`,
/// `mint_to`, `burn`, `initialize_account`, `close_account`. Field-name
/// divergences from canonical SPL naming (`MintTo.account` for the
/// recipient, `MintTo.mint_authority`, `Burn.account` for the source,
/// `InitializeAccount.rent_sysvar`) are handled per-arm in the
/// `(pinocchio_field, spec_arg)` map.
///
/// **Note on dead-code-ness**: `--target pinocchio` codegen currently
/// skips the Rust scaffold (`main.rs:3132`), so this emitter is never
/// reached from the CLI today. It still gets unit-tested directly via
/// `try_emit_cpi(_, _, _, Target::Pinocchio)`. When Pinocchio scaffold
/// (§8 slice 6) lands, this emitter is already wired.
fn emit_spl_token_cpi_pinocchio(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    match call.target_handler.as_str() {
        "transfer" => emit_spl_pinocchio(
            call,
            handler,
            spec,
            "Transfer",
            // (pinocchio_struct_field, spec_arg_name)
            &[("from", "from"), ("to", "to"), ("authority", "authority")],
            Some("amount"),
        ),
        "mint_to" => emit_spl_pinocchio(
            call,
            handler,
            spec,
            "MintTo",
            // pinocchio-token's MintTo names the recipient `account`
            // (canonical SPL `to`) and the signer `mint_authority`.
            &[
                ("mint", "mint"),
                ("account", "to"),
                ("mint_authority", "mint_authority"),
            ],
            Some("amount"),
        ),
        "burn" => emit_spl_pinocchio(
            call,
            handler,
            spec,
            "Burn",
            // Burn names the source slot `account` (canonical SPL `from`).
            &[
                ("account", "from"),
                ("mint", "mint"),
                ("authority", "authority"),
            ],
            Some("amount"),
        ),
        "initialize_account" => emit_spl_pinocchio(
            call,
            handler,
            spec,
            "InitializeAccount",
            // InitializeAccount names the rent sysvar `rent_sysvar`
            // (canonical SPL `rent`). No scalar.
            &[
                ("account", "account"),
                ("mint", "mint"),
                ("owner", "owner"),
                ("rent_sysvar", "rent"),
            ],
            None,
        ),
        "close_account" => emit_spl_pinocchio(
            call,
            handler,
            spec,
            "CloseAccount",
            &[
                ("account", "account"),
                ("destination", "destination"),
                ("authority", "authority"),
            ],
            None,
        ),
        _ => None,
    }
}

/// Emit one `pinocchio_token::instructions::<Struct> { … }.invoke()?;`
/// CPI. Field assignments use
/// the pinocchio-token struct field names (passed in
/// `field_to_arg.0`), resolved against the call site's argument list
/// (`field_to_arg.1`).
///
/// Note: pinocchio_token's struct field names diverge from the
/// canonical SPL naming for some handlers (`MintTo.account` vs SPL's
/// `to`; `MintTo.mint_authority` vs SPL's `authority`). The
/// `field_to_arg` map handles the translation at the codegen boundary,
/// mirroring how `emit_spl_anchor` handles the Anchor variants.
fn emit_spl_pinocchio(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    struct_name: &str,
    field_to_arg: &[(&str, &str)],
    scalar_arg: Option<&str>,
) -> Option<String> {
    let max_field = field_to_arg
        .iter()
        .map(|(f, _)| f.len())
        .chain(scalar_arg.map(|s| s.len()))
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str("        pinocchio_token::instructions::");
    out.push_str(struct_name);
    out.push_str(" {\n");
    for (struct_field, call_arg) in field_to_arg {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let pad = " ".repeat(max_field - struct_field.len());
        // The Pinocchio handler struct stores each account as
        // `&'a AccountInfo`, and `pinocchio_token`'s CPI structs take
        // `&'a AccountInfo` fields — so `self.<acct>` is already the
        // right type. (A leading `&` would yield `&&AccountInfo`.)
        out.push_str(&format!(
            "            {}:{} self.{},\n",
            struct_field, pad, arg.rust_expr
        ));
    }
    if let Some(name) = scalar_arg {
        let arg = call.args.iter().find(|a| a.name == name)?;
        let rhs = resolve_call_arg_for_amount(&arg.rust_expr, handler, spec);
        let pad = " ".repeat(max_field - name.len());
        out.push_str(&format!("            {}:{} {},\n", name, pad, rhs));
    }
    out.push_str("        }.invoke()?;\n");
    Some(out)
}

/// Dispatch a `call System.<handler>(...)` site to its
/// `pinocchio_system::instructions::*` shape. Only `transfer` is
/// mechanized this slice — it covers the lamport-movement path that the
/// generic CPI breadcrumb previously left as a `todo!()` on Pinocchio
/// (the System Program's lamport `Transfer` is the code class behind the
/// over-funded-PDA / lamport-transfer-DoS findings the bench corpus
/// surfaced). `create_account` and `assign` take a `&Pubkey` `owner`,
/// whose resolution from a spec `Pubkey` arg (e.g. the program's own ID)
/// is a follow-on; they return `None` so the caller keeps the existing
/// breadcrumb rather than emit code that may not type-check.
fn emit_system_cpi_pinocchio(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    match call.target_handler.as_str() {
        "transfer" => emit_system_pinocchio(
            call,
            handler,
            spec,
            "Transfer",
            // (pinocchio_system struct field, spec call-site arg)
            &[("from", "from"), ("to", "to")],
            &[("lamports", "amount")],
        ),
        // create_account / assign: see doc comment — `owner: &Pubkey`
        // resolution is a follow-on.
        _ => None,
    }
}

/// Emit one `pinocchio_system::instructions::<Struct> { … }.invoke()?;`
/// CPI for a `call System.<handler>(...)` site. Mirrors
/// `emit_spl_pinocchio` but targets the System Program crate and lets the
/// struct field name differ from the call-site arg name (System's
/// `Transfer.lamports` binds the spec's `amount` arg).
///
/// `account_fields` map `pinocchio_system` struct fields holding
/// `&AccountInfo` to call args (emitted as `self.<arg>`); `scalar_fields`
/// map `u64` struct fields to call args (resolved through
/// `resolve_call_arg_for_amount`, the same path the SPL scalar uses).
fn emit_system_pinocchio(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    struct_name: &str,
    account_fields: &[(&str, &str)],
    scalar_fields: &[(&str, &str)],
) -> Option<String> {
    let max_field = account_fields
        .iter()
        .chain(scalar_fields.iter())
        .map(|(f, _)| f.len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out.push_str("        pinocchio_system::instructions::");
    out.push_str(struct_name);
    out.push_str(" {\n");
    for (struct_field, call_arg) in account_fields {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let pad = " ".repeat(max_field - struct_field.len());
        // `pinocchio_system` CPI structs take `&AccountInfo` fields and the
        // generated Pinocchio handler stores each account as
        // `&'a AccountInfo`, so `self.<acct>` is already the right type
        // (a leading `&` would yield `&&AccountInfo`).
        out.push_str(&format!(
            "            {}:{} self.{},\n",
            struct_field, pad, arg.rust_expr
        ));
    }
    for (struct_field, call_arg) in scalar_fields {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let rhs = resolve_call_arg_for_amount(&arg.rust_expr, handler, spec);
        let pad = " ".repeat(max_field - struct_field.len());
        out.push_str(&format!("            {}:{} {},\n", struct_field, pad, rhs));
    }
    out.push_str("        }.invoke()?;\n");
    Some(out)
}

/// v2.29.2 — resolve the state-bearing account for a handler with a
/// spec-wide canonical fallback. `find_state_account(handler)` returns
/// `None` when multiple writable candidates exist and no PDA /
/// `on_account` disambiguator picks one (real-world specs often declare
/// the state account `readonly` in handlers that only read it, leaving
/// the per-handler resolver stuck among unrelated token / mint
/// accounts). When that happens, fall back to the spec-wide canonical
/// state account name and re-find it in this handler.
fn resolve_handler_state_account<'a>(
    handler: &'a ParsedHandler,
    spec: &ParsedSpec,
) -> Option<&'a crate::check::ParsedHandlerAccount> {
    if let Some(direct) = find_state_account(handler) {
        return Some(direct);
    }
    let canon = find_canonical_state_account_name(spec)?;
    handler.accounts.iter().find(|a| a.name == canon)
}

/// v2.29.2 — pick the most-likely state-bearing account name across the
/// whole spec. Used as a fallback when a handler's accounts block has
/// multiple writable candidates and no PDA/`on_account` disambiguator
/// (real-world specs frequently mark the state account `readonly` in
/// handlers that only read it, leaving the per-handler resolver stuck
/// among unrelated token / mint accounts).
///
/// Heuristic: count, for each non-signer / non-program / non-token /
/// non-mint account name across the whole spec:
///   1. how many handlers list it `writable`, then
///   2. how many handlers list it at all (writable or readonly).
///
/// The pair `(writable, total)` is compared lexicographically with the
/// highest pair winning; ties broken alphabetically (earlier name
/// first) for determinism. Returns `None` when no name accumulates a
/// non-zero pair.
///
/// The total-mentions tiebreaker is what makes the heuristic robust
/// against specs where the canonical state account is `readonly` in
/// most handlers (read-heavy programs): an account that's mentioned
/// in every handler is almost certainly the shared state, even when
/// individual writable counts tie with unrelated per-handler accounts.
fn find_canonical_state_account_name(spec: &ParsedSpec) -> Option<String> {
    let mut writable: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut total: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for h in &spec.handlers {
        for a in &h.accounts {
            if a.is_signer || a.is_program {
                continue;
            }
            if matches!(a.account_type.as_deref(), Some("token") | Some("mint")) {
                continue;
            }
            *total.entry(a.name.clone()).or_insert(0) += 1;
            if a.is_writable {
                *writable.entry(a.name.clone()).or_insert(0) += 1;
            }
        }
    }
    total
        .into_iter()
        .filter(|(name, _)| writable.get(name).copied().unwrap_or(0) > 0)
        .max_by(|(an, at), (bn, bt)| {
            let aw = writable.get(an).copied().unwrap_or(0);
            let bw = writable.get(bn).copied().unwrap_or(0);
            aw.cmp(&bw)
                .then_with(|| at.cmp(bt))
                .then_with(|| bn.cmp(an))
        })
        .map(|(name, _)| name)
}

/// Rewrite `s.<field>` patterns in a pre-rendered Rust expression so it
/// compiles inside the user-owned handler `impl` block (where the state
/// binder is `self`, not `ctx`). Mirrors the Step-2 logic from
/// `generate_guards::bind_state` but with `self.<state_acct>` as the
/// prefix.
///
/// Multi-variant ADT state fields route through the v2.29 Slice B
/// accessor (`(*self.<state>.inner.<field>())`); flat-state fields take
/// the bare `self.<state>.<field>` path. Word-bounded so identifiers
/// like `accounts[i].fee_credits` don't get corrupted.
///
/// v2.29.2 (friction-report follow-up, #12 deep): the existing
/// `resolve_call_arg_for_amount` only handles top-level CPI argument
/// expressions. Handler-body `let X = ref_impl(state.f, ...)` sites
/// emit pre-rendered RHS strings whose `s.<field>` references stayed
/// raw — `rustc` then rejected with `cannot find value 's'`. This
/// helper closes that gap for any in-`impl` emission site.
fn rewrite_state_refs_for_self(expr: &str, handler: &ParsedHandler, spec: &ParsedSpec) -> String {
    // v2.29.2 — when this handler has multiple writable candidates and
    // no PDA or `on_account` disambiguator (real-world shape: state
    // account declared `readonly` here while several token / mint
    // accounts are writable in this handler), `find_state_account
    // (handler)` bails with `None` and the rewrite would no-op leaving
    // `s.<field>` unbound. Fall back to the spec-wide canonical state
    // account: the non-signer / non-program / non-token / non-mint
    // account name that's writable in the most handlers, then look it
    // up by name in THIS handler's accounts.
    let Some(sa) = resolve_handler_state_account(handler, spec) else {
        return expr.to_string();
    };
    let multi_variant = is_multi_variant_adt_state(spec);
    let accessor_fields: std::collections::HashSet<String> = if multi_variant {
        let mut set = std::collections::HashSet::new();
        if let Some(acct) = spec.account_types.first() {
            let mut tys: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            let mut consistent: std::collections::BTreeMap<String, bool> =
                std::collections::BTreeMap::new();
            for variant in &acct.variants {
                for (fname, ftype) in &variant.fields {
                    if let Some(existing) = tys.get(fname) {
                        if existing != ftype {
                            consistent.insert(fname.clone(), false);
                        }
                    } else {
                        tys.insert(fname.clone(), ftype.clone());
                        consistent.insert(fname.clone(), true);
                    }
                }
            }
            for fname in tys.keys() {
                if *consistent.get(fname).unwrap_or(&true) {
                    set.insert(fname.clone());
                }
            }
        }
        set
    } else {
        std::collections::HashSet::new()
    };
    let bare_target = format!("self.{}.", sa.name);
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        if prev_ok && i + 1 < bytes.len() && bytes[i] == b's' && bytes[i + 1] == b'.' {
            let mut j = i + 2;
            while j < bytes.len() && is_ident_char(bytes[j]) {
                j += 1;
            }
            let field = &expr[i + 2..j];
            if !field.is_empty() && accessor_fields.contains(field) {
                out.push_str(&format!("(*self.{}.inner.{}())", sa.name, field));
                i = j;
                continue;
            } else if !field.is_empty() {
                out.push_str(&bare_target);
                i += 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Resolve a numeric / value argument's rust_expr to a form that's in
/// scope inside the handler `impl` block. Bare identifiers that match a
/// state field get the `self.<state_acct>.` prefix; handler params and
/// literals pass through unchanged. Multi-variant ADT state fields
/// route through the v2.29 Slice B accessor (`.inner.<field>()`)
/// since the wrapper struct only exposes `inner`, not the variant
/// payload fields directly.
fn resolve_call_arg_for_amount(
    rust_expr: &str,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> String {
    // v2.29 — handle `s.<field>` shape coming from `expr_to_rust`
    // on `state.<field>` paths (Ctx::Guard lowers `state` → `s`).
    // The CPI emission contexts don't have an `s` binding in scope,
    // so route directly to the runtime `self.<state_acct>.<field>`
    // form — the same shape the `bind_state` closure produces in
    // guards.rs. Without this, `call Token.transfer(amount =
    // state.total_deposits)` lowered to a bare `s.total_deposits`
    // reference and rustc rejected with `cannot find value 's'`.
    if let Some(field) = rust_expr.strip_prefix("s.") {
        if field.chars().all(|c| c.is_alphanumeric() || c == '_') {
            if let Some(sa) = find_state_account(handler) {
                if is_multi_variant_adt_state(spec) {
                    if let Some(acct) = spec.account_types.first() {
                        let is_variant_field = acct
                            .variants
                            .iter()
                            .any(|v| v.fields.iter().any(|(n, _)| n == field));
                        if is_variant_field {
                            return format!("(*self.{}.inner.{}())", sa.name, field);
                        }
                    }
                }
                return format!("self.{}.{}", sa.name, field);
            }
        }
    }
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
        if is_multi_variant_adt_state(spec) {
            // v2.29 Slice B (#12 deep): variant-payload field reads
            // route through the accessor emitted in generate_state.
            // Bail to the bare form when the field isn't a known
            // variant payload — flat-state-like access against the
            // wrapper's own non-inner fields stays valid (e.g.
            // `bump`).
            if let Some(acct) = spec.account_types.first() {
                let is_variant_field = acct
                    .variants
                    .iter()
                    .any(|v| v.fields.iter().any(|(n, _)| n == rust_expr));
                if is_variant_field {
                    return format!("(*self.{}.inner.{}())", sa.name, rust_expr);
                }
            }
        }
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
/// v2.26 Slice 2 — return shape of the multi-variant ADT handler-body
/// emitter. `needs_fill_tail` is true when at least one `modifies`
/// field landed as an agent-fill `todo!()` site; the caller propagates
/// this into `any_unmechanized` so the tail `todo!("fill non-mechanical …")`
/// fires even when every effect line mechanized cleanly.
struct VariantHandlerBody {
    body: String,
    needs_fill_tail: bool,
}

fn emit_variant_state_handler_body(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
    state_acct: &crate::check::ParsedHandlerAccount,
) -> Option<VariantHandlerBody> {
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
        )
        .map(|body| VariantHandlerBody {
            body,
            needs_fill_tail: false,
        });
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

    // v2.26 Slice 2 — modifies-driven agent-fill on the ADT path.
    // Diff `modifies` against the effect-LHS set; any leftover field
    // becomes a `*field = todo!(...)` site inside the match arm,
    // with the relevant `ensures` clauses quoted as comments. The
    // destructure must bind the modifies-only field too, otherwise
    // the `*field = ...` reference doesn't resolve.
    let modifies_only_fields: Vec<String> = handler
        .modifies
        .as_ref()
        .map(|modifies| {
            modifies
                .iter()
                .filter(|f| {
                    !mutated_fields.contains(f.as_str())
                        && post_variant.fields.iter().any(|(n, _)| n == *f)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for f in &modifies_only_fields {
        mutated_fields.insert(f.clone());
    }

    if mutated_fields.is_empty() {
        // Pure read-only handler (no effects on state, no modifies).
        // Emit a skip-style match so the runtime still rejects the
        // wrong variant — the guard layer doesn't catch variant
        // drift on its own.
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
        return Some(VariantHandlerBody {
            body: out,
            needs_fill_tail: false,
        });
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

    // v2.26 Slice 2 — emit modifies-driven agent-fill sites inside
    // the same match arm so the destructured binding is in scope.
    // Mirrors the flat-fields template at the codegen `if
    // !variant_body_emitted && !is_multi_variant_adt_state(spec)` site
    // (indented 12 spaces, `*field` deref instead of `self.X.field`).
    let mut needs_fill_tail = false;
    for field in &modifies_only_fields {
        let mut referencing: Vec<&str> = Vec::new();
        for e in &handler.ensures {
            if e.rust_expr.contains(field.as_str()) {
                referencing.push(e.rust_expr.as_str());
            }
        }
        out.push_str(&format!(
            "                // QED agent-fill site: `{}` is in `modifies` but not in `effect`.\n",
            field
        ));
        if referencing.is_empty() {
            out.push_str(&format!(
                "                //   No `ensures` clause references `{}` — the field is\n",
                field
            ));
            out.push_str(
                "                //   unconstrained. Either add an `ensures` constraint or\n",
            );
            out.push_str(&format!(
                "                //   remove `{}` from `modifies`. (Lint: unconstrained_modifies)\n",
                field
            ));
        } else {
            out.push_str("                //   Implement against the spec's ensures:\n");
            for r in &referencing {
                out.push_str(&format!("                //     ensures {}\n", r));
            }
            out.push_str(
                "                //   The Kani / proptest harness verifies the impl satisfies\n",
            );
            out.push_str(
                "                //   these clauses against the pre-state captured before the call.\n",
            );
        }
        out.push_str(&format!(
            "                *{} = todo!(\"compute {} to satisfy ensures above\");\n",
            field, field
        ));
        needs_fill_tail = true;
    }

    out.push_str("            }\n");
    out.push_str(&format!(
        "            _ => return Err({}::WrongState.into()),\n",
        err_enum
    ));
    out.push_str("        }\n");
    Some(VariantHandlerBody {
        body: out,
        needs_fill_tail,
    })
}

/// v2.24 S5c — render an effect RHS for the cross-variant promotion
/// emitter. The legal RHS shapes are restricted to ones the emitter
/// can lower deterministically:
///
///   - bare param (handler `takes_params`) → bare identifier
///   - bare const (spec `constants`) → bare identifier (Rust resolves)
///   - integer literal → bare integer
///   - `<account>.pubkey` where `<account>` is bound on the handler
///     and is a signer / writable account → `self.<account>.key()`
///   - bare pre-variant field name (v2.29 Slice C, payload→payload):
///     the destructured local shadowing `<field>` is in scope inside
///     the emitter's preamble → bare identifier
///
/// Anything else returns `None`, which bails the whole cross-variant
/// emitter back to the per-effect `todo!()` path so the agent fills
/// the gap manually instead of getting a silent miscompile.
fn resolve_cross_variant_rhs(
    raw: &str,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    pre_fields: &[String],
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
        // v2.29 Slice C — pre-variant field captured by the
        // destructure preamble. The emitter binds `<field>` as a
        // local before the assignment, so a bare reference resolves
        // directly. render_effect strips `state.` for bare paths
        // (chumsky_adapter.rs::render_effect line 1356), so the raw
        // form here is the bare field name.
        if pre_fields.iter().any(|n| n == raw) {
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
    // v2.24.0: three baseline cross-variant promotion patterns are
    // valid (unit→payload init, payload→unit terminate, unit→unit).
    // v2.29 Slice C lifts the last restriction — payload→payload now
    // emits a destructure preamble that captures pre fields as
    // locals, then assembles the post variant referencing those
    // locals where the spec carries `state.<pre_field>` reads.
    let pre_variant = spec
        .account_types
        .first()?
        .variants
        .iter()
        .find(|v| v.name == pre)?;

    let pre_field_names: Vec<String> = pre_variant.fields.iter().map(|(n, _)| n.clone()).collect();

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
        let resolved = resolve_cross_variant_rhs(rhs, handler, spec, &pre_field_names)?;
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

    // v2.29 Slice C — collect the pre-variant fields referenced by
    // any post-variant RHS. Only these need to be bound by the
    // destructure preamble; binding the rest would produce
    // `unused variable` warnings.
    let referenced_pre_fields: Vec<String> = pre_field_names
        .iter()
        .filter(|f| field_rhs.values().any(|rhs| rhs == *f))
        .cloned()
        .collect();

    let is_init_pre = matches!(pre, "Uninitialized" | "Empty");
    let mut out = String::new();

    if pre_variant.fields.is_empty() {
        // Unit pre — emit the bare matches! gate (unchanged from v2.24).
        // Init handlers skip the gate: `#[account(init, …)]` zeroes
        // the account, so there's no prior payload to mismatch on.
        if !is_init_pre {
            out.push_str(&format!(
                "        if !matches!(self.{}.inner, {}::{}) {{ return Err({}::WrongState.into()); }}\n",
                acct_binder, inner_name, pre, err_enum
            ));
        }
    } else if referenced_pre_fields.is_empty() {
        // Payload pre, no fields read by post — variant check only.
        // (Same shape as v2.24 payload-pre + unit-post; covers the
        // rare payload-pre + payload-post where post is fully
        // populated from params / consts / literals.)
        if !is_init_pre {
            out.push_str(&format!(
                "        if !matches!(self.{}.inner, {}::{} {{ .. }}) {{ return Err({}::WrongState.into()); }}\n",
                acct_binder, inner_name, pre, err_enum
            ));
        }
    } else {
        // Payload pre + at least one field is read by post — emit a
        // `match` that doubles as the variant-gate and the field
        // capture step. Bind only referenced fields via the
        // destructure pattern; ignore the rest with `..` so Rust
        // doesn't warn on unused bindings. The inner enum derives
        // Clone (codegen.rs:1053), so calling `.clone()` per field
        // works uniformly for both Copy and non-Copy field types
        // (e.g. `Map[N] Pubkey`).
        let bind_pat = referenced_pre_fields.join(", ");
        let local_pat = if referenced_pre_fields.len() == 1 {
            referenced_pre_fields[0].clone()
        } else {
            format!("({})", bind_pat)
        };
        let tuple_expr = if referenced_pre_fields.len() == 1 {
            format!("{}.clone()", referenced_pre_fields[0])
        } else {
            let parts: Vec<String> = referenced_pre_fields
                .iter()
                .map(|n| format!("{}.clone()", n))
                .collect();
            format!("({})", parts.join(", "))
        };
        out.push_str(&format!(
            "        let {} = match &self.{}.inner {{\n",
            local_pat, acct_binder
        ));
        out.push_str(&format!(
            "            {}::{} {{ {}, .. }} => {},\n",
            inner_name, pre, bind_pat, tuple_expr
        ));
        out.push_str(&format!(
            "            _ => return Err({}::WrongState.into()),\n",
            err_enum
        ));
        out.push_str("        };\n");
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
pub(crate) fn render_handler_accounts_struct(
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
    // v2.29 — drop the `<'info>` lifetime parameter when the handler
    // declares no accounts AND no implicit signer is added below.
    // Anchor's `#[derive(Accounts)]` still validates a unit struct,
    // but rustc rejects the empty `<'info>` since no field references
    // it. Pre-fix the empty-accounts case (e.g. permissionless
    // handlers without account binds) emitted `pub struct
    // Initialize<'info> {}` and tripped E0392.
    let needs_lifetime = !handler.accounts.is_empty() || handler.who.is_some();
    let struct_lifetime: &str = if needs_lifetime { &lifetime_params } else { "" };
    out.push_str(&format!("pub struct {}{} {{\n", pascal, struct_lifetime));

    if !handler.accounts.is_empty() {
        // v2.29.2 — use the canonical-fallback resolver so multi-
        // writable handlers whose state account is declared `readonly`
        // (or otherwise ambiguous) still get the state account typed
        // as `Account<'info, <StateStruct>>` rather than falling
        // through to `AccountInfo<'info>`. Without this, downstream
        // `self.<acct>.<field>` reads in let-bindings / guards / CPI
        // args fail with `no field on type __AccountInfo`.
        let state_acct = resolve_handler_state_account(handler, spec);
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

pub(crate) fn render_handler_scaffold(
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
    // v2.29 — drop `<'info>` from the impl + guard sigs when the
    // Accounts struct itself dropped it (zero accounts AND no
    // implicit signer). Keeps the impl / guard fn signatures
    // consistent with `render_handler_accounts_struct`'s decision.
    let handler_needs_lifetime = !handler.accounts.is_empty() || handler.who.is_some();
    let lifetime_params: String = if handler_needs_lifetime {
        surface.lifetime_params()
    } else {
        String::new()
    };
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
    //
    // v2.29 (#15) — Anchor needs `use crate::state::*;` too whenever
    // any handler param's type names a user-defined record or sum
    // type (so the scaffold body's `fn handler(&mut self, snap:
    // Snapshot, …)` resolves). The pre-v2.29 path skipped the
    // import on Anchor and emitted `pub fn handler(&mut self, snap:
    // Snapshot, …)` with `Snapshot` unresolved.
    let handler_param_uses_user_type = handler.takes_params.iter().any(|(_, ty)| {
        let bare = ty.trim();
        spec.records.iter().any(|r| r.name == bare)
            || spec.sum_types.iter().any(|s| s.name == bare)
            || spec.account_types.iter().any(|a| a.name == bare)
    });
    if render_struct || handler_param_uses_user_type {
        out.push_str("use crate::state::*;\n");
    }
    // v2.29 (#13) — handler body may reference `ref_impl` fns by name
    // (the spec's `let scaled = scale(snap.total, factor)` lowers to
    // a bare `scale(...)` call). Without this import the call doesn't
    // resolve. Spec declares no ref_impls → no import → no unused-
    // import warning.
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
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

    // v2.29 Slice A (#8) — `abstract <name> : <Type>` clauses become
    // user-fillable `todo!()` bindings in the Rust scaffold. The
    // structured prompt strings list the active `requires` clauses
    // so the agent / human knows what the concrete value must
    // satisfy. Emitted BEFORE let_bindings so any spec-level `let`
    // that references the binder resolves.
    for (binder_name, binder_ty_str) in &handler.abstract_binders {
        let ty = map_type_for_target(binder_ty_str, spec, target)?;
        let requires_summary: Vec<String> = handler
            .requires
            .iter()
            .map(|r| r.rust_expr.clone())
            .collect();
        let constraint_hint = if requires_summary.is_empty() {
            String::new()
        } else {
            format!(
                " Constraints from `requires`: {}.",
                requires_summary.join(" && ")
            )
        };
        out.push_str(&format!(
            "        let {}: {} = todo!(\"v2.29 abstract binder `{}` — fill with the concrete library / math value.{}\");\n",
            binder_name, ty, binder_name, constraint_hint
        ));
    }

    // Spec-level `let` bindings (e.g. `let total_fee = amount * 125 / 10000`)
    // must be emitted BEFORE the effect block — effect RHSs reference them.
    // Pre-fix: they were dropped on the Rust side, leaving undefined-variable
    // errors on `cargo build`.
    //
    // v2.29.2 — RHS is rendered with the spec's `s.<field>` shorthand
    // (the in-spec lowering of `state.<field>`), which is unbound in the
    // handler-body context. Rewrite through the same accessor logic the
    // CPI-arg path uses so multi-variant ADT state reads compile.
    for (binding_name, _lean_expr, rust_expr) in &handler.let_bindings {
        let rewritten = rewrite_state_refs_for_self(rust_expr, handler, spec);
        out.push_str(&format!("        let {} = {};\n", binding_name, rewritten));
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
        match try_emit_cpi(c, handler, spec, target) {
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
    let variant_body_emitted = variant_body.is_some();
    if let Some(VariantHandlerBody {
        body,
        needs_fill_tail,
    }) = variant_body
    {
        out.push_str(&body);
        if needs_fill_tail {
            any_unmechanized = true;
        }
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

    // v2.24.x Phase A.2 — `modifies [X, Y]` declared, but X/Y not
    // written in `effect { ... }`: emit a structured agent-fill
    // site for each unwritten field. This is the "Kani checks impl
    // against spec" pattern — the spec author declares the write
    // set + an ensures contract, codegen leaves the math as todo,
    // the agent fills it against the quoted ensures, and the
    // verification harnesses check the impl satisfies the contract.
    //
    // Restricted to the legacy flat-fields path (matches `mechanize_effect`
    // gating above). Multi-variant ADT specs route through
    // `emit_variant_state_handler_body` and need their own treatment.
    if !variant_body_emitted && !is_multi_variant_adt_state(spec) {
        if let (Some(modifies), Some(sa)) = (handler.modifies.as_ref(), state_acct) {
            let mut effect_fields: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for (lhs, _, _) in &handler.effects {
                let stripped = strip_variant_prefix(lhs, spec);
                let bare = strip_array_index_suffix(&stripped);
                effect_fields.insert(bare);
            }
            let acct_name = &sa.name;
            for field in modifies {
                if effect_fields.contains(field) {
                    continue;
                }
                // Find every ensures clause that references this field
                // (textual match — `rust_expr` carries `post.<field>` for
                // post-state refs and `pre.<field>` for `old(...)` refs).
                let mut referencing: Vec<&str> = Vec::new();
                for e in &handler.ensures {
                    if e.rust_expr.contains(field) {
                        referencing.push(e.rust_expr.as_str());
                    }
                }
                out.push_str(&format!(
                    "        // QED agent-fill site: `{}` is in `modifies` but not in `effect`.\n",
                    field
                ));
                if referencing.is_empty() {
                    out.push_str(&format!(
                        "        //   No `ensures` clause references `{}` — the field is\n",
                        field
                    ));
                    out.push_str(
                        "        //   unconstrained. Either add an `ensures` constraint or\n",
                    );
                    out.push_str(&format!(
                        "        //   remove `{}` from `modifies`. (Lint: unconstrained_modifies)\n",
                        field
                    ));
                } else {
                    out.push_str("        //   Implement against the spec's ensures:\n");
                    for r in &referencing {
                        out.push_str(&format!("        //     ensures {}\n", r));
                    }
                    out.push_str(
                        "        //   The Kani / proptest harness verifies the impl satisfies\n",
                    );
                    out.push_str(
                        "        //   these clauses against the pre-state captured before the call.\n",
                    );
                }
                out.push_str(&format!(
                    "        self.{}.{} = todo!(\"compute {} to satisfy ensures above\");\n",
                    acct_name, field, field
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
        match try_emit_cpi(c, handler, spec, target) {
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

/// Generate src/guards.rs — one function per handler containing all the
/// spec-declared guard checks. This file is always regenerated; any edit
/// is clobbered on the next `qedgen codegen` (by design).
pub(crate) fn generate_guards(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(target);
    let lifetime_params = surface.lifetime_params();
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Pinocchio: dedicated guard emission (raw &AccountInfo ctx + zeropod
    // state decode), shared with the codegen_mir delegation. (slice 6 4b)
    if matches!(target, Target::Pinocchio) {
        return emit_pinocchio_guards(spec, fp, output_dir);
    }

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
    // v2.26 Slice 3c — ref_impls are callable from `requires` bodies.
    // The spec's `expr_to_rust` lowering emits the call by name; the
    // ref_impl fn lives in `crate::ref_impls`, so import it
    // unconditionally whenever the spec declares any. Under
    // `#![allow(unused_imports)]` a never-used import is harmless.
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    // Pick up the per-handler `Accounts` structs. Anchor places them
    // at crate root (lib.rs); Quasar places them in
    // `instructions/<name>.rs` and re-exports via `instructions::*`.
    out.push_str(surface.guard_accounts_import());

    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        let any_mut = handler.accounts.iter().any(|a| a.is_writable);
        let self_ref = if any_mut { "&mut " } else { "&" };
        // v2.29 — match the handler-scaffold + Accounts-struct
        // lifetime decision so the guard fn's ctx ref doesn't
        // reference an unused `<'info>` on a unit Accounts struct.
        let handler_needs_lifetime = !handler.accounts.is_empty() || handler.who.is_some();
        let lp: &str = if handler_needs_lifetime {
            &lifetime_params
        } else {
            ""
        };
        let mut params = vec![format!("ctx: {}{}{}", self_ref, pascal, lp)];
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
            lp,
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
            // v2.29 — Anchor extension: when the seed references a
            // variant-payload field on a multi-variant ADT, the
            // `seeds = [...]` macro can't route through the
            // accessor and we suppressed it in
            // `quasar_account_attr`. Emit the runtime check here
            // instead. Quasar still fires on any state-field seed
            // (its R13 suppression is broader).
            let anchor_variant_field_seed = matches!(target, Target::Anchor)
                && needs_state_field_seed
                && is_multi_variant_adt_state(spec)
                && seeds.iter().any(|seed| {
                    let is_literal = seed.starts_with('"') && seed.ends_with('"');
                    if is_literal || bound_account_names.contains(seed.as_str()) {
                        return false;
                    }
                    spec.account_types.iter().any(|a| {
                        a.variants
                            .iter()
                            .any(|v| v.fields.iter().any(|(n, _)| n == seed))
                    })
                });
            let fire_r28 = (matches!(target, Target::Quasar) && needs_state_field_seed)
                || anchor_variant_field_seed;
            if !fire_r28 {
                continue;
            }

            let mut seed_exprs: Vec<String> = Vec::with_capacity(seeds.len() + 1);
            for seed in seeds {
                if let Some(inner) = seed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    seed_exprs.push(format!("b\"{}\"", inner));
                } else if bound_account_names.contains(seed.as_str()) {
                    // Handler-bound account: read its address.
                    match target {
                        Target::Anchor => seed_exprs.push(format!("ctx.{}.key().as_ref()", seed)),
                        _ => seed_exprs
                            .push(format!("ctx.{}.to_account_view().address().as_ref()", seed)),
                    }
                } else {
                    // State-field seed: read off the same PDA's stored
                    // value. For multi-variant ADTs on Anchor, route
                    // through the v2.29 Slice B accessor; for Quasar
                    // / flat-state, read the field directly.
                    let is_variant_field = matches!(target, Target::Anchor)
                        && spec.account_types.iter().any(|a| {
                            a.variants
                                .iter()
                                .any(|v| v.fields.iter().any(|(n, _)| n == seed))
                        });
                    if is_variant_field {
                        seed_exprs.push(format!("ctx.{}.inner.{}().as_ref()", acct.name, seed));
                    } else {
                        seed_exprs.push(format!("ctx.{}.{}.as_ref()", acct.name, seed));
                    }
                }
            }

            match target {
                Target::Anchor => {
                    // Anchor PDA verification uses
                    // `anchor_lang::solana_program::pubkey::Pubkey::
                    // create_program_address` with the stored bump
                    // to avoid the find_program_address syscall cost.
                    seed_exprs.push(format!("&[ctx.{}.bump]", acct.name));
                    out.push_str(&format!(
                        "    // R28 PDA check: ctx.{acct} matches its declared seeds (Anchor)\n    {{\n        let __seeds: &[&[u8]] = &[{seeds}];\n        let __expected = anchor_lang::solana_program::pubkey::Pubkey::create_program_address(__seeds, &crate::ID).map_err(|_| {err_enum}::InvalidPda)?;\n        if ctx.{acct}.key() != __expected {{\n            return Err({err_enum}::InvalidPda.into());\n        }}\n    }}\n",
                        acct = acct.name,
                        seeds = seed_exprs.join(", "),
                        err_enum = err_enum_name_r28,
                    ));
                }
                _ => {
                    seed_exprs.push(format!("&[ctx.{}.bump]", acct.name));
                    out.push_str(&format!(
                        "    // R28 PDA check: ctx.{acct} matches its declared seeds\n    {{\n        let __seeds: &[&[u8]] = &[{seeds}];\n        if quasar_lang::pda::verify_program_address(__seeds, &crate::ID, ctx.{acct}.to_account_view().address()).is_err() {{\n            return Err(ProgramError::from({err_enum}::InvalidPda));\n        }}\n    }}\n",
                        acct = acct.name,
                        seeds = seed_exprs.join(", "),
                        err_enum = err_enum_name_r28,
                    ));
                }
            }
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
        //
        // v2.29.2 — use the canonical-fallback resolver so multi-
        // writable handlers whose state account is `readonly` still
        // get `s.<field>` rewritten to `ctx.<canonical>.<field>`
        // instead of left unbound.
        let state_acct = resolve_handler_state_account(handler, spec);
        // Bare handler-account idents in spec expressions (e.g. the
        // `approver` in `state.members[i] == approver`) need to be
        // lowered to the runtime pubkey load `*ctx.<name>.to_account_view().address()`.
        // Without this, the spec's signer-binding compiles to `... ==
        // approver` where `approver` resolves to nothing in scope.
        let handler_account_names: Vec<String> =
            handler.accounts.iter().map(|a| a.name.clone()).collect();
        // v2.29 Slice G.4 — index handler accounts by name so the
        // `<acct>.<field>` rewriting can dispatch on
        // `imported_namespace` and route through the local mirror.
        let handler_accounts_by_name: std::collections::HashMap<
            &str,
            &crate::check::ParsedHandlerAccount,
        > = handler
            .accounts
            .iter()
            .map(|a| (a.name.as_str(), a))
            .collect();
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
                                // v2.29 Slice G.4 — `<imported_acct>.<field>`
                                // routes through the local mirror at
                                // `crate::imported::<ns>::<Type>`. Multi-
                                // variant ADT goes through the accessor
                                // (`(*ctx.<name>.inner.<field>())`); flat
                                // structs read directly off the wrapper's
                                // auto-deref (`ctx.<name>.<field>`).
                                if after < bytes.len() && bytes[after] == b'.' {
                                    if let Some(acct_meta) =
                                        handler_accounts_by_name.get(name.as_str())
                                    {
                                        if let (Some(ns), Some(ty)) =
                                            (&acct_meta.imported_namespace, &acct_meta.account_type)
                                        {
                                            // Extract the field ident.
                                            let mut j = after + 1;
                                            while j < bytes.len() && is_ident_char(bytes[j]) {
                                                j += 1;
                                            }
                                            let field = &expr[after + 1..j];
                                            if !field.is_empty() {
                                                let imported_ns =
                                                    spec.imported_namespaces.get(ns.as_str());
                                                let imported_ty = imported_ns.and_then(|ins| {
                                                    ins.account_types.iter().find(|a| &a.name == ty)
                                                });
                                                let is_multi_variant = imported_ty
                                                    .map(|a| a.variants.len() > 1)
                                                    .unwrap_or(false);
                                                if is_multi_variant {
                                                    after_accounts.push_str(&format!(
                                                        "(*ctx.{}.inner.{}())",
                                                        name, field
                                                    ));
                                                } else {
                                                    after_accounts.push_str(&format!(
                                                        "ctx.{}.{}",
                                                        name, field
                                                    ));
                                                }
                                                i = j;
                                                matched = true;
                                                break;
                                            }
                                        }
                                    }
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
            // v2.29 Slice B (#12 deep) — when the state is a multi-
            // variant ADT, fields that live on variant payloads can't
            // be reached through `ctx.<state>.<field>` directly
            // (the wrapper only carries `inner`). Route through the
            // accessor method emitted in generate_state. We look
            // ahead after each `s.` match to grab the identifier and
            // dispatch: known variant-payload field → accessor call,
            // otherwise → the bare `ctx.<state>.<field>` rewrite for
            // flat-state compatibility.
            let multi_variant = is_multi_variant_adt_state(spec);
            let accessor_fields: std::collections::HashSet<String> = if multi_variant {
                let mut set = std::collections::HashSet::new();
                if let Some(acct) = spec.account_types.first() {
                    let total = acct.variants.len();
                    let mut idx: std::collections::BTreeMap<String, usize> =
                        std::collections::BTreeMap::new();
                    let mut tys: std::collections::BTreeMap<String, String> =
                        std::collections::BTreeMap::new();
                    let mut consistent: std::collections::BTreeMap<String, bool> =
                        std::collections::BTreeMap::new();
                    for variant in &acct.variants {
                        for (fname, ftype) in &variant.fields {
                            *idx.entry(fname.clone()).or_insert(0) += 1;
                            if let Some(existing) = tys.get(fname) {
                                if existing != ftype {
                                    consistent.insert(fname.clone(), false);
                                }
                            } else {
                                tys.insert(fname.clone(), ftype.clone());
                                consistent.insert(fname.clone(), true);
                            }
                        }
                    }
                    let _ = total;
                    for fname in idx.keys() {
                        if *consistent.get(fname).unwrap_or(&true) {
                            set.insert(fname.clone());
                        }
                    }
                }
                set
            } else {
                std::collections::HashSet::new()
            };
            let bare_target = format!("ctx.{}.", sa.name);
            let bytes = after_accounts.as_bytes();
            let mut out = String::with_capacity(after_accounts.len());
            let mut i = 0;
            while i < bytes.len() {
                let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
                if prev_ok && i + 1 < bytes.len() && bytes[i] == b's' && bytes[i + 1] == b'.' {
                    // Look ahead to extract the field identifier.
                    let mut j = i + 2;
                    while j < bytes.len() && is_ident_char(bytes[j]) {
                        j += 1;
                    }
                    let field = &after_accounts[i + 2..j];
                    if !field.is_empty() && accessor_fields.contains(field) {
                        // v2.29 Slice B accessor call. Wrap in
                        // parens + deref so subsequent ops (e.g.
                        // `!*paused`, `state.lp_supply.bits`) parse
                        // against the accessor return value.
                        out.push_str(&format!("(*ctx.{}.inner.{}())", sa.name, field));
                        i = j;
                    } else {
                        out.push_str(&bare_target);
                        i += 2;
                    }
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

        // v2.29 Slice B — collect abstract-binder names. Requires that
        // reference an abstract binder can't run in the guard fn (the
        // binder is computed AFTER the guard fires in the handler
        // scaffold). Defer to the handler body and document the skip.
        let abstract_binder_names: Vec<&str> = handler
            .abstract_binders
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();

        // v2.29.2 — emit spec-level `let X = ref_impl(...)` bindings
        // here so `requires X > 0` clauses can reference them. Without
        // this, guards.rs emitted the requires check against a name
        // that's only bound later in the handler body (`let lp_out =
        // lp_token_out(...)` lives in the user-owned handler stub),
        // tripping `cannot find value 'lp_out' in this scope`. Each
        // RHS goes through `bind_state` so `s.<field>` reads route
        // through `ctx.<state>.<field>` (the guards binder).
        for (binding_name, _lean_expr, rust_expr) in &handler.let_bindings {
            let rewritten = bind_state(rust_expr);
            out.push_str(&format!(
                "    // let-binding from spec: {} = {}\n",
                binding_name, rust_expr
            ));
            out.push_str(&format!("    let {} = {};\n", binding_name, rewritten));
        }

        for req in &handler.requires {
            // Emit as a comment for human readers + an executable check.
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
            let raw = if pod_target {
                req.rust_expr_pod.trim()
            } else {
                req.rust_expr.trim()
            };

            // v2.29 Slice B — abstract-binder defer. The guard runs
            // before the user's handler body computes the binder; the
            // verifier still enforces this clause via the binder's
            // symbolic value. The user should re-assert it in their
            // handler body after the binder is computed.
            let references_abstract = !abstract_binder_names.is_empty()
                && abstract_binder_names
                    .iter()
                    .any(|name| contains_word_boundary(raw, name));
            if references_abstract {
                out.push_str(
                    "    //   DEFERRED — references an `abstract` binder; verifier still\n",
                );
                out.push_str(
                    "    //   enforces the clause symbolically. Re-assert in the handler body\n",
                );
                out.push_str(
                    "    //   after the `let <binder> = …;` line if you want a runtime check.\n",
                );
                continue;
            }

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

/// True when `expr` references the spec's state binder `s` (a word-bounded
/// `s` immediately followed by `.`), i.e. it reads a state field.
fn references_pinocchio_state(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b's'
            && (i == 0 || !is_ident_char(bytes[i - 1]))
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'.'
        {
            return true;
        }
    }
    false
}

/// `true` iff state field `field` is declared `Pubkey` in any account
/// variant. Pubkey fields lower to a raw `[u8; 32]` in the zeropod struct
/// (not a Pod scalar wrapper), so they are read by value — no `.get()` —
/// and compared against `*<acct>.key()` (also a `[u8; 32]` value).
fn state_field_is_pubkey(spec: &ParsedSpec, field: &str) -> bool {
    spec.account_types.iter().any(|a| {
        a.variants
            .iter()
            .any(|v| v.fields.iter().any(|(n, t)| n == field && t == "Pubkey"))
    })
}

/// Rewrite a spec-rendered Pod expression (`rust_expr_pod`) for the
/// Pinocchio guard / handler context: the state binder `s` → `state_var`
/// (the decoded zeropod `&Zc` view), and bare handler-account idents (or
/// `<acct>.pubkey`) → `*<acct_prefix>.<acct>.key()` (a `[u8; 32]` value).
/// `acct_prefix` is `"ctx"` in the per-handler guard fn (its param is
/// `ctx: &<Pascal>`) and `"self"` in the handler method's effect body
/// (where the accounts struct is `self`).
///
/// Scalar zeropod fields read through `.get()`; `Pubkey` fields are raw
/// `[u8; 32]` (no Pod wrapper) so they read by value with neither `.get()`
/// nor `&`, matching the deref'd `key()` form on the other side.
fn bind_pinocchio_expr(
    expr: &str,
    handler: &ParsedHandler,
    state_var: &str,
    acct_prefix: &str,
    spec: &ParsedSpec,
) -> String {
    let account_names: std::collections::HashSet<&str> =
        handler.accounts.iter().map(|a| a.name.as_str()).collect();
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        if prev_ok && is_ident_char(bytes[i]) {
            let start = i;
            let mut j = i;
            while j < bytes.len() && is_ident_char(bytes[j]) {
                j += 1;
            }
            let ident = &expr[start..j];
            if ident == "s" {
                // `s.<field>` → `<state_var>.<field>.get()` for a scalar
                // read (zeropod Pod fields need an explicit `.get()` to
                // produce the native integer). `Pubkey` fields are raw
                // `[u8; 32]` (no Pod wrapper) so they read by value with
                // no `.get()`. Complex paths (`s.x.y` / `s.x[i]`) emit the
                // path head without `.get()` — those nested/array reads
                // are a follow-on.
                if j < bytes.len() && bytes[j] == b'.' {
                    let fstart = j + 1;
                    let mut k = fstart;
                    while k < bytes.len() && is_ident_char(bytes[k]) {
                        k += 1;
                    }
                    let field = &expr[fstart..k];
                    let complex = k < bytes.len() && (bytes[k] == b'.' || bytes[k] == b'[');
                    if !field.is_empty() && !complex && !state_field_is_pubkey(spec, field) {
                        out.push_str(&format!("{}.{}.get()", state_var, field));
                    } else {
                        out.push_str(&format!("{}.{}", state_var, field));
                    }
                    i = k;
                    continue;
                }
                out.push_str(state_var);
                i = j;
                continue;
            }
            if account_names.contains(ident) {
                // `<acct>.pubkey` (spec's "this account's address") and a
                // bare `<acct>` both lower to the runtime key load,
                // deref'd to a `[u8; 32]` value so it compares against /
                // assigns into a raw-`[u8; 32]` Pubkey state field.
                let pubkey = b".pubkey";
                if j + pubkey.len() <= bytes.len()
                    && &bytes[j..j + pubkey.len()] == pubkey
                    && (j + pubkey.len() == bytes.len() || !is_ident_char(bytes[j + pubkey.len()]))
                {
                    out.push_str(&format!("*{}.{}.key()", acct_prefix, ident));
                    i = j + pubkey.len();
                    continue;
                }
                out.push_str(&format!("*{}.{}.key()", acct_prefix, ident));
                i = j;
                continue;
            }
            out.push_str(ident);
            i = j;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Emit `src/guards.rs` for the Pinocchio target (slice 6 4b). Per-handler
/// guard fns take `ctx: &<Pascal>` + params and return `ProgramResult`.
/// Handles signer-`auth` (`is_signer`) and `requires` / `aborts_if` (param
/// clauses directly; scalar state clauses via a one-time zeropod decode of
/// the state account, reusing `rust_expr_pod` since zeropod shares
/// quasar-pod's `.get()` API). Lifecycle pre-checks + PDA verification, and
/// state clauses on multi-account specs, are deferred (documented skip).
fn emit_pinocchio_guards(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let mut out = String::new();
    out.push_str(&marker(
        "DO NOT EDIT — regenerated from .qedspec",
        fp,
        "src/guards.rs",
    ));
    out.push_str("//! Per-handler guard checks derived from the `.qedspec` (Pinocchio).\n\n");
    out.push_str(
        "#![allow(unused_variables, unused_imports, dead_code, clippy::too_many_arguments)]\n\n",
    );
    out.push_str(
        "use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};\n",
    );
    out.push_str("use zeropod::ZeroPodFixed;\n");
    out.push_str("use crate::state::*;\n");
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    out.push_str("use crate::instructions::*;\n\n");

    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    // Multi-account state decode is deferred — the decode type is
    // unambiguous only for single-account specs.
    let single_state = spec.account_types.len() <= 1;
    let state_type = format!("{}Account", to_pascal_case(&spec.program_name));

    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        let mut params = vec![format!("ctx: &{}", pascal)];
        for (pname, ptype) in &handler.takes_params {
            params.push(format!("{}: {}", pname, map_type_standalone(ptype, spec)?));
        }
        out.push_str(&format!(
            "/// Guards for `{}` — generated from the spec's `requires` / `auth` clauses.\n",
            handler.name
        ));
        out.push_str(&format!(
            "pub fn {}({}) -> ProgramResult {{\n",
            handler.name,
            params.join(", ")
        ));

        if let Some(who) = &handler.who {
            if handler
                .accounts
                .iter()
                .any(|a| &a.name == who && a.is_signer)
            {
                out.push_str(&format!("    // auth {}\n", who));
                out.push_str(&format!(
                    "    if !ctx.{}.is_signer() {{\n        return Err(ProgramError::MissingRequiredSignature);\n    }}\n",
                    who
                ));
            }
        }

        let needs_state = handler
            .requires
            .iter()
            .any(|r| references_pinocchio_state(&r.rust_expr))
            || handler
                .aborts_if
                .iter()
                .any(|a| references_pinocchio_state(&a.rust_expr));
        let decoded = if needs_state && single_state {
            match resolve_handler_state_account(handler, spec) {
                Some(acct) => {
                    out.push_str(&format!(
                        "    let __state = {}::from_bytes(unsafe {{ ctx.{}.borrow_data_unchecked() }})\n        .map_err(|_| ProgramError::InvalidAccountData)?;\n",
                        state_type, acct.name
                    ));
                    true
                }
                None => false,
            }
        } else {
            false
        };

        for req in &handler.requires {
            let raw = req.rust_expr.trim();
            if references_pinocchio_state(raw) && !decoded {
                out.push_str(&format!(
                    "    // TODO(slice 6 4b-cont): state-referencing requires (multi-account /\n    //   unresolved state account) — not enforced yet: {}\n",
                    req.lean_expr.trim()
                ));
                continue;
            }
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let rust = bind_pinocchio_expr(raw, handler, "__state", "ctx", spec);
            let err = match &req.error_name {
                Some(e) => format!("ProgramError::from({}::{})", err_enum, e),
                None => "ProgramError::Custom(0xFF)".to_string(),
            };
            out.push_str(&format!("    if !({}) {{ return Err({}); }}\n", rust, err));
        }

        for ab in &handler.aborts_if {
            let raw = ab.rust_expr.trim();
            if references_pinocchio_state(raw) && !decoded {
                out.push_str(&format!(
                    "    // TODO(slice 6 4b-cont): state-referencing abort — not enforced yet: {}\n",
                    ab.lean_expr.trim()
                ));
                continue;
            }
            let rust = bind_pinocchio_expr(raw, handler, "__state", "ctx", spec);
            out.push_str(&format!(
                "    if ({}) {{ return Err(ProgramError::from({}::{})); }}\n",
                rust, err_enum, ab.error_name
            ));
        }

        out.push_str("    Ok(())\n}\n\n");
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
    "pinocchio",
    "pinocchio-token",
    "pinocchio-pubkey",
    "zeropod",
    "qedgen-macros",
];

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
pub(crate) fn merge_cargo_toml(existing: &str, fresh: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{ImportedNamespace, ParsedAccountType, ParsedVariant};

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

    /// Slice 6 step 3 — Pinocchio state.rs is zeropod zero-copy: a
    /// sum-type State lowers to a `#[repr(u8)]` discriminant tag enum +
    /// a flat `#[derive(ZeroPod)]` superset struct (tag byte + every
    /// variant field flattened, deduped). No `#[account]` / Anchor shape.
    #[test]
    fn pinocchio_state_sum_type_lowers_to_tag_plus_superset() {
        let src = r#"spec Escrow

type State
  | Uninitialized
  | Open of {
      initializer : Pubkey,
      amount      : U64,
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

        // Pinocchio state emission is a shared helper (codegen_mir calls it
        // directly; the legacy generate_state pipeline no longer routes
        // Pinocchio — MIR is the path).
        let mut state = String::new();
        emit_pinocchio_state(&spec, &fp, &mut state).unwrap();

        // zeropod imports, not the pinocchio AccountInfo prelude.
        assert!(
            state.contains("use zeropod::ZeroPod;"),
            "must import zeropod; got:\n{state}"
        );
        // Discriminant tag enum from the variant names.
        assert!(
            state.contains("#[repr(u8)]")
                && state.contains("pub enum EscrowAccountTag {")
                && state.contains("Uninitialized = 0,")
                && state.contains("Open = 1,")
                && state.contains("Closed = 2,"),
            "must emit a #[repr(u8)] tag enum from variants; got:\n{state}"
        );
        // Flat ZeroPod superset struct: tag byte + flattened variant fields
        // (Pubkey -> [u8; 32], U64 -> u64; the derive makes the Pod companion).
        assert!(
            state.contains("#[derive(ZeroPod)]\npub struct EscrowAccount {")
                && state.contains("pub tag: u8,")
                && state.contains("pub initializer: [u8; 32],")
                && state.contains("pub amount: u64,"),
            "must flatten variant payloads into one ZeroPod struct; got:\n{state}"
        );
        // No Anchor/Quasar shape leakage.
        assert!(
            !state.contains("#[account]") && !state.contains("EscrowAccountInner"),
            "Pinocchio state must not emit the #[account] wrapper/inner-enum; got:\n{state}"
        );
    }

    /// Slice 6 step 4a — Pinocchio instruction scaffold: a struct of
    /// `&AccountInfo` fields + a `process_<name>` wrapper that binds the
    /// account slice positionally, LE-parses numeric params, and calls
    /// `.handler()` (which calls `guards::<name>`).
    #[test]
    fn pinocchio_handler_scaffold_emits_struct_and_process_wrapper() {
        let src = r#"spec Vault
type Error | InvalidAmount
state { balance : U64 }
handler deposit (amount : U64) {
  accounts {
    authority : signer, writable
    vault     : writable
  }
  requires amount > 0 else InvalidAmount
  effect { balance += amount }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "deposit").unwrap();
        let out = render_pinocchio_handler_scaffold(handler, &spec).unwrap();

        // Accounts struct of &AccountInfo (no typed wrappers).
        assert!(
            out.contains("pub struct Deposit<'a> {")
                && out.contains("pub authority: &'a AccountInfo,")
                && out.contains("pub vault: &'a AccountInfo,"),
            "must emit a &AccountInfo accounts struct; got:\n{out}"
        );
        // Handler method takes the param + calls the guard.
        assert!(
            out.contains("pub fn handler(&mut self, amount: u64) -> ProgramResult {")
                && out.contains("guards::deposit(self, amount)?;"),
            "handler must take params + call guards; got:\n{out}"
        );
        // Effect body: zeropod mutable decode + checked scalar arithmetic.
        // (No MathOverflow declared → falls back to ProgramError::ArithmeticOverflow.)
        assert!(
            out.contains(
                "VaultAccount::from_bytes_mut(unsafe { self.vault.borrow_mut_data_unchecked() })"
            ),
            "effect body must mutably decode the state account; got:\n{out}"
        );
        assert!(
            out.contains("__state.balance = __state.balance.get().checked_add(amount).ok_or(ProgramError::ArithmeticOverflow)?.into();"),
            "scalar `+=` must lower to a checked .get()/.into() update; got:\n{out}"
        );
        // process_<name> wrapper: positional account binding + LE param
        // parse + struct build + dispatch.
        assert!(
            out.contains("pub fn process_deposit(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {")
                && out.contains("let [authority, vault, ..] = accounts else {")
                && out.contains("return Err(ProgramError::NotEnoughAccountKeys);")
                && out.contains("u64::from_le_bytes(")
                && out.contains("let mut ctx = Deposit { authority, vault };")
                && out.contains("ctx.handler(amount)"),
            "process wrapper must bind accounts + parse params + dispatch; got:\n{out}"
        );
        // No Anchor/Quasar Context shape.
        assert!(
            !out.contains("Context<") && !out.contains("Ctx<") && !out.contains("to_account_info"),
            "Pinocchio scaffold must not leak the Anchor/Quasar context shape; got:\n{out}"
        );
    }

    /// Slice 6 step 4b — Pinocchio guards.rs: signer-`auth` via `is_signer`,
    /// param `requires` directly, and state-referencing `requires` via a
    /// one-time zeropod decode (`State::from_bytes` + `__state.<field>.get()`).
    #[test]
    fn pinocchio_guards_signer_param_and_state_requires() {
        let src = r#"spec Vault
type Error | InvalidAmount | Insufficient
state { balance : U64 }
handler withdraw (amount : U64) {
  auth owner
  accounts {
    owner : signer
    vault : writable
  }
  requires amount > 0 else InvalidAmount
  requires state.balance >= amount else Insufficient
  effect { balance -= amount }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(out_dir.join("src")).unwrap();

        emit_pinocchio_guards(&spec, &fp, &out_dir).unwrap();
        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();

        // Guard fn signature + pinocchio/zeropod imports.
        assert!(
            guards.contains("use zeropod::ZeroPodFixed;")
                && guards
                    .contains("pub fn withdraw(ctx: &Withdraw, amount: u64) -> ProgramResult {"),
            "guard fn signature + imports; got:\n{guards}"
        );
        // Signer-auth.
        assert!(
            guards.contains("if !ctx.owner.is_signer() {")
                && guards.contains("return Err(ProgramError::MissingRequiredSignature);"),
            "must emit the signer-auth check; got:\n{guards}"
        );
        // Param requires — direct.
        assert!(
            guards.contains(
                "if !(amount > 0) { return Err(ProgramError::from(VaultError::InvalidAmount)); }"
            ),
            "param requires must emit a direct if-check; got:\n{guards}"
        );
        // State requires — decode + .get() on the decoded view.
        assert!(
            guards
                .contains("VaultAccount::from_bytes(unsafe { ctx.vault.borrow_data_unchecked() })"),
            "state-referencing requires must decode the state account; got:\n{guards}"
        );
        assert!(
            guards.contains("__state.balance.get() >= amount")
                && guards.contains("VaultError::Insufficient"),
            "state requires must read via the decoded __state view; got:\n{guards}"
        );
    }

    /// Regression for issue #71: `Pubkey` state fields lower to a raw
    /// `[u8; 32]` in the zeropod struct (no Pod scalar wrapper), so a
    /// `state.<pubkey> == <acct>.pubkey` guard must compare the field by
    /// value (NO `.get()`) against the deref'd account key
    /// (`*ctx.<acct>.key()`). Scalar fields keep `.get()`. The effect
    /// body sets a `Pubkey` field via the deref'd value directly (no
    /// `.into()`) and binds account refs through `self` (the handler
    /// method), not the guard fn's `ctx`.
    #[test]
    fn pinocchio_pubkey_state_field_reads_by_value_not_get() {
        let src = r#"spec Cfg
program_id "11111111111111111111111111111111"
type Error | BadAuth | BadLane
type State
  | Active of { admin_key : Pubkey, lane_count : U64 }
handler set_admin : State.Active -> State.Active {
  accounts { config : writable, admin : readonly }
  effect { Active.admin_key := admin.pubkey }
}
handler check (lane_id : U64) : State.Active -> State.Active {
  accounts { config : readonly, caller : readonly }
  requires caller.pubkey == state.admin_key else BadAuth
  requires lane_id < state.lane_count else BadLane
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(out_dir.join("src")).unwrap();
        emit_pinocchio_guards(&spec, &fp, &out_dir).unwrap();
        let guards = std::fs::read_to_string(out_dir.join("src/guards.rs")).unwrap();

        // Pubkey field: by value (no `.get()`), account key deref'd.
        assert!(
            guards.contains("*ctx.caller.key() == __state.admin_key"),
            "Pubkey guard must compare by value with a deref'd key; got:\n{guards}"
        );
        assert!(
            !guards.contains("admin_key.get()"),
            "Pubkey field must NOT be read through `.get()`; got:\n{guards}"
        );
        // Scalar field still reads through `.get()`.
        assert!(
            guards.contains("lane_id < __state.lane_count.get()"),
            "scalar field must keep `.get()`; got:\n{guards}"
        );

        // Effect body: Pubkey set via deref'd value, bound through `self`.
        let mut effect = String::new();
        let h = spec
            .handlers
            .iter()
            .find(|h| h.name == "set_admin")
            .unwrap();
        emit_pinocchio_effect_body(&mut effect, h, &spec);
        assert!(
            effect.contains("__state.admin_key = *self.admin.key();"),
            "Pubkey set must assign the deref'd key via `self` (no `.into()`); got:\n{effect}"
        );
    }

    /// v2.26 Slice 2 — the `unconstrained_modifies` lint must still
    /// fire on a multi-variant ADT spec where `modifies [X]` lists a
    /// field that's neither written by `effect` nor referenced by any
    /// `ensures` clause. The lint is field-name-based and target-
    /// agnostic, so this just locks the behavior against future
    /// regressions in the ADT path.
    #[test]
    fn unconstrained_modifies_fires_on_adt_spec() {
        let src = r#"spec Pool

type State
  | Uninitialized
  | Active of {
      pool_balance : U64,
      lp_supply    : U64,
    }

type Error
  | MathOverflow
  | WrongState
  | InvalidLifecycle
  | Unauthorized

handler deposit (amount : U64) : State.Active -> State.Active {
  accounts {
    pool : writable
    user : signer
  }
  requires amount > 0 else MathOverflow
  modifies [pool_balance, lp_supply]
  effect {
    Active.pool_balance += amount
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).unwrap();
        let warnings = crate::check::check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unconstrained_modifies")
            .expect("unconstrained_modifies must fire on the ADT spec");
        assert!(
            hit.message.contains("'lp_supply'"),
            "lint message must name the unconstrained field; got: {}",
            hit.message
        );
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

    /// Exercise try_emit_cpi against an end-to-end-parsed spec.
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
        let rendered =
            try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit Anchor CPI");
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

    /// Helper for the Quasar / Pinocchio CPI tests — same SPL Token
    /// transfer fixture shape used in
    /// `cpi_emits_anchor_spl_transfer_for_canonical_program_id`, but
    /// parameterized so each test can swap the called handler name in
    /// the call site.
    #[cfg(test)]
    fn parse_spl_transfer_caller_spec(called_handler: &str) -> crate::check::ParsedSpec {
        let spec_src = format!(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {{
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {{
    discriminant "0x03"
    accounts {{
      from      : writable
      to        : writable
      authority : signer
    }}
    requires amount > 0
    ensures  amount > 0
  }}
  handler mint_to (amount : U64) {{
    discriminant "0x07"
    accounts {{
      mint      : writable
      to        : writable
      authority : signer
    }}
  }}
}}

type State | Active of {{ balance : U64 }}
type Error | E

handler send (n : U64) : State.Active -> State.Active {{
  permissionless
  accounts {{
    state         : writable
    src           : writable
    dst           : writable
    mint          : writable
    auth          : signer
    token_program : program
  }}
  call Token.{}(from = src, to = dst, mint = mint, amount = n, authority = auth)
}}
"#,
            called_handler
        );
        crate::chumsky_adapter::parse_str(&spec_src).unwrap()
    }

    /// Caller fixture for `mint_to`: the canonical SPL interface names the
    /// signer slot `mint_authority`. Shared by the Quasar + Pinocchio
    /// mint_to tests (the shared transfer fixture passes `authority`, which
    /// mint_to doesn't accept).
    fn parse_mint_to_caller_spec() -> ParsedSpec {
        crate::chumsky_adapter::parse_str(
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
        .unwrap()
    }

    /// Caller fixture for `close_account` (no scalar; account/destination/
    /// authority). Shared by the Quasar + Pinocchio close tests.
    fn parse_close_account_caller_spec() -> ParsedSpec {
        crate::chumsky_adapter::parse_str(
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
        .unwrap()
    }

    /// Caller fixture for `initialize_account` (no scalar; account/mint/
    /// owner/rent). Shared by the Quasar (→ None) + Pinocchio init tests.
    fn parse_init_account_caller_spec() -> ParsedSpec {
        crate::chumsky_adapter::parse_str(
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
        .unwrap()
    }

    /// Spike: Quasar SPL Token transfer emits a one-line method chain
    /// on the token-program account, NOT an `anchor_spl::*` builder.
    /// The shape is:
    ///   self.token_program.transfer(&self.src, &self.dst, &self.auth, n).invoke()?;
    #[test]
    fn cpi_emits_quasar_spl_transfer() {
        let spec = parse_spl_transfer_caller_spec("transfer");
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Quasar)
            .expect("Quasar SPL transfer must emit");
        assert!(
            rendered.contains("self.token_program.transfer("),
            "Quasar shape must invoke transfer on the token-program account; got:\n{rendered}"
        );
        assert!(
            rendered.contains("&self.src"),
            "from arg must resolve to &self.src; got:\n{rendered}"
        );
        assert!(
            rendered.contains("&self.dst"),
            "to arg must resolve to &self.dst; got:\n{rendered}"
        );
        assert!(
            rendered.contains("&self.auth"),
            "authority arg must resolve to &self.auth; got:\n{rendered}"
        );
        assert!(
            rendered.trim_end().ends_with(".invoke()?;"),
            "Quasar trait chain must terminate with .invoke()?; got:\n{rendered}"
        );
        // Anti-regression: must NOT leak the Anchor shape.
        assert!(
            !rendered.contains("anchor_spl"),
            "Quasar emission must not import anchor_spl; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("CpiContext"),
            "Quasar emission must not construct CpiContext; got:\n{rendered}"
        );
    }

    /// Slice 2: Quasar SPL `mint_to` emits the trait method chain. The
    /// spec names the signer `mint_authority`; it resolves positionally
    /// into `TokenCpi::mint_to(mint, to, authority, amount)`.
    #[test]
    fn cpi_emits_quasar_spl_mint_to() {
        let spec = parse_mint_to_caller_spec();
        let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Quasar)
            .expect("Quasar SPL mint_to must emit");
        assert!(
            rendered.contains("self.token_program.mint_to("),
            "must invoke mint_to on the token-program account; got:\n{rendered}"
        );
        assert!(
            rendered.contains("&self.the_mint")
                && rendered.contains("&self.holder_ta")
                && rendered.contains("&self.minter"),
            "mint/to/mint_authority must resolve to call-site accounts; got:\n{rendered}"
        );
        assert!(
            rendered.trim_end().ends_with(".invoke()?;"),
            "must terminate with .invoke()?; got:\n{rendered}"
        );
    }

    /// Slice 2: Quasar SPL `burn` — TokenCpi::burn(from, mint, authority,
    /// amount). The shared transfer fixture supplies exactly this arg set.
    #[test]
    fn cpi_emits_quasar_spl_burn() {
        let spec = parse_spl_transfer_caller_spec("burn");
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered =
            try_emit_cpi(call, handler, &spec, Target::Quasar).expect("Quasar SPL burn must emit");
        assert!(
            rendered.contains("self.token_program.burn(")
                && rendered.contains("&self.src")
                && rendered.contains("&self.mint")
                && rendered.contains("&self.auth"),
            "burn must resolve from/mint/authority; got:\n{rendered}"
        );
    }

    /// Slice 2: Quasar SPL `close_account` — no scalar; three account args
    /// (account, destination, authority).
    #[test]
    fn cpi_emits_quasar_spl_close_account_no_amount() {
        let spec = parse_close_account_caller_spec();
        let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Quasar)
            .expect("Quasar SPL close_account must emit");
        assert!(
            rendered.contains("self.token_program.close_account(")
                && rendered.contains("&self.target_acct")
                && rendered.contains("&self.sweep_target")
                && rendered.contains("&self.closer"),
            "close_account must resolve account/destination/authority; got:\n{rendered}"
        );
        assert!(
            rendered.trim_end().ends_with(".invoke()?;"),
            "must terminate with .invoke()?; got:\n{rendered}"
        );
    }

    /// Slice 2: Quasar SPL `initialize_account` stays `None` — `quasar-spl`
    /// exposes only `initialize_account3` (owner is a raw `&Address`, no
    /// rent sysvar), which doesn't fit the uniform account-view helper.
    #[test]
    fn cpi_quasar_spl_initialize_account_falls_through_to_none() {
        let spec = parse_init_account_caller_spec();
        let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
        let call = handler.calls.first().unwrap();
        assert!(
            try_emit_cpi(call, handler, &spec, Target::Quasar).is_none(),
            "Quasar initialize_account has no uniform shape; must fall through to None"
        );
    }

    /// Spike commit 2: Pinocchio SPL Token transfer emits a struct-
    /// construction `Transfer { … }.invoke()?` — sibling shape to the
    /// Quasar method chain but with field assignments.
    ///
    /// Note: the Pinocchio emitter is dead code from the CLI today
    /// (scaffold gate at `main.rs:3132`); this test exercises the
    /// emitter directly. When slice 6 lands, this is the same string
    /// the CLI emits.
    #[test]
    fn cpi_emits_pinocchio_spl_transfer() {
        let spec = parse_spl_transfer_caller_spec("transfer");
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
            .expect("Pinocchio SPL transfer must emit");
        assert!(
            rendered.contains("pinocchio_token::instructions::Transfer {"),
            "Pinocchio shape must construct the qualified Transfer struct; got:\n{rendered}"
        );
        assert!(
            rendered.contains("from:") && rendered.contains("self.src"),
            "from field must resolve to self.src; got:\n{rendered}"
        );
        assert!(
            rendered.contains("to:") && rendered.contains("self.dst"),
            "to field must resolve to self.dst; got:\n{rendered}"
        );
        assert!(
            rendered.contains("authority:") && rendered.contains("self.auth"),
            "authority field must resolve to self.auth; got:\n{rendered}"
        );
        // The struct fields are `&'a AccountInfo`, so the emitter must NOT
        // prepend `&` (that would yield `&&AccountInfo`).
        assert!(
            !rendered.contains("&self."),
            "Pinocchio CPI must pass `self.<acct>` not `&self.<acct>`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("amount:"),
            "amount scalar must appear as a struct field; got:\n{rendered}"
        );
        assert!(
            rendered.contains("}.invoke()?;"),
            "Pinocchio struct must terminate with .invoke()?; got:\n{rendered}"
        );
        // Anti-regression: no Anchor / Quasar shape leakage.
        assert!(
            !rendered.contains("anchor_spl") && !rendered.contains("CpiContext"),
            "Pinocchio emission must not leak Anchor shape; got:\n{rendered}"
        );
        assert!(
            !rendered.contains(".transfer("),
            "Pinocchio is struct-construction, not method chain; got:\n{rendered}"
        );
    }

    /// Pinocchio System Program `transfer` CPI. `call System.transfer(...)`
    /// must lower to `pinocchio_system::instructions::Transfer { from, to,
    /// lamports }.invoke()?` — the spec's `amount` arg binds the struct's
    /// `lamports` field. Before this slice, System calls fell through to a
    /// `todo!()` breadcrumb on Pinocchio (only SPL Token was mechanized).
    #[test]
    fn cpi_emits_pinocchio_system_transfer() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface System {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    discriminant "0x02000000"
    accounts {
      from : signer, writable
      to   : writable
    }
    requires amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler topup (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    payer          : signer, writable
    pda            : writable
    system_program : program
  }
  call System.transfer(from = payer, to = pda, amount = n)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "topup").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
            .expect("Pinocchio System transfer must emit");
        assert!(
            rendered.contains("pinocchio_system::instructions::Transfer {"),
            "must construct the qualified System Transfer struct; got:\n{rendered}"
        );
        assert!(
            rendered.contains("from:") && rendered.contains("self.payer"),
            "from field must resolve to self.payer; got:\n{rendered}"
        );
        assert!(
            rendered.contains("to:") && rendered.contains("self.pda"),
            "to field must resolve to self.pda; got:\n{rendered}"
        );
        assert!(
            rendered.contains("lamports:"),
            "the spec `amount` arg must bind the `lamports` struct field; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("amount:"),
            "System Transfer has no `amount` field — it is `lamports`; got:\n{rendered}"
        );
        // Struct fields are `&AccountInfo`; no `&self.` double-ref.
        assert!(
            !rendered.contains("&self."),
            "Pinocchio CPI passes `self.<acct>`, not `&self.<acct>`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("}.invoke()?;"),
            "must terminate with .invoke()?; got:\n{rendered}"
        );
        // Anti-regression: no SPL-token / Anchor / Quasar shape leakage.
        assert!(
            !rendered.contains("pinocchio_token")
                && !rendered.contains("anchor")
                && !rendered.contains("CpiContext"),
            "System emission must not leak token / Anchor shape; got:\n{rendered}"
        );
    }

    /// System `create_account` / `assign` are not mechanized this slice
    /// (their `owner: &Pubkey` resolution from a spec `Pubkey` arg is a
    /// follow-on). `try_emit_cpi` must return `None` so the caller keeps
    /// the breadcrumb rather than emit code that may not type-check.
    #[test]
    fn cpi_pinocchio_system_create_account_is_breadcrumb() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface System {
  program_id "11111111111111111111111111111111"
  handler create_account (lamports : U64) (space : U64) (owner : Pubkey) {
    discriminant "0x00000000"
    accounts {
      payer       : signer, writable
      new_account : signer, writable
    }
    requires lamports > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler make (l : U64) (s : U64) (o : Pubkey) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    payer          : signer, writable
    pda            : signer, writable
    system_program : program
  }
  call System.create_account(payer = payer, new_account = pda, lamports = l, space = s, owner = o)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "make").unwrap();
        let call = handler.calls.first().unwrap();
        assert!(
            try_emit_cpi(call, handler, &spec, Target::Pinocchio).is_none(),
            "create_account must stay a breadcrumb (None) this slice"
        );
    }

    /// Anchor System Program `transfer` CPI. `call System.transfer(...)`
    /// must lower to the idiomatic `anchor_lang::system_program::transfer(
    /// CpiContext::new(cpi_program, Transfer { from, to }), amount)?` shape
    /// rather than the generic `solana_program::program::invoke` builder
    /// (which is what System fell through to on Anchor before this slice).
    #[test]
    fn cpi_emits_anchor_system_transfer() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface System {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    discriminant "0x02000000"
    accounts {
      from : signer, writable
      to   : writable
    }
    requires amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler topup (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    payer          : signer, writable
    pda            : writable
    system_program : program
  }
  call System.transfer(from = payer, to = pda, amount = n)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "topup").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor)
            .expect("Anchor System transfer must emit");
        assert!(
            rendered.contains("use anchor_lang::system_program::{self, Transfer};"),
            "must use anchor_lang::system_program::Transfer; got:\n{rendered}"
        );
        assert!(
            rendered.contains("from:") && rendered.contains("self.payer.to_account_info()"),
            "from must resolve to self.payer.to_account_info(); got:\n{rendered}"
        );
        assert!(
            rendered.contains("let cpi_program = self.system_program.to_account_info();"),
            "cpi_program must be the system_program account; got:\n{rendered}"
        );
        assert!(
            rendered.contains(
                "system_program::transfer(CpiContext::new(cpi_program, cpi_accounts), n)?;"
            ),
            "amount param `n` passes through bare to system_program::transfer; got:\n{rendered}"
        );
        // Anti-regression: must NOT fall back to the generic invoke builder
        // or leak the SPL token path.
        assert!(
            !rendered.contains("solana_program::program::invoke")
                && !rendered.contains("anchor_spl"),
            "System transfer must be idiomatic, not generic invoke / SPL; got:\n{rendered}"
        );
    }

    /// Slice 2b: Pinocchio SPL `mint_to` constructs the MintTo struct.
    /// pinocchio-token names the recipient slot `account` (canonical SPL
    /// `to`) and the signer `mint_authority`.
    #[test]
    fn cpi_emits_pinocchio_spl_mint_to() {
        let spec = parse_mint_to_caller_spec();
        let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
            .expect("Pinocchio SPL mint_to must emit");
        assert!(
            rendered.contains("pinocchio_token::instructions::MintTo {"),
            "must construct the MintTo struct; got:\n{rendered}"
        );
        assert!(
            rendered.contains("mint:") && rendered.contains("self.the_mint"),
            "mint field; got:\n{rendered}"
        );
        assert!(
            rendered.contains("account:") && rendered.contains("self.holder_ta"),
            "recipient maps to pinocchio field `account` ← spec `to`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("mint_authority:") && rendered.contains("self.minter"),
            "signer maps to `mint_authority`; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("&self."),
            "Pinocchio CPI must pass `self.<acct>` not `&self.<acct>`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("amount:") && rendered.contains("}.invoke()?;"),
            "amount scalar + .invoke()?; got:\n{rendered}"
        );
    }

    /// Slice 2b: Pinocchio SPL `burn` — Burn names the source slot
    /// `account` (canonical SPL `from`).
    #[test]
    fn cpi_emits_pinocchio_spl_burn() {
        let spec = parse_spl_transfer_caller_spec("burn");
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
            .expect("Pinocchio SPL burn must emit");
        assert!(
            rendered.contains("pinocchio_token::instructions::Burn {"),
            "must construct the Burn struct; got:\n{rendered}"
        );
        assert!(
            rendered.contains("account:") && rendered.contains("self.src"),
            "source maps to pinocchio field `account` ← spec `from`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("mint:") && rendered.contains("authority:"),
            "mint + authority fields; got:\n{rendered}"
        );
    }

    /// Slice 2b: Pinocchio SPL `initialize_account` — no scalar; rent
    /// sysvar maps to pinocchio field `rent_sysvar` (canonical SPL `rent`).
    #[test]
    fn cpi_emits_pinocchio_spl_initialize_account_no_amount() {
        let spec = parse_init_account_caller_spec();
        let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
            .expect("Pinocchio SPL initialize_account must emit");
        assert!(
            rendered.contains("pinocchio_token::instructions::InitializeAccount {"),
            "must construct InitializeAccount; got:\n{rendered}"
        );
        assert!(
            rendered.contains("rent_sysvar:") && rendered.contains("self.rent_sysvar"),
            "rent maps to pinocchio field `rent_sysvar`; got:\n{rendered}"
        );
        // No scalar — no `amount:` field.
        assert!(
            !rendered.contains("amount:") && rendered.contains("}.invoke()?;"),
            "no-amount handler must not emit an amount field; got:\n{rendered}"
        );
    }

    /// Slice 2b: Pinocchio SPL `close_account` — no scalar.
    #[test]
    fn cpi_emits_pinocchio_spl_close_account_no_amount() {
        let spec = parse_close_account_caller_spec();
        let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_cpi(call, handler, &spec, Target::Pinocchio)
            .expect("Pinocchio SPL close_account must emit");
        assert!(
            rendered.contains("pinocchio_token::instructions::CloseAccount {")
                && rendered.contains("self.target_acct")
                && rendered.contains("self.sweep_target")
                && rendered.contains("self.closer"),
            "close_account must resolve account/destination/authority; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("amount:") && rendered.contains("}.invoke()?;"),
            "no-amount handler must not emit an amount field; got:\n{rendered}"
        );
    }

    /// Pinocchio generic (non-SPL) CPI is unimplemented; the
    /// `(Target::Pinocchio, false)` branch in `try_emit_cpi` returns
    /// None (generic non-SPL Pinocchio CPI is a follow-on slice).
    #[test]
    fn cpi_pinocchio_non_spl_falls_through_to_none() {
        // A spec whose called interface is NOT the SPL Token program.
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
            try_emit_cpi(call, handler, &spec, Target::Pinocchio).is_none(),
            "Pinocchio generic CPI is unimplemented; must fall through to None"
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
            try_emit_cpi(call, handler, &spec, Target::Anchor).is_none(),
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor)
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor)
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
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
        let rendered = try_emit_cpi(call, handler, &spec, Target::Anchor).expect("should emit");
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

    /// v2.29 Slice C — payload-pre + payload-post cross-variant
    /// promotion emits a destructure preamble that captures the
    /// referenced pre fields as local bindings, followed by the
    /// post-variant assignment that reads those bindings.
    #[test]
    fn cross_variant_promotion_payload_to_payload_emits_destructure() {
        let src = r#"spec Promote
program_id "11111111111111111111111111111111"

type State
  | Open of { x : U64, y : U64 }
  | Closed of { y : U64 }

type Error
  | WrongState

handler close : State.Open -> State.Closed {
  accounts {
    authority : signer
    state_acct : writable
  }
  effect {
    state := .Closed { y := state.x }
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "close")
            .expect("close handler");
        let acct = spec.account_types.first().expect("state account type");
        let post_variant = acct
            .variants
            .iter()
            .find(|v| v.name == "Closed")
            .expect("Closed variant");
        let body = emit_cross_variant_promotion(
            handler,
            &spec,
            "state_acct",
            "Open",
            post_variant,
            "PromoteAccountInner",
            "PromoteError",
        )
        .expect("payload->payload promotion should lower in v2.29");
        // The destructure should bind `x` (referenced by `y := state.x`),
        // ignore `y` (post writes it from `x`, doesn't read pre's y),
        // and capture via `.clone()` so non-Copy variant fields work.
        assert!(
            body.contains("let x = match &self.state_acct.inner"),
            "expected destructure preamble binding `x`; got:\n{body}"
        );
        assert!(
            body.contains("PromoteAccountInner::Open { x, .. } => x.clone()"),
            "expected single-binding match arm with .clone(); got:\n{body}"
        );
        assert!(
            body.contains("return Err(PromoteError::WrongState.into())"),
            "expected WrongState guard on the no-match arm; got:\n{body}"
        );
        assert!(
            body.contains("self.state_acct.inner = PromoteAccountInner::Closed {"),
            "expected the post-variant assignment; got:\n{body}"
        );
        assert!(
            body.contains("y: x,"),
            "expected `y: x,` referencing the destructured local; got:\n{body}"
        );
    }

    /// v2.29 Slice H — when a spec's `imported_namespaces` carries an
    /// account type, codegen emits `src/imported/<ns>.rs` with the
    /// mirrored struct plus a `src/imported/mod.rs` re-exporter, and
    /// `src/lib.rs` declares `pub mod imported;`. Bundled-stub-only
    /// imports leave the map empty and the mirror dir is never
    /// created.
    #[test]
    fn imported_namespace_emits_local_mirror() {
        let mut spec = ParsedSpec {
            program_name: "ConsumerProgram".into(),
            ..ParsedSpec::default()
        };
        spec.account_types.push(ParsedAccountType {
            name: "Consumer".into(),
            fields: vec![("balance".into(), "U64".into())],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        });
        // Inject an imported namespace by hand (the resolver path is
        // exercised by check.rs tests; this test focuses on the
        // codegen-side mirror emission).
        let mut imported = ImportedNamespace {
            dep_key: "foreign_dep".into(),
            account_types: vec![ParsedAccountType {
                name: "ForeignState".into(),
                fields: vec![
                    ("admin".into(), "Pubkey".into()),
                    ("counter".into(), "U64".into()),
                ],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![],
            }],
            records: vec![],
        };
        let _ = &mut imported;
        spec.imported_namespaces.insert("Foreign".into(), imported);

        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(out_dir.join("src")).unwrap();

        generate_imported_mirror(&spec, &fp, &out_dir, Target::Anchor)
            .expect("imported mirror generation should succeed");

        let ns_file = out_dir.join("src/imported/Foreign.rs");
        let body =
            std::fs::read_to_string(&ns_file).expect("namespace mirror file should be written");
        assert!(
            body.contains("pub struct ForeignState"),
            "expected `ForeignState` mirror struct; got:\n{body}"
        );
        assert!(
            body.contains("pub admin: Pubkey,"),
            "expected `admin: Pubkey` field; got:\n{body}"
        );
        assert!(
            body.contains("#[account]"),
            "expected `#[account]` attr (Anchor target); got:\n{body}"
        );

        let mod_file = out_dir.join("src/imported/mod.rs");
        let mod_body =
            std::fs::read_to_string(&mod_file).expect("imported mod.rs should be written");
        assert!(
            mod_body.contains("pub mod Foreign;"),
            "expected `pub mod Foreign;` re-export; got:\n{mod_body}"
        );
    }

    /// v2.29 Slice H — multi-variant imported account types lower to
    /// the wrapper-struct + inner-enum shape and emit accessor
    /// methods on the inner enum (mirrors `generate_state`'s Slice B
    /// accessor work).
    #[test]
    fn imported_multi_variant_namespace_emits_accessors() {
        let mut spec = ParsedSpec {
            program_name: "Consumer".into(),
            ..ParsedSpec::default()
        };
        spec.account_types.push(ParsedAccountType {
            name: "Local".into(),
            fields: vec![("x".into(), "U64".into())],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        });
        let imported = ImportedNamespace {
            dep_key: "amm_dep".into(),
            account_types: vec![ParsedAccountType {
                name: "Pool".into(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Open".into(),
                        fields: vec![
                            ("admin".into(), "Pubkey".into()),
                            ("balance".into(), "U64".into()),
                        ],
                    },
                    ParsedVariant {
                        name: "Closed".into(),
                        fields: vec![("admin".into(), "Pubkey".into())],
                    },
                ],
            }],
            records: vec![],
        };
        spec.imported_namespaces.insert("AMM".into(), imported);

        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(out_dir.join("src")).unwrap();

        generate_imported_mirror(&spec, &fp, &out_dir, Target::Anchor)
            .expect("imported mirror generation should succeed");

        let body = std::fs::read_to_string(out_dir.join("src/imported/AMM.rs"))
            .expect("AMM mirror file should be written");
        assert!(
            body.contains("pub struct Pool"),
            "expected wrapper struct; got:\n{body}"
        );
        assert!(
            body.contains("pub inner: PoolInner,"),
            "expected `inner: PoolInner` field; got:\n{body}"
        );
        assert!(
            body.contains("pub enum PoolInner"),
            "expected inner enum; got:\n{body}"
        );
        // `admin` exists in both variants — accessor emitted, no
        // panic arm because the match exhausts.
        assert!(
            body.contains("pub fn admin(&self) -> &Pubkey"),
            "expected `admin` accessor; got:\n{body}"
        );
        // `balance` only in Open — accessor emits with a panic arm.
        assert!(
            body.contains("pub fn balance(&self) -> &u64"),
            "expected `balance` accessor; got:\n{body}"
        );
        assert!(
            body.contains("PoolInner::balance() called on a variant without `balance`"),
            "expected panic message for missing variant; got:\n{body}"
        );
    }

    /// v2.29.2 — `rewrite_state_refs_for_self` must work on handlers
    /// whose accounts block has multiple writable candidates and no
    /// PDA / `on_account` disambiguator (real-world specs frequently
    /// mark the state account `readonly` in handlers that only read
    /// it, so the per-handler resolver returns None). The spec-wide
    /// canonical-state heuristic picks the account name that's
    /// writable in the most other handlers and uses it as the binder
    /// target, even when this handler declares it `readonly`.
    #[test]
    fn rewrite_state_refs_uses_canonical_fallback_when_handler_state_acct_is_readonly() {
        const SRC: &str = r#"
spec Pool

type State
  | Active of {
      balance : U64,
    }

type Error | E

handler init (initial : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    user        : signer
    pool_config : writable
  }
  effect { balance := initial }
}

handler read_via_writable_decoy (amt : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    user        : signer
    pool_config : readonly
    decoy_a     : writable
    decoy_b     : writable
  }
  requires amt <= state.balance else E
  effect { balance := balance }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(SRC).expect("fixture should parse");
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "read_via_writable_decoy")
            .expect("handler not found");
        // `find_state_account` alone returns None for this handler (two
        // writable candidates: decoy_a, decoy_b — neither is the state
        // account; pool_config is readonly so it's only included in the
        // require_writable=false fallback, which then yields multiple
        // candidates too).
        assert!(
            find_state_account(handler).is_none(),
            "pre-condition: this fixture must surface the canonical-fallback path"
        );
        // The canonical fallback picks `pool_config` (writable in
        // `init`) and the rewriter routes `s.balance` through
        // `self.pool_config.balance`.
        let rewritten = rewrite_state_refs_for_self("s.balance + 1", handler, &spec);
        assert_eq!(
            rewritten, "self.pool_config.balance + 1",
            "v2.29.2 canonical-fallback rewrite must produce \
             `self.pool_config.balance` even when pool_config is \
             readonly in this handler; got: `{rewritten}`"
        );
    }
}
