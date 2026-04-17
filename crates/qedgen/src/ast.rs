//! Typed AST for `.qedspec` files.
//!
//! This replaces the string-rendered `ParsedSpec` IR as we migrate from pest
//! to chumsky. The typed AST is the intermediate form produced by the new
//! parser; an adapter translates it into the legacy `ParsedSpec` so downstream
//! consumers (check, lean_gen, kani, …) don't change during the transition.
//!
//! Design goals:
//!   - Guard expressions are a real algebraic type — not pre-rendered strings.
//!     This is the single enabler for scope checking, exhaustiveness,
//!     match-in-handler, and cheaper target-specific codegen.
//!   - Every node carries a `Span` so diagnostics can point at source.
//!   - No backend concerns leak in: no Lean unicode, no Rust identifiers.
//!
//! Scope of this file:
//!   - Core declarative constructs used by percolator.qedspec:
//!     records, ADTs, handlers, properties, covers, liveness.
//!   - Subset deliberately omitted in phase 1 (pest still handles these):
//!     sBPF instruction blocks, schemas, environments, PDAs, events.

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
    Const {
        name: String,
        value: u128,
    },
    /// `type T = { f : Type, ... }` — plain record.
    Record(RecordDecl),
    /// `type State | Active of { ... } | Draining | ...` — ADT.
    Adt(AdtDecl),
    /// `type Error | Foo | Bar = 42 "desc"` — flat enum for error codes.
    /// Represented as an ADT with variants with no payload; the name
    /// "Error" is conventional.
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
    /// `target assembly` or `target quasar` — codegen target hint.
    Target(String),
    /// `program_id "..."` — explicit program ID.
    ProgramId(String),
    /// `assembly "..."` — sBPF assembly file path.
    Assembly(String),
    /// `type Name = <type_ref>` — type alias, expands to its target.
    TypeAlias(TypeAliasDecl),
    /// `pubkey NAME [u64, u64, u64, u64]` — 4-chunk U64 pubkey literal (sBPF sugar).
    Pubkey(PubkeyDecl),
    /// `errors [Name = code "desc", ...]` — top-level error list (sBPF sugar,
    /// equivalent to `type Error | Name = code "desc" | ...`).
    Errors(Vec<ErrorEntry>),
    /// `instruction Name { ... }` — sBPF instruction block with layouts,
    /// guards, and properties.
    Instruction(InstructionDecl),
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
    /// `const NAME = VALUE` — instruction-local constant.
    Const { name: String, value: u128 },
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
    Effect(Vec<Node<EffectStmt>>),
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
    Invariant(String),
    /// `include schema_name` — forward-compat; phase 1 rejects.
    Include(String),
}

#[derive(Debug, Clone)]
pub struct AccountDescriptor {
    pub name: String,
    pub attrs: Vec<AccountAttr>,
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
}

#[derive(Debug, Clone)]
pub enum TransferAmount {
    Literal(u128),
    Path(Path),
}

#[derive(Debug, Clone)]
pub enum AccountAttr {
    Simple(String),     // signer, writable, readonly, program, token
    Type(String),       // `type token`
    Authority(String),  // `authority x`
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectOp {
    /// `+=`
    Add,
    /// `-=`
    Sub,
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
    /// `constraint expr` — constraint relating pre/post values.
    Constraint(Node<Expr>),
}
