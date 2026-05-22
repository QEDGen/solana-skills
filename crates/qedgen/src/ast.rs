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
//!
//! NOTE on `#![allow(dead_code)]`: several AST fields (`span`, variant
//! payloads like `TypeRef::Param`, `MatchBody::Noop`, doc strings) are parsed
//! and carried through the typed form but not yet consumed by every downstream
//! adapter/backend. They are intentional scaffolding for the pest→chumsky
//! migration and for planned diagnostics (span-based error reporting, Param
//! types like `Vec U64` in handler signatures, doc preservation in generated
//! artifacts). Removing them would lose information the parser already
//! recovers. Revisit per-field once the corresponding backend consumer lands.

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
    /// `program_id "..."` — explicit program ID.
    ProgramId(String),
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
    /// `interface Name { program_id "...", upstream { ... }, handler h(args) { ... } }`
    /// Declares a callee's public contract so a caller can `call Name.h(...)`
    /// with backend-appropriate artifacts. See docs/design/spec-composition.md §2.
    Interface(InterfaceDecl),
    /// `import Name from "key"` — bind a local name to an interface declared in
    /// a dependency. The `from` string is a key into `qed.toml`'s
    /// `[dependencies]` table; the resolver fetches the source (github / path)
    /// and merges the imported `interface` declarations into this spec's
    /// namespace under the given local `name`. See docs/design/spec-composition.md §3.
    Import {
        name: String,
        from: String,
        /// v2.8 fold-in F5: optional `as Alias` clause renames the
        /// imported interface in the consumer's namespace.
        as_name: Option<String>,
    },
    /// `pragma <name> { <top_item>* }` — platform-specific namespace.
    ///
    /// Keeps the core DSL platform-agnostic while letting target-specific
    /// constructs (sBPF `instruction`/`pubkey`/layouts, eventually Anchor
    /// or Quasar extensions) live in clearly scoped blocks. Target
    /// inference reads `ParsedSpec.pragmas` — presence of `sbpf` selects
    /// the assembly target with no explicit `target` keyword.
    Pragma(PragmaDecl),
    /// `pragma <name> = <ident>` — top-level key=value assignment. v2.24
    /// introduces this for `checked_overflow_error = MintOverflow` /
    /// `checked_underflow_error = BurnUnderflow`, which override the
    /// built-in `MathOverflow` / `MathUnderflow` defaults that
    /// `mechanize_effect` lowers `+=` / `-=` against. Per-effect `or
    /// <Variant>` (see `EffectStmt.on_error`) still wins over the pragma.
    PragmaAssign {
        name: String,
        value: String,
    },
    /// `schema name { requires expr else Err … }` — v2.24 #1. Reusable
    /// cross-cutting guard set. Handlers reference it via `uses name`
    /// (HandlerClause::Uses) and the adapter expands every requires
    /// in the schema into the handler's requires list.
    Schema(SchemaDecl),
    /// `ref_impl name (state : State) (params...) : T = <expr>` — v2.25
    /// reference implementation. Names an intermediate expression that
    /// `ensures` clauses can call. Lowers to a Lean `def` and inlines
    /// at Kani-harness assertion sites; Rust codegen skips it entirely
    /// (it's a verification-only construct, not part of the impl
    /// contract). Replaces the original `ghost` proposal — the user
    /// preferred the more honest naming, since the construct *is*
    /// a reference implementation the real Rust impl is checked against.
    RefImpl(RefImplDecl),
}

