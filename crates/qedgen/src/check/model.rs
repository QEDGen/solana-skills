//! Parsed-spec data model: the `Parsed*` AST types, their impls, and the
//! report/diagnostic types shared across the check submodules.

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
