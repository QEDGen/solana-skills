use anyhow::{Context, Result};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Whole-word match: boundaries are start/end of string or any non-alphanumeric, non-underscore byte.
fn contains_word(haystack: &str, needle: &str) -> bool {
    for (i, _) in haystack.match_indices(needle) {
        let before_ok = i == 0 || {
            let b = haystack.as_bytes()[i - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after = i + needle.len();
        let after_ok = after >= haystack.len() || {
            let b = haystack.as_bytes()[after];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

#[derive(Debug)]
pub struct PropertyStatus {
    pub name: String,
    pub status: Status,
    /// From doc: clause or auto-generated.
    pub intent: Option<String>,
    pub suggestion: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Proven,
    Sorry,
    Missing,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Proven => write!(f, "proven"),
            Status::Sorry => write!(f, "sorry"),
            Status::Missing => write!(f, "missing"),
        }
    }
}

/// Named account type. Single-account specs have one matching the program name;
/// otherwise each `account` block produces one.
#[derive(Debug, Clone)]
pub struct ParsedAccountType {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub lifecycle: Vec<String>,
    /// Reference to a PDA name (if this account is PDA-derived)
    pub pda_ref: Option<String>,
    /// Multi-variant ADT state; empty for single-record account types. When
    /// non-empty, codegen emits a real `pub enum` instead of the flattened
    /// struct; `fields` stays the union of variant fields (back-compat view).
    #[allow(dead_code)] // consumed by S5b codegen pass, not yet wired
    pub variants: Vec<ParsedVariant>,
}

/// Plain record type (`type T = { field : Type, ... }`); value type of `Map[N] T` fields.
#[derive(Debug, Clone)]
pub struct ParsedRecordType {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

/// Sum type with named variants (`type Account | Inactive | Active of { ... }`).
/// Lean codegen emits an `inductive` plus a `structure` per payload-carrying variant.
#[derive(Debug, Clone)]
pub struct ParsedSumType {
    pub name: String,
    pub variants: Vec<ParsedVariant>,
}

#[derive(Debug, Clone)]
pub struct ParsedVariant {
    pub name: String,
    /// Empty for no-payload variants like `| Inactive`.
    pub fields: Vec<(String, String)>,
}

/// Parsed aborts_if clause: condition → error name.
#[derive(Debug, Clone)]
pub struct ParsedAbort {
    pub lean_expr: String,
    pub rust_expr: String,
    /// Pod-aware Rust expression for Quasar (`.get()` postfix, `as i128` casts);
    /// codegen picks between this and `rust_expr` by `Target`.
    pub rust_expr_pod: String,
    pub error_name: String,
}

/// Parsed requires clause. When `error_name` is Some, generates both a guard
/// (positive form in transition) and an abort theorem (negated form).
#[derive(Debug, Clone)]
pub struct ParsedRequires {
    pub lean_expr: String,
    pub rust_expr: String,
    pub rust_expr_pod: String,
    pub error_name: Option<String>,
    /// Source AST body for AST-level lints (e.g. `old_in_single_state_context`).
    /// `None` for synthetic requires from `match`-arm desugaring.
    pub ast_body: Option<crate::ast::Node<crate::ast::Expr>>,
}

/// Parsed ensures clause: post-condition relating pre and post state.
/// In lean_expr, `old(state.x)` is rendered as `s.x` (pre-state) and
/// `state.x` as `s'.x` (post-state).
#[derive(Debug, Clone)]
pub struct ParsedEnsures {
    pub lean_expr: String,
    #[allow(dead_code)]
    pub rust_expr: String,
    #[allow(dead_code)]
    pub rust_expr_pod: String,
    /// Binary-mode rendering (`state.x` → `post.x`, `old(state.x)` → `pre.x`)
    /// for the ensures-preservation Kani harness; `rust_expr` flattens both to
    /// `s.x`, losing the pre/post distinction.
    #[allow(dead_code)]
    pub rust_expr_binary: String,
}

/// Parsed cover block (reachability).
#[derive(Debug, Clone)]
pub struct ParsedCover {
    pub name: String,
    pub traces: Vec<Vec<String>>,
    pub reachable: Vec<(String, Option<String>)>, // (op, when_lean_expr)
}

/// Parsed liveness block (leads-to).
#[derive(Debug, Clone)]
pub struct ParsedLiveness {
    pub name: String,
    pub from_state: String,
    pub leads_to_state: String,
    pub via_ops: Vec<String>,
    pub within_steps: Option<u64>,
}

/// Top-level invariant declaration.
///
/// Two forms:
/// - **Expression body** (`invariant <name> : <expr>`): codegen emits a real
///   theorem / harness; `lean_expr` and `rust_expr` populated.
/// - **Description-only** (`invariant <name> "<doc>"`): no predicate body;
///   codegen emits a structured comment, never `theorem foo : True := trivial`.
///   Flagged P3 by the `bare_invariant` lint.
#[derive(Debug, Clone)]
pub struct ParsedInvariant {
    pub name: String,
    /// May be empty when only an expression body was declared.
    pub doc: String,
    /// Lean form of the predicate. `None` for description-only.
    pub lean_expr: Option<String>,
    /// Rust form of the predicate. `None` for description-only.
    #[allow(dead_code)]
    pub rust_expr: Option<String>,
    /// Source AST body for the `old_in_single_state_context` lint.
    /// `None` for the description-only form.
    pub ast_body: Option<crate::ast::Node<crate::ast::Expr>>,
}

/// Parsed environment block (external state).
#[derive(Debug, Clone)]
pub struct ParsedEnvironment {
    pub name: String,
    pub mutates: Vec<(String, String)>, // (field, type)
    pub constraints: Vec<String>,       // lean form
    pub constraints_rust: Vec<String>,  // rust form
}

/// Parsed operation from a qedspec block.
///
/// Fields are shared across backends (kani/proptest/lean/codegen) to avoid
/// re-parsing; struct-level `allow(dead_code)` covers fields not all
/// feature sets touch.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedOperation {
    pub name: String,
    pub doc: Option<String>,
    pub who: Option<String>,
    /// Which account type this operation targets (from `on` clause).
    /// None means the default (first/only) account.
    pub on_account: Option<String>,
    pub has_when: bool,
    pub pre_status: Option<String>,
    pub post_status: Option<String>,
    pub has_calls: bool,
    pub program_id: Option<String>,
    pub has_u64_fields: bool,
    pub has_takes: bool,
    pub has_guard: bool,
    pub guard_str: Option<String>,
    pub has_effect: bool,
    pub takes_params: Vec<(String, String)>,
    pub effects: Vec<(String, String, String)>,
    /// Per-site `or <ErrorVariant>` overrides, parallel to `effects`
    /// (`effect_on_error[i]` overrides `effects[i]`). `None` without an
    /// explicit `or`, and for saturating / wrapping / `Set` effects where
    /// overrides are meaningless.
    pub effect_on_error: Vec<Option<String>>,
    pub calls_accounts: Vec<(String, String)>,
    pub calls_discriminator: Option<String>,
    pub emits: Vec<String>,
    /// Abort conditions: (lean_expr, rust_expr, error_name)
    pub aborts_if: Vec<ParsedAbort>,
}

/// Temporal shape of a property body, computed at parse time: any
/// `Expr::Old(_)` anywhere in the body ⇒ `Binary`, else `Unary`
/// (`expr_contains_old` in `chumsky_adapter.rs`). Drives proptest/kani
/// harness dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyClass {
    /// Single-state predicate: `fn name(s: &State) -> bool`.
    Unary,
    /// Transition predicate: `fn name(pre: &State, post: &State) -> bool`.
    /// Only meaningful at handler boundaries.
    Binary,
}

/// Parsed property from a qedspec block.
#[derive(Debug, Clone)]
pub struct ParsedProperty {
    pub name: String,
    /// Lean-rendered body (for proofs / diagnostics / drift).
    pub expression: Option<String>,
    /// Rust-rendered body, used verbatim by proptest/Kani. Contains
    /// `QEDGEN_UNSUPPORTED_QUANTIFIER` when a forall/exists can't lower to a
    /// bool body; callers skip emission then.
    pub rust_expression: Option<String>,
    /// Pod-aware Rust body for Quasar (mirrors `rust_expr_pod`).
    pub rust_expression_pod: Option<String>,
    pub preserved_by: Vec<String>,
    /// For `forall <binder> : <T>, body` with a binder too wide to exhaust
    /// (U16+, Fin[N>256]): body rendered with the binder free. proptest_gen
    /// emits `fn {prop}_at(s, binder)` and preservation tests check only the
    /// slot the handler was passed — sufficient for inductive preservation
    /// since the frame condition covers untouched slots. The bare `{prop}(&s)`
    /// predicate stays as the "true" stub for prop_assume sites.
    pub per_slot: Option<PerSlotForm>,
    /// Why a quantifier shape can't mechanically lower (nested forall,
    /// exists, unbounded binder, ...); feeds the P5
    /// `unsupported_quantifier_shape` lint. `None` = supported shape.
    pub quantifier_lint: Option<QuantifierLint>,
    pub class: PropertyClass,
    /// AST body for downstream walks (e.g. `vacuous_property_lowering` gates
    /// on `Expr::Old(_)`). `None` only on hand-built test fixtures.
    pub ast_body: Option<crate::ast::Node<crate::ast::Expr>>,
}

/// Per-slot rendering of a `forall <binder> : <T>, body` property; see
/// `ParsedProperty::per_slot`. Native rendering only (proptest_gen).
#[derive(Debug, Clone)]
pub struct PerSlotForm {
    pub binder_name: String,
    pub binder_type: String,
    pub rust_body: String,
}

/// Unsupported-quantifier info recorded by chumsky_adapter for the P5 lint.
/// Mirrors `crate::quantifier::Reason` without depending on its enum (keeps
/// `ParsedProperty` AST-free for test constructors).
#[derive(Debug, Clone)]
pub struct QuantifierLint {
    /// Stable rule discriminant: `nested_quantifier`, `unbounded_binder`,
    /// `exists_quantifier`. Used to key into `docs/limitations.md`.
    pub kind: String,
    /// Copied verbatim into the lint output.
    pub message: String,
    /// Byte range of the offending quantifier in the source spec (span rendering).
    pub span_start: usize,
    pub span_end: usize,
}

/// Sentinel embedded by `chumsky_adapter::expr_to_rust` when a quantifier in a
/// property body has no valid `fn p(&State) -> bool` lowering.
pub const QEDGEN_UNSUPPORTED_MARKER: &str = "QEDGEN_UNSUPPORTED_QUANTIFIER";

/// Does this Rust-rendered expression require harness-level scaffolding?
pub fn rust_expr_is_unsupported(rust_expr: &str) -> bool {
    rust_expr.contains(QEDGEN_UNSUPPORTED_MARKER)
}

/// PDA seed declaration from a qedspec block.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedPda {
    pub name: String,
    pub seeds: Vec<String>,
}

/// Event declaration from a qedspec block.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedEvent {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

/// Account entry within an operation's context: block.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedAccountEntry {
    pub name: String,
    pub account_type: String,
    pub inner_type: Option<String>,
    pub is_mut: bool,
    pub is_init: bool,
    pub is_init_if_needed: bool,
    pub payer: Option<String>,
    pub seeds_ref: Option<String>,
    pub has_bump: bool,
    pub close_target: Option<String>,
    pub has_one: Option<String>,
    pub token_mint: Option<String>,
    pub token_authority: Option<String>,
}

/// Per-operation account context.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedContext {
    pub operation: String,
    pub accounts: Vec<ParsedAccountEntry>,
}

// ============================================================================
// sBPF-specific structures
// ============================================================================

/// Known pubkey as 4-chunk U64 representation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedPubkey {
    pub name: String,
    pub chunks: Vec<String>, // 4 U64 values as strings
}

/// A field in an input/instruction layout with byte offset.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedLayoutField {
    pub name: String,
    pub field_type: String,
    pub offset: i64,
    pub description: Option<String>,
}

/// An sBPF validation guard.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedGuard {
    pub name: String,
    pub doc: Option<String>,
    pub checks: Option<String>, // guard expression (constants resolved to values)
    pub checks_raw: Option<String>, // guard expression (original constant names preserved)
    pub error: String,          // error code name
    pub fuel: Option<u64>,      // sBPF: fuel steps needed for this guard
}

/// An sBPF property (memory safety, data flow, CPI correctness, etc).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedSbpfProperty {
    pub name: String,
    pub doc: Option<String>,
    pub kind: SbpfPropertyKind,
}

/// The different kinds of sBPF properties.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SbpfPropertyKind {
    /// Memory safety — scope over guards or named list
    Scope { targets: Vec<String> },
    /// Data flow — a value derived from seeds or flowing through accounts
    Flow { target: String, kind: FlowKind },
    /// CPI correctness — a cross-program invocation with expected fields
    Cpi {
        program: String,
        instruction: String,
        fields: Vec<(String, String)>,
    },
    /// Happy path — after all guards pass, expect exit code
    HappyPath { exit_code: String },
    /// Generic (has expr + preserved_by, from state-machine properties)
    Generic,
}

/// Sub-kinds of data flow properties.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum FlowKind {
    FromSeeds(Vec<String>),
    Through(Vec<String>),
}

/// A single instruction handler in an sBPF program.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedInstruction {
    pub name: String,
    pub doc: Option<String>,
    pub discriminant: Option<String>,
    pub entry: Option<u64>,
    pub constants: Vec<(String, String)>,
    pub errors: Vec<ParsedErrorCode>,
    pub input_layout: Vec<ParsedLayoutField>,
    pub insn_layout: Vec<ParsedLayoutField>,
    pub guards: Vec<ParsedGuard>,
    pub properties: Vec<ParsedSbpfProperty>,
}

/// Error code with optional numeric value and description.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedErrorCode {
    pub name: String,
    pub value: Option<u64>,
    pub description: Option<String>,
}

// ============================================================================
// Unified handler types (v3 — target-agnostic)
// ============================================================================

/// A unified handler — replaces both ParsedOperation (Quasar) and
/// ParsedInstruction (sBPF). Represents any callable entry point with
/// guards, effects, accounts, and properties.
#[derive(Debug, Clone)]
pub struct ParsedHandler {
    pub name: String,
    pub doc: Option<String>,
    /// Who can invoke this handler (access control actor).
    pub who: Option<String>,
    /// Which account type this handler targets (multi-account specs).
    pub on_account: Option<String>,
    /// Pre-state lifecycle requirement.
    pub pre_status: Option<String>,
    /// Post-state lifecycle transition.
    pub post_status: Option<String>,
    pub takes_params: Vec<(String, String)>,
    /// Legacy guard expression (Lean form). Deprecated: use `requires` instead.
    pub guard_str: Option<String>,
    /// Legacy guard expression (Rust form). Deprecated: use `requires` instead.
    #[allow(dead_code)]
    pub guard_str_rust: Option<String>,
    /// Legacy abort conditions. Deprecated: use `requires ... else` instead.
    pub aborts_if: Vec<ParsedAbort>,
    /// Requires clauses: guard + optional abort. When error_name is Some,
    /// generates both transition guard and abort theorem.
    pub requires: Vec<ParsedRequires>,
    /// Post-conditions (ensures clauses). Uses s' for post-state, s for old().
    pub ensures: Vec<ParsedEnsures>,
    /// Frame condition: fields that may be modified. All others must stay unchanged.
    pub modifies: Option<Vec<String>>,
    /// Handler-level let bindings: (name, lean_expr, rust_expr).
    pub let_bindings: Vec<(String, String, String)>,
    /// All abort conditions are exhaustive — generates ↔ theorem instead of per-abort.
    pub aborts_total: bool,
    /// Deliberately permissionless — no `auth` required. Mutually exclusive
    /// with `who` (check.rs rejects both); opts out of the `no_access_control` P1 lint.
    pub permissionless: bool,
    /// State effects: (field, op, value) where op is
    /// "set" | "add" | "add_sat" | "add_wrap" | "sub" | "sub_sat" | "sub_wrap".
    /// "add"/"sub" are the checked defaults; `_sat` / `_wrap` carry the
    /// explicit opt-in from `+=!` / `+=?`.
    pub effects: Vec<(String, String, String)>,
    /// Per-site `or <ErrorVariant>` overrides, parallel to `effects`.
    /// See `ParsedOperation::effect_on_error`.
    pub effect_on_error: Vec<Option<String>>,
    /// IDL-level account descriptors.
    pub accounts: Vec<ParsedHandlerAccount>,
    /// Token transfer intents.
    pub transfers: Vec<ParsedTransfer>,
    pub emits: Vec<String>,
    /// Names of invariants this handler must preserve.
    pub invariants: Vec<String>,
    /// Invariants this handler ESTABLISHES at post-state without requiring
    /// them as a precondition (init / one-shot handlers). Codegen asserts
    /// them post-state but skips the `kani::assume` / `prop_assume!` pre-state guard.
    pub establishes: Vec<String>,
    /// Names of `include <schema>` clauses. The adapter's post-pass appends
    /// each schema's `requires` onto `self.requires`; stored (not expanded
    /// inline) so synthetic match-arm handlers inherit the same expansion.
    pub schema_includes: Vec<String>,
    /// Per-handler properties (from inline property/invariant clauses).
    pub properties: Vec<String>,
    /// `call Interface.handler(name = expr, ...)` sites resolved against a
    /// top-level `interface` block. Empty for handlers that don't CPI.
    #[allow(dead_code)]
    pub calls: Vec<ParsedCall>,
    /// Conditional-effect tree: `Some` when the spec uses `match` inside
    /// `effect { … }`. The flat `effects` field still holds the union of
    /// every arm's effects (back-compat); this carries arm grouping.
    pub effect_branches: Option<ParsedEffectBranches>,
    /// `abstract <name> : <Type>` clauses as `(name, dsl_type_string)`; the
    /// DSL type stays verbatim so each backend resolves its own concrete
    /// type. Kani: `kani::any()` + assume; proptest: `any::<T>()` +
    /// prop_assume; Lean: `∃ <name> : T,` wrapper; Rust scaffold:
    /// `let <name>: T = todo!(...)` for the agent to fill.
    pub abstract_binders: Vec<(String, String)>,
}

/// IR form of a top-level `match` block inside `effect { … }`.
#[derive(Debug, Clone)]
pub struct ParsedEffectBranches {
    /// Scrutinee expression rendered for Rust codegen.
    pub scrutinee_rust: String,
    /// Scrutinee rendered for Quasar/Pod targets; `emit_transition_fn`
    /// currently reads `scrutinee_rust`.
    #[allow(dead_code)]
    pub scrutinee_rust_pod: String,
    /// Scrutinee expression rendered for Lean.
    pub scrutinee_lean: String,
    pub arms: Vec<ParsedEffectArm>,
}

/// One arm of a `ParsedEffectBranches`.
#[derive(Debug, Clone)]
pub struct ParsedEffectArm {
    pub pattern_rust: String,
    pub pattern_lean: String,
    /// `true` for a wildcard arm.
    pub is_wildcard: bool,
    pub effects: Vec<(String, String, String)>,
    /// Per-site `or <ErrorVariant>` overrides, parallel to `effects` (see
    /// `ParsedOperation::effect_on_error`). No consumer reads it at the arm
    /// level yet — Anchor codegen reads the flat union, proptest/kani don't
    /// lower error variants.
    #[allow(dead_code)]
    pub effect_on_error: Vec<Option<String>>,
}

/// A resolved `call Target.handler(...)` site inside a handler body. The
/// target is split into interface + handler name for easier lookup; args
/// carry both Lean and Rust renderings so backends can pick their form.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedCall {
    pub target_interface: String,
    pub target_handler: String,
    pub args: Vec<ParsedCallArg>,
    /// Set for `let <name> = call …`: backends bind the callee's return
    /// value to this identifier. Tier-1/2 interfaces with a declared return
    /// type drive the Rust/Lean shape; Tier-0 falls back to an opaque placeholder.
    pub result_binding: Option<String>,
    /// `state_binders { callee_field = state.X, ... }` entries. Each binder:
    /// (1) adds an accessor param `(<callee_field> : State → Nat)` to the Lean
    /// axiom, applied with `(·.<caller_field>)`; (2) rewrites
    /// `pre/post.<callee_field>` → `pre/post.<caller_field>` in the Kani
    /// harness before `rewrite_pre_post_paths` flattens. Empty preserves the
    /// callee-frame, param-only axiom shape.
    pub state_binders: Vec<ParsedStateBinder>,
}

#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedCallArg {
    pub name: String,
    pub lean_expr: String,
    pub rust_expr: String,
    pub rust_expr_pod: String,
}

/// One entry in a `call X.y(state_binders { ... })` block: maps a callee-side
/// abstract field name to a caller-side State field.
///
/// The binder RHS must be a `state.<ident>` path — the adapter validates the
/// shape and extracts the trailing identifier; richer RHS forms are reserved
/// for v3.0. Substitution helpers synthesize `pre.` / `post.` prefixes and
/// Lean `(·.<caller_field>)` at use sites.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedStateBinder {
    /// Callee abstract field name, matched verbatim (word-boundary) in the
    /// callee's `ensures` text.
    pub callee_field: String,
    /// Caller-side bare field name (trailing ident from `state.<ident>`).
    pub caller_field: String,
}

impl ParsedHandler {
    pub fn has_guard(&self) -> bool {
        self.guard_str.is_some() || !self.requires.is_empty()
    }
    pub fn has_effect(&self) -> bool {
        !self.effects.is_empty()
    }
    /// True if the handler has a `transfers { }` block (legacy sugar for
    /// `call Token.transfer(...)`) or any `call Interface.handler(...)` site.
    pub fn has_calls(&self) -> bool {
        !self.transfers.is_empty() || !self.calls.is_empty()
    }

    /// Find the first signer account in this handler.
    pub fn signer_account(&self) -> Option<&ParsedHandlerAccount> {
        self.accounts.iter().find(|a| a.is_signer)
    }
    /// Check if any account has a token type.
    pub fn has_token_accounts(&self) -> bool {
        self.accounts
            .iter()
            .any(|a| a.account_type.as_deref() == Some("token"))
    }
    /// Check if any account has a token program.
    pub fn has_token_program(&self) -> bool {
        self.accounts
            .iter()
            .any(|a| a.is_program && a.account_type.as_deref() == Some("token"))
            || self
                .accounts
                .iter()
                .any(|a| a.name.contains("token_program"))
    }
    /// Check if any account has bumps (PDA seeds).
    pub fn has_bumps(&self) -> bool {
        self.accounts.iter().any(|a| a.pda_seeds.is_some())
    }
}

/// True iff the spec is a multi-variant ADT, the field lives inside a variant
/// payload (not on the wrapper), and the spec opted into wrapper-struct +
/// inner-enum codegen (ADT state repr).
///
/// Used by R25's `auth X → has_one = X` lowering and `emit_variant_auth_guard`
/// to decide whether the auth field is reachable from the Anchor wrapper. On
/// the flat-struct path every field sits directly on the wrapper, so `has_one`
/// works and a variant-destructure guard would reference a non-existent `inner` enum.
pub fn is_multi_variant_adt_with_field_in_variant(spec: &ParsedSpec, field: &str) -> bool {
    let Some(acct) = spec.account_types.first() else {
        return false;
    };
    if acct.variants.len() <= 1 {
        return false;
    }
    if !spec.state_repr_is_adt() {
        return false;
    }
    acct.variants
        .iter()
        .any(|v| v.fields.iter().any(|(n, _)| n == field))
}

/// True if the state struct backing this handler-account has `field`.
/// Multi-state specs walk `spec.account_types`; single-state specs use the
/// union in `spec.state_fields`. Used by R25's `auth X` → `has_one = X` lowering.
fn state_account_has_field(acct: &ParsedHandlerAccount, spec: &ParsedSpec, field: &str) -> bool {
    // Multi-state: match account name → ADT name (lowercase).
    for at in &spec.account_types {
        let lower = at.name.to_lowercase();
        if acct.name == lower || acct.name.starts_with(&lower) {
            return at.fields.iter().any(|(n, _)| n == field);
        }
    }
    // Single-state spec — fields union lives on the spec.
    spec.state_fields.iter().any(|(n, _)| n == field)
}

impl ParsedHandlerAccount {
    /// Generate the #[account(...)] attribute for codegen, target-aware.
    ///
    /// Anchor and Quasar both spell the attribute `#[account(...)]` but
    /// disagree on:
    ///
    /// - **Pubkey accessor**: Anchor uses `<acct>.key()`; Quasar uses
    ///   `<acct>.address()`. Quasar's `#[account]` macro also auto-handles
    ///   bare-ident seeds matching field names (expanding to
    ///   `<ident>.to_account_view().address().as_ref()`), so Quasar bare
    ///   idents are preferred over `.key().as_ref()`.
    /// - **State-field seeds in non-init handlers**: Anchor's macro evaluates
    ///   `<pda>.<field>.as_ref()` in a scope where `<pda>` is bound to the
    ///   parsed account. Quasar re-uses the same expression in a `Bumps::seeds()`
    ///   method where only `self` is in scope, so `vault.creator.as_ref()`
    ///   fails with E0425. For Quasar we omit the `seeds = [...]` directive
    ///   entirely on non-init handlers when seeds reference state fields —
    ///   `Account<T>`'s owner+discriminator check still protects type
    ///   confusion. Anchor keeps the original behavior.
    pub fn quasar_account_attr(
        &self,
        handler: &ParsedHandler,
        state_name: &str,
        target: crate::Target,
        spec: &ParsedSpec,
        is_state_account: bool,
    ) -> String {
        let _ = state_name;
        let mut parts = Vec::new();

        // Infer init from lifecycle. In multi-state specs only the account
        // matching the handler's `on_account` is init'd — sibling writable
        // PDAs in the same handler are pre-existing.
        let lifecycle_is_init = handler.pre_status.as_deref() == Some("Uninitialized")
            || handler.pre_status.as_deref() == Some("Empty");
        let on_account_matches = match handler.on_account.as_deref() {
            // Multi-state: only the named state account init's.
            Some(adt_name) => {
                let lower = adt_name.to_lowercase();
                self.name == lower || self.name.starts_with(&lower)
            }
            // Single-state spec: any writable PDA can be the init target.
            None => true,
        };
        let is_init =
            lifecycle_is_init && on_account_matches && !self.is_signer && self.pda_seeds.is_some();

        // `mut` is mutually exclusive with `init` in Anchor (init implies
        // mut) — emitting both trips `mut cannot be provided with init`.
        if self.is_writable && !is_init {
            parts.push("mut".to_string());
        }

        if is_init {
            parts.push("init".to_string());
            if let Some(signer) = handler.signer_account() {
                parts.push(format!("payer = {}", signer.name));
            }
            // Anchor requires `space = <bytes>` with `init`. We derive
            // `InitSpace` on every account type / inner enum / record, so
            // the canonical form is `space = 8 + <AccountStruct>::INIT_SPACE`
            // (8 = Anchor discriminator). The struct name must match what
            // `generate_state` emits.
            let space_target = match (target, handler.on_account.as_deref()) {
                // Multi-state spec: per-handler `on_account` names
                // the ADT being driven. The wrapper struct is
                // `<Name>Account`.
                (_, Some(adt_name)) => format!("{}Account", adt_name),
                // Single-state spec on Anchor: the wrapper is
                // `<Program>Account` (matches `generate_state`'s
                // non-multi branch).
                (crate::Target::Anchor, None) => format!(
                    "{}Account",
                    crate::codegen_shared::to_pascal_case(&spec.program_name)
                ),
                // Quasar handles space differently — its `init`
                // analogue takes size from the typed `Account<T>`
                // wrapper. Skip the `space` attribute on Quasar.
                _ => String::new(),
            };
            if !space_target.is_empty() {
                parts.push(format!("space = 8 + {}::INIT_SPACE", space_target));
            }
        }

        if let Some(ref seeds) = self.pda_seeds {
            let bound_account_names: std::collections::HashSet<&str> =
                handler.accounts.iter().map(|a| a.name.as_str()).collect();

            // Detect the case-3 (state-field) seeds. For Quasar non-init
            // handlers these don't survive the `Bumps::<acct>_seeds(self)`
            // method generation because `self.<seed>` isn't auto-captured —
            // omit `seeds`/`bump` on the per-handler attribute and rely on
            // owner+discriminator from `Account<T>`.
            let needs_state_field_seed = seeds.iter().any(|seed| {
                let is_literal = seed.starts_with('"') && seed.ends_with('"');
                !is_literal && !bound_account_names.contains(seed.as_str())
            });

            // v2.29 — extend the suppress to Anchor too when the
            // seed references a field that lives in a variant payload
            // of a multi-variant ADT. Anchor's `#[account(seeds =
            // […])]` macro requires syntactic field access; the
            // accessor `inner.<field>()` we emit for multi-variant
            // ADTs returns a `&Pubkey` via a method call which the
            // macro can't parse. Drop the macro-side `seeds = [...]`
            // for those accounts; the generic-guards.rs R28 pass
            // (below) emits a runtime PDA check that uses the
            // accessor directly.
            let anchor_variant_field_seed = matches!(target, crate::Target::Anchor)
                && !is_init
                && needs_state_field_seed
                && crate::codegen_shared::is_multi_variant_adt_state_pub(spec)
                && seeds.iter().any(|seed| {
                    let is_literal = seed.starts_with('"') && seed.ends_with('"');
                    if is_literal || bound_account_names.contains(seed.as_str()) {
                        return false;
                    }
                    // Is this a variant-payload field?
                    spec.account_types.iter().any(|a| {
                        a.variants
                            .iter()
                            .any(|v| v.fields.iter().any(|(n, _)| n == seed))
                    })
                });
            let suppress_seeds =
                (matches!(target, crate::Target::Quasar) && !is_init && needs_state_field_seed)
                    || anchor_variant_field_seed;

            if !suppress_seeds {
                let seed_parts: Vec<String> = seeds
                    .iter()
                    .map(|seed| {
                        if let Some(inner) =
                            seed.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                        {
                            format!("b\"{}\"", inner)
                        } else if bound_account_names.contains(seed.as_str()) {
                            // Quasar auto-handles bare idents matching field
                            // names; Anchor needs the explicit `.key().as_ref()`
                            // call.
                            match target {
                                crate::Target::Quasar => seed.clone(),
                                _ => format!("{}.key().as_ref()", seed),
                            }
                        } else {
                            // State-field seed (only reached on Anchor or on
                            // init handlers — non-init Quasar suppresses the
                            // whole seeds directive above).
                            format!("{}.{}.as_ref()", self.name, seed)
                        }
                    })
                    .collect();
                parts.push(format!("seeds = [{}]", seed_parts.join(", ")));
                parts.push("bump".to_string());
            }
        }

        // `token::authority = X` is only valid on accounts that are also
        // `init` / `init_if_needed` — quasar (and anchor) reject it on
        // already-existing accounts. The spec authority annotation
        // captures "this token account should belong to this authority";
        // for non-init accounts that's already enforced at init time and
        // doesn't need re-emission. For init accounts we emit it so the
        // macro can wire up the SPL InitToken CPI correctly.
        if is_init {
            if let Some(ref auth) = self.authority {
                parts.push(format!("token::authority = {}", auth));
            }
        }

        // R25: lower `auth X` to `has_one = X` when the state-bearing
        // account has a field named X. Without this binding, every handler
        // taking an authority signer is reachable by ANY signer — the
        // signer check verifies "someone signed", not "the right someone".
        // Anchor and Quasar both accept `has_one = field`.
        //
        // With multi-variant ADT state the auth field often lives in a
        // variant payload (`Active.owner`); Anchor's `has_one` macro can't
        // reach into the inner enum ("no field `owner` on `Account<…>`").
        // Skip emission there — the auth gap surfaces via a TODO line next
        // to the handler body rather than being dropped silently.
        if is_state_account {
            if let Some(ref who) = handler.who {
                if state_account_has_field(self, spec, who) {
                    // Suppress only on Anchor: its wrapper-struct +
                    // inner-enum emission hides variant-payload fields from
                    // `has_one`; Quasar's flat-struct emission keeps every
                    // field at top level so `has_one = field` works.
                    let suppress_for_anchor_variant = matches!(target, crate::Target::Anchor)
                        && is_multi_variant_adt_with_field_in_variant(spec, who);
                    if !suppress_for_anchor_variant {
                        parts.push(format!("has_one = {}", who));
                    }
                }
            }
        }

        if parts.is_empty() {
            String::new()
        } else {
            format!("    #[account({})]\n", parts.join(", "))
        }
    }
}

/// An account descriptor within a handler's `accounts` block.
/// IDL-level: no framework-specific annotations.
#[derive(Debug, Clone, Default)]
pub struct ParsedHandlerAccount {
    pub name: String,
    pub is_signer: bool,
    pub is_writable: bool,
    pub is_program: bool,
    /// PDA seeds if this account is program-derived.
    pub pda_seeds: Option<Vec<String>>,
    /// Account type constraint (e.g., "token").
    pub account_type: Option<String>,
    /// Authority constraint (e.g., "escrow").
    pub authority: Option<String>,
    /// Hardcoded base58 pubkey when the account has a fixed default
    /// (Codama `publicKeyValueNode`: system_program, the program itself,
    /// event authority, etc.). Lets brownfield codegen emit
    /// `solana_pubkey::pubkey!("...")` for these instead of generating a
    /// keypair the fuzzer would have to populate.
    pub default_pubkey: Option<String>,

    /// Set when `account_type` resolves to a type from an imported spec;
    /// carries the namespace alias (key into `ParsedSpec::imported_namespaces`).
    /// Anchor codegen lowers to `Account<'info, imported::<ns>::<type>>` and
    /// routes field reads through the local mirror at `src/imported/<ns>.rs`.
    pub imported_namespace: Option<String>,
}

/// A token transfer intent within a handler's `transfers` block —
/// declarative sugar over `call Token.transfer(...)`. The dual storage
/// (`transfers` + `calls`) is backward-compat; v3.0 collapses to
/// `ParsedCall` only (the keyword stays as parse-time sugar).
#[derive(Debug, Clone)]
pub struct ParsedTransfer {
    pub from: String,
    pub to: String,
    pub amount: Option<String>,
    pub authority: Option<String>,
}

/// Full parsed spec context.
#[derive(Debug, Default, Clone)]
pub struct ParsedSpec {
    /// Unified handlers from handler/operation/instruction blocks.
    pub handlers: Vec<ParsedHandler>,

    // Legacy fields — populated by forward bridge for backward compat.
    #[allow(dead_code)]
    pub operations: Vec<ParsedOperation>,
    pub invariants: Vec<ParsedInvariant>,
    pub properties: Vec<ParsedProperty>,
    #[allow(dead_code)]
    pub has_u64_fields: bool,
    #[allow(dead_code)]
    pub u64_field_names: Vec<String>,
    #[allow(dead_code)]
    pub program_id: Option<String>,
    #[allow(dead_code)]
    pub program_name: String,
    /// Flat union of state fields across account types (single-account: the
    /// account's fields; multi-account: the primary account's).
    #[allow(dead_code)]
    pub state_fields: Vec<(String, String)>,
    /// Flat lifecycle states (union across all account types for backward compat).
    #[allow(dead_code)]
    pub lifecycle_states: Vec<String>,
    #[allow(dead_code)]
    pub pdas: Vec<ParsedPda>,
    #[allow(dead_code)]
    pub events: Vec<ParsedEvent>,
    #[allow(dead_code)]
    pub error_codes: Vec<String>,
    #[allow(dead_code)]
    pub contexts: Vec<ParsedContext>,
    /// Named account types with per-account fields and lifecycle.
    /// Empty for single-account specs that use bare `state {}`.
    pub account_types: Vec<ParsedAccountType>,

    /// Plain record types declared with `type T = { ... }`.
    /// Used as value types of Map fields and for structured state entries.
    pub records: Vec<ParsedRecordType>,

    /// Sum types used as Map-value types (not as handler pre/post states).
    /// These are emitted as proper Lean `inductive` — with one `structure`
    /// per payload-carrying variant — rather than flattened into a single
    /// record with a discriminator field. `type Account | Inactive | Active
    /// of { ... }` referenced from `Map[N] Account` ends up here.
    pub sum_types: Vec<ParsedSumType>,

    /// Known pubkeys as 4-chunk U64 representations.
    #[allow(dead_code)]
    pub pubkeys: Vec<ParsedPubkey>,
    /// Instruction handlers (sBPF mode).
    #[allow(dead_code)]
    pub instructions: Vec<ParsedInstruction>,
    /// Global error codes with values (sBPF `Name = value "desc"` syntax).
    #[allow(dead_code)]
    pub valued_errors: Vec<ParsedErrorCode>,
    /// Global named constants (`const NAME = VALUE`).
    #[allow(dead_code)]
    pub constants: Vec<(String, String)>,
    /// Type aliases: `type AccountIdx = Fin[MAX_ACCOUNTS]` etc.
    /// Stored as (alias_name, rendered_target). Target is `Fin[N]`, `Nat`,
    /// a record name, etc. — whatever `TypeRef` the source points at.
    pub type_aliases: Vec<(String, String)>,
    /// Cover blocks (reachability properties).
    #[allow(dead_code)]
    pub covers: Vec<ParsedCover>,
    /// Liveness properties (leads-to).
    #[allow(dead_code)]
    pub liveness_props: Vec<ParsedLiveness>,
    /// Environment blocks (external state).
    #[allow(dead_code)]
    pub environments: Vec<ParsedEnvironment>,

    /// Callee contracts for CPI (docs/design/spec-composition.md §2).
    /// Tier-0 handlers have no `requires`/`ensures`; Tier-1/Tier-2 do.
    pub interfaces: Vec<ParsedInterface>,

    /// `import Name from "key"` statements; the resolver pairs them with
    /// `qed.toml` to fetch sources and merge their `interface` declarations
    /// into `interfaces` (docs/design/spec-composition.md §3).
    pub imports: Vec<ParsedImport>,

    /// Names of `pragma <name> { ... }` blocks that appeared in the spec.
    /// Used for target inference (`sbpf` → assembly target) and for
    /// platform-scoped feature flags in backends.
    pub pragmas: Vec<String>,

