// Some fields / variants are reserved for the v3.0 cleanup that lifts
// requires + effects into typed `Stmt` nodes; `allow(dead_code)` ratifies
// the "IR ahead of consumers" design.
#![allow(dead_code)]

//! qedgen MIR — typed Solana-native IR between the `.qedspec` parser
//! (`crate::check::ParsedSpec`) and the four codegens (lean_gen_mir,
//! codegen_mir, kani_mir + kani_impl, proptest_gen_mir). `lower(parsed)`
//! is the canonical entry. Design rationale: `docs/design/qedgen-mir-sketch.md`,
//! divergence classes: `docs/design/codegen-divergence.md`, fixture evidence:
//! `docs/design/intrinsic-fixture-map.md`.
//!
//! Design constraints:
//! 1. **Typed statements, opaque expressions.** `Expr` carries pre-rendered
//!    target strings (`lean`, `rust`, `rust_pod`, `rust_binary`) — the parser
//!    already lowers expressions per target; MIR's value is desugaring
//!    *structure*, not re-modelling expressions.
//! 2. **Desugared, not optimized.** Surface sugar (`+=!`, `else Err`,
//!    schema-includes, dotted-auth, `transfers {…}`) lowers to explicit
//!    primitive nodes; no const fold / dead-handler elimination.
//! 3. **Bug-reduction is the goal**, not LoC purity — a `Stmt` kind earns
//!    inclusion by closing a divergence class, not a codegen-count quorum.
//! 4. **qedgen-local scope.** No `runMir` Lean operational semantics; sBPF
//!    semantics + binary-proof engines come from the qedsvm package
//!    (`require qedsvm`, `SVM.SBPF.*`).
//!
//! Lowering reads `ParsedSpec` / `ParsedHandler` shapes (see check.rs).
//! Effects arrive as self-contained `ParsedEffect`s (#151 Slice 4 replaced
//! the parallel triple arrays) with op ∈ {"set", "add", "add_sat",
//! "add_wrap", "sub", "sub_sat", "sub_wrap"}; `transfers {…}` blocks lower
//! to `Stmt::TokenTransfer`, explicit `call`s to `Stmt::Cpi`.

use crate::check::ParsedSpec;
use std::collections::BTreeMap;

/// Typed expression tree (#151 Slice 0) — structural carrier that will
/// replace the pre-rendered string fields on `Expr` (slices 1–4).
pub mod expr_tree;

pub use expr_tree::{ExprTree, VariantShape};

// ----------------------------------------------------------------------
// Top-level
// ----------------------------------------------------------------------

/// Root MIR object for a single `.qedspec` program.
#[derive(Debug, Clone)]
pub struct Mir {
    /// Spec name (typically `spec <Name>` line).
    pub name: Symbol,
    /// State ADT. For multi-account specs this carries the *primary*
    /// account (single-account renderers stay byte-stable); multi-account
    /// renderers walk `account_states` instead (`account_states[0]` mirrors
    /// this).
    pub state: StateAdt,
    /// Per-account state lifts. Always populated; `len() == 1` for
    /// single-account specs, `> 1` for multi-account (lending = Pool + Loan).
    pub account_states: Vec<AccountStateMir>,
    /// Account-block surface — PDAs, owners, writability, init, authority,
    /// token-type annotations.
    pub accounts: AccountTable,
    /// Declared error variants (from `type Error | InvalidAmount | …` blocks).
    pub errors: ErrorEnum,
    /// Cross-program references — the sole lifted structure for everything
    /// an `import` resolves to AND every inline `interface { … }` block,
    /// keyed by local namespace alias. See `docs/design/mir-unified-imports.md`.
    pub imports: BTreeMap<Symbol, ImportedSpecMir>,
    /// Per-handler IR.
    pub handlers: Vec<HandlerMir>,
    /// Top-level invariants (whole-state predicates, not method-level).
    pub invariants: Vec<InvariantMir>,
    /// Declared events. Auxiliary — read when lowering `HandlerMir.emits`;
    /// not body statements.
    pub events: Vec<EventDecl>,
    /// Top-level `const NAME = VALUE` as `(name, raw-value-string)`;
    /// codegens render `abbrev` (Lean) / `pub const` (Rust).
    pub constants: Vec<(Symbol, String)>,
    /// Helpers referenced from spec bodies but declared opaquely; each
    /// becomes a Lean `opaque <name> : T1 → … → R`.
    pub uninterpreted_helpers: Vec<UninterpretedHelper>,
    /// `ref_impl name (params) : T = <expr>` — reference implementations
    /// referenced from `ensures`. Lower to Lean `def`s and inline at Kani
    /// assertion sites (unlike `uninterpreted_helpers`, these carry
    /// executable bodies).
    pub ref_impls: Vec<RefImpl>,
    /// `property name { ... } preserved_by [op, ...]` — each emits a Lean
    /// predicate `def` + master preservation theorem + per-handler
    /// sub-lemmas. Per-slot proptest forms and quantifier-lint metadata
    /// stay on `ParsedSpec` (proptest-codegen concerns).
    pub properties: Vec<PropertyMir>,
    /// `cover` reachability declarations — one existential theorem per
    /// trace + per `(op, when)` pair.
    pub covers: Vec<CoverMir>,
    /// `liveness` (leads-to) declarations — bounded-reachability theorems
    /// over lifecycle transitions.
    pub liveness_props: Vec<LivenessMir>,
    /// `environment` blocks (external state mutations) — one preservation
    /// theorem per property × environment.
    pub environments: Vec<EnvironmentMir>,
    /// `hook after_store(<field>)` / `hook before_cpi` assertion blocks
    /// (#67). Kani/proptest inject `AfterStore` asserts after matching
    /// field stores; Lean ignores hooks (enforcement deferred to qedsvm).
    pub hooks: Vec<HookMir>,
    /// `ghost` spec-only auxiliary state (#67) — extra verification-State
    /// fields (Lean / proptest / Kani) plus per-handler new-value
    /// expressions; the on-chain codegen ignores these.
    pub ghosts: Vec<GhostMir>,
    /// `type T = { … }` records (value types of `Map[N] T` fields), carried
    /// verbatim from `ParsedSpec.records`; the indexed-state Lean renderer
    /// (sole consumer) emits `structure T` + `instance Inhabited T`.
    pub records: Vec<crate::check::ParsedRecordType>,
    /// True for sBPF assembly specs (`pragma sbpf`, via
    /// `ParsedSpec::is_assembly_target`). Sole consumer is `lean_gen_mir`,
    /// which dispatches to `render_sbpf`; Kani/proptest skip assembly
    /// targets. Instruction / layout / guard data stays on `ParsedSpec`
    /// (one consumer → no cross-codegen divergence to prevent).
    pub is_assembly: bool,
    /// `pragma state_repr = adt` opt-in (`ParsedSpec::state_repr_is_adt`).
    /// True → multi-variant State lowers as `inductive State` (Lean) /
    /// wrapper-struct + inner-enum (Anchor); false (default) → flat
    /// `structure State` + `status` discriminant.
    /// `lean_gen_mir::is_multi_variant_adt` reads this.
    pub adt_state: bool,
}

