// v2.30 MIR — typed IR consumed by lean_gen_mir / kani_mir /
// codegen_mir / proptest_gen_mir. A handful of fields and enum
// variants are reserved for the v3.0 cleanup that lifts requires +
// effects into typed `Stmt` nodes (the deferred guards / proptest
// sub-emitter ports — see Phase 4g + Phase 5 deferral comments).
// Removing them now would force re-adding when that work starts;
// keeping `allow(dead_code)` here ratifies the "IR ahead of
// consumers" design.
#![allow(dead_code)]

//! qedgen MIR — typed Solana-native intermediate representation between
//! the `.qedspec` parser and the codegens.
//!
//! Phase 0 of the v2.30 refactor. See `docs/design/qedgen-mir-sketch.md` for
//! the design rationale, `docs/design/codegen-divergence.md` for the measured
//! cross-codegen divergence classes this IR closes, and
//! `docs/design/intrinsic-fixture-map.md` for the fixture evidence behind
//! the chosen `Stmt` set.
//!
//! ## What this is
//!
//! A typed IR that sits between `crate::check::ParsedSpec` and the four
//! primary codegens (`lean_gen.rs`, `codegen.rs`, `kani.rs` + `kani_impl.rs`,
//! `proptest_gen.rs`). Every codegen will eventually consume `Mir` instead
//! of pattern-matching on `ParsedSpec` directly.
//!
//! ## Key design constraints
//!
//! 1. **Structurally typed at the statement level, opaque at the expression
//!    level.** `Expr` carries pre-rendered target strings (`lean`, `rust`,
//!    `rust_pod`, `rust_binary`) — the parser already lowers expressions
//!    per target via `ParsedRequires` / `ParsedEnsures` etc. Re-modelling
//!    expressions as a typed tree would duplicate that work and reach back
//!    into `crate::ast::Node<crate::ast::Expr>`, which only `ParsedRequires`
//!    preserves. So MIR's value comes from desugaring *structure*, not
//!    expressions.
//!
//! 2. **MIR is desugared, not optimized.** Surface sugar (`+=!`, `else Err`,
//!    schema-includes, dotted-auth, `transfers {…}` blocks) lowers to
//!    explicit primitive nodes during parser→MIR. Optimizations
//!    (const fold, dead-handler elimination) are out of scope for v2.30.
//!
//! 3. **Bug-reduction is the goal**, not LoC purity. A `Stmt` kind earns
//!    inclusion by closing a divergence class from
//!    `docs/design/codegen-divergence.md`, not by reaching a codegen-count
//!    quorum. See [[feedback-mir-is-bug-reduction]] for the framing.
//!
//! 4. **qedgen-local scope.** No `runMir` Lean-side operational semantics
//!    (parked). No `applyOp ≡ runMir` equivalence lemma. The sBPF
//!    semantics + binary-proof engines come from the qedsvm package
//!    (`require qedsvm`, `SVM.SBPF.*`); the former vendored copy at
//!    `lean_solana/QEDGen/Solana/SBPF/` was deleted when qedsvm tagged
//!    v0.3.0 (solana-skills#86).
//!
//! ## Lowering source — ParsedSpec types we read from
//!
//! Phase 0a survey notes (cross-reference `crates/qedgen/src/check.rs`):
//!
//! - `ParsedSpec.handlers: Vec<ParsedHandler>` — main input.
//! - `ParsedSpec.account_types: Vec<ParsedAccountType>` — state ADT;
//!   carries `variants: Vec<ParsedVariant>` for multi-variant lifecycle.
//! - `ParsedSpec.records: Vec<ParsedRecordType>` — plain record types.
//! - `ParsedSpec.pdas: Vec<ParsedPda>` — top-level PDA declarations.
//! - `ParsedSpec.error_codes: Vec<String>` — declared error variants.
//! - `ParsedSpec.events: Vec<ParsedEvent>` — event declarations.
//! - `ParsedSpec.imported_namespaces` — v2.29 Slice F unified imports.
//!
//! Per-handler shapes consumed:
//!
//! - `ParsedHandler.who: Option<String>` + `permissionless: bool` — auth.
//! - `ParsedHandler.pre_status` / `post_status: Option<String>` — lifecycle.
//! - `ParsedHandler.requires: Vec<ParsedRequires>` — pre-conditions (with
//!   optional `error_name` for `requires X else Err`).
//! - `ParsedHandler.ensures: Vec<ParsedEnsures>` — post-conditions; carries
//!   `lean_expr`, `rust_expr`, `rust_expr_pod`, `rust_expr_binary`.
//! - `ParsedHandler.effects: Vec<(String, String, String)>` — `(field, op_kind, value)`.
//!   op_kind ∈ {"set", "add", "add_sat", "add_wrap", "sub", "sub_sat", "sub_wrap"}.
//! - `ParsedHandler.effect_on_error: Vec<Option<String>>` — v2.24 per-site error
//!   overrides parallel to `effects`.
//! - `ParsedHandler.effect_branches: Option<ParsedEffectBranches>` — issue #42
//!   conditional effects (only consumed by Lean today; MIR makes it first-class).
//! - `ParsedHandler.transfers: Vec<ParsedTransfer>` — declarative
//!   `transfers { from A to B amount X authority W }`. Lowers to
//!   `Stmt::TokenTransfer`.
//! - `ParsedHandler.calls: Vec<ParsedCall>` — explicit
//!   `call Interface.method(arg = expr, ...)`. `Token.transfer` calls lower
//!   to `Stmt::TokenTransfer`; everything else lowers to `Stmt::Cpi`.
//! - `ParsedHandler.accounts: Vec<ParsedHandlerAccount>` — per-handler
//!   account bindings (writable / signer / pda / authority / type).
//! - `ParsedHandler.emits: Vec<String>` — event emission (auxiliary).
//!
//! Predicate-carrier structs all share the same shape: each carries
//! `lean_expr: String` plus one or more Rust forms. MIR's `Expr` mirrors
//! this — see the `Expr` struct below.

use crate::check::ParsedSpec;
use std::collections::BTreeMap;

// ----------------------------------------------------------------------
// Top-level
// ----------------------------------------------------------------------

/// Root MIR object for a single `.qedspec` program.
#[derive(Debug, Clone)]
pub struct Mir {
    /// Spec name (typically `spec <Name>` line).
    pub name: Symbol,
    /// State ADT — variants and their fields. For single-variant specs,
    /// this is a single `StateVariant` with all fields and `tag = Symbol::default()`.
    ///
    /// For multi-account specs this carries the *primary* account so the
    /// single-account renderers stay byte-stable; `account_states[0]`
    /// reflects the same data. Multi-account renderers walk
    /// `account_states` instead.
    pub state: StateAdt,
    /// Per-account state lifts. Always populated; `len() == 1` for
    /// single-account specs (back-compat path), `> 1` for multi-account.
    /// Lending is the canonical multi-account fixture (Pool + Loan).
    /// v2.30 Phase 2.
    pub account_states: Vec<AccountStateMir>,
    /// Account-block surface — PDAs, owners, writability, init, authority,
    /// token-type annotations. Foundational per
    /// `docs/design/qedgen-mir-sketch.md` §"AccountTable is foundational"
    /// (339 fixture references across account-block features).
    pub accounts: AccountTable,
    /// Declared error variants (from `type Error | InvalidAmount | …` blocks).
    pub errors: ErrorEnum,
    /// Cross-program references — the sole lifted structure for
    /// everything an `import` resolves to AND every inline
    /// `interface { … }` block. v2.30 unified imports collapses the
    /// parallel `ParsedSpec.interfaces` + `ParsedSpec.imported_namespaces`
    /// surfaces into one canonical view keyed by local namespace
    /// alias. See `docs/design/mir-unified-imports.md`.
    pub imports: BTreeMap<Symbol, ImportedSpecMir>,
    /// Per-handler IR.
    pub handlers: Vec<HandlerMir>,
    /// Top-level invariants (whole-state predicates, not method-level).
    pub invariants: Vec<InvariantMir>,
    /// Declared events. Auxiliary — codegen reads them when lowering
    /// `HandlerMir.emits`; they're not body statements.
    pub events: Vec<EventDecl>,
    /// Top-level `const NAME = VALUE` declarations. Stored as
    /// `(name, raw-value-string)` — codegens render `abbrev NAME : Nat
    /// := VALUE` in Lean, `pub const NAME: u64 = VALUE;` in Rust, etc.
    pub constants: Vec<(Symbol, String)>,
    /// Uninterpreted helper functions referenced from spec bodies but
    /// declared opaquely. Each becomes a Lean `opaque <name> : T1 → T2
    /// → ... → R` declaration. Issue #8 finding #5.
    pub uninterpreted_helpers: Vec<UninterpretedHelper>,
    /// `ref_impl name (params) : T = <expr>` declarations. Reference
    /// implementations referenced from `ensures` clauses. Lower to
    /// Lean `def`s and inline at Kani-harness assertion sites
    /// (distinct from `uninterpreted_helpers`: those are axiomatic,
    /// these carry executable bodies).
    pub ref_impls: Vec<RefImpl>,
    /// Top-level `property name { ... } preserved_by [op, ...]`
    /// declarations. Each emits a Lean predicate `def` + a master
    /// preservation theorem (and per-handler sub-lemmas). Per-slot
    /// proptest forms (`PerSlotForm`) and quantifier-lint metadata
    /// stay on `ParsedSpec` for now — those are proptest-codegen
    /// concerns that don't need MIR lifting until that target ports.
    pub properties: Vec<PropertyMir>,
    /// Top-level `cover` reachability declarations. Each emits an
    /// existential theorem per trace + per `(op, when)` pair.
    pub covers: Vec<CoverMir>,
    /// Top-level `liveness` (leads-to) declarations. Each emits a
    /// bounded-reachability theorem over a lifecycle-state transition.
    pub liveness_props: Vec<LivenessMir>,
    /// Top-level `environment` blocks describing external state
    /// mutations. Each property × environment cross emits a
    /// preservation theorem.
    pub environments: Vec<EnvironmentMir>,
    /// Issue #67 item 3 — `ghost` spec-only auxiliary state. Lowered to
    /// extra verification-State fields (Lean / proptest / Kani) plus a
    /// per-handler new-value expression; the on-chain codegen ignores
    /// these entirely.
    pub ghosts: Vec<GhostMir>,
    /// Top-level `type T = { … }` record declarations (the value types of
    /// `Map[N] T` fields, e.g. percolator's `Account`). The indexed-state
    /// Lean renderer emits a `structure T` + `instance Inhabited T` per
    /// record. Carried verbatim from `ParsedSpec.records` — the indexed
    /// renderer is the only consumer today.
    pub records: Vec<crate::check::ParsedRecordType>,
    /// True for sBPF assembly specs (`pragma sbpf { … }`, detected via
    /// `ParsedSpec::is_assembly_target`). The Lean renderer dispatches on
    /// this flag to `render_sbpf` (assembly proofs over `Program.lean` +
    /// `wp_exec`) — a wholly different output shape from the state-machine
    /// renderers. The Kani/proptest backends skip assembly targets
    /// entirely (sBPF runtime behavior is exercised by client-side tests),
    /// so this flag has a single consumer: `lean_gen_mir`. The instruction
    /// / layout / guard data the renderer needs is read from `ParsedSpec`
    /// at the codegen call site (it is not lifted into MIR — with only one
    /// consumer there is no cross-codegen divergence for MIR to prevent).
    pub is_assembly: bool,
    /// State-representation opt-in (`pragma state_repr = adt`, via
    /// `ParsedSpec::state_repr_is_adt`). True → the multi-variant State
    /// lowers as a real `inductive State` (Lean) / wrapper-struct +
    /// inner-enum (Anchor Rust); false (default) → flat `structure
    /// State` + `status` discriminant. Replaces the pre-v2.33 footgun
    /// that keyed the choice on an incidental `WrongState` error
    /// variant. `lean_gen_mir::is_multi_variant_adt` reads this; the
    /// `ParsedSpec`-based codegens read `state_repr_is_adt()` directly.
    pub adt_state: bool,
}

