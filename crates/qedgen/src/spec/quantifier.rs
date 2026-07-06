//! Quantifier-shape classifier for property bodies.
//!
//! `supported_shape` either returns what codegen needs to emit a non-vacuous
//! Kani / proptest harness, or a precise reason the shape can't be lowered
//! (feeds the P5 lint in `check.rs`). Intentionally narrow: anything beyond a
//! single-binder `forall` over a primitive or named ADT is `Err` — users
//! split broader shapes into single-binder properties (`docs/limitations.md`).

use crate::ast::{Expr, Node, PropertyDecl, Quantifier, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// Body is `P(s_post)` — no binder; harness asserts `P(&s)` post-transition.
    NoQuantifier,
    /// Body is `forall <binder> : <ty>, inner(<binder>, s_post)`; harness
    /// binds `<binder>` symbolically (kani::any / proptest any).
    SingleBinderForall {
        binder: String,
        binder_ty: String,
        /// Span of the outer `forall …` for diagnostics.
        span: Span,
    },
}

/// Why a shape can't be lowered to a non-vacuous harness. Spans point at the
/// offending token; messages feed the P5 lint verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Quantifier nested inside another; workaround is splitting into two
    /// single-binder properties.
    NestedQuantifier { outer: Span, inner: Span },
    /// Binder type is an unbounded collection (`Vec<T>`, `List<T>`, …) —
    /// not enumerable by `kani::any::<T>()` / proptest `any::<T>()`.
    UnboundedBinderType { ty: String, span: Span },
    /// `exists`. The classifier can't resolve aliases, so it conservatively
    /// reports every `exists`; the chumsky_adapter clears it when a *bounded*
    /// binder (`Fin[N]`, directly or via alias) lowered to `(0..N).any(…)`.
    /// Only unbounded `exists` (e.g. over `U64`) survives to the lint.
    ExistsQuantifier { span: Span },
}

impl Reason {
    /// Human-readable P5 lint message.
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

    /// Diagnostic anchor — the offending quantifier token, not the property.
    pub fn span(&self) -> Span {
        match self {
            Reason::NestedQuantifier { inner, .. } => inner.clone(),
            Reason::UnboundedBinderType { span, .. } => span.clone(),
            Reason::ExistsQuantifier { span } => span.clone(),
        }
    }
}

/// Classify a property's quantifier shape. `SingleBinderForall` makes codegen
/// emit `<prop>_at(s, <binder>)` with the binder bound at the harness layer;
/// `Err` makes `check.rs` emit a P5 lint and emitters skip the harness
/// (avoiding a silent `true` stub).
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
        // Non-quantifier body. Sub-expressions may still contain quantifiers
        // (e.g. `(forall i, …) and (forall j, …)`) — any inner quant ⇒ P5.
        _ => match find_nested_quantifier(node) {
            None => Ok(Shape::NoQuantifier),
            Some((outer, inner)) => Err(Reason::NestedQuantifier { outer, inner }),
        },
    }
}

/// Is the binder type lowerable to `kani::any::<T>()` / proptest `any::<T>()`?
fn binder_type_supported(ty: &str) -> bool {
    let ty = ty.trim();
    match ty {
        "U8" | "U16" | "U32" | "U64" | "U128" => true,
        "I8" | "I16" | "I32" | "I64" | "I128" => true,
        "Bool" => true,
        // Fin[N] is a bounded index — proptest gets a usize range, Kani
        // `kani::any::<usize>()`.
        t if t.starts_with("Fin[") => true,
        // Unbounded collections: not enumerable.
        t if t.starts_with("Vec<") => false,
        t if t.starts_with("List<") => false,
        t if t.starts_with("Set<") => false,
        // `Map[N] T` as a binder doesn't make spec sense.
        t if t.starts_with("Map") => false,
        // Technically arbitrary-able, but symbolic authorities add no
        // coverage over arbitrary State.<pubkey>; may revisit.
        "Pubkey" => false,
        // Presumed user-declared (record / sum / lifecycle variant / alias):
        // codegen wires `kani::Arbitrary` for records and `arb_<Name>` for
        // sums. Unresolvable names fail at codegen / cargo check — a typo,
        // not a quantifier-shape bug.
        _ => true,
    }
}

/// Span of the first quantifier in the tree, if any. Returns the same span
/// twice — callers pick whichever slot they need (outer-already-known vs
/// no-top-quantifier paths).
/// Walks the shared `ast::for_each_child` spine (F2), keeping only the
/// `Quant` arm.
fn find_nested_quantifier(node: &Node<Expr>) -> Option<(Span, Span)> {
    if matches!(node.node, Expr::Quant { .. }) {
        return Some((node.span.clone(), node.span.clone()));
    }
    let mut found = None;
    crate::ast::for_each_child(&node.node, |child| {
        if found.is_none() {
            found = find_nested_quantifier(child);
        }
    });
    found
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
