//! Typed AST for `.qedspec` files — produced by the chumsky parser, then
//! translated to `ParsedSpec` by the adapter for downstream consumers.
//!
//! Design: guard expressions are a real algebraic type (enables scope
//! checking, exhaustiveness, match-in-handler, target-specific codegen);
//! every node carries a `Span` for diagnostics; no backend concerns leak in
//! (no Lean unicode, no Rust identifiers).
//!
//! `#![allow(dead_code)]`: some fields (`span`, `TypeRef::Param`,
//! `MatchBody::Noop`, doc strings) are parsed but not yet consumed by every
//! backend — intentional scaffolding for planned diagnostics and doc
//! preservation. Removing them would lose information the parser recovers.

#![allow(dead_code)]

use std::ops::Range;

/// Source span as a byte range into the input string.
pub type Span = Range<usize>;

/// Node wrapper carrying a span. Cheap to carry everywhere.
#[derive(Debug, Clone)]
pub struct Node<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Node<T> {
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span }
    }
}

// ============================================================================
// Top-level spec structure
// ============================================================================

#[derive(Debug, Clone)]
pub struct Spec {
    pub name: String,
    pub items: Vec<Node<TopItem>>,
}

#[derive(Debug, Clone)]
pub enum TopItem {
    /// `const NAME = VALUE` — top-level integer constant. `i128` so negative
    /// constants parse directly; consumers format via `to_string()` and
    /// `infer_const_type` picks a signed Rust type for negatives.
    Const {
        name: String,
        value: i128,
    },
    /// `type T = { f : Type, ... }` — plain record.
    Record(RecordDecl),
    /// `type State | Active of { ... } | Draining | ...` — ADT. Flat error
    /// enums (`type Error | Foo | Bar = 42 "desc"`) are the no-payload case,
    /// represented as an ADT with variants with no payload; the name
    /// "Error" is conventional.
    Adt(AdtDecl),
    /// `handler name (args) { requires / effect / … }` — an instruction handler.
    Handler(HandlerDecl),
    Property(PropertyDecl),
    Cover(CoverDecl),
    Liveness(LivenessDecl),
    Invariant(InvariantDecl),
    /// `pda name [seed1, seed2, ...]` — PDA seed declaration.
    Pda(PdaDecl),
    /// `event name { typed fields }` — emitted event declaration.
    Event(EventDecl),
    /// `environment name { mutates/constraint }` — external state.
    Environment(EnvironmentDecl),
    /// `program_id "..."` — explicit program ID.
    ProgramId(String),
    /// `type Name = <type_ref>` — type alias, expands to its target.
    TypeAlias(TypeAliasDecl),
    /// `dimension Name = U64` — nominal numeric type. Runtime ABI erases to
    /// the integer base while the checker preserves `Name` across arithmetic.
    Dimension(DimensionDecl),
    /// `pubkey NAME [u64, u64, u64, u64]` — 4-chunk U64 pubkey literal (sBPF sugar).
    Pubkey(PubkeyDecl),
    /// `errors [Name = code "desc", ...]` — top-level error list (sBPF sugar,
    /// equivalent to `type Error | Name = code "desc" | ...`).
    Errors(Vec<ErrorEntry>),
    /// `instruction Name { ... }` — sBPF instruction block with layouts,
    /// guards, and properties.
    Instruction(InstructionDecl),
    /// `interface Name { program_id "...", upstream { ... }, handler h(args) { ... } }`
    /// Declares a callee's public contract so a caller can `call Name.h(...)`
    /// with backend-appropriate artifacts. See docs/design/spec-composition.md §2.
    Interface(InterfaceDecl),
    /// `import Name from "key"` — bind a local name to an interface declared
    /// in a dependency. `from` keys into `qed.toml`'s `[dependencies]`; the
    /// resolver fetches the source (github / path) and merges the imported
    /// `interface` declarations under the local `name`. See
    /// docs/design/spec-composition.md §3.
    Import {
        name: String,
        from: String,
        /// Optional `as Alias` — renames the import in the consumer's namespace.
        as_name: Option<String>,
    },
    /// `pragma <name> { <top_item>* }` — platform-specific namespace. Keeps
    /// the core DSL platform-agnostic; target inference reads
    /// `ParsedSpec.pragmas` — presence of `sbpf` selects the assembly target
    /// with no explicit `target` keyword.
    Pragma(PragmaDecl),
    /// `pragma <name> = <ident>` — top-level key=value assignment, e.g.
    /// `checked_overflow_error = MintOverflow` overriding the built-in
    /// `MathOverflow` / `MathUnderflow` defaults that `mechanize_effect`
    /// lowers `+=` / `-=` against. Per-effect overrides
    /// (`EffectStmt.on_error`) still win over the pragma.
    PragmaAssign {
        name: String,
        value: String,
    },
    /// `schema name { requires expr else Err … }` — reusable cross-cutting
    /// guard set. Handlers reference it via `uses name`; the adapter expands
    /// each schema requires into the handler's requires list.
    Schema(SchemaDecl),
    /// `ref_impl name (state : State) (params...) : T = <expr>` — reference
    /// implementation `ensures` clauses can call. Lowers to a Lean `def`,
    /// inlines at Kani assertion sites; Rust codegen skips it (verification-
    /// only). Distinct from `Ghost`: `ref_impl` is a *stateless* pure
    /// function the real impl is checked against; a `ghost` is *stateful*
    /// spec-only state updated per-handler.
    RefImpl(RefImplDecl),
    /// `ghost <name> : <Ty> { init {…} on H(…) {…} }` — spec-only auxiliary
    /// state field: participates in invariants / properties / `requires` /
    /// `ensures` (as `state.<name>`), updated per-handler, omitted from
    /// on-chain codegen.
    Ghost(GhostDecl),
    /// `hook <kind> { assert … }` — cross-cutting assertion fired at a
    /// MIR-statement boundary; enforced in the Kani / proptest harnesses.
    Hook(HookDecl),
}

