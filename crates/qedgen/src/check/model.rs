//! Parsed-spec data model: the `Parsed*` AST types, their impls, and the
//! report/diagnostic types shared across the check submodules.

#[derive(Debug, serde::Serialize)]
pub struct PropertyStatus {
    pub name: String,
    pub status: Status,
    /// From doc: clause or auto-generated.
    pub intent: Option<String>,
    pub suggestion: Option<String>,
}

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
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

/// Handler-level `let name = expr` binding. `rust_expr` carries the
/// adapter's binding-site rendering (including the `as u64` narrowing for
/// `mul_div_*` RHSs); `tree` is the typed RHS (#156).
#[derive(Debug, Clone)]
pub struct ParsedLetBinding {
    pub name: String,
    pub rust_expr: String,
    pub tree: Option<crate::mir::ExprTree>,
}

/// Parsed requires clause. When `error_name` is Some, generates both a guard
/// (positive form in transition) and an abort theorem (negated form).
#[derive(Debug, Clone, Default)]
pub struct ParsedRequires {
    pub lean_expr: String,
    pub rust_expr: String,
    pub rust_expr_pod: String,
    /// Math-exact Rust rendering for harness predicates: integer arithmetic
    /// inside comparisons is widened to u128/i128 (and `-` saturates), so
    /// evaluating the predicate can't overflow-panic on unconstrained
    /// symbolic state. Matches the Lean `Nat` model exactly; the Kani /
    /// proptest guard positions consume this, the Anchor/Quasar scaffolds
    /// keep `rust_expr` (issue #146).
    pub rust_expr_math: String,
    pub error_name: Option<String>,
    /// Source AST body for AST-level lints (e.g. `old_in_single_state_context`).
    /// `None` for synthetic requires from `match`-arm desugaring.
    pub ast_body: Option<crate::ast::Node<crate::ast::Expr>>,
    /// Typed, name-resolved expression tree (#151 Slice 0), built from the
    /// canonicalized AST at adapt time. `None` only for legacy ingest
    /// paths (IDL, probes) and hand-built test fixtures.
    pub tree: Option<crate::mir::ExprTree>,
}

/// Parsed ensures clause: post-condition relating pre and post state.
/// In lean_expr, `old(state.x)` is rendered as `s.x` (pre-state) and
/// `state.x` as `s'.x` (post-state).
#[derive(Debug, Clone)]
pub struct ParsedEnsures {
    pub lean_expr: String,
    pub rust_expr: String,
    /// Read only by the pod-render parity corpus
    /// (`corpus_parity_with_legacy_rust_strings`) — production pod
    /// rendering flows from the tree.
    #[cfg_attr(not(test), allow(dead_code))]
    pub rust_expr_pod: String,
    /// Binary-mode rendering (`state.x` → `post.x`, `old(state.x)` → `pre.x`)
    /// for the ensures-preservation Kani harness; `rust_expr` flattens both to
    /// `s.x`, losing the pre/post distinction.
    pub rust_expr_binary: String,
    /// Binary-mode + math-exact rendering (see
    /// `ParsedRequires::rust_expr_math`) for harness asserts (issue #146).
    pub rust_expr_binary_math: String,
    /// Typed expression tree (#151 Slice 0); `old(...)` stays a structural
    /// `Old` node — renderer context decides the pre/post receivers.
    pub tree: Option<crate::mir::ExprTree>,
}

/// Parsed cover block (reachability).
#[derive(Debug, Clone)]
pub struct ParsedCover {
    pub name: String,
    pub traces: Vec<Vec<String>>,
    /// `(op, when_lean_expr, when_tree)` — the typed tree rides with the
    /// pre-rendered Lean form (#156).
    #[allow(clippy::type_complexity)]
    pub reachable: Vec<(String, Option<String>, Option<crate::mir::ExprTree>)>,
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
    pub rust_expr: Option<String>,
    /// Source AST body for the `old_in_single_state_context` lint.
    /// `None` for the description-only form.
    pub ast_body: Option<crate::ast::Node<crate::ast::Expr>>,
    /// Typed expression tree (#151 Slice 0); `None` for description-only.
    pub tree: Option<crate::mir::ExprTree>,
}

/// Parsed environment block (external state).
#[derive(Debug, Clone)]
pub struct ParsedEnvironment {
    pub name: String,
    pub mutates: Vec<(String, String)>, // (field, type)
    /// Typed fields in distinct external namespaces: `(object, field, type)`.
    pub external_fields: Vec<(String, String, String)>,
    /// Typed metadata for each constraint, one per declared `constraint`.
    pub typed_constraints: Vec<ParsedEnvironmentConstraint>,
}