/// v2.24 #1 — top-level `schema` block. Body carries a list of
/// `requires expr else Err` clauses. No state effects or ensures —
/// schemas are *only* for cross-cutting guards.
#[derive(Debug, Clone)]
pub struct SchemaDecl {
    pub name: String,
    pub doc: Option<String>,
    /// Each entry is one `requires <expr> [else <ErrorName>]` clause,
    /// keeping the same `(body, on_fail)` shape that
    /// `HandlerClause::Requires` carries so the adapter can reuse
    /// the same lowering path.
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

/// v2.25 — `ref_impl name (p1 : T1) (p2 : T2) : R = <expr>`.
/// Reference implementation that ensures clauses can call. The body
/// is a pure expression over the typed parameters; no state mutation,
/// no side effects.
#[derive(Debug, Clone)]
pub struct RefImplDecl {
    pub name: String,
    pub doc: Option<String>,
    pub params: Vec<TypedField>,
    pub return_type: TypeRef,
    pub body: Node<Expr>,
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
    /// v2.20 §S1.2 — effect body items can be unconditional statements
    /// or `match` blocks.
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
    Invariant(String),
    /// `establishes Name` — this handler establishes the named invariant
    /// at post-state. Unlike `invariant Name` (which means "preserves"),
    /// the harness/proof does NOT assume the invariant holds pre-transition.
    /// Use for handlers that bring the system from an uninitialized state
    /// into one where the invariant becomes true, or for one-shot
    /// transitions that elevate an invariant after the fact.
    Establishes(String),
    /// `permissionless` — marks the handler as deliberately-unauthenticated.
    /// Opts out of the `no_access_control` P1 lint (v2.7 G4). Mutually
    /// exclusive with `auth X`; check.rs rejects both appearing together.
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
    /// v2.24 #11 — optional `let <name> = call …` binding. When `Some`,
    /// the call's return value is bound to the given identifier so
    /// downstream effects / requires can reference it. The interface
    /// handler's return-type declaration is what gives the binding a
    /// real semantics; without it the binding is opaque.
    pub result_binding: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CallArg {
    pub name: String,
    pub value: Node<Expr>,
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
    /// v2.24 #11 — optional `-> Type` return-type declaration. When
    /// `Some`, callers can write `let x = call Foo.handler(...)` and
    /// the codegen lowers to `let x = T::try_from_slice(&ret_bytes)?`
    /// after the CPI via Solana's `get_return_data` syscall. `None`
    /// keeps the existing terminal-statement shape.
    pub return_type: Option<TypeRef>,
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
    /// v2.24 #9 — `call Interface.handler(...)` — case maps to a
    /// synthetic handler that issues this CPI (and nothing else).
    /// Used for outcome-conditional CPI patterns where the match
    /// arm picks between different external calls instead of
    /// different state mutations. Optional effect block alongside
    /// the call for cases that do both.
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
    /// v2.24 §S1a — per-site error-variant override on checked `+=` / `-=`.
    /// `pool += amount else MintOverflow` parses with `on_error =
    /// Some("MintOverflow")`. The keyword is `else` (same as `requires X
    /// else Err`), not `or` — `or` would collide with the boolean infix
    /// `or` already parsed by `expr()`. None means "fall back to the
    /// `pragma checked_overflow_error` / `pragma checked_underflow_error`
    /// default, then the built-in `MathOverflow` / `MathUnderflow`."
    /// Always None for saturating (`+=!`) / wrapping (`+=?`) / `Set` —
    /// those can't fail, so the adapter drops any override even if the
    /// parser captured one.
    pub on_error: Option<String>,
}

/// v2.20 §S1.2 — a single statement inside `effect { … }`. Either a leaf
/// `EffectStmt` (the historical unconditional form, `x += y`) or a
/// `match`-shape conditional that branches over a scrutinee expression.
#[derive(Debug, Clone)]
pub enum EffectBlock {
    /// Unconditional effect statement — the only form pre-v2.20.
    Stmt(EffectStmt),
    /// `match <scrutinee> { 0 => <effect>, 1 => <effect>, _ => <effect> }`.
    Match {
        scrutinee: Node<Expr>,
        arms: Vec<EffectMatchArm>,
    },
}

/// One arm of an effect-level `match`. v2.20 supports literal-integer
/// patterns and a `_` wildcard.
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

/// Per-effect arithmetic semantics. v2.7 G3 introduced the distinction —
/// prior versions always lowered `+=` to wrapping in the transition model,
/// but that produced false-positive overflow hits in Kani for specs whose
/// deployed implementation uses `checked_add`.
///
/// - `Add` / `Sub` = **checked** (default; matches deployed Anchor
///   `checked_add(..).ok_or(err)?` — overflow short-circuits the transition).
/// - `AddSat` / `SubSat` = **saturating** (`pool +=! net`) — clamps to
///   `{u8,u64,…}::MAX` or `::MIN` on over/underflow.
/// - `AddWrap` / `SubWrap` = **wrapping** (`pool +=? net`) — the pre-v2.7
///   default; still valid opt-in for specs that deliberately use modular
///   arithmetic.
/// - `Set` = assignment (`:=` or `=`).
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
    /// `if cond then a else b` — full conditional in expression position
    /// (v2.8 fold-in F9). Lowers to Lean's `if … then … else …` and to a
    /// Rust `if … { … } else { … }` block. Both branches must produce a
    /// value of the same type — Lean's elaborator and Rust's type checker
    /// enforce this; qedgen just plumbs the structure through.
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
    /// v2.24 #3 — `preserved_by all except [h1, h2, ...]` shorthand
    /// for "every handler other than the listed ones". The adapter
    /// expands this against the spec's full handler list, producing
    /// a concrete `Some(Vec<String>)` for downstream consumers.
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
    /// `constraint expr` — constraint relating pre/post values.
    Constraint(Node<Expr>),
}