/// Top-level `schema` block. No state effects or ensures — schemas are
/// *only* for cross-cutting guards.
#[derive(Debug, Clone)]
pub struct SchemaDecl {
    pub name: String,
    pub doc: Option<String>,
    /// One `requires <expr> [else <ErrorName>]` per entry — same
    /// `(body, on_fail)` shape as `HandlerClause::Requires` so the adapter
    /// reuses the same lowering path.
    pub requires: Vec<(Node<Expr>, Option<String>)>,
}

/// Platform-specific namespace. Parser accepts arbitrary `TopItem`s inside;
/// the adapter restricts which items are valid per pragma name.
#[derive(Debug, Clone)]
pub struct PragmaDecl {
    pub name: String,
    pub doc: Option<String>,
    pub items: Vec<Node<TopItem>>,
}

// ============================================================================
// sBPF-specific AST nodes (instruction blocks, layouts, guards, properties)
// ============================================================================

/// `pubkey NAME [c0, c1, c2, c3]` — 32-byte pubkey as 4 U64 chunks.
#[derive(Debug, Clone)]
pub struct PubkeyDecl {
    pub name: String,
    pub chunks: Vec<u128>,
}

/// One entry in a top-level `errors [...]` list.
#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub name: String,
    pub code: Option<u64>,
    pub description: Option<String>,
}

/// `instruction Name { ... }` — an sBPF instruction handler.
#[derive(Debug, Clone)]
pub struct InstructionDecl {
    pub name: String,
    pub doc: Option<String>,
    pub items: Vec<InstructionItem>,
}

/// A clause inside an instruction block.
#[derive(Debug, Clone)]
pub enum InstructionItem {
    /// `discriminant IDENT` or `discriminant INT`.
    Discriminant(String),
    /// `entry N` — byte offset of instruction entry point in the program.
    Entry(u64),
    /// `const NAME = VALUE` — instruction-local constant. `i128` for
    /// negative constants; see `TopItem::Const`.
    Const { name: String, value: i128 },
    /// `errors [...]` inside an instruction — per-instruction error list.
    Errors(Vec<ErrorEntry>),
    /// `input_layout { ... }` — layout of input buffer.
    InputLayout(Vec<LayoutField>),
    /// `insn_layout { ... }` — layout of instruction data register.
    InsnLayout(Vec<LayoutField>),
    /// `guard NAME { ... }` — a validation guard.
    Guard(GuardDecl),
    /// `property NAME { ... }` — an sBPF property block.
    SbpfProperty(SbpfPropertyDecl),
}

/// A field in `input_layout {}` / `insn_layout {}`:
/// `name : Type @ offset "description"`.
#[derive(Debug, Clone)]
pub struct LayoutField {
    pub name: String,
    pub field_type: String,
    pub offset: i64,
    pub description: Option<String>,
}

/// `guard NAME { checks? error fuel? }`.
#[derive(Debug, Clone)]
pub struct GuardDecl {
    pub name: String,
    pub doc: Option<String>,
    /// Parsed checks expression, or None if the guard has no checks clause.
    pub checks: Option<Node<Expr>>,
    pub error: String,
    pub fuel: Option<u64>,
}

/// `property NAME { ... }` — sBPF property body.
#[derive(Debug, Clone)]
pub struct SbpfPropertyDecl {
    pub name: String,
    pub doc: Option<String>,
    pub clauses: Vec<SbpfPropClause>,
}

/// Clause inside an sBPF property block.
#[derive(Debug, Clone)]
pub enum SbpfPropClause {
    /// `expr <guard-expr>` — a propositional body.
    Expr(Node<Expr>),
    /// `preserved_by all | [...]` — preservation hint.
    PreservedBy(PreservedBy),
    /// `scope guards | [names]` — memory-safety scope selector.
    Scope(Vec<String>),
    /// `flow target from seeds [...]` or `flow target through [...]`.
    Flow { target: String, kind: SbpfFlowKind },
    /// `cpi program instruction { ... }` — expected CPI envelope.
    Cpi {
        program: String,
        instruction: String,
        fields: Vec<(String, String)>,
    },
    /// `after all guards` — mark the subsequent `exit` as the post-guard exit.
    AfterAllGuards,
    /// `exit N` — happy-path exit code.
    Exit(u64),
}