/// Structural carrier for an environment constraint.
///
/// The rendered strings deliberately coexist with the tree during the
/// migration: current generators still consume them, while new consumers can
/// distinguish a unary post-state assumption from a binary pre/post relation
/// without parsing target code.
#[derive(Debug, Clone)]
pub struct ParsedEnvironmentConstraint {
    pub tree: Option<crate::mir::ExprTree>,
    pub class: PropertyClass,
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
    /// Pod-aware Rust body for Quasar; read only by the pod-render parity
    /// corpus — production pod rendering flows from the tree.
    #[cfg_attr(not(test), allow(dead_code))]
    pub rust_expression_pod: Option<String>,
    /// Math-exact Rust body (see `ParsedRequires::rust_expr_math`): the
    /// Kani/proptest property predicate fns consume this so evaluating a
    /// property with internal arithmetic can't overflow-panic (issue #146).
    pub rust_expression_math: Option<String>,
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
    /// Typed expression tree (#151 Slice 0). `None` only on hand-built
    /// test fixtures.
    pub tree: Option<crate::mir::ExprTree>,
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

/// Sentinel embedded by the Rust sum lowerings when a `sum` binder has no
/// finite `Fin[N]` domain. Unlike the quantifier marker (a comment plus a
/// `true` stub), this renders as a bare comment in expression position, so
/// every consumer MUST skip expressions carrying it.
pub const QEDGEN_UNSUPPORTED_SUM_MARKER: &str = "QEDGEN_UNSUPPORTED_SUM";

/// Does this Rust-rendered expression require harness-level scaffolding?
pub fn rust_expr_is_unsupported(rust_expr: &str) -> bool {
    rust_expr.contains(QEDGEN_UNSUPPORTED_MARKER)
        || rust_expr.contains(QEDGEN_UNSUPPORTED_SUM_MARKER)
}

/// Does this lifecycle state NAME denote account nonexistence (the account
/// has not been created yet / was closed)? Shared by the Crucible PDA
/// materialization gate and domain-sequence seeding so the two never drift.
/// Deliberately excludes names like `Inactive`/`Paused`, which describe an
/// EXISTING account in a logical state.
pub fn state_name_is_nonexistent(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "uninitialized" | "uninitialised" | "not_initialized" | "not_initialised" | "empty"
    )
}

