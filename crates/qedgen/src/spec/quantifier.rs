//! Quantifier-shape classifier for property bodies.
//!
//! v2.20 §S1.1 — `forall d : T, P(d)` must lower to a Kani / proptest harness
//! that actually exercises the post-state, not the silent `true` stub that
//! caused 88 of 106 Kani harnesses to verify vacuously in the v2.19 audits.
//!
//! `supported_shape` walks the property body and either returns the
//! information codegen needs to emit a non-vacuous harness, or a precise
//! reason why the shape can't be mechanically lowered. Reasons feed the P5
//! lint in `check.rs`.
//!
//! The classifier is intentionally narrow: anything beyond a single-binder
//! `forall` over a primitive or named ADT type returns `Err(Reason::...)`.
//! Broader shapes (nested quantifiers, `Vec<T>` of unbounded length, exists)
//! either get split by the user into multiple single-binder properties (per
//! `docs/limitations.md`) or wait for a future release.

use crate::ast::{Expr, Node, PropertyDecl, Quantifier, Span};

/// Classifier output: a property either has no quantifier (the legacy
/// `state.field >= 0` shape), or has a single supported binder shape we can
/// lower to a real harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// Body is `P(s_post)` — no binder. The harness asserts `P(&s)` after the
    /// transition; codegen unchanged from the legacy path.
    NoQuantifier,
    /// Body is `forall <binder> : <ty>, inner(<binder>, s_post)`. The
    /// harness binds `<binder>` symbolically (kani::any / proptest any) and
    /// asserts `inner(&<binder>, &s_post)` after the transition.
    SingleBinderForall {
        binder: String,
        binder_ty: String,
        /// Span of the outer `forall …` for diagnostics.
        span: Span,
    },
}

/// Why a property's quantifier shape can't be lowered to a non-vacuous
/// harness. Each variant carries a span so `qedgen check` can point at the
/// exact source token. The string descriptions are intentionally human-
/// readable; they feed the P5 lint message verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Quantifier nests inside another quantifier (e.g. `forall i : A,
    /// forall j : B, …`). Splitting into two single-binder properties is the
    /// supported workaround.
    NestedQuantifier { outer: Span, inner: Span },
    /// Binder type can't be enumerated by either `kani::any::<T>()` or a
    /// proptest `any::<T>()` strategy because it's an unbounded collection
    /// (`Vec<T>`, `List<T>`, etc.). Bounded `Map[N] T` would be supported
    /// but the spec grammar uses it as a state-field type, not a binder.
    UnboundedBinderType { ty: String, span: Span },
    /// `exists` quantifier. A *bounded* `exists` (binder is a `Fin[N]`
    /// index domain, directly or via an alias) lowers to a real
    /// `(0..N).any(…)` harness predicate — the chumsky_adapter resolves
    /// the alias, renders that, and suppresses this reason. This variant
    /// only survives for an *unbounded* `exists` (e.g. over `U64`), which
    /// can't be enumerated in a test loop. The classifier can't resolve
    /// aliases on its own, so it conservatively reports every `exists`
    /// here and lets the adapter clear it when the body lowered cleanly.
    ExistsQuantifier { span: Span },
}

impl Reason {
    /// Human-readable message for the P5 lint and `docs/limitations.md`
    /// cross-references.
    pub fn message(&self) -> String {
        match self {
            Reason::NestedQuantifier { .. } => {
                "nested quantifier — split into two single-binder properties".to_string()
            }
            Reason::UnboundedBinderType { ty, .. } => format!(
                "binder type `{}` can't be enumerated by Kani / proptest — bound the range or split the property",
                ty
            ),
            Reason::ExistsQuantifier { .. } => {
                "`exists` over an unbounded domain — bound the binder with a \
                 `Fin[N]` index type (or `U8`) so it lowers to `(0..N).any(…)`"
                    .to_string()
            }
        }
    }

    /// Anchor span for diagnostics — points at the offending quantifier
    /// token, not the whole property.
    pub fn span(&self) -> Span {
        match self {
            Reason::NestedQuantifier { inner, .. } => inner.clone(),
            Reason::UnboundedBinderType { span, .. } => span.clone(),
            Reason::ExistsQuantifier { span } => span.clone(),
        }
    }
}

