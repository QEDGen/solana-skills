//! `ExprTree` construction — #151 Slice 0.
//!
//! Builds the name-resolved, type-annotated [`ExprTree`] from a
//! **canonicalized** AST expression (run `canonicalize_state_refs` first for
//! handler-scoped positions — bare state reads must already be
//! `state.`-rooted). This is the one place expression name resolution
//! happens; renderers (slices 1–3) consume the resolved tree and never see
//! `TypeEnv` / `CanonScope` again.

use crate::mir::expr_tree::{
    BindingKind, ExprTree, QuantKind, TreeArithOp, TreeBoolOp, TreeCmpOp, TreeMatchArm, TreePath,
    TreeSeg,
};

use super::*;

/// Scope + type context for one tree build. Unlike `CanonScope` (which only
/// needs "is this a state field?" and a merged skip-set), the tree builder
/// distinguishes every binding class, so the sets stay separate.
pub(super) struct TreeCx<'a, 'env> {
    pub env: &'a TypeEnv<'env>,
    pub consts: ConstTable<'a>,
    /// `ghost` names — a subset of `env.state_fields` that must classify
    /// `BindingKind::Ghost`, not `StateField`.
    pub ghosts: std::collections::BTreeSet<String>,
    pub params: std::collections::BTreeSet<String>,
    pub lets: std::collections::BTreeSet<String>,
    pub accounts: std::collections::BTreeSet<String>,
    pub externals: std::collections::BTreeSet<String>,
    pub abstracts: std::collections::BTreeSet<String>,
}

/// Collect `ghost` names from the typed spec (order-independent — ghosts
/// may be declared after the handlers that reference them).
pub(super) fn ghost_names(spec: &a::Spec) -> std::collections::BTreeSet<String> {
    spec.items
        .iter()
        .filter_map(|Node { node, .. }| match node {
            TopItem::Ghost(g) => Some(g.name.clone()),
            _ => None,
        })
        .collect()
}

impl<'a, 'env> TreeCx<'a, 'env> {
    /// Spec-level context: properties, invariants, schemas, ghost bodies —
    /// no handler scope, so params / lets / accounts / abstracts are empty.
    pub(super) fn spec_level(
        env: &'a TypeEnv<'env>,
        consts: ConstTable<'a>,
        ghosts: std::collections::BTreeSet<String>,
    ) -> Self {
        TreeCx {
            env,
            consts,
            ghosts,
            params: Default::default(),
            lets: Default::default(),
            accounts: Default::default(),
            externals: Default::default(),
            abstracts: Default::default(),
        }
    }

    pub(super) fn for_environment(
        environment: &a::EnvironmentDecl,
        env: &'a TypeEnv<'env>,
        consts: ConstTable<'a>,
        ghosts: std::collections::BTreeSet<String>,
    ) -> Self {
        let mut cx = TreeCx::spec_level(env, consts, ghosts);
        for clause in &environment.clauses {
            if let a::EnvClause::External { object, .. } = &clause.node {
                cx.externals.insert(object.clone());
            }
        }
        cx
    }

    /// Handler-scoped context. Mirrors `canon_scope_for_handler`'s
    /// collection walk (clauses may appear in any order) but keeps each
    /// binding class separate. Legacy `takes` fields count as params.
    pub(super) fn for_handler(
        h: &a::HandlerDecl,
        env: &'a TypeEnv<'env>,
        consts: ConstTable<'a>,
        ghosts: std::collections::BTreeSet<String>,
    ) -> Self {
        let mut cx = TreeCx::spec_level(env, consts, ghosts);
        cx.params.extend(h.params.iter().map(|p| p.name.clone()));
        for Node { node: clause, .. } in &h.clauses {
            match clause {
                a::HandlerClause::Accounts(descs) => {
                    cx.accounts.extend(descs.iter().map(|d| d.name.clone()));
                }
                a::HandlerClause::Let { name, .. } => {
                    cx.lets.insert(name.clone());
                }
                a::HandlerClause::Abstract { name, .. } => {
                    cx.abstracts.insert(name.clone());
                }
                a::HandlerClause::Takes(fields) => {
                    cx.params.extend(fields.iter().map(|f| f.name.clone()));
                }
                a::HandlerClause::Auth(actor) => {
                    // The auth actor names an account binding (dotted forms
                    // are desugared later; only the head matters here).
                    let head = actor.split('.').next().unwrap_or(actor);
                    cx.accounts.insert(head.to_string());
                }
                _ => {}
            }
        }
        cx
    }