/// PDA seed declaration from a qedspec block.
#[derive(Debug, Clone)]
pub struct ParsedPda {
    pub name: String,
    pub seeds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDimension {
    pub name: String,
    pub base: String,
}

/// Event declaration from a qedspec block.
#[derive(Debug, Clone)]
pub struct ParsedEvent {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

// ============================================================================
// sBPF-specific structures
// ============================================================================

/// Known pubkey as 4-chunk U64 representation.
#[derive(Debug, Clone)]
pub struct ParsedPubkey {
    pub name: String,
    pub chunks: Vec<String>, // 4 U64 values as strings
}

/// A field in an input/instruction layout with byte offset.
#[derive(Debug, Clone)]
pub struct ParsedLayoutField {
    pub name: String,
    pub field_type: String,
    pub offset: i64,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
    pub description: Option<String>,
}

/// An sBPF validation guard.
#[derive(Debug, Clone)]
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
pub struct ParsedSbpfProperty {
    pub name: String,
    pub doc: Option<String>,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
    pub kind: SbpfPropertyKind,
}

/// The different kinds of sBPF properties.
#[derive(Debug, Clone)]
#[allow(dead_code)] // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
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
#[allow(dead_code)] // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
pub enum FlowKind {
    FromSeeds(Vec<String>),
    Through(Vec<String>),
}

/// A single instruction handler in an sBPF program.
#[derive(Debug, Clone)]
pub struct ParsedInstruction {
    pub name: String,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
    pub doc: Option<String>,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
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
pub struct ParsedErrorCode {
    pub name: String,
    pub value: Option<u64>,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
    pub description: Option<String>,
}

// ============================================================================
// Unified handler types (v3 — target-agnostic)
// ============================================================================

/// One effect statement, fully self-contained — replaces the four
/// index-aligned parallel arrays (#151 Slice 4).
#[derive(Debug, Clone)]
pub struct ParsedEffect {
    /// Target field path (`counter`, `accounts[i].capital`, `Variant.field`).
    pub field: String,
    /// Name-resolved, structured target path. Production effects carry this
    /// from the adapter so MIR/codegen never need to reparse `field` or
    /// discover subscripts with string scanning. `None` is reserved for
    /// legacy ingest and hand-built fixtures.
    pub lhs: Option<crate::mir::expr_tree::TreePath>,
    /// Op kind: "set" | "add" | "add_sat" | "add_wrap" | "sub" | "sub_sat" |
    /// "sub_wrap". "add"/"sub" are the checked defaults; `_sat` / `_wrap`
    /// carry the explicit opt-in from `+=!` / `+=?`.
    pub op: String,
    /// Legacy single-form value string (the old triple's third slot).
    pub value: String,
    /// Adapter-rendered Rust RHS (the old `effects_rust` slot). Compound RHS
    /// (ref_impl calls, conditionals, arithmetic) is rendered from the
    /// canonicalized typed AST via `expr_to_rust`, so the Kani/proptest
    /// harnesses receive compilable Rust instead of the Lean-form string in
    /// `value` (issues #143/#144). Simple values (params, literals, bare
    /// fields) carry the same string as `value` so binder-specific
    /// resolution (`resolve_value`) keeps working. Empty for handlers built
    /// outside the chumsky adapter (IDL ingest, probes) — MIR lowering
    /// falls back to `value`.
    pub value_rust: String,
    /// Per-site `or <ErrorVariant>` override (checked add/sub only).
    /// `None` without an explicit `or`, and for saturating / wrapping /
    /// `Set` effects where overrides are meaningless.
    pub on_error: Option<String>,
    /// Typed RHS tree (#151); `None` for legacy ingest and hand-built
    /// fixtures.
    pub tree: Option<crate::mir::ExprTree>,
}

impl ParsedEffect {
    /// Build a legacy-shaped effect from the old `(field, op, value)`
    /// triple — no adapter Rust rendering, no per-site override, no typed
    /// tree. Used by hand-built test fixtures; MIR lowering falls back to
    /// `value` when `value_rust` is empty. (Test-only today — drop the cfg
    /// if a non-adapter ingest path starts producing effects.)
    #[cfg(test)]
    pub fn from_triple(
        field: impl Into<String>,
        op: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        ParsedEffect {
            field: field.into(),
            lhs: None,
            op: op.into(),
            value: value.into(),
            value_rust: String::new(),
            on_error: None,
            tree: None,
        }
    }
}

/// A unified handler — replaced the pre-unification per-frontend shapes
/// (`ParsedOperation` for Quasar, `ParsedInstruction` for sBPF, both since
/// deleted). Represents any callable entry point with guards, effects,
/// accounts, and properties.
#[derive(Debug, Clone, Default)]
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
    /// Requires clauses: guard + optional abort. When error_name is Some,
    /// generates both transition guard and abort theorem.
    pub requires: Vec<ParsedRequires>,
    /// Post-conditions (ensures clauses). Uses s' for post-state, s for old().
    pub ensures: Vec<ParsedEnsures>,
    /// Frame condition: fields that may be modified. All others must stay unchanged.
    pub modifies: Option<Vec<String>>,
    /// Handler-level `let name = expr` bindings, in declaration order
    /// (later bindings may reference earlier ones).
    pub let_bindings: Vec<ParsedLetBinding>,
    /// All abort conditions are exhaustive — generates ↔ theorem instead of per-abort.
    pub aborts_total: bool,
    /// Deliberately permissionless — no `auth` required. Mutually exclusive
    /// with `who` (check.rs rejects both); opts out of the `no_access_control` P1 lint.
    pub permissionless: bool,
    /// State effects, one self-contained `ParsedEffect` per site (#151
    /// Slice 4 — replaces the four index-aligned parallel arrays).
    pub effects: Vec<ParsedEffect>,
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
    /// Scrutinee expression rendered for Lean.
    pub scrutinee_lean: String,
    /// Typed scrutinee tree (#151 Slice 0).
    pub scrutinee_tree: Option<crate::mir::ExprTree>,
    pub arms: Vec<ParsedEffectArm>,
}

/// One arm of a `ParsedEffectBranches`.
#[derive(Debug, Clone)]
pub struct ParsedEffectArm {
    pub pattern_lean: String,
    /// Typed pattern tree (#156); `None` for wildcard arms.
    pub pattern_tree: Option<crate::mir::ExprTree>,
    /// `true` for a wildcard arm.
    pub is_wildcard: bool,
    /// Per-arm effects, one self-contained `ParsedEffect` per site (#151
    /// Slice 4). Consumed per-arm by `lower_handler`'s `Stmt::Branch`
    /// lowering, so each arm's checked effects carry their own abort error.
    pub effects: Vec<ParsedEffect>,
}

/// A resolved `call Target.handler(...)` site inside a handler body. The
/// target is split into interface + handler name for easier lookup; args
/// carry both Lean and Rust renderings so backends can pick their form.
#[derive(Debug, Default, Clone)]
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
pub struct ParsedCallArg {
    pub name: String,
    pub rust_expr: String,
    /// Typed argument tree (#151 Slice 0).
    pub tree: Option<crate::mir::ExprTree>,
}

/// One entry in a `call X.y(state_binders { ... })` block: maps a callee-side
/// abstract field name to a caller-side State field.
///
/// The binder RHS must be a `state.<ident>` path — the adapter validates the
/// shape and extracts the trailing identifier; richer RHS forms are reserved
/// for v3.0. Substitution helpers synthesize `pre.` / `post.` prefixes and
/// Lean `(·.<caller_field>)` at use sites.
#[derive(Debug, Default, Clone)]
pub struct ParsedStateBinder {
    /// Callee abstract field name, matched verbatim (word-boundary) in the
    /// callee's `ensures` text.
    pub callee_field: String,
    /// Caller-side bare field name (trailing ident from `state.<ident>`).
    pub caller_field: String,
}

impl ParsedHandler {
    pub fn has_guard(&self) -> bool {
        !self.requires.is_empty()
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
    /// Typed amount tree (#156); `None` when no amount declared.
    pub amount_tree: Option<crate::mir::ExprTree>,
    pub authority: Option<String>,
}

/// Full parsed spec context.
#[derive(Debug, Default, Clone)]
pub struct ParsedSpec {
    /// Unified handlers from handler/operation/instruction blocks.
    pub handlers: Vec<ParsedHandler>,