/// Classify a property's quantifier shape.
///
/// `Ok(Shape::NoQuantifier)` — body has no quantifier, legacy lowering path
///   is correct and non-vacuous.
/// `Ok(Shape::SingleBinderForall { … })` — body is `forall <binder> : <ty>,
///   inner` where `inner` itself has no further quantifiers; codegen emits
///   `<prop>_at(s, <binder>)` and binds `<binder>` at the harness layer.
/// `Err(Reason::…)` — body has a quantifier the lowering can't handle.
///   `check.rs` emits a P5 lint; emitters skip the harness for this property
///   to avoid the silent `true` stub.
pub fn supported_shape(prop: &PropertyDecl) -> Result<Shape, Reason> {
    classify_expr(&prop.body)
}

fn classify_expr(node: &Node<Expr>) -> Result<Shape, Reason> {
    match &node.node {
        // Top-level forall — accept the single-binder shape, reject if the
        // body has further quantifiers (we don't synthesize two binders).
        Expr::Quant {
            kind: Quantifier::Forall,
            binder,
            binder_ty,
            body,
        } => {
            if let Some((_, inner_span)) = find_nested_quantifier(body) {
                return Err(Reason::NestedQuantifier {
                    outer: node.span.clone(),
                    inner: inner_span,
                });
            }
            if !binder_type_supported(binder_ty) {
                return Err(Reason::UnboundedBinderType {
                    ty: binder_ty.clone(),
                    span: node.span.clone(),
                });
            }
            Ok(Shape::SingleBinderForall {
                binder: binder.clone(),
                binder_ty: binder_ty.clone(),
                span: node.span.clone(),
            })
        }
        Expr::Quant {
            kind: Quantifier::Exists,
            ..
        } => Err(Reason::ExistsQuantifier {
            span: node.span.clone(),
        }),
        // Anything else — non-quantifier body. Sub-expressions might still
        // contain quantifiers (e.g. `(forall i, …) and (forall j, …)`); for
        // v2.20 we accept only the `NoQuantifier` and `SingleBinderForall`
        // shapes, so any inner quantifier ⇒ P5 lint.
        _ => match find_nested_quantifier(node) {
            None => Ok(Shape::NoQuantifier),
            Some((outer, inner)) => Err(Reason::NestedQuantifier { outer, inner }),
        },
    }
}

/// Is the binder type lowerable to `kani::any::<T>()` / proptest `any::<T>()`?
///
/// Accepted:
///   - Integer primitives (U8/U16/U32/U64/U128, I8/I16/I32/I64/I128)
///   - Bool
///   - Named record / sum / lifecycle-state references (e.g. `Distribution`,
///     `Pool.Active`) — record codegen derives `kani::Arbitrary` and emits a
///     proptest `arb_<Name>()` strategy.
///
/// Rejected:
///   - Unbounded compounds: `Vec<T>`, `List<T>`, etc.
///   - `Pubkey` (32 bytes; technically arbitrary-able but the harness model
///     for "all possible authorities" doesn't add coverage proptest already
///     gets from arbitrary State.<pubkey>).
///   - Compound `Map[N] T` as a binder doesn't make spec sense; reject.
fn binder_type_supported(ty: &str) -> bool {
    let ty = ty.trim();
    match ty {
        // Primitives.
        "U8" | "U16" | "U32" | "U64" | "U128" => true,
        "I8" | "I16" | "I32" | "I64" | "I128" => true,
        "Bool" => true,
        // Fin[N] is a bounded index type — proptest gives it usize-range and
        // Kani picks `kani::any::<usize>()`. Accept.
        t if t.starts_with("Fin[") => true,
        // `Vec<T>` / `List<T>` / `Set<T>` — unbounded, reject.
        t if t.starts_with("Vec<") => false,
        t if t.starts_with("List<") => false,
        t if t.starts_with("Set<") => false,
        // `Map[N] T` as a binder doesn't make spec sense; reject.
        t if t.starts_with("Map") => false,
        // `Pubkey` — see comment above; reject for now, may revisit.
        "Pubkey" => false,
        // Anything else is presumed a user-declared type (record, sum,
        // lifecycle variant `Account.Active`, alias). Accept; the codegen
        // already wires `kani::Arbitrary` for records and `arb_<Name>` for
        // sum types. If the name doesn't resolve, codegen / cargo check
        // surface the failure — that's a typo, not a quantifier-shape bug.
        _ => true,
    }
}

