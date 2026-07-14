//! Bare state-field canonicalization: `requires active == 0` must read the
//! same state slot as `requires state.active == 0`. The parser accepts the
//! bare form, but only `state.`-rooted paths pick up the state receiver at
//! render time — bare roots fell through verbatim into every backend (Lean
//! `if active = 0`, Kani/proptest `if !((active == 0))`), none of which
//! compile (issue #139). Rewriting the typed AST here, before the Lean/Rust
//! string projections and before `ast_body` is stored, fixes all consumers
//! at one seam.

use super::*;

/// Handler-scoped name sets deciding whether a bare path root is a state
/// field read. `skip` holds every name bound by something other than state:
/// params, consts, handler accounts, handler-level `let`s, abstract binders,
/// and the `auth` actor. Expression-level binders (quantifiers, `sum`, `let
/// … in`, match-arm payloads) shadow lexically during the walk instead.
pub(super) struct CanonScope {
    state_fields: std::collections::BTreeSet<String>,
    skip: std::collections::BTreeSet<String>,
}

/// Build the scope for one handler. Clauses may appear in any order, so
/// accounts / lets / abstract binders are collected up front rather than as
/// the clause loop reaches them.
pub(super) fn canon_scope_for_handler(
    h: &a::HandlerDecl,
    consts: ConstTable,
    env: &TypeEnv,
) -> CanonScope {
    let mut skip: std::collections::BTreeSet<String> = consts.keys().cloned().collect();
    skip.extend(h.params.iter().map(|p| p.name.clone()));
    for Node { node: clause, .. } in &h.clauses {
        match clause {
            a::HandlerClause::Accounts(descs) => {
                skip.extend(descs.iter().map(|d| d.name.clone()));
            }
            a::HandlerClause::Let { name, .. } => {
                skip.insert(name.clone());
            }
            a::HandlerClause::Abstract { name, .. } => {
                skip.insert(name.clone());
            }
            a::HandlerClause::Auth(actor) => {
                skip.insert(actor.clone());
            }
            _ => {}
        }
    }
    // `env.state_fields` carries ADT variant fields + ghosts; the record
    // form (`type State = { … }` / `state { … }` sugar) keeps its fields in
    // `records["State"]` instead (see `TypeEnv::from_spec`), so union both.
    let mut state_fields: std::collections::BTreeSet<String> =
        env.state_fields.keys().cloned().collect();
    if let Some(state_record) = env.records.get("State") {
        state_fields.extend(state_record.keys().cloned());
    }
    CanonScope { state_fields, skip }
}

/// Rewrite bare state-field reads (`active`) into `state.`-rooted paths
/// (`state.active`) throughout `n`. Returns an owned copy; the input AST is
/// shared with other clause consumers and stays untouched.
pub(super) fn canonicalize_state_refs(n: &Node<Expr>, scope: &CanonScope) -> Node<Expr> {
    let mut shadow: Vec<String> = Vec::new();
    walk(n, scope, &mut shadow)
}

fn walk(n: &Node<Expr>, scope: &CanonScope, shadow: &mut Vec<String>) -> Node<Expr> {
    Node::new(walk_expr(&n.node, scope, shadow), n.span.clone())
}

fn boxed(n: &Node<Expr>, scope: &CanonScope, shadow: &mut Vec<String>) -> Box<Node<Expr>> {
    Box::new(walk(n, scope, shadow))
}

/// Walk under a binder: `name` shadows any same-named state field within
/// `body` only.
fn walk_under(
    name: &str,
    body: &Node<Expr>,
    scope: &CanonScope,
    shadow: &mut Vec<String>,
) -> Box<Node<Expr>> {
    shadow.push(name.to_string());
    let out = boxed(body, scope, shadow);
    shadow.pop();
    out
}