    /// Interface-handler context: the callee's params are in scope; there
    /// is no state / account / let scope on an interface contract.
    pub(super) fn for_interface_handler(
        h: &a::InterfaceHandlerDecl,
        env: &'a TypeEnv<'env>,
        consts: ConstTable<'a>,
    ) -> Self {
        let mut cx = TreeCx::spec_level(env, consts, Default::default());
        cx.params.extend(h.params.iter().map(|p| p.name.clone()));
        for Node { node: clause, .. } in &h.clauses {
            if let a::InterfaceHandlerClause::Accounts(descs) = clause {
                cx.accounts.extend(descs.iter().map(|d| d.name.clone()));
            }
        }
        if let Some(binder) = &h.result_binder {
            // The return-value binder reads like a param inside the
            // callee's own ensures.
            cx.params.insert(binder.clone());
        }
        cx
    }
}

/// Build the resolved tree from a canonicalized AST expression.
pub(super) fn build_expr_tree(n: &Node<Expr>, cx: &TreeCx) -> ExprTree {
    let mut shadow: Vec<String> = Vec::new();
    build(&n.node, cx, &mut shadow)
}

/// [`build_expr_tree`] over a bare (span-less) AST expression — for
/// synthesized nodes (transfer amounts, ghost-update RHS folding) that
/// have no surrounding `Node`.
pub(super) fn build_expr_tree_raw(e: &Expr, cx: &TreeCx) -> ExprTree {
    let mut shadow: Vec<String> = Vec::new();
    build(e, cx, &mut shadow)
}

fn boxed(n: &Node<Expr>, cx: &TreeCx, shadow: &mut Vec<String>) -> Box<ExprTree> {
    Box::new(build(&n.node, cx, shadow))
}

/// Build under a binder: `name` shadows outer bindings within `body`.
fn under(name: &str, body: &Node<Expr>, cx: &TreeCx, shadow: &mut Vec<String>) -> Box<ExprTree> {
    shadow.push(name.to_string());
    let out = boxed(body, cx, shadow);
    shadow.pop();
    out
}