/// Walk an expression tree and return the span of the first quantifier
/// found, if any. Returns `(span_of_quant, span_of_quant)` — the caller
/// picks whichever it wants. Used by both the nested-quantifier walker
/// (where the "outer" forall is already captured separately) and the no-
/// top-quantifier path (where we treat any inner quant as nested).
fn find_nested_quantifier(node: &Node<Expr>) -> Option<(Span, Span)> {
    match &node.node {
        Expr::Quant { .. } => Some((node.span.clone(), node.span.clone())),
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => {
            find_nested_quantifier(lhs).or_else(|| find_nested_quantifier(rhs))
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => find_nested_quantifier(inner),
        Expr::Sum { body, .. } => find_nested_quantifier(body),
        Expr::MulDivFloor { a, b, d } | Expr::MulDivCeil { a, b, d } => find_nested_quantifier(a)
            .or_else(|| find_nested_quantifier(b))
            .or_else(|| find_nested_quantifier(d)),
        Expr::Match { scrutinee, arms } => find_nested_quantifier(scrutinee).or_else(|| {
            arms.iter()
                .find_map(|arm| find_nested_quantifier(&arm.body))
        }),
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => find_nested_quantifier(cond)
            .or_else(|| find_nested_quantifier(then_branch))
            .or_else(|| find_nested_quantifier(else_branch)),
        Expr::Let { value, body, .. } => {
            find_nested_quantifier(value).or_else(|| find_nested_quantifier(body))
        }
        Expr::App { args, .. } => args.iter().find_map(find_nested_quantifier),
        Expr::Field { base, .. } => find_nested_quantifier(base),
        Expr::RecordLit(fs) => fs.iter().find_map(|(_, v)| find_nested_quantifier(v)),
        Expr::RecordUpdate { base, updates } => find_nested_quantifier(base)
            .or_else(|| updates.iter().find_map(|(_, v)| find_nested_quantifier(v))),
        Expr::Ctor { payload, .. } => payload.as_ref().and_then(|p| find_nested_quantifier(p)),
        Expr::IsVariant { scrutinee, .. } => find_nested_quantifier(scrutinee),
        // Leaves: no inner expression to walk.
        Expr::Int(_) | Expr::Bool(_) | Expr::Path(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ArithOp, BoolOp, CmpOp, Path, PathSeg, PreservedBy};

    fn n<T>(node: T, span: Span) -> Node<T> {
        Node::new(node, span)
    }

    fn prop(body: Node<Expr>) -> PropertyDecl {
        PropertyDecl {
            name: "p".to_string(),
            doc: None,
            body,
            preserved_by: PreservedBy::All,
        }
    }

    fn path(name: &str) -> Expr {
        Expr::Path(Path {
            root: name.to_string(),
            segments: Vec::<PathSeg>::new(),
        })
    }

    #[test]
    fn no_quantifier_is_supported() {
        let body = n(
            Expr::Cmp {
                op: CmpOp::Ge,
                lhs: Box::new(n(path("x"), 0..1)),
                rhs: Box::new(n(Expr::Int(0), 4..5)),
            },
            0..5,
        );
        assert_eq!(supported_shape(&prop(body)), Ok(Shape::NoQuantifier));
    }

    #[test]
    fn single_forall_over_record_is_supported() {
        let inner = n(
            Expr::Cmp {
                op: CmpOp::Ge,
                lhs: Box::new(n(path("d"), 20..21)),
                rhs: Box::new(n(Expr::Int(0), 24..25)),
            },
            20..25,
        );
        let body = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "d".to_string(),
                binder_ty: "Distribution".to_string(),
                body: Box::new(inner),
            },
            0..30,
        );
        let shape = supported_shape(&prop(body)).expect("supported");
        match shape {
            Shape::SingleBinderForall {
                binder, binder_ty, ..
            } => {
                assert_eq!(binder, "d");
                assert_eq!(binder_ty, "Distribution");
            }
            other => panic!("expected SingleBinderForall, got {:?}", other),
        }
    }

    #[test]
    fn single_forall_over_u64_is_supported() {
        let inner = n(
            Expr::Cmp {
                op: CmpOp::Ge,
                lhs: Box::new(n(path("v"), 20..21)),
                rhs: Box::new(n(Expr::Int(0), 24..25)),
            },
            20..25,
        );
        let body = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "v".to_string(),
                binder_ty: "U64".to_string(),
                body: Box::new(inner),
            },
            0..30,
        );
        assert!(matches!(
            supported_shape(&prop(body)),
            Ok(Shape::SingleBinderForall { .. })
        ));
    }

    #[test]
    fn nested_forall_is_rejected() {
        let innermost = n(Expr::Bool(true), 30..34);
        let mid = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "c".to_string(),
                binder_ty: "Claim".to_string(),
                body: Box::new(innermost),
            },
            15..40,
        );
        let outer = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "d".to_string(),
                binder_ty: "Distribution".to_string(),
                body: Box::new(mid),
            },
            0..40,
        );
        match supported_shape(&prop(outer)) {
            Err(Reason::NestedQuantifier { .. }) => {}
            other => panic!("expected NestedQuantifier, got {:?}", other),
        }
    }

    #[test]
    fn exists_is_rejected() {
        let inner = n(Expr::Bool(true), 10..14);
        let body = n(
            Expr::Quant {
                kind: Quantifier::Exists,
                binder: "d".to_string(),
                binder_ty: "Distribution".to_string(),
                body: Box::new(inner),
            },
            0..20,
        );
        match supported_shape(&prop(body)) {
            Err(Reason::ExistsQuantifier { .. }) => {}
            other => panic!("expected ExistsQuantifier, got {:?}", other),
        }
    }

    #[test]
    fn forall_with_quantifier_inside_conjunction_is_rejected() {
        // `forall d : T, (forall c : Claim, …) and …` — inner quant lives
        // behind a `BoolOp::And`, so the find_nested_quantifier walker has
        // to descend through BoolOp.
        let inner_quant = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "c".to_string(),
                binder_ty: "Claim".to_string(),
                body: Box::new(n(Expr::Bool(true), 40..44)),
            },
            20..45,
        );
        let conj = n(
            Expr::BoolOp {
                op: BoolOp::And,
                lhs: Box::new(inner_quant),
                rhs: Box::new(n(Expr::Bool(true), 50..54)),
            },
            20..54,
        );
        let outer = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "d".to_string(),
                binder_ty: "Distribution".to_string(),
                body: Box::new(conj),
            },
            0..54,
        );
        assert!(matches!(
            supported_shape(&prop(outer)),
            Err(Reason::NestedQuantifier { .. })
        ));
    }

    #[test]
    fn vec_binder_is_rejected() {
        let inner = n(Expr::Bool(true), 20..24);
        let body = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "v".to_string(),
                binder_ty: "Vec<U64>".to_string(),
                body: Box::new(inner),
            },
            0..24,
        );
        assert!(matches!(
            supported_shape(&prop(body)),
            Err(Reason::UnboundedBinderType { .. })
        ));
    }

    #[test]
    fn nested_in_subexpr_when_no_top_forall_is_rejected() {
        // Body is `(forall v : U64, …) and state.x >= 0` — no top-level
        // forall, but a quantifier sits inside the conjunction. The
        // emitters can't lower this; should P5.
        let inner_quant = n(
            Expr::Quant {
                kind: Quantifier::Forall,
                binder: "v".to_string(),
                binder_ty: "U64".to_string(),
                body: Box::new(n(Expr::Bool(true), 25..29)),
            },
            0..30,
        );
        let cmp = n(
            Expr::Cmp {
                op: CmpOp::Ge,
                lhs: Box::new(n(path("state.x"), 35..42)),
                rhs: Box::new(n(Expr::Int(0), 46..47)),
            },
            35..47,
        );
        let body = n(
            Expr::BoolOp {
                op: BoolOp::And,
                lhs: Box::new(inner_quant),
                rhs: Box::new(cmp),
            },
            0..47,
        );
        assert!(matches!(
            supported_shape(&prop(body)),
            Err(Reason::NestedQuantifier { .. })
        ));
    }

    #[test]
    fn no_quantifier_with_arith_path_is_supported() {
        let body = n(
            Expr::Cmp {
                op: CmpOp::Ge,
                lhs: Box::new(n(
                    Expr::Arith {
                        op: ArithOp::Add,
                        lhs: Box::new(n(path("a"), 0..1)),
                        rhs: Box::new(n(path("b"), 4..5)),
                    },
                    0..5,
                )),
                rhs: Box::new(n(Expr::Int(0), 9..10)),
            },
            0..10,
        );
        assert_eq!(supported_shape(&prop(body)), Ok(Shape::NoQuantifier));
    }
}