    // Legacy fields — populated by forward bridge for backward compat.
    pub invariants: Vec<ParsedInvariant>,
    pub properties: Vec<ParsedProperty>,
    pub program_id: Option<String>,
    pub program_name: String,
    /// Flat union of state fields across account types (single-account: the
    /// account's fields; multi-account: the primary account's).
    pub state_fields: Vec<(String, String)>,
    /// Flat lifecycle states (union across all account types for backward compat).
    pub lifecycle_states: Vec<String>,
    pub pdas: Vec<ParsedPda>,
    pub events: Vec<ParsedEvent>,
    pub error_codes: Vec<String>,
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
    pub pubkeys: Vec<ParsedPubkey>,
    /// Instruction handlers (sBPF mode).
    pub instructions: Vec<ParsedInstruction>,
    /// Global error codes with values (sBPF `Name = value "desc"` syntax).
    pub valued_errors: Vec<ParsedErrorCode>,
    /// Global named constants (`const NAME = VALUE`).
    pub constants: Vec<(String, String)>,
    /// Type aliases: `type AccountIdx = Fin[MAX_ACCOUNTS]` etc.
    /// Stored as (alias_name, rendered_target). Target is `Fin[N]`, `Nat`,
    /// a record name, etc. — whatever `TypeRef` the source points at.
    pub type_aliases: Vec<(String, String)>,
    /// Nominal numeric declarations. Each also appears in `type_aliases` so
    /// codegen erases it to the declared integer ABI type.
    pub dimensions: Vec<ParsedDimension>,
    /// Cover blocks (reachability properties).
    pub covers: Vec<ParsedCover>,
    /// Liveness properties (leads-to).
    pub liveness_props: Vec<ParsedLiveness>,
    /// Environment blocks (external state).
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
    pub verified_callees: std::collections::BTreeMap<String, std::path::PathBuf>,

    /// proof_hash drift detected during qed.lock reconciliation in Frozen
    /// mode; empty in Auto/Skip (lock auto-writes) and in Frozen without
    /// drift. main.rs routes these via `upstream_check::route_findings` —
    /// P2 by default, CRIT under `--strict`.
    pub proof_hash_findings: Vec<crate::upstream_check::DepCheckResult>,