impl Mir {
    /// Look up a handler's lowered body by name. Handler names are
    /// unique (spec validation rejects duplicates), and the per-account
    /// scoped `ParsedSpec` views the Kani/proptest emitters build only
    /// *filter* the handler set — they never rewrite handler content —
    /// so a name-keyed lookup against the unscoped `Mir` stays valid
    /// inside scoped sections.
    pub fn handler_block(&self, name: &str) -> Option<&Block> {
        self.handlers
            .iter()
            .find(|h| h.name == name)
            .map(|h| &h.body)
    }
}

// ----------------------------------------------------------------------
// State
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct StateAdt {
    /// One or more variants. Single-record specs lower to a single
    /// unnamed variant carrying all the fields.
    pub variants: Vec<StateVariant>,
    /// Lifecycle-only variant labels (no payload) for back-compat with
    /// pre-v2.24 lifecycle-only state declarations.
    pub lifecycle_states: Vec<Symbol>,
}

#[derive(Debug, Clone)]
pub struct StateVariant {
    pub tag: VariantTag,
    pub fields: Vec<FieldDecl>,
}

/// Per-account state lift. v2.30 Phase 2 multi-account scaffolding:
/// `Mir.state` collapses to the primary account for back-compat, while
/// `Mir.account_states` carries the full list so the multi-account
/// renderer can emit per-account `<Name>State` structs, per-group
/// `apply<Name>Op` dispatchers, and per-group preservation theorems.
/// Single-account specs surface as `account_states.len() == 1`.
#[derive(Debug, Clone)]
pub struct AccountStateMir {
    /// Account-type name from the `type <Name>` block (e.g., "Pool",
    /// "Loan"). Drives Lean type naming: `<Name>State`, `<Name>Status`,
    /// `<Name>Operation`, `apply<Name>Op`.
    pub name: Symbol,
    /// Variant declarations when the account is an ADT (`type Pool |
    /// Uninitialized | Active of {…}`). Empty for flat-record accounts.
    pub variants: Vec<StateVariant>,
    /// Flat record fields when the account is `type Pool { … }`. Empty
    /// for ADT accounts (fields live on each `StateVariant`).
    pub fields: Vec<FieldDecl>,
    /// Lifecycle-only variant labels (no payload).
    pub lifecycle_states: Vec<Symbol>,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: Symbol,
    pub ty: Ty,
}

// ----------------------------------------------------------------------
// AccountTable
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct AccountTable {
    /// Top-level `pda <name> [seeds]` declarations. Per-account inline PDAs
    /// (`acct : writable, pda [seeds]`) live in `AccountBindingShape.pda_ref`.
    pub pdas: BTreeMap<Symbol, PdaDeclaration>,
    /// Handler-scoped bindings are stored on each `HandlerMir.accounts`;
    /// this map holds spec-level account shapes shared across handlers
    /// when the surface DSL eventually supports them (today every
    /// account binding is handler-scoped, so this is reserved for v3.0
    /// — kept here to fix the shape).
    #[allow(dead_code)]
    pub spec_level_bindings: BTreeMap<Symbol, AccountBindingShape>,
}

#[derive(Debug, Clone)]
pub struct PdaDeclaration {
    pub name: Symbol,
    /// Seeds as pre-rendered target strings; same opaque-string discipline
    /// as `Expr`. Each seed maps to its own multi-target rendering since
    /// seeds can be literals, account refs, or param refs.
    pub seeds: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct AccountBindingShape {
    pub name: Symbol,
    pub writable: bool,
    pub is_signer: bool,
    pub init: bool,
    pub is_program: bool,
    pub kind: AccountKind,
    /// `authority <other_account>` annotation. None for accounts without
    /// declared authority (signer accounts, programs).
    pub authority: Option<AccountRef>,
    /// Refers to a `PdaDeclaration` in `Mir.accounts.pdas` when the
    /// account is PDA-derived.
    pub pda_ref: Option<Symbol>,
    /// v2.29 Slice G — when the account's type comes from an imported
    /// spec, this carries the namespace alias.
    pub imported_namespace: Option<Symbol>,
    /// v2.29 brownfield — hard-coded base58 pubkey when this account is
    /// a well-known default (system_program, the program itself, event
    /// authority, etc.). Codegen lowers to `solana_pubkey::pubkey!("…")`.
    pub default_pubkey: Option<String>,
    /// `account_type` annotation (e.g., `type token` → AccountKind::Token,
    /// or a user-declared type name → AccountKind::TypedAccount).
    pub account_type: Option<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    /// `account : signer` — must sign the transaction.
    Signer,
    /// `account : type token` — SPL Token account.
    Token,
    /// `account : type mint` — SPL Mint account.
    Mint,
    /// `account : program` — a program ID account, not data.
    Program,
    /// PDA-derived data account (`pda [seeds]`).
    Pda,
    /// `account_type` resolves to a user-declared `type T` block.
    TypedAccount,
    /// Account with no specific kind annotation. Treated as a plain
    /// data account whose schema is declared elsewhere.
    Plain,
}

// ----------------------------------------------------------------------
// Handler
// ----------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HandlerMir {
    pub name: Symbol,
    pub doc: Option<String>,
    /// Anchor instruction discriminator if the handler declared one.
    pub discriminant: Option<Vec<u8>>,
    pub params: Vec<(Symbol, Ty)>,
    /// Per-handler account bindings — the `accounts { … }` block.
    pub accounts: Vec<AccountBindingShape>,
    /// Authorization requirement (`auth <account>` or
    /// `auth <account>.<field>` dotted form, post-v2.29.1 desugaring).
    /// `None` means handler is either permissionless or had no auth
    /// requirement declared.
    pub auth: Option<AccountOrField>,
    pub permissionless: bool,
    /// Lifecycle transition (`: State.V1 -> State.V2` in handler signature).
    /// Lowered into a synthetic entry-`RequireOrAbort` by Phase 3's
    /// `lifecycle_lower` MIR→MIR pass; carried separately here to
    /// preserve the user-level intent and let some codegens emit
    /// alternative shapes if needed.
    pub transition: Option<(VariantTag, VariantTag)>,
    /// Multi-account routing — when the handler signature qualifies the
    /// pre-state with an account name (e.g. `: Loan.Empty -> Loan.Active`),
    /// `on_account` records `"Loan"`. `None` means the handler defaults
    /// to the primary account. Mirrors `ParsedHandler.on_account`.
    /// v2.30 Phase 2 multi-account codegen reads this to group handlers
    /// per account.
    pub on_account: Option<Symbol>,
    /// Pre-conditions. Schema-includes are already expanded in
    /// `chumsky_adapter.rs:3125+`; what arrives here is the flat list.
    pub pre: Vec<Predicate>,
    /// ALL `requires` predicates in original spec order (both `else
    /// <Err>` and bare), before the `split_requires` partition into
    /// `pre` (bare) + body `RequireOrAbort` (with-err). The indexed-state
    /// Lean renderer emits guard conjuncts from this so its ordering
    /// matches `lean_gen`'s single-list iteration (the split-then-
    /// concatenate path reordered an interleaved bare/with-err sequence
    /// — e.g. percolator's match-arm-abort guards). Same predicate
    /// construction as `split_requires` (`Predicate(Expr::from_requires)`).
    pub requires_in_order: Vec<Predicate>,
    /// Pre-conditions with `else <ErrorName>` markers — these lower to
    /// `Stmt::RequireOrAbort` rather than collected `pre`.
    /// Empty after Phase 3 lowering (passes synthesize them into `body`);
    /// populated during parser→MIR before the pass runs.
    pub requires_or_abort: Vec<RequireOrAbortClause>,
    /// Legacy `aborts_if <pred> Error` clauses. Parallel to
    /// `requires_or_abort` but with the predicate already in the
    /// "abort triggers when this holds" sense (not negated).
    /// Carries the predicate alongside the error for theorem emission
    /// (Lean's `theorem h_aborts_if_Err (s) (h : <pred>) : ... = none`).
    pub aborts_if: Vec<AbortClause>,
    pub body: Block,
    /// Post-conditions (`ensures`).
    pub post: Vec<Predicate>,
    /// Frame condition — fields that may be modified. None means
    /// "everything modifiable per the effects list."
    pub modifies: Option<Vec<Path>>,
    /// Event names emitted by this handler. Codegen pulls event schema
    /// from `Mir.events`.
    pub emits: Vec<Symbol>,
    /// Per-handler invariant references (names of invariants this handler
    /// must preserve).
    pub invariants: Vec<Symbol>,
    /// v2.17 — invariants this handler *establishes* at post-state without
    /// requiring at pre-state (init / one-shot handlers).
    pub establishes: Vec<Symbol>,
    /// v2.29 Slice A — `abstract <name> : <Type>` declarations. Each
    /// codegen lowers to its own existential / fuzz-input / agent-fill
    /// shape. Pair: (name, dsl-type-string).
    pub abstract_binders: Vec<(Symbol, String)>,
    /// `aborts_total` — every abort branch is exhaustive; codegen emits
    /// a ↔ theorem instead of per-abort.
    pub aborts_total: bool,
}

#[derive(Debug, Clone)]
pub struct RequireOrAbortClause {
    pub pred: Predicate,
    pub err: ErrorRef,
}

/// Legacy `aborts_if <pred> Error` clause. Functionally inverse of
/// `RequireOrAbortClause` — here the predicate IS the abort
/// condition, not its negation. Kept distinct so the emitted Lean
/// theorem hypothesis matches the source shape.
#[derive(Debug, Clone)]
pub struct AbortClause {
    pub pred: Predicate,
    pub err: ErrorRef,
}

// ----------------------------------------------------------------------
// Statements
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