/// A `hook … { assert <expr>, … }` block (mirrors `ParsedHook`); asserts
/// carry both Lean and Rust renderings.
#[derive(Debug, Clone)]
pub struct HookMir {
    pub kind: HookKind,
    pub asserts: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookKind {
    /// Fires after any store to the named state field.
    AfterStore(Symbol),
    /// Fires before a CPI (optionally only to the named callee namespace).
    BeforeCpi(Option<Symbol>),
}

impl Mir {
    /// Handler body by name. Names are unique (validation rejects dupes),
    /// and the Kani/proptest per-account scoped views only *filter* the
    /// handler set — never rewrite content — so this lookup against the
    /// unscoped `Mir` stays valid inside scoped sections.
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

/// Per-account state lift. `Mir.state` collapses to the primary account for
/// back-compat; `Mir.account_states` carries the full list for per-account
/// `<Name>State` structs, `apply<Name>Op` dispatchers, and per-group
/// preservation theorems.
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
    /// Reserved for spec-level account shapes shared across handlers; today
    /// every binding is handler-scoped (`HandlerMir.accounts`).
    pub spec_level_bindings: BTreeMap<Symbol, AccountBindingShape>,
}

#[derive(Debug, Clone)]
pub struct PdaDeclaration {
    pub name: Symbol,
    /// Seeds — string literals (quoted), account refs, or param refs, as
    /// written in the spec. Not expressions: `ExprTree` has no string
    /// literal, and no backend renders seeds as code (#156).
    pub seeds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AccountBindingShape {
    pub name: Symbol,
    pub writable: bool,
    pub is_signer: bool,
    pub init: bool,
    pub is_program: bool,
    pub kind: AccountKind,
    /// `authority <other_account>` annotation; `None` when undeclared.
    pub authority: Option<AccountRef>,
    /// `PdaDeclaration` key in `Mir.accounts.pdas` for PDA-derived accounts.
    pub pda_ref: Option<Symbol>,
    /// Namespace alias when the account's type comes from an imported spec.
    pub imported_namespace: Option<Symbol>,
    /// Hard-coded base58 pubkey for well-known defaults (system_program,
    /// the program itself, …); lowers to `solana_pubkey::pubkey!("…")`.
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
    /// Authorization requirement (`auth <account>` or dotted
    /// `auth <account>.<field>`). `None` = permissionless or undeclared.
    pub auth: Option<AccountOrField>,
    pub permissionless: bool,
    /// Lifecycle transition (`: State.V1 -> State.V2`). Lowered into a
    /// synthetic entry-`RequireOrAbort` by the `lifecycle_lower` pass;
    /// carried separately to preserve user-level intent.
    pub transition: Option<(VariantTag, VariantTag)>,
    /// Multi-account routing — `: Loan.Empty -> Loan.Active` records
    /// `"Loan"`; `None` = primary account. Mirrors
    /// `ParsedHandler.on_account`; multi-account codegen groups handlers
    /// per account with this.
    pub on_account: Option<Symbol>,
    /// Pre-conditions (schema-includes already expanded upstream in
    /// `chumsky_adapter.rs`).
    pub pre: Vec<Predicate>,
    /// ALL `requires` in original spec order (both `else <Err>` and bare),
    /// before the `split_requires` partition. The indexed-state Lean
    /// renderer emits guard conjuncts from this so ordering matches the
    /// single-list iteration — split-then-concatenate reordered
    /// interleaved bare/with-err sequences.
    pub requires_in_order: Vec<Predicate>,
    /// `requires … else <ErrorName>` clauses — lower to
    /// `Stmt::RequireOrAbort` rather than `pre`. Populated during
    /// parser→MIR; empty after the Phase 3 pass synthesizes them into `body`.
    pub requires_or_abort: Vec<RequireOrAbortClause>,
    /// Handler-level `let name = expr` bindings, in declaration order
    /// (later bindings may reference earlier ones). The Lean transition
    /// binds them before the guard; theorem statements inline them via
    /// [`HandlerMir::inline_let_bindings`].
    pub lets: Vec<(Symbol, Expr)>,
    pub body: Block,
    /// Post-conditions (`ensures`).
    pub post: Vec<Predicate>,
    /// Frame condition — fields that may be modified. `None` =
    /// "everything modifiable per the effects list".
    pub modifies: Option<Vec<Path>>,
    /// Emitted event names; schema lives in `Mir.events`.
    pub emits: Vec<Symbol>,
    /// Names of invariants this handler must preserve.
    pub invariants: Vec<Symbol>,
    /// Invariants this handler *establishes* at post-state without
    /// requiring at pre-state (init / one-shot handlers).
    pub establishes: Vec<Symbol>,
    /// `abstract <name> : <Type>` declarations as (name, dsl-type-string);
    /// each codegen lowers to its own existential / fuzz-input / agent-fill
    /// shape.
    pub abstract_binders: Vec<(Symbol, String)>,
    /// `aborts_total` — abort branches are exhaustive; codegen emits a ↔
    /// theorem instead of per-abort.
    pub aborts_total: bool,
}

impl HandlerMir {
    /// A predicate tree with this handler's `let`-bound names inlined,
    /// transitively (`net = total - fee` inlines `fee` first). Emission
    /// positions outside the transition def — theorem statements — can't
    /// reference the def-level `let` bindings, so they substitute the RHS
    /// trees instead. Bindings without a tree (hand-built fixtures) and
    /// projected reads (`binding.field`) keep the bare name.
    pub fn inline_let_bindings(&self, tree: &ExprTree) -> ExprTree {
        let mut inlined: std::collections::HashMap<&str, ExprTree> =
            std::collections::HashMap::new();
        for (name, rhs) in &self.lets {
            let Some(rhs_tree) = &rhs.tree else { continue };
            let rhs_inlined = expr_tree::map_paths(rhs_tree, &mut |p, _| {
                (matches!(p.binding, expr_tree::BindingKind::LetBound) && p.segments.is_empty())
                    .then(|| inlined.get(p.root.as_str()).cloned())
                    .flatten()
            });
            inlined.insert(name.as_str(), rhs_inlined);
        }
        expr_tree::map_paths(tree, &mut |p, _| {
            (matches!(p.binding, expr_tree::BindingKind::LetBound) && p.segments.is_empty())
                .then(|| inlined.get(p.root.as_str()).cloned())
                .flatten()
        })
    }
}

#[derive(Debug, Clone)]
pub struct RequireOrAbortClause {
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

/// Statement kinds (fixture evidence per kind:
/// `docs/design/intrinsic-fixture-map.md`).
///
/// **Exhaustiveness discipline (#66):** matches over `Stmt` must list every
/// variant explicitly — no `_` arms. The point of the closed enum is that
/// adding a variant breaks every consumer at compile time, forcing each
/// backend to decide how the new statement kind renders (even if the
/// decision is "nothing"). A wildcard arm silently opts the site out of
/// that safety net.
#[derive(Debug, Clone)]
pub enum Stmt {
    // ---- Primary intrinsics ----
    /// Authorization-or-abort: the canonical `requires X else Err` shape.
    RequireOrAbort {
        pred: Predicate,
        err: ErrorRef,
    },