#[derive(Debug, Clone)]
pub enum SbpfFlowKind {
    FromSeeds(Vec<String>),
    Through(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct TypeAliasDecl {
    pub name: String,
    pub target: TypeRef,
}

#[derive(Debug, Clone)]
pub struct DimensionDecl {
    pub name: String,
    pub base: TypeRef,
}

// ============================================================================
// Type declarations
// ============================================================================

#[derive(Debug, Clone)]
pub struct RecordDecl {
    pub name: String,
    pub fields: Vec<TypedField>,
}

#[derive(Debug, Clone)]
pub struct AdtDecl {
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub code: Option<u64>,
    pub description: Option<String>,
    pub fields: Vec<TypedField>,
}

#[derive(Debug, Clone)]
pub struct TypedField {
    pub name: String,
    pub ty: TypeRef,
}

/// `ref_impl name (p1 : T1) (p2 : T2) : R = <expr>` — body is a pure
/// expression over the typed parameters; no state mutation, no side effects.
#[derive(Debug, Clone)]
pub struct RefImplDecl {
    pub name: String,
    pub doc: Option<String>,
    pub params: Vec<TypedField>,
    pub return_type: TypeRef,
    pub body: Node<Expr>,
}

/// `ghost <name> : <Ty> { init { <expr> } on <handler>(<params>) { <name>
/// := <expr> } … }`. See `TopItem::Ghost`. Scalar types only
/// (`U64`/`U128`/`I64`/`I128`/`Bool`); a ghost over a `Map`/record is
/// rejected at lint time.
#[derive(Debug, Clone)]
pub struct GhostDecl {
    pub name: String,
    pub doc: Option<String>,
    pub ty: TypeRef,
    /// Initial value — a constant expression evaluated when the State is
    /// first constructed.
    pub init: Node<Expr>,
    /// Per-handler update clauses. A handler with no clause leaves the
    /// ghost unchanged (frame condition).
    pub updates: Vec<GhostUpdate>,
}

/// One `on <handler> { <name> := <expr> }` clause of a ghost. The named
/// handler's parameters are in scope in the update RHS (resolved by the
/// adapter from the handler's signature, so there's no type redeclaration
/// to drift). `stmt` is a single assignment whose `lhs` is the ghost name
/// and whose `rhs` may reference `state.<ghost>` and the handler params.
#[derive(Debug, Clone)]
pub struct GhostUpdate {
    pub handler: String,
    pub stmt: EffectStmt,
}

/// `hook <kind> { assert <expr> … }` — assertions fire wherever a matching
/// MIR statement occurs in any handler. Enforced in the runtime backends
/// (Kani / proptest); Lean enforcement lands with qedsvm.
#[derive(Debug, Clone)]
pub struct HookDecl {
    pub doc: Option<String>,
    pub kind: HookKind,
    /// One or more `assert <expr>` predicates checked at the firing point.
    pub asserts: Vec<Node<Expr>>,
}

/// Which MIR-statement boundary a hook fires at.
#[derive(Debug, Clone)]
pub enum HookKind {
    /// `after_store(<field>)` — fires immediately after any assignment to
    /// the named state field. Anchored in the runtime transition right
    /// after that field's effect.
    AfterStore(String),
    /// `before_cpi` / `before_cpi(<Iface>)` — fires before a CPI (optionally
    /// only to the named callee). Enforcement is the Lean CPI-theorem
    /// precondition path (deferred); the runtime state model has no CPI to
    /// anchor to, so this currently lints as enforcement-deferred.
    BeforeCpi(Option<String>),
}

/// A type reference in the source language.
#[derive(Debug, Clone)]
pub enum TypeRef {
    /// Named type or primitive: `U128`, `Account`, `Pubkey`.
    Named(String),
    /// Parameterized: `Vec U64`, `Option Pubkey`.
    Param(String, String),
    /// `Map[N] T` — bounded map keyed by an index domain of size `N`.
    Map { bound: String, inner: Box<TypeRef> },
    /// `Fin[N]` — bounded natural index domain of size `N`. Used as the
    /// index type in aliases like `type AccountIdx = Fin[MAX_ACCOUNTS]`.
    Fin { bound: String },
}

// ============================================================================
// Handlers
// ============================================================================

#[derive(Debug, Clone)]
pub struct HandlerDecl {
    pub name: String,
    pub doc: Option<String>,
    pub params: Vec<TypedField>,
    /// Pre/post state references (`Pool.Active` etc.). None for
    /// unannotated handlers.
    pub pre: Option<QualifiedPath>,
    pub post: Option<QualifiedPath>,
    pub clauses: Vec<Node<HandlerClause>>,
}

#[derive(Debug, Clone)]
pub enum HandlerClause {
    Auth(String),
    Accounts(Vec<AccountDescriptor>),
    Requires {
        guard: Node<Expr>,
        on_fail: Option<String>,
    },
    Ensures(Node<Expr>),
    Modifies(Vec<String>),
    Let {
        name: String,
        value: Node<Expr>,
    },
    /// Effect body items: unconditional statements or `match` blocks.
    Effect(Vec<Node<EffectBlock>>),
    /// `transfers { from A to B amount X authority Y; ... }` — token transfer intents.
    Transfers(Vec<TransferClause>),
    /// Legacy sugar: `takes x : Type` or `takes { x : T, y : U }` —
    /// equivalent to declaring `(x : Type)` in the handler signature.
    Takes(Vec<TypedField>),
    /// Guarded branches: first-match dispatch on boolean conditions.
    /// Desugars to multiple synthetic handlers in the adapter; lets a
    /// single declared handler have multiple outcomes (abort vs effect
    /// vs different post-states) depending on runtime state.
    Match(MatchClause),
    Emits(String),
    AbortsTotal,
    /// `abstract <name> : <Type>` — existentially-quantified value usable in
    /// `requires` / `effect` / `ensures` without expressing how it's
    /// computed. Lowers to `kani::any()` + `assume` (Kani), `Arbitrary` +
    /// `prop_assume!` (proptest), an existential in Lean, and
    /// `let <name>: T = todo!(...)` in the Rust scaffold (agent-filled).
    Abstract {
        name: String,
        ty: TypeRef,
    },
    Invariant(String),
    /// `establishes Name` — handler establishes the named invariant at
    /// post-state. Unlike `invariant Name` ("preserves"), the harness/proof
    /// does NOT assume the invariant pre-transition. For init-style or
    /// one-shot transitions that make the invariant true.
    Establishes(String),
    /// `permissionless` — deliberately-unauthenticated; opts out of the
    /// `no_access_control` P1 lint. Mutually exclusive with `auth X`
    /// (check.rs rejects both together).
    Permissionless,
    /// `include schema_name` — forward-compat; phase 1 rejects.
    Include(String),
    /// `call Interface.handler(name = expr, ...)` — terminal CPI invocation.
    /// Resolves against a top-level `interface` block; backends emit
    /// tier-appropriate artifacts (CPI builder in Rust, hypotheses/rewrites
    /// in Lean when the interface declares ensures). See
    /// docs/design/spec-composition.md §2.
    Call(CallExpr),
}

/// `call Target.handler(arg1 = v1, arg2 = v2, ...)` parsed form.
#[derive(Debug, Clone)]
pub struct CallExpr {
    /// Qualified name of the target. Usually `Interface.handler` (len 2),
    /// but longer paths are accepted — the resolver decides.
    pub target: QualifiedPath,
    /// Keyword arguments, in source order. Positional args are not allowed.
    pub args: Vec<CallArg>,
    /// Optional `let <name> = call …` binding. The interface handler's
    /// return-type declaration gives the binding semantics; without it the
    /// binding is opaque.
    pub result_binding: Option<String>,
    /// Optional `state_binders { callee_field = state.X, ... }` block —
    /// maps each callee-side abstract State field (e.g. SPL Token
    /// `from_balance`) to a caller-side state path. Backends extend the Lean
    /// axiom signature with `(field : State → Nat)` accessor params and
    /// substitute `pre/post.<callee_field>` → `pre/post.<caller_field>` in
    /// the Kani assume splice. Empty (default) keeps the callee-frame,
    /// param-only axiom shape.
    pub state_binders: Vec<StateBinder>,
}

#[derive(Debug, Clone)]
pub struct CallArg {
    pub name: String,
    pub value: Node<Expr>,
}

/// One entry inside `state_binders { ... }`: callee-side abstract field
/// (LHS) → caller-side state path (RHS). The RHS must be a `state.<ident>`
/// field path — richer forms (let-bindings, computed paths) are rejected at
/// lower time. Backends combine the extracted field with `pre.` / `post.`,
/// or wrap it as a Lean accessor lambda `(·.<caller_field>)`.
#[derive(Debug, Clone)]
pub struct StateBinder {
    /// Callee's abstract field name; word-boundary substitution finds every
    /// occurrence in the callee's `ensures` text.
    pub callee_field: String,
    /// Caller-side expression; the adapter validates the `state.<ident>`
    /// shape and extracts the trailing field name.
    pub caller_expr: Node<Expr>,
}

#[derive(Debug, Clone)]
pub struct AccountDescriptor {
    pub name: String,
    pub attrs: Vec<AccountAttr>,
}

// ============================================================================
// Interface declarations (callee contracts for CPI)
// ============================================================================

/// `interface Name { program_id "...", upstream { ... }, handler h(args) { ... } }`
///
/// Contract for a program we CPI into. Shape-only (Tier 0), hand-authored
/// effects (Tier 1), or imported from another qedspec (Tier 2) — the AST is
/// the same shape, backends decide how much they can emit based on whether
/// `requires`/`ensures` are populated.
#[derive(Debug, Clone)]
pub struct InterfaceDecl {
    pub name: String,
    pub doc: Option<String>,
    pub program_id: Option<String>,
    pub upstream: Option<UpstreamDecl>,
    /// Optional `state { name : Type, ... }` — types of the abstract State
    /// accessors referenced by handler ensures; defaults to `Nat` when
    /// absent (back-compat). The type picks the Lean codomain of
    /// `(X : State → T)` in the axiom signature: `U*` → `Nat`, `I*` → `Int`,
    /// `Bool` → `Bool`, `Pubkey` → `Pubkey`.
    pub state_fields: Vec<TypedField>,
    pub handlers: Vec<InterfaceHandlerDecl>,
}

/// `upstream { package "...", version "...", binary_hash "...", ... }` —
/// pins a library interface to the exact upstream program it was verified
/// against. `binary_hash` is authoritative; the rest is informational.
#[derive(Debug, Clone, Default)]
pub struct UpstreamDecl {
    pub package: Option<String>,
    pub version: Option<String>,
    pub source: Option<String>,
    pub binary_hash: Option<String>,
    pub idl_hash: Option<String>,
    /// Which backends were actually run (e.g. ["proptest", "kani"]).
    /// `"lean"` appears only when the program is genuinely proven, not
    /// merely axiomatized — no overclaiming.
    pub verified_with: Vec<String>,
    pub verified_at: Option<String>,
}

/// A handler inside an `interface` block. Structurally a subset of
/// `HandlerDecl`: no pre/post transition, no `effect`, no `emits` (callee
/// state is opaque to the caller; callee events are the callee's business).
#[derive(Debug, Clone)]
pub struct InterfaceHandlerDecl {
    pub name: String,
    pub doc: Option<String>,
    pub params: Vec<TypedField>,
    /// Optional `-> Type` return type. When `Some`, `let x = call
    /// Foo.handler(...)` lowers to `T::try_from_slice(&ret_bytes)?` after
    /// the CPI via `get_return_data`; `None` keeps the terminal-statement
    /// shape.
    pub return_type: Option<TypeRef>,
    /// Optional named binder for the return value (`-> result : U64`) —
    /// a free name in scope of the handler's own `ensures`; the CPI
    /// substitution helper rewrites it to the caller's `let X = …` binder
    /// per call site. Defaults to `"result"` when only `-> Type` is written.
    pub result_binder: Option<String>,
    pub clauses: Vec<Node<InterfaceHandlerClause>>,
}

/// Clauses allowed inside an interface-handler body.
#[derive(Debug, Clone)]
pub enum InterfaceHandlerClause {
    /// `discriminant 0xABCD` or `discriminant name` — instruction selector.
    Discriminant(String),
    Accounts(Vec<AccountDescriptor>),
    Requires {
        guard: Node<Expr>,
        on_fail: Option<String>,
    },
    Ensures(Node<Expr>),
}

#[derive(Debug, Clone)]
pub struct TransferClause {
    pub from: String,
    pub to: String,
    pub amount: Option<TransferAmount>,
    pub authority: Option<String>,
}

/// A `branch { case c1: b1 | case c2: b2 | otherwise: b3 }` construct.
/// Dispatched first-match: the first arm whose guard holds fires. Arms
/// without a guard (`otherwise`) always match and must appear last.
#[derive(Debug, Clone)]
pub struct MatchClause {
    pub arms: Vec<MatchArm>,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    /// Guard expression. `None` for `otherwise` arms.
    pub guard: Option<Node<Expr>>,
    pub body: MatchBody,
    /// Suffix attached to the synthetic handler name. Derived from the
    /// user-supplied label (if any) or an ordinal index.
    pub label: String,
}

#[derive(Debug, Clone)]
pub enum MatchBody {
    /// `abort ErrorName` — case maps to a new aborting requires.
    Abort(String),
    /// `effect { ... }` — case maps to a synthetic handler with these effects.
    Effect(Vec<Node<EffectStmt>>),
    /// Empty body — case is a no-op (state unchanged, no error).
    Noop,
    /// `call Interface.handler(...)` — case maps to a synthetic handler
    /// issuing this CPI (outcome-conditional CPI patterns). Optional effect
    /// block alongside the call for cases that do both.
    Call(CallExpr, Vec<Node<EffectStmt>>),
}

#[derive(Debug, Clone)]
pub enum TransferAmount {
    Literal(u128),
    Path(Path),
}

#[derive(Debug, Clone)]
pub enum AccountAttr {
    Simple(String),    // signer, writable, readonly, program, token
    Type(String),      // `type token`
    Authority(String), // `authority x`
    Pda(Vec<String>),
}

// ============================================================================
// Effects
// ============================================================================

#[derive(Debug, Clone)]
pub struct EffectStmt {
    pub lhs: Path,
    pub op: EffectOp,
    pub rhs: Node<Expr>,
    /// Per-site error-variant override on checked `+=` / `-=`:
    /// `pool += amount else MintOverflow`. Keyword is `else` (as in
    /// `requires X else Err`), not `or` — `or` would collide with the
    /// boolean infix parsed by `expr()`. `None` falls back to the
    /// `pragma checked_*_error` default, then built-in `MathOverflow` /
    /// `MathUnderflow`. Always `None` for saturating / wrapping / `Set` —
    /// those can't fail, so the adapter drops any captured override.
    pub on_error: Option<String>,
}

/// A single statement inside `effect { … }`: a leaf `EffectStmt`
/// (`x += y`) or a `match`-shape conditional over a scrutinee expression.
#[derive(Debug, Clone)]
pub enum EffectBlock {
    /// Unconditional effect statement.
    Stmt(EffectStmt),
    /// `match <scrutinee> { 0 => <effect>, 1 => <effect>, _ => <effect> }`.
    Match {
        scrutinee: Node<Expr>,
        arms: Vec<EffectMatchArm>,
    },
}

/// One arm of an effect-level `match`. Literal-integer patterns and `_`
/// wildcard only.
#[derive(Debug, Clone)]
pub struct EffectMatchArm {
    pub pattern: EffectPattern,
    pub body: Vec<Node<EffectBlock>>,
}

/// Patterns accepted by effect-block `match` arms.
#[derive(Debug, Clone)]
pub enum EffectPattern {
    Literal(u128),
    Wildcard,
}

impl EffectBlock {
    /// Walk the block tree and yield every leaf `EffectStmt` in source
    /// order. Used by consumers that don't care about conditional
    /// structure — typechecking, the cross-handler expr walker.
    pub fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a EffectStmt>) {
        match self {
            EffectBlock::Stmt(s) => out.push(s),
            EffectBlock::Match { arms, .. } => {
                for arm in arms {
                    for nested in &arm.body {
                        nested.node.collect_leaves(out);
                    }
                }
            }
        }
    }
}

/// Collect leaf `EffectStmt`s from a `Vec<Node<EffectBlock>>`.
pub fn flatten_effect_blocks(blocks: &[Node<EffectBlock>]) -> Vec<&EffectStmt> {
    let mut out = Vec::new();
    for b in blocks {
        b.node.collect_leaves(&mut out);
    }
    out
}

/// Per-effect arithmetic semantics. Checked is the default — it matches
/// deployed `checked_add(..).ok_or(err)?` patterns (overflow short-circuits
/// the transition); wrapping is an explicit opt-in for modular arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectOp {
    /// `+=` — checked add (default, matches `checked_add` in deployed programs)
    Add,
    /// `+=!` — saturating add
    AddSat,
    /// `+=?` — wrapping add
    AddWrap,
    /// `-=` — checked sub (default)
    Sub,
    /// `-=!` — saturating sub
    SubSat,
    /// `-=?` — wrapping sub
    SubWrap,
    /// `:=` (or `=`)
    Set,
}