fn walk_expr(e: &Expr, scope: &CanonScope, shadow: &mut Vec<String>) -> Expr {
    match e {
        Expr::Int(_) | Expr::Bool(_) => e.clone(),
        Expr::Path(p) => {
            if p.root != "state"
                && scope.state_fields.contains(&p.root)
                && !scope.skip.contains(&p.root)
                && !shadow.iter().any(|s| s == &p.root)
            {
                let mut segments = Vec::with_capacity(p.segments.len() + 1);
                segments.push(a::PathSeg::Field(p.root.clone()));
                segments.extend(p.segments.iter().cloned());
                Expr::Path(a::Path {
                    root: "state".to_string(),
                    segments,
                })
            } else {
                e.clone()
            }
        }
        Expr::Old(inner) => Expr::Old(boxed(inner, scope, shadow)),
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => Expr::Sum {
            binder: binder.clone(),
            binder_ty: binder_ty.clone(),
            body: walk_under(binder, body, scope, shadow),
        },
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body,
        } => Expr::Quant {
            kind: *kind,
            binder: binder.clone(),
            binder_ty: binder_ty.clone(),
            body: walk_under(binder, body, scope, shadow),
        },
        Expr::QuantIn {
            kind,
            binder,
            coll,
            body,
        } => Expr::QuantIn {
            kind: *kind,
            binder: binder.clone(),
            coll: boxed(coll, scope, shadow),
            body: walk_under(binder, body, scope, shadow),
        },
        Expr::BoolOp { op, lhs, rhs } => Expr::BoolOp {
            op: *op,
            lhs: boxed(lhs, scope, shadow),
            rhs: boxed(rhs, scope, shadow),
        },
        Expr::Not(inner) => Expr::Not(boxed(inner, scope, shadow)),
        Expr::Cmp { op, lhs, rhs } => Expr::Cmp {
            op: *op,
            lhs: boxed(lhs, scope, shadow),
            rhs: boxed(rhs, scope, shadow),
        },
        Expr::Arith { op, lhs, rhs } => Expr::Arith {
            op: *op,
            lhs: boxed(lhs, scope, shadow),
            rhs: boxed(rhs, scope, shadow),
        },
        Expr::Paren(inner) => Expr::Paren(boxed(inner, scope, shadow)),
        Expr::MulDivFloor { a, b, d } => Expr::MulDivFloor {
            a: boxed(a, scope, shadow),
            b: boxed(b, scope, shadow),
            d: boxed(d, scope, shadow),
        },
        Expr::MulDivCeil { a, b, d } => Expr::MulDivCeil {
            a: boxed(a, scope, shadow),
            b: boxed(b, scope, shadow),
            d: boxed(d, scope, shadow),
        },
        Expr::MulDivRoundHalfUp { a, b, d } => Expr::MulDivRoundHalfUp {
            a: boxed(a, scope, shadow),
            b: boxed(b, scope, shadow),
            d: boxed(d, scope, shadow),
        },
        Expr::Contains { coll, elem } => Expr::Contains {
            coll: boxed(coll, scope, shadow),
            elem: boxed(elem, scope, shadow),
        },
        Expr::Len(inner) => Expr::Len(boxed(inner, scope, shadow)),
        Expr::Match { scrutinee, arms } => Expr::Match {
            scrutinee: boxed(scrutinee, scope, shadow),
            arms: arms
                .iter()
                .map(|arm| a::MatchExprArm {
                    variant: arm.variant.clone(),
                    binder: arm.binder.clone(),
                    body: match &arm.binder {
                        Some(b) => walk_under(b, &arm.body, scope, shadow),
                        None => boxed(&arm.body, scope, shadow),
                    },
                })
                .collect(),
        },
        Expr::Ctor { variant, payload } => Expr::Ctor {
            variant: variant.clone(),
            payload: payload.as_ref().map(|p| boxed(p, scope, shadow)),
        },
        Expr::RecordLit(fields) => Expr::RecordLit(
            fields
                .iter()
                .map(|(n, v)| (n.clone(), walk(v, scope, shadow)))
                .collect(),
        ),
        Expr::RecordUpdate { base, updates } => Expr::RecordUpdate {
            base: boxed(base, scope, shadow),
            updates: updates
                .iter()
                .map(|(n, v)| (n.clone(), walk(v, scope, shadow)))
                .collect(),
        },
        Expr::IsVariant { scrutinee, variant } => Expr::IsVariant {
            scrutinee: boxed(scrutinee, scope, shadow),
            variant: variant.clone(),
        },
        Expr::App { func, args } => Expr::App {
            func: func.clone(),
            args: args.iter().map(|n| walk(n, scope, shadow)).collect(),
        },
        Expr::Field { base, field } => Expr::Field {
            base: boxed(base, scope, shadow),
            field: field.clone(),
        },
        Expr::Let { name, value, body } => Expr::Let {
            name: name.clone(),
            value: boxed(value, scope, shadow),
            body: walk_under(name, body, scope, shadow),
        },
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => Expr::IfThenElse {
            cond: boxed(cond, scope, shadow),
            then_branch: boxed(then_branch, scope, shadow),
            else_branch: boxed(else_branch, scope, shadow),
        },
    }
}

// ============================================================================
// Shadow-aware path-reference collection (backs `unknown_guard_identifier`)
// ============================================================================

/// A path reference surfaced from a guard-position expression, with
/// expression-level binders (quantifiers, `sum`, `let … in`, match-arm
/// payloads) already discharged — shadowed roots never surface.
pub(crate) enum GuardPathRef {
    /// Segment-less bare root: `active`, `amount`. Roots with segments
    /// (`vault.owner`, `Active.total`) are account / variant / namespace
    /// references with too many legitimate shapes to classify here.
    Bare(String),
    /// First field segment of a `state.`-rooted path: `state.active` →
    /// `active`. A name absent from the declared state fields renders
    /// verbatim (`s.actve`) and won't compile.
    StateField(String),
}

pub(crate) fn collect_guard_path_refs(node: &Node<Expr>) -> Vec<GuardPathRef> {
    let mut out = Vec::new();
    let mut shadow: Vec<String> = Vec::new();
    collect_refs(node, &mut shadow, &mut out);
    out
}

/// Walks the shared binder-aware `ast::for_each_child_with_binder` spine
/// (F2), keeping only the `Path` leaf arm; binder pushes/pops come from
/// the spine's binder slots.
fn collect_refs(n: &Node<Expr>, shadow: &mut Vec<String>, out: &mut Vec<GuardPathRef>) {
    if let Expr::Path(p) = &n.node {
        if p.root == "state" {
            if let Some(a::PathSeg::Field(f)) = p.segments.first() {
                out.push(GuardPathRef::StateField(f.clone()));
            }
        } else if p.segments.is_empty() && !shadow.iter().any(|s| s == &p.root) {
            out.push(GuardPathRef::Bare(p.root.clone()));
        }
        return;
    }
    a::for_each_child_with_binder(&n.node, |child, binder| match binder {
        Some(b) => {
            shadow.push(b.to_string());
            collect_refs(child, shadow, out);
            shadow.pop();
        }
        None => collect_refs(child, shadow, out),
    });
}