/// Statement kinds. Total: 12 — 4 primary intrinsics + 7 effect/control +
/// 1 escape hatch CPI. See `docs/design/intrinsic-fixture-map.md` for
/// the fixture evidence per kind.
///
/// **Exhaustiveness discipline (#66):** matches over `Stmt` must list
/// every variant explicitly — no `_` arms. The point of the closed enum
/// is that adding a variant breaks every consumer at compile time, so
/// each backend is forced to decide how the new statement kind renders
/// (even if the decision is "nothing"). A wildcard arm silently opts the
/// site out of that safety net.
#[derive(Debug, Clone)]
pub enum Stmt {
    // ---- Primary intrinsics (fixture-evidence anchored) ----
    /// Authorization-or-abort. Canonical `requires X else Err` shape;
    /// 96 uses across 15 of 21 main fixtures. Closes divergence A3.
    RequireOrAbort {
        pred: Predicate,
        err: ErrorRef,
    },

    /// SPL Token Transfer (`call Token.transfer` or `transfers {}` block).
    /// 7 fixtures, 15 total uses. Closes divergence A2 (Kani/proptest CPI
    /// gap) and A4 (CPI ensures coordination).
    TokenTransfer {
        from: AccountRef,
        to: AccountRef,
        amount: Expr,
        /// `None` when the `transfers` block declared no `authority`
        /// clause. The CPI envelope theorem needs a 3-account shape,
        /// so authorityless transfers skip the theorem emission with
        /// an obligation comment — preserving the v2.29 behavior.
        authority: Option<AccountRef>,
    },

    /// Lifecycle promotion to a new variant, carrying payload.
    /// 1 main fixture + regression coverage. Closes A2 (Kani/proptest
    /// variant-promotion gap).
    VariantPromote {
        from_tag: VariantTag,
        to_tag: VariantTag,
        payload: Vec<(Symbol, Expr)>,
    },

    // ---- Effect-op kinds (closes B1: effect-op string-literal dispatch) ----
    /// `field := value`. Escape hatch for arbitrary effect RHS.
    Assign {
        path: Path,
        rhs: Expr,
    },

    /// `field +=` with checked overflow → ErrorRef. Default arithmetic
    /// shape post-v2.7 G3 / v2.24.
    CheckedAdd {
        path: Path,
        delta: Expr,
        err: ErrorRef,
    },
    CheckedSub {
        path: Path,
        delta: Expr,
        err: ErrorRef,
    },

    /// `field +=?` — wrapping arithmetic, no error. v2.24 explicit marker
    /// (parser op_kind `add_wrap`; see the tier table in
    /// `rust_codegen_util::emit_transition_fn_inner`).
    WrapAdd {
        path: Path,
        delta: Expr,
    },
    WrapSub {
        path: Path,
        delta: Expr,
    },

    /// `field +=!` — saturating arithmetic, no error. v2.24 explicit marker
    /// (parser op_kind `add_sat`).
    SatAdd {
        path: Path,
        delta: Expr,
    },
    SatSub {
        path: Path,
        delta: Expr,
    },

    // ---- Control flow (closes A1: ParsedEffectBranches divergence) ----
    /// Conditional effect block. Lowered from
    /// `ParsedHandler.effect_branches` (issue #42).
    Branch {
        scrutinee: BranchScrutinee,
        arms: Vec<BranchArm>,
        default: Option<Block>,
    },

    /// Terminal abort. Used as the canonical post-`Branch` exit for
    /// fail paths and standalone abort clauses.
    Abort(ErrorRef),

    // ---- Escape hatches ----
    /// Generic CPI to a non-Token interface.
    Cpi {
        /// References the alias in `Mir.imports`.
        target: InterfaceRef,
        /// Which handler within the targeted interface.
        method: MethodRef,
        args: Vec<CallArg>,
        /// v2.27 Track A — caller-supplied projections from the
        /// callee's abstract state vocabulary onto the caller's
        /// concrete State fields. Empty when the caller declared no
        /// `state_binders { ... }` block on the call site (preserves
        /// the v2.26 callee-frame, param-only axiom shape).
        state_binders: Vec<StateBinder>,
        /// `let X = call ...` binder; `None` for terminal calls.
        result_binding: Option<Symbol>,
    },

    /// Event emission — auxiliary, not a state mutation. Most codegens
    /// emit nothing or a `emit!(EventName { ... })` macro call.
    Emit {
        event: Symbol,
    },
}

#[derive(Debug, Clone)]
pub enum BranchScrutinee {
    /// Boolean test (e.g., `if pred then …`).
    Predicate(Predicate),
    /// Match on a value scrutinee — `effect_branches.scrutinee_rust`.
    /// Stored opaquely per the opaque-expression discipline; arms
    /// pattern-match on the rendered form.
    Match(Expr),
}

#[derive(Debug, Clone)]
pub struct BranchArm {
    /// For `Predicate` scrutinees, this is empty (the predicate IS the
    /// guard). For `Match` scrutinees, this is the pattern as a
    /// pre-rendered string (per the opaque-expression discipline).
    pub pattern: Option<Expr>,
    pub block: Block,
}

#[derive(Debug, Clone)]
pub struct CallArg {
    pub name: Symbol,
    pub value: Expr,
}

// ----------------------------------------------------------------------
// Predicates and expressions (opaque carriers per design)
// ----------------------------------------------------------------------

/// Opaque expression carrier. The parser already lowers expressions to
/// per-target string forms; MIR mirrors them without re-modelling.
///
/// One of the fields will be non-empty depending on the source. For
/// `ParsedRequires`-derived expressions, `lean`, `rust`, and `rust_pod`
/// are all populated. For `ParsedEnsures`-derived expressions, the
/// `rust_binary` form is additionally populated. Each codegen picks the
/// field it needs.
#[derive(Debug, Clone, Default)]
pub struct Expr {
    pub lean: String,
    pub rust: String,
    pub rust_pod: String,
    /// v2.25 — binary-mode rendering for ensures clauses
    /// (`state.x` → `post.x`, `old(state.x)` → `pre.x`). Empty for
    /// expressions sourced from pre-conditions or effect RHS where the
    /// distinction doesn't apply.
    pub rust_binary: String,
    /// Source AST retained when available (today only `ParsedRequires`
    /// preserves it). Lints and AST-level checks can read this; codegens
    /// shouldn't.
    pub source_span: Option<SourceSpan>,
}

#[derive(Debug, Clone)]
pub struct Predicate(pub Expr);

/// Source-span placeholder. Today qedgen's parsing doesn't surface
/// spans up to ParsedSpec uniformly; the v3.0 refactor will. Kept as
/// an opaque carrier so adding real spans later is non-breaking.
#[derive(Debug, Clone, Default)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

// ----------------------------------------------------------------------
// Invariants
// ----------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct InvariantMir {
    pub name: Symbol,
    pub doc: String,
    /// `None` for description-only invariants (pre-v2.14 stubs flagged
    /// by the `bare_invariant` lint).
    pub body: Option<Predicate>,
}

// ----------------------------------------------------------------------
// Types
// ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    U8,
    U16,
    U32,
    U64,
    U128,
    I64,
    I128,
    Bool,
    Pubkey,
    /// User-declared type name (a record, sum type, or imported type).
    Custom(Symbol),
    /// Bounded map keyed by `Pubkey` with value type `Custom(_)` and
    /// capacity carried verbatim as a string. Accepts both numeric
    /// literals (`Map[10] TokenAccount`) and constant-name references
    /// (`Map[MAX_MEMBERS] Pubkey`); the latter resolves via a top-
    /// level `const` declaration that the indexed-state renderer
    /// emits as `abbrev <name> : Nat := <value>`.
    Map {
        capacity: Symbol,
        value: Box<Ty>,
    },
}

// ----------------------------------------------------------------------
// Errors / events / interfaces
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ErrorEnum {
    pub variants: Vec<Symbol>,
}

#[derive(Debug, Clone)]
pub struct EventDecl {
    pub name: Symbol,
    pub fields: Vec<FieldDecl>,
}

/// Uninterpreted helper signature. Codegens lower as `opaque <name> :
/// T1 → T2 → ... → R` (Lean) or as TODO call-sites (Rust). First
/// encounter wins for the inferred signature — inconsistent uses
/// across the spec would need a richer type inference pass.
#[derive(Debug, Clone)]
pub struct UninterpretedHelper {
    pub name: Symbol,
    /// DSL-form argument types (`U64`, `Pubkey`, ...).
    pub arg_types: Vec<String>,
    /// DSL-form return type.
    pub return_type: String,
}

/// Spec-level property declaration.
///
/// Body lives in `expression` as a pre-rendered Lean expression
/// (parsing as `Prop`). `preserved_by` names the handlers that must
/// preserve the property — codegen emits per-(property × handler)
/// preservation sub-lemmas plus a master case-split theorem.
#[derive(Debug, Clone)]
pub struct PropertyMir {
    pub name: Symbol,
    /// Lean-rendered predicate body, `None` for description-only
    /// properties (no theorem to emit).
    pub expression: Option<Expr>,
    /// Handler names this property is preserved by.
    pub preserved_by: Vec<Symbol>,
}

/// Cover (reachability) declaration. Mirrors `check::ParsedCover`.
///
/// Each `cover <name> [op1, op2, ...]` line lowers to one `CoverMir`
/// with a single trace. The `name <ident> reachable when <expr>` shape
/// lowers to a `(handler, Some(expr))` entry in `reachable`. Codegen
/// emits one existential theorem per trace (nested `∃` chain
/// asserting the trace runs to completion) plus one theorem per
/// `reachable` entry.
#[derive(Debug, Clone)]
pub struct CoverMir {
    pub name: Symbol,
    /// Each inner Vec is a handler-name sequence to drive from an
    /// initial state. The outer Vec lets one cover bundle multiple
    /// trace alternatives.
    pub traces: Vec<Vec<Symbol>>,
    /// `(handler-name, optional when-predicate)` reachability claims.
    /// `when` carries a Lean-rendered Bool predicate over `s : State`.
    pub reachable: Vec<(Symbol, Option<Predicate>)>,
}

/// Liveness (leads-to) declaration. Mirrors `check::ParsedLiveness`.
///
/// Encodes `liveness <name> : <from> ~> <to> via [op, ...] within N`:
/// from the source lifecycle state, applying some sequence of `via_ops`
/// of length ≤ `within_steps` reaches the target lifecycle state.
/// Codegen emits a single theorem of the form
/// `∃ ops, ops.length ≤ N ∧ ∀ s', applyOps s signer ops = some s' → s'.status = .<to>`.
#[derive(Debug, Clone)]
pub struct LivenessMir {
    pub name: Symbol,
    /// Source lifecycle state tag (e.g., `Open`). The leading `State.`
    /// prefix from the surface DSL is stripped by the parser.
    pub from_state: Symbol,
    /// Target lifecycle state tag (e.g., `Closed`).
    pub leads_to_state: Symbol,
    /// Handlers eligible to drive the transition. Order is preserved
    /// because the lifecycle-path search walks them sequentially.
    pub via_ops: Vec<Symbol>,
    /// Step bound. `None` is treated as the legacy default of 10 at
    /// emit time.
    pub within_steps: Option<u64>,
}