    /// SPL Token Transfer (`call Token.transfer` or `transfers {}` block).
    TokenTransfer {
        from: AccountRef,
        to: AccountRef,
        amount: Expr,
        /// `None` when no `authority` clause. The CPI envelope theorem needs
        /// a 3-account shape, so authorityless transfers skip theorem
        /// emission with an obligation comment.
        authority: Option<AccountRef>,
    },

    /// Lifecycle promotion to a new variant, carrying payload.
    VariantPromote {
        from_tag: VariantTag,
        to_tag: VariantTag,
        payload: Vec<(Symbol, Expr)>,
    },

    // ---- Effect-op kinds ----
    /// `field := value`. Escape hatch for arbitrary effect RHS.
    Assign {
        path: Path,
        rhs: Expr,
    },

    /// `field +=` — checked overflow → ErrorRef (the default arithmetic shape).
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

    /// `field +=?` — wrapping arithmetic, no error (parser op_kind
    /// `add_wrap`; tier table in `rust_codegen_util::emit_transition_fn_inner`).
    WrapAdd {
        path: Path,
        delta: Expr,
    },
    WrapSub {
        path: Path,
        delta: Expr,
    },

    /// `field +=!` — saturating arithmetic, no error (parser op_kind `add_sat`).
    SatAdd {
        path: Path,
        delta: Expr,
    },
    SatSub {
        path: Path,
        delta: Expr,
    },

    // ---- Control flow ----
    /// Conditional effect block, lowered from
    /// `ParsedHandler.effect_branches` (#42).
    Branch {
        scrutinee: BranchScrutinee,
        arms: Vec<BranchArm>,
        default: Option<Block>,
    },

