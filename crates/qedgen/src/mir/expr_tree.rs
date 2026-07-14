//! Typed expression tree — #151 Slice 0.
//!
//! `ExprTree` is the structural carrier for every expression-shaped value in
//! MIR, replacing (incrementally, slices 1–4) the six pre-rendered string
//! fields on [`super::Expr`]. It mirrors the surface AST
//! (`crate::ast::Expr`) *post-canonicalization and post-desugar*:
//!
//! * no `Paren` nodes — grouping is structural;
//! * bare state-field reads are already `state.`-rooted (the issue-#139
//!   canonicalization seam in `chumsky_adapter::canon` runs before tree
//!   construction);
//! * every `Path` is **name-resolved** to a [`BindingKind`] against the
//!   handler's scope at adapt time, so renderers never guess a receiver;
//! * numeric leaves carry their resolved [`Ty`] where the type environment
//!   knows it, so widening/narrowing decisions read real types instead of
//!   `Kind` heuristics.
//!
//! **Exhaustiveness discipline (#66, extended to expressions):** matches
//! over `ExprTree` and `BindingKind` must list every variant explicitly —
//! no `_` arms. A new expression shape must break every consumer at compile
//! time; that failure mode is the point of the closed enum (see the #139 /
//! #143–#146 / B12 bug class this replaces).
//!
//! During the port the tree rides alongside the string fields
//! (`Expr.tree`); Slice 4 deletes the strings.

use super::{Symbol, Ty};

/// The Rust shape of an enum variant, for rendering an `is .Variant` test as a
/// shape-correct `matches!` pattern: `Enum::V` (Unit), `Enum::V(..)` (Tuple —
/// e.g. `Custom(i64)`), `Enum::V { .. }` (Struct — e.g. `Approved { timestamp }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantShape {
    Unit,
    Tuple,
    Struct,
}

impl VariantShape {
    /// The Rust `matches!` sub-pattern for `<enum>::<variant>` at this shape:
    /// `Enum::V` (Unit), `Enum::V(..)` (Tuple), `Enum::V { .. }` (Struct).
    pub fn match_pattern(self, enum_name: &str, variant: &str) -> String {
        self.arm_pattern(enum_name, variant, None)
    }

    /// A `match`-arm pattern for `<enum>::<variant>`, binding a tuple payload to
    /// `binder` when given: `Enum::V` (Unit), `Enum::V(b)` / `Enum::V(..)`
    /// (Tuple, with/without a binder), `Enum::V { .. }` (Struct — a binder isn't
    /// yet threaded into struct-variant field access).
    pub fn arm_pattern(self, enum_name: &str, variant: &str, binder: Option<&str>) -> String {
        match self {
            VariantShape::Unit => format!("{enum_name}::{variant}"),
            VariantShape::Tuple => match binder {
                Some(b) => format!("{enum_name}::{variant}({b})"),
                None => format!("{enum_name}::{variant}(..)"),
            },
            VariantShape::Struct => format!("{enum_name}::{variant} {{ .. }}"),
        }
    }
}