    /// Proof-package dirs of every imported interface in the transitive
    /// closure that ships a Lake-buildable proof package (DFS-pre-order,
    /// deduped). `qedgen verify --recursive` builds each bottom-up so "dep
    /// graph fully proven" reduces to "every layer's Lake build succeeds."
    pub verified_proof_pkgs: Vec<std::path::PathBuf>,

    /// Per-import account-type bookkeeping: local name (alias or source
    /// `bound_name`) → `ImportedNamespace`. Populated when an imported
    /// source carries `type` declarations beyond the interface-stub shape;
    /// empty for interface-only imports (bundled stdlib stubs). Drives
    /// `codegen_mir::emit_imported_mirror` (`src/imported/<ns>.rs`) and
    /// `<ns>.<Type>` resolution in account-binding positions.
    pub imported_namespaces: std::collections::BTreeMap<String, ImportedNamespace>,
}

/// Adapted form of `ast::RefImplDecl`; carries both Lean and Rust renderings
/// so the body lowers into Spec.lean (`def`) and the impl-targeted Kani
/// harness (inlined at the assertion site).
///
/// Deliberately NOT a tree carrier (#223 decision): the bodies are whole
/// function bodies rendered once at adapt time and emitted verbatim —
/// never re-targeted across binders/receivers — so the string lane is the
/// design, not a #156 port gap. See `mir::RefImpl` for the full rationale.
#[derive(Debug, Clone)]
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
    /// Typed initial-value tree (#156) — every backend renders from it.
    pub init_tree: Option<crate::mir::ExprTree>,
    pub updates: Vec<ParsedGhostUpdate>,
}

/// One `on <handler>` update of a ghost. `value_*` is the *complete* new
/// value of the ghost after the handler runs (the assignment operator has
/// already been folded in: `+= d` became `<ghost> + (d)`), so each backend
/// just emits `<ghost> := <value>`.
#[derive(Debug, Clone)]
pub struct ParsedGhostUpdate {
    pub handler: String,
    pub value_rust: String,
    /// Typed complete-new-value tree (#156; op already folded in).
    pub value_tree: Option<crate::mir::ExprTree>,
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
    /// Typed assert tree (#156) — every backend renders from it.
    pub tree: Option<crate::mir::ExprTree>,
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

    /// A spec that mirrors a real on-chain `#[account]` struct — `pragma
    /// state_struct = <Name>` (the struct's real name) and/or `pragma
    /// state_module = <path>` (its module). Both are brownfield-only signals:
    /// the program already exists, so codegen must NOT synthesize a greenfield
    /// Rust scaffold for it. `codegen_mir` never reads these pragmas, so the
    /// scaffold-skip they gate changes no scaffold output. Single source of
    /// truth for the Codegen dispatch's scaffold decision.
    pub fn is_struct_mirror(&self) -> bool {
        self.pragma_value("state_struct").is_some() || self.pragma_value("state_module").is_some()
    }