fn build(e: &Expr, cx: &TreeCx, shadow: &mut Vec<String>) -> ExprTree {
    match e {
        Expr::Int(v) => ExprTree::Int(*v),
        Expr::Bool(b) => ExprTree::Bool(*b),
        Expr::Path(p) => ExprTree::Path(build_path(p, cx, shadow)),
        Expr::Old(inner) => ExprTree::Old(boxed(inner, cx, shadow)),
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => ExprTree::Sum {
            binder: binder.clone(),
            binder_ty: binder_ty.clone(),
            fin_bound: cx.env.fin_bound(binder_ty),
            body: under(binder, body, cx, shadow),
        },
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body,
        } => ExprTree::Quant {
            kind: match kind {
                a::Quantifier::Forall => QuantKind::Forall,
                a::Quantifier::Exists => QuantKind::Exists,
            },
            binder: binder.clone(),
            binder_ty: binder_ty.clone(),
            fin_bound: cx.env.fin_bound(binder_ty),
            body: under(binder, body, cx, shadow),
        },
        Expr::QuantIn {
            kind,
            binder,
            coll,
            body,
        } => ExprTree::QuantIn {
            kind: match kind {
                a::Quantifier::Forall => QuantKind::Forall,
                a::Quantifier::Exists => QuantKind::Exists,
            },
            binder: binder.clone(),
            coll: boxed(coll, cx, shadow),
            body: under(binder, body, cx, shadow),
        },
        Expr::BoolOp { op, lhs, rhs } => ExprTree::BoolOp {
            op: match op {
                a::BoolOp::And => TreeBoolOp::And,
                a::BoolOp::Or => TreeBoolOp::Or,
                a::BoolOp::Implies => TreeBoolOp::Implies,
            },
            lhs: boxed(lhs, cx, shadow),
            rhs: boxed(rhs, cx, shadow),
        },
        Expr::Not(inner) => ExprTree::Not(boxed(inner, cx, shadow)),
        Expr::Cmp { op, lhs, rhs } => ExprTree::Cmp {
            op: match op {
                a::CmpOp::Eq => TreeCmpOp::Eq,
                a::CmpOp::Ne => TreeCmpOp::Ne,
                a::CmpOp::Le => TreeCmpOp::Le,
                a::CmpOp::Ge => TreeCmpOp::Ge,
                a::CmpOp::Lt => TreeCmpOp::Lt,
                a::CmpOp::Gt => TreeCmpOp::Gt,
            },
            lhs: boxed(lhs, cx, shadow),
            rhs: boxed(rhs, cx, shadow),
        },
        Expr::Arith { op, lhs, rhs } => ExprTree::Arith {
            op: match op {
                a::ArithOp::Add => TreeArithOp::Add,
                a::ArithOp::Sub => TreeArithOp::Sub,
                a::ArithOp::Mul => TreeArithOp::Mul,
                a::ArithOp::Div => TreeArithOp::Div,
                a::ArithOp::Mod => TreeArithOp::Mod,
            },
            lhs: boxed(lhs, cx, shadow),
            rhs: boxed(rhs, cx, shadow),
        },
        // Grouping is structural in the tree — Paren desugars away.
        Expr::Paren(inner) => build(&inner.node, cx, shadow),
        Expr::MulDivFloor { a, b, d } => ExprTree::MulDivFloor {
            a: boxed(a, cx, shadow),
            b: boxed(b, cx, shadow),
            d: boxed(d, cx, shadow),
        },
        Expr::MulDivCeil { a, b, d } => ExprTree::MulDivCeil {
            a: boxed(a, cx, shadow),
            b: boxed(b, cx, shadow),
            d: boxed(d, cx, shadow),
        },
        Expr::MulDivRoundHalfUp { a, b, d } => ExprTree::MulDivRoundHalfUp {
            a: boxed(a, cx, shadow),
            b: boxed(b, cx, shadow),
            d: boxed(d, cx, shadow),
        },
        Expr::Match { scrutinee, arms } => {
            // Resolve the scrutinee's enum (a Path scrutinee resolves via the
            // type env) so the arm patterns are shape-correct; each named arm's
            // shape comes from `resolve_variant`. The `"_"` wildcard has no shape.
            let enum_hint = match &scrutinee.node {
                Expr::Path(p) => cx.env.path_type_name(p),
                _ => None,
            };
            ExprTree::Match {
                scrutinee: boxed(scrutinee, cx, shadow),
                arms: arms
                    .iter()
                    .map(|arm| TreeMatchArm {
                        variant: arm.variant.clone(),
                        binder: arm.binder.clone(),
                        body: match &arm.binder {
                            Some(b) => under(b, &arm.body, cx, shadow),
                            None => boxed(&arm.body, cx, shadow),
                        },
                        shape: cx
                            .env
                            .resolve_variant(enum_hint.as_deref(), &arm.variant)
                            .map(|(_, s)| s)
                            .unwrap_or(crate::mir::VariantShape::Struct),
                    })
                    .collect(),
                enum_ty: enum_hint,
            }
        }
        // #325 — the three constructor forms resolve their nominal type
        // HERE, while the TypeEnv is in scope (mirroring `IsVariant` /
        // `Match` below): the env-less Rust renderer needs a concrete
        // type name, never a placeholder.
        Expr::Ctor { variant, payload } => ExprTree::Ctor {
            variant: variant.clone(),
            payload: payload.as_ref().map(|p| boxed(p, cx, shadow)),
            ty: cx
                .env
                .resolve_variant(None, variant)
                .map(|(enum_name, _)| enum_name),
        },
        Expr::RecordLit(fields) => {
            let field_names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
            ExprTree::RecordLit {
                fields: fields
                    .iter()
                    .map(|(n, v)| (n.clone(), build(&v.node, cx, shadow)))
                    .collect(),
                ty: cx.env.record_for_fields(&field_names),
            }
        }
        Expr::RecordUpdate { base, updates } => ExprTree::RecordUpdate {
            ty: match &base.node {
                Expr::Path(p) => cx.env.path_type_name(p),
                _ => None,
            },
            base: boxed(base, cx, shadow),
            updates: updates
                .iter()
                .map(|(n, v)| (n.clone(), build(&v.node, cx, shadow)))
                .collect(),
        },
        Expr::IsVariant { scrutinee, variant } => {
            // Resolve the enum + variant shape now, while the TypeEnv is in
            // scope: a Path scrutinee (`state.status is .Approved`) resolves
            // its enum via the type env; `resolve_variant` falls back to a
            // global unique-name search. Carried into the node for the
            // env-less Rust renderer.
            let hint = match &scrutinee.node {
                Expr::Path(p) => cx.env.path_type_name(p),
                _ => None,
            };
            let resolved = cx.env.resolve_variant(hint.as_deref(), variant);
            ExprTree::IsVariant {
                scrutinee: boxed(scrutinee, cx, shadow),
                variant: variant.clone(),
                enum_ty: resolved.as_ref().map(|(e, _)| e.clone()),
                shape: resolved
                    .map(|(_, s)| s)
                    .unwrap_or(crate::mir::VariantShape::Struct),
            }
        }
        Expr::App { func, args } => ExprTree::App {
            func: func.clone(),
            args: args.iter().map(|n| build(&n.node, cx, shadow)).collect(),
        },
        // `contains(coll, elem)` — collection membership.
        Expr::Contains { coll, elem } => ExprTree::Contains {
            coll: Box::new(build(&coll.node, cx, shadow)),
            elem: Box::new(build(&elem.node, cx, shadow)),
        },
        // `len(coll)` — collection length.
        Expr::Len(coll) => ExprTree::Len(Box::new(build(&coll.node, cx, shadow))),
        Expr::Field { base, field } => ExprTree::Field {
            base: boxed(base, cx, shadow),
            field: field.clone(),
        },
        Expr::Let { name, value, body } => ExprTree::Let {
            name: name.clone(),
            value: boxed(value, cx, shadow),
            body: under(name, body, cx, shadow),
        },
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => ExprTree::IfThenElse {
            cond: boxed(cond, cx, shadow),
            then_branch: boxed(then_branch, cx, shadow),
            else_branch: boxed(else_branch, cx, shadow),
        },
    }
}