/// Environment (external-state-change) declaration. Mirrors
/// `check::ParsedEnvironment`.
///
/// Encodes `environment <name> { mutates <field> : <T>; constraint <expr> }`:
/// every spec property must hold after the listed fields mutate under
/// the listed constraints. Codegen emits one preservation theorem per
/// (property × environment) pair, with `new_<field>` parameters and
/// constraint hypotheses. `mutates` carries field-name and the MIR
/// `Ty` (pre-resolved from the surface type string at lowering time);
/// `constraints` carries Lean-rendered predicates.
#[derive(Debug, Clone)]
pub struct EnvironmentMir {
    pub name: Symbol,
    pub mutates: Vec<(Symbol, Ty)>,
    pub constraints: Vec<Predicate>,
}

/// `ref_impl <name> (params) : <return_type> = <expr>` declaration.
/// Carries pre-rendered body strings per backend, same opaque-string
/// discipline as `Expr`.
#[derive(Debug, Clone)]
pub struct RefImpl {
    pub name: Symbol,
    pub doc: Option<String>,
    pub params: Vec<(Symbol, String)>,
    pub return_type: String,
    pub lean_body: String,
    pub rust_body: String,
}

/// Issue #67 item 3 — lowered `ghost` declaration. A spec-only auxiliary
/// State field with a pre-rendered initial value and a per-handler
/// new-value expression (keyed by handler name). Backends that model the
/// verification State (Lean / proptest / Kani) add `name : ty` as a State
/// field, initialise it to `init`, and — in each handler that appears in
/// `updates` — assign `name := updates[handler]` alongside the normal
/// effects. The on-chain program codegen never reads this.
#[derive(Debug, Clone)]
pub struct GhostMir {
    pub name: Symbol,
    pub doc: Option<String>,
    pub ty: Ty,
    pub init: Expr,
    /// Handler name → complete new-value expression after that handler.
    pub updates: BTreeMap<Symbol, Expr>,
}

#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub name: Symbol,
    /// Declared program ID for the callee. `None` for inline
    /// `interface { … }` blocks that omit the field; the legacy
    /// `lean_gen` lowering renders `"<unknown>"` in that case.
    pub program_id: Option<Symbol>,
    pub methods: BTreeMap<Symbol, InterfaceMethod>,
}

#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: Symbol,
    pub params: Vec<(Symbol, Ty)>,
    /// Pre-rendered callee ensures clauses — fed into per-callsite
    /// substitution by the `cpi_substitute` MIR→MIR pass.
    pub ensures: Vec<Predicate>,
    /// v2.27 Track A — typed abstract-state vocabulary declared by
    /// the optional interface-level `state { name : Type, ... }` block.
    /// Empty when the interface declares no state. Used by the CPI
    /// theorem emitter to pick the right Lean codomain in the bundled
    /// axiom signature (`State → T`).
    pub state_fields: Vec<(Symbol, Ty)>,
    /// v2.26 Track K — when the source declared
    /// `-> <ident> : <Type>`, the identifier names the return value
    /// inside the callee's ensures. Substitution rewrites this name to
    /// the caller's `let X = ...` binder; `None` falls back to the
    /// literal `"result"` for back-compat.
    pub result_binder: Option<Symbol>,
    /// v2.24 #11 declared handler return type, in source DSL form
    /// (e.g. `U64`). `None` for terminal handlers.
    pub return_type: Option<Symbol>,
}

// ----------------------------------------------------------------------
// Cross-program references — unified imports (v2.30 / Phase 1c-7)
// ----------------------------------------------------------------------
//
// One canonical lifted structure for everything an `import` resolves
// to AND every inline `interface { … }` block. See
// `docs/design/mir-unified-imports.md` for the design rationale —
// notably the collapse of the parallel `ParsedSpec.interfaces` +
// `ParsedSpec.imported_namespaces` surfaces into a single MIR view.
//
// Tier classification under unification is derivable, not declared:
//   * Tier 0 — `ImportOrigin::Inline` OR an external import with
//     every interface declaring no `ensures`. No call-site warrant.
//   * Tier 1 — external import with non-empty `ensures` AND
//     `Some(upstream)` carrying a `binary_hash` pin. Caller theorems
//     apply the bundled axiom; runtime CPI is warranted by the pin.
//   * Tier 2 — same as Tier 1 plus a bundled proof package under
//     `crates/qedgen/data/proofs/`. The lakefile `require`s pull the
//     callee's verified theorems in directly (Stance 2 in
//     [[project-stance3-qedsvm-discharge]]).

/// One imported source — both types and call contracts come from the
/// same artifact, warranted by the same `binary_hash` pin (when the
/// import is external).
#[derive(Debug, Clone)]
pub struct ImportedSpecMir {
    /// Local alias used by `call <alias>.handler(...)` and
    /// `<alias>.<Type>` references. Falls back to the bound name when
    /// no `as` clause is declared. For `ImportOrigin::Inline`, the
    /// alias IS the interface name itself (see
    /// `mir-unified-imports.md` §"Open questions" #2).
    pub alias: Symbol,
    /// Where the imported source came from — built-in stdlib key,
    /// user-supplied file path, or the `Inline` marker for inline
    /// `interface` blocks (no source, no warrant).
    pub origin: ImportOrigin,
    /// Account-type declarations exported by the imported spec.
    /// Re-emitted as Rust mirrors at `src/imported/<alias>.rs` when
    /// non-empty. Empty for Tier-0 interface-only stubs (SPL Token /
    /// System Program / Metaplex bundled stubs) and inline blocks.
    pub account_types: Vec<crate::check::ParsedAccountType>,
    /// Record types referenced by the imported account types.
    /// Re-emitted alongside `account_types` so the mirror is
    /// self-contained.
    pub records: Vec<crate::check::ParsedRecordType>,
    /// Interface (call-contract) declarations the imported spec
    /// exports. Each carries handlers + ensures + requires + the
    /// abstract state-field vocabulary (v2.27 Phase 0). For inline
    /// `interface Foo { ... }` blocks, this map has a single entry
    /// keyed by `Foo`.
    pub interfaces: BTreeMap<Symbol, InterfaceDecl>,
    /// `upstream { binary_hash = ... }` pin warranting the entire
    /// imported artifact. The pin justifies trusting both
    /// `interfaces` ensures AND `account_types` layouts — they're
    /// the same artifact, not two contracts. `None` for
    /// `ImportOrigin::Inline` (Tier 0 by construction).
    pub upstream: Option<crate::check::ParsedUpstream>,
    /// v2.27 Track B / Stance 2 — set to `Some(pkg_root)` when the
    /// imported source ships a bundled proof package whose theorems
    /// will discharge this import's per-handler ensures. `None` keeps
    /// the Stance-1 axiom path active (consumer emits its own
    /// sibling axiom module). The package root informs the lakefile
    /// `require` directive that pulls in the provider's proofs.
    pub verified_pkg_root: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub enum ImportOrigin {
    /// Built-in stdlib key resolved through the bundled qedspec at
    /// `crates/qedgen/data/interfaces/<key>.qedspec` (e.g. `"spl"`,
    /// `"system"`, `"metaplex"`).
    Builtin(Symbol),
    /// File-path import from the consumer's `qed.toml`. The path
    /// stored is the manifest dep key (the value after `from "..."`).
    File(Symbol),
    /// Inline `interface Foo { ... }` block declared in the
    /// consumer's own spec. No source, no `upstream` pin — Tier 0
    /// by construction. The interface name doubles as the namespace
    /// alias (`Mir.imports["Foo"]` and `Mir.imports["Foo"]
    /// .interfaces["Foo"]`).
    Inline,
}

/// v2.27 Track A — caller-supplied projection from a callee's
/// abstract state field onto a caller's concrete State field.
///
/// At today's surface, the RHS must be a single dotted path
/// (`state.<ident>`). The MIR type tightens that to a `Path`,
/// trading the free-string shape for structure. Richer RHS
/// expressions are reserved for a future spec evolution.
#[derive(Debug, Clone)]
pub struct StateBinder {
    /// LHS — callee abstract field name (from the imported
    /// interface's `state { ... }` block). Word-boundary substitution
    /// catches every occurrence in the callee's ensures.
    pub callee_field: Symbol,
    /// RHS — caller-side projection. Typically a single bare state
    /// field (`Path { segments: ["caller_field"] }`) lifted from
    /// `state.<ident>` at the surface; carrying the full `Path`
    /// shape leaves room for `state.X.Y` projections to land
    /// alongside [[project-stance3-qedsvm-discharge]] without
    /// reshaping the IR.
    pub caller_projection: Path,
}

// ----------------------------------------------------------------------
// Reference types
// ----------------------------------------------------------------------

/// Interned symbol — today just a String for simplicity, will become
/// a hash-interned id when the corpus grows large enough to warrant it.
pub type Symbol = String;

pub type VariantTag = Symbol;
pub type ErrorRef = Symbol;
pub type EventRef = Symbol;

#[derive(Debug, Clone)]
pub struct InterfaceRef(pub Symbol);

#[derive(Debug, Clone)]
pub struct MethodRef(pub Symbol);

#[derive(Debug, Clone)]
pub struct Path {
    /// Dotted path, e.g., `state.admin` parses as `["state", "admin"]`.
    /// First segment indicates the namespace (`state`, an account
    /// binding name, a handler param).
    pub segments: Vec<Symbol>,
}

impl Path {
    pub fn single(name: impl Into<Symbol>) -> Self {
        Path {
            segments: vec![name.into()],
        }
    }