    /// `pragma <key> = <value>` assignments, stored as `(key, value)` so new
    /// keys don't require ParsedSpec edits. Known keys:
    /// `checked_overflow_error` / `checked_underflow_error` — error variant
    /// for checked `+=` / `-=` failure (defaults `MathOverflow` /
    /// `MathUnderflow`). Lookup via `pragma_value(key)`; per-site
    /// `EffectStmt.on_error` still wins.
    pub pragma_assignments: Vec<(String, String)>,

    /// Top-level `schema name { requires … }` blocks — reusable guard
    /// bundles. Handlers reference them via `include <name>`, expanded into
    /// the handler's `requires` at parse time so downstream lints/codegen
    /// see the union as if inlined.
    #[allow(dead_code)]
    pub schemas: Vec<ParsedSchema>,

    /// Helper functions referenced by name but not declared in the spec, as
    /// `(func_name, arg_types_in_lean, return_type)`. Codegen emits an
    /// `axiom` per entry so Lake can typecheck the surrounding expressions
    /// without full semantics. First-encounter wins for the signature.
    pub uninterpreted_helpers: Vec<(String, Vec<String>, String)>,

    /// Top-level `ref_impl name (...) : T = <expr>` declarations, referenced
    /// from `ensures`. Lower to Lean `def`s and inline at Kani assertion
    /// sites. Unlike `uninterpreted_helpers` (axiomatic), these carry an
    /// executable body.
    #[allow(dead_code)]
    pub ref_impls: Vec<ParsedRefImpl>,

    /// `ghost <name> : <Ty> { init {…} on H {…} }` spec-only auxiliary
    /// state. Rendered to verification-State fields (Lean / proptest / Kani)
    /// plus per-handler updates; omitted from on-chain codegen entirely.
    pub ghosts: Vec<ParsedGhost>,

    /// `hook <kind> { assert … }` cross-cutting assertions. Enforced in
    /// Kani / proptest transitions at the matching MIR-statement boundary;
    /// ignored by Lean (deferred) and on-chain codegen.
    pub hooks: Vec<ParsedHook>,

    /// Verified-callee composition (Stance 2): maps each imported interface
    /// whose provider shipped a Lake-buildable proof package
    /// (`<source>/.qed/proofs/<Iface>.lean` + lakefile) — local name after
    /// any `as` rename — to the proof package root. lean_gen skips the local
    /// sibling axiom module and emits a `require` directive;
    /// `lint_pinned_imports` emits `cpi_unverified_callee` P2 for pinned
    /// imports NOT in this set (the Stance-1 trust gap).
    #[allow(dead_code)]
    pub verified_callees: std::collections::BTreeMap<String, std::path::PathBuf>,

    /// proof_hash drift detected during qed.lock reconciliation in Frozen
    /// mode; empty in Auto/Skip (lock auto-writes) and in Frozen without
    /// drift. main.rs routes these via `upstream_check::route_findings` —
    /// P2 by default, CRIT under `--strict`.
    #[allow(dead_code)]
    pub proof_hash_findings: Vec<crate::upstream_check::DepCheckResult>,

    /// Proof-package dirs of every imported interface in the transitive
    /// closure that ships a Lake-buildable proof package (DFS-pre-order,
    /// deduped). `qedgen verify --recursive` builds each bottom-up so "dep
    /// graph fully proven" reduces to "every layer's Lake build succeeds."
    #[allow(dead_code)]
    pub verified_proof_pkgs: Vec<std::path::PathBuf>,

    /// Per-import account-type bookkeeping: local name (alias or source
    /// `bound_name`) → `ImportedNamespace`. Populated when an imported
    /// source carries `type` declarations beyond the interface-stub shape;
    /// empty for interface-only imports (bundled stdlib stubs). Drives
    /// `generate_imported_mirror` (`src/imported/<ns>.rs`) and `<ns>.<Type>`
    /// resolution in account-binding positions.
    #[allow(dead_code)]
    pub imported_namespaces: std::collections::BTreeMap<String, ImportedNamespace>,
}

/// Adapted form of `ast::RefImplDecl`; carries both Lean and Rust renderings
/// so the body lowers into Spec.lean (`def`) and the impl-targeted Kani
/// harness (inlined at the assertion site).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedRefImpl {
    pub name: String,
    pub doc: Option<String>,
    /// `(name, type_string)`; types keep the source DSL form (`U64`,
    /// `Map[N] T`, …) so each backend picks its own mapping.
    pub params: Vec<(String, String)>,
    pub return_type: String,
    pub lean_body: String,
    pub rust_body: String,
}

/// Lowered `ghost` declaration; pre-rendered per backend, same
/// opaque-string discipline as `ParsedRefImpl`.
#[derive(Debug, Clone)]
pub struct ParsedGhost {
    pub name: String,
    pub doc: Option<String>,
    /// DSL type string (`U64`, `I128`, `Bool`, …). Scalar only.
    pub ty: String,
    /// Initial value, rendered for each backend.
    pub init_lean: String,
    pub init_rust: String,
    pub updates: Vec<ParsedGhostUpdate>,
}

/// One `on <handler>` update of a ghost. `value_*` is the *complete* new
/// value of the ghost after the handler runs (the assignment operator has
/// already been folded in: `+= d` became `<ghost> + (d)`), so each backend
/// just emits `<ghost> := <value>`.
#[derive(Debug, Clone)]
pub struct ParsedGhostUpdate {
    pub handler: String,
    pub value_lean: String,
    pub value_rust: String,
}

/// Lowered `hook` declaration, pre-rendered per backend.
#[derive(Debug, Clone)]
pub struct ParsedHook {
    pub kind: ParsedHookKind,
    pub asserts: Vec<ParsedHookAssert>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedHookKind {
    /// Fires after any store to the named state field.
    AfterStore(String),
    /// Fires before a CPI (optionally only to the named callee namespace).
    BeforeCpi(Option<String>),
}

/// One `assert <expr>` in a hook body, rendered per backend.
#[derive(Debug, Clone)]
pub struct ParsedHookAssert {
    /// Lean rendering, retained for the deferred Lean enforcement path
    /// (qedsvm). Not consumed today — Lean ignores hooks.
    #[allow(dead_code)]
    pub lean: String,
    pub rust: String,
}

impl ParsedSpec {
    /// True iff the spec declared `pragma <name> { ... }`.
    pub fn has_pragma(&self, name: &str) -> bool {
        self.pragmas.iter().any(|p| p == name)
    }

    /// Target inference: `pragma sbpf` present → assembly target, else
    /// Quasar/Anchor (the default). Single source of truth.
    pub fn is_assembly_target(&self) -> bool {
        self.has_pragma("sbpf")
    }

    /// `pragma state_repr = adt` present → multi-variant State lowers as
    /// `inductive State` (Lean) / wrapper-struct + inner-enum (Anchor);
    /// absent → flat `structure State` + `status` discriminant. Single
    /// source of truth for flat-vs-ADT across all four backends. Note
    /// `WrongState` is NOT a repr signal — it's only the error returned on
    /// a variant-mismatch fallthrough.
    pub fn state_repr_is_adt(&self) -> bool {
        self.pragma_value("state_repr") == Some("adt")
    }

    /// Look up a `pragma <key> = <value>` assignment.
    pub fn pragma_value(&self, key: &str) -> Option<&str> {
        self.pragma_assignments
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// `import Name from "key" [as Alias]` statement, captured before resolution.
/// `name` must match a declared `interface Name` in the imported source;
/// `from` keys into `qed.toml`'s `[dependencies]`. The local name at
/// `call ...` sites is `as_name.unwrap_or(name)`.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedImport {
    pub name: String,
    pub from: String,
    pub as_name: Option<String>,
}

/// Full-spec import bookkeeping. When the imported source declares full
/// `type` blocks (a complete qedspec, not an interface stub), the resolver
/// captures its account types/records so codegen can emit a local Rust
/// mirror at `src/imported/<ns>.rs` — handler accounts blocks can then name
/// `<ns>::<Type>` without depending on the foreign crate. Interface-only
/// imports (bundled stdlib stubs) leave the map empty.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ImportedNamespace {
    /// Manifest dep key (`from "..."` value); cited in the generated
    /// mirror's banner comment for traceability.
    pub dep_key: String,
    /// Every `type` block in the imported spec; re-emitted as local Rust
    /// via the same `emit_account_type` path as the consumer's own state.
    pub account_types: Vec<ParsedAccountType>,
    /// Record types referenced by the imported account types, emitted
    /// alongside so the mirror is self-contained.
    pub records: Vec<ParsedRecordType>,
}

impl ParsedImport {
    /// Name used at `call <X>.handler(...)` sites; falls back to `name`
    /// when no alias is declared.
    #[allow(dead_code)]
    pub fn local_name(&self) -> &str {
        self.as_name.as_deref().unwrap_or(&self.name)
    }
}

/// Callee contract: program ID + per-handler shape (and optional effects).
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedInterface {
    pub name: String,
    pub doc: Option<String>,
    pub program_id: Option<String>,
    pub upstream: Option<ParsedUpstream>,
    /// Typed callee-state vocabulary from the optional interface-level
    /// `state { name : Type, ... }` block; chooses the abstract accessor's
    /// Lean codomain (`State → T`) for `state.X` references in
    /// `ensures`/`requires`. Empty → lean_gen defaults to `State → Nat`.
    pub state_fields: Vec<(String, String)>,
    pub handlers: Vec<ParsedInterfaceHandler>,
}

/// Upstream version pin for a library interface — `binary_hash` is
/// authoritative; the rest is informational. `verified_with` lists only
/// backends that were actually run; `"lean"` appears only when the callee is
/// genuinely proven, not axiomatized.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedUpstream {
    pub package: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
    pub binary_hash: Option<String>,
    pub idl_hash: Option<String>,
    pub verified_with: Vec<String>,
    pub verified_at: Option<String>,
}

/// One handler inside an interface block. The `requires`/`ensures` vectors
/// are empty for Tier-0 (shape-only) interfaces. Populated for Tier-1
/// (hand-authored) and Tier-2 (imported) interfaces.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct ParsedInterfaceHandler {
    pub name: String,
    pub doc: Option<String>,
    pub params: Vec<(String, String)>,
    pub discriminant: Option<String>,
    pub accounts: Vec<ParsedHandlerAccount>,
    pub requires: Vec<ParsedRequires>,
    pub ensures: Vec<ParsedEnsures>,
    /// Declared return type (e.g. `-> U64`): callers using `let x = call …`
    /// get a typed binding via `get_return_data`. `None` (typical Tier-0)
    /// means the call is terminal; caller-side `let` is dropped with a warning.
    pub return_type: Option<String>,
    /// For `-> <ident> : <Type>`, the identifier names the return value in
    /// the callee's `ensures`; the CPI substitution rewrites it to the
    /// caller's `let X = …` binder per call site. `None` falls back to the
    /// literal `"result"`.
    pub result_binder: Option<String>,
}

/// Parsed `schema` block: a named bundle of `requires` clauses handlers
/// reference via `include <name>` (pause gating, time-window checks, …).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ParsedSchema {
    pub name: String,
    pub doc: Option<String>,
    /// One entry per `requires expr else Err` clause; same shape as
    /// `ParsedHandler.requires` so the adapter can clone-and-append.
    pub requires: Vec<ParsedRequires>,
}

/// Check spec coverage: which properties have proofs, which have sorry, which are missing.
pub fn check(spec_path: &Path, proofs_dir: &Path) -> Result<Vec<PropertyStatus>> {
    let parsed = parse_spec_file(spec_path)?;

    let properties = generate_properties(&parsed);

    if properties.is_empty() {
        eprintln!("No properties found in {}", spec_path.display());
        return Ok(vec![]);
    }

    let mut proof_content = String::new();
    collect_lean_files(proofs_dir, &mut proof_content)?;

    let results: Vec<PropertyStatus> = properties
        .into_iter()
        .map(|(name, intent, suggestion)| {
            let status = check_property_status(&name, &proof_content);
            let suggestion = if status != Status::Proven {
                suggestion
            } else {
                None
            };
            PropertyStatus {
                name,
                status,
                intent: Some(intent),
                suggestion,
            }
        })
        .collect();

    Ok(results)
}

/// Parse a spec from disk (.qedspec only). `path` is a single `.qedspec`
/// file or a directory: every `.qedspec` under it (recursively) must declare
/// the same `spec Name`; top items merge in sorted source-path order. The
/// multi-file form is pure convention — no grammar, no `import`/`module`.
pub fn parse_spec_file(path: &Path) -> Result<ParsedSpec> {
    parse_spec_file_with_opts(
        path,
        crate::qed_lock::LockMode::Auto,
        crate::import_resolver::CacheOpts::default(),
    )
}

/// Parse with explicit qed.lock mode (e.g. `qedgen check --frozen` passes
/// `LockMode::Frozen`). Thin wrapper kept for existing external callers.
#[allow(dead_code)]
pub fn parse_spec_file_with_lock(
    path: &Path,
    lock_mode: crate::qed_lock::LockMode,
) -> Result<ParsedSpec> {
    parse_spec_file_with_opts(
        path,
        lock_mode,
        crate::import_resolver::CacheOpts::default(),
    )
}

/// Full-control entry: explicit lock mode + cache policy.
/// `qedgen check --frozen --no-cache` calls this with both overrides.
pub fn parse_spec_file_with_opts(
    path: &Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> Result<ParsedSpec> {
    if path.is_dir() {
        return parse_spec_dir_with_opts(path, lock_mode, cache_opts);
    }

    // A non-existent path would otherwise fall through to the extension
    // check and report a confusing "Unsupported spec format: .".
    if !path.exists() {
        anyhow::bail!(
            "spec path does not exist: {}\n\
             Pass either a `.qedspec` file or a directory containing `.qedspec` files.",
            path.display()
        );
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "qedspec" {
        anyhow::bail!(
            "Unsupported spec format: .{}. Only .qedspec files are supported.\n\
             Convert Lean specs to .qedspec format (see examples/).",
            ext
        );
    }

    let src =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let typed = crate::chumsky_parser::parse(&src).map_err(|errs| {
        let msg = errs
            .iter()
            .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, &src)))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!("parse error in {}:\n{}", path.display(), msg)
    })?;
    let mut parsed = crate::chumsky_adapter::adapt(&typed);
    crate::chumsky_adapter::typecheck_spec(&typed, &parsed)?;
    let manifest_dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve_and_merge_imports(&mut parsed, manifest_dir, lock_mode, cache_opts)?;
    validate_imported_account_refs(&parsed)?;
    Ok(parsed)
}

/// Parse every `.qedspec` under `dir` (recursively), require a shared
/// `spec Name`, and merge top items. Files are visited in sorted path order
/// so the `ParsedSpec` and all downstream artifacts are deterministic.
fn parse_spec_dir_with_opts(
    dir: &Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> Result<ParsedSpec> {
    let mut files = Vec::new();
    collect_qedspec_files(dir, &mut files)?;
    files.sort();

    anyhow::ensure!(
        !files.is_empty(),
        "no .qedspec files found under {}",
        dir.display()
    );

    let mut merged_name: Option<String> = None;
    let mut merged_items: Vec<crate::ast::Node<crate::ast::TopItem>> = Vec::new();

    for file in &files {
        let src =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let typed = crate::chumsky_parser::parse(&src).map_err(|errs| {
            let msg = errs
                .iter()
                .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, &src)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::anyhow!("parse error in {}:\n{}", file.display(), msg)
        })?;

        match &merged_name {
            None => merged_name = Some(typed.name.clone()),
            Some(existing) if existing != &typed.name => {
                anyhow::bail!(
                    "spec name mismatch in {}: declared `spec {}`, but a sibling \
                     file declares `spec {}`. Every .qedspec fragment in a \
                     multi-file spec directory must declare the same name.",
                    file.display(),
                    typed.name,
                    existing,
                );
            }
            _ => {}
        }

        merged_items.extend(typed.items);
    }

    let merged = crate::ast::Spec {
        name: merged_name.expect("non-empty files implies non-empty name"),
        items: merged_items,
    };
    let mut parsed = crate::chumsky_adapter::adapt(&merged);
    crate::chumsky_adapter::typecheck_spec(&merged, &parsed)?;
    resolve_and_merge_imports(&mut parsed, dir, lock_mode, cache_opts)?;
    validate_imported_account_refs(&parsed)?;
    Ok(parsed)
}

/// Every `acct : Ident.Ident` binding (parsed into
/// `ParsedHandlerAccount::imported_namespace`) must reference a known
/// namespace AND a known type within it. Bare bindings (`acct : signer`,
/// `acct : LocalState`) bypass this validator.
fn validate_imported_account_refs(parsed: &ParsedSpec) -> Result<()> {
    for handler in &parsed.handlers {
        for acct in &handler.accounts {
            let Some(ref ns) = acct.imported_namespace else {
                continue;
            };
            let Some(ref ty) = acct.account_type else {
                anyhow::bail!(
                    "handler `{}` account `{}` declares an imported namespace `{}` \
                     but no type name after the `.` — write `type {}.<TypeName>`",
                    handler.name,
                    acct.name,
                    ns,
                    ns,
                );
            };
            let imported_ns = parsed.imported_namespaces.get(ns).ok_or_else(|| {
                let known = if parsed.imported_namespaces.is_empty() {
                    "no imports declared".to_string()
                } else {
                    format!(
                        "known namespaces: {}",
                        parsed
                            .imported_namespaces
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                };
                anyhow::anyhow!(
                    "handler `{}` account `{}` references unknown namespace `{}` \
                     (in `type {}.{}`); {}. Add `import {} from \"<dep_key>\"` \
                     at the top of the spec.",
                    handler.name,
                    acct.name,
                    ns,
                    ns,
                    ty,
                    known,
                    ns,
                )
            })?;
            let known_in_ns = imported_ns.account_types.iter().any(|a| &a.name == ty);
            if !known_in_ns {
                anyhow::bail!(
                    "handler `{}` account `{}` references type `{}.{}` but namespace \
                     `{}` declares no such type (known types in namespace: {}). \
                     Check the imported spec at dep `{}`.",
                    handler.name,
                    acct.name,
                    ns,
                    ty,
                    ns,
                    imported_ns
                        .account_types
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    imported_ns.dep_key,
                );
            }
        }
    }
    Ok(())
}

/// Resolve every `import Name from "key"` against `qed.toml` in
/// `manifest_dir`, fetch the imported source(s) (path or github), parse, and
/// merge the matching `interface Name { ... }` into `parsed.interfaces`.
///
/// Resolution is shallow: imported specs' own `import` statements are not
/// transitively walked — each consumer declares its direct deps in its own
/// qed.toml.
fn resolve_and_merge_imports(
    parsed: &mut ParsedSpec,
    manifest_dir: &Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> anyhow::Result<()> {
    if parsed.imports.is_empty() {
        return Ok(());
    }

    // Locate qed.toml. Required when imports are present, EXCEPT when
    // every import resolves to a bundled-stdlib builtin (`from "spl"`,
    // `from "system"`). The resolver short-circuits those before
    // consulting the manifest, so an empty manifest is fine.
    let manifest = match crate::qed_manifest::load_from_dir(manifest_dir)? {
        Some(m) => m,
        None => {
            if crate::import_resolver::all_imports_are_builtins(&parsed.imports) {
                crate::qed_manifest::Manifest::default()
            } else {
                anyhow::bail!(
                    "spec has {} `import` statement(s) but no `qed.toml` next to it (expected at {})",
                    parsed.imports.len(),
                    manifest_dir
                        .join(crate::qed_manifest::MANIFEST_FILENAME)
                        .display(),
                )
            }
        }
    };

    let resolved = crate::import_resolver::resolve_imports_with_opts(
        &parsed.imports,
        &manifest,
        manifest_dir,
        cache_opts,
    )?;

    let mut lock = crate::qed_lock::LockFile::new();

    for r in resolved {
        let imported = parse_imported_sources(&r).with_context(|| {
            format!(
                "parsing imported spec `{}` (dep key `{}`)",
                r.bound_name, r.dep_key,
            )
        })?;

        // Imported source may declare an explicit `interface <name>` block
        // OR rely on implicit synthesis from top-level handlers (DSL ref:
        // every handler in the imported spec is public).
        let explicit = imported.interfaces.iter().find(|i| i.name == r.bound_name);
        let synthesized: Option<ParsedInterface> = if explicit.is_none() {
            synthesize_interface_from_imported(&r.bound_name, &imported)
        } else {
            None
        };
        // Data-only import: no `interface <bound>` block and no top-level
        // handlers, but at least one `type` declaration. Synthesize a
        // minimal empty interface (program_id only) so the merge loop runs
        // and `imported_namespaces` gets populated — supports
        // `acct : Foreign.State` field reads without any CPI surface.
        let data_only_iface: Option<ParsedInterface> =
            if explicit.is_none() && synthesized.is_none() && !imported.account_types.is_empty() {
                Some(ParsedInterface {
                    name: r.bound_name.clone(),
                    doc: None,
                    program_id: imported.program_id.clone(),
                    upstream: None,
                    state_fields: Vec::new(),
                    handlers: Vec::new(),
                })
            } else {
                None
            };
        let iface = match (explicit, &synthesized, &data_only_iface) {
            (Some(i), _, _) => i,
            (None, Some(i), _) => i,
            (None, None, Some(i)) => i,
            (None, None, None) => {
                let where_clause = if r.sources.len() == 1 {
                    format!("at {}", r.sources[0].0.display())
                } else {
                    format!("(merged from {} fragments)", r.sources.len())
                };
                anyhow::bail!(
                    "import `{}` from `{}` — imported source {} declares no `interface {}` block, no top-level handlers, and no `type` declarations. Add an `interface {{ ... }}`, at least one `handler`, or at least one `type` block to the imported spec.",
                    r.bound_name,
                    r.dep_key,
                    where_clause,
                    r.bound_name,
                );
            }
        };

        // Build the lock entry while everything is in scope. Bundled-stdlib
        // builtins don't appear in `manifest.dependencies`; their entry uses
        // a synthetic `builtin:<key>` source identifier. Imported
        // account-type names go on the entry so `--frozen` notices a
        // renamed/removed type before codegen breaks on a missing mirror;
        // comma-joined to keep the on-disk shape one TOML string.
        let imported_type_names = imported
            .account_types
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let lock_entry = if let Some(dep) = manifest.dependencies.get(&r.dep_key) {
            crate::qed_lock::entry_for_resolved(&r, dep, iface, &imported_type_names)
        } else {
            crate::qed_lock::entry_for_builtin(&r, iface, &imported_type_names)
        };
        lock.dependencies.push(lock_entry);

        // Apply the optional `as <alias>` rename when merging.
        let mut merged = iface.clone();
        if let Some(alias) = &r.local_alias {
            merged.name = alias.clone();
        }
        // Register the verified-callee mapping under the local (post-alias)
        // name — lean_gen looks up by this name. Each pkg_root also goes
        // onto `verified_proof_pkgs` (path-deduped after the loop) so
        // `verify --recursive` can walk the dep graph without re-resolving;
        // the resolver returns DFS-pre-order, naturally bottom-up-by-leaf.
        if r.has_proofs {
            if let Some(ref pkg_root) = r.proof_pkg_root {
                parsed
                    .verified_callees
                    .insert(merged.name.clone(), pkg_root.clone());
                parsed.verified_proof_pkgs.push(pkg_root.clone());
            }
        }
        let local_ns_name = merged.name.clone();
        parsed.interfaces.push(merged);

        // Every imported source registers here — including bundled stubs
        // with empty `account_types`. `imported_namespaces` is the canonical
        // parse-layer truth for "every imported source"; the empty case is
        // meaningful (Tier-0 stubs), not a suppression signal — "anything to
        // mirror?" is codegen's call (`generate_imported_mirror`). Local
        // name follows the same alias-or-bound-name rule as the interface
        // merge so type refs match call names.
        let ns = ImportedNamespace {
            dep_key: r.dep_key.clone(),
            account_types: imported.account_types.clone(),
            records: imported.records.clone(),
        };
        parsed.imported_namespaces.insert(local_ns_name, ns);
    }
    // Dedup preserving first-seen DFS order — handles diamond dep shapes.
    let mut seen = std::collections::HashSet::new();
    parsed
        .verified_proof_pkgs
        .retain(|p| seen.insert(p.clone()));

    let proof_hash_findings = crate::qed_lock::handle_lock(manifest_dir, &lock, lock_mode)?;
    parsed.proof_hash_findings = proof_hash_findings;

    Ok(())
}

/// Synthesize a `ParsedInterface` from the imported spec's top-level
/// handlers when no explicit `interface { … }` block is declared (DSL ref:
/// every handler in the imported spec is public). Tier-2 contract:
/// requires/ensures from the handlers' clauses, accounts from their accounts
/// blocks. `None` when there are no top-level handlers (caller emits a
/// clearer error).
fn synthesize_interface_from_imported(
    bound_name: &str,
    imported: &ParsedSpec,
) -> Option<ParsedInterface> {
    if imported.handlers.is_empty() {
        return None;
    }
    let handlers = imported
        .handlers
        .iter()
        .map(|h| ParsedInterfaceHandler {
            name: h.name.clone(),
            doc: h.doc.clone(),
            params: h.takes_params.clone(),
            discriminant: None,
            accounts: h.accounts.clone(),
            requires: h.requires.clone(),
            ensures: h.ensures.clone(),
            // Top-level handlers can't declare a return type or named
            // binder until the handler grammar grows them: `let x = call …`
            // bindings drop with a lint warning, and substitution falls
            // back to the literal "result".
            return_type: None,
            result_binder: None,
        })
        .collect();
    Some(ParsedInterface {
        name: bound_name.to_string(),
        doc: None,
        program_id: imported.program_id.clone(),
        upstream: None,
        // Synthesized interfaces carry no abstract-state vocabulary:
        // top-level handlers express ensures with concrete `state.X`
        // references, so the bundled-axiom path needing typed accessors
        // never fires for Tier-2 callees.
        state_fields: Vec::new(),
        handlers,
    })
}

/// Parse the source bytes for one resolved import. Single-file deps go
/// through `chumsky_adapter::parse_str`; multi-file deps follow the same
/// path-sorted merge logic as `parse_spec_dir` (same `spec Name`, top items
/// merged before the adapter runs).
fn parse_imported_sources(r: &crate::import_resolver::ResolvedImport) -> Result<ParsedSpec> {
    if r.sources.len() == 1 {
        let (src_path, src_bytes) = &r.sources[0];
        return crate::chumsky_adapter::parse_str(src_bytes)
            .with_context(|| format!("parsing imported spec source at {}", src_path.display()));
    }

    // Multi-file: parse each, merge AST top items, validate name consistency.
    let mut merged_name: Option<String> = None;
    let mut merged_items: Vec<crate::ast::Node<crate::ast::TopItem>> = Vec::new();
    for (path, src) in &r.sources {
        let typed = crate::chumsky_parser::parse(src).map_err(|errs| {
            let msg = errs
                .iter()
                .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, src)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::anyhow!("parse error in imported {}:\n{}", path.display(), msg)
        })?;
        match &merged_name {
            None => merged_name = Some(typed.name.clone()),
            Some(existing) if existing != &typed.name => anyhow::bail!(
                "imported spec fragment {} declares `spec {}`, but a sibling \
                 fragment declares `spec {}`. Every fragment of a multi-file \
                 imported dep must declare the same name.",
                path.display(),
                typed.name,
                existing,
            ),
            _ => {}
        }
        merged_items.extend(typed.items);
    }
    let merged = crate::ast::Spec {
        name: merged_name.expect("non-empty source list implies a name"),
        items: merged_items,
    };
    let parsed = crate::chumsky_adapter::adapt(&merged);
    crate::chumsky_adapter::typecheck_spec(&merged, &parsed)?;
    Ok(parsed)
}

/// Read the spec source — file or directory of fragments — as one string,
/// joined in the loader's sorted-path order. Raw-text consumers (e.g.
/// `spec_hash_for_handler`) MUST use this so their hash matches what the
/// proc-macro computes at compile time.
pub fn read_spec_source(path: &Path) -> Result<String> {
    if path.is_dir() {
        let mut files = Vec::new();
        collect_qedspec_files(path, &mut files)?;
        files.sort();
        let mut out = String::new();
        for f in &files {
            let src =
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
            out.push_str(&src);
            if !src.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(out)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }
}

/// Recursive collector for `.qedspec` files under a directory, depth-first.
/// Silently skips non-UTF8 paths (pathologically rare in a source tree).
fn collect_qedspec_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            collect_qedspec_files(&path, out)?;
        } else if file_type.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("qedspec")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Expected properties as (property_name, intent_description, optional_suggestion).
/// Works off unified `spec.handlers` across all target types.
fn generate_properties(spec: &ParsedSpec) -> Vec<(String, String, Option<String>)> {
    let mut props = Vec::new();

    // ── Handler-level proof obligations (unified, works for all targets) ──

    for handler in &spec.handlers {
        // CPI correctness: handler has transfers → needs CPI proof
        if !handler.transfers.is_empty() {
            let intent = format!("{} transfers tokens — verify CPI correctness", handler.name);
            let suggestion = Some(
                "Prove CPI targets the correct program with correct accounts and discriminator."
                    .to_string(),
            );
            props.push((format!("{}.cpi_correct", handler.name), intent, suggestion));
        }

        // Per-handler properties (from sBPF instruction guards/properties)
        for prop_name in &handler.properties {
            let intent = format!("{}: {}", handler.name, prop_name);
            let suggestion =
                Some("Prove with wp_exec. See SKILL.md sBPF proof workflow.".to_string());
            props.push((
                format!("{}.{}", handler.name, prop_name),
                intent,
                suggestion,
            ));
        }

        // Per-handler invariant obligations
        for inv_name in &handler.invariants {
            let intent = format!("{} preserves invariant {}", handler.name, inv_name);
            let suggestion = Some(format!("unfold {} at h_inv ⊢; omega", inv_name));
            props.push((
                format!("{}.preserves_{}", handler.name, inv_name),
                intent,
                suggestion,
            ));
        }
    }

    // ── Top-level invariants ──

    for inv in &spec.invariants {
        let name = &inv.name;
        let intent = match (&inv.lean_expr, inv.doc.is_empty()) {
            (Some(expr), _) => format!("Invariant: {}", expr),
            (None, false) => format!("Invariant: {}", inv.doc),
            (None, true) => format!("Invariant: {}", name),
        };
        let suggestion = Some(
            "This invariant stub is generated as `True` by the DSL. \
             For a meaningful conservation proof, define the predicate and prove it \
             is preserved by all operations."
                .to_string(),
        );
        props.push((name.clone(), intent, suggestion));
    }

    // ── Per-handler property preservation (state-machine properties) ──

    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            let intent = format!(
                "{} is preserved by {}. Prove by unfold/omega.",
                prop.name, op_name
            );
            let suggestion = Some(format!(
                "unfold {} {}Transition at h_inv h ⊢; split_ifs at h with h_eq; simp_all; omega",
                prop.name, op_name
            ));
            props.push((
                format!("{}_preserved_by_{}", prop.name, op_name),
                intent,
                suggestion,
            ));
        }
    }

    props
}

/// Check whether a property is proven, sorry, or missing in the proof content.
fn check_property_status(property_name: &str, proof_content: &str) -> Status {
    // Property names use dots ("Initialize.rejects_wrong_data_len"); proofs
    // may use dots, underscores, «»-quoted names, or (hand-written) the bare
    // name without prefix.
    let leaf = property_name;
    let leaf_underscore = property_name.replace('.', "_");

    let escaped_dot = regex::escape(leaf);
    let escaped_under = regex::escape(&leaf_underscore);
    // For «»-quoted: initialize.access_control → «initialize»\.access_control
    let quoted = leaf.splitn(2, '.').collect::<Vec<_>>();
    let escaped_quoted = if quoted.len() == 2 {
        format!(
            r"«{}»\.{}",
            regex::escape(quoted[0]),
            regex::escape(quoted[1])
        )
    } else {
        escaped_dot.clone()
    };

    // Bare name without instruction prefix (hand-written proofs), plus
    // lowercase prefix forms: "Initialize.X" → "init_X" / "initialize_X".
    let extra_patterns = if quoted.len() == 2 {
        let prefix = quoted[0].to_lowercase();
        let short_prefix = if prefix.len() > 4 {
            &prefix[..4]
        } else {
            &prefix
        };
        let bare = regex::escape(quoted[1]);
        let prefixed_short = format!("{}_{}", regex::escape(short_prefix), bare);
        let prefixed_full = format!("{}_{}", regex::escape(&prefix), bare);
        format!("{}|{}|{}", bare, prefixed_short, prefixed_full)
    } else {
        String::new()
    };

    let theorem_pattern = if extra_patterns.is_empty() {
        format!(
            r"theorem\s+(?:{}|{}|{})\b",
            escaped_dot, escaped_under, escaped_quoted
        )
    } else {
        format!(
            r"theorem\s+(?:{}|{}|{}|{})\b",
            escaped_dot, escaped_under, escaped_quoted, extra_patterns
        )
    };
    let theorem_re = Regex::new(&theorem_pattern).unwrap();

    let Some(m) = theorem_re.find(proof_content) else {
        return Status::Missing;
    };

    // Extract theorem body: from the match to the next top-level keyword
    let rest = &proof_content[m.start()..];
    static BODY_END_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\n(?:theorem|def|noncomputable def|namespace|end|section|#)").unwrap()
    });
    let body = match BODY_END_RE.find(&rest[1..]) {
        Some(end_match) => &rest[..end_match.start() + 1],
        None => rest, // last theorem in file
    };

    if body.contains("sorry") || body.contains(":= trivial") {
        return Status::Sorry;
    }

    Status::Proven
}

/// Recursively collect all .lean file contents from a directory.
fn collect_lean_files(dir: &Path, out: &mut String) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_lean_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("lean") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push_str(&content);
                out.push('\n');
            }
        }
    }
    Ok(())
}

// ============================================================================
// Unified drift detection (qedgen check --code --kani)
// ============================================================================

/// Severity of a completeness warning.
#[derive(Debug, PartialEq, Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A concrete counterexample showing how an operation breaks a property.
/// Structured as data so the agent can reason about it and present it clearly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Counterexample {
    pub property: String,
    pub handler: String,
    /// Pre-state field values (boundary case where invariant barely holds).
    pub pre_state: Vec<(String, i64)>,
    /// Invariant evaluated on pre-state (e.g., "3 ≤ 3").
    pub pre_check: String,
    /// Effects applied (e.g., ["member_count -= 1"]).
    pub effects: Vec<String>,
    pub post_state: Vec<(String, i64)>,
    /// Invariant evaluated on post-state (e.g., "3 ≤ 2").
    pub post_check: String,
    pub invariant_holds: bool,
}

/// A structured fix option for a lint warning.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FixOption {
    /// Short label (e.g., "Add guard", "Strengthen property").
    pub label: String,
    pub rationale: String,
    /// Concrete DSL code to add/change.
    pub snippet: String,
}

/// A spec completeness finding — structured for agent consumption.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletenessWarning {
    /// Rule identifier (e.g., "no_access_control", "unguarded_arithmetic").
    pub rule: String,
    pub severity: Severity,
    /// 1=security, 2=correctness, 3=completeness, 4=quality, 5=polish.
    pub priority: u8,
    pub message: String,
    /// The operation or field this warning relates to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Concrete fix the agent can offer to apply.
    pub fix: String,
    /// Example DSL snippet showing the fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterexample: Option<Counterexample>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub fix_options: Vec<FixOption>,
}

/// Drift status for a generated code file.
#[derive(Debug, PartialEq)]
pub enum DriftStatus {
    InSync,
    NoHash,
    SpecChanged,
    Missing,
    Orphaned,
}

/// Result of checking a single generated file.
#[derive(Debug)]
pub struct DriftResult {
    pub file: String,
    pub status: DriftStatus,
    pub detail: Option<String>,
}

/// Drift status for a Kani harness.
#[derive(Debug, PartialEq)]
pub enum KaniDriftStatus {
    InSync,
    Missing,
    Orphaned,
    FileStale,
}

/// Result of checking a single Kani harness.
#[derive(Debug)]
pub struct KaniDriftResult {
    pub harness_name: String,
    pub status: KaniDriftStatus,
}

/// Full unified report.
pub struct UnifiedReport {
    pub completeness: Vec<CompletenessWarning>,
    pub code_drift: Option<Vec<DriftResult>>,
    pub kani_drift: Option<Vec<KaniDriftResult>>,
    pub lean_coverage: Vec<PropertyStatus>,
}

impl UnifiedReport {
    pub fn issue_count(&self) -> usize {
        let comp = self
            .completeness
            .iter()
            .filter(|w| w.severity == Severity::Warning)
            .count();
        let code = self.code_drift.as_ref().map_or(0, |v| {
            v.iter().filter(|d| d.status != DriftStatus::InSync).count()
        });
        let kani = self.kani_drift.as_ref().map_or(0, |v| {
            v.iter()
                .filter(|d| d.status != KaniDriftStatus::InSync)
                .count()
        });
        let lean = self
            .lean_coverage
            .iter()
            .filter(|r| r.status != Status::Proven)
            .count();
        comp + code + kani + lean
    }
}

fn fields_for_handler<'a>(spec: &'a ParsedSpec, handler: &ParsedHandler) -> &'a [(String, String)] {
    if let Some(account_name) = handler.on_account.as_deref() {
        if let Some(account) = spec
            .account_types
            .iter()
            .find(|acct| acct.name == account_name)
        {
            return &account.fields;
        }
    }
    &spec.state_fields
}

fn suggested_effect_lines(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
    is_init_like: bool,
) -> Vec<String> {
    handler
        .takes_params
        .iter()
        .map(|(name, _)| name.as_str())
        .take(3)
        .map(|param| {
            let matching_field = fields_for_handler(spec, handler)
                .iter()
                .find(|(field, _)| field.contains(param) || param.contains(field.as_str()));
            if let Some((field, _)) = matching_field {
                if is_init_like {
                    format!("    {} = {}", field, param)
                } else {
                    format!("    {} += {}", field, param)
                }
            } else if is_init_like {
                format!("    <field> = {}", param)
            } else {
                format!("    <field> += {}", param)
            }
        })
        .collect()
}