/// Closed expression tree. See module docs for invariants.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprTree {
    /// Integer literal. Untyped at the leaf — the surrounding context
    /// (assignment target, comparison peer) decides the width; kind
    /// inference defaults to `Nat`.
    Int(u128),
    /// Boolean literal.
    Bool(bool),
    /// Name-resolved path — see [`TreePath`].
    Path(TreePath),
    /// `old(e)` — pre-state read; only meaningful in two-state contexts
    /// (`ensures`, binary properties). Renderer context picks the receiver.
    Old(Box<ExprTree>),
    /// `sum i : IdxType, body` — bounded aggregate.
    Sum {
        binder: Symbol,
        /// Binder type in source-DSL form (`AccountIdx`, `Fin[N]`, …).
        binder_ty: Symbol,
        /// Resolved finite bound for Rust/Kani/proptest lowering. `None`
        /// keeps non-finite sums explicit and unsupported.
        fin_bound: Option<Symbol>,
        body: Box<ExprTree>,
    },
    /// `forall i : T, body` / `exists i : T, body`.
    Quant {
        kind: QuantKind,
        binder: Symbol,
        /// Binder type in source-DSL form.
        binder_ty: Symbol,
        /// Resolved `Fin[N]` bound when `binder_ty` is a bounded index
        /// domain (directly or via a type alias): the `N` symbol — a
        /// numeric literal or a `const` name. Resolved at build time so
        /// renderers don't need the alias table.
        fin_bound: Option<Symbol>,
        body: Box<ExprTree>,
    },
    /// `exists|forall x in coll, body` — bounded quantifier over a COLLECTION
    /// value (a `Vec`), binding each element to `x`. Rust `coll.iter().any|all`,
    /// Lean `∃|∀ x ∈ coll`.
    QuantIn {
        kind: QuantKind,
        binder: Symbol,
        coll: Box<ExprTree>,
        body: Box<ExprTree>,
    },
    BoolOp {
        op: TreeBoolOp,
        lhs: Box<ExprTree>,
        rhs: Box<ExprTree>,
    },
    Not(Box<ExprTree>),
    Cmp {
        op: TreeCmpOp,
        lhs: Box<ExprTree>,
        rhs: Box<ExprTree>,
    },
    Arith {
        op: TreeArithOp,
        lhs: Box<ExprTree>,
        rhs: Box<ExprTree>,
    },
    /// `mul_div_floor(a, b, d)` — exact floor of `(a * b) / d` without
    /// intermediate overflow (u128-widened helpers downstream).
    MulDivFloor {
        a: Box<ExprTree>,
        b: Box<ExprTree>,
        d: Box<ExprTree>,
    },
    /// `mul_div_ceil(a, b, d)`.
    MulDivCeil {
        a: Box<ExprTree>,
        b: Box<ExprTree>,
        d: Box<ExprTree>,
    },
    /// `mul_div_round_half_up(a, b, d)` — nearest, ties upward.
    MulDivRoundHalfUp {
        a: Box<ExprTree>,
        b: Box<ExprTree>,
        d: Box<ExprTree>,
    },
    /// `contains(coll, elem)` — collection membership. Rust `coll.contains(&elem)`,
    /// Lean `elem ∈ coll`. Produces a `Bool`.
    Contains {
        coll: Box<ExprTree>,
        elem: Box<ExprTree>,
    },
    /// `len(coll)` — collection length. Rust `coll.len()`, Lean `coll.length`.
    /// Produces a `Nat`.
    Len(Box<ExprTree>),
    /// `match scrutinee with | Variant binder => body | …`.
    Match {
        scrutinee: Box<ExprTree>,
        arms: Vec<TreeMatchArm>,
        /// Resolved enum type name of the scrutinee, for shape-correct arm
        /// patterns (`RustCx` has no `TypeEnv` at emission time). `None` when
        /// unresolvable; the renderer falls back to the scrutinee Path leaf `Ty`.
        enum_ty: Option<Symbol>,
    },
    /// `.Variant` / `.Variant payload` — sum-type constructor application.
    Ctor {
        variant: Symbol,
        payload: Option<Box<ExprTree>>,
    },
    /// `{ field := expr, … }` — anonymous record literal.
    RecordLit(Vec<(Symbol, ExprTree)>),
    /// `{ base with field := expr, … }` — functional record update.
    RecordUpdate {
        base: Box<ExprTree>,
        updates: Vec<(Symbol, ExprTree)>,
    },
    /// `x is .Variant` — constructor test.
    IsVariant {
        scrutinee: Box<ExprTree>,
        variant: Symbol,
        /// Resolved enum type name, for shape-correct Rust `matches!`
        /// rendering (`RustCx` has no `TypeEnv` at emission time, so the
        /// name is resolved once at build time and carried here). `None`
        /// when unresolvable; the renderer then falls back to the scrutinee
        /// Path leaf `Ty`.
        enum_ty: Option<Symbol>,
        /// The variant's Rust shape → its `matches!` pattern (`Enum::V` /
        /// `Enum::V(..)` / `Enum::V { .. }`). Resolved from the sum-type
        /// registry at build time.
        shape: VariantShape,
    },
    /// `f(arg1, …)` — application of a spec-level helper (uninterpreted or
    /// `ref_impl`) or a builtin (`now()`, `current_epoch()`).
    App {
        func: Symbol,
        args: Vec<ExprTree>,
    },
    /// Postfix field access on a non-path base — `left(n).key`.
    Field {
        base: Box<ExprTree>,
        field: Symbol,
    },
    /// `let name = value in body`.
    Let {
        name: Symbol,
        value: Box<ExprTree>,
        body: Box<ExprTree>,
    },
    /// `if cond then a else b`.
    IfThenElse {
        cond: Box<ExprTree>,
        then_branch: Box<ExprTree>,
        else_branch: Box<ExprTree>,
    },
}