/// Resolve a path's root against the scope and annotate the leaf type.
/// Resolution order: state-rooted → expression binders (lexical shadow) →
/// params → consts → lets → accounts → abstract binders → unresolved.
fn build_path(p: &a::Path, cx: &TreeCx, shadow: &[String]) -> TreePath {
    let segments: Vec<TreeSeg> = p
        .segments
        .iter()
        .map(|seg| match seg {
            a::PathSeg::Field(f) => TreeSeg::Field(f.clone()),
            a::PathSeg::Index(i) => TreeSeg::Index(i.clone()),
        })
        .collect();

    let binding = if p.root == "state" {
        match p.segments.first() {
            Some(a::PathSeg::Field(f)) if cx.ghosts.contains(f) => BindingKind::Ghost,
            _ => BindingKind::StateField,
        }
    } else if shadow.iter().any(|s| s == &p.root) {
        BindingKind::ExprBinder
    } else if cx.params.contains(&p.root) {
        BindingKind::Param
    } else if let Some(value) = cx.consts.get(&p.root) {
        BindingKind::Const(value.clone())
    } else if cx.lets.contains(&p.root) {
        BindingKind::LetBound
    } else if cx.accounts.contains(&p.root) {
        BindingKind::Account
    } else if cx.externals.contains(&p.root) && cx.env.is_external_root(&p.root) {
        BindingKind::External
    } else if cx.abstracts.contains(&p.root) {
        BindingKind::AbstractBinder
    } else {
        BindingKind::Unresolved
    };

    // Leaf type from the environment: state-rooted paths walk fields /
    // records / Map subscripts; bare params resolve through the handler
    // signature. Everything else stays untyped.
    let ty = path_leaf_type(p, cx.env).map(|ty| ty_of_type_ref(ty, cx.env));

    TreePath {
        root: p.root.clone(),
        binding,
        segments,
        ty,
    }
}

/// Leaf `TypeRef` of a path. Like `TypeEnv::path_type_ref` but also
/// resolves record-form state (`type State = { … }` / `state { … }` sugar
/// keeps its fields in `records["State"]`, not `state_fields`) — the
/// fallback stays tree-builder-only: widening `path_type_ref` itself would
/// change `path_is_pod_field`'s answers and with them the rendered Quasar
/// output.
fn path_leaf_type<'e>(p: &a::Path, env: &TypeEnv<'e>) -> Option<&'e a::TypeRef> {
    env.resolve_path_leaf(p, true)
}

/// Source `TypeRef` → MIR `Ty`. Named forms route through the same
/// string-level parser codegen uses (`mir::parse_ty`) so `Custom` spellings
/// stay consistent; `Fin` / `Vec` / `Option` map to their structured `Ty`
/// variants (#327) — `Custom` is reserved for declared nominal types.
fn ty_of_type_ref(t: &a::TypeRef, env: &TypeEnv<'_>) -> crate::mir::Ty {
    match t {
        a::TypeRef::Named(n) => crate::mir::parse_ty(&env.resolve_alias_name(n)),
        a::TypeRef::Param(head, tail) => {
            let inner = crate::mir::parse_ty(&env.resolve_alias_name(tail));
            match head.as_str() {
                "Vec" => crate::mir::Ty::Vec {
                    value: Box::new(inner),
                },
                "Option" => crate::mir::Ty::Option {
                    value: Box::new(inner),
                },
                _ => crate::mir::Ty::Custom(format!("{} {}", head, tail)),
            }
        }
        a::TypeRef::Map { bound, inner } => crate::mir::Ty::Map {
            capacity: bound.clone(),
            value: Box::new(ty_of_type_ref(inner, env)),
        },
        a::TypeRef::Fin { bound } => crate::mir::Ty::Fin {
            bound: bound.clone(),
        },
    }
}