    /// Look up a `pragma <key> = <value>` assignment.
    pub fn pragma_value(&self, key: &str) -> Option<&str> {
        self.pragma_assignments
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// All values for a repeatable pragma key, in source order. Used by
    /// `pragma harness_use` (one `use` path per line — a spec may need several).
    pub fn pragma_values(&self, key: &str) -> Vec<&str> {
        self.pragma_assignments
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// `import Name from "key" [as Alias]` statement, captured before resolution.
/// `name` must match a declared `interface Name` in the imported source;
/// `from` keys into `qed.toml`'s `[dependencies]`. The local name at
/// `call ...` sites is `as_name.unwrap_or(name)`.
#[derive(Debug, Default, Clone)]
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

/// Callee contract: program ID + per-handler shape (and optional effects).
#[derive(Debug, Default, Clone)]
pub struct ParsedInterface {
    pub name: String,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
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
pub struct ParsedUpstream {
    #[allow(dead_code)]
    // upstream {} pin metadata: parsed for the declared block shape; only version + binary_hash are consumed today
    pub package: Option<String>,
    pub version: Option<String>,
    #[allow(dead_code)]
    // upstream {} pin metadata: parsed for the declared block shape; only version + binary_hash are consumed today
    pub source: Option<String>,
    pub binary_hash: Option<String>,
    #[allow(dead_code)]
    // upstream {} pin metadata: parsed for the declared block shape; only version + binary_hash are consumed today
    pub idl_hash: Option<String>,
    #[allow(dead_code)]
    // upstream {} pin metadata: parsed for the declared block shape; only version + binary_hash are consumed today
    pub verified_with: Vec<String>,
    #[allow(dead_code)]
    // upstream {} pin metadata: parsed for the declared block shape; only version + binary_hash are consumed today
    pub verified_at: Option<String>,
}

/// One handler inside an interface block. The `requires`/`ensures` vectors
/// are empty for Tier-0 (shape-only) interfaces. Populated for Tier-1
/// (hand-authored) and Tier-2 (imported) interfaces.
#[derive(Debug, Default, Clone)]
pub struct ParsedInterfaceHandler {
    pub name: String,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
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
pub struct ParsedSchema {
    pub name: String,
    #[allow(dead_code)]
    // parsed-but-unconsumed spec info, kept for future consumers; see spec/ast.rs note
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

/// Per-severity tallies for a lint set.
///
/// Construct via [`SeverityCounts::of`], whose match is exhaustive by
/// discipline (no `_` arm — see the `Stmt` enum doc in `mir.rs`): adding
/// a `Severity` variant is a compile error here, not a silently
/// uncounted lint. Open-coded `.filter(|w| w.severity == …)` tallies are
/// how Error-severity lints vanished from the check summary and exit
/// code for ten releases (#260); count through this instead (#270).
#[derive(Debug, Default, Clone, Copy)]
pub struct SeverityCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

impl SeverityCounts {
    pub fn of(warnings: &[CompletenessWarning]) -> Self {
        let mut counts = Self::default();
        for w in warnings {
            match w.severity {
                Severity::Error => counts.errors += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Info => counts.infos += 1,
            }
        }
        counts
    }

    /// The check exit policy: Error ≥ Warning fail; Info never does (#260).
    pub fn fails_check(&self) -> bool {
        self.errors > 0 || self.warnings > 0
    }
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

impl CompletenessWarning {
    /// Builder seed — rule / severity / priority / message; the remaining
    /// fields default (no subject/example/counterexample, empty fix, no fix
    /// options). Chain `.subject()` / `.fix()` / `.example()` /
    /// `.counterexample()` as needed (or struct-update over the base for
    /// `Option`-typed locals). Lint rules use the `warn(...)` shorthand in
    /// `lints::shared`.
    pub fn new(rule: &str, severity: Severity, priority: u8, message: impl Into<String>) -> Self {
        CompletenessWarning {
            rule: rule.to_string(),
            severity,
            priority,
            message: message.into(),
            subject: None,
            fix: String::new(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        }
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = fix.into();
        self
    }

    pub fn example(mut self, example: impl Into<String>) -> Self {
        self.example = Some(example.into());
        self
    }

    pub fn counterexample(mut self, ce: Counterexample) -> Self {
        self.counterexample = Some(ce);
        self
    }
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
        // #270: Error-severity completeness findings count as issues too —
        // the old Warning-only filter let E-class lints through the
        // `check --code/--kani` exit gate (#260's bug class).
        let counts = SeverityCounts::of(&self.completeness);
        let comp = counts.errors + counts.warnings;
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

#[cfg(test)]
mod severity_counts_tests {
    use super::*;

    fn lint(severity: Severity) -> CompletenessWarning {
        CompletenessWarning {
            rule: "test_rule".into(),
            severity,
            priority: 1,
            message: String::new(),
            subject: None,
            fix: String::new(),
            example: None,
            counterexample: None,
            fix_options: Vec::new(),
        }
    }

    #[test]
    fn tally_counts_every_severity_and_errors_fail() {
        let counts = SeverityCounts::of(&[
            lint(Severity::Error),
            lint(Severity::Warning),
            lint(Severity::Info),
            lint(Severity::Info),
        ]);
        assert_eq!(
            (counts.errors, counts.warnings, counts.infos),
            (1, 1, 2),
            "every variant must land in exactly one bucket"
        );
        assert!(counts.fails_check(), "errors must fail");
        assert!(
            !SeverityCounts::of(&[lint(Severity::Info)]).fails_check(),
            "info alone must not fail"
        );
    }

    #[test]
    fn issue_count_includes_error_severity_completeness() {
        // #270: an Error-only report must register as an issue — the old
        // Warning-only filter waved E-class lints through the unified
        // check exit gate.
        let report = UnifiedReport {
            completeness: vec![lint(Severity::Error)],
            code_drift: None,
            kani_drift: None,
            lean_coverage: Vec::new(),
        };
        assert_eq!(report.issue_count(), 1);
    }
}