/// A name-resolved path. `root` preserves the source spelling of the head
/// identifier (`"state"` for state-rooted paths, the bare name otherwise);
/// renderers must dispatch on `binding`, never string-compare `root`.
#[derive(Debug, Clone, PartialEq)]
pub struct TreePath {
    pub root: Symbol,
    pub binding: BindingKind,
    /// Segments after the root. For `BindingKind::StateField` /
    /// `BindingKind::Ghost` the first segment is the state-field name
    /// (`state.accounts[i].capital` → `[Field(accounts), Index(i),
    /// Field(capital)]`).
    pub segments: Vec<TreeSeg>,
    /// Resolved leaf type when the type environment knows it (state fields,
    /// record fields through `Map` subscripts, handler params). `None` for
    /// account references, expression binders, and unresolved names.
    pub ty: Option<Ty>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TreeSeg {
    /// `.field`
    Field(Symbol),
    /// `[ident]` — subscript by a bound variable or param name.
    Index(Symbol),
}

/// What a path's root name resolved to in the enclosing scope. Resolution
/// happens once, at adapt time, against the handler's scope + the spec's
/// type environment — the enrichment that lets renderers pick receivers
/// (`s.` / `post.` / `ctx.state.`) structurally.
#[derive(Debug, Clone, PartialEq)]
pub enum BindingKind {
    /// Program-state field read (`state.`-rooted after canonicalization).
    StateField,
    /// A `ghost` spec-only field — reads like a state field on the
    /// verification backends, invisible to on-chain codegen.
    Ghost,
    /// Handler parameter (including legacy `takes` fields).
    Param,
    /// Top-level `const` (or integer-max builtin); carries the resolved
    /// literal value so renderers can substitute without a table.
    Const(String),
    /// Handler-level `let` binding.
    LetBound,
    /// Account binding from the handler's `accounts { … }` block (or the
    /// `auth` actor).
    Account,
    /// Field in a declared external namespace (`clock.slot`, `oracle.price`).
    External,
    /// `abstract <name> : <Type>` existential binder.
    AbstractBinder,
    /// Bound by an enclosing expression-level binder (quantifier, `sum`,
    /// `let … in`, match-arm payload).
    ExprBinder,
    /// Not resolvable in the current scope (foreign namespaces, interface
    /// result binders, helper names in odd positions). Renderers emit the
    /// source spelling verbatim.
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantKind {
    Forall,
    Exists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeBoolOp {
    And,
    Or,
    Implies,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeCmpOp {
    Eq,
    Ne,
    Le,
    Ge,
    Lt,
    Gt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeMatchArm {
    /// Constructor name the arm matches on; `"_"` = the catch-all wildcard.
    pub variant: Symbol,
    /// Optional binder for the variant's payload (bound in the arm body).
    pub binder: Option<Symbol>,
    pub body: Box<ExprTree>,
    /// The variant's Rust shape, for the arm's pattern (`Unit` for `"_"`).
    pub shape: VariantShape,
}

/// Numeric kind for operator-coercion decisions — the tree-native
/// replacement for `chumsky_adapter::Kind`. Unsigned widths collapse to
/// `Nat`, signed to `Int`; `Bool` and `Other` don't participate in
/// arithmetic promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumKind {
    Nat,
    Int,
    Bool,
    Other,
}

/// `Ty` → `NumKind`. `Custom` names that the DSL accepts but `Ty` doesn't
/// model natively (`I8`/`I16`/`I32`, `Fin[N]`) classify here so kind
/// inference matches `TypeEnv::type_ref_kind`.
pub fn ty_num_kind(ty: &Ty) -> NumKind {
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => NumKind::Nat,
        Ty::I64 | Ty::I128 => NumKind::Int,
        Ty::Bool => NumKind::Bool,
        Ty::Pubkey | Ty::Bytes32 | Ty::Bytes64 => NumKind::Other,
        Ty::Map { .. } => NumKind::Other,
        Ty::Custom(name) => match name.as_str() {
            "I8" | "I16" | "I32" => NumKind::Int,
            n if n.starts_with("Fin[") => NumKind::Nat,
            _ => NumKind::Other,
        },
    }
}

impl TreePath {
    /// Kind of the value this path reads. Mirrors `TypeEnv::path_kind`'s
    /// conventions: unknown leaves default to `Nat` (the conservative
    /// choice the existing codegen was written against), negative consts
    /// classify `Int`.
    pub fn num_kind(&self) -> NumKind {
        if let Some(ty) = &self.ty {
            return ty_num_kind(ty);
        }
        match &self.binding {
            BindingKind::Const(value) => {
                if value.trim_start().starts_with('-') {
                    NumKind::Int
                } else {
                    NumKind::Nat
                }
            }
            BindingKind::StateField
            | BindingKind::Ghost
            | BindingKind::Param
            | BindingKind::LetBound
            | BindingKind::Account
            | BindingKind::External
            | BindingKind::AbstractBinder
            | BindingKind::ExprBinder
            | BindingKind::Unresolved => NumKind::Nat,
        }
    }
}

impl ExprTree {
    /// Kind inference over the resolved tree — the drop-in replacement for
    /// `TypeEnv::infer` (renderers stop needing the type environment).
    /// `Int` dominates `Nat` in arithmetic joins; unknowns stay `Nat`.
    pub fn num_kind(&self) -> NumKind {
        fn join(l: &ExprTree, r: &ExprTree) -> NumKind {
            match (l.num_kind(), r.num_kind()) {
                (NumKind::Int, _) | (_, NumKind::Int) => NumKind::Int,
                _ => NumKind::Nat,
            }
        }
        match self {
            ExprTree::Int(_) => NumKind::Nat,
            ExprTree::Bool(_) => NumKind::Bool,
            ExprTree::Path(p) => p.num_kind(),
            ExprTree::Old(inner) => inner.num_kind(),
            ExprTree::Sum { body, .. } => body.num_kind(),
            ExprTree::Quant { .. } => NumKind::Bool,
            ExprTree::QuantIn { .. } => NumKind::Bool,
            ExprTree::BoolOp { .. } => NumKind::Bool,
            ExprTree::Not(_) => NumKind::Bool,
            ExprTree::Cmp { .. } => NumKind::Bool,
            ExprTree::Contains { .. } => NumKind::Bool,
            ExprTree::Len(_) => NumKind::Nat,
            ExprTree::Arith { lhs, rhs, .. } => join(lhs, rhs),
            // Divisor kind doesn't promote — it's a scale.
            ExprTree::MulDivFloor { a, b, .. }
            | ExprTree::MulDivCeil { a, b, .. }
            | ExprTree::MulDivRoundHalfUp { a, b, .. } => join(a, b),
            // First arm's body decides; arms must agree (elaborator-checked).
            ExprTree::Match { arms, .. } => arms
                .first()
                .map(|arm| arm.body.num_kind())
                .unwrap_or(NumKind::Other),
            ExprTree::Ctor { .. } => NumKind::Other,
            ExprTree::RecordLit(_) => NumKind::Other,
            ExprTree::RecordUpdate { base, .. } => base.num_kind(),
            ExprTree::IsVariant { .. } => NumKind::Bool,
            ExprTree::App { .. } => NumKind::Other,
            ExprTree::Field { .. } => NumKind::Other,
            ExprTree::Let { body, .. } => body.num_kind(),
            ExprTree::IfThenElse { then_branch, .. } => then_branch.num_kind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path(field: &str, ty: Ty) -> ExprTree {
        ExprTree::Path(TreePath {
            root: "state".to_string(),
            binding: BindingKind::StateField,
            segments: vec![TreeSeg::Field(field.to_string())],
            ty: Some(ty),
        })
    }

    #[test]
    fn num_kind_int_dominates_nat() {
        let e = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(state_path("capital", Ty::U128)),
            rhs: Box::new(state_path("pnl", Ty::I128)),
        };
        assert_eq!(e.num_kind(), NumKind::Int);
    }

    #[test]
    fn num_kind_predicates_are_bool() {
        let cmp = ExprTree::Cmp {
            op: TreeCmpOp::Le,
            lhs: Box::new(ExprTree::Int(1)),
            rhs: Box::new(ExprTree::Int(2)),
        };
        assert_eq!(cmp.num_kind(), NumKind::Bool);
        let q = ExprTree::Quant {
            kind: QuantKind::Forall,
            binder: "i".into(),
            binder_ty: "U8".into(),
            fin_bound: None,
            body: Box::new(ExprTree::Bool(true)),
        };
        assert_eq!(q.num_kind(), NumKind::Bool);
    }

    #[test]
    fn num_kind_negative_const_is_int() {
        let p = TreePath {
            root: "MIN_PNL".to_string(),
            binding: BindingKind::Const("-100".to_string()),
            segments: vec![],
            ty: None,
        };
        assert_eq!(p.num_kind(), NumKind::Int);
        let p2 = TreePath {
            root: "MAX".to_string(),
            binding: BindingKind::Const("100".to_string()),
            segments: vec![],
            ty: None,
        };
        assert_eq!(p2.num_kind(), NumKind::Nat);
    }

    #[test]
    fn ty_num_kind_covers_dsl_only_names() {
        assert_eq!(ty_num_kind(&Ty::Custom("I16".into())), NumKind::Int);
        assert_eq!(ty_num_kind(&Ty::Custom("Fin[8]".into())), NumKind::Nat);
        assert_eq!(ty_num_kind(&Ty::Custom("Snapshot".into())), NumKind::Other);
    }
}