// ============================================================================
// Paths, qualified identifiers, subscripts
// ============================================================================

/// A path expression: `state.accounts[i].capital`, `s.x`, `authority`.
/// Stored as a root ident + a list of segments so we can inspect structure
/// (e.g., detect Map-typed roots).
#[derive(Debug, Clone)]
pub struct Path {
    pub root: String,
    pub segments: Vec<PathSeg>,
}

impl Path {
    /// Render as source syntax: `root.field[idx].field`. The single source
    /// of truth for Path→string spelling — renderers that need a prefix
    /// swap (Lean `s.`/`s'.`) keep their own root handling.
    pub fn to_source_string(&self) -> String {
        let mut s = self.root.clone();
        push_segments_source(&mut s, &self.segments);
        s
    }

    /// Segments-only render (no root) — the `state.`-stripped form:
    /// `state.a[i].b` → `a[i].b`.
    pub fn segments_source_string(&self) -> String {
        let mut s = String::new();
        push_segments_source(&mut s, &self.segments);
        s
    }
}

/// Append `.field` / `[idx]` segments to `s`; the leading `.` is elided
/// when `s` is empty (segments-only render).
fn push_segments_source(s: &mut String, segments: &[PathSeg]) {
    for seg in segments {
        match seg {
            PathSeg::Field(f) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(f);
            }
            PathSeg::Index(i) => {
                s.push('[');
                s.push_str(i);
                s.push(']');
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum PathSeg {
    /// `.field`
    Field(String),
    /// `[ident]` — subscript by a bound variable.
    Index(String),
}

/// Dotted qualified identifier with no subscripts — for state names, type
/// references in transition signatures. `State.Active` etc.
#[derive(Debug, Clone)]
pub struct QualifiedPath(pub Vec<String>);

// ============================================================================
// Guard / property expressions — the core win of the typed AST
// ============================================================================

/// Typed expression tree. Covers everything in the current `guard_*` pest
/// ladder plus explicit `Sum`, `Quantifier`, `Old`, and `Subscript` nodes.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer literal.
    Int(u128),
    /// Boolean / propositional literal. Rendered as Lean `True` / `False`
    /// (the propositional form, since guards elaborate against `Prop`).
    Bool(bool),
    /// A path with optional subscript segments: `state.accounts[i].capital`.
    Path(Path),
    /// `old(path_or_expr)` — only meaningful inside `ensures`.
    Old(Box<Node<Expr>>),
    /// `sum i : IdxType, body` — bounded aggregate.
    Sum {
        binder: String,
        binder_ty: String,
        body: Box<Node<Expr>>,
    },
    /// `forall i : T, body` or `exists i : T, body`.
    Quant {
        kind: Quantifier,
        binder: String,
        binder_ty: String,
        body: Box<Node<Expr>>,
    },
    /// Boolean op: and, or, implies.
    BoolOp {
        op: BoolOp,
        lhs: Box<Node<Expr>>,
        rhs: Box<Node<Expr>>,
    },
    /// `not e`.
    Not(Box<Node<Expr>>),
    /// Comparison: `==`, `!=`, `<=`, `>=`, `<`, `>`.
    Cmp {
        op: CmpOp,
        lhs: Box<Node<Expr>>,
        rhs: Box<Node<Expr>>,
    },
    /// Arithmetic: `+`, `-`, `*`, `/`, `%`.
    Arith {
        op: ArithOp,
        lhs: Box<Node<Expr>>,
        rhs: Box<Node<Expr>>,
    },
    /// Parenthesized sub-expression (preserved for pretty-printing; not
    /// semantically meaningful).
    Paren(Box<Node<Expr>>),
    /// `mul_div_floor(a, b, d)` — exact floor of `(a * b) / d` without
    /// intermediate overflow. Built-in because on integer VMs this is the
    /// canonical way to express any scaled multiplication (fixed-point),
    /// and users writing it by hand tend to get the widen-before-divide
    /// step wrong.
    MulDivFloor {
        a: Box<Node<Expr>>,
        b: Box<Node<Expr>>,
        d: Box<Node<Expr>>,
    },
    /// `mul_div_ceil(a, b, d)` — exact ceiling of `(a * b) / d`.
    MulDivCeil {
        a: Box<Node<Expr>>,
        b: Box<Node<Expr>>,
        d: Box<Node<Expr>>,
    },
    /// `mul_div_round_half_up(a, b, d)` — nearest integer to `(a * b) / d`,
    /// with exact half-way cases rounded upward. Intended for non-negative
    /// quantities with `d > 0`.
    MulDivRoundHalfUp {
        a: Box<Node<Expr>>,
        b: Box<Node<Expr>>,
        d: Box<Node<Expr>>,
    },
    /// `contains(coll, elem)` — collection membership. Rust `coll.contains(&elem)`,
    /// Lean `elem ∈ coll`. Produces a `Bool`. Built-in (not an `in` operator)
    /// because `in` is a reserved keyword. Used for `Vec` state fields
    /// (`contains(state.approved, signer)`) in requires/ensures.
    Contains {
        coll: Box<Node<Expr>>,
        elem: Box<Node<Expr>>,
    },
    /// `len(coll)` — collection length. Rust `coll.len()`, Lean `coll.length`.
    /// Produces a `Nat`. For `Vec` state fields (`len(state.approved) >=
    /// threshold`) in requires/ensures.
    Len(Box<Node<Expr>>),
    /// `exists x in coll, body` / `forall x in coll, body` — a bounded quantifier
    /// over a COLLECTION value (a `Vec` field), binding each element to `x` in
    /// `body`. Distinct from `Quant`, which quantifies over a TYPE domain
    /// (`Fin[N]` / small int). Rust: `coll.iter().any|all(|x| body)`; Lean:
    /// `∃|∀ x ∈ coll, body`. The vehicle for "some/every element of a collection
    /// satisfies P" — e.g. `exists c in state.post_hook_accounts, c == authority`.
    QuantIn {
        kind: Quantifier,
        binder: String,
        coll: Box<Node<Expr>>,
        body: Box<Node<Expr>>,
    },
    /// Inline `match scrutinee with | Variant binder => body | Variant => body`.
    /// Dispatches on a sum-typed scrutinee's constructor. `binder` is `Some`
    /// when the variant carries a payload and the arm wants to name it;
    /// field-level destructuring is not supported in phase 1 (use `binder.field`
    /// in the body). Bare variant names (no payload) use `None`.
    Match {
        scrutinee: Box<Node<Expr>>,
        arms: Vec<MatchExprArm>,
    },
    /// `.Variant` or `.Variant payload` — constructor application for a
    /// sum-typed value. Payload is a single expression (typically a
    /// record literal `{ f := v, ... }` or a record update `{ base with f := v }`,
    /// but any expression is grammatically allowed). Lean's elaborator
    /// resolves the variant's expected payload type.
    Ctor {
        variant: String,
        payload: Option<Box<Node<Expr>>>,
    },
    /// `{ field := expr, ... }` — anonymous record literal. Renders to Lean
    /// as `{ field := expr, ... }`; the expected structure type is resolved
    /// from context (typically as a constructor's payload).
    RecordLit(Vec<(String, Node<Expr>)>),
    /// `{ base with field := expr, ... }` — functional record update.
    /// Renders to Lean's native `{ base with ... }` syntax. Essential for
    /// concise handler bodies when operating on sum-typed records, so
    /// match arms don't have to reconstruct every field.
    RecordUpdate {
        base: Box<Node<Expr>>,
        updates: Vec<(String, Node<Expr>)>,
    },
    /// `x is .Variant` — constructor test yielding a Prop (True if `x` was
    /// built with `.Variant`, False otherwise). Desugars to a one-arm match
    /// during Lean rendering; lets handler guards write
    /// `requires accounts[i] is .Active else SlotInactive` instead of a
    /// verbose match boilerplate.
    IsVariant {
        scrutinee: Box<Node<Expr>>,
        variant: String,
    },
    /// `f(arg1, arg2, ...)` — function application. Renders to Lean as
    /// space-separated `f arg1 arg2` and to Rust as `f(arg1, arg2)`. The
    /// function name is left abstract: for spec-level helpers like
    /// `parent(n)` or `left(n)` in a tree invariant, downstream codegen
    /// declares them as uninterpreted symbols (axioms or Lean defs) in the
    /// support module. Zero-arg calls are rejected; bare identifiers parse
    /// as paths.
    App { func: String, args: Vec<Node<Expr>> },
    /// Postfix field access on an arbitrary expression — `e.field`.
    /// Enables chains like `left(n).key` where the base isn't a bare path.
    /// (Simple bare paths `a.b.c` still route to `Expr::Path`.)
    Field {
        base: Box<Node<Expr>>,
        field: String,
    },
    /// `let name = value in body` — ML-style expression-level binding.
    /// Derives a value once and references it by `name` in `body`. Lowers
    /// to Lean's `let name := value; body` and to a Rust block
    /// `{ let name = value; body }`.
    Let {
        name: String,
        value: Box<Node<Expr>>,
        body: Box<Node<Expr>>,
    },
    /// `if cond then a else b` — conditional in expression position. Lowers
    /// to Lean `if … then … else …` and a Rust `if` block. Branch types
    /// must agree — Lean's elaborator and rustc enforce it; qedgen just
    /// plumbs the structure through.
    IfThenElse {
        cond: Box<Node<Expr>>,
        then_branch: Box<Node<Expr>>,
        else_branch: Box<Node<Expr>>,
    },
}

#[derive(Debug, Clone)]
pub struct MatchExprArm {
    /// Constructor name the arm matches on (e.g. `Active`, `Inactive`).
    pub variant: String,
    /// Optional binder for the variant's payload. `Some("a")` means the arm
    /// body can reference fields via `a.capital` etc.; `None` for no-payload
    /// variants or arms that don't need the data.
    pub binder: Option<String>,
    pub body: Box<Node<Expr>>,
}

/// Visit every direct child expression of `expr` in source order (F2).
///
/// The single walker spine: recursive `Expr` traversals build on this and
/// keep only their interesting arms, so a new `Expr` variant is a compile
/// error *here* (the match is exhaustive by discipline — no `_` arm, same
/// contract as the MIR `Stmt` enum) instead of a silent miss in five
/// hand-rolled walkers.
pub fn for_each_child<'a>(expr: &'a Expr, mut f: impl FnMut(&'a Node<Expr>)) {
    for_each_child_with_binder(expr, |child, _| f(child));
}

/// Binder-aware variant of [`for_each_child`]: `f(child, binder)` where
/// `binder` is the name newly bound in scope for that child — quantifier /
/// sum binders, `let` names, match-arm payload binders — and `None` for
/// non-binding positions.
pub fn for_each_child_with_binder<'a>(
    expr: &'a Expr,
    mut f: impl FnMut(&'a Node<Expr>, Option<&'a str>),
) {
    match expr {
        // Leaves.
        Expr::Int(_) | Expr::Bool(_) | Expr::Path(_) => {}
        Expr::Old(inner) | Expr::Not(inner) | Expr::Paren(inner) => f(inner, None),
        Expr::Sum { binder, body, .. } | Expr::Quant { binder, body, .. } => f(body, Some(binder)),
        Expr::QuantIn {
            binder, coll, body, ..
        } => {
            f(coll, None);
            f(body, Some(binder));
        }
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => {
            f(lhs, None);
            f(rhs, None);
        }
        Expr::MulDivFloor { a, b, d }
        | Expr::MulDivCeil { a, b, d }
        | Expr::MulDivRoundHalfUp { a, b, d } => {
            f(a, None);
            f(b, None);
            f(d, None);
        }
        Expr::Contains { coll, elem } => {
            f(coll, None);
            f(elem, None);
        }
        Expr::Len(inner) => f(inner, None),
        Expr::Match { scrutinee, arms } => {
            f(scrutinee, None);
            for arm in arms {
                f(&arm.body, arm.binder.as_deref());
            }
        }
        Expr::Ctor { payload, .. } => {
            if let Some(p) = payload {
                f(p, None);
            }
        }
        Expr::RecordLit(fields) => {
            for (_, v) in fields {
                f(v, None);
            }
        }
        Expr::RecordUpdate { base, updates } => {
            f(base, None);
            for (_, v) in updates {
                f(v, None);
            }
        }
        Expr::IsVariant { scrutinee, .. } => f(scrutinee, None),
        Expr::App { args, .. } => {
            for arg in args {
                f(arg, None);
            }
        }
        Expr::Field { base, .. } => f(base, None),
        Expr::Let { name, value, body } => {
            f(value, None);
            f(body, Some(name));
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            f(cond, None);
            f(then_branch, None);
            f(else_branch, None);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantifier {
    Forall,
    Exists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
    Implies,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Le,
    Ge,
    Lt,
    Gt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

// ============================================================================
// Properties, covers, liveness, invariants
// ============================================================================

#[derive(Debug, Clone)]
pub struct PropertyDecl {
    pub name: String,
    pub doc: Option<String>,
    pub body: Node<Expr>,
    pub preserved_by: PreservedBy,
}

#[derive(Debug, Clone)]
pub enum PreservedBy {
    All,
    Some(Vec<String>),
    /// `preserved_by all except [h1, h2, ...]` — the adapter expands this
    /// against the full handler list into a concrete `Some(Vec<String>)`.
    AllExcept(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct CoverDecl {
    pub name: String,
    /// Simple `cover name [a, b, c]` is a single-trace cover.
    pub traces: Vec<Vec<String>>,
    /// `reachable foo when expr` clauses from block-form covers.
    pub reachable: Vec<(String, Option<Node<Expr>>)>,
}

#[derive(Debug, Clone)]
pub struct LivenessDecl {
    pub name: String,
    pub from_state: QualifiedPath,
    pub to_state: QualifiedPath,
    pub via: Vec<String>,
    pub within: u64,
}

#[derive(Debug, Clone)]
pub struct InvariantDecl {
    pub name: String,
    pub body: InvariantBody,
}

#[derive(Debug, Clone)]
pub enum InvariantBody {
    Expr(Node<Expr>),
    Description(String),
}

#[derive(Debug, Clone)]
pub struct PdaDecl {
    pub name: String,
    /// Seeds: either a literal string or an identifier reference.
    pub seeds: Vec<PdaSeed>,
}

#[derive(Debug, Clone)]
pub enum PdaSeed {
    Literal(String),
    Ident(String),
}

#[derive(Debug, Clone)]
pub struct EventDecl {
    pub name: String,
    pub fields: Vec<TypedField>,
}

#[derive(Debug, Clone)]
pub struct EnvironmentDecl {
    pub name: String,
    pub clauses: Vec<Node<EnvClause>>,
}

#[derive(Debug, Clone)]
pub enum EnvClause {
    /// `mutates field : Type` — field that mutates externally.
    Mutates { field: String, ty: String },
    /// `external clock.slot : U64` — a typed field in a namespace distinct
    /// from program state. Constraints relate pre/post values with `old(...)`.
    External {
        object: String,
        field: String,
        ty: TypeRef,
    },
    /// `constraint expr` — constraint relating pre/post values.
    Constraint(Node<Expr>),
}