    pub fn dotted(parts: &[&str]) -> Self {
        Path {
            segments: parts.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AccountRef {
    /// Refers to an entry in the handler's `accounts` block by name.
    ByBinding(Symbol),
    /// Refers to the handler's primary state account (used when
    /// `auth self` or implicit self-references appear).
    SelfState,
}

#[derive(Debug, Clone)]
pub enum AccountOrField {
    /// Bare `auth <account_name>`.
    Account(AccountRef),
    /// Dotted-auth v2.29.1 — `auth <account>.<field>`. Already desugared
    /// to a synthetic `requires` clause in `chumsky_adapter.rs:3144`, but
    /// the structured form is retained here so codegens that want the
    /// original shape (e.g., the lint that detects auth patterns) can
    /// reach it.
    AccountField { account: AccountRef, field: Symbol },
}

// ----------------------------------------------------------------------
// Lowering entry point — implemented in Phase 0c
// ----------------------------------------------------------------------

// ----------------------------------------------------------------------
// Expr constructors
// ----------------------------------------------------------------------

impl Expr {
    /// Build an `Expr` from a `ParsedRequires` — uses all three render
    /// forms (`lean_expr` / `rust_expr` / `rust_expr_pod`). `rust_binary`
    /// stays empty since requires runs in single-state context.
    pub fn from_requires(req: &crate::check::ParsedRequires) -> Self {
        Expr {
            lean: req.lean_expr.clone(),
            rust: req.rust_expr.clone(),
            rust_pod: req.rust_expr_pod.clone(),
            rust_binary: String::new(),
            source_span: None,
        }
    }

    /// Build an `Expr` from a `ParsedEnsures` — uses all four render
    /// forms including `rust_expr_binary` for the pre/post split.
    pub fn from_ensures(ens: &crate::check::ParsedEnsures) -> Self {
        Expr {
            lean: ens.lean_expr.clone(),
            rust: ens.rust_expr.clone(),
            rust_pod: ens.rust_expr_pod.clone(),
            rust_binary: ens.rust_expr_binary.clone(),
            source_span: None,
        }
    }

    /// Build an `Expr` from a raw single-form string (e.g., an effect
    /// value, a transfer amount, a seed). Stores in `rust` only;
    /// codegens that need other forms must render on the fly from this.
    /// This is the Phase 0 compromise — proper multi-form rendering at
    /// lowering time requires touching the parser, deferred to v3.0.
    pub fn from_raw(s: impl Into<String>) -> Self {
        let s = s.into();
        Expr {
            lean: s.clone(),
            rust: s.clone(),
            rust_pod: s.clone(),
            rust_binary: String::new(),
            source_span: None,
        }
    }
}

// ----------------------------------------------------------------------
// Lowering — ParsedSpec → MIR
// ----------------------------------------------------------------------

/// Lower a fully-parsed and checked `ParsedSpec` to MIR.
///
/// The lowering is **lossless w.r.t. semantics** but not w.r.t. source
/// syntax — surface sugar (schema-includes, `transfers` blocks,
/// dotted-auth) is desugared into the explicit `Stmt` shapes during
/// lowering. Schema-include expansion and dotted-auth desugaring have
/// already run upstream in `chumsky_adapter.rs:3125+`; lowering reads
/// the expanded form.
///
/// Pre-conditions: `parsed` must have passed `check::check_spec` so
/// post-pass expansions have run. The lowering assumes these are
/// already done and does not re-validate.
///
/// Phase 0c scope: lowers TokenTransfer, RequireOrAbort, Assign,
/// CheckedAdd / CheckedSub, WrapAdd / WrapSub, SatAdd / SatSub,
/// lifecycle gating via `HandlerMir.transition`, and the AccountTable.
/// `Stmt::Branch` (conditional effects) and `Stmt::VariantPromote`
/// recognize their parsed source but emit a structured TODO `Stmt::Abort`
/// stub today; Phase 5 fills them in.
pub fn lower(parsed: &ParsedSpec) -> Mir {
    Mir {
        name: parsed.program_name.clone(),
        state: lower_state(parsed),
        account_states: lower_account_states(parsed),
        accounts: lower_account_table(parsed),
        errors: lower_errors(parsed),
        imports: lower_imports(parsed),
        handlers: parsed.handlers.iter().map(lower_handler).collect(),
        invariants: lower_invariants(parsed),
        events: lower_events(parsed),
        constants: parsed.constants.clone(),
        uninterpreted_helpers: parsed
            .uninterpreted_helpers
            .iter()
            .map(|(name, arg_types, return_type)| UninterpretedHelper {
                name: name.clone(),
                arg_types: arg_types.clone(),
                return_type: return_type.clone(),
            })
            .collect(),
        ref_impls: parsed
            .ref_impls
            .iter()
            .map(|r| RefImpl {
                name: r.name.clone(),
                doc: r.doc.clone(),
                params: r.params.clone(),
                return_type: r.return_type.clone(),
                lean_body: r.lean_body.clone(),
                rust_body: r.rust_body.clone(),
            })
            .collect(),
        properties: parsed
            .properties
            .iter()
            .map(|p| PropertyMir {
                name: p.name.clone(),
                expression: p.expression.as_ref().map(|lean| Expr {
                    lean: lean.clone(),
                    rust: p.rust_expression.clone().unwrap_or_default(),
                    rust_pod: p.rust_expression_pod.clone().unwrap_or_default(),
                    rust_binary: String::new(),
                    source_span: None,
                }),
                preserved_by: p.preserved_by.clone(),
            })
            .collect(),
        covers: lower_covers(parsed),
        liveness_props: lower_liveness(parsed),
        environments: lower_environments(parsed),
        ghosts: lower_ghosts(parsed),
        records: parsed.records.clone(),
        is_assembly: parsed.is_assembly_target(),
        adt_state: parsed.state_repr_is_adt(),
    }
}

/// Lift every `type <Account>` declaration into a parallel
/// `AccountStateMir`. The primary account is the first entry;
/// multi-account codegen walks the full list. v2.30 Phase 2.
fn lower_account_states(parsed: &ParsedSpec) -> Vec<AccountStateMir> {
    parsed
        .account_types
        .iter()
        .map(|at| {
            let variants = at
                .variants
                .iter()
                .map(|v| StateVariant {
                    tag: v.name.clone(),
                    fields: v
                        .fields
                        .iter()
                        .map(|(n, t)| FieldDecl {
                            name: n.clone(),
                            ty: parse_ty(t),
                        })
                        .collect(),
                })
                .collect();
            let fields = at
                .fields
                .iter()
                .map(|(n, t)| FieldDecl {
                    name: n.clone(),
                    ty: parse_ty(t),
                })
                .collect();
            AccountStateMir {
                name: at.name.clone(),
                variants,
                fields,
                lifecycle_states: at.lifecycle.clone(),
            }
        })
        .collect()
}

fn lower_state(parsed: &ParsedSpec) -> StateAdt {
    // Primary account type drives the state ADT. Multi-account specs
    // surface only the primary here; per-account state lives on each
    // handler's `accounts` binding. v3.0 may revisit.
    let primary = parsed.account_types.first();

    let variants = match primary {
        Some(at) if !at.variants.is_empty() => at
            .variants
            .iter()
            .map(|v| StateVariant {
                tag: v.name.clone(),
                fields: v
                    .fields
                    .iter()
                    .map(|(n, t)| FieldDecl {
                        name: n.clone(),
                        ty: parse_ty(t),
                    })
                    .collect(),
            })
            .collect(),
        Some(at) => {
            // Single-record account type — one synthetic variant
            // carrying all fields. tag = the account-type name for
            // back-compat with codegens that key on the name.
            vec![StateVariant {
                tag: at.name.clone(),
                fields: at
                    .fields
                    .iter()
                    .map(|(n, t)| FieldDecl {
                        name: n.clone(),
                        ty: parse_ty(t),
                    })
                    .collect(),
            }]
        }
        None => vec![],
    };

    StateAdt {
        variants,
        lifecycle_states: primary.map(|at| at.lifecycle.clone()).unwrap_or_default(),
    }
}

fn lower_account_table(parsed: &ParsedSpec) -> AccountTable {
    let mut pdas = BTreeMap::new();
    for pda in &parsed.pdas {
        pdas.insert(
            pda.name.clone(),
            PdaDeclaration {
                name: pda.name.clone(),
                seeds: pda
                    .seeds
                    .iter()
                    .map(|s| Expr::from_raw(s.clone()))
                    .collect(),
            },
        );
    }
    AccountTable {
        pdas,
        spec_level_bindings: BTreeMap::new(),
    }
}

fn lower_errors(parsed: &ParsedSpec) -> ErrorEnum {
    ErrorEnum {
        variants: parsed.error_codes.clone(),
    }
}

/// Lower `ParsedSpec` import sources (both `import` resolutions in
/// `imported_namespaces` and inline `interface { ... }` blocks in
/// `interfaces`) to the unified `BTreeMap<Symbol, ImportedSpecMir>`
/// shape. Implements step 4 of
/// `docs/design/mir-unified-imports.md`.
///
/// Discrimination rule: external imports register in both
/// `parsed.imported_namespaces` (keyed by local alias) and
/// `parsed.interfaces` (one entry per alias, name post-rename).
/// Inline `interface { … }` blocks register only in
/// `parsed.interfaces`. So the algorithm:
///   1. Walk `imported_namespaces` → external import entries
///      (`ImportOrigin::Builtin` if `dep_key` resolves through
///      `import_resolver::builtin_source`, else
///      `ImportOrigin::File`).
///   2. Walk `parsed.interfaces`; any entry whose name is NOT a key
///      in `imported_namespaces` is an inline block →
///      `ImportOrigin::Inline`.
fn lower_imports(parsed: &ParsedSpec) -> BTreeMap<Symbol, ImportedSpecMir> {
    let mut imports = BTreeMap::new();

    // Step 1 — external imports.
    for (local_name, ns) in &parsed.imported_namespaces {
        let iface = parsed.interfaces.iter().find(|i| &i.name == local_name);
        let mut interfaces_map = BTreeMap::new();
        if let Some(i) = iface {
            interfaces_map.insert(i.name.clone(), lift_interface(i));
        }
        let origin = if crate::import_resolver::builtin_source(&ns.dep_key).is_some() {
            ImportOrigin::Builtin(ns.dep_key.clone())
        } else {
            ImportOrigin::File(ns.dep_key.clone())
        };
        imports.insert(
            local_name.clone(),
            ImportedSpecMir {
                alias: local_name.clone(),
                origin,
                account_types: ns.account_types.clone(),
                records: ns.records.clone(),
                interfaces: interfaces_map,
                upstream: iface.and_then(|i| i.upstream.clone()),
                verified_pkg_root: parsed.verified_callees.get(local_name).cloned(),
            },
        );
    }

    // Step 2 — inline interface blocks.
    for iface in &parsed.interfaces {
        if parsed.imported_namespaces.contains_key(&iface.name) {
            continue;
        }
        let mut interfaces_map = BTreeMap::new();
        interfaces_map.insert(iface.name.clone(), lift_interface(iface));
        imports.insert(
            iface.name.clone(),
            ImportedSpecMir {
                alias: iface.name.clone(),
                origin: ImportOrigin::Inline,
                account_types: Vec::new(),
                records: Vec::new(),
                interfaces: interfaces_map,
                upstream: None,
                verified_pkg_root: None,
            },
        );
    }

    imports
}

/// Lift a single `ParsedInterface` to the MIR `InterfaceDecl` shape.
///
/// Preserves the legacy semantics: `program_id` flows through as
/// `Option<Symbol>` (the CPI emitter renders `"<unknown>"` for the
/// `None` case to match the v2.x output exactly), and every handler's
/// `ensures` clauses become `Predicate`s carrying their pre-rendered
/// Lean / Rust forms — Phase 3's `cpi_substitute` MIR→MIR pass will
/// rewrite them per call site.
fn lift_interface(iface: &crate::check::ParsedInterface) -> InterfaceDecl {
    let mut methods = BTreeMap::new();
    for h in &iface.handlers {
        let params: Vec<(Symbol, Ty)> = h
            .params
            .iter()
            .map(|(n, t)| (n.clone(), parse_ty(t)))
            .collect();
        let ensures: Vec<Predicate> = h
            .ensures
            .iter()
            .map(|e| Predicate(Expr::from_ensures(e)))
            .collect();
        methods.insert(
            h.name.clone(),
            InterfaceMethod {
                name: h.name.clone(),
                params,
                ensures,
                state_fields: iface
                    .state_fields
                    .iter()
                    .map(|(n, t)| (n.clone(), parse_ty(t)))
                    .collect(),
                result_binder: h.result_binder.clone(),
                return_type: h.return_type.clone(),
            },
        );
    }
    InterfaceDecl {
        name: iface.name.clone(),
        program_id: iface.program_id.clone(),
        methods,
    }
}

fn lower_invariants(parsed: &ParsedSpec) -> Vec<InvariantMir> {
    parsed
        .invariants
        .iter()
        .map(|inv| InvariantMir {
            name: inv.name.clone(),
            doc: inv.doc.clone(),
            body: inv.lean_expr.as_ref().map(|lean| {
                Predicate(Expr {
                    lean: lean.clone(),
                    rust: inv.rust_expr.clone().unwrap_or_default(),
                    rust_pod: String::new(),
                    rust_binary: String::new(),
                    source_span: None,
                })
            }),
        })
        .collect()
}

fn lower_covers(parsed: &ParsedSpec) -> Vec<CoverMir> {
    parsed
        .covers
        .iter()
        .map(|c| CoverMir {
            name: c.name.clone(),
            traces: c.traces.clone(),
            reachable: c
                .reachable
                .iter()
                .map(|(op, when)| {
                    let pred = when.as_ref().map(|expr| {
                        Predicate(Expr {
                            lean: expr.clone(),
                            ..Default::default()
                        })
                    });
                    (op.clone(), pred)
                })
                .collect(),
        })
        .collect()
}

fn lower_liveness(parsed: &ParsedSpec) -> Vec<LivenessMir> {
    parsed
        .liveness_props
        .iter()
        .map(|l| LivenessMir {
            name: l.name.clone(),
            from_state: l.from_state.clone(),
            leads_to_state: l.leads_to_state.clone(),
            via_ops: l.via_ops.clone(),
            within_steps: l.within_steps,
        })
        .collect()
}

fn lower_ghosts(parsed: &ParsedSpec) -> Vec<GhostMir> {
    parsed
        .ghosts
        .iter()
        .map(|g| GhostMir {
            name: g.name.clone(),
            doc: g.doc.clone(),
            ty: parse_ty(&g.ty),
            init: Expr {
                lean: g.init_lean.clone(),
                rust: g.init_rust.clone(),
                ..Default::default()
            },
            updates: g
                .updates
                .iter()
                .map(|u| {
                    (
                        u.handler.clone(),
                        Expr {
                            lean: u.value_lean.clone(),
                            rust: u.value_rust.clone(),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

fn lower_environments(parsed: &ParsedSpec) -> Vec<EnvironmentMir> {
    parsed
        .environments
        .iter()
        .map(|env| EnvironmentMir {
            name: env.name.clone(),
            mutates: env
                .mutates
                .iter()
                .map(|(name, ty)| (name.clone(), parse_ty(ty)))
                .collect(),
            constraints: env
                .constraints
                .iter()
                .enumerate()
                .map(|(i, lean)| {
                    Predicate(Expr {
                        lean: lean.clone(),
                        rust: env.constraints_rust.get(i).cloned().unwrap_or_default(),
                        ..Default::default()
                    })
                })
                .collect(),
        })
        .collect()
}

fn lower_events(parsed: &ParsedSpec) -> Vec<EventDecl> {
    parsed
        .events
        .iter()
        .map(|ev| EventDecl {
            name: ev.name.clone(),
            fields: ev
                .fields
                .iter()
                .map(|(n, t)| FieldDecl {
                    name: n.clone(),
                    ty: parse_ty(t),
                })
                .collect(),
        })
        .collect()
}

fn lower_handler(h: &crate::check::ParsedHandler) -> HandlerMir {
    let transition = match (&h.pre_status, &h.post_status) {
        (Some(pre), Some(post)) => Some((pre.clone(), post.clone())),
        _ => None,
    };

    let (pre, requires_or_abort) = split_requires(&h.requires);
    // Original spec order of all requires (for the indexed renderer's
    // conjunct ordering — see `HandlerMir.requires_in_order`). Identical
    // predicate construction to `split_requires`.
    let requires_in_order: Vec<Predicate> = h
        .requires
        .iter()
        .map(|r| Predicate(Expr::from_requires(r)))
        .collect();
    let aborts_if: Vec<AbortClause> = h
        .aborts_if
        .iter()
        .map(|a| AbortClause {
            pred: Predicate(Expr {
                lean: a.lean_expr.clone(),
                rust: a.rust_expr.clone(),
                rust_pod: a.rust_expr_pod.clone(),
                rust_binary: String::new(),
                source_span: None,
            }),
            err: a.error_name.clone(),
        })
        .collect();

    HandlerMir {
        name: h.name.clone(),
        doc: h.doc.clone(),
        discriminant: None, // Anchor IDL extractor populates this elsewhere; Phase 0 stub
        params: h
            .takes_params
            .iter()
            .map(|(n, t)| (n.clone(), parse_ty(t)))
            .collect(),
        accounts: h.accounts.iter().map(lower_account_binding).collect(),
        auth: lower_auth(h),
        permissionless: h.permissionless,
        transition,
        on_account: h.on_account.clone(),
        pre,
        requires_in_order,
        requires_or_abort,
        aborts_if,
        body: lower_body(h),
        post: h
            .ensures
            .iter()
            .map(|e| Predicate(Expr::from_ensures(e)))
            .collect(),
        modifies: h
            .modifies
            .as_ref()
            .map(|m| m.iter().map(|s| Path::single(s.clone())).collect()),
        emits: h.emits.clone(),
        invariants: h.invariants.clone(),
        establishes: h.establishes.clone(),
        abstract_binders: h.abstract_binders.clone(),
        aborts_total: h.aborts_total,
    }
}

/// Split `ParsedRequires` into (pure pre-conditions, requires-or-abort).
/// Clauses with `error_name = Some(...)` go to the requires-or-abort
/// list (lowered to `Stmt::RequireOrAbort` in the body); clauses
/// without go to `pre` (silent pre-conditions used in theorem
/// hypotheses but not enforced via abort).
fn split_requires(
    requires: &[crate::check::ParsedRequires],
) -> (Vec<Predicate>, Vec<RequireOrAbortClause>) {
    let mut pre = Vec::new();
    let mut roa = Vec::new();
    for r in requires {
        let expr = Expr::from_requires(r);
        match &r.error_name {
            Some(err) => roa.push(RequireOrAbortClause {
                pred: Predicate(expr),
                err: err.clone(),
            }),
            None => pre.push(Predicate(expr)),
        }
    }
    (pre, roa)
}

fn lower_auth(h: &crate::check::ParsedHandler) -> Option<AccountOrField> {
    h.who.as_ref().map(|who| {
        // Dotted form was desugared in chumsky_adapter.rs:3144 — by the
        // time we see it, `who` is the bare signer-account name (the
        // dotted clause was synthesized into requires). But keep the
        // structured form here so future passes can recover the original
        // shape from a paired ParsedRequires lookup if needed.
        AccountOrField::Account(AccountRef::ByBinding(who.clone()))
    })
}

fn lower_account_binding(a: &crate::check::ParsedHandlerAccount) -> AccountBindingShape {
    let kind = match a.account_type.as_deref() {
        Some("token") => AccountKind::Token,
        Some("mint") => AccountKind::Mint,
        _ if a.is_program => AccountKind::Program,
        _ if a.is_signer => AccountKind::Signer,
        _ if a.pda_seeds.is_some() => AccountKind::Pda,
        Some(_other) => AccountKind::TypedAccount,
        None => AccountKind::Plain,
    };

    AccountBindingShape {
        name: a.name.clone(),
        writable: a.is_writable,
        is_signer: a.is_signer,
        init: false, // ParsedHandlerAccount doesn't carry `init` today;
        // Anchor's #[account(init)] comes from account_attr —
        // pre-v3.0 lives in a separate parser surface. v3.0
        // unifies.
        is_program: a.is_program,
        kind,
        authority: a
            .authority
            .as_ref()
            .map(|name| AccountRef::ByBinding(name.clone())),
        pda_ref: None, // inline `pda [seeds]` is captured on
        // `ParsedHandlerAccount.pda_seeds`; top-level
        // pdas live in AccountTable. v3.0 unifies.
        imported_namespace: a.imported_namespace.clone(),
        default_pubkey: a.default_pubkey.clone(),
        account_type: a.account_type.clone(),
    }
}

fn lower_body(h: &crate::check::ParsedHandler) -> Block {
    let mut stmts = Vec::new();

    // 1. RequireOrAbort clauses from `requires X else Err`.
    for r in &h.requires {
        if let Some(err) = &r.error_name {
            stmts.push(Stmt::RequireOrAbort {
                pred: Predicate(Expr::from_requires(r)),
                err: err.clone(),
            });
        }
    }

    // 2. Aborts-if (legacy form, still appears in some specs).
    for ab in &h.aborts_if {
        stmts.push(Stmt::Abort(ab.error_name.clone()));
    }

    // 3. Effects → typed Stmt kinds per op_kind. Suppressed when the
    //    handler declared `effect { match … }` — `h.effects` then
    //    carries the *union* of every arm's effects (parser back-compat
    //    view) and the real statements live on the `Stmt::Branch`
    //    lowered in step 7; lowering both would double-emit.
    if h.effect_branches.is_none() {
        stmts.extend(lower_effects(&h.effects, &h.effect_on_error));
    }

    // 4. Transfers — desugar each into a TokenTransfer Stmt.
    for tr in &h.transfers {
        stmts.push(Stmt::TokenTransfer {
            from: AccountRef::ByBinding(tr.from.clone()),
            to: AccountRef::ByBinding(tr.to.clone()),
            amount: tr
                .amount
                .as_ref()
                .map(|a| Expr::from_raw(a.clone()))
                .unwrap_or_default(),
            authority: tr
                .authority
                .as_ref()
                .map(|a| AccountRef::ByBinding(a.clone())),
        });
    }

    // 5. Explicit CPI calls — all lower to `Stmt::Cpi`. The legacy
    //    `lean_gen::render_cpi_theorems` deliberately routes
    //    `call Token.transfer(...)` through the call-site ensures-as-
    //    axiom half (and reserves the transfer-envelope half for
    //    `transfers { ... }` blocks). Collapsing them at lowering
    //    time would erase that intent.
    for call in &h.calls {
        let stmt = {
            Stmt::Cpi {
                target: InterfaceRef(call.target_interface.clone()),
                method: MethodRef(call.target_handler.clone()),
                args: call
                    .args
                    .iter()
                    .map(|a| CallArg {
                        name: a.name.clone(),
                        value: Expr {
                            lean: a.lean_expr.clone(),
                            rust: a.rust_expr.clone(),
                            rust_pod: a.rust_expr_pod.clone(),
                            rust_binary: String::new(),
                            source_span: None,
                        },
                    })
                    .collect(),
                state_binders: call
                    .state_binders
                    .iter()
                    .map(|b| StateBinder {
                        callee_field: b.callee_field.clone(),
                        caller_projection: Path::single(b.caller_field.clone()),
                    })
                    .collect(),
                result_binding: call.result_binding.clone(),
            }
        };
        stmts.push(stmt);
    }

    // 6. Event emissions.
    for ev in &h.emits {
        stmts.push(Stmt::Emit { event: ev.clone() });
    }

    // 7. ParsedEffectBranches → `Stmt::Branch` (Phase 5, issue #42).
    //    Non-wildcard arms lower in declaration order; the wildcard arm
    //    becomes `default`. The flat union in `h.effects` was suppressed
    //    in step 3 — the Branch is the sole carrier of these effects.
    if let Some(br) = &h.effect_branches {
        let mut arms = Vec::new();
        let mut default = None;
        for arm in &br.arms {
            let block = Block {
                stmts: lower_effects(&arm.effects, &arm.effect_on_error),
            };
            if arm.is_wildcard {
                default = Some(block);
            } else {
                arms.push(BranchArm {
                    pattern: Some(Expr {
                        lean: arm.pattern_lean.clone(),
                        rust: arm.pattern_rust.clone(),
                        rust_pod: arm.pattern_rust.clone(),
                        rust_binary: String::new(),
                        source_span: None,
                    }),
                    block,
                });
            }
        }
        stmts.push(Stmt::Branch {
            scrutinee: BranchScrutinee::Match(Expr {
                lean: br.scrutinee_lean.clone(),
                rust: br.scrutinee_rust.clone(),
                rust_pod: br.scrutinee_rust_pod.clone(),
                rust_binary: String::new(),
                source_span: None,
            }),
            arms,
            default,
        });
    }

    Block { stmts }
}

/// Lower a parsed effect-triple list into typed `Stmt`s.
/// `on_error[i]` (when present) supplies the per-site error name for
/// checked variants. Shared by the flat-effects path and the per-arm
/// `Stmt::Branch` lowering.
fn lower_effects(effects: &[(String, String, String)], on_error: &[Option<String>]) -> Vec<Stmt> {
    let mut stmts = Vec::new();
    for (i, (field, op_kind, value)) in effects.iter().enumerate() {
        let err_override = on_error.get(i).and_then(|o| o.clone());
        let path = parse_field_path(field);
        let rhs = Expr::from_raw(value.clone());
        let stmt = match op_kind.as_str() {
            "set" => Stmt::Assign { path, rhs },
            "add" => Stmt::CheckedAdd {
                path,
                delta: rhs,
                err: err_override.unwrap_or_else(|| "Overflow".to_string()),
            },
            "sub" => Stmt::CheckedSub {
                path,
                delta: rhs,
                err: err_override.unwrap_or_else(|| "Underflow".to_string()),
            },
            "add_wrap" => Stmt::WrapAdd { path, delta: rhs },
            "sub_wrap" => Stmt::WrapSub { path, delta: rhs },
            "add_sat" => Stmt::SatAdd { path, delta: rhs },
            "sub_sat" => Stmt::SatSub { path, delta: rhs },
            other => {
                // Unknown op_kind — synthesize an Assign with a structured
                // comment marker so codegens can surface it as a bug.
                Stmt::Assign {
                    path,
                    rhs: Expr::from_raw(format!(
                        "/* MIR-TODO: unknown op_kind `{other}` */ {value}"
                    )),
                }
            }
        };
        stmts.push(stmt);
    }
    stmts
}

/// Parse a dotted field path like `state.admin` or `accounts.escrow_ta.amount`
/// into a `Path`. For Phase 0, just splits on `.`.
fn parse_field_path(s: &str) -> Path {
    Path {
        segments: s.split('.').map(|seg| seg.to_string()).collect(),
    }
}

/// Parse a DSL type string into a `Ty`. Best-effort — unknown forms
/// become `Ty::Custom(name)`. v3.0 will type-check this rigorously.
fn parse_ty(s: &str) -> Ty {
    match s.trim() {
        "U8" => Ty::U8,
        "U16" => Ty::U16,
        "U32" => Ty::U32,
        "U64" => Ty::U64,
        "U128" => Ty::U128,
        "I64" => Ty::I64,
        "I128" => Ty::I128,
        "Bool" => Ty::Bool,
        "Pubkey" => Ty::Pubkey,
        other => {
            // `Map[N] T` matcher. Accepts either a numeric literal
            // (`Map[10] TokenAccount`) or a constant-name reference
            // (`Map[MAX_MEMBERS] Pubkey`). The capacity passes through
            // as a string; the indexed-state renderer resolves
            // identifier capacities via the spec's `const` table.
            if let Some(rest) = other.strip_prefix("Map[") {
                if let Some(close) = rest.find(']') {
                    let cap_str = rest[..close].trim().to_string();
                    let inner = rest[close + 1..].trim();
                    if !cap_str.is_empty() {
                        return Ty::Map {
                            capacity: cap_str,
                            value: Box::new(parse_ty(inner)),
                        };
                    }
                }
            }
            Ty::Custom(other.to_string())
        }
    }
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as FsPath;

    /// Phase-5 #42 — `effect { match … }` lowers to a real `Stmt::Branch`:
    /// non-wildcard arms in order, wildcard arm as `default`, NO flat
    /// union effects (they would double-emit), no Phase-5 stub `Abort`.
    #[test]
    fn effect_branches_lower_to_branch_stmt() {
        let src = r#"spec CondFee
program_id "11111111111111111111111111111111"
type State
  | Active of { a : U64, b : U64, d : U64 }
type Error
  | InvalidAmount
handler route (fee_type : U8) (amount : U64) : State.Active -> State.Active {
  permissionless
  requires amount > 0 else InvalidAmount
  effect {
    match fee_type {
      0 => a +=! amount,
      1 => b += amount,
      _ => d := 0,
    }
  }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = lower(&spec);
        let body = mir.handler_block("route").expect("route body");

        let branches: Vec<&Stmt> = body
            .stmts
            .iter()
            .filter(|s| matches!(s, Stmt::Branch { .. }))
            .collect();
        assert_eq!(branches.len(), 1, "exactly one Branch stmt: {body:?}");
        let Stmt::Branch {
            scrutinee,
            arms,
            default,
        } = branches[0]
        else {
            unreachable!()
        };
        let BranchScrutinee::Match(scrut) = scrutinee else {
            panic!("expected Match scrutinee");
        };
        assert_eq!(scrut.rust, "fee_type");
        assert_eq!(arms.len(), 2, "two non-wildcard arms");
        assert!(
            matches!(arms[0].block.stmts.as_slice(), [Stmt::SatAdd { .. }]),
            "arm 0 should be SatAdd: {:?}",
            arms[0].block.stmts
        );
        assert!(
            matches!(arms[1].block.stmts.as_slice(), [Stmt::CheckedAdd { .. }]),
            "arm 1 should be CheckedAdd: {:?}",
            arms[1].block.stmts
        );
        let default = default.as_ref().expect("wildcard arm becomes default");
        assert!(
            matches!(default.stmts.as_slice(), [Stmt::Assign { .. }]),
            "default should be Assign: {:?}",
            default.stmts
        );

        // No flat union effects and no stub Abort alongside the Branch.
        assert!(
            !body.stmts.iter().any(|s| matches!(
                s,
                Stmt::Assign { .. }
                    | Stmt::CheckedAdd { .. }
                    | Stmt::CheckedSub { .. }
                    | Stmt::WrapAdd { .. }
                    | Stmt::WrapSub { .. }
                    | Stmt::SatAdd { .. }
                    | Stmt::SatSub { .. }
                    | Stmt::Abort(_)
            )),
            "flat union effects / stub Abort must be suppressed: {body:?}"
        );
    }

    // ---- Phase 0b: type-composition smoke tests ----

    #[test]
    fn mir_types_construct() {
        let mir = Mir {
            name: "Test".to_string(),
            state: StateAdt::default(),
            account_states: vec![],
            accounts: AccountTable::default(),
            errors: ErrorEnum::default(),
            imports: BTreeMap::new(),
            handlers: vec![],
            invariants: vec![],
            events: vec![],
            constants: vec![],
            uninterpreted_helpers: vec![],
            ref_impls: vec![],
            properties: vec![],
            covers: vec![],
            liveness_props: vec![],
            environments: vec![],
            ghosts: vec![],
            records: vec![],
            is_assembly: false,
            adt_state: false,
        };
        assert_eq!(mir.name, "Test");
        assert!(mir.handlers.is_empty());
    }

    #[test]
    fn path_helpers() {
        let p = Path::single("state");
        assert_eq!(p.segments, vec!["state"]);

        let p2 = Path::dotted(&["state", "admin"]);
        assert_eq!(p2.segments, vec!["state", "admin"]);
    }

    #[test]
    fn stmt_variants_compose() {
        let _ = Stmt::RequireOrAbort {
            pred: Predicate(Expr::default()),
            err: "Unauthorized".to_string(),
        };
        let _ = Stmt::TokenTransfer {
            from: AccountRef::ByBinding("src".to_string()),
            to: AccountRef::ByBinding("dst".to_string()),
            amount: Expr::default(),
            authority: Some(AccountRef::ByBinding("auth".to_string())),
        };
        let _ = Stmt::Assign {
            path: Path::single("counter"),
            rhs: Expr::default(),
        };
        let _ = Stmt::CheckedAdd {
            path: Path::single("balance"),
            delta: Expr::default(),
            err: "Overflow".to_string(),
        };
        let _ = Stmt::Abort("InvalidState".to_string());
        let _ = Stmt::Branch {
            scrutinee: BranchScrutinee::Predicate(Predicate(Expr::default())),
            arms: vec![],
            default: None,
        };
    }

    #[test]
    fn parse_ty_known_forms() {
        assert_eq!(parse_ty("U64"), Ty::U64);
        assert_eq!(parse_ty("Pubkey"), Ty::Pubkey);
        assert_eq!(parse_ty(" Bool "), Ty::Bool);
        // Custom and Map forms parse to expected variants.
        assert!(matches!(parse_ty("Snapshot"), Ty::Custom(s) if s == "Snapshot"));
        let m = parse_ty("Map[10] TokenAccount");
        match m {
            Ty::Map { capacity, value } => {
                assert_eq!(capacity, "10");
                assert!(matches!(*value, Ty::Custom(s) if s == "TokenAccount"));
            }
            other => panic!("expected Map, got {:?}", other),
        }
        // Constant-name capacity also lifts to Ty::Map (the indexed-
        // state renderer resolves the name via the spec's const table).
        let m = parse_ty("Map[MAX_MEMBERS] Pubkey");
        match m {
            Ty::Map { capacity, value } => {
                assert_eq!(capacity, "MAX_MEMBERS");
                assert!(matches!(*value, Ty::Pubkey));
            }
            other => panic!("expected Map, got {:?}", other),
        }
    }

    // ---- Phase 0d: fixture-based lowering tests ----
    //
    // Each test parses a real .qedspec from `examples/` and lowers it to
    // MIR, asserting structural properties. Pass = lowering succeeds
    // without panic and key features survive the round-trip.
    //
    // These tests use `parse_spec_file` which exercises the full
    // chumsky parser + chumsky_adapter post-pass pipeline. Schema-include
    // expansion and dotted-auth desugaring run before lowering sees the
    // ParsedSpec, matching production usage.

    fn lower_fixture(rel_path: &str) -> Mir {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let workspace_root = FsPath::new(&manifest_dir)
            .ancestors()
            .nth(2)
            .expect("workspace root above crates/qedgen");
        let spec_path = workspace_root.join(rel_path);
        assert!(
            spec_path.exists(),
            "fixture not found: {}",
            spec_path.display()
        );
        // `parse_spec_file` handles both single-file and multi-file
        // (directory) specs — escrow-split distributes its handlers
        // across `handlers/*.qedspec` and needs the directory as input.
        let parsed = crate::check::parse_spec_file(&spec_path)
            .unwrap_or_else(|e| panic!("parse {}: {e}", spec_path.display()));
        lower(&parsed)
    }

    #[test]
    fn lower_escrow_pilot() {
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");

        // Three handlers: initialize, exchange, cancel.
        assert_eq!(mir.handlers.len(), 3, "expected 3 handlers");
        let names: Vec<&str> = mir.handlers.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"initialize"));
        assert!(names.contains(&"exchange"));
        assert!(names.contains(&"cancel"));

        // All three handlers carry lifecycle transitions.
        for h in &mir.handlers {
            assert!(
                h.transition.is_some(),
                "handler {} should have a transition",
                h.name
            );
        }

        // Each non-init handler emits ≥1 TokenTransfer in its body.
        for h in &mir.handlers {
            if h.name == "initialize" {
                // initialize has 1 transfer (deposit)
                let xfers = h
                    .body
                    .stmts
                    .iter()
                    .filter(|s| matches!(s, Stmt::TokenTransfer { .. }))
                    .count();
                assert_eq!(xfers, 1, "initialize should have 1 TokenTransfer");
            } else if h.name == "exchange" {
                let xfers = h
                    .body
                    .stmts
                    .iter()
                    .filter(|s| matches!(s, Stmt::TokenTransfer { .. }))
                    .count();
                assert_eq!(xfers, 2, "exchange should have 2 TokenTransfers");
            } else if h.name == "cancel" {
                let xfers = h
                    .body
                    .stmts
                    .iter()
                    .filter(|s| matches!(s, Stmt::TokenTransfer { .. }))
                    .count();
                assert_eq!(xfers, 1, "cancel should have 1 TokenTransfer");
            }
        }

        // exchange and cancel both have `requires X else Unauthorized`
        // → RequireOrAbort in body.
        for name in ["exchange", "cancel"] {
            let h = mir.handlers.iter().find(|h| h.name == name).unwrap();
            let roa_count = h
                .body
                .stmts
                .iter()
                .filter(|s| matches!(s, Stmt::RequireOrAbort { .. }))
                .count();
            assert!(
                roa_count >= 1,
                "{} should have ≥1 RequireOrAbort, found {}",
                name,
                roa_count
            );
        }

        // initialize has `requires deposit_amount > 0 and receive_amount > 0 else InvalidAmount`.
        let init = mir
            .handlers
            .iter()
            .find(|h| h.name == "initialize")
            .unwrap();
        let roa = init
            .body
            .stmts
            .iter()
            .filter(|s| matches!(s, Stmt::RequireOrAbort { .. }))
            .count();
        assert!(roa >= 1, "initialize should have ≥1 RequireOrAbort");

        // Three error variants declared.
        assert!(mir.errors.variants.contains(&"InvalidAmount".to_string()));
        assert!(mir.errors.variants.contains(&"Unauthorized".to_string()));
        assert!(mir.errors.variants.contains(&"AlreadyClosed".to_string()));

        // State is a multi-variant ADT (Uninitialized | Open | Closed).
        assert!(mir.state.variants.len() >= 2);

        // PDA `escrow` declared.
        assert!(
            mir.accounts.pdas.contains_key("escrow"),
            "PDA 'escrow' should be in AccountTable.pdas, found: {:?}",
            mir.accounts.pdas.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn lower_lending_pilot() {
        let mir = lower_fixture("examples/rust/lending/lending.qedspec");
        assert!(!mir.handlers.is_empty(), "lending has handlers");
        // Lending uses TokenTransfer for deposit/withdraw flows.
        let total_xfers: usize = mir
            .handlers
            .iter()
            .map(|h| {
                h.body
                    .stmts
                    .iter()
                    .filter(|s| matches!(s, Stmt::TokenTransfer { .. }))
                    .count()
            })
            .sum();
        assert!(total_xfers > 0, "lending should lower TokenTransfers");

        // Every handler should have well-formed account bindings.
        for h in &mir.handlers {
            assert!(
                !h.accounts.is_empty(),
                "handler {} should have account bindings",
                h.name
            );
        }
    }

    #[test]
    fn lower_multisig_pilot() {
        let mir = lower_fixture("examples/rust/multisig/multisig.qedspec");
        assert!(!mir.handlers.is_empty());

        // Multisig has RequireOrAbort dominance — most clauses carry
        // `else Err`.
        let total_roa: usize = mir
            .handlers
            .iter()
            .map(|h| {
                h.body
                    .stmts
                    .iter()
                    .filter(|s| matches!(s, Stmt::RequireOrAbort { .. }))
                    .count()
            })
            .sum();
        assert!(
            total_roa > 0,
            "multisig should lower RequireOrAbort clauses"
        );
    }

    #[test]
    fn lower_bundled_stdlib_demo() {
        let mir = lower_fixture("examples/rust/bundled-stdlib-demo/pool.qedspec");
        assert!(!mir.handlers.is_empty());
    }

    #[test]
    fn lower_effects_to_typed_stmt() {
        // Walk every handler in every pilot fixture and confirm each
        // effect op_kind lowers to its typed Stmt kind (not the
        // unknown-op fallback marker).
        for fixture in &[
            "examples/rust/escrow/escrow.qedspec",
            "examples/rust/lending/lending.qedspec",
            "examples/rust/multisig/multisig.qedspec",
            "examples/rust/bundled-stdlib-demo/pool.qedspec",
        ] {
            let mir = lower_fixture(fixture);
            for h in &mir.handlers {
                for s in &h.body.stmts {
                    if let Stmt::Assign { rhs, .. } = s {
                        // Unknown op_kind path tags the RHS with a TODO comment.
                        // If any Assign carries that, our op-kind switch missed something.
                        assert!(
                            !rhs.rust.starts_with("/* MIR-TODO: unknown op_kind"),
                            "fixture {} has unknown-op_kind effect: {}",
                            fixture,
                            rhs.rust
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lower_preserves_handler_count_across_pilot() {
        // Smoke: every pilot fixture parses + lowers without panic and
        // yields ≥1 handler. Catches future parser regressions or
        // lowering-side panics.
        for fixture in &[
            "examples/rust/escrow/escrow.qedspec",
            "examples/rust/escrow-split",
            "examples/rust/lending/lending.qedspec",
            "examples/rust/multisig/multisig.qedspec",
            "examples/rust/bundled-stdlib-demo/pool.qedspec",
        ] {
            let mir = lower_fixture(fixture);
            assert!(
                !mir.handlers.is_empty(),
                "{} should have ≥1 handler in MIR",
                fixture
            );
        }
    }

    // ---- Phase 1c-7 (unified imports) — lower_imports tests ----

    #[test]
    fn lower_imports_builtin_spl_token() {
        // bundled-stdlib-demo/pool.qedspec is the only pilot fixture
        // with an explicit `import Token from "spl"` line. The lifted
        // import should land as `Mir.imports["Token"]` tagged
        // `ImportOrigin::Builtin("spl")` with the SPL Token interface
        // (carrying `transfer`, `mint_to`, etc.) lifted into
        // `.interfaces["Token"]`. v2.30 unified-imports step 0
        // guarantees the entry exists even though the bundled stub
        // declares no `type`s.
        let mir = lower_fixture("examples/rust/bundled-stdlib-demo/pool.qedspec");
        let token = mir
            .imports
            .get("Token")
            .expect("bundled-stdlib-demo should import Token namespace");
        assert_eq!(token.alias, "Token");
        match &token.origin {
            ImportOrigin::Builtin(k) => assert_eq!(k, "spl"),
            other => panic!("expected ImportOrigin::Builtin(\"spl\"), got {:?}", other),
        }
        // Tier-0 shape for SPL Token (interface stub, no `type` decls).
        assert!(token.account_types.is_empty());
        // Interface lifted under same local name.
        assert!(token.interfaces.contains_key("Token"));
        let token_iface = &token.interfaces["Token"];
        assert!(
            token_iface.methods.contains_key("transfer"),
            "SPL Token bundled stub should declare transfer; got methods: {:?}",
            token_iface.methods.keys().collect::<Vec<_>>()
        );
        // SPL Token ships an `upstream { binary_hash = ... }` pin in
        // the bundled stub.
        assert!(
            token.upstream.is_some(),
            "SPL Token bundled stub should carry an upstream pin"
        );
    }

    #[test]
    fn lower_imports_inline_interface() {
        // issue-8 pool.qedspec declares an inline `interface MockEncrypt
        // { ... }` block — it should lower as
        // `Mir.imports["MockEncrypt"]` tagged `ImportOrigin::Inline`
        // with no upstream pin (Tier 0 by construction).
        let mir = lower_fixture("crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec");
        let mock = mir
            .imports
            .get("MockEncrypt")
            .expect("issue-8 pool should declare MockEncrypt namespace");
        assert!(matches!(mock.origin, ImportOrigin::Inline));
        assert!(mock.upstream.is_none(), "inline blocks have no upstream");
        assert!(
            mock.account_types.is_empty(),
            "inline blocks declare no types"
        );
        // The interface name doubles as the namespace alias.
        assert_eq!(mock.alias, "MockEncrypt");
        assert!(mock.interfaces.contains_key("MockEncrypt"));
    }

    #[test]
    fn lower_imports_no_imports_is_empty() {
        // Specs that initiate CPIs only through the `transfers { }`
        // sugar (no explicit `import` line, no inline `interface`
        // block) should produce an empty `Mir.imports`. The escrow
        // pilot is the canonical case — three `transfers` blocks but
        // no top-level import.
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
        assert!(
            mir.imports.is_empty(),
            "escrow should have no lifted imports; got: {:?}",
            mir.imports.keys().collect::<Vec<_>>()
        );
    }
}