fn reachable_lifecycle_states(spec: &ParsedSpec) -> std::collections::HashSet<String> {
    let mut reachable: std::collections::HashSet<String> = spec
        .account_types
        .iter()
        .filter_map(|acct| acct.lifecycle.first().cloned())
        .collect();
    // Always include the global initial state — account-level lifecycles
    // may start at a later state (e.g. "Active") while the true entry
    // state (e.g. "Uninitialized") is only declared globally.
    if let Some(initial) = spec.lifecycle_states.first() {
        reachable.insert(initial.clone());
    }

    let mut changed = true;
    while changed {
        changed = false;
        for op in &spec.handlers {
            let next_state = match op.post_status.as_ref() {
                Some(post) => post,
                None => continue,
            };
            let can_reach = match op.pre_status.as_ref() {
                Some(pre) => reachable.contains(pre),
                None => true,
            };
            if can_reach && reachable.insert(next_state.clone()) {
                changed = true;
            }
        }
    }

    reachable
}

/// Look up the declared type of a field, checking the handler's target account
/// first, then falling back to the global state_fields.
fn find_field_type(spec: &ParsedSpec, op: &ParsedHandler, field: &str) -> Option<String> {
    if let Some(ref acct_name) = op.on_account {
        if let Some(acct) = spec.account_types.iter().find(|a| a.name == *acct_name) {
            if let Some((_, t)) = acct.fields.iter().find(|(n, _)| n == field) {
                return Some(t.clone());
            }
        }
    }
    spec.state_fields
        .iter()
        .find(|(n, _)| n == field)
        .map(|(_, t)| t.clone())
}

/// Detect the comparison operator and LHS/RHS in a property expression.
/// Returns (lhs_field, operator, rhs_ref) where rhs_ref is either a field name
/// or "__const" for constant comparisons (e.g., `s.V ≤ 10000`).
fn parse_property_relation<'a>(
    expr: &'a str,
    prop_fields: &[&'a str],
) -> Option<(&'a str, &'a str, &'a str)> {
    for op in &[" ≤ ", " ≥ ", " < ", " > ", " = "] {
        if let Some(pos) = expr.find(op) {
            let lhs = &expr[..pos];
            let rhs = &expr[pos + op.len()..];
            // Find which prop field is on each side. A transition property
            // (one referencing `old(...)`) renders the post-state as
            // `s'.<field>` and the `old(...)` side as `s.<field>`; match
            // both so the post side isn't misread as a constant.
            let side_field = |side: &str| {
                prop_fields.iter().find(|f| {
                    side.contains(&format!("s.{}", f)) || side.contains(&format!("s'.{}", f))
                })
            };
            let lhs_field = side_field(lhs);
            let rhs_field = side_field(rhs);
            match (lhs_field, rhs_field) {
                (Some(lf), Some(rf)) => return Some((lf, op.trim(), rf)),
                // Single field vs constant (e.g., s.V ≤ 10000000)
                (Some(lf), None) => return Some((lf, op.trim(), "__const")),
                (None, Some(rf)) => return Some(("__const", op.trim(), rf)),
                _ => {}
            }
        }
    }
    None
}

/// True iff any of the handler's `requires` clauses textually reference any
/// of the named property fields (as `state.<f>` or `s.<f>` with a word
/// boundary on the trailing side, so `state.x` doesn't match `state.xyz`).
///
/// Used by `preserved_by_all_potential_violation` to suppress boundary-only
/// false positives — when the spec author has bounded the relevant fields,
/// trust their claim of inductive preservation rather than firing a warning
/// the local effect-analyzer can't refute.
fn requires_constrains_prop_fields(op: &ParsedHandler, prop_fields: &[&str]) -> bool {
    for req in &op.requires {
        for expr in [&req.rust_expr, &req.lean_expr] {
            for field in prop_fields {
                for prefix in ["state.", "s."] {
                    let needle = format!("{}{}", prefix, field);
                    let mut search = expr.as_str();
                    while let Some(pos) = search.find(&needle) {
                        let after = search[pos + needle.len()..]
                            .chars()
                            .next()
                            .map(|c| !c.is_alphanumeric() && c != '_')
                            .unwrap_or(true);
                        if after {
                            return true;
                        }
                        search = &search[pos + needle.len()..];
                    }
                }
            }
        }
    }
    false
}

fn build_counterexample(
    expr: &str,
    prop_name: &str,
    prop_fields: &[&str],
    op: &ParsedHandler,
    modified_fields: &[&str],
    constants: &[(String, String)],
) -> Option<Counterexample> {
    let relation = parse_property_relation(expr, prop_fields);

    let effect_triples: Vec<(&str, &str, &str)> = op
        .effects
        .iter()
        .filter(|(f, _, _)| modified_fields.contains(&f.as_str()))
        .map(|(f, k, v)| (f.as_str(), k.as_str(), v.as_str()))
        .collect();

    if effect_triples.is_empty() {
        return None;
    }

    let (lhs, op_sym, rhs) = relation?;

    // Transition property: the post side renders `s'.<field>`, the `old(...)`
    // side `s.<field>` (frozen at the pre-state snapshot). Without per-side
    // frozen-ness detection the effect would land on whichever side's field
    // name matches first — inverting `counter ≥ old(counter)` into a bogus
    // violation.
    let is_transition = expr.contains("s'.");
    let (lhs_frozen, rhs_frozen) = if is_transition {
        let mut split = (false, false);
        for opv in &[" ≤ ", " ≥ ", " < ", " > ", " = "] {
            if let Some(pos) = expr.find(opv) {
                let lhs_raw = &expr[..pos];
                let rhs_raw = &expr[pos + opv.len()..];
                let frozen = |raw: &str, field: &str| {
                    field != "__const"
                        && raw.contains(&format!("s.{}", field))
                        && !raw.contains(&format!("s'.{}", field))
                };
                split = (frozen(lhs_raw, lhs), frozen(rhs_raw, rhs));
                break;
            }
        }
        split
    } else {
        (false, false)
    };
    // Display label: a frozen side is the `old(...)` snapshot.
    let label = |field: &str, frozen: bool| {
        if frozen {
            format!("old({})", field)
        } else {
            field.to_string()
        }
    };

    // Build a boundary pre-state where the invariant barely holds
    let (lhs_val, rhs_val): (i64, i64) = match op_sym {
        "≤" | "<=" => (3, 3),
        "≥" | ">=" => (3, 3),
        "<" => (2, 3),
        ">" => (3, 2),
        _ => (3, 3),
    };

    let mut pre_state = Vec::new();
    if lhs != "__const" {
        pre_state.push((label(lhs, lhs_frozen), lhs_val));
    }
    if rhs != "__const" {
        pre_state.push((label(rhs, rhs_frozen), rhs_val));
    }

    let pre_check = format!("{} {} {}", lhs_val, op_sym, rhs_val);

    let mut post_lhs = lhs_val;
    let mut post_rhs = rhs_val;
    let mut effects = Vec::new();
    for (field, kind, value) in &effect_triples {
        let v: i64 = value.parse().unwrap_or_else(|_| {
            constants
                .iter()
                .find(|(n, _)| n == value)
                .and_then(|(_, val)| val.parse().ok())
                .unwrap_or(1)
        });
        let desc = match *kind {
            "add" => format!("{} += {}", field, value),
            "sub" => format!("{} -= {}", field, value),
            "set" => format!("{} = {}", field, value),
            _ => continue,
        };
        effects.push(desc);
        // Effects mutate only the live (non-frozen) side; an `old(...)`
        // reference stays at its pre-state snapshot.
        if *field == lhs && !lhs_frozen {
            match *kind {
                "add" => post_lhs += v,
                "sub" => post_lhs -= v,
                "set" => post_lhs = v,
                _ => {}
            }
        }
        if *field == rhs && !rhs_frozen {
            match *kind {
                "add" => post_rhs += v,
                "sub" => post_rhs -= v,
                "set" => post_rhs = v,
                _ => {}
            }
        }
    }

    let mut post_state = Vec::new();
    if lhs != "__const" {
        post_state.push((label(lhs, lhs_frozen), post_lhs));
    }
    if rhs != "__const" {
        post_state.push((label(rhs, rhs_frozen), post_rhs));
    }

    let holds = match op_sym {
        "≤" | "<=" => post_lhs <= post_rhs,
        "≥" | ">=" => post_lhs >= post_rhs,
        "<" => post_lhs < post_rhs,
        ">" => post_lhs > post_rhs,
        _ => false,
    };

    let post_check = format!("{} {} {}", post_lhs, op_sym, post_rhs);

    Some(Counterexample {
        property: prop_name.to_string(),
        handler: op.name.clone(),
        pre_state,
        pre_check,
        effects,
        post_state,
        post_check,
        invariant_holds: holds,
    })
}

/// Build structured fix suggestions for a property preservation conflict.
fn build_fix_suggestions(
    expr: &str,
    prop_name: &str,
    op: &ParsedHandler,
    prop_fields: &[&str],
    modified_fields: &[&str],
) -> Vec<FixOption> {
    let relation = parse_property_relation(expr, prop_fields);
    let unmodified: Vec<&&str> = prop_fields
        .iter()
        .filter(|f| !modified_fields.contains(f))
        .collect();

    let mut fixes = Vec::new();

    // Fix A: add a guard that ensures the invariant holds after the effect.
    // Only meaningful when the two sides are distinct fields — a transition
    // property (`counter ≥ old(counter)`) has the same field on both sides,
    // where a `requires state.counter > state.counter` guard is nonsensical.
    if let Some((lhs, op_sym, rhs)) = relation.filter(|&(l, _, r)| l != r) {
        for (field, kind, _value) in &op.effects {
            if !modified_fields.contains(&field.as_str()) {
                continue;
            }
            if kind == "sub" {
                if field.as_str() == rhs && (op_sym == "≤" || op_sym == "<=") {
                    fixes.push(FixOption {
                        label: "Add guard".to_string(),
                        rationale: format!(
                            "{} subtracts from {} (RHS of ≤). A strict inequality guard ensures the invariant survives.",
                            op.name, rhs
                        ),
                        snippet: format!(
                            "handler {}\n  requires state.{} < state.{}",
                            op.name, lhs, rhs
                        ),
                    });
                } else if field.as_str() == lhs && (op_sym == "≥" || op_sym == ">=") {
                    fixes.push(FixOption {
                        label: "Add guard".to_string(),
                        rationale: format!(
                            "{} subtracts from {} (LHS of ≥). A strict inequality guard ensures the invariant survives.",
                            op.name, lhs
                        ),
                        snippet: format!(
                            "handler {}\n  requires state.{} > state.{}",
                            op.name, lhs, rhs
                        ),
                    });
                }
            }
        }
    }

    // Fix B: add the handler to preserved_by
    fixes.push(FixOption {
        label: "Add to preserved_by".to_string(),
        rationale: format!(
            "Include '{}' in the property's preserved_by list. Requires a guard (option above) to make the proof go through.",
            op.name
        ),
        snippet: format!(
            "property {} {{\n  preserved_by [..., {}]\n}}",
            prop_name, op.name
        ),
    });

    // Fix C: add a compensating effect
    if let Some(unmod) = unmodified.first() {
        fixes.push(FixOption {
            label: "Add compensating effect".to_string(),
            rationale: format!(
                "Adjust '{}' alongside the modified field(s) to maintain the invariant.",
                unmod
            ),
            snippet: format!(
                "handler {}\n  effect {{ {} = <adjusted_value> }}",
                op.name, unmod
            ),
        });
    }

    fixes
}