    // ---- Escape hatches ----
    /// Generic CPI to a non-Token interface.
    Cpi {
        /// References the alias in `Mir.imports`.
        target: InterfaceRef,
        /// Which handler within the targeted interface.
        method: MethodRef,
        args: Vec<CallArg>,
        /// Caller-supplied projections from the callee's abstract state
        /// vocabulary onto the caller's concrete State fields. Empty when
        /// the call site declared no `state_binders { ... }` block
        /// (callee-frame, param-only axiom shape).
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

/// Typed expression carrier: the name-resolved tree (#151 Slice 0) plus
/// the source span. The six pre-rendered per-target strings are gone
/// (#156) — every backend renders from the tree. `tree: None` marks the
/// legitimately-empty carrier (`Expr::default()`, e.g. an undeclared
/// transfer amount); render helpers produce the empty string for it.
#[derive(Debug, Clone, Default)]
pub struct Expr {
    pub tree: Option<ExprTree>,
    /// Source span when available; lints may read it, codegens shouldn't.
    pub source_span: Option<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct Predicate(pub Expr);

/// Source-span placeholder — parsing doesn't surface spans uniformly yet;
/// opaque carrier so adding real spans later is non-breaking.
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
    /// `None` for description-only invariants (flagged by `bare_invariant`).
    pub body: Option<Predicate>,
}

// ----------------------------------------------------------------------
// Types
// ----------------------------------------------------------------------

/// The canonical DSL type IR (#327). Every advertised surface spelling
/// parses into a structured variant; `Custom` means a user-declared
/// nominal type (record, sum type, alias target, or imported type),
/// never "the parser did not understand this" — undeclared spellings are
/// rejected by the `unknown_type` check lint before codegen runs.
/// Backends match this enum exhaustively (no `_` arms — the `Stmt`
/// discipline), so a new type shape is a compile error at every
/// consumer, not a silent string fallthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    U8,
    U16,
    U32,
    U64,
    U128,
    I8,
    I16,
    I32,
    I64,
    I128,
    Bool,
    Pubkey,
    /// Opaque 32-byte token (`[u8; 32]`): hash / digest / merkle root.
    /// Equality-only semantics, like `Pubkey`, but lowers to a raw byte
    /// array in every Rust-facing backend (#191).
    Bytes32,
    /// Opaque 64-byte token (`[u8; 64]`): ed25519/secp256k1 signature,
    /// recovered secp pubkey. Equality-only semantics (#191).
    Bytes64,
    /// `Fin[N]` — bounded natural index domain. Bound carried verbatim
    /// (numeric literal or constant name), resolved via the spec's
    /// `const` table where a concrete value is needed.
    Fin {
        bound: Symbol,
    },
    /// `Vec T` — growable sequence. No bound policy in the DSL yet:
    /// Rust-facing backends render `Vec<T>`; proptest reports it as an
    /// unsupported strategy target (#330) until a bound policy exists.
    Vec {
        value: Box<Ty>,
    },
    /// `Option T`.
    Option {
        value: Box<Ty>,
    },
    /// User-declared type name (a record, sum type, or imported type).
    Custom(Symbol),
    /// Bounded map keyed by `Pubkey`; capacity carried verbatim as a string —
    /// numeric literal (`Map[10] T`) or constant name (`Map[MAX_MEMBERS] T`),
    /// the latter resolved via the spec's `const` table.
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

/// Uninterpreted helper signature — Lean `opaque <name> : T1 → … → R`,
/// Rust TODO call-sites. First encounter wins for the inferred signature.
#[derive(Debug, Clone)]
pub struct UninterpretedHelper {
    pub name: Symbol,
    /// DSL-form argument types (`U64`, `Pubkey`, ...).
    pub arg_types: Vec<String>,
    /// DSL-form return type.
    pub return_type: String,
}

/// Spec-level property. `expression` is a pre-rendered Lean `Prop`;
/// `preserved_by` handlers get per-(property × handler) sub-lemmas plus a
/// master case-split theorem.
#[derive(Debug, Clone)]
pub struct PropertyMir {
    pub name: Symbol,
    /// Lean-rendered predicate body, `None` for description-only
    /// properties (no theorem to emit).
    pub expression: Option<Expr>,
    /// Handler names this property is preserved by.
    pub preserved_by: Vec<Symbol>,
    /// Unary (single-state, `s.`) vs Binary (`old(...)`/post) — picks the
    /// Lean render context for the predicate body (#156 emission port).
    pub class: crate::check::PropertyClass,
}

/// Cover (reachability) declaration, mirroring `check::ParsedCover`.
/// `cover <name> [op, ...]` → one trace; `<ident> reachable when <expr>` →
/// a `(handler, Some(expr))` entry. Codegen emits one existential theorem
/// per trace + one per `reachable` entry.
#[derive(Debug, Clone)]
pub struct CoverMir {
    pub name: Symbol,
    /// Handler-name sequences driven from an initial state; the outer Vec
    /// bundles trace alternatives.
    pub traces: Vec<Vec<Symbol>>,
    /// `(handler, optional when-predicate)` claims; `when` is a
    /// Lean-rendered Bool predicate over `s : State`.
    pub reachable: Vec<(Symbol, Option<Predicate>)>,
}

/// Liveness (leads-to) declaration, mirroring `check::ParsedLiveness`.
/// `liveness <name> : <from> ~> <to> via [op, ...] within N` emits
/// `∃ ops, ops.length ≤ N ∧ ∀ s', applyOps s signer ops = some s' → s'.status = .<to>`.
#[derive(Debug, Clone)]
pub struct LivenessMir {
    pub name: Symbol,
    /// Source lifecycle tag (leading `State.` stripped by the parser).
    pub from_state: Symbol,
    /// Target lifecycle tag.
    pub leads_to_state: Symbol,
    /// Eligible handlers; order preserved (the path search walks sequentially).
    pub via_ops: Vec<Symbol>,
    /// Step bound; `None` = legacy default of 10 at emit time.
    pub within_steps: Option<u64>,
}

/// Environment (external-state-change) declaration, mirroring
/// `check::ParsedEnvironment`. Every spec property must hold after the
/// `mutates` fields change under the `constraints`; codegen emits one
/// preservation theorem per (property × environment) with `new_<field>`
/// params and constraint hypotheses.
#[derive(Debug, Clone)]
pub struct EnvironmentMir {
    pub name: Symbol,
    pub mutates: Vec<(Symbol, Ty)>,
    pub external_fields: Vec<(Symbol, Symbol, Ty)>,
    /// Legacy predicate list retained for byte-stable generators.
    /// Typed, temporally classified constraint metadata. Empty for legacy
    /// parsed specs which only supplied `constraints`.
    pub typed_constraints: Vec<EnvironmentConstraintMir>,
}

#[derive(Debug, Clone)]
pub struct EnvironmentConstraintMir {
    pub predicate: Predicate,
    pub class: crate::check::PropertyClass,
}

/// `ref_impl <name> (params) : <return_type> = <expr>` declaration; carries
/// pre-rendered per-backend body strings (opaque-string discipline).
///
/// Deliberately NOT a tree carrier (#223 decision, closing the #156 tail):
/// these are whole *function bodies*, rendered once at adapt time under a
/// fixed context (param scope; `Ctx::Guard`) and emitted verbatim as
/// standalone `def`/`fn` bodies. No backend ever re-targets them across
/// binders or receivers — the one transformation trees exist to make safe
/// — so a structural carrier would add a second source of truth with no
/// consumer. If inlining or verification ever needs to look *inside* a
/// body, add a tree alongside these strings; do not scan or rewrite them.
#[derive(Debug, Clone)]
pub struct RefImpl {
    pub name: Symbol,
    pub doc: Option<String>,
    pub params: Vec<(Symbol, String)>,
    pub return_type: String,
    pub lean_body: String,
    pub rust_body: String,
}

/// Lowered `ghost` declaration (#67) — spec-only auxiliary State field.
/// Verification-State backends (Lean / proptest / Kani) add `name : ty`,
/// initialise to `init`, and assign `name := updates[handler]` in each
/// listed handler; the on-chain codegen never reads this.
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
    /// Callee program ID. `None` for inline `interface { … }` blocks that
    /// omit it; the CPI emitter renders `"<unknown>"` in that case.
    pub program_id: Option<Symbol>,
    pub methods: BTreeMap<Symbol, InterfaceMethod>,
}

#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: Symbol,
    pub params: Vec<(Symbol, Ty)>,
    /// Pre-rendered callee ensures clauses — fed into per-callsite
    /// substitution by `cpi_substitute`.
    pub ensures: Vec<Predicate>,
    /// Typed abstract-state vocabulary from the interface-level
    /// `state { name : Type, ... }` block; the CPI theorem emitter uses it
    /// to pick the Lean codomain in the bundled axiom signature (`State → T`).
    pub state_fields: Vec<(Symbol, Ty)>,
    /// `-> <ident> : <Type>` return-value identifier inside the callee's
    /// ensures; substitution rewrites it to the caller's `let X = ...`
    /// binder. `None` falls back to the literal `"result"`.
    pub result_binder: Option<Symbol>,
    /// Declared handler return type in source DSL form (e.g. `U64`);
    /// `None` for terminal handlers.
    pub return_type: Option<Symbol>,
}

// ----------------------------------------------------------------------
// Cross-program references — unified imports
// ----------------------------------------------------------------------
//
// One canonical lifted structure for everything an `import` resolves to AND
// every inline `interface { … }` block (`docs/design/mir-unified-imports.md`).
//
// Tier classification is derivable, not declared:
//   * Tier 0 — `ImportOrigin::Inline` OR external import with no `ensures`
//     anywhere. No call-site warrant.
//   * Tier 1 — external import with non-empty `ensures` AND `Some(upstream)`
//     carrying a `binary_hash` pin: caller theorems apply the bundled axiom,
//     runtime CPI is warranted by the pin.
//   * Tier 2 — Tier 1 plus a bundled proof package under
//     `crates/qedgen/data/proofs/`; lakefile `require`s pull the callee's
//     verified theorems in directly.

/// One imported source — types and call contracts come from the same
/// artifact, warranted by the same `binary_hash` pin (when external).
#[derive(Debug, Clone)]
pub struct ImportedSpecMir {
    /// Local alias for `call <alias>.handler(...)` / `<alias>.<Type>`.
    /// Falls back to the bound name without an `as` clause; for
    /// `ImportOrigin::Inline` the alias IS the interface name.
    pub alias: Symbol,
    /// Built-in stdlib key, user file path, or `Inline` (no source, no warrant).
    pub origin: ImportOrigin,
    /// Exported account-type declarations; re-emitted as Rust mirrors at
    /// `src/imported/<alias>.rs` when non-empty. Empty for Tier-0
    /// interface-only stubs and inline blocks.
    pub account_types: Vec<crate::check::ParsedAccountType>,
    /// Record types referenced by `account_types`, re-emitted alongside so
    /// the mirror is self-contained.
    pub records: Vec<crate::check::ParsedRecordType>,
    /// Exported interface (call-contract) declarations: handlers, ensures,
    /// requires, and abstract state-field vocabulary. Inline blocks yield a
    /// single entry keyed by the interface name.
    pub interfaces: BTreeMap<Symbol, InterfaceDecl>,
    /// `upstream { binary_hash = ... }` pin warranting the *entire* imported
    /// artifact — both `interfaces` ensures AND `account_types` layouts
    /// (same artifact, not two contracts). `None` for `Inline` (Tier 0 by
    /// construction).
    pub upstream: Option<crate::check::ParsedUpstream>,
    /// `Some(pkg_root)` when the import ships a bundled proof package
    /// (Stance 2) discharging its per-handler ensures; informs the lakefile
    /// `require`. `None` keeps the Stance-1 axiom path (consumer emits its
    /// own sibling axiom module).
    pub verified_pkg_root: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub enum ImportOrigin {
    /// Built-in stdlib key (`"spl"`, `"system"`, `"metaplex"`), resolved
    /// through `crates/qedgen/data/interfaces/<key>.qedspec`.
    Builtin(Symbol),
    /// File-path import; stores the manifest dep key (after `from "..."`).
    File(Symbol),
    /// Inline `interface Foo { ... }` block — no source, no `upstream` pin,
    /// Tier 0 by construction. The interface name doubles as the alias.
    Inline,
}

/// Caller-supplied projection from a callee's abstract state field onto a
/// caller's concrete State field. Today's surface restricts the RHS to a
/// single dotted path (`state.<ident>`); MIR carries a `Path` so richer
/// `state.X.Y` projections can land without reshaping the IR.
#[derive(Debug, Clone)]
pub struct StateBinder {
    /// Callee abstract field name (from the interface's `state { ... }`
    /// block); word-boundary substitution catches every ensures occurrence.
    pub callee_field: Symbol,
    /// Caller-side projection — typically a single bare state field lifted
    /// from `state.<ident>`.
    pub caller_projection: Path,
}

// ----------------------------------------------------------------------
// Reference types
// ----------------------------------------------------------------------

/// Interned symbol — a plain String until the corpus warrants interning.
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
    /// Structured source path for effect targets. This is populated by the
    /// parser adapter and retained through lowering; legacy constructors
    /// leave it empty.
    pub tree: Option<expr_tree::TreePath>,
}

impl Path {
    pub fn single(name: impl Into<Symbol>) -> Self {
        Path {
            segments: vec![name.into()],
            tree: None,
        }
    }

