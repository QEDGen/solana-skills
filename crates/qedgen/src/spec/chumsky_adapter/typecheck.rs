//! Post-adapt typechecking: helper-signature collection, numeric/field-type
//! tables, per-handler effect/comparison type checks, and the recursion guard.

use super::*;

/// Walk every guard / ensures / effect-RHS / property body and collect
/// every `Expr::App` call site as an uninterpreted helper. First encounter
/// wins for the signature; duplicates (same name + arity) are skipped.
///
/// Return type is always boolean — every App call in the DSL lives in a
/// boolean-valued position. A call in an arithmetic position (`effect
/// { x := foo(y) + 1 }`) won't typecheck against the emitted axiom; richer
/// context-sensitive inference is future work.
pub(super) fn collect_uninterpreted_helpers(
    spec: &a::Spec,
    parsed: &ParsedSpec,
) -> Vec<(String, Vec<String>, String)> {
    let field_types = collect_field_types(parsed);
    let mut out: Vec<(String, Vec<String>, String)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();

    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Handler(h) => {
                let param_types: std::collections::HashMap<String, String> = h
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                    .collect();
                for Node { node: clause, .. } in &h.clauses {
                    match clause {
                        a::HandlerClause::Requires { guard, .. } => {
                            walk_apps(&guard.node, &field_types, &param_types, &mut out, &mut seen);
                        }
                        a::HandlerClause::Ensures(e) => {
                            walk_apps(&e.node, &field_types, &param_types, &mut out, &mut seen);
                        }
                        a::HandlerClause::Effect(blocks) => {
                            // Flatten through `match` arms.
                            for stmt in a::flatten_effect_blocks(blocks) {
                                walk_apps(
                                    &stmt.rhs.node,
                                    &field_types,
                                    &param_types,
                                    &mut out,
                                    &mut seen,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            TopItem::Property(p) => {
                walk_apps(
                    &p.body.node,
                    &field_types,
                    &std::collections::HashMap::new(),
                    &mut out,
                    &mut seen,
                );
            }
            // Walk ref_impl bodies too: a helper called only from a
            // ref_impl body must enter the uninterpreted-helper bag or Lean
            // fails on the unresolved name. The post-walk filter strips
            // names that are themselves ref_impls so declarations don't
            // collide.
            TopItem::RefImpl(r) => {
                let param_types: std::collections::HashMap<String, String> = r
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                    .collect();
                walk_apps(
                    &r.body.node,
                    &field_types,
                    &param_types,
                    &mut out,
                    &mut seen,
                );
            }
            _ => {}
        }
    }
    out
}

fn walk_apps(
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    out: &mut Vec<(String, Vec<String>, String)>,
    seen: &mut std::collections::HashSet<(String, usize)>,
) {
    if let Expr::App { func, args } = expr {
        // Skip the `now()` / `current_epoch()` builtins — they resolve via
        // support-library axioms (`QEDGen.Solana.Valid.now : Nat`); emitting
        // `axiom now : Bool` here would collide at elaboration.
        let is_builtin = (func == "now" || func == "current_epoch") && args.is_empty();
        if !is_builtin {
            let key = (func.clone(), args.len());
            if seen.insert(key) {
                let arg_types: Vec<String> = args
                    .iter()
                    .map(|n| infer_lean_type(&n.node, field_types, param_types))
                    .collect();
                // Bool, not Prop: requires/ensures lower to a transition
                // `if`-guard, which Lean needs `Decidable` — `axiom foo : T
                // → Prop` is opaque and fails to compile. `Bool` is
                // auto-`Decidable` and lifts into Prop via the `b = true`
                // coercion the call-site renderer already produces.
                out.push((func.clone(), arg_types, "Bool".to_string()));
            }
        }
    }
    // Shared spine (F2): this previously skipped `Match` / `Let` /
    // `IfThenElse` / `RecordLit` / `RecordUpdate` / `Ctor` / `Field` /
    // `IsVariant`, silently missing helper calls in those positions.
    crate::ast::for_each_child(expr, |child| {
        walk_apps(&child.node, field_types, param_types, out, seen);
    });
}

/// Best-effort Lean type for an argument expression. Used only for
/// axiom signature synthesis; a wrong guess degrades to a type error
/// at `lake build` time, but isn't silently corrupting anything.
fn infer_lean_type(
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
) -> String {
    match expr {
        Expr::Int(_) => "Nat".to_string(),
        Expr::Bool(_) => "Bool".to_string(),
        Expr::Path(p) => {
            let dsl_type = resolve_path_type(p, field_types, param_types);
            match dsl_type {
                Some("Pubkey") => "Pubkey".to_string(),
                Some("Bytes32") => "Bytes32".to_string(),
                Some("Bytes64") => "Bytes64".to_string(),
                Some("Bool") => "Bool".to_string(),
                Some(t) if is_signed_int(t) => "Int".to_string(),
                Some(_) => "Nat".to_string(),
                None => "Nat".to_string(),
            }
        }
        _ => "Nat".to_string(),
    }
}

fn is_signed_int(t: &str) -> bool {
    matches!(t, "I8" | "I16" | "I32" | "I64" | "I128")
}

/// Narrow check-time guard for Pubkey-vs-numeric-literal mismatches in
/// effect RHS and `requires` / `ensures` comparisons — the DSL has no
/// Pubkey literal syntax, so `state.key := 0` is always a category error.
/// Deliberately narrow: only fail when one side resolves to a Pubkey field
/// and the other is a bare integer literal.
pub fn typecheck_spec(spec: &a::Spec, parsed: &ParsedSpec) -> anyhow::Result<()> {
    let field_types = collect_field_types(parsed);
    let const_literals = collect_numeric_consts(spec);
    let dimensions = validate_dimensions(parsed)?;
    validate_external_fields(spec)?;
    validate_environment_external_scope(spec)?;

    for Node { node, .. } in &spec.items {
        if let TopItem::Handler(h) = node {
            let param_types: std::collections::HashMap<String, String> = h
                .params
                .iter()
                .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                .collect();
            typecheck_handler(h, &field_types, &param_types, &const_literals)?;
            typecheck_handler_dimensions(
                h,
                &field_types,
                &param_types,
                &dimensions,
                &const_literals,
            )?;
        } else {
            let empty = std::collections::HashMap::new();
            match node {
                TopItem::Property(p) => check_expr_dimensions(
                    &format!("property `{}`", p.name),
                    &p.body.node,
                    &field_types,
                    &empty,
                    &dimensions,
                    &const_literals,
                )?,
                TopItem::Invariant(i) => {
                    if let a::InvariantBody::Expr(body) = &i.body {
                        check_expr_dimensions(
                            &format!("invariant `{}`", i.name),
                            &body.node,
                            &field_types,
                            &empty,
                            &dimensions,
                            &const_literals,
                        )?;
                    }
                }
                TopItem::Environment(env) => {
                    let mut environment_fields = field_types.clone();
                    let mut seen_external = std::collections::HashSet::new();
                    for clause in &env.clauses {
                        if let a::EnvClause::External { object, field, ty } = &clause.node {
                            if object == "state" {
                                anyhow::bail!(
                                    "environment `{}` external object cannot use reserved namespace `state`",
                                    env.name
                                );
                            }
                            if !seen_external.insert((object.clone(), field.clone())) {
                                anyhow::bail!(
                                    "environment `{}` declares duplicate external field `{}.{}`",
                                    env.name,
                                    object,
                                    field
                                );
                            }
                            environment_fields.insert(field.clone(), type_ref_to_string(ty));
                        }
                    }
                    for clause in &env.clauses {
                        if let a::EnvClause::Constraint(expr) = &clause.node {
                            check_expr_dimensions(
                                &format!("environment `{}` constraint", env.name),
                                &expr.node,
                                &environment_fields,
                                &empty,
                                &dimensions,
                                &const_literals,
                            )?;
                        }
                    }
                }
                TopItem::RefImpl(r) => {
                    let params = r
                        .params
                        .iter()
                        .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                        .collect();
                    check_expr_dimensions(
                        &format!("ref_impl `{}`", r.name),
                        &r.body.node,
                        &field_types,
                        &params,
                        &dimensions,
                        &const_literals,
                    )?;
                }
                _ => {}
            }
        }
    }

    // Reject recursive `ref_impl`s (direct or mutual): Lean would emit a
    // non-terminating `def` and fail elaboration — surface at adapt time
    // with a fix-it pointing at structural decomposition.
    check_no_recursive_ref_impls(spec)?;

    Ok(())
}

fn validate_external_fields(spec: &a::Spec) -> anyhow::Result<()> {
    let mut declarations = std::collections::HashMap::new();
    for item in &spec.items {
        let TopItem::Environment(environment) = &item.node else {
            continue;
        };
        for clause in &environment.clauses {
            let a::EnvClause::External { object, field, ty } = &clause.node else {
                continue;
            };
            if object == "state" {
                anyhow::bail!(
                    "environment `{}` external object cannot use reserved namespace `state`",
                    environment.name
                );
            }
            let key = (object.clone(), field.clone());
            let rendered = type_ref_to_string(ty);
            if let Some(previous) = declarations.insert(key, rendered.clone()) {
                if previous != rendered {
                    anyhow::bail!(
                        "external field `{}.{}` has conflicting types `{}` and `{}`",
                        object,
                        field,
                        previous,
                        rendered
                    );
                }
            }
        }
    }
    Ok(())
}

/// External fields are scoped to the environment that declares them: the
/// tree builder classifies a constraint path whose root is an external of a
/// DIFFERENT environment as `Unresolved`, which then renders verbatim into
/// uncompilable Kani/Lean artifacts with no other diagnostic. Reject it here.
fn validate_environment_external_scope(spec: &a::Spec) -> anyhow::Result<()> {
    use std::collections::{BTreeMap, BTreeSet};

    // External object name -> environments that declare it.
    let mut object_owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in &spec.items {
        if let TopItem::Environment(env) = &item.node {
            for clause in &env.clauses {
                if let a::EnvClause::External { object, .. } = &clause.node {
                    object_owners
                        .entry(object.clone())
                        .or_default()
                        .insert(env.name.clone());
                }
            }
        }
    }

    for item in &spec.items {
        let TopItem::Environment(env) = &item.node else {
            continue;
        };
        let local: BTreeSet<&str> = env
            .clauses
            .iter()
            .filter_map(|clause| match &clause.node {
                a::EnvClause::External { object, .. } => Some(object.as_str()),
                _ => None,
            })
            .collect();
        for clause in &env.clauses {
            let a::EnvClause::Constraint(expr) = &clause.node else {
                continue;
            };
            let mut roots = BTreeSet::new();
            collect_path_roots(&expr.node, &mut roots);
            for root in &roots {
                if local.contains(root.as_str()) {
                    continue;
                }
                if let Some(owners) = object_owners.get(root) {
                    if let Some(other) = owners.iter().find(|owner| owner.as_str() != env.name) {
                        anyhow::bail!(
                            "environment `{}` constraint references external `{}`, which is \
                             declared in environment `{}`. Externals are scoped to their own \
                             environment — declare `external {}.<field> : <Ty>` inside `{}`, or \
                             move the constraint.",
                            env.name,
                            root,
                            other,
                            root,
                            env.name
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Collect the root identifier of every `Expr::Path` in `e`. Used to detect
/// external namespace references in environment constraints.
fn collect_path_roots(e: &a::Expr, out: &mut std::collections::BTreeSet<String>) {
    use a::Expr::*;
    let recur = |node: &a::Node<a::Expr>, out: &mut _| collect_path_roots(&node.node, out);
    match e {
        Int(_) | Bool(_) => {}
        Path(p) => {
            out.insert(p.root.clone());
        }
        Old(inner) | Not(inner) | Paren(inner) | Len(inner) => recur(inner, out),
        Sum { body, .. } | Quant { body, .. } => recur(body, out),
        BoolOp { lhs, rhs, .. } | Cmp { lhs, rhs, .. } | Arith { lhs, rhs, .. } => {
            recur(lhs, out);
            recur(rhs, out);
        }
        MulDivFloor { a, b, d } | MulDivCeil { a, b, d } | MulDivRoundHalfUp { a, b, d } => {
            recur(a, out);
            recur(b, out);
            recur(d, out);
        }
        Contains { coll, elem } => {
            recur(coll, out);
            recur(elem, out);
        }
        QuantIn { coll, body, .. } => {
            recur(coll, out);
            recur(body, out);
        }
        Match { scrutinee, arms } => {
            recur(scrutinee, out);
            for arm in arms {
                collect_path_roots(&arm.body.node, out);
            }
        }
        Ctor { payload, .. } => {
            if let Some(payload) = payload {
                recur(payload, out);
            }
        }
        RecordLit(fields) => {
            for (_, value) in fields {
                collect_path_roots(&value.node, out);
            }
        }
        RecordUpdate { base, updates } => {
            recur(base, out);
            for (_, value) in updates {
                collect_path_roots(&value.node, out);
            }
        }
        IsVariant { scrutinee, .. } => recur(scrutinee, out),
        App { args, .. } => {
            for arg in args {
                collect_path_roots(&arg.node, out);
            }
        }
        Field { base, .. } => recur(base, out),
        Let { value, body, .. } => {
            recur(value, out);
            recur(body, out);
        }
        IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            recur(cond, out);
            recur(then_branch, out);
            recur(else_branch, out);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DimensionValue {
    Literal,
    Scalar,
    Unit(String),
    Other,
}

fn validate_dimensions(
    parsed: &ParsedSpec,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    for dimension in &parsed.dimensions {
        if !matches!(
            dimension.base.as_str(),
            "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128"
        ) {
            anyhow::bail!(
                "dimension `{}` must use an integer base type, found `{}`",
                dimension.name,
                dimension.base
            );
        }
        if out
            .insert(dimension.name.clone(), dimension.base.clone())
            .is_some()
        {
            anyhow::bail!("duplicate dimension declaration `{}`", dimension.name);
        }
    }
    Ok(out)
}

fn typecheck_handler_dimensions(
    h: &a::HandlerDecl,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    dimensions: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    let context = format!("handler `{}`", h.name);
    for clause in &h.clauses {
        match &clause.node {
            a::HandlerClause::Effect(blocks) => {
                for stmt in a::flatten_effect_blocks(blocks) {
                    check_effect_dimensions(
                        &context,
                        stmt,
                        field_types,
                        param_types,
                        dimensions,
                        const_literals,
                    )?;
                }
            }
            a::HandlerClause::Requires { guard, .. } => check_expr_dimensions(
                &format!("{context} requires"),
                &guard.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?,
            a::HandlerClause::Ensures(expr) => check_expr_dimensions(
                &format!("{context} ensures"),
                &expr.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?,
            _ => {}
        }
    }
    Ok(())
}

fn check_effect_dimensions(
    context: &str,
    stmt: &a::EffectStmt,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    dimensions: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    check_expr_dimensions(
        context,
        &stmt.rhs.node,
        field_types,
        param_types,
        dimensions,
        const_literals,
    )?;
    let Some(lhs_ty) = resolve_path_type(&stmt.lhs, field_types, param_types) else {
        return Ok(());
    };
    let Some(lhs_unit) = dimensions
        .get_key_value(lhs_ty)
        .map(|(name, _)| name.clone())
    else {
        return Ok(());
    };
    match infer_dimension(
        &stmt.rhs.node,
        field_types,
        param_types,
        dimensions,
        const_literals,
    )? {
        DimensionValue::Literal | DimensionValue::Other => Ok(()),
        DimensionValue::Unit(rhs) if rhs == lhs_unit => Ok(()),
        DimensionValue::Unit(rhs) => anyhow::bail!(
            "{context} effect on `{}` has dimension mismatch: expected `{lhs_unit}`, found `{rhs}`",
            render_path_human(&stmt.lhs)
        ),
        DimensionValue::Scalar => anyhow::bail!(
            "{context} effect on `{}` has dimension mismatch: expected `{lhs_unit}`, found scalar integer",
            render_path_human(&stmt.lhs)
        ),
    }
}

fn check_expr_dimensions(
    context: &str,
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    dimensions: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    if let Expr::Cmp { lhs, rhs, .. } = expr {
        let left = infer_dimension(
            &lhs.node,
            field_types,
            param_types,
            dimensions,
            const_literals,
        )?;
        let right = infer_dimension(
            &rhs.node,
            field_types,
            param_types,
            dimensions,
            const_literals,
        )?;
        require_compatible(context, "comparison", left, right)?;
    }
    if matches!(
        expr,
        Expr::Arith { .. }
            | Expr::MulDivFloor { .. }
            | Expr::MulDivCeil { .. }
            | Expr::MulDivRoundHalfUp { .. }
    ) {
        let _ = infer_dimension(expr, field_types, param_types, dimensions, const_literals)?;
    }
    let mut child_error = None;
    crate::ast::for_each_child(expr, |child| {
        if child_error.is_none() {
            child_error = check_expr_dimensions(
                context,
                &child.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )
            .err();
        }
    });
    if let Some(err) = child_error {
        return Err(err);
    }
    Ok(())
}

fn infer_dimension(
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    dimensions: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<DimensionValue> {
    use DimensionValue::*;
    Ok(match expr {
        Expr::Int(_) => Literal,
        Expr::Path(path) => {
            if numeric_literal_value(expr, const_literals).is_some() {
                Literal
            } else if let Some(ty) = resolve_path_type(path, field_types, param_types) {
                if dimensions.contains_key(ty) {
                    Unit(ty.to_string())
                } else if is_integer_type(ty) {
                    Scalar
                } else {
                    Other
                }
            } else {
                Other
            }
        }
        Expr::Old(inner) | Expr::Paren(inner) => infer_dimension(
            &inner.node,
            field_types,
            param_types,
            dimensions,
            const_literals,
        )?,
        Expr::Arith { op, lhs, rhs } => {
            let left = infer_dimension(
                &lhs.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            let right = infer_dimension(
                &rhs.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            match op {
                a::ArithOp::Add | a::ArithOp::Sub | a::ArithOp::Mod => {
                    combine_additive("arithmetic", left, right)?
                }
                a::ArithOp::Mul => combine_multiply(left, right)?,
                a::ArithOp::Div => combine_divide(left, right)?,
            }
        }
        Expr::MulDivFloor { a, b, d }
        | Expr::MulDivCeil { a, b, d }
        | Expr::MulDivRoundHalfUp { a, b, d } => {
            let av = infer_dimension(
                &a.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            let bv = infer_dimension(
                &b.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            let dv = infer_dimension(
                &d.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            combine_divide(combine_multiply(av, bv)?, dv)?
        }
        Expr::Len(_) => Scalar,
        Expr::Sum { body, .. } => infer_dimension(
            &body.node,
            field_types,
            param_types,
            dimensions,
            const_literals,
        )?,
        Expr::IfThenElse {
            then_branch,
            else_branch,
            ..
        } => {
            let left = infer_dimension(
                &then_branch.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            let right = infer_dimension(
                &else_branch.node,
                field_types,
                param_types,
                dimensions,
                const_literals,
            )?;
            combine_additive("conditional branches", left, right)?
        }
        _ => Other,
    })
}

fn require_compatible(
    context: &str,
    operation: &str,
    left: DimensionValue,
    right: DimensionValue,
) -> anyhow::Result<()> {
    use DimensionValue::*;
    match (&left, &right) {
        (Other, _) | (_, Other) | (Literal, _) | (_, Literal) => Ok(()),
        (Scalar, Scalar) => Ok(()),
        (Unit(a), Unit(b)) if a == b => Ok(()),
        _ => anyhow::bail!(
            "{context} {operation} has dimension mismatch: `{}` versus `{}`",
            dimension_label(&left),
            dimension_label(&right)
        ),
    }
}

fn combine_additive(
    operation: &str,
    left: DimensionValue,
    right: DimensionValue,
) -> anyhow::Result<DimensionValue> {
    use DimensionValue::*;
    require_compatible("expression", operation, left.clone(), right.clone())?;
    Ok(match (left, right) {
        (Other, _) | (_, Other) => Other,
        (Literal, value) | (value, Literal) => value,
        (Scalar, Scalar) => Scalar,
        (Unit(unit), Unit(_)) => Unit(unit),
        _ => Other,
    })
}

fn combine_multiply(left: DimensionValue, right: DimensionValue) -> anyhow::Result<DimensionValue> {
    use DimensionValue::*;
    Ok(match (left, right) {
        (Other, _) | (_, Other) => Other,
        (Literal, Literal) | (Literal, Scalar) | (Scalar, Literal) | (Scalar, Scalar) => Scalar,
        (Unit(unit), Literal | Scalar) | (Literal | Scalar, Unit(unit)) => Unit(unit),
        (Unit(a), Unit(b)) => anyhow::bail!(
            "expression multiplication would create an unsupported compound dimension `{a}*{b}`"
        ),
    })
}

fn combine_divide(left: DimensionValue, right: DimensionValue) -> anyhow::Result<DimensionValue> {
    use DimensionValue::*;
    Ok(match (left, right) {
        (Other, _) | (_, Other) => Other,
        (Literal | Scalar, Literal | Scalar) => Scalar,
        (Unit(unit), Literal | Scalar) => Unit(unit),
        (Unit(a), Unit(b)) if a == b => Scalar,
        (Unit(a), Unit(b)) => anyhow::bail!(
            "expression division has dimension mismatch: `{a}` versus `{b}`"
        ),
        (Literal | Scalar, Unit(unit)) => anyhow::bail!(
            "expression division by dimension `{unit}` would create an unsupported inverse dimension"
        ),
    })
}

fn dimension_label(value: &DimensionValue) -> &str {
    match value {
        DimensionValue::Literal => "numeric literal",
        DimensionValue::Scalar => "scalar integer",
        DimensionValue::Unit(unit) => unit,
        DimensionValue::Other => "non-numeric value",
    }
}

fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128"
    )
}

/// Collect every function name referenced as `Expr::App { func, .. }`
/// anywhere in `expr`. Used by the ref_impl recursion checker — direct
/// and mutual recursion both manifest as a ref_impl body calling
/// another (or itself).
fn collect_app_funcs(expr: &Expr, out: &mut std::collections::HashSet<String>) {
    if let Expr::App { func, .. } = expr {
        out.insert(func.clone());
    }
    crate::ast::for_each_child(expr, |child| collect_app_funcs(&child.node, out));
}

/// Reject recursive `ref_impl`s: build the call graph restricted to
/// ref_impl names and DFS-detect cycles. Direct (`r calls r`) and mutual
/// (`r → s → r`) recursion both fail with a fix-it.
fn check_no_recursive_ref_impls(spec: &a::Spec) -> anyhow::Result<()> {
    // Gather ref_impl names + their bodies.
    let mut ref_impls: Vec<(String, &Node<Expr>)> = Vec::new();
    for Node { node, .. } in &spec.items {
        if let TopItem::RefImpl(r) = node {
            ref_impls.push((r.name.clone(), &r.body));
        }
    }
    if ref_impls.is_empty() {
        return Ok(());
    }
    let ref_impl_names: std::collections::HashSet<String> =
        ref_impls.iter().map(|(n, _)| n.clone()).collect();

    // Build the per-impl set of called ref_impl names. Calls to
    // non-ref_impl functions (builtins, uninterpreted helpers,
    // mul_div_*) are ignored.
    let mut call_graph: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (name, body) in &ref_impls {
        let mut calls = std::collections::HashSet::new();
        collect_app_funcs(&body.node, &mut calls);
        let edges: Vec<String> = calls
            .into_iter()
            .filter(|f| ref_impl_names.contains(f))
            .collect();
        call_graph.insert(name.clone(), edges);
    }

    // DFS cycle detection: WHITE (unvisited), GRAY (on stack), BLACK
    // (fully explored). A back-edge to GRAY signals a cycle.
    enum Color {
        Gray,
        Black,
    }
    let mut color: std::collections::HashMap<String, Color> = std::collections::HashMap::new();

    fn visit(
        node: &str,
        graph: &std::collections::HashMap<String, Vec<String>>,
        color: &mut std::collections::HashMap<String, Color>,
        stack: &mut Vec<String>,
    ) -> anyhow::Result<()> {
        color.insert(node.to_string(), Color::Gray);
        stack.push(node.to_string());
        if let Some(succs) = graph.get(node) {
            for next in succs {
                match color.get(next) {
                    Some(Color::Gray) => {
                        // Cycle. Find the start of the cycle in `stack`.
                        let cycle_start = stack.iter().position(|n| n == next).unwrap_or(0);
                        let mut cycle: Vec<&str> =
                            stack[cycle_start..].iter().map(|s| s.as_str()).collect();
                        cycle.push(next.as_str());
                        let chain = cycle.join(" -> ");
                        anyhow::bail!(
                            "recursive `ref_impl` not supported: {chain}\n\
                             v2.26 rejects direct and mutual recursion in `ref_impl` bodies.\n\
                             Fix: split into a non-recursive helper plus a state-bearing handler.\n\
                             Termination + Lean `def` lowering for recursive refs is a meta-question\n\
                             outside the LP-shape scope this construct targets."
                        );
                    }
                    Some(Color::Black) => continue,
                    None => visit(next, graph, color, stack)?,
                }
            }
        }
        stack.pop();
        color.insert(node.to_string(), Color::Black);
        Ok(())
    }

    for (name, _) in &ref_impls {
        if !color.contains_key(name) {
            let mut stack: Vec<String> = Vec::new();
            visit(name, &call_graph, &mut color, &mut stack)?;
        }
    }

    Ok(())
}

fn collect_numeric_consts(spec: &a::Spec) -> std::collections::HashMap<String, i128> {
    let mut out = std::collections::HashMap::new();
    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Const { name, value } => {
                out.insert(name.clone(), *value);
            }
            TopItem::Pragma(p) => {
                for Node { node, .. } in &p.items {
                    if let TopItem::Const { name, value } = node {
                        out.insert(name.clone(), *value);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Flatten every field declaration in the spec into `name → DSL-type`.
/// State fields, record fields, sum-variant payload fields, and
/// account-type fields all live in the same namespace from the DSL's
/// point of view — the same `state.key` can resolve against any of them.
fn collect_field_types(parsed: &ParsedSpec) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for (n, t) in &parsed.state_fields {
        out.insert(n.clone(), t.clone());
    }
    for rec in &parsed.records {
        for (n, t) in &rec.fields {
            out.insert(n.clone(), t.clone());
        }
    }
    for sum in &parsed.sum_types {
        for v in &sum.variants {
            for (n, t) in &v.fields {
                out.insert(n.clone(), t.clone());
            }
        }
    }
    for acct in &parsed.account_types {
        for (n, t) in &acct.fields {
            out.insert(n.clone(), t.clone());
        }
    }
    out
}

fn typecheck_handler(
    h: &a::HandlerDecl,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    for Node { node, .. } in &h.clauses {
        match node {
            a::HandlerClause::Effect(blocks) => {
                // Typecheck every leaf, including under match.
                for stmt in a::flatten_effect_blocks(blocks) {
                    check_effect_typed(&h.name, stmt, field_types, param_types, const_literals)?;
                }
            }
            a::HandlerClause::Requires { guard, .. } => {
                check_cmp_types(
                    &h.name,
                    "requires",
                    &guard.node,
                    field_types,
                    param_types,
                    const_literals,
                )?;
            }
            a::HandlerClause::Ensures(e) => {
                check_cmp_types(
                    &h.name,
                    "ensures",
                    &e.node,
                    field_types,
                    param_types,
                    const_literals,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Resolve the leaf field of a path like `state.key` or
/// `accounts[i].capital` to its DSL type, if declared.
fn resolve_path_type<'a>(
    p: &a::Path,
    field_types: &'a std::collections::HashMap<String, String>,
    param_types: &'a std::collections::HashMap<String, String>,
) -> Option<&'a str> {
    // Walk the path to find the last `.field` segment — that's the leaf
    // whose declared type matters for assignment/comparison.
    let mut last_field: Option<&str> = None;
    for seg in &p.segments {
        if let a::PathSeg::Field(f) = seg {
            last_field = Some(f.as_str());
        }
    }
    match last_field {
        Some(name) => field_types.get(name).map(String::as_str),
        None => {
            // Bare root identifier — either a handler param or a state
            // field with no segments.
            param_types
                .get(&p.root)
                .map(String::as_str)
                .or_else(|| field_types.get(&p.root).map(String::as_str))
        }
    }
}

fn check_effect_typed(
    handler_name: &str,
    stmt: &a::EffectStmt,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    let lhs_type = match resolve_path_type(&stmt.lhs, field_types, param_types) {
        Some(t) => t,
        None => return Ok(()),
    };
    if matches!(lhs_type, "Pubkey" | "Bytes32" | "Bytes64") {
        if let Some(v) = numeric_literal_value(&stmt.rhs.node, const_literals) {
            anyhow::bail!(
                "handler `{}` effect `{} := {}`: {} field cannot be assigned a numeric literal. \
                 The DSL has no {} literal syntax — use a handler parameter, a constant, \
                 or the spec's `program_id` as the source value.",
                handler_name,
                render_path_human(&stmt.lhs),
                v,
                lhs_type,
                lhs_type
            );
        }
    }
    Ok(())
}

fn check_cmp_types(
    handler_name: &str,
    clause_kind: &str,
    expr: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    match expr {
        Expr::Cmp { lhs, rhs, .. } => {
            check_cmp_pair(
                handler_name,
                clause_kind,
                &lhs.node,
                &rhs.node,
                field_types,
                param_types,
                const_literals,
            )?;
            // Cmp operands are terminal atoms in the DSL (no nested Cmp),
            // so no need to recurse into them.
        }
        Expr::BoolOp { lhs, rhs, .. } => {
            check_cmp_types(
                handler_name,
                clause_kind,
                &lhs.node,
                field_types,
                param_types,
                const_literals,
            )?;
            check_cmp_types(
                handler_name,
                clause_kind,
                &rhs.node,
                field_types,
                param_types,
                const_literals,
            )?;
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => {
            check_cmp_types(
                handler_name,
                clause_kind,
                &inner.node,
                field_types,
                param_types,
                const_literals,
            )?;
        }
        Expr::Quant { body, .. } | Expr::Sum { body, .. } => {
            check_cmp_types(
                handler_name,
                clause_kind,
                &body.node,
                field_types,
                param_types,
                const_literals,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn check_cmp_pair(
    handler_name: &str,
    clause_kind: &str,
    lhs: &Expr,
    rhs: &Expr,
    field_types: &std::collections::HashMap<String, String>,
    param_types: &std::collections::HashMap<String, String>,
    const_literals: &std::collections::HashMap<String, i128>,
) -> anyhow::Result<()> {
    // Try both orientations (LHS Pubkey / RHS Int and vice versa).
    let pubkey_vs_int = |p: &Expr, i: &Expr| -> Option<(String, i128)> {
        let path = match p {
            Expr::Path(path) => path,
            _ => return None,
        };
        let t = resolve_path_type(path, field_types, param_types)?;
        if !matches!(t, "Pubkey" | "Bytes32" | "Bytes64") {
            return None;
        }
        if let Some(v) = numeric_literal_value(i, const_literals) {
            return Some((render_path_human(path), v));
        }
        None
    };
    if let Some((path_str, v)) = pubkey_vs_int(lhs, rhs).or_else(|| pubkey_vs_int(rhs, lhs)) {
        anyhow::bail!(
            "handler `{}` {} compares Pubkey `{}` with numeric literal `{}`. \
             The DSL has no Pubkey-literal syntax — compare against a handler parameter, \
             a constant, or the spec's `program_id` instead.",
            handler_name,
            clause_kind,
            path_str,
            v
        );
    }
    Ok(())
}

fn numeric_literal_value(
    expr: &Expr,
    const_literals: &std::collections::HashMap<String, i128>,
) -> Option<i128> {
    match expr {
        // Integer literals stay non-negative at the AST (`Expr::Int(u128)`);
        // negative literals desugar to `Arith { Sub, Int(0), Int(v) }` and
        // are recognized via the Sub branch below.
        Expr::Int(v) => i128::try_from(*v).ok(),
        Expr::Path(p) if p.segments.is_empty() => const_literals.get(&p.root).copied(),
        Expr::Paren(inner) | Expr::Old(inner) => numeric_literal_value(&inner.node, const_literals),
        Expr::Arith {
            op: a::ArithOp::Sub,
            lhs,
            rhs,
        } => {
            let l = numeric_literal_value(&lhs.node, const_literals)?;
            let r = numeric_literal_value(&rhs.node, const_literals)?;
            l.checked_sub(r)
        }
        _ => None,
    }
}

fn render_path_human(p: &a::Path) -> String {
    p.to_source_string()
}

#[cfg(test)]
mod tests {
    /// F2 regression: `walk_apps` previously skipped `IfThenElse` / `Let`
    /// positions, so helper calls there never entered the
    /// uninterpreted-helper bag (and Lean failed on the unresolved name).
    /// The shared `for_each_child` spine descends everywhere.
    #[test]
    fn walk_apps_descends_into_if_and_let() {
        let spec = r#"
spec WalkerDemo

type State | Active of { count : U64, }

handler bump : State.Active -> State.Active {
  requires (if state.count > 0 then fee(state.count) else 0) <= 100
  requires (let y = rebate(state.count) in y) <= 5
  effect {
    count += 1
  }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(spec).unwrap();
        let names: Vec<&str> = parsed
            .uninterpreted_helpers
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert!(
            names.contains(&"fee"),
            "helper call under IfThenElse must be collected, got {names:?}"
        );
        assert!(
            names.contains(&"rebate"),
            "helper call under Let must be collected, got {names:?}"
        );
    }

    #[test]
    fn nominal_dimensions_accept_same_unit_and_literals() {
        let spec = r#"
spec Units

dimension Lamports = U64
type State = { balance : Lamports, }
type Error | Bad

handler deposit(amount : Lamports) : State -> State {
  permissionless
  requires amount >= 0 else Bad
  effect { balance += amount }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(spec).expect("dimensioned spec");
        assert_eq!(parsed.dimensions.len(), 1);
        assert_eq!(parsed.dimensions[0].name, "Lamports");
        assert!(parsed
            .type_aliases
            .contains(&("Lamports".to_string(), "U64".to_string())));
    }

    #[test]
    fn nominal_dimensions_reject_cross_unit_arithmetic() {
        let spec = r#"
spec Units

dimension Lamports = U64
dimension Tokens = U64
type State = { balance : Lamports, inventory : Tokens, }
type Error | Bad

handler broken : State -> State {
  permissionless
  requires state.balance + state.inventory >= 0 else Bad
}
"#;
        let err = crate::chumsky_adapter::parse_str(spec).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("dimension mismatch"), "{message}");
        assert!(message.contains("Lamports"), "{message}");
        assert!(message.contains("Tokens"), "{message}");
    }

    #[test]
    fn nominal_dimensions_reject_unit_scalar_assignment() {
        let spec = r#"
spec Units

dimension Lamports = U64
type State = { balance : Lamports, raw : U64, }

handler broken : State -> State {
  permissionless
  effect { balance := state.raw }
}
"#;
        let err = crate::chumsky_adapter::parse_str(spec).unwrap_err();
        assert!(format!("{err:#}").contains("expected `Lamports`, found scalar integer"));
    }
}