/// Check spec completeness — heuristic rules for under-specification.
/// Returns structured warnings with fix suggestions for agent consumption.
pub fn check_completeness(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    // Find a likely signer field name from state (first Pubkey field)
    let signer_hint = spec
        .state_fields
        .iter()
        .find(|(_, t)| t == "Pubkey")
        .map(|(n, _)| n.as_str())
        .unwrap_or("authority");

    // Variant index for `Variant.field` LHS normalization, shared by every
    // effect-LHS lint so the variant prefix is stripped before comparing
    // against bare field names. Maps variant name → its payload fields;
    // empty when no account type has variants.
    let mut variant_fields: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for acct in &spec.account_types {
        for variant in &acct.variants {
            let entry = variant_fields.entry(variant.name.clone()).or_default();
            for (fname, _) in &variant.fields {
                entry.insert(fname.clone());
            }
        }
    }
    // Strip a leading `Variant.` prefix when it names a known variant:
    // `Active.pool` → `pool`; `accounts[i].cap` / `pool` → unchanged.
    let normalize_lhs = |lhs: &str| -> String {
        if let Some(dot) = lhs.find('.') {
            let head = &lhs[..dot];
            if variant_fields.contains_key(head) {
                return lhs[dot + 1..].to_string();
            }
        }
        lhs.to_string()
    };

    // ADT-state transitions return `Err(WrongState)` on a variant-mismatch
    // fallthrough; without that error variant declared the emitted Rust
    // fails to compile. The failure is loud at `cargo check` — this lint
    // just surfaces it at spec-check time with a clear fix.
    if spec.state_repr_is_adt()
        && spec
            .account_types
            .first()
            .map(|a| a.variants.len() > 1)
            .unwrap_or(false)
        && !spec.error_codes.iter().any(|c| c == "WrongState")
    {
        warnings.push(CompletenessWarning {
            rule: "adt_state_missing_wrong_state".to_string(),
            severity: Severity::Warning,
            priority: 2,
            message: "`pragma state_repr = adt` is set but no `WrongState` error is declared — the inductive transitions return `Err(WrongState)` on a variant-mismatch fallthrough, which won't compile".to_string(),
            subject: None,
            fix: "Add `WrongState` to `type Error`, or drop `pragma state_repr = adt` to use the flat State representation.".to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Ghost-variable validation.
    if !spec.ghosts.is_empty() {
        let scalar = |t: &str| {
            matches!(
                t.trim(),
                "U8" | "U16"
                    | "U32"
                    | "U64"
                    | "U128"
                    | "I8"
                    | "I16"
                    | "I32"
                    | "I64"
                    | "I128"
                    | "Bool"
            )
        };
        // Ghosts are only wired into the flat single-account verification
        // State today. Indexed (`Map[N]`), multi-account, and explicit
        // ADT-state shapes don't yet thread ghost fields through their
        // renderers, so flag rather than silently drop them.
        let is_indexed = spec
            .state_fields
            .iter()
            .any(|(_, t)| t.trim_start().starts_with("Map"));
        let is_multi_account = spec.account_types.len() > 1;
        let is_adt = spec.state_repr_is_adt();
        let unsupported_shape = is_indexed || is_multi_account || is_adt;
        let handler_names: std::collections::BTreeSet<&str> =
            spec.handlers.iter().map(|h| h.name.as_str()).collect();
        for g in &spec.ghosts {
            if !scalar(&g.ty) {
                warnings.push(CompletenessWarning {
                    rule: "ghost_non_scalar_type".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "ghost '{}' has non-scalar type '{}' — ghosts must be a scalar (U8…U128 / I8…I128 / Bool)",
                        g.name, g.ty
                    ),
                    subject: Some(g.name.clone()),
                    fix: "Use a scalar ghost type. Aggregate quantities over collections belong in a `property` via `sum i : Idx, …`, not a ghost.".to_string(),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
            for u in &g.updates {
                if !handler_names.contains(u.handler.as_str()) {
                    warnings.push(CompletenessWarning {
                        rule: "ghost_update_unknown_handler".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: format!(
                            "ghost '{}' has an `on {}` clause, but no handler named '{}' exists",
                            g.name, u.handler, u.handler
                        ),
                        subject: Some(g.name.clone()),
                        fix: "Name an existing handler in the `on` clause, or remove the clause."
                            .to_string(),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
            if unsupported_shape {
                warnings.push(CompletenessWarning {
                    rule: "ghost_unsupported_state_shape".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "ghost '{}' is declared with an indexed / multi-account / ADT state — ghost fields are only wired into the flat single-account verification State today",
                        g.name
                    ),
                    subject: Some(g.name.clone()),
                    fix: "Move the ghost to a flat single-account spec, or track the quantity in a `property` (e.g. `sum i : Idx, accounts[i].x`) until ghost support lands for this shape.".to_string(),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Hook validation.
    if !spec.hooks.is_empty() {
        // Lean enforcement is deferred (lands with qedsvm); hooks are
        // currently checked only in the Kani / proptest harnesses.
        warnings.push(CompletenessWarning {
            rule: "hook_lean_unsupported".to_string(),
            severity: Severity::Info,
            priority: 3,
            message: "hooks are enforced in the Kani / proptest harnesses; Lean enforcement is deferred (lands with qedsvm)".to_string(),
            subject: None,
            fix: "No action needed — `qedgen verify --kani` / `--proptest` exercise the hook assertions.".to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
        let state_field_names: std::collections::BTreeSet<&str> =
            spec.state_fields.iter().map(|(n, _)| n.as_str()).collect();
        let is_indexed = spec
            .state_fields
            .iter()
            .any(|(_, t)| t.trim_start().starts_with("Map"));
        let unsupported_shape = is_indexed || spec.account_types.len() > 1;
        for hook in &spec.hooks {
            match &hook.kind {
                ParsedHookKind::AfterStore(field) => {
                    if !state_field_names.contains(field.as_str()) {
                        warnings.push(CompletenessWarning {
                            rule: "hook_unknown_field".to_string(),
                            severity: Severity::Warning,
                            priority: 2,
                            message: format!(
                                "hook `after_store({})` names '{}', which is not a state field",
                                field, field
                            ),
                            subject: None,
                            fix: "Name a declared state field in `after_store(<field>)`."
                                .to_string(),
                            example: None,
                            counterexample: None,
                            fix_options: vec![],
                        });
                    }
                    if unsupported_shape {
                        warnings.push(CompletenessWarning {
                            rule: "hook_unsupported_state_shape".to_string(),
                            severity: Severity::Warning,
                            priority: 2,
                            message: format!(
                                "hook `after_store({})` is declared with an indexed / multi-account state — `after_store` is wired into the flat single-account transition only",
                                field
                            ),
                            subject: None,
                            fix: "Use a flat single-account spec, or assert the post-store condition in a `property`.".to_string(),
                            example: None,
                            counterexample: None,
                            fix_options: vec![],
                        });
                    }
                }
                ParsedHookKind::BeforeCpi(_) => {
                    warnings.push(CompletenessWarning {
                        rule: "hook_before_cpi_unsupported".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: "`hook before_cpi` enforcement is deferred — the runtime state model has no CPI to anchor to, and the Lean CPI-theorem precondition path lands with qedsvm".to_string(),
                        subject: None,
                        fix: "Encode the precondition as a `requires` on the calling handler for now, or assert it via `after_store` on the field the CPI consumes.".to_string(),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
            if hook.asserts.is_empty() {
                warnings.push(CompletenessWarning {
                    rule: "hook_no_assert".to_string(),
                    severity: Severity::Info,
                    priority: 3,
                    message: "hook has no `assert` clause — it checks nothing".to_string(),
                    subject: None,
                    fix: "Add at least one `assert <expr>` to the hook body.".to_string(),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    for op in &spec.handlers {
        // `auth X` and `permissionless` are contradictory; surface as P1
        // rather than silently letting one take precedence.
        if op.permissionless && op.who.is_some() {
            warnings.push(CompletenessWarning {
                rule: "contradictory_auth".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{}' declares both `auth {}` and `permissionless` — pick one",
                    op.name,
                    op.who.as_deref().unwrap_or("?"),
                ),
                subject: Some(op.name.clone()),
                fix: "Remove one: `permissionless` for deliberately-open handlers, `auth X` for access-controlled ones.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 1: handler without `auth`. Skipped for `permissionless` —
        // an explicit opt-in, not a missing declaration.
        if op.who.is_none() && !op.permissionless {
            warnings.push(CompletenessWarning {
                rule: "no_access_control".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!("handler '{}' has no `auth` — anyone can call it", op.name),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add `auth {}` to restrict who can execute this handler, or `permissionless` if this handler is deliberately open",
                    signer_hint
                ),
                example: Some(format!("  handler {}\n    auth {}", op.name, signer_hint)),
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 2: handler not covered by any property
        let covered = spec
            .properties
            .iter()
            .any(|p| p.preserved_by.contains(&op.name));
        if !covered && !spec.properties.is_empty() {
            let prop_names: Vec<&str> = spec.properties.iter().map(|p| p.name.as_str()).collect();
            warnings.push(CompletenessWarning {
                rule: "uncovered_operation".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' is not in any property's `preserved_by`",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add '{}' to an existing property's `preserved_by` list, or confirm it doesn't need property coverage",
                    op.name
                ),
                example: Some(format!(
                    "  property {} \"...\"\n    preserved_by: ..., {}",
                    prop_names.first().unwrap_or(&"my_property"),
                    op.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 3: add effect without explicit overflow bound (type-aware),
        // per-field. Sub effects get auto-guarded for underflow by codegen,
        // so only add overflow warns here.
        {
            // Collect all guard text for substring matching
            let all_guards: String = {
                let mut g = op.guard_str.clone().unwrap_or_default();
                for req in &op.requires {
                    g.push(' ');
                    g.push_str(&req.lean_expr);
                }
                g
            };

            for (field, kind, val) in &op.effects {
                if kind != "add" {
                    continue;
                }
                // Check if any guard already bounds this field's addition.
                // Use contains_word on the val side to avoid "1" matching "10".
                let patterns = [
                    format!("state.{} + {}", field, val),
                    format!("{} + state.{}", val, field),
                    format!("s.{} + {}", field, val),
                    format!("{} + s.{}", val, field),
                ];
                let field_bounded = patterns.iter().any(|pat| contains_word(&all_guards, pat));
                if field_bounded {
                    continue;
                }

                // Cumulative bound: `requires state.x + a + b <= U64_MAX`
                // bounds both `+= a` and `+= b`, but the per-pair patterns
                // above only match the first additive term. Accept when the
                // field appears in an additive expression AND the effect's
                // RHS appears as a bare word in the same guard string.
                let field_in_add = [
                    format!("state.{} +", field),
                    format!("s.{} +", field),
                    format!("+ state.{}", field),
                    format!("+ s.{}", field),
                ]
                .iter()
                .any(|pat| all_guards.contains(pat.as_str()));
                if field_in_add && contains_word(&all_guards, val) {
                    continue;
                }

                let field_type = find_field_type(spec, op, field);
                let type_max = match field_type.as_deref() {
                    Some("U8") => "U8_MAX (255)",
                    Some("U16") => "U16_MAX (65535)",
                    Some("U32") => "U32_MAX",
                    Some("U128") => "U128_MAX",
                    _ => "U64_MAX",
                };
                let type_label = field_type.as_deref().unwrap_or("U64");
                warnings.push(CompletenessWarning {
                    rule: "unguarded_arithmetic".to_string(),
                    severity: Severity::Info,
                    priority: 2,
                    message: format!(
                        "handler '{}' adds to {} field '{}' without an explicit bound — codegen auto-inserts a {} guard, but an explicit `requires` with a tighter domain bound produces stronger proofs",
                        op.name, type_label, field, type_label
                    ),
                    subject: Some(op.name.clone()),
                    fix: format!(
                        "Add `requires state.{} + {} <= MY_BOUND` for a tighter bound than {} max",
                        field, val, type_label
                    ),
                    example: Some(format!(
                        "  handler {}\n    requires state.{} + {} <= {}",
                        op.name, field, val, type_max
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }

        // Rule 6: handler has no when/then lifecycle
        if op.pre_status.is_none() && op.post_status.is_none() {
            warnings.push(CompletenessWarning {
                rule: "no_lifecycle".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{}' has no `when`/`then` — no state machine enforcement",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add `when` and `then` clauses to enforce handler ordering".to_string(),
                example: Some(format!(
                    "  handler {}\n    when Active\n    then Active",
                    op.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 4: state fields never modified (excluding Pubkey)
    for (fname, ftype) in &spec.state_fields {
        if ftype == "Pubkey" {
            continue;
        }
        // A Map / record field counts as modified when written through
        // indexing or nested access (`accounts[i].active := 1`,
        // `pool.balance += amount`) — matching only whole-field LHS gave
        // false-positive `unused_field` on every Map field.
        let modified = spec.handlers.iter().any(|op| {
            op.effects.iter().any(|(f, _, _)| {
                let lhs = normalize_lhs(f);
                if lhs == *fname {
                    return true;
                }
                // Match `<fname>.` (record-nested) or `<fname>[` (Map-indexed)
                // as effective writes of the named field.
                lhs.starts_with(&format!("{}.", fname)) || lhs.starts_with(&format!("{}[", fname))
            })
        });
        if !modified {
            let mutating_ops: Vec<&str> = spec
                .handlers
                .iter()
                .filter(|op| op.has_effect())
                .map(|op| op.name.as_str())
                .collect();
            let op_hint = mutating_ops.first().copied().unwrap_or("some_handler");
            warnings.push(CompletenessWarning {
                rule: "unused_field".to_string(),
                severity: Severity::Info,
                priority: 4,
                message: format!("state field '{}' is never modified by any effect", fname),
                subject: Some(fname.clone()),
                fix: format!(
                    "Add an `effect: {} set <value>` or `effect: {} add <value>` to an operation, or remove the field if it's not needed",
                    fname, fname
                ),
                example: Some(format!(
                    "  operation {}\n    effect: {} set new_value",
                    op_hint, fname
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 5: property references nonexistent handler
    let op_names: Vec<&str> = spec.handlers.iter().map(|o| o.name.as_str()).collect();
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            if !op_names.contains(&op_name.as_str()) {
                warnings.push(CompletenessWarning {
                    rule: "dangling_preserved_by".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "property '{}' references nonexistent handler '{}'",
                        prop.name, op_name
                    ),
                    subject: Some(format!("{}.preserved_by.{}", prop.name, op_name)),
                    fix: format!(
                        "Check the spelling of '{}' — available handlers: {}",
                        op_name,
                        op_names.join(", ")
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule: quantifier over a type that can't be exhausted at test time.
    // Two distinct shapes:
    //   - `forall s : <StateType>` — universal over states (e.g. `Pool.Active`).
    //     Always Lean territory; the whole quantifier is redundant since
    //     `state.x` already refers to the current state. Advice: drop it.
    //   - `forall i : <BinderType>` — bounded value quantifier over a primitive
    //     (U16+, AccountIdx, etc.). U8/I8 fit in proptest; wider types emit a
    //     stub `true`. Advice: narrow the binder.
    let state_type_names: std::collections::HashSet<String> = spec
        .account_types
        .iter()
        .flat_map(|at| {
            // Both the bare type name (e.g. `Pool`) and `Pool.<Variant>` for
            // each lifecycle variant — qedspec quantifiers use the qualified
            // form `Pool.Active` to range over a specific lifecycle state.
            let qualified = at
                .lifecycle
                .iter()
                .map(move |v| format!("{}.{}", at.name, v));
            std::iter::once(at.name.clone()).chain(qualified)
        })
        .collect();
    for prop in &spec.properties {
        // Per-slot lowering already provides a proptest-checkable form for
        // wide-binder forall properties (see ParsedProperty::per_slot).
        // The lint's "harness emits true" warning isn't accurate for these:
        // the per-slot `{prop}_at` predicate is generated and called at the
        // modified slot in each handler's preservation test.
        if prop.per_slot.is_some() {
            continue;
        }
        // When P5 `unsupported_quantifier_shape` fires, skip the legacy
        // `unchecked_quantifier` — P5 carries strictly more precise
        // information (kind + span); double-reporting clutters.
        if prop.quantifier_lint.is_some() {
            continue;
        }
        if let Some(ref rust_expr) = prop.rust_expression {
            if rust_expr_is_unsupported(rust_expr) {
                // Extract the quantifier kind and binder type from the sentinel
                // comment so the message is specific.
                let detail = rust_expr
                    .trim_start_matches("/*")
                    .trim_end_matches("*/")
                    .trim()
                    .trim_start_matches(QEDGEN_UNSUPPORTED_MARKER)
                    .trim_start_matches(':')
                    .trim()
                    .to_string();
                // Pull the binder type out of `forall <var> : <Type>` so we
                // can pick the right advice. Detail looks like
                // `forall s : Pool.Active — lower at harness level`.
                let binder_type: Option<String> = detail
                    .split_once(':')
                    .and_then(|(_, rest)| rest.split('—').next())
                    .map(|s| s.trim().to_string());
                let is_state_quantifier = binder_type
                    .as_ref()
                    .map(|t| state_type_names.contains(t))
                    .unwrap_or(false);
                let (fix, example) = if is_state_quantifier {
                    (
                        "Drop the `forall s : <State>` wrapper — properties are \
                         implicitly evaluated against the current state. Use \
                         `state.<field>` directly."
                            .to_string(),
                        Some(format!(
                            "  // instead of: forall s : <State>, s.x >= s.y\n  \
                             property {} :\n    state.x >= state.y",
                            prop.name
                        )),
                    )
                } else {
                    (
                        "Use U8 or I8 as the quantifier binder type (≤256 values, \
                         exhausted automatically), or split the property into a \
                         per-element guard."
                            .to_string(),
                        Some(format!(
                            "  // instead of: forall v : U64, …\n  \
                             property {} :\n    forall v : U8, …",
                            prop.name
                        )),
                    )
                };
                warnings.push(CompletenessWarning {
                    rule: "unchecked_quantifier".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "property '{}' uses a quantifier over a type that proptest/Kani \
                         cannot exhaust — the harness emits `true` and skips the check ({})",
                        prop.name, detail
                    ),
                    subject: Some(prop.name.clone()),
                    fix,
                    example,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // P5: quantifier shape unsupported by codegen. The chumsky_adapter
    // records a precise reason (nested forall, exists, unbounded binder);
    // surfacing it here shows the exact breaking construct instead of a
    // silent `true` stub later. Supersedes `unchecked_quantifier` for the
    // shapes it covers (that lint skips when quantifier_lint is Some).
    for prop in &spec.properties {
        let Some(qlint) = &prop.quantifier_lint else {
            continue;
        };
        let workaround = match qlint.kind.as_str() {
            "nested_quantifier" => {
                "Split into two single-binder properties — one per quantifier — \
                 so each lowers to a bool-valued harness independently."
            }
            "unbounded_binder" => {
                "Use a primitive (U8…U128) or a declared record type as the binder. \
                 `Vec<T>` / `List<T>` aren't enumerable by Kani / proptest in v2.20."
            }
            "exists_quantifier" => {
                "A bounded `exists` (binder typed `Fin[N]`, e.g. via an index \
                 alias like `MemberIdx = Fin[MAX_MEMBERS]`) lowers to \
                 `(0..N).any(…)`. This `exists` ranges over an unbounded domain \
                 (e.g. `U64`); bound the binder with a `Fin[N]` index type so it \
                 can be enumerated."
            }
            _ => "See docs/limitations.md#unsupported-quantifier-shapes for the workaround.",
        };
        warnings.push(CompletenessWarning {
            rule: "unsupported_quantifier_shape".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "property '{}' has a quantifier shape qedgen v2.20 can't lower to a \
                 non-vacuous harness — {} (bytes {}..{})",
                prop.name, qlint.message, qlint.span_start, qlint.span_end,
            ),
            subject: Some(prop.name.clone()),
            fix: workaround.to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }

    // P6: informational note that `Pubkey` state fields lower structurally
    // to `[u8; 32]` in the verification State (proptest generates them via
    // the 32-byte-array strategy).
    //
    // Scope — every place a Pubkey field can land as state:
    // `account_types[*].fields`, `sum_types[*].variants[*].fields`, and
    // `records[*].fields`. `state_fields` is a flat mirror of the first
    // account type's fields and is intentionally not scanned (double-firing).
    {
        let push_p6 = |warnings: &mut Vec<CompletenessWarning>, holder: &str, field: &str| {
            warnings.push(CompletenessWarning {
                rule: "pubkey_state_field_unsupported".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "P6: Pubkey field '{}' in {} is lowered to `[u8; 32]` in \
                     the generated proptest / Kani harness. The user-facing \
                     Anchor program target keeps the `Pubkey` type.",
                    field, holder,
                ),
                subject: Some(format!("{}.{}", holder, field)),
                fix: format!(
                    "No action required. To compare against an Anchor `Pubkey` \
                     param, convert at the call site: `s.{} == pk.to_bytes()`.",
                    field,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        };

        for acct in &spec.account_types {
            for (fname, ftype) in &acct.fields {
                if ftype == "Pubkey" {
                    push_p6(&mut warnings, &acct.name, fname);
                }
            }
        }
        for sum in &spec.sum_types {
            for variant in &sum.variants {
                for (fname, ftype) in &variant.fields {
                    if ftype == "Pubkey" {
                        let holder = format!("{}.{}", sum.name, variant.name);
                        push_p6(&mut warnings, &holder, fname);
                    }
                }
            }
        }
        for rec in &spec.records {
            for (fname, ftype) in &rec.fields {
                if ftype == "Pubkey" {
                    push_p6(&mut warnings, &rec.name, fname);
                }
            }
        }
    }

    // P7: effect references an undeclared state field. Codegen emits the
    // access verbatim and Rust fails deep inside the generated harness with
    // `no field "foo" on type "State"`; P7 catches it at `qedgen check` with
    // a precise spec-side message. Two paths:
    //   (a) LHS — `effect { undeclared := ... }`: split on `.`/`[` and check
    //       the root only; nested fields under a declared record-typed field
    //       elaborate fine downstream.
    //   (b) RHS — `effect { x := state.undeclared }`: scan the rendered Lean
    //       form for `state.<word>` and check each captured word.
    {
        // All field names declared anywhere as state. This is permissive
        // (a field that exists in any account variant clears P7 even if
        // the handler's specific lifecycle transition doesn't carry it)
        // — false negatives are preferable to a noisy lint that fires
        // on legitimate cross-variant references at this stage.
        let mut declared: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for acct in &spec.account_types {
            for (fname, _) in &acct.fields {
                declared.insert(fname.clone());
            }
        }
        for sum in &spec.sum_types {
            for variant in &sum.variants {
                for (fname, _) in &variant.fields {
                    declared.insert(fname.clone());
                }
            }
        }
        for rec in &spec.records {
            for (fname, _) in &rec.fields {
                declared.insert(fname.clone());
            }
        }
        for (fname, _) in &spec.state_fields {
            declared.insert(fname.clone());
        }

        let push_p7 =
            |warnings: &mut Vec<CompletenessWarning>, handler: &str, side: &str, name: &str| {
                warnings.push(CompletenessWarning {
                    rule: "undeclared_state_field_in_effect".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "P7: handler '{}' references undeclared state field \
                         '{}' on the {} of an effect — codegen will emit the \
                         reference verbatim and `cargo test` will fail with \
                         'no field' downstream",
                        handler, name, side,
                    ),
                    subject: Some(format!("{}.{}", handler, name)),
                    fix: format!(
                        "Declare `{}` in your state schema (an account_type \
                         field, a sum-variant payload field, or a record \
                         field), or rename the effect reference to match an \
                         existing field.",
                        name
                    ),
                    example: Some(format!(
                        "  type State\n    | Active of {{ {} : U64, ... }}\n",
                        name
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            };

        let strip_root = |path: &str| -> String {
            // Take the segment before the first `.` or `[`. Handles bare
            // (`foo`), nested (`foo.bar`), and indexed (`foo[i]`) forms.
            let mut end = path.len();
            for (i, c) in path.char_indices() {
                if c == '.' || c == '[' {
                    end = i;
                    break;
                }
            }
            path[..end].to_string()
        };

        // `Variant.field` LHS forms (`Active.pool := …`) bind the root to a
        // state ADT variant name, not a field; `variant_fields` (built at
        // the top of this fn) keeps the variant index consistent across
        // every effect-LHS lint.
        let second_seg = |path: &str| -> Option<String> {
            // Read the segment between the first and second separator.
            // `Active.pool` → Some("pool"); `Active.x[i]` → Some("x");
            // `Active` (no separator) → None.
            let bytes = path.as_bytes();
            let first = bytes.iter().position(|c| *c == b'.' || *c == b'[')?;
            // Only `.<ident>` is the form we care about for variant lookup.
            if bytes[first] != b'.' {
                return None;
            }
            let rest = &path[first + 1..];
            let mut end = rest.len();
            for (i, c) in rest.char_indices() {
                if c == '.' || c == '[' {
                    end = i;
                    break;
                }
            }
            Some(rest[..end].to_string())
        };

        // (a) LHS check
        for op in &spec.handlers {
            for (lhs, _kind, _rhs) in &op.effects {
                let root = strip_root(lhs);
                if root.is_empty() || declared.contains(&root) {
                    continue;
                }
                // `state := <expr>` is the variant-promotion /
                // whole-record-assignment form (`state := .Active { … }`):
                // `state` is a binder, not a field. The RHS check below
                // still scrutinizes field references in the payload.
                if root == "state" {
                    continue;
                }
                // Synthetic handlers (`_case_N`, `_otherwise`) inherit
                // their parent's effects; flagging twice would be noisy.
                if op.name.contains("_case_") || op.name.ends_with("_otherwise") {
                    continue;
                }
                // `Variant.field` LHS: a variant name as the path root is
                // legal in a multi-variant ADT state — re-target P7 at the
                // actual field, checked against that variant's payload.
                if let Some(variant_payload) = variant_fields.get(&root) {
                    if let Some(field) = second_seg(lhs) {
                        if !variant_payload.contains(&field) && !declared.contains(&field) {
                            push_p7(
                                &mut warnings,
                                &op.name,
                                "LHS",
                                &format!("{}.{}", root, field),
                            );
                        }
                    }
                    // Path root is a known variant — never push the
                    // variant name itself as "undeclared field".
                    continue;
                }
                push_p7(&mut warnings, &op.name, "LHS", &root);
            }
        }

        // (b) RHS check — scan rendered Lean form for state-path
        // references. `expr_to_lean` renders `state.X` as `s.X` (the
        // standard Lean binder for the current state), so we match that
        // form. The leading `\b` keeps `xs.foo` / `as.bar` from
        // triggering — only bare `s.` token boundaries match.
        let state_path_re =
            regex::Regex::new(r"\bs\.([A-Za-z_][A-Za-z0-9_]*)").expect("static regex");
        for op in &spec.handlers {
            let mut seen_rhs: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for (_lhs, _kind, rhs) in &op.effects {
                for caps in state_path_re.captures_iter(rhs) {
                    let name = caps.get(1).unwrap().as_str().to_string();
                    if declared.contains(&name) || !seen_rhs.insert(name.clone()) {
                        continue;
                    }
                    if op.name.contains("_case_") || op.name.ends_with("_otherwise") {
                        continue;
                    }
                    push_p7(&mut warnings, &op.name, "RHS", &name);
                }
            }
        }
    }

    // Rule 7: takes params (U64) with no guard — suggest input validation
    for op in &spec.handlers {
        if op.has_guard() {
            continue;
        }
        // Skip if rule 3 (unguarded_arithmetic) already fired for this op
        let already_flagged = warnings
            .iter()
            .any(|w| w.rule == "unguarded_arithmetic" && w.subject.as_deref() == Some(&op.name));
        if already_flagged {
            continue;
        }
        let u64_params: Vec<&str> = op
            .takes_params
            .iter()
            .filter(|(_, t)| t == "U64")
            .map(|(n, _)| n.as_str())
            .collect();
        if !u64_params.is_empty() {
            let guard_parts: Vec<String> =
                u64_params.iter().map(|p| format!("{} > 0", p)).collect();
            let guard_expr = guard_parts.join(" and ");
            warnings.push(CompletenessWarning {
                rule: "missing_guard_from_takes".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{}' takes U64 params but has no guard — no input validation",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add input validation for takes parameters".to_string(),
                example: Some(format!("  handler {}\n    guard {}", op.name, guard_expr)),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 8: takes params + lifecycle transition but no effect
    for op in &spec.handlers {
        if op.has_effect() {
            continue;
        }
        // ensures-only handlers are deliberate — the author pinned frame
        // conditions (`ensures state.x == old(state.x)`) instead of an
        // effect. Legitimate shape, not a gap.
        if !op.ensures.is_empty() {
            continue;
        }
        // A `call X.handler(...)` (CPI), `transfers` block, or declared
        // `modifies [...]` IS the handler's effect — firing here on
        // CPI-only handlers would force fictional state writes.
        if !op.calls.is_empty() || !op.transfers.is_empty() || op.modifies.is_some() {
            continue;
        }
        // Synthetic per-arm handlers (`<parent>_case_<N>`, `_otherwise`)
        // from `match` expansion have no effect by construction; mirror the
        // codegen's name convention so the lint doesn't fire on them.
        if op.name.contains("_case_") || op.name.ends_with("_otherwise") {
            continue;
        }
        // Top-level abort handlers carry `aborts_if` / `aborts_total` and
        // also have no effect by construction.
        if !op.aborts_if.is_empty() || op.aborts_total {
            continue;
        }
        let has_lifecycle = op.pre_status.is_some() || op.post_status.is_some();
        let is_init_like = op.name.contains("init") || op.name.contains("create");
        if !op.takes_params.is_empty() && (has_lifecycle || is_init_like) {
            let effect_lines = suggested_effect_lines(spec, op, is_init_like);
            warnings.push(CompletenessWarning {
                rule: "missing_effect".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "handler '{}' takes params and transitions state but has no effect",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add an effect block to describe state changes".to_string(),
                example: Some(format!(
                    "  handler {}\n  effect {{\n{}\n  }}",
                    op.name,
                    effect_lines.join("\n")
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 9: handlers with effects but zero properties
    let has_effects = spec.handlers.iter().any(|op| op.has_effect());
    if has_effects && spec.properties.is_empty() && spec.invariants.is_empty() {
        // Suggest conservation if paired add/sub exist on same field
        let mut modified_fields: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            for (field, kind, _) in &op.effects {
                modified_fields
                    .entry(field.as_str())
                    .or_default()
                    .push(kind.as_str());
            }
        }
        let conservation_candidates: Vec<&str> = modified_fields
            .iter()
            .filter(|(_, kinds)| kinds.contains(&"add") && kinds.contains(&"sub"))
            .map(|(f, _)| *f)
            .collect();

        let op_list: Vec<&str> = spec
            .handlers
            .iter()
            .filter(|op| op.has_effect())
            .map(|op| op.name.as_str())
            .collect();
        let preserved_by = if op_list.len() <= 4 {
            format!("[{}]", op_list.join(", "))
        } else {
            "all".to_string()
        };

        let example = if !conservation_candidates.is_empty() {
            let field = conservation_candidates[0];
            format!(
                "  property conservation {{\n    expr state.{} >= 0\n    preserved_by {}\n  }}",
                field, preserved_by
            )
        } else {
            format!(
                "  property my_invariant {{\n    expr <your invariant expression>\n    preserved_by {}\n  }}",
                preserved_by
            )
        };

        warnings.push(CompletenessWarning {
            rule: "no_properties".to_string(),
            severity: Severity::Warning,
            priority: 3,
            message: "spec has effects but no properties — verification has nothing to prove"
                .to_string(),
            subject: None,
            fix: "Add at least one property to define what the verification should prove"
                .to_string(),
            example: Some(example),
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Rule 10: handler has token program in accounts but no transfers.
    //
    // Suppressed on lifecycle-init handlers that create a token account:
    // Anchor's `#[account(init, token::… / associated_token::…)]` handles
    // the SPL Token CPI implicitly — no explicit `transfers` / `call
    // Token.*` needed. Init detection is a shape predicate (pre-state
    // variant carries no payload fields = freshly-created account), not a
    // hardcoded name list, which over-fired on specs naming the pre-state
    // `Uninit` / `Created` / etc. Unit variants come from both
    // `account_types[*].variants` and `sum_types`.
    let unit_variant_names: std::collections::HashSet<&str> = spec
        .account_types
        .iter()
        .flat_map(|a| a.variants.iter())
        .chain(spec.sum_types.iter().flat_map(|s| s.variants.iter()))
        .filter(|v| v.fields.is_empty())
        .map(|v| v.name.as_str())
        .collect();
    for handler in &spec.handlers {
        if !handler.has_token_program() {
            continue;
        }
        if !handler.has_calls() {
            let is_lifecycle_init = handler
                .pre_status
                .as_deref()
                .map(|s| unit_variant_names.contains(s))
                .unwrap_or(false);
            // No writable-token-account sub-condition: real specs often
            // leave token accounts bare-typed and let Anchor resolve via
            // init constraints; `is_lifecycle_init && !has_calls()` already
            // captures the shape Anchor's init macro covers implicitly.
            if is_lifecycle_init {
                continue;
            }
            let writable_tokens: Vec<&str> = handler
                .accounts
                .iter()
                .filter(|a| {
                    a.is_writable && a.account_type.as_deref() == Some("token") && !a.is_program
                })
                .map(|a| a.name.as_str())
                .collect();
            let signer_name = handler
                .signer_account()
                .map(|a| a.name.as_str())
                .unwrap_or("authority");
            let accounts_str = if writable_tokens.len() >= 2 {
                format!(
                    "from {} to {} authority {}",
                    writable_tokens[0], writable_tokens[1], signer_name
                )
            } else if writable_tokens.len() == 1 {
                format!(
                    "from {} to dest authority {}",
                    writable_tokens[0], signer_name
                )
            } else {
                format!("from source to dest authority {}", signer_name)
            };
            warnings.push(CompletenessWarning {
                rule: "missing_cpi_for_token_context".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "handler '{}' has token_program in accounts but no `transfers` block",
                    handler.name
                ),
                subject: Some(handler.name.clone()),
                fix: "Add a `transfers` block to specify token movements".to_string(),
                example: Some(format!(
                    "  handler {}\n    transfers {{\n      {} amount <expr>\n    }}",
                    handler.name, accounts_str
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 11: no errors block but handlers have guards
    let any_guards = spec.handlers.iter().any(|op| op.has_guard());
    if any_guards && spec.error_codes.is_empty() {
        warnings.push(CompletenessWarning {
            rule: "no_errors_block".to_string(),
            severity: Severity::Info,
            priority: 4,
            message: "spec has guards but no `errors` block — codegen can't generate error types"
                .to_string(),
            subject: None,
            fix: "Add an errors block listing all failure modes".to_string(),
            example: Some("  errors [InvalidAmount, Unauthorized, AlreadyClosed]".to_string()),
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Rule 12: lifecycle states unreachable by any operation transition
    if spec.lifecycle_states.len() > 1 {
        let reachable = reachable_lifecycle_states(spec);
        for state in &spec.lifecycle_states {
            if !reachable.contains(state) {
                warnings.push(CompletenessWarning {
                    rule: "lifecycle_unreachable_state".to_string(),
                    severity: Severity::Info,
                    priority: 2,
                    message: format!(
                        "lifecycle state '{}' cannot be reached from any initial state via operation transitions",
                        state
                    ),
                    subject: Some(state.clone()),
                    fix: format!(
                        "Add a `when: {}` or `then: {}` clause to an operation, or remove '{}' from the lifecycle",
                        state, state, state
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 13: write_without_read — state field written in effects but never read in guards/properties
    {
        // Normalize variant-prefixed LHS (`Active.pool` → `pool`) so the
        // read-match finds bare references, and emit leaf names for nested
        // paths: `accounts[i].fee_credits` writes both `accounts` and
        // `fee_credits` for bare-leaf reads in properties/requires.
        let mut written_fields: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for op in &spec.handlers {
            for (field, _, _) in &op.effects {
                let normalized = normalize_lhs(field);
                written_fields.insert(normalized.clone());
                // Also seed every dotted segment / index root so
                // nested-path writes count for the read-side bare-
                // leaf search. `accounts[i].fee_credits` →
                // `accounts`, `fee_credits`. Pure ident segments only;
                // skip the `[…]` indexing form.
                for seg in normalized
                    .split(['.', '[', ']'])
                    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
                {
                    written_fields.insert(seg.to_string());
                }
            }
        }
        // Gather every text that might mention a state field — all
        // requires / ensures / property bodies / invariants, not just the
        // legacy `guard_str` slot (which modern specs leave `None`,
        // making reads invisible and the lint false-positive-heavy).
        let mut texts: Vec<&str> = Vec::new();
        for op in &spec.handlers {
            if let Some(ref guard) = op.guard_str {
                texts.push(guard.as_str());
            }
            for req in &op.requires {
                texts.push(req.lean_expr.as_str());
                texts.push(req.rust_expr.as_str());
            }
            for ens in &op.ensures {
                texts.push(ens.lean_expr.as_str());
            }
        }
        for prop in &spec.properties {
            if let Some(ref expr) = prop.expression {
                texts.push(expr.as_str());
            }
        }
        for inv in &spec.invariants {
            if let Some(ref e) = inv.lean_expr {
                texts.push(e.as_str());
            }
        }
        let mut read_fields: std::collections::HashSet<String> = std::collections::HashSet::new();
        for text in &texts {
            for field in &written_fields {
                if text.contains(&format!("s.{}", field))
                    || text.contains(&format!("state.{}", field))
                    || contains_word(text, field)
                {
                    read_fields.insert(field.clone());
                }
            }
        }
        for field in &written_fields {
            if !read_fields.contains(field) {
                warnings.push(CompletenessWarning {
                    rule: "write_without_read".to_string(),
                    severity: Severity::Info,
                    priority: 3,
                    message: format!(
                        "state field '{}' is written in effects but never referenced in any guard or property",
                        field
                    ),
                    subject: Some(field.clone()),
                    fix: format!(
                        "Add '{}' to a property expression or guard, or verify that writing it without reading is intentional",
                        field
                    ),
                    example: Some(format!(
                        "  property my_invariant {{\n    expr state.{} >= 0\n    preserved_by all\n  }}",
                        field
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 14: dead_guard — a guard conjunct subsumed by another on the same operation
    {
        static CMP_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?:s\.|state\.)?(\w+)\s*(>=|<=|>|<|=)\s*(\d+)$").unwrap()
        });
        let cmp_re = &*CMP_RE;
        for op in &spec.handlers {
            if let Some(ref guard) = op.guard_str {
                // Split on ∧ and "and" to get individual conjuncts
                let conjuncts: Vec<&str> = guard
                    .split('\u{2227}')
                    .flat_map(|s| s.split(" and "))
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();

                let parsed: Vec<(usize, &str, &str, i64)> = conjuncts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        cmp_re.captures(c).and_then(|caps| {
                            let field = caps.get(1)?.as_str();
                            let cmp = caps.get(2)?.as_str();
                            let val: i64 = caps.get(3)?.as_str().parse().ok()?;
                            Some((i, field, cmp, val))
                        })
                    })
                    .collect();

                for &(i, field_a, cmp_a, val_a) in &parsed {
                    for &(j, field_b, cmp_b, val_b) in &parsed {
                        if i == j || field_a != field_b {
                            continue;
                        }
                        // Check if conjunct j implies conjunct i (making i redundant)
                        let subsumed = match (cmp_a, cmp_b) {
                            (">=", ">=") => val_b >= val_a, // x >= 5 implies x >= 3
                            (">", ">") => val_b >= val_a,   // x > 5 implies x > 3
                            (">=", ">") => val_b >= val_a,  // x > 5 implies x >= 5
                            ("<=", "<=") => val_b <= val_a, // x <= 3 implies x <= 5
                            ("<", "<") => val_b <= val_a,
                            ("<=", "<") => val_b <= val_a,
                            _ => false,
                        };
                        if subsumed && i != j {
                            warnings.push(CompletenessWarning {
                                rule: "dead_guard".to_string(),
                                severity: Severity::Info,
                                priority: 4,
                                message: format!(
                                    "guard conjunct '{}' on operation '{}' is subsumed by '{}'",
                                    conjuncts[i], op.name, conjuncts[j]
                                ),
                                subject: Some(op.name.clone()),
                                fix: format!("Remove the redundant conjunct '{}'", conjuncts[i]),
                                example: None,
                                counterexample: None,
                                fix_options: vec![],
                            });
                            break; // Only report once per subsumed conjunct
                        }
                    }
                }
            }
        }
    }

    // Rule 15: circular_lifecycle_no_terminal — lifecycle where every state has outgoing transitions
    if spec.lifecycle_states.len() > 1 {
        let mut outgoing: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            if let (Some(ref pre), Some(ref post)) = (&op.pre_status, &op.post_status) {
                if pre != post {
                    outgoing
                        .entry(pre.as_str())
                        .or_default()
                        .insert(post.as_str());
                }
            }
        }
        // A terminal state has no outgoing transitions to a different state
        let terminal_exists = spec
            .lifecycle_states
            .iter()
            .any(|s| !outgoing.contains_key(s.as_str()) || outgoing[s.as_str()].is_empty());
        if !terminal_exists {
            warnings.push(CompletenessWarning {
                rule: "circular_lifecycle_no_terminal".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: "lifecycle has no terminal state — every state has outgoing transitions"
                    .to_string(),
                subject: None,
                fix: "Consider whether the cycle is intentional. If not, designate a terminal state by removing its outgoing transitions.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 16: excluded_op_modifies_property — handler NOT in preserved_by modifies fields
    // referenced by the property. The inductive theorem will need a manual proof (not sorry).
    for prop in &spec.properties {
        if let Some(ref expr) = prop.expression {
            // Extract field names from the property expression.
            // The expression is in Lean form (s.field_name) from the parser.
            let prop_fields: Vec<&str> = {
                let mut fields = Vec::new();
                // Check both "s." (Lean form) and "state." (DSL form) patterns
                for prefix in &["s.", "state."] {
                    for (i, _) in expr.match_indices(prefix) {
                        let rest = &expr[i + prefix.len()..];
                        let end = rest
                            .find(|c: char| !c.is_alphanumeric() && c != '_')
                            .unwrap_or(rest.len());
                        if end > 0 {
                            let field = &rest[..end];
                            if !fields.contains(&field) {
                                fields.push(field);
                            }
                        }
                    }
                }
                fields
            };

            let uses_all = prop.preserved_by.iter().any(|p| p == "all");
            if uses_all {
                continue; // all ops are in preserved_by, no exclusion
            }

            for op in &spec.handlers {
                if prop.preserved_by.contains(&op.name) {
                    // Handler is claimed to preserve the property — verify via
                    // effect analysis. Warn when the effect demonstrably violates
                    // the bound (covers preserved_by all expansion and explicit lists).
                    let covered_modified: Vec<&str> = op
                        .effects
                        .iter()
                        .filter(|(f, _, _)| prop_fields.contains(&f.as_str()))
                        .map(|(f, _, _)| f.as_str())
                        .collect();
                    if !covered_modified.is_empty() {
                        // Skip when any `requires` references a property
                        // field: the boundary `build_counterexample` picks is
                        // often unreachable because of guards the local
                        // analyzer doesn't model (dedup bitmaps, lifecycle
                        // gates). Trust the author's bound; preserved_by
                        // claims with NO constraining guard still fire.
                        if requires_constrains_prop_fields(op, &prop_fields) {
                            continue;
                        }
                        if let Some(ce) = build_counterexample(
                            expr,
                            &prop.name,
                            &prop_fields,
                            op,
                            &covered_modified,
                            &spec.constants,
                        ) {
                            if !ce.invariant_holds {
                                warnings.push(CompletenessWarning {
                                    rule: "preserved_by_all_potential_violation".to_string(),
                                    severity: Severity::Warning,
                                    priority: 1,
                                    message: format!(
                                        "handler '{}' is in `preserved_by` for property '{}' but effect analysis suggests a violation",
                                        op.name, prop.name
                                    ),
                                    subject: Some(op.name.clone()),
                                    fix: format!(
                                        "Add a guard to '{}' ensuring the invariant holds after the effect, or remove it from `preserved_by`",
                                        op.name
                                    ),
                                    example: None,
                                    counterexample: Some(ce),
                                    fix_options: vec![],
                                });
                            }
                        }
                    }
                    continue;
                }
                // Check if this excluded op modifies any field in the property expression
                let modified_prop_fields: Vec<&str> = op
                    .effects
                    .iter()
                    .filter(|(f, _, _)| prop_fields.contains(&f.as_str()))
                    .map(|(f, _, _)| f.as_str())
                    .collect();

                if !modified_prop_fields.is_empty() {
                    // Skip if ALL effects on property fields are monotonically safe.
                    // e.g., sub on LHS of ≤ can only decrease the LHS → invariant still holds.
                    if let Some((lhs, op_sym, _rhs)) = parse_property_relation(expr, &prop_fields) {
                        let all_safe = op
                            .effects
                            .iter()
                            .filter(|(f, _, _)| modified_prop_fields.contains(&f.as_str()))
                            .all(|(f, kind, _)| {
                                let on_lhs = f.as_str() == lhs;
                                match (kind.as_str(), op_sym, on_lhs) {
                                    ("sub", "≤", true) | ("sub", "<=", true) => true, // decreasing LHS of ≤
                                    ("add", "≥", true) | ("add", ">=", true) => true, // increasing LHS of ≥
                                    ("sub", "≥", false) | ("sub", ">=", false) => true, // decreasing RHS of ≥
                                    ("add", "≤", false) | ("add", "<=", false) => true, // increasing RHS of ≤
                                    _ => false,
                                }
                            });
                        if all_safe {
                            continue; // monotonically preserves the invariant
                        }
                    }

                    let counterexample = build_counterexample(
                        expr,
                        &prop.name,
                        &prop_fields,
                        op,
                        &modified_prop_fields,
                        &spec.constants,
                    );

                    let fix_options = build_fix_suggestions(
                        expr,
                        &prop.name,
                        op,
                        &prop_fields,
                        &modified_prop_fields,
                    );

                    let fix = fix_options.first().map_or_else(
                        || format!(
                            "Add '{}' to property '{}' `preserved_by` with a guard, or restructure the property",
                            op.name, prop.name
                        ),
                        |f| f.snippet.clone(),
                    );

                    warnings.push(CompletenessWarning {
                        rule: "excluded_op_modifies_property".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: format!(
                            "handler '{}' modifies field(s) [{}] used in property '{}' but is excluded from `preserved_by` — no inductive arm is generated for this handler, so the per-arm proof obligation is silently dropped. Either add the handler to `preserved_by` (and discharge the proof) or refactor the property so this handler doesn't need to preserve it.",
                            op.name,
                            modified_prop_fields.join(", "),
                            prop.name
                        ),
                        subject: Some(op.name.clone()),
                        fix,
                        example: None,
                        counterexample,
                        fix_options,
                    });
                }
            }
        }
    }

    // Rule 17: invariant_no_body — doc-string-only invariant. Lean codegen
    // would lower it to `theorem <name> : True := trivial` (vacuous, banned
    // by the no-tautological-proofs policy); surface at check time.
    for inv in &spec.invariants {
        if inv.lean_expr.is_none() {
            warnings.push(CompletenessWarning {
                rule: "invariant_no_body".to_string(),
                severity: Severity::Error,
                priority: 1,
                message: format!(
                    "invariant '{}' has only a description string, no `expr` body — \
                     codegen would emit `theorem {} : True := trivial` (vacuous proof)",
                    inv.name, inv.name
                ),
                subject: Some(inv.name.clone()),
                fix: format!(
                    "Add an `expr` body to invariant '{}': \
                     `invariant {} {{ expr <predicate-over-state> preserved_by all }}`",
                    inv.name, inv.name
                ),
                example: Some(format!(
                    "  invariant {} {{\n    expr state.total_in == state.total_out\n    preserved_by all\n  }}",
                    inv.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Validate new-DSL constructs: Map[N] T fields, subscripted effect LHS.
    warnings.extend(check_map_and_subscript(spec));

    // CPI tier lint: call sites whose target is Tier 0 (no ensures declared)
    // get flagged so users see the gap between "my Rust compiles" and "my
    // program is verified." See docs/design/spec-composition.md §2.
    warnings.extend(check_shape_only_cpi(spec));

    // Complement to shape_only_cpi: declared handlers with no `ensures`
    // leave the caller's Lean theorem carrying `by sorry`.
    warnings.extend(check_cpi_no_callee_ensures(spec));

    // Trust-anchor advisory: imported interfaces discharging via Stance-1
    // axiom because the provider shipped no proof package. P2 advisory;
    // the caller still gets discharge.
    warnings.extend(check_cpi_unverified_callee(spec));

    // PDA seed collision: two PDA declarations with identical seed tuples resolve
    // to the same on-chain address — a common source of account confusion bugs.
    warnings.extend(check_pda_collisions(spec));

    // Checked-arithmetic effects (`+=` / `-=`) make the generated Rust
    // reference `<ProgramName>Error::MathOverflow`; without that variant
    // declared, cargo build fails — surface it at check time instead.
    warnings.extend(check_checked_arith_needs_math_overflow(spec));

    // Per-site `or X` overrides or checked_overflow/underflow pragmas
    // referencing undeclared Error variants would also fail cargo build.
    warnings.extend(check_unknown_error_variant(spec));

    // Opt-in non-default arithmetic (`+=?`/`-=?` wrapping, `+=!`/`-=!`
    // saturating) needs surfacing but isn't reproducible from the spec
    // alone — lives in check, not probe (reproducer-only probe contract).
    warnings.extend(check_wrapping_arithmetic_opt_in(spec));

    // Spec-authoring lints for post-codegen-audit security shapes. See
    // `docs/prds/SPEC-AUTHORING-LINTS-v2.10.md` for the auditor-finding mapping.
    warnings.extend(check_unbound_auth(spec));
    warnings.extend(check_unguarded_indexed_mutation(spec));
    warnings.extend(check_scalar_counter_no_dedup(spec));
    warnings.extend(check_unguarded_terminal_transition(spec));
    warnings.extend(check_unconditional_value_transfer(spec));

    // Flag bare same-named field references in multi-ADT specs.
    // Lint-only; user qualifies or splits the property.
    warnings.extend(check_cross_adt_field_ambiguity(spec));

    // vacuous_property_lowering: codegen-induced tautologies, the
    // unsupported-quantifier marker, and literal `true` bodies.
    // Author-written tautologies are silently accepted.
    warnings.extend(check_vacuous_property_lowering(spec));

    // `old(...)` inside `requires` / `invariant` is a category error —
    // both describe a single state with no "old" value. P1 with fix-it.
    warnings.extend(check_old_in_single_state_context(spec));

    // `type Error = { … }` (record brace form) parses cleanly but yields no
    // error variants, silently breaking every `error_codes` consumer. P0.
    warnings.extend(check_error_declared_as_record(spec));

    // `modifies [X]` with no effect write and no `ensures` reference: the
    // field is completely unconstrained — Lean frame proofs allow any
    // post-value, the impl-fill site has nothing to verify against. P0.
    warnings.extend(check_unconstrained_modifies(spec));

    // ref_impl bodies with potentially-overflowing arithmetic over bounded
    // numerics: Lean proves on unbounded `Nat`; Rust runs on `u64`/`i64`
    // where the same expression can wrap or panic. Bounded-arith
    // verification lives in Kani; the same predicate drives the
    // impl-targeted Kani auto-trigger.
    warnings.extend(check_ref_impl_unbounded_arith(spec));

    // ≥2 CPI calls whose substituted ensures reference the SAME caller-state
    // field: both `kani::assume` lines fire at one splice point against one
    // (pre, post) snapshot pair, which can over-constrain. Per-call snapshot
    // frames is v3.0-class.
    warnings.extend(check_multi_cpi_same_field(spec));

    // Sort by priority (ascending), then by rule name for stability.
    warnings.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.rule.cmp(&b.rule)));

    warnings
}

/// Defense-in-depth lint for three vacuous-property-body shapes in the
/// *rendered Rust*:
///
/// 1. **Codegen-induced tautology (P1, AST-gated).** AST body contains
///    `Expr::Old(_)` AND `rust_expression` reduces to `<expr> cmp <expr>`
///    with structurally identical sides — the temporal marker was dropped
///    during lowering. Should be unreachable from current codegen; kept as
///    a regression net.
/// 2. **Unsupported-quantifier marker (P1).** `rust_expression` contains
///    `QEDGEN_UNSUPPORTED_QUANTIFIER` — codegen emitted a stub `true` body.
///    Unlike `unsupported_quantifier_shape`, fires regardless of `per_slot`.
/// 3. **Literal `true` body (P1).** Catches any other codegen path that
///    short-circuited to a constant.
///
/// **Author-written tautologies are silently accepted**: no `Expr::Old(_)`
/// in the AST + identical sides is an authored choice (the "field tracking"
/// pattern). Rule 1 gates on `Expr::Old(_)` precisely so this passes.
fn check_vacuous_property_lowering(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for prop in &spec.properties {
        let Some(rs) = prop.rust_expression.as_deref() else {
            continue;
        };
        let trimmed = rs.trim();

        // Rule 2 — unconditional: marker present, body is a stub.
        if rs.contains(QEDGEN_UNSUPPORTED_MARKER) {
            warnings.push(CompletenessWarning {
                rule: "vacuous_property_lowering".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "property '{}' lowered Rust contains \
                     QEDGEN_UNSUPPORTED_QUANTIFIER — the harness emits a `true` \
                     body and skips the real check",
                    prop.name
                ),
                subject: Some(prop.name.clone()),
                fix: "Rewrite the quantifier in a shape qedgen can lower \
                      (see docs/limitations.md#unsupported-quantifier-shapes) \
                      or split the property into per-element guards."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            continue;
        }

        // Rule 3 — unconditional: bare `true` body.
        if trimmed == "true" {
            warnings.push(CompletenessWarning {
                rule: "vacuous_property_lowering".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "property '{}' lowered to the literal `true` — the harness \
                     can never fail. Check the spec body and re-run check.",
                    prop.name
                ),
                subject: Some(prop.name.clone()),
                fix: "Inspect the property body for a spec construct that \
                      lowered to a constant. If the property is genuinely \
                      trivial, remove it; otherwise file a codegen bug."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            continue;
        }

        // Rule 1 — AST-gated; without the gate this would fire on
        // author-written tautologies (`state.admin == state.admin`
        // field-tracking), which the lint must not override.
        let Some(ast) = &prop.ast_body else {
            continue;
        };
        if !crate::chumsky_adapter::expr_contains_old(ast) {
            continue;
        }
        let Some((lhs, _op, rhs)) = parse_top_level_cmp(trimmed) else {
            continue;
        };
        if lhs == rhs {
            warnings.push(CompletenessWarning {
                rule: "vacuous_property_lowering".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "property '{}' uses `old(...)` but lowered Rust collapses to a \
                     structural tautology (`{} {} {}`). The temporal marker was \
                     dropped during lowering — this indicates a codegen regression.",
                    prop.name, lhs, _op, rhs
                ),
                subject: Some(prop.name.clone()),
                fix: "File a qedgen issue with the spec snippet. Pre-v2.23 this \
                      was the default behavior for `old(...)` in proptest/Kani; \
                      post-Slices 2-4 it should be unreachable."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// Split a rendered Rust comparison `<lhs> <op> <rhs>` at the top-level
/// comparison operator (string-level, no AST). Top-level = not inside
/// parens, generic args (`Vec<...>`), or `[...]` indices; first depth-0
/// comparison wins, with `==`/`!=`/`<=`/`>=` matched before `<`/`>`.
/// `None` if the expression isn't a top-level comparison.
fn parse_top_level_cmp(expr: &str) -> Option<(&str, &str, &str)> {
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'(' | b'[' | b'<' => {
                // `<` could be the comparison or the start of a generic.
                // Heuristic: if the next char is `=`, it's `<=` — handle
                // below. Otherwise treat `<` as depth-increment only when
                // preceded by an alphanumeric (generic) or whitespace
                // around a punctuation form is the comparison case.
                if b == b'<' {
                    let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                    let next = if i + 1 < bytes.len() {
                        bytes[i + 1]
                    } else {
                        b' '
                    };
                    // `<=` — comparison
                    if next == b'=' && depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 2..].trim();
                        return Some((lhs, "<=", rhs));
                    }
                    // bare `<` at depth 0 after an identifier could be a
                    // generic-list start (e.g. `Vec<u8>`). Treat as depth
                    // increment in that case.
                    if prev.is_ascii_alphanumeric() || prev == b'_' {
                        depth += 1;
                    } else if depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 1..].trim();
                        return Some((lhs, "<", rhs));
                    }
                } else {
                    depth += 1;
                }
            }
            b')' | b']' | b'>' => {
                if b == b'>' {
                    let next = if i + 1 < bytes.len() {
                        bytes[i + 1]
                    } else {
                        b' '
                    };
                    if next == b'=' && depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 2..].trim();
                        return Some((lhs, ">=", rhs));
                    }
                    if depth > 0 {
                        depth -= 1;
                    } else if depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 1..].trim();
                        return Some((lhs, ">", rhs));
                    }
                } else if depth > 0 {
                    depth -= 1;
                }
            }
            b'=' => {
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' && depth == 0 {
                    let lhs = expr[..i].trim();
                    let rhs = expr[i + 2..].trim();
                    return Some((lhs, "==", rhs));
                }
            }
            b'!' => {
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' && depth == 0 {
                    let lhs = expr[..i].trim();
                    let rhs = expr[i + 2..].trim();
                    return Some((lhs, "!=", rhs));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `old_in_single_state_context`: P1 when `Expr::Old(_)` appears in a
/// `requires` clause or `invariant` body. Both describe a single state —
/// no transition has happened, so there is no "old" value; the right
/// constructs are `ensures` / `property … preserved_by …`. Left alone,
/// Lean renders guillemet-quoted `«old(...)»` (type-fails downstream) and
/// Rust silently drops the marker. Synthetic requires (match-arm
/// desugaring) carry `ast_body: None` and are skipped — no source to fix.
fn check_old_in_single_state_context(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        for req in &op.requires {
            let Some(ast) = &req.ast_body else { continue };
            if crate::chumsky_adapter::expr_contains_old(ast) {
                warnings.push(make_old_in_single_state_warning(
                    &op.name,
                    "requires",
                    &req.rust_expr,
                ));
            }
        }
    }
    for inv in &spec.invariants {
        let Some(ast) = &inv.ast_body else { continue };
        if crate::chumsky_adapter::expr_contains_old(ast) {
            let body_display = inv.lean_expr.as_deref().unwrap_or("(body)");
            warnings.push(make_old_in_single_state_warning(
                &inv.name,
                "invariant",
                body_display,
            ));
        }
    }
    warnings
}

/// `type Error = { ... }` (record brace form) parses as a `Record` named
/// `Error` with `error_codes` left empty, so every error-variant consumer
/// (`WrongState` gate, `MathOverflow` check) misbehaves silently. P0
/// pointing at the pipe form; also fires when both forms are declared
/// (signals user confusion).
fn check_error_declared_as_record(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let has_error_record = spec.records.iter().any(|r| r.name == "Error");
    if !has_error_record {
        return warnings;
    }
    let fields_hint = spec
        .records
        .iter()
        .find(|r| r.name == "Error")
        .map(|r| {
            r.fields
                .iter()
                .map(|(n, _)| format!("  | {}", n))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "  | InvalidAmount\n  | Unauthorized".to_string());
    warnings.push(CompletenessWarning {
        rule: "error_declared_as_record".to_string(),
        severity: Severity::Error,
        priority: 0,
        message: "`type Error = { ... }` (record brace form) does not declare error \
                  variants — the parser treats it as a struct named `Error` and \
                  `spec.error_codes` ends up empty. Downstream lowering then \
                  misbehaves silently (CPI error refs unresolved, `WrongState` / \
                  `MathOverflow` gates don't fire)."
            .to_string(),
        subject: Some("Error".to_string()),
        fix: "Use the pipe form instead of `= { ... }`. Each variant goes on its \
              own line with a leading `|`."
            .to_string(),
        example: Some(format!("  type Error\n{}", fields_hint)),
        counterexample: None,
        fix_options: vec![],
    });
    warnings
}

/// Predicate shared with `kani_impl::spec_triggers_impl_harness`: true iff
/// a ref_impl carries arithmetic that could overflow on bounded Rust types
/// (the Lean lowering on `Nat`/`Int` cannot). Used as both a lint trigger
/// and the impl-targeted Kani auto-trigger so ref_impl-bearing specs always
/// get the bit-width-bounded verification surface.
pub fn ref_impl_has_overflow_risk(r: &ParsedRefImpl) -> bool {
    let has_numeric_io = std::iter::once(&r.return_type)
        .chain(r.params.iter().map(|(_, t)| t))
        .any(|t| {
            matches!(
                t.trim(),
                "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128"
            )
        });
    if !has_numeric_io {
        return false;
    }
    // Pure-expression bodies — `*` is always multiplication, `<<` is always
    // left-shift, `+`/`-` are always add/sub (no pointer arithmetic, no
    // unary `-` ambiguity in our DSL emission). A simple substring check
    // is sufficient and the lint's false-positive cost is "user is told
    // to run Kani" — tolerable.
    let body = &r.rust_body;
    body.contains('*') || body.contains("<<") || body.contains('+') || body.contains('-')
}

fn check_ref_impl_unbounded_arith(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for r in &spec.ref_impls {
        if !ref_impl_has_overflow_risk(r) {
            continue;
        }
        let mut ops: Vec<&str> = Vec::new();
        if r.rust_body.contains('*') {
            ops.push("*");
        }
        if r.rust_body.contains("<<") {
            ops.push("<<");
        }
        if r.rust_body.contains('+') {
            ops.push("+");
        }
        if r.rust_body.contains('-') {
            ops.push("-");
        }
        warnings.push(CompletenessWarning {
            rule: "ref_impl_unbounded_arith".to_string(),
            severity: Severity::Info,
            priority: 2,
            message: format!(
                "ref_impl '{}' uses {} over bounded-numeric params/return. \
                 Lean lowers this to `Nat`/`Int` (unbounded — no overflow), \
                 but the generated Rust runs on `u64`/`i64`/etc. where the \
                 same expression can wrap (release) or panic (debug). \
                 Bounded-arithmetic verification lives in Kani.",
                r.name,
                ops.join("/"),
            ),
            subject: Some(r.name.clone()),
            fix: "Run `qedgen verify --kani` against the generated impl-targeted \
                Kani harness — auto-emitted starting v2.26 whenever a ref_impl \
                trips this lint. The harness drives every numeric param with \
                `kani::any()` and produces a concrete counterexample at the \
                bit-width boundary."
                .to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

fn check_unconstrained_modifies(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for h in &spec.handlers {
        let Some(modifies) = h.modifies.as_ref() else {
            continue;
        };
        // Set of bare field names written by the effect block.
        let mut effect_fields: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (lhs, _, _) in &h.effects {
            // Strip a leading `Variant.` prefix (multi-variant ADT specs
            // use Variant-qualified LHS) and any `[idx]` subscript so the
            // bare field name lines up with the modifies list.
            let stripped = lhs
                .split_once('.')
                .map(|(_, rest)| rest)
                .unwrap_or(lhs.as_str());
            let bare = stripped.split('[').next().unwrap_or(stripped);
            effect_fields.insert(bare);
        }
        for field in modifies {
            if effect_fields.contains(field.as_str()) {
                continue;
            }
            // Does any ensures clause reference this field by name?
            // Conservative textual scan — `rust_expr` carries `post.<field>`
            // / `pre.<field>` / `s.<field>` depending on opts. Substring
            // match is fine because field names are user-declared and
            // bounded; false positives (`field` substring of another
            // field) are caught by the codegen lint when emitting the
            // fill site.
            let referenced = h
                .ensures
                .iter()
                .any(|e| e.rust_expr.contains(field.as_str()));
            if referenced {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "unconstrained_modifies".to_string(),
                severity: Severity::Error,
                priority: 0,
                message: format!(
                    "handler '{}' lists '{}' in `modifies` but no `effect` writes \
                     it and no `ensures` clause references it — the field is \
                     completely unconstrained. Verification harnesses have no \
                     contract to check against and the Lean frame conditions \
                     allow any post-value.",
                    h.name, field
                ),
                subject: Some(h.name.clone()),
                fix: format!(
                    "Either add an `ensures` clause that constrains `{}` against \
                     its pre-state value (so Kani / proptest can verify the impl \
                     satisfies the contract), or remove `{}` from `modifies` if \
                     it isn't really being modified.",
                    field, field
                ),
                example: Some(format!(
                    "  ensures {}_grew : state.{} >= old(state.{})",
                    field, field, field
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// Extract `pre.<field>` / `post.<field>` references from a
/// `rust_expr_binary`-rendered expression. The binary-mode renderer is the
/// only source of these tokens, so a static regex is sufficient and stable.
/// `pre.X` and `post.X` both normalize to `X` — the Kani impl harness reads
/// both from the same snapshot pair, so either binds the same locals.
pub fn extract_pre_post_field_refs(expr: &str) -> std::collections::BTreeSet<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        // Word-boundary at the start ensures `xpre.foo` doesn't match.
        Regex::new(r"\b(?:pre|post)\.([A-Za-z_][A-Za-z0-9_]*)").expect("static regex")
    });
    let mut fields = std::collections::BTreeSet::new();
    for cap in RE.captures_iter(expr) {
        fields.insert(cap[1].to_string());
    }
    fields
}

/// Per-handler predicate shared by `check.rs` (lint) and `kani_impl.rs`
/// (breadcrumb comment). For each unordered call pair whose callees resolve
/// in `spec.interfaces`, runs the same substitution as
/// `emit_cpi_ensures_as_assume` and reports `pre.X` / `post.X` references
/// appearing in both callees' substituted ensures. Tier-0 callees are
/// silent. Returns `(call_i_label, call_j_label, shared_field)` triples;
/// label format `Iface.handler` mirrors the harness CPI-block comment.
pub fn multi_cpi_shared_fields(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
) -> Vec<(String, String, String)> {
    // Resolve every call's substituted-ensures field set up front. Tier-0
    // / unresolved callees get an empty set and effectively drop out of the
    // pairwise compare.
    let resolved: Vec<(String, std::collections::BTreeSet<String>)> = handler
        .calls
        .iter()
        .map(|call| {
            let label = format!("{}.{}", call.target_interface, call.target_handler);
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                return (label, std::collections::BTreeSet::new());
            };
            let Some(callee) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                return (label, std::collections::BTreeSet::new());
            };
            let mut fields = std::collections::BTreeSet::new();
            for ens in &callee.ensures {
                let substituted = crate::cpi_substitute::substitute_callee_ensures_rust_binary(
                    &ens.rust_expr_binary,
                    call,
                    &callee.params,
                    callee.result_binder.as_deref(),
                );
                fields.extend(extract_pre_post_field_refs(&substituted));
            }
            (label, fields)
        })
        .collect();

    let mut findings = Vec::new();
    for i in 0..resolved.len() {
        if resolved[i].1.is_empty() {
            continue;
        }
        for j in (i + 1)..resolved.len() {
            if resolved[j].1.is_empty() {
                continue;
            }
            if disjoint_token_transfer_resources(&handler.calls[i], &handler.calls[j]) {
                continue;
            }
            // Set intersection ordered by BTreeSet iteration (stable
            // alphabetical for deterministic lint output).
            for field in resolved[i].1.intersection(&resolved[j].1) {
                findings.push((resolved[i].0.clone(), resolved[j].0.clone(), field.clone()));
            }
        }
    }
    findings
}

fn disjoint_token_transfer_resources(left: &ParsedCall, right: &ParsedCall) -> bool {
    fn token_transfer_resources(call: &ParsedCall) -> Option<std::collections::BTreeSet<String>> {
        if call.target_interface != "Token" || call.target_handler != "transfer" {
            return None;
        }

        let mut resources = std::collections::BTreeSet::new();
        for arg_name in ["from", "to"] {
            let arg = call.args.iter().find(|arg| arg.name == arg_name)?;
            resources.insert(arg.rust_expr.trim().to_string());
        }
        Some(resources)
    }

    let Some(left_resources) = token_transfer_resources(left) else {
        return false;
    };
    let Some(right_resources) = token_transfer_resources(right) else {
        return false;
    };
    left_resources.is_disjoint(&right_resources)
}

/// P2 informational lint for the multi-CPI ordering gap; one warning per
/// shared field per call pair.
fn check_multi_cpi_same_field(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        let findings = multi_cpi_shared_fields(spec, handler);
        for (call_i_label, call_j_label, field) in findings {
            warnings.push(CompletenessWarning {
                rule: "multi_cpi_same_field".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{}' makes multiple CPI calls ({} and {}) whose \
                     substituted ensures both reference '{}'. Kani's impl-targeted \
                     harness has only one (pre_{}, post_{}) snapshot pair captured \
                     at handler boundary; both assumes will fire at the same splice \
                     point, which can over-constrain.",
                    handler.name, call_i_label, call_j_label, field, field, field
                ),
                subject: Some(handler.name.clone()),
                fix: "Until per-call snapshot frames land (v3.0), either: (1) \
                      merge the CPI calls into a single helper handler whose \
                      ensures captures the combined effect; (2) tighten each \
                      callee's ensures so they reference disjoint fields; or \
                      (3) split the multi-CPI handler into separate handlers \
                      (one per CPI) so each gets its own (pre, post) snapshot."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

fn make_old_in_single_state_warning(
    holder: &str,
    kind: &str,
    body_snippet: &str,
) -> CompletenessWarning {
    CompletenessWarning {
        rule: "old_in_single_state_context".to_string(),
        severity: Severity::Warning,
        priority: 1,
        message: format!(
            "'{}' uses `old(...)` inside a `{}` body ({}) — only meaningful in \
             `ensures` or `property` bodies (a binary transition context). \
             `requires` and `invariant` describe a single state and have no \
             \"old\" value to reference.",
            holder, kind, body_snippet
        ),
        subject: Some(holder.to_string()),
        fix: "If you meant a precondition on the pre-state, drop `old(...)` \
              and reference `state.x` directly. If you meant a property across \
              the transition, lift the clause into a `property X : ... \
              preserved_by Y`."
            .to_string(),
        example: None,
        counterexample: None,
        fix_options: vec![],
    }
}

/// `[missing_math_overflow]`: checked effects (`+=` / `-=`) lower to
/// `checked_add` / `checked_sub` returning `<ProgramName>Error::MathOverflow`
/// / `::MathUnderflow`; without the variant declared, the generated code
/// fails `cargo build` with "unknown variant" — surface at lint time.
/// Per-effect overrides and pragma defaults defer to
/// `check_unknown_error_variant`. Back-compat fallback honored: declared
/// `MathOverflow` but not `MathUnderflow` → `-=` raises `MathOverflow`.
fn check_checked_arith_needs_math_overflow(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let has_overflow = has_decl("MathOverflow");
    let has_underflow = has_decl("MathUnderflow");
    let pragma_overflow = spec.pragma_value("checked_overflow_error");
    let pragma_underflow = spec.pragma_value("checked_underflow_error");

    // Collect handlers whose builtin-default lowering would reference a
    // variant the spec didn't declare. Per-site overrides skip this lint
    // (their variant check lives in `check_unknown_error_variant`).
    let mut missing: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    let mut handlers_missing: Vec<String> = Vec::new();

    for h in &spec.handlers {
        let mut handler_fires = false;
        for (idx, (_, op_kind, _)) in h.effects.iter().enumerate() {
            let on_error = h.effect_on_error.get(idx).and_then(|o| o.as_deref());
            if on_error.is_some() {
                continue; // per-site override handled elsewhere
            }
            match op_kind.as_str() {
                "add" => {
                    if pragma_overflow.is_some() {
                        continue;
                    }
                    if !has_overflow {
                        missing.insert("MathOverflow");
                        handler_fires = true;
                    }
                }
                "sub" => {
                    if pragma_underflow.is_some() {
                        continue;
                    }
                    // Back-compat: declared MathOverflow but not
                    // MathUnderflow → `-=` falls back to MathOverflow.
                    if has_underflow {
                        continue;
                    }
                    if has_overflow {
                        continue; // back-compat path
                    }
                    missing.insert("MathUnderflow");
                    handler_fires = true;
                }
                _ => {}
            }
        }
        if handler_fires {
            handlers_missing.push(h.name.clone());
        }
    }

    if missing.is_empty() {
        return Vec::new();
    }
    let names = handlers_missing.join(", ");
    let variants_list: Vec<String> = missing.iter().map(|s| s.to_string()).collect();
    let variants = variants_list.join(" / ");
    let fix_block = variants_list
        .iter()
        .map(|v| format!("      | {}", v))
        .collect::<Vec<_>>()
        .join("\n");
    vec![CompletenessWarning {
        rule: "missing_math_overflow".to_string(),
        severity: Severity::Warning,
        priority: 2,
        message: format!(
            "handler(s) [{}] use checked-arithmetic effects (`+=` / `-=`), but `type Error` doesn't declare a `{}` variant. The generated Rust references `{}Error::{}` and won't compile without it.",
            names,
            variants,
            crate::codegen_shared::to_pascal_case(&spec.program_name),
            variants,
        ),
        subject: None,
        fix: format!(
            "Add `{}` to your `type Error | …` block. Example:\n\n    type Error\n{}\n      | …\n\nOr opt out of checked semantics per-effect with `+=!` (saturating) or `+=?` (wrapping), or override the variant inline with `pool += amount else MyVariant`.",
            variants, fix_block,
        ),
        example: None,
        counterexample: None,
        fix_options: vec![],
    }]
}

/// `unknown_error_variant`: a per-site `or X` override or checked_overflow/
/// underflow pragma references a variant not declared in `type Error | …` —
/// the generated Rust references `<ProgramName>Error::X` and won't compile.
fn check_unknown_error_variant(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let mut warnings = Vec::new();

    // Pragma references — fire once per pragma, not once per handler.
    for (key, value) in &spec.pragma_assignments {
        if (key == "checked_overflow_error" || key == "checked_underflow_error") && !has_decl(value)
        {
            warnings.push(CompletenessWarning {
                rule: "unknown_error_variant".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "`pragma {} = {}` references a variant absent from `type Error | …`. Generated Rust references `{}Error::{}` and won't compile.",
                    key,
                    value,
                    crate::codegen_shared::to_pascal_case(&spec.program_name),
                    value,
                ),
                subject: Some(value.clone()),
                fix: format!(
                    "Add `{}` to your `type Error | …` block, drop the pragma, or replace it with a declared variant name.",
                    value,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Per-site `or X` references.
    for h in &spec.handlers {
        for on_error in h.effect_on_error.iter().flatten() {
            if !has_decl(on_error) {
                warnings.push(CompletenessWarning {
                    rule: "unknown_error_variant".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "handler '{}' has an effect with `else {}` referencing a variant absent from `type Error | …`. Generated Rust references `{}Error::{}` and won't compile.",
                        h.name,
                        on_error,
                        crate::codegen_shared::to_pascal_case(&spec.program_name),
                        on_error,
                    ),
                    subject: Some(h.name.clone()),
                    fix: format!(
                        "Add `{}` to your `type Error | …` block, drop the `else {}` suffix to fall back to the default, or use a declared variant.",
                        on_error, on_error,
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }
    warnings
}

/// `[wrapping_arithmetic]` / `[saturating_arithmetic]` — explicit
/// non-default arithmetic opt-ins (default `+=` / `-=` is checked):
///
/// - **Wrapping** (`+=?` / `-=?`): silent overflow modulo 2^N; almost always
///   wrong on monetary amounts. Warning, P1.
/// - **Saturating** (`+=!` / `-=!`): caps at MAX/MIN, hiding bugs that should
///   error; sometimes legitimate (rate limiters, epoch counters). Info, P2.
///
/// Lives in check, not probe: a real structural pattern but a spec-authoring
/// concern, not a reproducible vulnerability (probe ships reproducer-bearing
/// findings only).
fn check_wrapping_arithmetic_opt_in(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        for (field, kind, _value) in &op.effects {
            let (severity, priority, label, default_op) = match kind.as_str() {
                "add_wrap" => (Severity::Warning, 1, "wrapping", "+="),
                "sub_wrap" => (Severity::Warning, 1, "wrapping", "-="),
                "add_sat" => (Severity::Info, 2, "saturating", "+="),
                "sub_sat" => (Severity::Info, 2, "saturating", "-="),
                _ => continue,
            };
            warnings.push(CompletenessWarning {
                rule: format!("{}_arithmetic", label),
                severity,
                priority,
                message: format!(
                    "handler `{}` uses {} arithmetic on `{}` (op `{}`) — silent overflow {}. Default `{}` (checked) aborts on overflow.",
                    op.name,
                    label,
                    field,
                    kind,
                    if label == "wrapping" { "modulo 2^N" } else { "saturating to MAX/MIN" },
                    default_op,
                ),
                subject: Some(format!("{}::{}::{}", op.name, field, kind)),
                fix: format!(
                    "If the {label} semantic is intentional (epoch wrap, rate limiter), document the invariant inline. Otherwise change `{kind}` to `{default_op}` (checked) — the spec's `type Error` block must declare `MathOverflow`.",
                    label = label,
                    kind = kind,
                    default_op = default_op,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// True iff this handler's `auth X` will be lowered to `has_one = X` by
/// R25 — that is, `X` is a field on a state account this handler
/// touches. Used by terminal-transition and value-transfer lints to
/// avoid false positives on auth-bound handlers (the signer identity
/// IS the gate).
fn r25_will_bind_auth(handler: &ParsedHandler, spec: &ParsedSpec) -> bool {
    let Some(ref who) = handler.who else {
        return false;
    };
    if spec.account_types.is_empty() {
        return spec.state_fields.iter().any(|(n, _)| n == who);
    }
    spec.account_types
        .iter()
        .any(|at| at.fields.iter().any(|(n, _)| n == who))
}

// ============================================================================
// Spec-authoring lints (audit follow-up)
//
// Complement codegen fixes R25–R28 by surfacing the *spec shapes* behind
// under-specified auth, value transfer, and lifecycle transitions. Each
// lint maps 1:1 to a post-codegen audit finding; catching them at check
// time means routine spec gaps don't wait for an auditor invocation.
// ============================================================================

/// `[cross_adt_field_ambiguity]` — multi-ADT spec has a property whose
/// expression mentions a bare field name that's declared in 2+ account
/// types, and the reference isn't qualified by an account prefix. Codegen
/// then assigns the property to every ADT module whose field set the
/// expression substring-matches, which silently produces duplicate (and
/// usually wrong) predicates.
///
/// Lint, don't auto-qualify: auto-qualification would silently pick the
/// first-matching ADT and can wedge invariants against the wrong State.
fn check_cross_adt_field_ambiguity(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    if spec.account_types.len() < 2 {
        return warnings;
    }

    // Build field_name → Vec<account_name>. Keep only fields declared on
    // 2+ account types (the ambiguous set).
    let mut field_to_adts: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for acct in &spec.account_types {
        for (fname, _) in &acct.fields {
            field_to_adts
                .entry(fname.as_str())
                .or_default()
                .push(acct.name.as_str());
        }
    }
    field_to_adts.retain(|_, adts| adts.len() >= 2);
    if field_to_adts.is_empty() {
        return warnings;
    }

    let adt_prefixes: Vec<String> = spec
        .account_types
        .iter()
        .map(|a| format!("{}.", a.name.to_lowercase()))
        .collect();

    // Walk every property's expression. For each ambiguous field, check
    // for word-boundary references that are NOT already qualified by an
    // ADT-name prefix or by `state.` (state.X means "the implicit single
    // State", which is itself ambiguous in multi-ADT mode — flag it too).
    for prop in &spec.properties {
        let Some(ref expr) = prop.expression else {
            continue;
        };
        for (&field, adts) in &field_to_adts {
            // Quick reject: no occurrence of the field name anywhere.
            if !expr.contains(field) {
                continue;
            }
            // Walk every word-boundary position where `field` appears.
            // A reference is "qualified" if the immediately-preceding
            // character is a `.` AND the preceding identifier matches
            // one of the lowercase ADT names (`<adt>.<field>`).
            let bytes = expr.as_bytes();
            let needle = field.as_bytes();
            let mut idx = 0;
            let mut any_unqualified = false;
            while let Some(rel) = expr[idx..].find(field) {
                let start = idx + rel;
                let end = start + needle.len();
                // Word-boundary check: not preceded/followed by identifier chars.
                let pre_is_ident = start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
                let post_is_ident =
                    end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
                if !pre_is_ident && !post_is_ident {
                    // Is this an `<adt>.<field>` reference?
                    let qualified = adt_prefixes.iter().any(|p| {
                        let p_bytes = p.as_bytes();
                        start >= p_bytes.len()
                            && bytes[start - p_bytes.len()..start].eq_ignore_ascii_case(p_bytes)
                    });
                    if !qualified {
                        any_unqualified = true;
                        break;
                    }
                }
                idx = end;
            }
            if !any_unqualified {
                continue;
            }
            let adt_list = adts.join(", ");
            let first_adt_lower = adts[0].to_lowercase();
            warnings.push(CompletenessWarning {
                rule: "cross_adt_field_ambiguity".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "property '{}' references field `{}` which is declared in multiple account types ({}); codegen will emit the predicate inside every matching module",
                    prop.name, field, adt_list,
                ),
                subject: Some(prop.name.clone()),
                fix: format!(
                    "Qualify the reference with the owning account type (e.g. `{}.{}`), or split the property into one per account type.",
                    first_adt_lower, field,
                ),
                example: Some(format!(
                    "  property {} \"...\"\n    {}.{} >= 0",
                    prop.name, first_adt_lower, field,
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// `[unbound_auth]` — `auth X` doesn't match a state field, so codegen's
/// `auth → has_one` lowering (R25) can't fire. The signer check verifies
/// "someone signed," not "the right someone."
///
/// Closed by R25 when `X` IS a state field. Catches the percolator-CRIT
/// shape — auth name without a state-side anchor.
fn check_unbound_auth(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        if handler.permissionless {
            continue;
        }
        let Some(ref who) = handler.who else {
            // `no_access_control` already covers the no-auth case; don't
            // double-flag.
            continue;
        };
        // Skip handlers without a discoverable state account — single-
        // signer admin handlers without state aren't this lint's target.
        if handler.accounts.is_empty() {
            continue;
        }
        // The state-bearing account in this handler — same logic as
        // codegen.rs::find_state_account, but we only need to know
        // *whether* one exists for field lookup. A handler with multiple
        // state candidates falls back to single-state field set.
        let has_who_field = if spec.account_types.is_empty() {
            spec.state_fields.iter().any(|(n, _)| n == who)
        } else {
            spec.account_types
                .iter()
                .any(|at| at.fields.iter().any(|(n, _)| n == who))
        };
        if has_who_field {
            continue;
        }
        // The auth name might still have a state-side binding via an
        // explicit `requires` clause. If any `requires` references both
        // `who` and a state field, treat the spec as deliberately
        // self-binding and skip the warning.
        let manually_bound = handler
            .requires
            .iter()
            .any(|r| r.lean_expr.contains(who) && r.lean_expr.contains("s."));
        if manually_bound {
            continue;
        }
        // Also accept the dotted-auth desugar / cross-program shape, where
        // the binding clause reads an imported-account field: pattern
        // `<acct>.<field> = <who>.pubkey` (Lean form) with `<acct>` a
        // non-signer account. Covers both the `auth <acct>.<field>` sugar
        // (adapt() rewrites it to this synthesized clause) and the
        // hand-written `requires` longhand.
        let who_pubkey = format!("{who}.pubkey");
        let auth_bound_via_account = handler.requires.iter().any(|r| {
            if !r.lean_expr.contains(&who_pubkey) {
                return false;
            }
            handler
                .accounts
                .iter()
                .any(|a| !a.is_signer && r.lean_expr.contains(&format!("{}.", a.name)))
        });
        if auth_bound_via_account {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unbound_auth".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' declares `auth {who}` but no state field is named `{who}`. R25's `auth → has_one` lowering only fires when the auth name matches a state field — as written, any signer can call this handler against any program-owned account.",
                handler = handler.name,
                who = who,
            ),
            subject: Some(handler.name.clone()),
            fix: format!(
                "Either (a) add `{who} : Pubkey` to the state account so codegen emits `has_one = {who}`, (b) add an explicit `requires state.<field> == {who} else Unauthorized` clause that binds the signer to a stored value, or (c) mark the handler `permissionless` if it's deliberately open.",
                who = who,
            ),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[unguarded_indexed_mutation]` — handler takes an index parameter
/// and mutates `state.<map>[i]`, but no `requires` binds the index to
/// the signer. Catches the multisig::approve/reject shape — anyone can
/// vote with any `member_index` because the spec doesn't tie the index
/// to the signer's pubkey.
fn check_unguarded_indexed_mutation(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        if handler.permissionless {
            continue;
        }
        let Some(ref who) = handler.who else {
            continue;
        };
        // Index-shaped params (Fin[N], U8/U16/U32 used for indexing).
        // We accept any unsigned int as a candidate; the trigger is
        // whether the param actually appears as an index in an effect's
        // LHS.
        let index_params: Vec<&str> = handler
            .takes_params
            .iter()
            .filter(|(_, t)| {
                let tt = t.trim();
                tt.starts_with("Fin") || matches!(tt, "U8" | "U16" | "U32" | "U64")
            })
            .map(|(n, _)| n.as_str())
            .collect();
        if index_params.is_empty() {
            continue;
        }
        // Does any effect LHS use one of the index params?
        let mut indexed_effect_param: Option<&str> = None;
        for (lhs, _, _) in &handler.effects {
            for p in &index_params {
                let needle = format!("[{}]", p);
                if lhs.contains(&needle) {
                    indexed_effect_param = Some(p);
                    break;
                }
            }
            if indexed_effect_param.is_some() {
                break;
            }
        }
        let Some(idx_param) = indexed_effect_param else {
            continue;
        };
        // Is there a requires that binds `who` to `state.<map>[<idx_param>]`?
        let has_binding = handler.requires.iter().any(|r| {
            let e = r.lean_expr.as_str();
            e.contains(who) && e.contains(&format!("[{}]", idx_param))
        });
        if has_binding {
            continue;
        }
        // R25 has_one binding counts as a gate too. When the auth name
        // matches a state field, only that pubkey can drive the
        // handler — so the indexed mutation IS gated, just by signer
        // identity rather than by the index itself. Multisig::add_member
        // is the canonical shape: the creator sets `members[i]`,
        // `auth creator` + `has_one = creator` binds the writer.
        if r25_will_bind_auth(handler, spec) {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unguarded_indexed_mutation".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' takes index `{idx} : <int>` and mutates `state.<map>[{idx}]`, but no `requires` clause binds `{idx}` to the signer `{who}`. As written, any signer can drive the indexed mutation against any slot — the only existing check is the bounds (`{idx} < bound`), which rules out out-of-range but not unauthorized writes.",
                handler = handler.name,
                idx = idx_param,
                who = who,
            ),
            subject: Some(handler.name.clone()),
            fix: format!(
                "Add a `requires` clause that ties `{idx}` to `{who}`, e.g.:\n\n    requires state.members[{idx}] == {who} else NotAMember\n\nWithout it, `{idx}` is just a number the caller picks.",
                idx = idx_param,
                who = who,
            ),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[scalar_counter_no_dedup]` — handler increments a scalar counter
/// (e.g. `approval_count += 1`) bounded by another scalar
/// (e.g. `approval_count + rejection_count < member_count`), but the
/// spec has no per-actor tracking field that prevents the same actor
/// from voting multiple times. Catches the dedup arm of the multisig
/// approve/reject HIGH.
fn check_scalar_counter_no_dedup(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    // Map field names whose type starts with Bool/U8 + "Map[" — the kinds
    // of fields users add for per-actor dedup (`voted : Map[N] U8`,
    // `processed : Map[N] Bool`).
    let has_dedup_shaped_field = |spec: &ParsedSpec| -> bool {
        let by_state = spec.state_fields.iter();
        let by_account = spec.account_types.iter().flat_map(|at| at.fields.iter());
        by_state.chain(by_account).any(|(_, t)| {
            let tt = t.trim();
            tt.starts_with("Map[") && (tt.ends_with("Bool") || tt.ends_with("U8"))
        })
    };
    if has_dedup_shaped_field(spec) {
        // Spec already has at least one dedup-shaped field — assume the
        // user has thought about this and skip. (If they have one but
        // forgot to use it, that's a separate concern.)
        return warnings;
    }
    for handler in &spec.handlers {
        for (lhs, op_kind, _) in &handler.effects {
            if op_kind != "add" {
                continue;
            }
            // Scalar increment — no subscript on the LHS.
            if lhs.contains('[') {
                continue;
            }
            // Is the incremented field bounded by ANOTHER STATE FIELD
            // in any requires clause? Const-bounded scalars (TVL caps,
            // overflow guards) don't fit this lint's shape — the
            // multisig pattern is specifically "this counter ceiling
            // is itself a state field" (`approval_count + ... <
            // member_count`), where the ceiling is per-vault dynamic
            // data and per-actor dedup is the missing piece.
            let bounded_by_state = handler.requires.iter().any(|r| {
                let e = &r.lean_expr;
                if !e.contains(lhs.as_str()) {
                    return false;
                }
                if !e.contains('<') && !e.contains('≤') {
                    return false;
                }
                // At least two distinct state-field references
                // (ours + at least one other on the bound side).
                e.matches("s.").count() >= 2 || e.matches("state.").count() >= 2
            });
            if !bounded_by_state {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "scalar_counter_no_dedup".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{handler}' increments scalar counter `{lhs}` toward an existing bound, but the spec has no per-actor record (e.g. `voted : Map[N] U8`) preventing the same actor from incrementing across different signer pubkeys.",
                    handler = handler.name,
                    lhs = lhs,
                ),
                subject: Some(handler.name.clone()),
                fix: format!(
                    "Add a per-actor tracking field and a corresponding requires clause:\n\n    state.Active of {{ ... voted : Map[N] U8 ... }}\n\n    handler {handler} (i : U8) ... {{\n      requires state.voted[i] == 0 else AlreadyVoted\n      effect {{\n        {lhs} += 1\n        voted[i] := 1\n      }}\n    }}",
                    handler = handler.name,
                    lhs = lhs,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            // Only one warning per handler.
            break;
        }
    }
    warnings
}

/// `[unguarded_terminal_transition]` — handler transitions to a terminal
/// lifecycle state (a state that's not the post of any other handler,
/// or matches the heuristic terminal-name list) with no `requires`
/// clauses AND no R25-eligible auth binding. Catches the
/// lending::liquidate HIGH (anyone-can-liquidate).
fn check_unguarded_terminal_transition(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let terminal_name_heuristic: &[&str] = &[
        "Liquidated",
        "Closed",
        "Drained",
        "Cancelled",
        "Burned",
        "Settled",
        "Redeemed",
        "Finalized",
    ];
    for handler in &spec.handlers {
        let Some(ref post) = handler.post_status else {
            continue;
        };
        let is_named_terminal = terminal_name_heuristic.iter().any(|t| t == post);
        let is_structurally_terminal = !spec
            .handlers
            .iter()
            .any(|h| h.pre_status.as_deref() == Some(post.as_str()));
        if !is_named_terminal && !is_structurally_terminal {
            continue;
        }
        // Init handlers (Uninitialized → Active) aren't this lint's target —
        // a fresh-account creation transition with no requires is fine.
        let pre = handler.pre_status.as_deref().unwrap_or("");
        if matches!(pre, "Uninitialized" | "Empty") {
            continue;
        }
        if !handler.requires.is_empty() {
            continue;
        }
        // R25 has_one binding counts as a gate. If the handler's `auth X`
        // matches a state field, R25 emits `has_one = X` and only the
        // matching pubkey can trigger the transition. This is the
        // escrow::cancel / escrow::exchange shape — gated by signer
        // identity, no data precondition needed.
        if r25_will_bind_auth(handler, spec) {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unguarded_terminal_transition".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' transitions to terminal state `{post}` with no `requires` clauses. Terminal transitions usually need a guard — anyone with the right account shape can otherwise trigger the transition.",
                handler = handler.name,
                post = post,
            ),
            subject: Some(handler.name.clone()),
            fix: "Add a `requires` clause that gates the transition. For liquidation: a health threshold (`requires state.amount > state.collateral else AccountHealthy`). For closing: an empty-balance check (`requires state.balance == 0`). For settlement: a finality predicate.".to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[unconditional_value_transfer]` — handler has a `transfers` clause
/// where the source account is owned by program state (i.e. has
/// `authority X` with X being a handler-bound account that's program-
/// derived), AND the handler has no `requires` clause that constrains
/// who can call it. Catches the lending::liquidate vault-drain shape.
fn check_unconditional_value_transfer(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        for transfer in &handler.transfers {
            // Look up the `from` account in the handler's accounts list.
            // If it has a token authority that points at a writable
            // PDA-typed account in this handler, the source is program-
            // owned.
            let Some(from_acct) = handler.accounts.iter().find(|a| a.name == transfer.from) else {
                continue;
            };
            let Some(ref auth_name) = from_acct.authority else {
                continue;
            };
            let auth_is_program_owned = handler
                .accounts
                .iter()
                .any(|a| &a.name == auth_name && a.is_writable && a.pda_seeds.is_some());
            if !auth_is_program_owned {
                continue;
            }
            // Does the handler have a constraining requires beyond
            // amount-validity? We treat "amount > 0" / "amount < ..." as
            // not constraining caller identity.
            let has_caller_requires = handler.requires.iter().any(|r| {
                let e = &r.lean_expr;
                // Heuristic: caller-binding requires reference state.<field>
                // rather than just the amount param.
                e.contains("s.") || e.contains("state.")
            });
            if has_caller_requires {
                continue;
            }
            // R25 has_one binding counts as a caller gate — escrow::exchange
            // and ::cancel are both auth-bound (`auth taker` / `auth
            // initializer` matching state fields), so the transfer is
            // already gated by signer identity even without an explicit
            // `requires`.
            if r25_will_bind_auth(handler, spec) {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "unconditional_value_transfer".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{handler}' transfers from program-owned `{from}` (authority `{auth}`) with no `requires` clauses constraining who can call it. Value-extracting handlers usually need an authority binding or a precondition that gates the transfer.",
                    handler = handler.name,
                    from = transfer.from,
                    auth = auth_name,
                ),
                subject: Some(handler.name.clone()),
                fix: "Either bind the auth to a state field (so R25 emits `has_one = X`) or add a precondition that gates the transfer (e.g. health check, redemption ratio, allowance). Without one, any signer can extract value from the program-owned account.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            break; // one warning per handler
        }
    }
    warnings
}

/// `cpi_no_callee_ensures`: flags a call site whose interface handler has
/// no `ensures` — the caller's Lean proof carries `by sorry` (Tier-0
/// axiomatization) with no post-condition to discharge. Distinct from
/// `shape_only_cpi` (missing interface/handler declarations): this fires
/// on declared handlers that simply have no post-condition shape.
fn check_cpi_no_callee_ensures(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue; // shape_only_cpi handles undeclared interfaces.
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue; // shape_only_cpi handles undeclared handlers.
            };
            if !ih.ensures.is_empty() {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "cpi_no_callee_ensures".to_string(),
                severity: Severity::Info,
                priority: 1,
                message: format!(
                    "handler '{}' calls `{}.{}` — callee has no `ensures` clauses; \
                     caller's Lean theorem carries `by sorry` (Tier-0 axiomatization)",
                    handler.name, call.target_interface, call.target_handler,
                ),
                subject: Some(handler.name.clone()),
                fix: format!(
                    "Add at least one `ensures <expr>` inside `interface {} {{ handler {} {{ ... }} }}`, \
                     or commit to an `upstream {{ binary_hash = ... }}` pin on the interface so the \
                     caller can discharge via the bundled axiom module.",
                    call.target_interface, call.target_handler,
                ),
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* observable post-condition */\n    }}\n  }}",
                    call.target_interface, call.target_handler,
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// `cpi_unverified_callee`: callee has `ensures` but no imported proof
/// package. The caller still gets discharge via the bundled axiom (Stance
/// 1), but the trust anchor is "binary matches a pinned hash" rather than
/// "we have a proof against the callee's spec." Fires on bundled-stdlib
/// builtins (no proofs shipped) and external imports without
/// `<source>/.qed/proofs/<Iface>.lean` + `lakefile.lean`; suppressed when
/// `spec.verified_callees` has the interface. P2 advisory — `qedgen verify
/// --require-verified` escalates.
fn check_cpi_unverified_callee(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    // Only walk imports — in-spec interfaces declared inline by the
    // author aren't "callees" from a composition standpoint; they're
    // contracts the same author is committing to.
    let import_iface_names: std::collections::HashSet<&str> = spec
        .imports
        .iter()
        .map(|i| i.as_name.as_deref().unwrap_or(i.name.as_str()))
        .collect();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            if !import_iface_names.contains(call.target_interface.as_str()) {
                continue;
            }
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue;
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue;
            };
            if ih.ensures.is_empty() {
                // cpi_no_callee_ensures (P1) owns this case.
                continue;
            }
            if spec.verified_callees.contains_key(&iface.name) {
                continue;
            }
            // One warning per (interface, handler) pair — same call
            // site referenced from multiple handlers shouldn't fire N
            // times.
            let key = format!("{}.{}", iface.name, ih.name);
            if !seen.insert(key) {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "cpi_unverified_callee".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "import `{}` is unverified — `{}.{}` discharges via Stance-1 axiom (binary_hash pin) instead of an imported proof",
                    iface.name, iface.name, ih.name,
                ),
                subject: Some(iface.name.clone()),
                fix: format!(
                    "Ship a Lake-buildable proof package alongside the provider's qedspec at \
                     `<source>/.qed/proofs/{}.lean` (with a sibling `lakefile.lean` declaring \
                     `package {}`). The consumer's codegen will auto-detect the package and \
                     swap the caller's theorem from Stance 1 (axiom) to Stance 2 (imported proof).",
                    iface.name,
                    crate::lean_sidecars::proof_pkg_name(&iface.name),
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// One finding per imported interface that `qedgen verify
/// --require-verified` would reject; carries enough context for main.rs to
/// render a CRIT line and exit non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct UnverifiedCallee {
    pub interface_name: String,
    pub fix_hint: String,
}

/// `qedgen verify --require-verified` predicate. Yields one
/// [`UnverifiedCallee`] per imported interface that: was reached via
/// `import` (not declared inline); has at least one handler with non-empty
/// `ensures` (Tier-0 shape-only imports are exempt — `cpi_no_callee_ensures`
/// covers them); is absent from `spec.verified_callees`; and is NOT
/// sentinel-pinned (`sha256:00…00`). Sentinel-pinned native programs
/// (System) are documented runtime trust boundaries — their `ensures` are
/// discharged by the validator itself, so counting them "unverified" would
/// fail every spec that imports them. Empty vec = dep graph fully proven
/// from a Stance-2 standpoint; mirrors `check_cpi_unverified_callee`.
#[allow(dead_code)]
pub fn collect_require_verified_findings(spec: &ParsedSpec) -> Vec<UnverifiedCallee> {
    let import_iface_names: std::collections::HashSet<&str> = spec
        .imports
        .iter()
        .map(|i| i.as_name.as_deref().unwrap_or(i.name.as_str()))
        .collect();

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for iface in &spec.interfaces {
        if !import_iface_names.contains(iface.name.as_str()) {
            continue;
        }
        let has_ensures = iface.handlers.iter().any(|h| !h.ensures.is_empty());
        if !has_ensures {
            continue;
        }
        if spec.verified_callees.contains_key(&iface.name) {
            continue;
        }
        if iface
            .upstream
            .as_ref()
            .and_then(|u| u.binary_hash.as_deref())
            .map(crate::upstream_check::is_sentinel_hash)
            .unwrap_or(false)
        {
            continue;
        }
        if !seen.insert(iface.name.clone()) {
            continue;
        }
        let proof_pkg = crate::lean_sidecars::proof_pkg_name(&iface.name);
        results.push(UnverifiedCallee {
            interface_name: iface.name.clone(),
            fix_hint: format!(
                "provider must ship `<source>/.qed/proofs/{}.lean` + a sibling `lakefile.lean` \
                 declaring `package {}`. Run without --require-verified to accept Stance-1 \
                 axiom discharge instead.",
                iface.name, proof_pkg
            ),
        });
    }
    results
}

fn check_shape_only_cpi(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    for handler in &spec.handlers {
        for call in &handler.calls {
            let iface = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface);
            let target_handler =
                iface.and_then(|i| i.handlers.iter().find(|h| h.name == call.target_handler));

            let (reason, fix) = match (iface, target_handler) {
                (None, _) => (
                    format!(
                        "interface `{}` is not declared in this spec — the call compiles but has no contract",
                        call.target_interface
                    ),
                    format!(
                        "Declare `interface {} {{ ... }}` at the top level, or `qedgen interface --idl <path>` to scaffold one.",
                        call.target_interface
                    ),
                ),
                (Some(_), None) => (
                    format!(
                        "interface `{}` has no handler named `{}` — check for a typo or add the handler",
                        call.target_interface, call.target_handler
                    ),
                    format!(
                        "Add `handler {}` inside `interface {} {{ ... }}`, or update the call site to match a real handler.",
                        call.target_handler, call.target_interface
                    ),
                ),
                // Declared interface + declared handler: skip, even with no
                // `ensures`. Firing here pressured authors into `ensures
                // true` on shapes with no meaningful post-condition (Token
                // init / metadata-create / close); the import-level Tier
                // 0/1/2 signal already covers it.
                _ => continue,
            };

            warnings.push(CompletenessWarning {
                rule: "shape_only_cpi".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' calls `{}.{}` — {}",
                    handler.name, call.target_interface, call.target_handler, reason
                ),
                subject: Some(handler.name.clone()),
                fix,
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* what the callee guarantees */\n    }}\n  }}",
                    call.target_interface, call.target_handler
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    warnings
}

/// Parsed form of a field type string. Captures the distinction between a
/// plain type (e.g. `U128`, `Account`) and a bounded map (`Map[N] T`).
///
/// Only `Map { .. }` is inspected by the current consumer; `Simple` carries
/// the trimmed type string for future linting passes (e.g., primitive-type
/// checks, alias resolution) and intentionally remains exhaustive.
#[derive(Debug)]
enum FieldTypeShape<'a> {
    Simple(#[allow(dead_code)] &'a str),
    Map { bound: &'a str, inner: &'a str },
}

fn check_pda_collisions(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let pdas = &spec.pdas;

    // Classify a seed token: is it a literal/constant or a variable reference?
    // Seeds from the adapter: string literals are stored with surrounding quotes
    // (e.g. `"vault"`), named constants are ALL_CAPS, variables are lowercase idents.
    let is_literal = |s: &str| -> bool {
        s.starts_with('"')
            || s.chars()
                .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
    };

    for i in 0..pdas.len() {
        for j in (i + 1)..pdas.len() {
            let a = &pdas[i];
            let b = &pdas[j];

            if a.seeds == b.seeds {
                // Exact collision — same seed tuple → same address always.
                warnings.push(CompletenessWarning {
                    rule: "pda_seed_collision".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "PDA '{}' and PDA '{}' have identical seed tuples [{}] — they will always resolve to the same on-chain address",
                        a.name, b.name, a.seeds.join(", ")
                    ),
                    subject: Some(a.name.clone()),
                    fix: format!(
                        "Add a distinguishing seed to '{}' or '{}' (e.g., a discriminator byte or unique program-specific tag)",
                        a.name, b.name
                    ),
                    example: Some(format!(
                        "  pda {} [\"{}_tag\", {}]\n  pda {} [\"{}_tag\", {}]",
                        a.name,
                        a.name.to_lowercase(),
                        a.seeds.join(", "),
                        b.name,
                        b.name.to_lowercase(),
                        b.seeds.join(", ")
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
                continue;
            }

            // Possible collision: same literal seeds, differing only in variable positions.
            let a_literals: Vec<&str> = a
                .seeds
                .iter()
                .filter(|s| is_literal(s))
                .map(|s| s.as_str())
                .collect();
            let b_literals: Vec<&str> = b
                .seeds
                .iter()
                .filter(|s| is_literal(s))
                .map(|s| s.as_str())
                .collect();

            if !a_literals.is_empty() && a_literals == b_literals && a.seeds.len() == b.seeds.len()
            {
                // Same structure, same literals — variable seeds could collide at runtime.
                warnings.push(CompletenessWarning {
                    rule: "pda_seed_possible_collision".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "PDA '{}' and PDA '{}' share all literal seeds [{}] and differ only in variable positions — they can collide at runtime when variables hold the same values",
                        a.name, b.name, a_literals.join(", ")
                    ),
                    subject: Some(a.name.clone()),
                    fix: format!(
                        "Add a unique literal discriminator seed to '{}' or '{}' so their namespaces cannot overlap",
                        a.name, b.name
                    ),
                    example: Some(format!(
                        "  pda {} [\"{}\", ...]\n  pda {} [\"{}\", ...]",
                        a.name,
                        a.name.to_lowercase(),
                        b.name,
                        b.name.to_lowercase()
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    warnings
}

/// Parse a field-type source string into a structured view.
/// Returns `Simple` for `U128`, `Account`, `Vec U64` and `Map { ... }` for
/// `Map[CONST] T` (bound and inner trimmed).
fn classify_field_type(s: &str) -> FieldTypeShape<'_> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound = rest[..close].trim();
                let inner = rest[close + 1..].trim();
                return FieldTypeShape::Map { bound, inner };
            }
        }
    }
    FieldTypeShape::Simple(trimmed)
}

/// Validate `Map[N] T` field declarations and subscript usage.
///   - `N` must be a declared `const`
///   - `T` must be either a declared record or a well-known primitive
///   - Effect LHS of form `field[i].x` must reference a Map-typed state field
fn check_map_and_subscript(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    use std::collections::{HashMap, HashSet};

    let mut warnings = Vec::new();

    let const_names: HashSet<&str> = spec.constants.iter().map(|(n, _)| n.as_str()).collect();
    let record_names: HashSet<&str> = spec.records.iter().map(|r| r.name.as_str()).collect();
    // Enum-typed Map bounds (`Map[AddressField] T`): a unit-only sum type
    // gives one slot per variant (per-variant PDAs). Mixed-variant sums are
    // rejected by the second pass so the slot shape stays homogeneous.
    let unit_only_sum_names: HashSet<&str> = spec
        .sum_types
        .iter()
        .filter(|s| s.variants.iter().all(|v| v.fields.is_empty()))
        .map(|s| s.name.as_str())
        .collect();

    // Collect Map-typed fields across all account types, keyed by field name.
    let mut map_fields: HashMap<&str, (&str, &str, &str)> = HashMap::new(); // field → (owner, bound, inner)

    for acct in &spec.account_types {
        for (fname, ftype) in &acct.fields {
            if let FieldTypeShape::Map { bound, inner } = classify_field_type(ftype) {
                // Rule: bound must be a declared const OR a unit-only sum type.
                if !const_names.contains(bound) && !unit_only_sum_names.contains(bound) {
                    warnings.push(CompletenessWarning {
                        rule: "map_bound_not_const".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "field '{}.{}' uses Map[{}] but '{}' is neither a declared `const` nor a unit-only enum type",
                            acct.name, fname, bound, bound
                        ),
                        subject: Some(fname.clone()),
                        fix: format!("Add `const {} = <size>` or declare `type {} | Variant1 | Variant2 | …` at the top of the spec", bound, bound),
                        example: Some(format!("  const {} = 1024", bound)),
                        counterexample: None,
                        fix_options: vec![],
                    });
                }

                // Rule: inner must be a record or a known primitive
                let is_known = record_names.contains(inner)
                    || matches!(
                        inner,
                        "Bool"
                            | "U8"
                            | "U16"
                            | "U32"
                            | "U64"
                            | "U128"
                            | "I8"
                            | "I16"
                            | "I32"
                            | "I64"
                            | "I128"
                            | "Pubkey"
                    );
                if !is_known {
                    warnings.push(CompletenessWarning {
                        rule: "map_value_unknown".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "field '{}.{}' uses Map[{}] {} but '{}' is neither a declared record nor a primitive",
                            acct.name, fname, bound, inner, inner
                        ),
                        subject: Some(fname.clone()),
                        fix: format!("Declare `type {} = {{ ... }}`", inner),
                        example: Some(format!(
                            "  type {} = {{\n    active : Bool,\n    capital : U128,\n  }}",
                            inner
                        )),
                        counterexample: None,
                        fix_options: vec![],
                    });
                }

                map_fields.insert(fname.as_str(), (acct.name.as_str(), bound, inner));
            }
        }
    }

    // Effect LHS validation: any `name[i]...` must refer to a Map-typed field.
    for op in &spec.handlers {
        for (field, _, _) in &op.effects {
            if let Some(bracket) = field.find('[') {
                let root = &field[..bracket];
                if !map_fields.contains_key(root) {
                    warnings.push(CompletenessWarning {
                        rule: "subscript_not_map".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "handler '{}' has effect `{}` but '{}' is not a Map-typed state field",
                            op.name, field, root
                        ),
                        subject: Some(op.name.clone()),
                        fix: format!(
                            "Declare `{} : Map[MAX_...] SomeRecord` in the state type, or remove the subscript",
                            root
                        ),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
        }
    }

    warnings
}

/// Lint with explicit control over both lock behavior and cache policy.
/// `qedgen check --frozen --no-cache` calls this.
pub fn lint_with_opts(
    spec_path: &std::path::Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> Result<Vec<CompletenessWarning>> {
    let spec = parse_spec_file_with_opts(spec_path, lock_mode, cache_opts)?;
    Ok(check_completeness(&spec))
}

// ============================================================================
// Coverage matrix (qedgen coverage)
// ============================================================================

/// A single cell in the operation × property coverage matrix.
#[derive(Debug, serde::Serialize)]
pub struct CoverageCell {
    pub operation: String,
    pub property: String,
    pub covered: bool,
}

/// The full coverage matrix: which operations are covered by which properties.
#[derive(Debug, serde::Serialize)]
pub struct CoverageMatrix {
    pub operations: Vec<String>,
    pub properties: Vec<String>,
    pub cells: Vec<CoverageCell>,
    pub gaps: Vec<String>,
    pub coverage_pct: f64,
}

/// Build a coverage matrix from a parsed spec.
pub fn coverage_matrix(spec: &ParsedSpec) -> CoverageMatrix {
    let op_names: Vec<String> = spec.handlers.iter().map(|o| o.name.clone()).collect();
    let prop_names: Vec<String> = spec
        .properties
        .iter()
        .filter(|p| p.expression.is_some())
        .map(|p| p.name.clone())
        .collect();

    let mut cells = Vec::new();
    let mut covered_ops = std::collections::HashSet::new();

    for op in &op_names {
        for prop in &spec.properties {
            if prop.expression.is_none() {
                continue;
            }
            let covered = prop.preserved_by.contains(op);
            if covered {
                covered_ops.insert(op.clone());
            }
            cells.push(CoverageCell {
                operation: op.clone(),
                property: prop.name.clone(),
                covered,
            });
        }
    }

    let gaps: Vec<String> = op_names
        .iter()
        .filter(|op| !covered_ops.contains(*op))
        .cloned()
        .collect();

    let coverage_pct = if op_names.is_empty() {
        100.0
    } else {
        (covered_ops.len() as f64 / op_names.len() as f64) * 100.0
    };

    CoverageMatrix {
        operations: op_names,
        properties: prop_names,
        cells,
        gaps,
        coverage_pct,
    }
}

/// Print a formatted coverage table to stderr.
pub fn print_coverage_table(matrix: &CoverageMatrix) {
    if matrix.properties.is_empty() {
        eprintln!("No properties defined — nothing to show.");
        return;
    }

    let op_col_width = matrix
        .operations
        .iter()
        .map(|o| o.len())
        .max()
        .unwrap_or(9)
        .max(9);
    let prop_col_width = matrix
        .properties
        .iter()
        .map(|p| p.len())
        .max()
        .unwrap_or(4)
        .max(4);

    eprint!("{:<width$}", "operation", width = op_col_width + 2);
    for prop in &matrix.properties {
        eprint!(" {:^width$}", prop, width = prop_col_width);
    }
    eprintln!();

    eprint!("{}", "-".repeat(op_col_width + 2));
    for _ in &matrix.properties {
        eprint!("-{}", "-".repeat(prop_col_width));
    }
    eprintln!();

    for op in &matrix.operations {
        eprint!("{:<width$}", op, width = op_col_width + 2);
        for prop in &matrix.properties {
            let covered = matrix
                .cells
                .iter()
                .any(|c| &c.operation == op && &c.property == prop && c.covered);
            let mark = if covered { "Y" } else { "-" };
            eprint!(" {:^width$}", mark, width = prop_col_width);
        }
        eprintln!();
    }

    eprintln!();
    eprintln!(
        "Coverage: {:.0}% ({}/{} operations covered by at least one property)",
        matrix.coverage_pct,
        matrix.operations.len() - matrix.gaps.len(),
        matrix.operations.len()
    );

    if !matrix.gaps.is_empty() {
        eprintln!("Gaps: {}", matrix.gaps.join(", "));
    }
}

/// Check code drift — compare generated files against current spec.
pub fn check_code_drift(
    spec: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    code_dir: &std::path::Path,
) -> Result<Vec<DriftResult>> {
    let mut results = Vec::new();

    // Files codegen owns and stamps with `spec-hash:<hex>` — these are the
    // ones drift detection should compare against the spec fingerprint.
    let mut codegen_owned_files: Vec<String> = vec![
        "src/lib.rs".to_string(),
        "src/state.rs".to_string(),
        "src/instructions/mod.rs".to_string(),
        "Cargo.toml".to_string(),
    ];
    // src/guards.rs is codegen-owned whenever any handler has a `requires`
    // / `aborts_if` clause that lowers to runtime guard logic — omitting it
    // lets material guard drift report "in sync".
    let any_handler_has_guards = spec
        .handlers
        .iter()
        .any(|h| !h.requires.is_empty() || !h.aborts_if.is_empty() || h.guard_str.is_some());
    if any_handler_has_guards {
        codegen_owned_files.push("src/guards.rs".to_string());
    }
    if !spec.events.is_empty() {
        codegen_owned_files.push("src/events.rs".to_string());
    }
    if !spec.error_codes.is_empty() {
        codegen_owned_files.push("src/errors.rs".to_string());
    }
    // ref_impls.rs is codegen-owned whenever the spec declares any
    // `ref_impl` — the file holds one `pub fn` per impl.
    if !spec.ref_impls.is_empty() {
        codegen_owned_files.push("src/ref_impls.rs".to_string());
    }

    // Per-handler files at `src/instructions/<handler>.rs` are user-owned
    // and never re-stamped after the initial scaffold, so NoHash is their
    // expected steady state, not a drift signal — only Missing matters.
    let user_owned_handler_files: Vec<String> = spec
        .handlers
        .iter()
        .map(|h| format!("src/instructions/{}.rs", h.name))
        .collect();

    for file in user_owned_handler_files
        .iter()
        .chain(codegen_owned_files.iter())
    {
        let path = code_dir.join(file);
        if !path.exists() {
            results.push(DriftResult {
                file: file.clone(),
                status: DriftStatus::Missing,
                detail: Some("expected by spec but not found".to_string()),
            });
            continue;
        }

        // User-owned handler files don't carry a spec-hash by design;
        // their existence is the only thing drift detection asserts.
        if user_owned_handler_files.contains(file) {
            results.push(DriftResult {
                file: file.clone(),
                status: DriftStatus::InSync,
                detail: None,
            });
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        let embedded = crate::fingerprint::extract_spec_hash(&content);
        let expected = fp.file_hashes.get(file.as_str());

        match (embedded, expected) {
            (None, _) => {
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::NoHash,
                    detail: Some(
                        "no embedded spec-hash (generated before fingerprinting)".to_string(),
                    ),
                });
            }
            (Some(ref emb), Some(exp)) if emb == exp => {
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::InSync,
                    detail: None,
                });
            }
            (Some(_), Some(_)) => {
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::SpecChanged,
                    detail: Some("spec changed since last generation".to_string()),
                });
            }
            (Some(_), None) => {
                // Hash in file but no expected hash — shouldn't happen, treat as in-sync
                results.push(DriftResult {
                    file: file.clone(),
                    status: DriftStatus::InSync,
                    detail: None,
                });
            }
        }
    }

    // Check for orphaned instruction files
    let instr_dir = code_dir.join("src/instructions");
    if instr_dir.exists() {
        let expected_ops: Vec<String> = spec
            .handlers
            .iter()
            .map(|o| format!("{}.rs", o.name))
            .collect();
        if let Ok(entries) = std::fs::read_dir(&instr_dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname == "mod.rs" {
                    continue;
                }
                if fname.ends_with(".rs") && !expected_ops.contains(&fname) {
                    results.push(DriftResult {
                        file: format!("src/instructions/{}", fname),
                        status: DriftStatus::Orphaned,
                        detail: Some("file not expected by current spec".to_string()),
                    });
                }
            }
        }
    }

    Ok(results)
}

/// Flag residual `todo!()` placeholders in user-owned handler files.
/// `cargo check` passes through `todo!()` (returns `!`) and drift detection
/// only covers codegen-owned files, so without this lint a scaffolded
/// program ships with placeholder business logic uncaught. A `todo!()`
/// inside a `#[qed(verified, ...)]` body means events / transfers / CPIs /
/// non-mechanical effects haven't been filled.
pub fn check_handler_todos(
    spec: &ParsedSpec,
    code_dir: &std::path::Path,
) -> Result<Vec<CompletenessWarning>> {
    let mut warnings = Vec::new();

    let instructions_dir = code_dir.join("src").join("instructions");
    if !instructions_dir.exists() {
        return Ok(warnings);
    }

    for handler in &spec.handlers {
        let path = instructions_dir.join(format!("{}.rs", handler.name));
        if !path.exists() {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let parsed = match syn::parse_file(&source) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if !file_has_qed_verified_todo(&parsed) {
            continue;
        }

        let mut hints: Vec<String> = Vec::new();
        for emit in &handler.emits {
            hints.push(format!("emit `{}` event", emit));
        }
        for t in &handler.transfers {
            hints.push(format!("token transfer `{} -> {}`", t.from, t.to));
        }
        for call in &handler.calls {
            hints.push(format!(
                "CPI `{}.{}`",
                call.target_interface, call.target_handler
            ));
        }
        let hint_text = if hints.is_empty() {
            "non-mechanical effects".to_string()
        } else {
            hints.join(", ")
        };

        let rel = path
            .strip_prefix(code_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());

        warnings.push(CompletenessWarning {
            rule: "handler_unfilled_todo".to_string(),
            severity: Severity::Warning,
            priority: 2,
            message: format!(
                "handler `{}` has an unfilled `todo!()` in {} — spec expects: {}",
                handler.name, rel, hint_text
            ),
            subject: Some(handler.name.clone()),
            fix: format!(
                "Open `{}` and fill the body using guard calls, state structs, and the spec's declared {} as the contract. Codegen leaves `todo!()` so the agent closes the loop on business logic; the placeholder type-checks but panics at runtime.",
                rel, hint_text
            ),
            example: None,
            counterexample: None,
            fix_options: Vec::new(),
        });
    }

    Ok(warnings)
}

fn file_has_qed_verified_todo(file: &syn::File) -> bool {
    use syn::visit::Visit;

    struct V {
        in_verified: u32,
        any: bool,
    }

    impl V {
        fn enter_with<F>(&mut self, attrs: &[syn::Attribute], visit: F)
        where
            F: FnOnce(&mut Self),
        {
            let verified = has_qed_verified_attr(attrs);
            if verified {
                self.in_verified += 1;
            }
            visit(self);
            if verified {
                self.in_verified -= 1;
            }
        }
    }

    impl<'ast> Visit<'ast> for V {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            let attrs = f.attrs.clone();
            self.enter_with(&attrs, |v| syn::visit::visit_item_fn(v, f));
        }
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            let attrs = f.attrs.clone();
            self.enter_with(&attrs, |v| syn::visit::visit_impl_item_fn(v, f));
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            if self.in_verified > 0 {
                if let Some(seg) = mac.path.segments.last() {
                    if seg.ident == "todo" {
                        self.any = true;
                    }
                }
            }
            syn::visit::visit_macro(self, mac);
        }
    }

    let mut v = V {
        in_verified: 0,
        any: false,
    };
    v.visit_file(file);
    v.any
}

fn has_qed_verified_attr(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("qed") {
            continue;
        }
        if let syn::Meta::List(list) = &attr.meta {
            if list.tokens.to_string().contains("verified") {
                return true;
            }
        }
    }
    false
}

/// Check Kani drift — compare harness file against current spec.
pub fn check_kani_drift(
    spec: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    kani_path: &std::path::Path,
) -> Result<Vec<KaniDriftResult>> {
    let mut results = Vec::new();

    if !kani_path.exists() {
        results.push(KaniDriftResult {
            harness_name: "(file)".to_string(),
            status: KaniDriftStatus::Missing,
        });
        return Ok(results);
    }

    let content = std::fs::read_to_string(kani_path)?;

    // File-level hash check
    let embedded = crate::fingerprint::extract_spec_hash(&content);
    let expected = fp.file_hashes.get("tests/kani.rs");
    let file_stale = match (embedded, expected) {
        (Some(ref emb), Some(exp)) => emb != exp,
        (None, _) => true,
        _ => false,
    };

    // Build expected harness names (same logic as kani::generate)
    let mut expected_harnesses = Vec::new();
    for op in &spec.handlers {
        if op.who.is_some() {
            expected_harnesses.push(format!("verify_{}_access_control", op.name));
        }
        if op.has_guard() {
            expected_harnesses.push(format!("verify_{}_rejects_invalid", op.name));
        }
        if let (Some(pre_s), Some(post_s)) = (&op.pre_status, &op.post_status) {
            let pre = pre_s.to_lowercase();
            let post = post_s.to_lowercase();
            expected_harnesses.push(format!("verify_{}_transition_{}_to_{}", op.name, pre, post));
        }
        if op.has_effect() {
            expected_harnesses.push(format!("verify_{}_effects", op.name));
        }
    }
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            expected_harnesses.push(format!("verify_{}_preserves_{}", op_name, prop.name));
        }
    }

    // Parse file for fn verify_* names
    static FN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"fn\s+(verify_\w+)\s*\(").unwrap());
    let fn_re = &*FN_RE;
    let found_harnesses: Vec<String> = fn_re
        .captures_iter(&content)
        .map(|c| c[1].to_string())
        .collect();

    for expected in &expected_harnesses {
        if found_harnesses.contains(expected) {
            if file_stale {
                results.push(KaniDriftResult {
                    harness_name: expected.clone(),
                    status: KaniDriftStatus::FileStale,
                });
            } else {
                results.push(KaniDriftResult {
                    harness_name: expected.clone(),
                    status: KaniDriftStatus::InSync,
                });
            }
        } else {
            results.push(KaniDriftResult {
                harness_name: expected.clone(),
                status: KaniDriftStatus::Missing,
            });
        }
    }

    for found in &found_harnesses {
        if !expected_harnesses.contains(found) {
            results.push(KaniDriftResult {
                harness_name: found.clone(),
                status: KaniDriftStatus::Orphaned,
            });
        }
    }

    Ok(results)
}

/// Run unified drift detection across all layers.
pub fn check_unified(
    spec_path: &std::path::Path,
    proofs_dir: &std::path::Path,
    code_dir: Option<&std::path::Path>,
    kani_path: Option<&std::path::Path>,
) -> Result<UnifiedReport> {
    let spec = parse_spec_file(spec_path)?;
    let fp = crate::fingerprint::compute_fingerprint(&spec);

    // 1. Spec completeness
    let mut completeness = check_completeness(&spec);

    // 2. Code drift + residual `todo!()` lint (both code-aware).
    let code_drift = if let Some(dir) = code_dir {
        completeness.extend(check_handler_todos(&spec, dir)?);
        Some(check_code_drift(&spec, &fp, dir)?)
    } else {
        None
    };

    // 3. Kani drift
    let kani_drift = if let Some(path) = kani_path {
        Some(check_kani_drift(&spec, &fp, path)?)
    } else {
        None
    };

    // 4. Lean coverage
    let lean_coverage = check(spec_path, proofs_dir)?;

    Ok(UnifiedReport {
        completeness,
        code_drift,
        kani_drift,
        lean_coverage,
    })
}

/// Print the unified drift report.
pub fn print_unified_report(spec_name: &str, report: &UnifiedReport) {
    // Spec completeness
    let warns = report
        .completeness
        .iter()
        .filter(|w| w.severity == Severity::Warning)
        .count();
    let infos = report
        .completeness
        .iter()
        .filter(|w| w.severity == Severity::Info)
        .count();

    eprintln!("──── Spec Completeness ──────────────────────────────────");
    if report.completeness.is_empty() {
        eprintln!("  (no issues)");
    } else {
        for w in &report.completeness {
            let icon = match w.severity {
                Severity::Error => "E",
                Severity::Warning => "!",
                Severity::Info => "i",
            };
            eprintln!("  {} [{}] {}", icon, w.rule, w.message);
            eprintln!("    Fix: {}", w.fix);
        }
    }
    eprintln!("  {} warning(s), {} info\n", warns, infos);

    // Code drift
    if let Some(ref drift) = report.code_drift {
        eprintln!("──── Code Drift ─────────────────────────────────────────");
        let issues = drift
            .iter()
            .filter(|d| d.status != DriftStatus::InSync)
            .count();
        let synced = drift
            .iter()
            .filter(|d| d.status == DriftStatus::InSync)
            .count();
        for d in drift {
            let (icon, tag) = match d.status {
                DriftStatus::InSync => ("✓", ""),
                DriftStatus::NoHash => ("?", " NO HASH"),
                DriftStatus::SpecChanged => ("✗", " SPEC CHANGED"),
                DriftStatus::Missing => ("✗", " MISSING"),
                DriftStatus::Orphaned => ("?", " ORPHANED"),
            };
            let detail = d
                .detail
                .as_ref()
                .map(|s| format!(" — {}", s))
                .unwrap_or_default();
            eprintln!("  {} {:<40} {}{}", icon, d.file, tag, detail);
        }
        eprintln!("  {} file(s) need attention, {} in sync\n", issues, synced);
    }

    // Kani drift
    if let Some(ref drift) = report.kani_drift {
        eprintln!("──── Kani Drift ─────────────────────────────────────────");
        let issues = drift
            .iter()
            .filter(|d| d.status != KaniDriftStatus::InSync)
            .count();
        let synced = drift
            .iter()
            .filter(|d| d.status == KaniDriftStatus::InSync)
            .count();
        for d in drift {
            let (icon, tag) = match d.status {
                KaniDriftStatus::InSync => ("✓", ""),
                KaniDriftStatus::Missing => ("✗", " MISSING"),
                KaniDriftStatus::Orphaned => ("?", " ORPHANED"),
                KaniDriftStatus::FileStale => ("✗", " FILE STALE"),
            };
            eprintln!("  {} {:<40} {}", icon, d.harness_name, tag);
        }
        eprintln!(
            "  {} harness(es) need attention, {} in sync\n",
            issues, synced
        );
    }

    // Lean coverage
    let proven = report
        .lean_coverage
        .iter()
        .filter(|r| r.status == Status::Proven)
        .count();
    let total = report.lean_coverage.len();

    eprintln!("──── Lean Coverage ──────────────────────────────────────");
    if report.lean_coverage.is_empty() {
        eprintln!("  (no properties declared)");
    } else {
        for r in &report.lean_coverage {
            let (icon, tag) = match r.status {
                Status::Proven => ("✓", ""),
                Status::Sorry => ("✗", " [sorry]"),
                Status::Missing => ("✗", " [missing]"),
            };
            let intent = r
                .intent
                .as_ref()
                .map(|s| format!(" — {}", s))
                .unwrap_or_default();
            eprintln!("  {} {:<40} {}{}", icon, r.name, tag, intent);
        }
    }
    eprintln!("  {}/{} proven\n", proven, total);

    // Summary
    let total_issues = report.issue_count();
    eprintln!(
        "──── {} {} — {} issue(s) ────",
        spec_name,
        if total_issues == 0 { "CLEAN" } else { "DRIFT" },
        total_issues
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_spec() -> ParsedSpec {
        ParsedSpec::default()
    }

    #[test]
    fn wrapping_arithmetic_lint_fires_on_wrap() {
        let mut spec = empty_spec();
        let mut h = make_handler("tick");
        h.effects
            .push(("epoch".to_string(), "add_wrap".to_string(), "1".to_string()));
        spec.handlers.push(h);
        let warnings = check_wrapping_arithmetic_opt_in(&spec);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule, "wrapping_arithmetic");
        assert_eq!(warnings[0].severity, Severity::Warning);
        assert!(warnings[0].message.contains("wrapping"));
    }

    #[test]
    fn wrapping_arithmetic_lint_fires_on_saturating() {
        let mut spec = empty_spec();
        let mut h = make_handler("apply");
        h.effects.push((
            "balance".to_string(),
            "add_sat".to_string(),
            "delta".to_string(),
        ));
        spec.handlers.push(h);
        let warnings = check_wrapping_arithmetic_opt_in(&spec);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule, "saturating_arithmetic");
        assert_eq!(warnings[0].severity, Severity::Info);
    }

    #[test]
    fn wrapping_arithmetic_lint_silent_on_default_checked() {
        let mut spec = empty_spec();
        let mut h = make_handler("deposit");
        h.effects
            .push(("total".to_string(), "add".to_string(), "amount".to_string()));
        h.effects.push((
            "fee_pool".to_string(),
            "sub".to_string(),
            "amount".to_string(),
        ));
        spec.handlers.push(h);
        assert!(check_wrapping_arithmetic_opt_in(&spec).is_empty());
    }

    #[test]
    fn wrapping_arithmetic_lint_fires_per_op() {
        let mut spec = empty_spec();
        let mut h = make_handler("complex");
        h.effects
            .push(("a".to_string(), "add_wrap".to_string(), "1".to_string()));
        h.effects
            .push(("b".to_string(), "sub_sat".to_string(), "1".to_string()));
        spec.handlers.push(h);
        let warnings = check_wrapping_arithmetic_opt_in(&spec);
        assert_eq!(warnings.len(), 2);
    }

    fn make_handler(name: &str) -> ParsedHandler {
        ParsedHandler {
            name: name.to_string(),
            doc: None,
            who: Some("authority".to_string()),
            on_account: None,
            pre_status: Some("Active".to_string()),
            post_status: Some("Active".to_string()),
            takes_params: vec![],
            guard_str: None,
            guard_str_rust: None,
            aborts_if: vec![],
            requires: vec![],
            ensures: vec![],
            modifies: None,
            let_bindings: vec![],
            aborts_total: false,
            permissionless: false,
            effects: vec![],
            effect_on_error: vec![],
            accounts: vec![],
            transfers: vec![],
            emits: vec![],
            invariants: vec![],
            establishes: vec![],
            properties: vec![],
            schema_includes: vec![],
            calls: vec![],
            effect_branches: None,
            abstract_binders: vec![],
        }
    }

    // `state { fields }` sugar must expose Map-typed fields to
    // `check_map_and_subscript` — otherwise `subscript_not_map` fires on
    // every effect LHS that subscripts a sugared Map field.
    #[test]
    fn state_sugar_map_field_is_visible_to_subscript_lint() {
        let src = r#"
spec Probe
const MAX = 8
type User = { active : Bool, balance : U64, }
state {
  lsts : Map[MAX] User,
}
type Error
  | InvalidAmount
handler deposit (idx : U64) (amt : U64) {
  effect { lsts[idx].balance := amt }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_map_and_subscript(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "subscript_not_map"),
            "spurious subscript_not_map on `state {{ ... }}` sugar: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // `modifies [X]` + no effect write + no ensures referencing X =
    // completely unconstrained field; fires P0.
    #[test]
    fn unconstrained_modifies_lint_fires_on_uncovered_field() {
        let src = r#"
spec Probe
state { pool_balance : U64, lp_supply : U64 }
type Error
  | InvalidAmount
  | MathOverflow
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  effect { pool_balance += amount }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_unconstrained_modifies(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unconstrained_modifies")
            .expect("unconstrained_modifies fires for lp_supply");
        assert_eq!(hit.severity, Severity::Error);
        assert!(
            hit.message.contains("'lp_supply'"),
            "message names the field, got: {}",
            hit.message
        );
        // pool_balance is in modifies AND in effect — no warning for it.
        assert!(
            !warnings
                .iter()
                .any(|w| w.message.contains("'pool_balance'")),
            "pool_balance must not fire — it's written by the effect"
        );
    }

    // Inverse: when an `ensures` clause references the field, the
    // lint stays silent. The field is constrained even if the effect
    // block doesn't write it (the "Kani checks impl" pattern).
    #[test]
    fn unconstrained_modifies_lint_silent_when_ensures_references_field() {
        let src = r#"
spec Probe
state { pool_balance : U64, lp_supply : U64 }
type Error
  | InvalidAmount
  | MathOverflow
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  effect { pool_balance += amount }
  ensures lp_supply >= old(state.lp_supply)
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_unconstrained_modifies(&spec);
        assert!(
            warnings.is_empty(),
            "lint must stay silent when ensures references the field, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ========================================================================
    // adt_state_missing_wrong_state lint
    // ========================================================================

    /// `pragma state_repr = adt` selects the inductive representation,
    /// whose variant-mismatch fallthrough returns `Err(WrongState)`.
    /// Declaring the pragma without the error variant would emit
    /// non-compiling Rust, so `check` surfaces it. Without the pragma the
    /// same spec lowers flat and the lint stays silent.
    #[test]
    fn adt_state_pragma_without_wrong_state_fires() {
        let body = r#"
program_id "11111111111111111111111111111111"

type State
  | Uninitialized
  | Active of { balance : U64 }
  | Closed

type Error
  | InvalidAmount

handler open (amount : U64) : State.Uninitialized -> State.Active {
  auth owner
  accounts { owner : signer, writable }
  requires amount > 0 else InvalidAmount
}"#;

        // pragma set, no WrongState → fires
        let adt = crate::chumsky_adapter::parse_str(&format!(
            "spec Adt\npragma state_repr = adt\n{body}"
        ))
        .expect("parse adt");
        let w = check_completeness(&adt);
        let hit = w
            .iter()
            .find(|w| w.rule == "adt_state_missing_wrong_state")
            .unwrap_or_else(|| {
                panic!(
                    "lint must fire; got: {:?}",
                    w.iter().map(|w| &w.rule).collect::<Vec<_>>()
                )
            });
        assert_eq!(hit.severity, Severity::Warning);
        assert_eq!(hit.priority, 2);

        // no pragma (flat) → silent even without WrongState
        let flat =
            crate::chumsky_adapter::parse_str(&format!("spec Flat\n{body}")).expect("parse flat");
        assert!(
            !check_completeness(&flat)
                .iter()
                .any(|w| w.rule == "adt_state_missing_wrong_state"),
            "flat specs don't need WrongState; lint must stay silent"
        );
    }

    // ========================================================================
    // multi_cpi_same_field lint
    // ========================================================================

    /// Two CPI calls whose substituted ensures both reference the same
    /// caller-state field (`post.vault_balance`) → lint fires P2 Info.
    /// Mirrors the bear-hug scenario where two `Token.transfer` calls
    /// drain the same vault. Without per-call snapshot frames (v3.0),
    /// the Kani harness can over-constrain.
    #[test]
    fn multi_cpi_same_field_fires_on_two_token_transfers_from_same_vault() {
        let src = r#"spec MultiCpi
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.vault_balance == old(state.vault_balance) - amount
  }
}

state { vault_balance : U64 }

handler split (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call Token.transfer(from = 0, to = 1, amount = a, authority = 0)
  call Token.transfer(from = 0, to = 2, amount = b, authority = 0)
  effect { vault_balance -= a }
  ensures state.vault_balance == old(state.vault_balance) - a - b
}"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "multi_cpi_same_field")
            .unwrap_or_else(|| {
                panic!(
                    "multi_cpi_same_field must fire; got: {:?}",
                    warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
                )
            });
        assert_eq!(hit.severity, Severity::Info);
        assert_eq!(hit.priority, 2);
        assert!(
            hit.message.contains("'vault_balance'"),
            "message must name the shared field; got: {}",
            hit.message
        );
        assert!(
            hit.message.contains("Token.transfer"),
            "message must name the call pair; got: {}",
            hit.message
        );
        assert_eq!(hit.subject.as_deref(), Some("split"));
    }

    /// Disjoint Token.transfer resources are handled by the Pinocchio
    /// impl-targeted token projection backend. The abstract callee fields
    /// are the same, but the generated proof reads and asserts each token
    /// account's concrete amount independently.
    #[test]
    fn multi_cpi_same_field_silent_on_disjoint_token_transfer_resources() {
        let src = r#"spec MultiCpiDisjointToken
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.from_balance == old(state.from_balance) - amount
    ensures state.to_balance == old(state.to_balance) + amount
  }
}

state { from_balance : U64, to_balance : U64 }

handler swap_like (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call Token.transfer(from = user_input, to = hub_input, amount = a, authority = auth)
  call Token.transfer(from = hub_output, to = user_output, amount = b, authority = auth)
  ensures state.from_balance == old(state.from_balance) - a
}"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        assert!(
            warnings.is_empty(),
            "disjoint Token.transfer resources use per-account projections; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Two CPI calls whose substituted ensures reference disjoint
    /// caller-state fields → lint stays silent. No (pre, post) snapshot
    /// pair is shared, so the over-constraint risk doesn't apply.
    #[test]
    fn multi_cpi_same_field_silent_on_disjoint_fields() {
        let src = r#"spec MultiCpiDisjoint
program_id "11111111111111111111111111111111"

interface VaultA {
  program_id "11111111111111111111111111111111"
  handler debit (amount : U64) {
    accounts { vault : writable }
    requires amount > 0
    ensures state.vault_a_balance == old(state.vault_a_balance) - amount
  }
}

interface VaultB {
  program_id "11111111111111111111111111111111"
  handler debit (amount : U64) {
    accounts { vault : writable }
    requires amount > 0
    ensures state.vault_b_balance == old(state.vault_b_balance) - amount
  }
}

state { vault_a_balance : U64, vault_b_balance : U64 }

handler tap_both (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call VaultA.debit(amount = a)
  call VaultB.debit(amount = b)
  effect { vault_a_balance -= a }
  effect { vault_b_balance -= b }
  ensures state.vault_a_balance == old(state.vault_a_balance) - a
  ensures state.vault_b_balance == old(state.vault_b_balance) - b
}"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        assert!(
            warnings.is_empty(),
            "disjoint-field CPI ensures must not fire multi_cpi_same_field; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Tier-0 callees (no `ensures` declared) → no substituted field
    /// references → lint stays silent regardless of CPI multiplicity.
    /// Catches the spec-shape where the user hasn't yet declared the
    /// callee's contract; the `cpi_no_callee_ensures` lint surfaces
    /// that gap separately.
    #[test]
    fn multi_cpi_same_field_silent_on_tier0_callees() {
        let src = r#"spec MultiCpiTier0
program_id "11111111111111111111111111111111"

interface Logger {
  program_id "11111111111111111111111111111111"
  handler log (msg : U64) {
    accounts { sink : writable }
  }
}

state { counter : U64 }

handler tick_twice (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call Logger.log(msg = a)
  call Logger.log(msg = b)
  effect { counter += a }
  ensures state.counter == old(state.counter) + a
}"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        assert!(
            warnings.is_empty(),
            "tier-0 callees produce no field refs → lint must stay silent; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn error_declared_as_record_lint_fires_and_suggests_pipe_form() {
        let src = r#"
spec Probe
state { balance : U64 }
type Error = {
  InvalidAmount : U64,
  Unauthorized : U64,
}
handler init { effect { balance := 0 } }
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_error_declared_as_record(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "error_declared_as_record")
            .expect("error_declared_as_record fires");
        assert_eq!(hit.severity, Severity::Error);
        let example = hit.example.as_deref().unwrap_or("");
        assert!(
            example.contains("type Error\n  | InvalidAmount"),
            "example should suggest pipe form, got: {}",
            example
        );
    }

    #[test]
    fn test_missing_guard_from_takes_fires() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_guard_from_takes"),
            "expected missing_guard_from_takes, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_guard_from_takes_skips_when_guard_exists() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_guard_from_takes"),
            "should not fire when guard exists"
        );
    }

    #[test]
    fn test_missing_effect_fires() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.guard_str = Some("amount > 0".to_string());
        // has lifecycle (pre/post set via make_handler) but no effect
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "missing_effect"),
            "expected missing_effect, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// `call X.handler(...)`, `transfers { … }`, or `modifies [...]` all
    /// count as effect-satisfying — the lint must not fire on CPI-only
    /// handlers where state writes are the wrong abstraction.
    #[test]
    fn test_missing_effect_skips_when_handler_has_only_calls() {
        let mut h = make_handler("init_mint");
        h.takes_params = vec![("decimals".to_string(), "U64".to_string())];
        h.guard_str = Some("decimals > 0".to_string());
        h.calls = vec![ParsedCall {
            target_interface: "Token".to_string(),
            target_handler: "initialize_mint".to_string(),
            args: vec![],
            result_binding: None,
            state_binders: Vec::new(),
        }];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "missing_effect should not fire when handler has CPI calls; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// `modifies [field, ...]` is the frame-condition shape for handlers
    /// whose writes the spec doesn't model further — must satisfy the lint.
    #[test]
    fn test_missing_effect_skips_when_handler_has_modifies() {
        let mut h = make_handler("opaque_update");
        h.takes_params = vec![("payload".to_string(), "U64".to_string())];
        h.guard_str = Some("payload > 0".to_string());
        h.modifies = Some(vec!["balance".to_string()]);
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "missing_effect should not fire when handler declares `modifies`; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_effect_skips_when_effect_exists() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.guard_str = Some("amount > 0".to_string());
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "should not fire when effect exists"
        );
    }

    #[test]
    fn test_missing_effect_uses_on_account_fields() {
        let mut h = make_handler("borrow");
        h.on_account = Some("Loan".to_string());
        h.takes_params = vec![("loan_amount".to_string(), "U64".to_string())];
        h.guard_str = Some("loan_amount > 0".to_string());
        h.pre_status = Some("Empty".to_string());
        h.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            handlers: vec![h],
            account_types: vec![
                ParsedAccountType {
                    name: "Pool".to_string(),
                    fields: vec![("total_deposits".to_string(), "U64".to_string())],
                    lifecycle: vec!["Active".to_string()],
                    pda_ref: None,
                    variants: vec![],
                },
                ParsedAccountType {
                    name: "Loan".to_string(),
                    fields: vec![("loan_amount".to_string(), "U64".to_string())],
                    lifecycle: vec!["Empty".to_string(), "Active".to_string()],
                    pda_ref: None,
                    variants: vec![],
                },
            ],
            state_fields: vec![("total_deposits".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Empty".to_string(), "Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        let warning = warnings
            .iter()
            .find(|w| w.rule == "missing_effect")
            .expect("expected missing_effect warning");
        let example = warning
            .example
            .as_deref()
            .expect("missing_effect should include example");
        assert!(
            example.contains("loan_amount += loan_amount"),
            "expected account-aware suggestion, got: {}",
            example
        );
        assert!(
            !example.contains("total_deposits"),
            "should not use fields from a different account type: {}",
            example
        );
    }

    #[test]
    fn permissionless_skips_no_access_control() {
        // `permissionless` opts out of the P1 `no_access_control` lint;
        // without the marker, who-less handlers still fire.
        let mut h = make_handler("init_user");
        h.who = None;
        h.permissionless = true;
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "no_access_control"),
            "permissionless handler must not fire no_access_control: {warnings:?}"
        );
    }

    #[test]
    fn no_access_control_still_fires_without_marker() {
        // Control: handler with no auth and no permissionless marker still
        // triggers the lint.
        let mut h = make_handler("init_user");
        h.who = None;
        // h.permissionless stays false
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "no_access_control"),
            "who-less handler without permissionless should fire: {warnings:?}"
        );
    }

    #[test]
    fn permissionless_with_auth_surfaces_contradictory_auth() {
        // Both `auth X` and `permissionless` is contradictory — not a silent
        // precedence situation. Lint surfaces a clear P1.
        let mut h = make_handler("weird");
        h.who = Some("authority".to_string());
        h.permissionless = true;
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        let w = warnings
            .iter()
            .find(|w| w.rule == "contradictory_auth")
            .expect("contradictory_auth should fire");
        assert!(
            w.message.contains("authority") && w.message.contains("permissionless"),
            "message should name both: {}",
            w.message
        );
    }

    #[test]
    fn test_no_properties_fires() {
        let mut h = make_handler("deposit");
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "no_properties"),
            "expected no_properties, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_properties_skips_with_property() {
        let mut h = make_handler("deposit");
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("state.balance >= 0".to_string()),
                rust_expression: Some("s.balance >= 0".to_string()),
                rust_expression_pod: Some("s.balance >= 0".to_string()),
                preserved_by: vec!["deposit".to_string()],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "no_properties"),
            "should not fire when properties exist"
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context() {
        let mut h = make_handler("transfer");
        // Has token program in accounts but no transfers block
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "source".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "dest".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "expected missing_cpi_for_token_context, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_suppressed_on_lifecycle_init() {
        // An `initialize` handler creating a writable token account via
        // Anchor's `#[account(init, ...)]` needs no explicit `transfers` /
        // `call Token.*` — the init macro handles the SPL CPI implicitly.
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninitialized".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "vault".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: Some(vec!["vault".to_string(), "authority".to_string()]),
                account_type: Some("token".to_string()),
                authority: Some("vault_pda".to_string()),
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Uninitialized".to_string(), "Active".to_string()],
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Uninitialized".to_string(),
                        fields: vec![],
                    },
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("balance".to_string(), "U64".to_string())],
                    },
                ],
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "lifecycle-init handler creating a token account should NOT fire \
             missing_cpi_for_token_context; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_suppressed_on_non_canonical_init_name() {
        // The suppression keys on "pre-state variant has no payload", not
        // a hardcoded name list — specs naming the pre-init variant
        // `Uninit` / `Created` / etc. must stay silent too. Mirror of the
        // canonical-name test above with `Uninit` substituted.
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninit".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "vault".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: Some(vec!["vault".to_string(), "authority".to_string()]),
                account_type: Some("token".to_string()),
                authority: Some("vault_pda".to_string()),
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Uninit".to_string(), "Active".to_string()],
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Uninit".to_string(),
                        fields: vec![],
                    },
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("balance".to_string(), "U64".to_string())],
                    },
                ],
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "init handler with non-canonical pre-state variant `Uninit` \
             must NOT fire missing_cpi_for_token_context (v2.29.2 shape \
             predicate); got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_suppressed_when_no_typed_token_account() {
        // The suppression must not require a writable account typed
        // `token`: real specs leave token accounts bare-typed and rely on
        // Anchor's `init, associated_token::*` constraints to resolve the
        // type. `is_lifecycle_init && !has_calls()` is sufficient.
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninit".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                // Bare writable, no `type token` — Anchor would type it
                // via an `init, associated_token::*` constraint set the
                // spec doesn't repeat.
                name: "pool_balance_account".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Uninit".to_string(), "Active".to_string()],
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Uninit".to_string(),
                        fields: vec![],
                    },
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("balance".to_string(), "U64".to_string())],
                    },
                ],
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "lifecycle-init handler with token_program but no `type token` \
             writable account must NOT fire missing_cpi_for_token_context \
             (v2.29.2 — Anchor init handles SPL implicitly via constraint \
             set); got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_still_fires_on_non_init() {
        // Complement to the suppression: a handler in a non-init
        // lifecycle (e.g. Active → Active) with token_program and a
        // writable token account but no transfers SHOULD still fire —
        // Anchor's init macro doesn't apply, so the missing CPI is a
        // real spec gap.
        let mut h = make_handler("transfer");
        h.pre_status = Some("Active".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "source".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "non-init handler with token_program and no transfers SHOULD \
             still fire; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_lifecycle_unreachable_state() {
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninitialized".to_string());
        h.post_status = Some("Active".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Closed".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "lifecycle_unreachable_state"
                    && w.subject.as_deref() == Some("Closed")),
            "expected lifecycle_unreachable_state for Closed, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_lifecycle_disconnected_subgraph_is_unreachable() {
        let mut init = make_handler("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let mut close = make_handler("close");
        close.pre_status = Some("Frozen".to_string());
        close.post_status = Some("Closed".to_string());

        let spec = ParsedSpec {
            handlers: vec![init, close],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Frozen".to_string(),
                "Closed".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Frozen")
            }),
            "expected disconnected state Frozen to be unreachable, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
        assert!(
            warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Closed")
            }),
            "expected downstream state Closed to be unreachable, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_global_initial_state_seeded_when_account_lifecycle_differs() {
        // Account lifecycle starts at "Active", but the global initial state
        // is "Uninitialized". Without always seeding the global initial state,
        // "Uninitialized" would be flagged as unreachable even though it is
        // the entry point of the lifecycle.
        let mut init = make_handler("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            handlers: vec![init],
            account_types: vec![ParsedAccountType {
                name: "Pool".to_string(),
                fields: vec![],
                lifecycle: vec!["Active".to_string(), "Frozen".to_string()],
                pda_ref: None,
                variants: vec![],
            }],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Frozen".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state"
                    && w.subject.as_deref() == Some("Uninitialized")
            }),
            "Uninitialized is the global initial state and should NOT be flagged as unreachable, got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "lifecycle_unreachable_state")
                .map(|w| &w.subject)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_errors_block_fires() {
        let mut h = make_handler("deposit");
        h.guard_str = Some("amount > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "no_errors_block"),
            "expected no_errors_block, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_priority_ordering() {
        // Build a spec that triggers multiple rules at different priorities
        let mut h = make_handler("deposit");
        h.who = None; // priority 1: no_access_control
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        // no guard → priority 1: unguarded_arithmetic + missing_guard_from_takes
        // no properties → priority 3: no_properties
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("authority".to_string(), "Pubkey".to_string()),
                ("balance".to_string(), "U64".to_string()),
            ],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        // Verify sorted ascending by priority
        for window in warnings.windows(2) {
            assert!(
                window[0].priority <= window[1].priority,
                "warnings not sorted by priority: {} ({}) should come before {} ({})",
                window[0].rule,
                window[0].priority,
                window[1].rule,
                window[1].priority
            );
        }
    }

    #[test]
    fn test_complete_spec_clean() {
        let spec_content = include_str!("../../../../examples/rust/escrow/escrow.qedspec");
        let spec =
            crate::chumsky_adapter::parse_str(spec_content).expect("escrow.qedspec should parse");
        let warnings = check_completeness(&spec);
        // A well-formed spec should have zero `Warning`-severity findings.
        // (P6 on Pubkey state fields is Info-only, so it never appears here.)
        let warning_rules: Vec<&str> = warnings
            .iter()
            .filter(|w| w.severity == Severity::Warning)
            .map(|w| w.rule.as_str())
            .collect();
        assert!(
            warning_rules.is_empty(),
            "escrow.qedspec should be Warning-clean but got: {:?}",
            warning_rules
        );
    }

    // ========================================================================
    // Spec-authoring lint regression tests. Each fixture mirrors an audit
    // finding shape — if the lints stop firing, those recurring spec-shape
    // gaps go uncaught.
    // ========================================================================

    /// Fixture mirroring the percolator-CRIT shape: `auth authority` but
    /// no `authority` field on the state. Every handler is reachable by
    /// any signer.
    const UNBOUND_AUTH_FIXTURE: &str = r#"
spec Vault

type State
  | Uninitialized
  | Active of {
      balance : U64,
    }

type Error | InvalidAmount

handler init : State.Uninitialized -> State.Active {
  auth authority
  accounts {
    authority : signer
    vault     : writable
  }
  effect { balance := 0 }
}

handler withdraw (amount : U64) : State.Active -> State.Active {
  auth authority
  accounts {
    authority : signer
    vault     : writable
  }
  requires amount > 0 else InvalidAmount
  effect { balance -= amount }
}
"#;

    #[test]
    fn lint_unbound_auth_fires() {
        let spec =
            crate::chumsky_adapter::parse_str(UNBOUND_AUTH_FIXTURE).expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let unbound: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "unbound_auth")
            .collect();
        assert!(
            !unbound.is_empty(),
            "expected unbound_auth to fire on a spec with `auth authority` and no state field; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// Dotted-auth desugar (`auth <acct>.<field>`) synthesizes a
    /// `requires <acct>.<field> == <signer>.pubkey else Unauthorized`
    /// clause and rewrites `who` to the signer name; `unbound_auth` must
    /// recognize the imported-account binding shape (not just `s.<field>`
    /// state references) and stay silent. Fixture distills the bundled
    /// cross-program-vault `emergency_close` shape.
    const DOTTED_AUTH_BOUND_FIXTURE: &str = r#"
spec Vault

type State
  | Active of {
      total_deposits : U64,
    }

type AdminConfig
  | Active of {
      admin : Pubkey,
    }

type Error | Unauthorized

handler close : State.Active -> State.Active {
  auth admin_config.admin
  accounts {
    admin        : signer
    vault        : writable
    admin_config : type AdminConfig
  }
  effect { total_deposits := 0 }
}
"#;

    #[test]
    fn lint_unbound_auth_silent_on_dotted_auth_desugar() {
        let spec = crate::chumsky_adapter::parse_str(DOTTED_AUTH_BOUND_FIXTURE)
            .expect("dotted-auth fixture should parse");
        let warnings = check_completeness(&spec);
        let unbound: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "unbound_auth")
            .collect();
        assert!(
            unbound.is_empty(),
            "unbound_auth must stay silent when the synthesized `requires \
             <acct>.<field> == <signer>.pubkey` clause binds the signer \
             via an imported account (v2.29.2 escape); got: {:?}",
            unbound.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Fixture mirroring the multisig::approve/reject HIGH: handler
    /// takes `member_index` and mutates `state.voted[member_index]` but
    /// no `requires` binds the index to the signer.
    const UNGUARDED_INDEXED_FIXTURE: &str = r#"
spec Voting

const N = 8

type State
  | Uninitialized
  | Active of {
      voted : Map[N] U8,
      count : U8,
    }

type Error | OutOfRange | MathOverflow

handler vote (member_index : U8) : State.Active -> State.Active {
  auth voter
  accounts {
    voter : signer
    vault : writable
  }
  requires member_index < 8 else OutOfRange
  effect {
    count += 1
    voted[member_index] := 1
  }
}
"#;

    #[test]
    fn lint_unguarded_indexed_mutation_fires() {
        let spec = crate::chumsky_adapter::parse_str(UNGUARDED_INDEXED_FIXTURE)
            .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_indexed_mutation")
            .collect();
        assert!(
            !hits.is_empty(),
            "expected unguarded_indexed_mutation to fire on a vote-by-index handler with no signer↔index binding; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// Fixture mirroring the lending::liquidate HIGH: handler
    /// transitions to a terminal state with no `requires`.
    const UNGUARDED_TERMINAL_FIXTURE: &str = r#"
spec Loan

type State
  | Empty
  | Active of {
      borrower : Pubkey,
      amount   : U64,
    }
  | Liquidated

type Error | NotFound

handler liquidate : State.Active -> State.Liquidated {
  auth liquidator
  accounts {
    liquidator : signer
    loan       : writable
  }
  effect { amount := 0 }
}
"#;

    #[test]
    fn lint_unguarded_terminal_transition_fires() {
        let spec = crate::chumsky_adapter::parse_str(UNGUARDED_TERMINAL_FIXTURE)
            .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_terminal_transition")
            .collect();
        assert!(
            !hits.is_empty(),
            "expected unguarded_terminal_transition to fire on a Liquidated transition with no requires; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// Inverse: when the transition IS gated by an explicit `requires`,
    /// the lint should NOT fire (audit-fixed lending::liquidate shape).
    const GATED_TERMINAL_FIXTURE: &str = r#"
spec Loan

type State
  | Empty
  | Active of {
      borrower   : Pubkey,
      amount     : U64,
      collateral : U64,
    }
  | Liquidated

type Error | AccountHealthy

handler liquidate : State.Active -> State.Liquidated {
  auth liquidator
  accounts {
    liquidator : signer
    loan       : writable
  }
  requires state.amount > state.collateral else AccountHealthy
  effect { amount := 0 }
}
"#;

    #[test]
    fn lint_gated_terminal_transition_does_not_fire() {
        let spec = crate::chumsky_adapter::parse_str(GATED_TERMINAL_FIXTURE)
            .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<&str> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_terminal_transition")
            .map(|w| w.rule.as_str())
            .collect();
        assert!(
            hits.is_empty(),
            "unguarded_terminal_transition should not fire on health-gated liquidate; got: {:?}",
            hits
        );
    }

    // ========================================================================
    // Coverage matrix, write_without_read, circular_lifecycle
    // ========================================================================

    #[test]
    fn test_coverage_matrix_full_coverage() {
        let spec_content = include_str!("../../../../examples/rust/multisig/multisig.qedspec");
        let spec =
            crate::chumsky_adapter::parse_str(spec_content).expect("multisig.qedspec should parse");
        let matrix = coverage_matrix(&spec);
        assert_eq!(matrix.coverage_pct, 100.0);
        assert!(matrix.gaps.is_empty());
        // 8 handlers: create_vault, propose, approve, reject, execute,
        // cancel_proposal, add_member, remove_member.
        assert_eq!(matrix.operations.len(), 8);
        assert_eq!(matrix.properties.len(), 2);
    }

    #[test]
    fn test_coverage_matrix_detects_gaps() {
        let mut h_covered = make_handler("deposit");
        h_covered.effects = vec![("balance".into(), "add".into(), "amount".into())];
        let mut h_uncovered = make_handler("withdraw");
        h_uncovered.effects = vec![("balance".into(), "sub".into(), "amount".into())];

        let spec = ParsedSpec {
            handlers: vec![h_covered, h_uncovered],
            state_fields: vec![("balance".into(), "U64".into())],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("state.balance >= 0".to_string()),
                rust_expression: Some("s.balance >= 0".to_string()),
                rust_expression_pod: Some("s.balance >= 0".to_string()),
                preserved_by: vec!["deposit".to_string()], // only covers deposit
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let matrix = coverage_matrix(&spec);
        assert_eq!(matrix.gaps, vec!["withdraw"]);
        assert!(matrix.coverage_pct < 100.0);
    }

    #[test]
    fn test_write_without_read_lint() {
        let mut h = make_handler("deposit");
        h.guard_str = Some("amount > 0".to_string());
        h.effects = vec![
            ("balance".into(), "add".into(), "amount".into()),
            ("counter".into(), "add".into(), "1".into()),
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("authority".into(), "Pubkey".into()),
                ("balance".into(), "U64".into()),
                ("counter".into(), "U64".into()),
            ],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("s.balance >= 0".to_string()),
                rust_expression: Some("s.balance >= 0".to_string()),
                rust_expression_pod: Some("s.balance >= 0".to_string()),
                preserved_by: vec!["deposit".to_string()],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        // "counter" is written but never read in any guard or property
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("counter")),
            "expected write_without_read for 'counter', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .map(|w| &w.subject)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_circular_lifecycle_no_terminal() {
        let mut h1 = make_handler("advance");
        h1.pre_status = Some("A".to_string());
        h1.post_status = Some("B".to_string());
        let mut h2 = make_handler("retreat");
        h2.pre_status = Some("B".to_string());
        h2.post_status = Some("A".to_string());
        let spec = ParsedSpec {
            handlers: vec![h1, h2],
            lifecycle_states: vec!["A".to_string(), "B".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "circular_lifecycle_no_terminal"),
            "expected circular_lifecycle_no_terminal, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ---- contains_word unit tests ----

    #[test]
    fn test_contains_word_basic() {
        assert!(contains_word("balance > 0", "balance"));
        assert!(contains_word("check balance here", "balance"));
        assert!(!contains_word("imbalance > 0", "balance"));
        assert!(!contains_word("rebalance_flag", "balance"));
        assert!(!contains_word("my_balance_v2", "balance"));
    }

    #[test]
    fn test_contains_word_short_field() {
        // Field "id" must not match inside "valid", "provide", "identity"
        assert!(!contains_word("valid > 0", "id"));
        assert!(!contains_word("provide_service", "id"));
        assert!(!contains_word("identity = true", "id"));
        // But should match when standalone
        assert!(contains_word("id > 0", "id"));
        assert!(contains_word("state.id > 0", "id"));
        assert!(contains_word("check id here", "id"));
    }

    #[test]
    fn test_contains_word_at_boundaries() {
        assert!(contains_word("id", "id"));
        assert!(contains_word("id ", "id"));
        assert!(contains_word(" id", "id"));
        assert!(contains_word("(id)", "id"));
        assert!(contains_word("id+1", "id"));
        assert!(!contains_word("kid", "id"));
        assert!(!contains_word("ids", "id"));
    }

    // ---- write_without_read word-boundary tests ----

    #[test]
    fn test_write_without_read_no_substring_match() {
        // Field "id" written in effects, guard only has "valid" — should NOT count as read
        let mut h = make_handler("update");
        h.effects = vec![("id".to_string(), "set".to_string(), "1".to_string())];
        h.guard_str = Some("valid > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("id".to_string(), "U64".to_string()),
                ("valid".to_string(), "U64".to_string()),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "write_without_read"
                    && w.subject.as_deref() == Some("id")),
            "field 'id' should be flagged as write_without_read when guard only contains 'valid', got: {:?}",
            warnings.iter().filter(|w| w.rule == "write_without_read").collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_write_without_read_bare_word_match() {
        // Field "balance" written in effects, guard has "balance > 0" — should count as read
        let mut h = make_handler("deposit");
        h.effects = vec![(
            "balance".to_string(),
            "add".to_string(),
            "amount".to_string(),
        )];
        h.guard_str = Some("balance > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "write_without_read"
                    && w.subject.as_deref() == Some("balance")),
            "field 'balance' should NOT be flagged when guard contains bare word 'balance', got: {:?}",
            warnings.iter().filter(|w| w.rule == "write_without_read").collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_write_without_read_prefixed_match() {
        // Field "id" written, guard has "state.id > 0" — should count as read
        let mut h = make_handler("update");
        h.effects = vec![("id".to_string(), "set".to_string(), "1".to_string())];
        h.guard_str = Some("state.id > 0".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("id".to_string(), "U64".to_string())],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("id")),
            "field 'id' should NOT be flagged when guard contains 'state.id', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .collect::<Vec<_>>()
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Multi-file spec loader
    // ──────────────────────────────────────────────────────────────────────

    const SPEC_ROOT: &str = r#"
spec Demo

type State
  | Active of { count : U64 }
"#;

    const SPEC_INC: &str = r#"
spec Demo

/// Increments count
handler inc (x : U64) : State.Active -> State.Active {
  effect { count += x }
}
"#;

    const SPEC_DEC: &str = r#"
spec Demo

handler dec (x : U64) : State.Active -> State.Active {
  effect { count -= x }
}
"#;

    #[test]
    fn multi_file_spec_merges_handlers_across_fragments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo.qedspec"), SPEC_ROOT).unwrap();
        std::fs::create_dir_all(dir.path().join("handlers")).unwrap();
        std::fs::write(dir.path().join("handlers/inc.qedspec"), SPEC_INC).unwrap();
        std::fs::write(dir.path().join("handlers/dec.qedspec"), SPEC_DEC).unwrap();

        let parsed = parse_spec_file(dir.path()).unwrap();
        assert_eq!(parsed.program_name, "Demo");
        let names: Vec<_> = parsed.handlers.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"inc"), "got handlers: {:?}", names);
        assert!(names.contains(&"dec"), "got handlers: {:?}", names);
    }

    #[test]
    fn parse_spec_file_surfaces_clear_error_for_missing_path() {
        // A non-existent --spec path must say so explicitly instead of
        // falling through to the extension check ("Unsupported spec format: .").
        let missing = std::path::PathBuf::from("/tmp/does_not_exist_g5.qedspec");
        let err = parse_spec_file(&missing).unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist' in error, got: {err}"
        );
        assert!(
            !err.contains("Unsupported spec format"),
            "should not surface the extension-check error for missing path: {err}"
        );
    }

    #[test]
    fn multi_file_spec_rejects_name_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.qedspec"), SPEC_ROOT).unwrap();
        std::fs::write(
            dir.path().join("b.qedspec"),
            "spec Other\n\nhandler noop : State.Active -> State.Active { effect {} }\n",
        )
        .unwrap();

        let err = parse_spec_file(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("spec name mismatch"),
            "expected name-mismatch error, got: {err}"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // [shape_only_cpi] lint
    // ──────────────────────────────────────────────────────────────────────

    /// Declared Tier-0 interfaces with no `ensures` must not fire
    /// `shape_only_cpi` — firing would force `ensures true` tautologies on
    /// handlers with no meaningful post-condition. The lint still fires for
    /// undeclared interfaces / missing handlers (real spec bugs).
    #[test]
    fn shape_only_cpi_silent_on_declared_tier0_interface() {
        let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert!(
            hits.is_empty(),
            "Tier-0 interface with no `ensures` should not fire shape_only_cpi; got: {:?}",
            hits.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn shape_only_cpi_fires_on_undeclared_interface() {
        let src = r#"spec Demo

handler pay : State.A -> State.A {
  call Jupiter.swap(pool = amm, amount_in = 100, min_out = 90)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one shape_only_cpi warning, got {:?}",
            ws
        );
        assert!(hits[0].message.contains("not declared"));
    }

    #[test]
    fn shape_only_cpi_silent_on_tier1_interface() {
        // Interface declares at least one ensures — no lint should fire.
        let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert!(
            hits.is_empty(),
            "Tier 1 interfaces should not lint, got: {:?}",
            hits
        );
    }

    // ----- cpi_unverified_callee P2 lint -----

    #[test]
    fn cpi_unverified_callee_fires_on_unverified_import() {
        // Simulates an `import Token from "..."` whose provider didn't
        // ship a proof package. The resolver wouldn't have populated
        // `verified_callees` so the lint should fire.
        let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert_eq!(
            ws.len(),
            1,
            "expected one unverified-callee warning; got: {ws:?}"
        );
        assert_eq!(ws[0].rule, "cpi_unverified_callee");
        assert_eq!(ws[0].priority, 2);
        assert!(ws[0].message.contains("Stance-1 axiom"));
        assert!(ws[0].fix.contains(".qed/proofs"));
        assert!(
            ws[0].fix.contains("tokenProofs"),
            "fix message should name the expected lake package; got: {}",
            ws[0].fix
        );
    }

    #[test]
    fn cpi_unverified_callee_silent_when_verified_callees_lists_iface() {
        // Same shape but `verified_callees` has the import registered,
        // simulating a provider that did ship proofs.
        let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let mut parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        parsed
            .verified_callees
            .insert("Token".to_string(), std::path::PathBuf::from("/tmp/x"));
        let ws = check_cpi_unverified_callee(&parsed);
        assert!(
            ws.is_empty(),
            "verified callee should suppress the lint; got: {ws:?}"
        );
    }

    #[test]
    fn cpi_unverified_callee_silent_on_in_spec_interfaces() {
        // Interface declared inline (no `import` statement) — the
        // author owns both the contract and the call, so there's no
        // external trust gap to surface.
        let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert!(
            ws.is_empty(),
            "inline interface (no import) should not fire; got: {ws:?}"
        );
    }

    #[test]
    fn cpi_unverified_callee_silent_on_tier0_imports() {
        // Imported interface with no `ensures` — cpi_no_callee_ensures
        // (P1) owns that case; cpi_unverified_callee should stay quiet.
        let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert!(
            ws.is_empty(),
            "Tier-0 imports should not double-fire; got: {ws:?}"
        );
    }

    #[test]
    fn cpi_unverified_callee_deduplicates_repeated_calls() {
        // Two handlers both calling Token.transfer — the lint should
        // surface the trust-gap once per (interface, handler), not per
        // call site.
        let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay_a : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}

handler pay_b : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 2)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert_eq!(ws.len(), 1, "should dedupe across call sites; got: {ws:?}");
    }

    // ----- end cpi_unverified_callee -----

    #[test]
    fn call_clause_populates_handler_calls() {
        let src = r#"spec Demo

handler exchange : State.A -> State.B {
  call Token.transfer(from = taker_ta, to = initializer_ta, amount = taker_amount)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = &parsed.handlers[0];
        assert_eq!(handler.calls.len(), 1);
        let c = &handler.calls[0];
        assert_eq!(c.target_interface, "Token");
        assert_eq!(c.target_handler, "transfer");
        assert_eq!(c.args.len(), 3);
        assert_eq!(c.args[0].name, "from");
        assert_eq!(c.args[2].name, "amount");
        // Args carry both renderings so backends can pick the form they want.
        assert!(!c.args[0].rust_expr.is_empty());
        assert!(!c.args[0].lean_expr.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────────
    // pragma sbpf { ... } adaptation
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn pragma_sbpf_unpacks_inner_items() {
        let src = r#"spec Transfer

pragma sbpf {
  pubkey TOKEN_PROGRAM [6, 221, 246, 225]

  instruction transfer {
    discriminant 3
    entry 0
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
        assert_eq!(parsed.pubkeys.len(), 1);
        assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].name, "transfer");
    }

    #[test]
    fn pragma_body_adapts_into_standard_parsed_spec_fields() {
        // Items wrapped in `pragma sbpf { ... }` must land in the same
        // ParsedSpec fields downstream consumers already read — pubkeys,
        // instructions, etc. The pragma is a grammatical namespace, not
        // a new parallel tree.
        let src = r#"spec T

pragma sbpf {
  pubkey TOKEN_PROGRAM [1, 2, 3, 4]

  instruction foo {
    discriminant 1
    entry 0
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
        assert!(parsed.has_pragma("sbpf"));
        assert_eq!(parsed.pubkeys.len(), 1);
        assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].name, "foo");
    }

    #[test]
    fn top_level_sbpf_items_now_rejected() {
        // Platform-specifics (pubkey, instruction, assembly) only parse
        // behind `pragma sbpf { ... }` — the grammar keeps them out of the
        // core surface.
        let src = r#"spec T

pubkey TOKEN_PROGRAM [1, 2, 3, 4]
"#;
        assert!(
            crate::chumsky_adapter::parse_str(src).is_err(),
            "top-level `pubkey` should no longer parse"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // ML syntax — let...in in expressions
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn let_in_renders_to_lean_and_rust() {
        let src = r#"spec T
type State | A of { balance : U64 }

handler h (amount : U64) : State.A -> State.A {
  ensures let delta = old(state.balance) - state.balance in delta == amount
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = &parsed.handlers[0];
        assert_eq!(handler.ensures.len(), 1);
        let e = &handler.ensures[0];
        // Lean form uses Lean's let-binding syntax.
        assert!(
            e.lean_expr.contains("let delta :="),
            "expected Lean let-binding, got: {}",
            e.lean_expr
        );
        // Rust form lowers to a block expression.
        assert!(
            e.rust_expr.contains("let delta ="),
            "expected Rust let-in-block, got: {}",
            e.rust_expr
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Smoke test — match and ctors in the grammar.
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn ml_match_and_ctor_already_parse() {
        let src = r#"spec T
type State | Active of { count : U64 } | Closed

handler inspect : State.Active -> State.Active {
  ensures
    match state with
    | Active a => a.count >= 0
    | Closed => true
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.handlers.len(), 1);
        assert_eq!(parsed.handlers[0].ensures.len(), 1);
        // The rendered form should reference both variants.
        let lean = &parsed.handlers[0].ensures[0].lean_expr;
        assert!(lean.contains("Active"), "got: {}", lean);
        assert!(lean.contains("Closed"), "got: {}", lean);
    }

    #[test]
    fn interface_block_populates_parsed_spec() {
        let src = r#"spec Escrow

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:abc"
    verified_with ["proptest", "kani"]
    verified_at  "2026-04-18"
  }

  handler transfer (amount : U64) {
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.interfaces.len(), 1);
        let i = &parsed.interfaces[0];
        assert_eq!(i.name, "Token");
        assert_eq!(
            i.program_id.as_deref(),
            Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
        );

        let u = i.upstream.as_ref().expect("upstream present");
        assert_eq!(u.binary_hash.as_deref(), Some("sha256:abc"));
        // Lean absent by design — no overclaiming.
        assert!(!u.verified_with.contains(&"lean".to_string()));

        assert_eq!(i.handlers.len(), 1);
        let h = &i.handlers[0];
        assert_eq!(h.name, "transfer");
        assert_eq!(h.params, vec![("amount".to_string(), "U64".to_string())]);
        assert_eq!(h.accounts.len(), 3);
        assert_eq!(h.requires.len(), 1);
        assert_eq!(h.ensures.len(), 1);
    }

    #[test]
    fn unchecked_quantifier_lint_fires_for_large_type() {
        // U64 quantifier can't be exhausted — check.rs must warn so the user
        // knows the property is being silently skipped in proptest/Kani.
        let spec = ParsedSpec {
            properties: vec![ParsedProperty {
                name: "all_balances_positive".to_string(),
                expression: Some("∀ v : Nat, v ≥ 0".to_string()),
                rust_expression: Some(
                    "/* QEDGEN_UNSUPPORTED_QUANTIFIER: forall v : U64 \
                     — lower at harness level */"
                        .to_string(),
                ),
                rust_expression_pod: None,
                preserved_by: vec![],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "unchecked_quantifier"),
            "expected unchecked_quantifier lint for U64 forall, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
        let w = warnings
            .iter()
            .find(|w| w.rule == "unchecked_quantifier")
            .unwrap();
        assert_eq!(w.priority, 1, "unchecked_quantifier must be P1");
        assert!(
            w.message.contains("all_balances_positive"),
            "message must name the property"
        );
    }

    #[test]
    fn unchecked_quantifier_lint_does_not_fire_for_u8() {
        // U8 forall lowers to a real iterator — no lint should fire.
        let spec = ParsedSpec {
            properties: vec![ParsedProperty {
                name: "bytes_nonneg".to_string(),
                expression: Some("∀ v : Nat, v ≥ 0".to_string()),
                rust_expression: Some("(u8::MIN..=u8::MAX).all(|v| v >= 0)".to_string()),
                rust_expression_pod: None,
                preserved_by: vec![],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "unchecked_quantifier"),
            "U8 forall must not fire unchecked_quantifier"
        );
    }

    #[test]
    fn multi_file_spec_source_matches_single_file_concat() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1.qedspec"), SPEC_ROOT).unwrap();
        std::fs::write(dir.path().join("2.qedspec"), SPEC_INC).unwrap();

        // read_spec_source must emit fragments in sorted-path order so
        // spec_hash_for_handler finds handler bodies regardless of which
        // fragment they live in.
        let src = read_spec_source(dir.path()).unwrap();
        assert!(
            src.contains("type State"),
            "root fragment missing in merged source"
        );
        assert!(
            src.contains("handler inc"),
            "handler fragment missing in merged source"
        );
    }

    #[test]
    fn build_counterexample_resolves_named_const_in_effect() {
        let handler = ParsedHandler {
            name: "reset".to_string(),
            effects: vec![("counter".to_string(), "set".to_string(), "ZERO".to_string())],
            ..make_handler("reset")
        };
        let constants = vec![("ZERO".to_string(), "0".to_string())];
        let ce = build_counterexample(
            "s.counter \u{2264} 5",
            "bounded",
            &["counter"],
            &handler,
            &["counter"],
            &constants,
        )
        .expect("should produce a counterexample");
        let post = ce
            .post_state
            .iter()
            .find(|(f, _)| f == "counter")
            .unwrap()
            .1;
        assert_eq!(post, 0, "ZERO should resolve to 0, not fall back to 1");
    }

    #[test]
    fn preserved_by_all_potential_violation_fires_for_named_const_effect() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Test
program_id "11111111111111111111111111111111"
const STEP = 5
type State | Active of { counter : U64 }
type Error | E
property counter_small :
  state.counter <= 3
  preserved_by all
handler tick : State.Active -> State.Active {
  permissionless
  effect { counter := STEP }
}"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "preserved_by_all_potential_violation"),
            "must warn when preserved_by all handler demonstrably violates the property"
        );
    }

    /// Transition property `counter >= old(counter)` preserved by an `add`
    /// handler must NOT fire — guards against the counterexample builder
    /// misreading `s'.counter` as a constant and applying the effect to the
    /// `old(...)` side (inverting the relation into a bogus violation).
    #[test]
    fn preserved_by_transition_property_silent_when_add_preserves_monotonicity() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Test
program_id "11111111111111111111111111111111"
type State | Active of { counter : U64 }
type Error | E
property counter_monotonic :
  state.counter >= old(state.counter)
  preserved_by all
handler grow (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { counter += delta }
}"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "preserved_by_all_potential_violation"),
            "add preserves `counter >= old(counter)` — must not flag a violation"
        );
    }

    /// The same transition property `counter >= old(counter)` claimed-
    /// preserved by a `sub` handler MUST still fire — decreasing the post
    /// side genuinely breaks monotonicity.
    #[test]
    fn preserved_by_transition_property_fires_when_sub_breaks_monotonicity() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Test
program_id "11111111111111111111111111111111"
type State | Active of { counter : U64 }
type Error | E
property counter_monotonic :
  state.counter >= old(state.counter)
  preserved_by all
handler shrink : State.Active -> State.Active {
  permissionless
  effect { counter -= 1 }
}"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "preserved_by_all_potential_violation"),
            "sub breaks `counter >= old(counter)` — must flag the violation"
        );
    }

    /// `build_fix_suggestions` must not emit a nonsensical
    /// `requires state.counter > state.counter` guard for a transition
    /// property (same field on both sides). Fix A is suppressed; Fix B
    /// (add to preserved_by) still applies.
    #[test]
    fn build_fix_suggestions_skips_self_guard_for_transition_property() {
        let handler = ParsedHandler {
            name: "shrink".to_string(),
            effects: vec![("counter".to_string(), "sub".to_string(), "1".to_string())],
            ..make_handler("shrink")
        };
        let fixes = build_fix_suggestions(
            "s'.counter \u{2265} s.counter",
            "counter_monotonic",
            &handler,
            &["counter"],
            &["counter"],
        );
        assert!(
            !fixes
                .iter()
                .any(|f| f.snippet.contains("state.counter > state.counter")
                    || f.snippet.contains("state.counter < state.counter")),
            "must not suggest a self-comparison guard; got: {:?}",
            fixes.iter().map(|f| &f.snippet).collect::<Vec<_>>()
        );
        assert!(
            fixes.iter().any(|f| f.label == "Add to preserved_by"),
            "the preserved_by fix should still be offered"
        );
    }

    // ----- PDA seed collision -----

    #[test]
    fn pda_seed_collision_fires_for_identical_seeds() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"
            spec CollisionTest

            pda vault ["vault", user]
            pda escrow ["vault", user]

            state { dummy : U64 }
            "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "pda_seed_collision"),
            "must warn on identical seed tuples; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn pda_seed_collision_no_false_positive_for_distinct_seeds() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"
            spec CollisionTest

            pda vault ["vault", user]
            pda escrow ["escrow", user]

            state { dummy : U64 }
            "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "pda_seed_collision"),
            "must NOT warn when seeds differ by literal discriminator"
        );
    }

    #[test]
    fn pda_seed_possible_collision_fires_when_literals_match_but_vars_differ() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"
            spec CollisionTest

            pda order_a ["order", user_a]
            pda order_b ["order", user_b]

            state { dummy : U64 }
            "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "pda_seed_possible_collision"),
            "must warn on same literals but different variable seeds; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ----- missing_math_overflow lint -----

    #[test]
    fn missing_math_overflow_fires_when_checked_arith_used_without_declaration() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | InvalidAmount

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "missing_math_overflow")
            .expect("expected missing_math_overflow warning");
        assert!(hit.message.contains("deposit"));
        assert!(hit.message.contains("PoolError::MathOverflow"));
    }

    #[test]
    fn missing_math_overflow_silent_when_variant_is_declared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | InvalidAmount

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "should not warn when MathOverflow is declared in Error sum"
        );
    }

    #[test]
    fn missing_math_overflow_silent_when_no_checked_arithmetic() {
        // Spec uses only `effect { x := ... }` (set, no overflow path).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Reset
program_id "11111111111111111111111111111111"
type State | Active of { counter : U64 }
type Error | InvalidAmount

handler clear : State.Active -> State.Active {
  permissionless
  effect { counter := 0 }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "no checked arith → no MathOverflow obligation"
        );
    }

    // ----- -= raises MathUnderflow (with back-compat) -----

    #[test]
    fn missing_math_overflow_fires_on_sub_without_underflow_or_overflow() {
        // Pure `-=` with neither MathOverflow nor MathUnderflow declared
        // → fires for MathUnderflow (the default for `-=`).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | InvalidAmount

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance -= n }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "missing_math_overflow")
            .expect("expected missing_math_overflow warning for MathUnderflow");
        assert!(
            hit.message.contains("MathUnderflow"),
            "v2.24: `-=` defaults to MathUnderflow; message was {:?}",
            hit.message
        );
    }

    #[test]
    fn missing_math_overflow_silent_on_sub_with_only_overflow_declared() {
        // Back-compat: declared MathOverflow but not MathUnderflow →
        // `-=` falls back to MathOverflow; lint stays silent.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance -= n }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "back-compat: only MathOverflow declared → -= falls back; no warning"
        );
    }

    // ----- unknown_error_variant lint -----

    #[test]
    fn unknown_error_variant_fires_on_per_site_override_with_undeclared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MathUnderflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n else MintOverflow }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unknown_error_variant")
            .expect("expected unknown_error_variant warning");
        assert!(hit.message.contains("MintOverflow"));
        assert!(hit.message.contains("deposit"));
    }

    #[test]
    fn unknown_error_variant_fires_on_pragma_with_undeclared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MathUnderflow

pragma checked_overflow_error = MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unknown_error_variant")
            .expect("expected unknown_error_variant warning for pragma");
        assert!(hit.message.contains("checked_overflow_error"));
        assert!(hit.message.contains("MintOverflow"));
    }

    #[test]
    fn unknown_error_variant_silent_when_override_is_declared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n else MintOverflow }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "unknown_error_variant"),
            "per-site override referencing a declared variant should not fire"
        );
        // The site provides an override, so missing_math_overflow defers
        // (the `+=` doesn't fall back to the builtin default).
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "per-site override defers missing_math_overflow"
        );
    }

    // ----- import resolution + interface merge -----

    #[test]
    fn parse_spec_file_resolves_path_imports_and_merges_interface() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Imported interface lives at <dir>/token.qedspec.
        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();

        // Manifest declares a path source.
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();

        // Consumer spec imports the interface.
        let consumer_path = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer_path,
            r#"spec Escrow
import Token from "spl_token"

type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer_path).expect("parse + resolve should succeed");
        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(parsed.imports[0].name, "Token");
        // Token interface from token.qedspec should now be in parsed.interfaces.
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Token"),
            "Token interface should be merged into parsed.interfaces; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn parse_spec_file_errors_when_imports_present_but_no_qed_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer_path = tmp.path().join("escrow.qedspec");
        std::fs::write(
            &consumer_path,
            r#"spec Escrow
import Token from "spl_token"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let err = format!("{:#}", parse_spec_file(&consumer_path).unwrap_err());
        assert!(
            err.contains("no `qed.toml`"),
            "expected `no qed.toml` error, got: {err}"
        );
    }

    #[test]
    fn parse_spec_file_errors_when_bound_name_not_in_imported_source() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        std::fs::write(
            spec_dir.join("other.qedspec"),
            r#"spec OtherIface
interface NotToken {
  program_id "11111111111111111111111111111111"
}
"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "other.qedspec" }
"#,
        )
        .unwrap();
        let consumer_path = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer_path,
            r#"spec Escrow
import Token from "spl_token"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let err = format!("{:#}", parse_spec_file(&consumer_path).unwrap_err());
        assert!(
            err.contains("declares no `interface Token`"),
            "expected `no interface Token` error, got: {err}"
        );
    }

    #[test]
    fn parse_spec_file_no_imports_does_not_require_qed_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("plain.qedspec");
        std::fs::write(
            &path,
            r#"spec Plain
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();
        // No qed.toml, no imports — should parse cleanly.
        let parsed = parse_spec_file(&path).unwrap();
        assert!(parsed.imports.is_empty());
    }

    // ----- qed.lock integration -----

    fn write_simple_path_dep_setup(spec_dir: &std::path::Path) -> std::path::PathBuf {
        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:9c1edeadbeef"
    verified_with ["proptest"]
    verified_at  "2026-04-25"
  }
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();
        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token"

type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();
        consumer
    }

    #[test]
    fn parse_spec_file_auto_writes_lock_with_resolved_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Lock should not exist before parse.
        assert!(!tmp.path().join("qed.lock").exists());

        parse_spec_file(&consumer).expect("parse should succeed and write lock");

        let lock = crate::qed_lock::read(tmp.path())
            .unwrap()
            .expect("lock should be written");
        assert_eq!(lock.dependencies.len(), 1);
        let entry = &lock.dependencies[0];
        assert_eq!(entry.name, "spl_token");
        assert_eq!(entry.source, "path:token.qedspec");
        assert!(entry.spec_hash.starts_with("sha256:"));
        // Path source — no commit, no ref, no sub-path.
        assert!(entry.git_ref.is_none());
        assert!(entry.resolved_commit.is_none());
        // Upstream block from the imported interface flowed through.
        assert_eq!(
            entry.upstream_binary_hash.as_deref(),
            Some("sha256:9c1edeadbeef")
        );
        assert_eq!(entry.upstream_version.as_deref(), Some("4.0.3"));
    }

    #[test]
    fn parse_spec_file_with_lock_frozen_errors_when_lock_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Frozen mode + no lock on disk → error.
        let err = format!(
            "{:#}",
            parse_spec_file_with_lock(&consumer, crate::qed_lock::LockMode::Frozen).unwrap_err()
        );
        assert!(err.contains("stale (--frozen)"), "got: {err}");
    }

    #[test]
    fn parse_spec_file_with_lock_frozen_succeeds_when_lock_current() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Auto first to write the lock, then Frozen to verify it stays current.
        parse_spec_file(&consumer).unwrap();
        parse_spec_file_with_lock(&consumer, crate::qed_lock::LockMode::Frozen)
            .expect("frozen should pass when lock is current");
    }

    #[test]
    fn parse_spec_file_renames_imported_interface_via_as_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();
        // Consumer uses `as Tk` to rename Token → Tk in its namespace.
        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token" as Tk
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer).expect("alias-renamed import should parse + merge");
        // Imported interface should appear under its alias name `Tk`,
        // not the source-side `Token`.
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Tk"),
            "expected interface renamed to `Tk`; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
        assert!(
            !parsed.interfaces.iter().any(|i| i.name == "Token"),
            "the source-side name `Token` should not leak into consumer when an alias is set"
        );
    }

    #[test]
    fn parse_spec_file_resolves_multi_file_imported_dep() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Imported dep is a *directory* of fragments. Each declares the
        // same `spec MultiToken`; one carries the interface, another
        // carries a sidecar event used in the interface's docs.
        let dep_dir = spec_dir.join("multitoken");
        std::fs::create_dir(&dep_dir).unwrap();
        std::fs::write(
            dep_dir.join("a-iface.qedspec"),
            r#"spec MultiToken
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            dep_dir.join("b-event.qedspec"),
            r#"spec MultiToken
event TokenMoved {
  amount : U64,
}
"#,
        )
        .unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "multitoken" }
"#,
        )
        .unwrap();

        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer)
            .expect("multi-file imported dep should parse + merge end-to-end");
        // Token interface from a-iface.qedspec lives in the merged consumer.
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Token"),
            "interface from multi-file dep should be merged in; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn parse_spec_file_errors_when_multi_file_dep_fragments_disagree_on_spec_name() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        let dep_dir = spec_dir.join("bad-multi");
        std::fs::create_dir(&dep_dir).unwrap();
        std::fs::write(
            dep_dir.join("a.qedspec"),
            "spec NameOne\ninterface Token { program_id \"x\" }\n",
        )
        .unwrap();
        std::fs::write(
            dep_dir.join("b.qedspec"),
            "spec NameTwo\nevent E { amount : U64 }\n",
        )
        .unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
bad = { path = "bad-multi" }
"#,
        )
        .unwrap();

        let consumer = spec_dir.join("c.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Caller
import Token from "bad"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let err = format!("{:#}", parse_spec_file(&consumer).unwrap_err());
        assert!(
            err.contains("must declare the same name"),
            "expected name-mismatch error; got: {err}"
        );
    }

    #[test]
    fn parse_spec_file_with_lock_frozen_errors_when_imported_source_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Auto-write a baseline lock, then mutate the imported source — the
        // spec hash should drift, so Frozen catches it.
        parse_spec_file(&consumer).unwrap();
        std::fs::write(
            tmp.path().join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "DIFFERENT11111111111111111111111111111111"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        let err = format!(
            "{:#}",
            parse_spec_file_with_lock(&consumer, crate::qed_lock::LockMode::Frozen).unwrap_err()
        );
        assert!(err.contains("spec_hash"), "got: {err}");
    }

    // ----- Rule 17: invariant_no_body -----

    #[test]
    fn invariant_no_body_fires_on_doc_only_invariant() {
        // The escrow / escrow-split shape: invariant declared with only a
        // description string, no `expr` body. Lean codegen would emit
        // `theorem conservation : True := trivial`.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Demo
type State | Active of { counter : U64 }

invariant conservation "total tokens preserved across all handlers"

handler bump : State.Active -> State.Active {
  auth admin
  accounts { admin : signer }
  effect { counter += 1 }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "invariant_no_body")
            .collect();
        assert_eq!(hits.len(), 1, "expected one finding: {hits:#?}");
        assert!(hits[0].message.contains("conservation"));
    }

    #[test]
    fn invariant_no_body_silent_on_real_body() {
        // An invariant with a proper expression body — no finding.
        // The DSL form: `invariant <name> : <expr>` (one-liner, no
        // preserved_by — the expression body alone is what matters
        // for this lint).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Demo
type State | Active of { counter : U64 }

invariant counter_nonneg : state.counter >= 0

handler bump : State.Active -> State.Active {
  auth admin
  accounts { admin : signer }
  effect { counter += 1 }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "invariant_no_body"),
            "real expr body should suppress: {warnings:#?}"
        );
    }

    // ── P6: pubkey_state_field_unsupported ────────────────────────────────
    //
    // Guards the structural lowering note: a State carrying
    // `authority : Pubkey` lowers to `[u8; 32]` in the verification State;
    // P6 surfaces the lowering at check time.

    #[test]
    fn pubkey_state_field_lint_fires_on_account_type() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec PubkeyState
type State
  | Active of {
      authority : Pubkey,
      balance : U64,
    }
handler h : State.Active -> State.Active {
  permissionless
  effect { balance += 1 }
}
"#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "pubkey_state_field_unsupported")
            .collect();
        assert_eq!(hits.len(), 1, "expected exactly one P6 hit: {hits:#?}");
        let w = hits[0];
        assert!(
            w.message.contains("P6:") && w.message.contains("'authority'"),
            "message must cite P6 and name the field: {}",
            w.message
        );
        // P6 is Info-only: Pubkey state fields lower to `[u8; 32]`
        // automatically; the lint just documents the lowering.
        assert!(
            w.message.contains("lowered to `[u8; 32]`"),
            "message must describe the lowering: {}",
            w.message
        );
        assert_eq!(w.priority, 3, "P6 is now a P3 informational");
        assert_eq!(w.severity, Severity::Info);
    }

    #[test]
    fn pubkey_state_field_lint_silent_without_pubkey_field() {
        // Control: no Pubkey field in state → no P6.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec NoPubkey
type State | Active of { balance : U64 }
handler bump : State.Active -> State.Active {
  permissionless
  effect { balance += 1 }
}
"#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "pubkey_state_field_unsupported"),
            "no Pubkey field → no P6, got: {warnings:#?}"
        );
    }

    #[test]
    fn pubkey_state_field_lint_fires_per_field() {
        // Two Pubkey fields → two P6 lints, each naming its specific
        // field. The non-Pubkey `balance` must not appear in any hit's
        // subject. This pins field-scoped reporting (mirrors how
        // `wrapping_arithmetic` fires per-op).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec PubkeyMulti
type State
  | Active of {
      authority : Pubkey,
      mint : Pubkey,
      balance : U64,
    }
handler h : State.Active -> State.Active {
  permissionless
  effect { balance += 1 }
}
"#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "pubkey_state_field_unsupported")
            .collect();
        assert_eq!(hits.len(), 2, "expected two P6 hits: {hits:#?}");
        let subjects: Vec<&str> = hits
            .iter()
            .map(|w| w.subject.as_deref().unwrap_or(""))
            .collect();
        assert!(
            subjects.iter().any(|s| s.ends_with(".authority")),
            "must name authority: {subjects:?}"
        );
        assert!(
            subjects.iter().any(|s| s.ends_with(".mint")),
            "must name mint: {subjects:?}"
        );
        assert!(
            !subjects.iter().any(|s| s.ends_with(".balance")),
            "must NOT name balance: {subjects:?}"
        );
    }

    // ── P7: undeclared_state_field_in_effect ──────────────────────────────

    #[test]
    fn p7_fires_on_lhs_undeclared_field() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec P7Lhs
type State | Active of { balance : U64 }
handler bump : State.Active -> State.Active {
  permissionless
  effect { undeclared += 1 }
}
"#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "undeclared_state_field_in_effect")
            .collect();
        assert!(
            hits.iter()
                .any(|w| w.message.contains("LHS") && w.message.contains("'undeclared'")),
            "expected LHS hit naming `undeclared`; got: {hits:#?}"
        );
    }

    #[test]
    fn p7_fires_on_rhs_undeclared_state_reference() {
        // RHS check catches `state.<field>` references inside complex
        // expressions. A bare `state.X` RHS goes through render_effect's
        // path-stripping shortcut (it ends up as just `X`), which is
        // indistinguishable from a param reference at lint time — that
        // case is caught downstream by codegen unless the user wrote
        // any composition. We pin the composition case here.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec P7Rhs
type State | Active of { balance : U64 }
handler bump : State.Active -> State.Active {
  permissionless
  effect { balance := state.missing + 1 }
}
"#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "undeclared_state_field_in_effect")
            .collect();
        assert!(
            hits.iter()
                .any(|w| w.message.contains("RHS") && w.message.contains("'missing'")),
            "expected RHS hit naming `missing`; got: {hits:#?}"
        );
    }

    #[test]
    fn p7_silent_when_all_fields_declared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec P7Clean
type State | Active of { balance : U64, total : U64 }
handler add : State.Active -> State.Active {
  permissionless
  effect { total := state.balance }
}
"#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "undeclared_state_field_in_effect"),
            "clean spec must not fire P7, got: {warnings:#?}"
        );
    }

    #[test]
    fn unguarded_arithmetic_accepts_cumulative_bound_across_multiple_adds() {
        // A single `requires state.x + a + b <= U64_MAX` logically bounds
        // both `state.x += a` and `state.x += b`; the lint must accept the
        // cumulative form, not just per-pair patterns.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler deposit (a : U64) (b : U64) : State.Active -> State.Active {
  permissionless
  requires state.balance + a + b <= U64_MAX
  effect {
    balance += a
    balance += b
  }
}
"#,
        )
        .expect("cumulative-bound spec must parse");
        let warnings = check_completeness(&spec);
        let arith_hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_arithmetic")
            .collect();
        assert!(
            arith_hits.is_empty(),
            "cumulative bound should satisfy unguarded_arithmetic for all adds; got: {arith_hits:#?}"
        );
    }

    #[test]
    fn u64_max_builtin_resolves_in_requires_clause() {
        // `U64_MAX` (and friends) are seeded as builtin consts so users
        // don't have to declare `const U64_MAX = …` per spec.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  requires state.balance + n <= U64_MAX
  effect { balance += n }
}
"#,
        )
        .expect("U64_MAX should resolve as a builtin");
        let warnings = check_completeness(&spec);
        // With the U64_MAX guard, unguarded_arithmetic should be silent.
        assert!(
            !warnings.iter().any(|w| w.rule == "unguarded_arithmetic"),
            "U64_MAX builtin should satisfy unguarded_arithmetic; got: {warnings:#?}"
        );
    }

    #[test]
    fn p7_does_not_fire_on_state_variant_promotion() {
        // `state := .Variant { ... }` is the documented variant-promotion /
        // whole-state-assignment form; P7 must not strip the LHS root and
        // flag `state` as an undeclared field.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Lifecycle
program_id "11111111111111111111111111111111"
type State
  | Setup of { x : U64 }
  | Active of { x : U64 }
type Error | E

handler activate : State.Setup -> State.Active {
  permissionless
  effect {
    state := .Active { x := 0 }
  }
}
"#,
        )
        .expect("variant-promotion spec must parse");
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "undeclared_state_field_in_effect"),
            "P7 must not fire on `state := .Variant {{...}}`; got: {warnings:#?}"
        );
    }

    #[test]
    fn p7_ignores_synthetic_match_arm_handlers() {
        // `_case_N` / `_otherwise` synthetic handlers inherit their
        // parent's effects — they don't get a second P7 hit because
        // the parent already covers it.
        let mut spec = ParsedSpec::default();
        spec.account_types.push(ParsedAccountType {
            name: "State".into(),
            fields: vec![("balance".into(), "U64".into())],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        });
        spec.handlers.push(ParsedHandler {
            name: "outer_case_0".into(),
            permissionless: true,
            effects: vec![("undeclared".into(), "set".into(), "0".into())],
            ..synthetic_handler_default("outer_case_0")
        });
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "undeclared_state_field_in_effect"),
            "P7 must not fire on `_case_N` synthetic handlers: {warnings:#?}"
        );
    }

    fn synthetic_handler_default(name: &str) -> ParsedHandler {
        ParsedHandler {
            name: name.into(),
            doc: None,
            who: None,
            on_account: None,
            pre_status: None,
            post_status: None,
            takes_params: vec![],
            guard_str: None,
            guard_str_rust: None,
            aborts_if: vec![],
            requires: vec![],
            ensures: vec![],
            modifies: None,
            let_bindings: vec![],
            aborts_total: false,
            permissionless: false,
            effects: vec![],
            effect_on_error: vec![],
            accounts: vec![],
            transfers: vec![],
            emits: vec![],
            invariants: vec![],
            establishes: vec![],
            properties: vec![],
            schema_includes: vec![],
            calls: vec![],
            effect_branches: None,
            abstract_binders: vec![],
        }
    }

    // Cross-ADT field-ambiguity lint. Three cases:
    //   (a) two ADTs share a field name AND a property references the bare
    //       name → lint fires.
    //   (b) single-ADT spec → never fires (lint short-circuits).
    //   (c) explicit `<adt>.<field>` qualification → does not fire.
    #[test]
    fn cross_adt_field_ambiguity_fires_on_bare_reference() {
        let src = r#"spec Pair

type Distribution
  | Empty
  | Active of {
      authority : Pubkey,
      balance   : U64,
    }

type Claim
  | Empty
  | Active of {
      claimant : Pubkey,
      balance  : U64,
    }

property positive_balance :
  state.balance >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_cross_adt_field_ambiguity(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "cross_adt_field_ambiguity"),
            "expected cross_adt_field_ambiguity to fire on bare `state.balance` ref, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
        );
        // The message names both ADTs so the user can pick.
        let msg = &warnings
            .iter()
            .find(|w| w.rule == "cross_adt_field_ambiguity")
            .unwrap()
            .message;
        assert!(
            msg.contains("Distribution"),
            "message must name Distribution: {}",
            msg
        );
        assert!(msg.contains("Claim"), "message must name Claim: {}", msg);
    }

    #[test]
    fn cross_adt_field_ambiguity_silent_on_single_adt() {
        // Lending's exact shape: two ADTs but no overlapping field names.
        // Cross-ADT lint must stay silent. (We don't try lending itself
        // because the parser needs proper headers; use a synthetic two-ADT
        // spec with disjoint fields.)
        let src = r#"spec Lending

type Pool
  | Uninitialized
  | Active of {
      authority      : Pubkey,
      total_deposits : U64,
    }

type Loan
  | Empty
  | Active of {
      borrower : Pubkey,
      amount   : U64,
    }

property pool_nonneg :
  state.total_deposits >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_cross_adt_field_ambiguity(&spec);
        assert!(
            warnings.is_empty(),
            "no overlapping fields → no lint, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn cross_adt_field_ambiguity_silent_when_qualified() {
        // Same shape as the positive-case fixture, but the property
        // qualifies the reference as `distribution.balance`. The lint
        // must NOT fire — the user has already disambiguated.
        let src = r#"spec Pair

type Distribution
  | Empty
  | Active of {
      authority : Pubkey,
      balance   : U64,
    }

type Claim
  | Empty
  | Active of {
      claimant : Pubkey,
      balance  : U64,
    }

property positive_balance :
  distribution.balance >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_cross_adt_field_ambiguity(&spec);
        assert!(
            warnings.is_empty(),
            "qualified `distribution.balance` should clear the ambiguity, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    // ========================================================================
    // ParsedAccountType.variants populated for multi-variant ADTs
    // ========================================================================

    #[test]
    fn multi_variant_adt_populates_account_variants() {
        // Two-variant state ADT. Flat `fields` view stays the union (first
        // occurrence wins); `variants` carries the per-variant shape so
        // codegen can emit `pub enum State { Setup{...}, Active{...} }`.
        let src = r#"spec Multi
program_id "11111111111111111111111111111111"

type State
  | Setup of { owner : Pubkey }
  | Active of {
      owner : Pubkey,
      pool  : U64,
    }

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let state = spec
            .account_types
            .iter()
            .find(|a| a.name == "State")
            .expect("state account type present");

        assert_eq!(
            state.variants.len(),
            2,
            "two-variant ADT should produce two ParsedVariant entries"
        );
        assert_eq!(state.variants[0].name, "Setup");
        assert_eq!(state.variants[1].name, "Active");
        assert_eq!(state.variants[0].fields.len(), 1);
        assert_eq!(state.variants[1].fields.len(), 2);
        // Flat view stays populated as the union (back-compat).
        assert!(state.fields.iter().any(|(n, _)| n == "owner"));
        assert!(state.fields.iter().any(|(n, _)| n == "pool"));
    }

    #[test]
    fn no_payload_variant_keeps_empty_field_list() {
        // A unit-style variant (no payload) should still appear in
        // `variants` with an empty field list so codegen can emit
        // `pub enum State { Inactive, Active{...} }`.
        let src = r#"spec NoPayload
program_id "11111111111111111111111111111111"

type State
  | Inactive
  | Active of { pool : U64 }

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let state = spec
            .account_types
            .iter()
            .find(|a| a.name == "State")
            .expect("state account type present");
        assert_eq!(state.variants.len(), 2);
        let inactive = state
            .variants
            .iter()
            .find(|v| v.name == "Inactive")
            .expect("unit variant retained");
        assert!(
            inactive.fields.is_empty(),
            "no-payload variant has zero fields"
        );
    }

    // ========================================================================
    // Variant-prefixed effect LHS doesn't false-positive lints
    // ========================================================================

    #[test]
    fn variant_prefixed_lhs_passes_all_effect_lints() {
        // `Active.pool := amount` on a multi-variant ADT state must NOT
        // trigger undeclared_state_field_in_effect (P7 LHS),
        // write_without_read (Rule 13), or unused_field (Rule 4) — all
        // three walk the LHS string and must not treat the variant prefix
        // as a field name.
        let src = r#"spec MultiVar
program_id "11111111111111111111111111111111"

type State
  | Setup of { owner : Pubkey }
  | Active of {
      owner : Pubkey,
      pool  : U64,
    }

type Error
  | MathOverflow

handler activate (amount : U64) : State.Setup -> State.Active {
  auth owner
  requires amount > 0
  effect {
    Active.pool := amount
  }
}

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_completeness(&spec);
        let rules: Vec<&str> = warnings.iter().map(|w| w.rule.as_str()).collect();

        assert!(
            !rules.contains(&"undeclared_state_field_in_effect"),
            "P7 should not fire on `Active.pool := amount` (Active is a variant, pool is its field) — got: {:?}",
            rules
        );
        assert!(
            !rules.contains(&"write_without_read"),
            "write_without_read should match `pool` (read by property) to `Active.pool` (written) — got: {:?}",
            rules
        );
        assert!(
            !rules.contains(&"unused_field"),
            "unused_field should see `pool` as modified via `Active.pool := amount` — got: {:?}",
            rules
        );
    }

    #[test]
    fn variant_prefixed_lhs_still_catches_unknown_field() {
        // A real bug: `Active.poool := amount` (typo). P7 should fire
        // with subject `activate.Active.poool` — the variant prefix is
        // legal, the field name behind it isn't declared anywhere.
        let src = r#"spec MultiVarTypo
program_id "11111111111111111111111111111111"

type State
  | Setup of { owner : Pubkey }
  | Active of {
      owner : Pubkey,
      pool  : U64,
    }

type Error
  | MathOverflow

handler activate (amount : U64) : State.Setup -> State.Active {
  auth owner
  requires amount > 0
  effect {
    Active.poool := amount
  }
}

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_completeness(&spec);
        let p7s: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "undeclared_state_field_in_effect")
            .collect();
        assert_eq!(
            p7s.len(),
            1,
            "expected exactly one P7 hit on the misspelled `poool`, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
        assert!(
            p7s[0].subject.as_deref().unwrap_or("").contains("poool"),
            "P7 subject should name the misspelled field, got: {:?}",
            p7s[0].subject
        );
    }

    // ========================================================================
    // vacuous_property_lowering lint
    // ========================================================================

    const VPL_SPEC_HEAD: &str = r#"
spec VplTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, admin : U64 }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}
"#;

    #[test]
    fn parse_top_level_cmp_handles_simple_comparison() {
        let r = parse_top_level_cmp("s.balance >= s.balance");
        assert_eq!(r, Some(("s.balance", ">=", "s.balance")));
    }

    #[test]
    fn parse_top_level_cmp_handles_equality() {
        let r = parse_top_level_cmp("s.admin == s.admin");
        assert_eq!(r, Some(("s.admin", "==", "s.admin")));
    }

    #[test]
    fn parse_top_level_cmp_returns_none_on_non_comparison() {
        let r = parse_top_level_cmp("s.x + 1");
        assert!(r.is_none(), "expected None on non-comparison; got: {:?}", r);
    }

    #[test]
    fn vpl_lint_silent_on_author_tautology_without_old() {
        // pool.qedspec:660-662 pattern — `state.x == state.x` with no
        // `old(...)` in the AST. The author wants the field surfaced in
        // proofs; the lint must NOT fire.
        let src = format!(
            "{}{}",
            VPL_SPEC_HEAD,
            r#"property admin_tracked : state.admin == state.admin preserved_by all"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings.is_empty(),
            "author-written tautology (no Expr::Old) must not fire; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_silent_on_distinct_sides() {
        // Distinct comparison — silent regardless of `old(...)`.
        let src = format!(
            "{}{}",
            VPL_SPEC_HEAD, r#"property balance_le_max : state.balance <= 1000 preserved_by all"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings.is_empty(),
            "distinct-sides comparison must not fire; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_silent_on_binary_property_post_slice_2() {
        // A binary property (`old(...)` in body) lowers to
        // `post.balance >= pre.balance` — distinct sides, no tautology.
        // If the lint fires here, codegen regressed.
        let src = format!(
            "{}{}",
            VPL_SPEC_HEAD,
            r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_vacuous_property_lowering(&spec);
        let vpl: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "vacuous_property_lowering")
            .collect();
        assert!(
            vpl.is_empty(),
            "binary property correctly lowered to pre/post must not fire VPL; got: {:?}",
            vpl.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_fires_on_literal_true_body() {
        // Construct a property whose rust_expression is the literal "true"
        // — Rule 3 unconditionally fires.
        let mut spec = ParsedSpec::default();
        spec.properties.push(ParsedProperty {
            name: "always_true".to_string(),
            expression: Some("True".to_string()),
            rust_expression: Some("true".to_string()),
            rust_expression_pod: Some("true".to_string()),
            preserved_by: vec![],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        });
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "vacuous_property_lowering"),
            "literal `true` body must fire VPL; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
        );
    }

    // ========================================================================
    // old_in_single_state_context lint
    // ========================================================================

    const OLD_SSC_SPEC_HEAD: &str = r#"
spec OldSscTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64 }

type Error
  | E
  | BadGuard
"#;

    #[test]
    fn old_ssc_lint_fires_on_old_in_requires() {
        // `old(...)` inside a `requires` body — category error, P1.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  requires state.balance >= old(state.balance) else BadGuard
  effect { balance := balance + delta }
}
"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "old_in_single_state_context"),
            "expected lint to fire on old() inside requires; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
        );
        let w = &warnings[0];
        assert_eq!(w.severity, Severity::Warning);
        assert_eq!(w.priority, 1);
        assert!(w.message.contains("requires"), "msg: {}", w.message);
    }

    #[test]
    fn old_ssc_lint_fires_on_old_in_invariant() {
        // `old(...)` inside an `invariant` body — category error, P1.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
invariant balance_nondec : state.balance >= old(state.balance)

handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}
"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "old_in_single_state_context"
                    && w.message.contains("invariant")),
            "expected lint to fire on old() inside invariant; got: {:?}",
            warnings.iter().map(|w| (&w.rule, &w.message)).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn old_ssc_lint_silent_on_clean_requires() {
        // `requires` without `old(...)` — silent, no false positive.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  requires delta > 0 else BadGuard
  effect { balance := balance + delta }
}
"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings.is_empty(),
            "clean requires must not fire the lint; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn old_ssc_lint_silent_on_old_in_ensures() {
        // `old(...)` inside `ensures` — the right context, must NOT fire.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
  ensures state.balance >= old(state.balance)
}
"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings.is_empty(),
            "old() in ensures must not fire the lint; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn old_ssc_lint_silent_on_old_in_property() {
        // `old(...)` inside a `property` body — the right context, must
        // NOT fire.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}

property balance_monotonic :
  state.balance >= old(state.balance)
  preserved_by all
"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings.is_empty(),
            "old() in property body must not fire the single-state lint; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_fires_on_unsupported_quantifier_marker() {
        // Construct a property whose rust_expression carries the marker
        // — Rule 2 unconditionally fires.
        let mut spec = ParsedSpec::default();
        spec.properties.push(ParsedProperty {
            name: "stub_forall".to_string(),
            expression: Some("forall x : U64, x > 0".to_string()),
            rust_expression: Some(format!(
                "/* {} : forall x : U64, x > 0 */ true",
                QEDGEN_UNSUPPORTED_MARKER
            )),
            rust_expression_pod: Some("true".to_string()),
            preserved_by: vec![],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        });
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "vacuous_property_lowering"
                    && w.message.contains("QEDGEN_UNSUPPORTED_QUANTIFIER")),
            "marker body must fire VPL with marker mention; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    /// ref_impl with multiplication over U64 params trips the lint: Lean
    /// lowers to `Nat` (no overflow); Rust runs `u64 * u64` which can wrap
    /// or panic.
    #[test]
    fn ref_impl_with_multiplication_over_u64_fires_unbounded_arith_lint() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl scaled (a : U64) (b : U64) : U64 = a * b

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_ref_impl_unbounded_arith(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"
                    && w.subject.as_deref() == Some("scaled")),
            "expected ref_impl_unbounded_arith on `scaled`; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
    }

    /// Pure-division ref_impl doesn't trip the lint — `/` cannot produce
    /// values exceeding the inputs in unsigned arithmetic.
    #[test]
    fn ref_impl_with_division_only_does_not_fire_unbounded_arith_lint() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl half (a : U64) : U64 = a / 2

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_ref_impl_unbounded_arith(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"),
            "lint should not fire on division-only ref_impl; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
    }

    /// Ref impls without bounded-numeric params (e.g., Pubkey predicates)
    /// don't trip the lint even when they do arithmetic on other inputs.
    /// Lean and Rust agree on Bool / Pubkey semantics, so no gap.
    #[test]
    fn ref_impl_with_no_numeric_params_does_not_fire_unbounded_arith_lint() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { admin : Pubkey }

ref_impl is_admin (who : Pubkey) (admin : Pubkey) : Bool = who == admin

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect {}
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_ref_impl_unbounded_arith(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"),
            "lint should not fire when ref_impl has no bounded-numeric IO; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
    }

    // ------------------------------------------------------------------
    // collect_require_verified_findings
    // ------------------------------------------------------------------

    #[test]
    fn require_verified_fires_on_unverified_import_with_ensures() {
        // Non-sentinel binary_hash so the sentinel exemption doesn't
        // intercept. `verified_callees` is empty → provider shipped no
        // proof package → finding.
        let src = r#"spec Demo

import Token from "amm_lib"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert_eq!(
            findings.len(),
            1,
            "expected one finding for unverified Token; got: {findings:?}"
        );
        assert_eq!(findings[0].interface_name, "Token");
        assert!(
            findings[0].fix_hint.contains(".qed/proofs"),
            "fix hint should point at the proof-package path; got: {}",
            findings[0].fix_hint
        );
    }

    #[test]
    fn require_verified_silent_when_provider_shipped_proofs() {
        // verified_callees populated → provider has proofs → no finding.
        let src = r#"spec Demo

import Token from "amm_lib"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let mut parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        parsed
            .verified_callees
            .insert("Token".to_string(), std::path::PathBuf::from("/tmp/x"));
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "verified callee must suppress the finding; got: {findings:?}"
        );
    }

    #[test]
    fn require_verified_silent_on_tier0_imports() {
        // No ensures clauses on any handler → Tier 0. Owned by the
        // cpi_no_callee_ensures P1 lint, not by --require-verified.
        let src = r#"spec Demo

import Token from "amm_lib"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "Tier-0 (no ensures) imports must not fire --require-verified; got: {findings:?}"
        );
    }

    #[test]
    fn require_verified_silent_on_sentinel_pinned_natives() {
        // Sentinel binary_hash (sha256:00…00) marks a native program
        // (System Program style) — the validator runtime is the trust
        // boundary, not a proof package. `--require-verified` exempts
        // these so any spec that imports `from "system"` doesn't
        // false-fail.
        let src = r#"spec Demo

import System from "system_lib"

interface System {
  program_id "11111111111111111111111111111111"
  upstream { binary_hash "sha256:0000000000000000000000000000000000000000000000000000000000000000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call System.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "sentinel-pinned native must be exempt; got: {findings:?}"
        );
    }

    #[test]
    fn require_verified_silent_on_inline_interfaces() {
        // Interface declared inline (no `import` statement) — author
        // owns both sides of the contract. `--require-verified` only
        // gates on imported interfaces.
        let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "inline interfaces must not fire; got: {findings:?}"
        );
    }

    // ----- end collect_require_verified_findings -----

    // ------------------------------------------------------------------
    // ParsedSpec.verified_proof_pkgs population — pins the
    // resolver→ParsedSpec wiring (the `lake build` runner is exercised
    // end-to-end elsewhere).
    // ------------------------------------------------------------------

    #[test]
    fn verified_proof_pkgs_populated_when_provider_ships_proof_package() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Provider qedspec at spec_dir/token.qedspec.
        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec TokenLib
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}
"#,
        )
        .unwrap();
        // Proof package alongside the qedspec — both module + lakefile
        // must be present for `has_proofs` to be true.
        let proofs_dir = spec_dir.join(".qed").join("proofs");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        std::fs::write(proofs_dir.join("Token.lean"), "-- stub proof").unwrap();
        std::fs::write(proofs_dir.join("lakefile.lean"), "package tokenProofs").unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();
        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token"

type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer).expect("parse should succeed");
        assert_eq!(
            parsed.verified_proof_pkgs.len(),
            1,
            "expected 1 proof package; got {:?}",
            parsed.verified_proof_pkgs
        );
        assert!(
            parsed.verified_proof_pkgs[0].ends_with(".qed/proofs")
                || parsed.verified_proof_pkgs[0].ends_with(".qed\\proofs"),
            "should point at the provider's proof package root; got: {}",
            parsed.verified_proof_pkgs[0].display()
        );
    }

    #[test]
    fn verified_proof_pkgs_empty_when_no_provider_proofs() {
        // No `.qed/proofs/` alongside the provider qedspec → resolver
        // sets has_proofs=false → no entry in verified_proof_pkgs.
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());
        let parsed = parse_spec_file(&consumer).expect("parse should succeed");
        assert!(
            parsed.verified_proof_pkgs.is_empty(),
            "no provider proofs → empty list; got: {:?}",
            parsed.verified_proof_pkgs
        );
    }

    // ----- end verified_proof_pkgs -----
}