    pub fn dotted(parts: &[&str]) -> Self {
        Path {
            segments: parts.iter().map(|s| s.to_string()).collect(),
            tree: None,
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
    /// Dotted `auth <account>.<field>`. Already desugared to a synthetic
    /// `requires` in `chumsky_adapter.rs`; the structured form is retained
    /// for consumers that want the original shape (e.g. auth-pattern lints).
    AccountField { account: AccountRef, field: Symbol },
}

// ----------------------------------------------------------------------
// Expr constructors
// ----------------------------------------------------------------------

impl Expr {
    /// From a `ParsedRequires`.
    pub fn from_requires(req: &crate::check::ParsedRequires) -> Self {
        Expr {
            tree: req.tree.clone(),
            source_span: None,
        }
    }

    /// From a `ParsedEnsures`.
    pub fn from_ensures(ens: &crate::check::ParsedEnsures) -> Self {
        Expr {
            tree: ens.tree.clone(),
            source_span: None,
        }
    }

    /// From a raw single-form string (synthetic markers, hand-built
    /// test triples). Numeric text becomes an `Int` leaf; anything else
    /// becomes a verbatim-spelling `Unresolved` path, which both
    /// renderers emit as written — reproducing the old string-carrier
    /// behavior without a string field.
    pub fn from_raw(s: impl Into<String>) -> Self {
        let s = s.into();
        let tree = if s.trim().is_empty() {
            None
        } else if let Ok(v) = s.trim().parse::<u128>() {
            Some(ExprTree::Int(v))
        } else {
            Some(ExprTree::Path(expr_tree::TreePath {
                root: s,
                binding: expr_tree::BindingKind::Unresolved,
                segments: vec![],
                ty: None,
            }))
        };
        Expr {
            tree,
            source_span: None,
        }
    }

    /// Attach a typed expression tree (builder-style tail for the
    /// `from_*` constructors).
    pub fn with_tree(mut self, tree: Option<ExprTree>) -> Self {
        self.tree = tree;
        self
    }
}

// ----------------------------------------------------------------------
// Lowering — ParsedSpec → MIR
// ----------------------------------------------------------------------

/// Lower a fully-parsed and checked `ParsedSpec` to MIR — the canonical
/// entry point, consumed by all four codegens.
///
/// Lossless w.r.t. semantics but not source syntax: surface sugar
/// (schema-includes, `transfers` blocks, dotted-auth) is desugared into
/// explicit `Stmt` shapes; schema-include expansion and dotted-auth
/// desugaring have already run upstream in `chumsky_adapter.rs`.
///
/// Pre-condition: `parsed` must have passed `check::check_spec` (post-pass
/// expansions done); lowering does not re-validate.
pub fn lower(parsed: &ParsedSpec) -> Mir {
    Mir {
        name: parsed.program_name.clone(),
        state: lower_state(parsed),
        account_states: lower_account_states(parsed),
        accounts: lower_account_table(parsed),
        errors: lower_errors(parsed),
        imports: lower_imports(parsed),
        handlers: parsed
            .handlers
            .iter()
            .map(|handler| lower_handler(handler, parsed))
            .collect(),
        invariants: lower_invariants(parsed),
        events: lower_events(parsed),
        constants: parsed.constants.clone(),
        hooks: parsed
            .hooks
            .iter()
            .map(|h| HookMir {
                kind: match &h.kind {
                    crate::check::ParsedHookKind::AfterStore(f) => HookKind::AfterStore(f.clone()),
                    crate::check::ParsedHookKind::BeforeCpi(ns) => HookKind::BeforeCpi(ns.clone()),
                },
                asserts: h
                    .asserts
                    .iter()
                    .map(|a| Expr::default().with_tree(a.tree.clone()))
                    .collect(),
            })
            .collect(),
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
                class: p.class,
                expression: p.expression.as_ref().map(|_lean| Expr {
                    tree: p.tree.clone(),
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

/// Lift every `type <Account>` declaration into an `AccountStateMir`;
/// primary account first, multi-account codegen walks the full list.
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
                            ty: parse_ty_resolved(t, parsed),
                        })
                        .collect(),
                })
                .collect();
            let fields = at
                .fields
                .iter()
                .map(|(n, t)| FieldDecl {
                    name: n.clone(),
                    ty: parse_ty_resolved(t, parsed),
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
    // Primary account type drives the state ADT; multi-account specs
    // surface only the primary here.
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
                        ty: parse_ty_resolved(t, parsed),
                    })
                    .collect(),
            })
            .collect(),
        Some(at) => {
            // Single-record account type — one synthetic variant with all
            // fields; tag = account-type name (codegens key on it).
            vec![StateVariant {
                tag: at.name.clone(),
                fields: at
                    .fields
                    .iter()
                    .map(|(n, t)| FieldDecl {
                        name: n.clone(),
                        ty: parse_ty_resolved(t, parsed),
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
                seeds: pda.seeds.clone(),
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

/// Lower import sources (external `imported_namespaces` + inline
/// `interfaces`) to the unified `BTreeMap<Symbol, ImportedSpecMir>`.
///
/// Discrimination rule: external imports register in BOTH
/// `imported_namespaces` (keyed by local alias) and `interfaces`; inline
/// blocks register only in `interfaces`. So: (1) walk
/// `imported_namespaces` → external entries (`Builtin` if `dep_key`
/// resolves via `import_resolver::builtin_source`, else `File`); (2) any
/// `interfaces` entry not keyed in `imported_namespaces` is `Inline`.
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

/// Lift a `ParsedInterface` to `InterfaceDecl`. `program_id` flows through
/// as `Option<Symbol>` (CPI emitter renders `"<unknown>"` for `None`);
/// handler `ensures` become `Predicate`s that `cpi_substitute` rewrites
/// per call site.
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
            body: inv
                .lean_expr
                .as_ref()
                .map(|_lean| Predicate(Expr::default().with_tree(inv.tree.clone()))),
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
                .map(|(op, _when_lean, when_tree)| {
                    let pred = when_tree
                        .as_ref()
                        .map(|t| Predicate(Expr::default().with_tree(Some(t.clone()))));
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
            ty: parse_ty_resolved(&g.ty, parsed),
            init: Expr::default().with_tree(g.init_tree.clone()),
            updates: g
                .updates
                .iter()
                .map(|u| {
                    (
                        u.handler.clone(),
                        Expr::default().with_tree(u.value_tree.clone()),
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
                .map(|(name, ty)| (name.clone(), parse_ty_resolved(ty, parsed)))
                .collect(),
            external_fields: env
                .external_fields
                .iter()
                .map(|(object, field, ty)| {
                    (object.clone(), field.clone(), parse_ty_resolved(ty, parsed))
                })
                .collect(),
            typed_constraints: env
                .typed_constraints
                .iter()
                .map(|constraint| EnvironmentConstraintMir {
                    predicate: Predicate(Expr::default().with_tree(constraint.tree.clone())),
                    class: constraint.class,
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
                    ty: parse_ty_resolved(t, parsed),
                })
                .collect(),
        })
        .collect()
}

fn lower_handler(h: &crate::check::ParsedHandler, parsed: &ParsedSpec) -> HandlerMir {
    let transition = match (&h.pre_status, &h.post_status) {
        (Some(pre), Some(post)) => Some((pre.clone(), post.clone())),
        _ => None,
    };

    let (pre, requires_or_abort) = split_requires(&h.requires);
    // All requires in spec order (see `HandlerMir.requires_in_order`);
    // same predicate construction as `split_requires`.
    let requires_in_order: Vec<Predicate> = h
        .requires
        .iter()
        .map(|r| Predicate(Expr::from_requires(r)))
        .collect();
    HandlerMir {
        name: h.name.clone(),
        doc: h.doc.clone(),
        discriminant: None, // Anchor IDL extractor populates this elsewhere; Phase 0 stub
        params: h
            .takes_params
            .iter()
            .map(|(n, t)| (n.clone(), parse_ty_resolved(t, parsed)))
            .collect(),
        accounts: h.accounts.iter().map(lower_account_binding).collect(),
        auth: lower_auth(h),
        permissionless: h.permissionless,
        transition,
        on_account: h.on_account.clone(),
        pre,
        requires_in_order,
        requires_or_abort,
        lets: h
            .let_bindings
            .iter()
            .map(|b| (b.name.clone(), Expr::default().with_tree(b.tree.clone())))
            .collect(),
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

/// Split `ParsedRequires`: clauses with `error_name = Some(...)` →
/// requires-or-abort (body `Stmt::RequireOrAbort`); bare clauses → `pre`
/// (theorem hypotheses, not enforced via abort).
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
        // Dotted form was already desugared in chumsky_adapter.rs — `who`
        // is the bare signer-account name here.
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
        init: false, // ParsedHandlerAccount doesn't carry `init`; Anchor's
        // #[account(init)] lives in a separate parser surface until v3.0.
        is_program: a.is_program,
        kind,
        authority: a
            .authority
            .as_ref()
            .map(|name| AccountRef::ByBinding(name.clone())),
        pda_ref: None, // inline `pda [seeds]` lives on
        // `ParsedHandlerAccount.pda_seeds`; top-level pdas in AccountTable.
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

    // 2. Effects → typed Stmt kinds per op_kind. Suppressed under
    //    `effect { match … }`: `h.effects` then carries the *union* of all
    //    arms (parser back-compat view) and the real statements live on the
    //    step-7 `Stmt::Branch` — lowering both would double-emit.
    if h.effect_branches.is_none() {
        stmts.extend(lower_effects(&h.effects));
    }

    // 4. Transfers — desugar each into a TokenTransfer Stmt.
    for tr in &h.transfers {
        let amount = match &tr.amount_tree {
            Some(tree) => Expr::default().with_tree(Some(tree.clone())),
            None => tr
                .amount
                .as_ref()
                .map(|a| Expr::from_raw(a.clone()))
                .unwrap_or_default(),
        };
        stmts.push(Stmt::TokenTransfer {
            from: AccountRef::ByBinding(tr.from.clone()),
            to: AccountRef::ByBinding(tr.to.clone()),
            amount,
            authority: tr
                .authority
                .as_ref()
                .map(|a| AccountRef::ByBinding(a.clone())),
        });
    }

    // 5. Explicit CPI calls — ALL lower to `Stmt::Cpi`, including
    //    `call Token.transfer(...)`: the ensures-as-axiom half is for call
    //    sites, the transfer-envelope half is reserved for `transfers {}`
    //    blocks. Collapsing them at lowering time would erase that intent.
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
                        value: Expr::default().with_tree(a.tree.clone()),
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

    // 7. ParsedEffectBranches → `Stmt::Branch`: non-wildcard arms in
    //    declaration order, wildcard arm as `default`. The flat union was
    //    suppressed in step 3 — the Branch is the sole carrier.
    if let Some(br) = &h.effect_branches {
        let mut arms = Vec::new();
        let mut default = None;
        for arm in &br.arms {
            let block = Block {
                stmts: lower_effects(&arm.effects),
            };
            if arm.is_wildcard {
                default = Some(block);
            } else {
                arms.push(BranchArm {
                    pattern: Some(Expr::default().with_tree(arm.pattern_tree.clone())),
                    block,
                });
            }
        }
        stmts.push(Stmt::Branch {
            scrutinee: BranchScrutinee::Match(Expr::default().with_tree(br.scrutinee_tree.clone())),
            arms,
            default,
        });
    }

    Block { stmts }
}

/// Lower `ParsedEffect`s into typed `Stmt`s. Each effect is self-contained
/// (#151 Slice 4): `on_error` supplies the per-site error name for checked
/// variants, `value_rust` the Rust-form RHS (adapter-rendered; see
/// `ParsedEffect::value_rust`), `tree` the typed RHS tree (#151). Shared by
/// the flat-effects path and per-arm `Stmt::Branch` lowering.
fn lower_effects(effects: &[crate::check::ParsedEffect]) -> Vec<Stmt> {
    let mut stmts = Vec::new();
    for eff in effects {
        let err_override = eff.on_error.clone();
        let mut path = parse_field_path(&eff.field);
        path.tree = eff.lhs.clone();
        let tree = eff.tree.clone();
        let value = &eff.value;
        // `value` doubles as the Lean form; the Rust form comes from the
        // adapter rendering when present (empty for ParsedHandlers built
        // outside the chumsky adapter — IDL ingest, probes — where the
        // single string is all we have).
        let rhs = match tree {
            Some(tree) => Expr::default().with_tree(Some(tree)),
            None => Expr::from_raw(if eff.value_rust.is_empty() {
                value.clone()
            } else {
                eff.value_rust.clone()
            }),
        };
        let stmt = match eff.op.as_str() {
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
                // Unknown op_kind — Assign with a structured marker so
                // codegens surface it as a bug.
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

/// Parse a dotted field path (`state.admin`) into a `Path` — splits on `.`.
fn parse_field_path(s: &str) -> Path {
    Path {
        segments: s.split('.').map(|seg| seg.to_string()).collect(),
        tree: None,
    }
}

/// Parse a DSL type string into a `Ty`. Best-effort — unknown forms
/// become `Ty::Custom(name)`. v3.0 will type-check this rigorously.
/// `pub(crate)`: the adapter's tree builder (`chumsky_adapter::tree`)
/// routes `TypeRef::Named` through it so `Custom` spellings stay
/// consistent across lowering and tree annotation.
fn parse_ty_resolved(s: &str, parsed: &ParsedSpec) -> Ty {
    let mut resolved = s.trim();
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert(resolved.to_string()) {
        let Some((_, rhs)) = parsed
            .type_aliases
            .iter()
            .find(|(name, _)| name == resolved)
        else {
            break;
        };
        resolved = rhs.trim();
    }
    parse_ty(resolved)
}

pub(crate) fn parse_ty(s: &str) -> Ty {
    match s.trim() {
        "U8" => Ty::U8,
        "U16" => Ty::U16,
        "U32" => Ty::U32,
        "U64" => Ty::U64,
        "U128" => Ty::U128,
        "I8" => Ty::I8,
        "I16" => Ty::I16,
        "I32" => Ty::I32,
        "I64" => Ty::I64,
        "I128" => Ty::I128,
        "Bool" => Ty::Bool,
        "Pubkey" => Ty::Pubkey,
        "Bytes32" => Ty::Bytes32,
        "Bytes64" => Ty::Bytes64,
        other => {
            // `Map[N] T` / `Map [N] T`: numeric literal or constant-name
            // capacity, passed through as a string (see `Ty::Map`).
            // `split_map_type` tolerates the spaced spelling, matching
            // `map_type` — the Rust and Lean backends must agree on which
            // spellings are structured (#327).
            if other.starts_with("Map") {
                if let Some((cap_str, inner)) = crate::codegen_shared::split_map_type(other) {
                    if !cap_str.is_empty() {
                        return Ty::Map {
                            capacity: cap_str.to_string(),
                            value: Box::new(parse_ty(inner)),
                        };
                    }
                }
            }
            // `Fin[N]` / `Fin [N]`.
            if let Some(rest) = other.strip_prefix("Fin") {
                let rest = rest.trim_start();
                if let Some(bound) = rest
                    .strip_prefix('[')
                    .and_then(|r| r.strip_suffix(']'))
                    .map(str::trim)
                {
                    if !bound.is_empty() {
                        return Ty::Fin {
                            bound: bound.to_string(),
                        };
                    }
                }
            }
            // `Vec T` / `Option T` — whitespace-separated single argument,
            // recursing so `Vec Map[4] U64` stays structured.
            if let Some(inner) = other.strip_prefix("Vec ") {
                return Ty::Vec {
                    value: Box::new(parse_ty(inner)),
                };
            }
            if let Some(inner) = other.strip_prefix("Option ") {
                return Ty::Option {
                    value: Box::new(parse_ty(inner)),
                };
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

    /// `effect { match … }` lowers to a real `Stmt::Branch`: non-wildcard
    /// arms in order, wildcard as `default`, NO flat union effects
    /// (double-emit) and no stub `Abort`.
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
        assert!(
            matches!(&scrut.tree, Some(ExprTree::Path(p)) if p.root == "fee_type"),
            "scrutinee tree should be the fee_type read: {:?}",
            scrut.tree
        );
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

        // No flat union effects alongside the Branch.
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
            )),
            "flat union effects must be suppressed: {body:?}"
        );
    }

    // ---- Type-composition smoke tests ----

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
            hooks: vec![],
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

    #[test]
    fn parse_ty_structured_forms() {
        // #327 — every advertised parameterized spelling parses into a
        // structured variant; `Custom` is reserved for declared nominals.
        assert_eq!(parse_ty("I8"), Ty::I8);
        assert_eq!(parse_ty("I32"), Ty::I32);
        assert_eq!(
            parse_ty("Fin[8]"),
            Ty::Fin {
                bound: "8".to_string()
            }
        );
        assert_eq!(
            parse_ty("Fin[MAX_ACCOUNTS]"),
            Ty::Fin {
                bound: "MAX_ACCOUNTS".to_string()
            }
        );
        assert_eq!(
            parse_ty("Vec U64"),
            Ty::Vec {
                value: Box::new(Ty::U64)
            }
        );
        assert_eq!(
            parse_ty("Option Pubkey"),
            Ty::Option {
                value: Box::new(Ty::Pubkey)
            }
        );
        // Spaced Map spelling now agrees with `map_type`'s tolerance.
        match parse_ty("Map [4] U64") {
            Ty::Map { capacity, value } => {
                assert_eq!(capacity, "4");
                assert!(matches!(*value, Ty::U64));
            }
            other => panic!("expected Map, got {:?}", other),
        }
        // Head names that merely START with a structured keyword stay
        // nominal — `Vector`, `Options`, `Finality`, `MapReduce`.
        assert!(matches!(parse_ty("Vector"), Ty::Custom(s) if s == "Vector"));
        assert!(matches!(parse_ty("Finality"), Ty::Custom(s) if s == "Finality"));
        assert!(matches!(parse_ty("MapReduce"), Ty::Custom(s) if s == "MapReduce"));
    }

    // ---- Fixture-based lowering tests ----
    //
    // Each test parses a real .qedspec from `examples/` via
    // `parse_spec_file` (full chumsky parser + adapter post-passes,
    // matching production) and asserts structural properties of the MIR.

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
        // `parse_spec_file` handles single-file and directory specs
        // (escrow-split needs the directory as input).
        let parsed = crate::check::parse_spec_file(&spec_path)
            .unwrap_or_else(|e| panic!("parse {}: {e}", spec_path.display()));
        lower(&parsed)
    }

    #[test]
    fn lower_preserves_tree_less_raw_effect_and_transfer_values() {
        let mut handler = crate::check::ParsedHandler::default();
        handler
            .effects
            .push(crate::check::ParsedEffect::from_triple(
                "balance", "set", "amount",
            ));
        handler.transfers.push(crate::check::ParsedTransfer {
            from: "source".into(),
            to: "destination".into(),
            amount: Some("amount".into()),
            amount_tree: None,
            authority: None,
        });

        let body = lower_body(&handler);
        assert_eq!(body.stmts.len(), 2);

        let Stmt::Assign { rhs, .. } = &body.stmts[0] else {
            panic!("expected tree-less effect to lower to Assign");
        };
        assert_eq!(
            crate::rust_codegen_util::mir_expr_rust(rhs),
            "amount",
            "raw effect RHS must survive without an adapter tree"
        );

        let Stmt::TokenTransfer { amount, .. } = &body.stmts[1] else {
            panic!("expected tree-less transfer to lower to TokenTransfer");
        };
        assert_eq!(
            crate::rust_codegen_util::mir_expr_rust(amount),
            "amount",
            "raw transfer amount must survive without an adapter tree"
        );
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
        let environment = mir
            .environments
            .iter()
            .find(|environment| environment.name == "interest_rate_change")
            .expect("lending environment");
        assert_eq!(environment.typed_constraints.len(), 1);
        assert_eq!(
            environment.typed_constraints[0].class,
            crate::check::PropertyClass::Unary
        );
        assert!(environment.typed_constraints[0].predicate.0.tree.is_some());
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
    fn lower_environment_preserves_binary_constraint_tree() {
        let parsed = crate::chumsky_adapter::parse_str(
            r#"spec EnvironmentTree
program_id "11111111111111111111111111111111"

type State = { rate : U64, }
type Error | Bad

environment rate_change {
  mutates rate : U64
  constraint state.rate >= old(state.rate)
}
"#,
        )
        .expect("parse binary environment constraint");
        let mir = lower(&parsed);
        let environment = &mir.environments[0];
        let typed = &environment.typed_constraints[0];

        assert_eq!(typed.class, crate::check::PropertyClass::Binary);
        assert!(matches!(typed.predicate.0.tree, Some(ExprTree::Cmp { .. })));
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
        // Every effect op_kind in the pilot fixtures lowers to its typed
        // Stmt kind (no unknown-op fallback marker).
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
                        let rendered = crate::rust_codegen_util::mir_expr_rust(rhs);
                        assert!(
                            !rendered.starts_with("/* MIR-TODO: unknown op_kind"),
                            "fixture {} has unknown-op_kind effect: {}",
                            fixture,
                            rendered
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lower_preserves_handler_count_across_pilot() {
        // Smoke: every pilot fixture parses + lowers without panic, ≥1 handler.
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

    // ---- lower_imports tests ----

    #[test]
    fn lower_imports_builtin_spl_token() {
        // `import Token from "spl"` should land as `Mir.imports["Token"]`
        // tagged `Builtin("spl")` with the SPL Token interface lifted into
        // `.interfaces["Token"]` — even though the bundled stub declares
        // no `type`s.
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
        // Inline `interface MockEncrypt { ... }` lowers as
        // `Mir.imports["MockEncrypt"]` tagged `Inline`, no upstream pin.
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
        // CPIs only via `transfers {}` sugar (no `import`, no inline
        // `interface`) → empty `Mir.imports`. Escrow is the canonical case.
        let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
        assert!(
            mir.imports.is_empty(),
            "escrow should have no lifted imports; got: {:?}",
            mir.imports.keys().collect::<Vec<_>>()
        );
    }

    // ---- ExprTree carriage (#151 Slice 0) ----

    /// Every adapter-built expression position must carry a typed tree
    /// through lowering: RequireOrAbort predicates, `pre` hypotheses,
    /// `post` ensures, effect-statement RHS, and property bodies. The
    /// pilot fixtures exercise the full DSL surface, so a `None` here
    /// means an adapt site forgot to build its tree.
    #[test]
    fn lower_carries_expr_trees_across_pilots() {
        for fixture in &[
            "examples/rust/escrow/escrow.qedspec",
            "examples/rust/lending/lending.qedspec",
            "examples/rust/multisig/multisig.qedspec",
            "examples/rust/bundled-stdlib-demo/pool.qedspec",
        ] {
            let mir = lower_fixture(fixture);
            for h in &mir.handlers {
                for p in h.pre.iter().chain(h.post.iter()) {
                    assert!(
                        p.0.tree.is_some(),
                        "{fixture}: handler {} pre/post missing tree",
                        h.name,
                    );
                }
                for s in &h.body.stmts {
                    let rhs = match s {
                        Stmt::RequireOrAbort { pred, .. } => Some(&pred.0),
                        Stmt::Assign { rhs, .. } => Some(rhs),
                        Stmt::CheckedAdd { delta, .. }
                        | Stmt::CheckedSub { delta, .. }
                        | Stmt::WrapAdd { delta, .. }
                        | Stmt::WrapSub { delta, .. }
                        | Stmt::SatAdd { delta, .. }
                        | Stmt::SatSub { delta, .. } => Some(delta),
                        // Transfers / CPI / events / branches / aborts carry
                        // trees in later slices (or none applies).
                        Stmt::TokenTransfer { .. }
                        | Stmt::VariantPromote { .. }
                        | Stmt::Branch { .. }
                        | Stmt::Cpi { .. }
                        | Stmt::Emit { .. } => None,
                    };
                    if let Some(expr) = rhs {
                        assert!(
                            expr.tree.is_some(),
                            "{fixture}: handler {} stmt missing tree",
                            h.name,
                        );
                    }
                }
            }
            for prop in &mir.properties {
                if let Some(e) = &prop.expression {
                    assert!(
                        e.tree.is_some(),
                        "{fixture}: property {} missing tree",
                        prop.name
                    );
                }
            }
        }
    }
}

pub(crate) mod cpi_substitute;
