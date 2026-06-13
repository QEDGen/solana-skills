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
    match expr {
        Expr::App { func, args } => {
            // Skip the `now()` builtin — it resolves via the support-library
            // axiom `QEDGen.Solana.Valid.now : Nat`; emitting `axiom now :
            // Bool` here would collide at elaboration.
            if func == "now" && args.is_empty() {
                return;
            }
            // `current_epoch()` — same support-library shape as `now`.
            if func == "current_epoch" && args.is_empty() {
                return;
            }
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
            for n in args {
                walk_apps(&n.node, field_types, param_types, out, seen);
            }
        }
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => {
            walk_apps(&lhs.node, field_types, param_types, out, seen);
            walk_apps(&rhs.node, field_types, param_types, out, seen);
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => {
            walk_apps(&inner.node, field_types, param_types, out, seen);
        }
        Expr::Quant { body, .. } | Expr::Sum { body, .. } => {
            walk_apps(&body.node, field_types, param_types, out, seen);
        }
        Expr::MulDivFloor { a, b, d } | Expr::MulDivCeil { a, b, d } => {
            walk_apps(&a.node, field_types, param_types, out, seen);
            walk_apps(&b.node, field_types, param_types, out, seen);
            walk_apps(&d.node, field_types, param_types, out, seen);
        }
        _ => {}
    }
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

    for Node { node, .. } in &spec.items {
        if let TopItem::Handler(h) = node {
            let param_types: std::collections::HashMap<String, String> = h
                .params
                .iter()
                .map(|p| (p.name.clone(), type_ref_to_string(&p.ty)))
                .collect();
            typecheck_handler(h, &field_types, &param_types, &const_literals)?;
        }
    }

    // Reject recursive `ref_impl`s (direct or mutual): Lean would emit a
    // non-terminating `def` and fail elaboration — surface at adapt time
    // with a fix-it pointing at structural decomposition.
    check_no_recursive_ref_impls(spec)?;

    Ok(())
}

/// Collect every function name referenced as `Expr::App { func, .. }`
/// anywhere in `expr`. Used by the ref_impl recursion checker — direct
/// and mutual recursion both manifest as a ref_impl body calling
/// another (or itself).
fn collect_app_funcs(expr: &Expr, out: &mut std::collections::HashSet<String>) {
    match expr {
        Expr::App { func, args } => {
            out.insert(func.clone());
            for n in args {
                collect_app_funcs(&n.node, out);
            }
        }
        Expr::BoolOp { lhs, rhs, .. }
        | Expr::Cmp { lhs, rhs, .. }
        | Expr::Arith { lhs, rhs, .. } => {
            collect_app_funcs(&lhs.node, out);
            collect_app_funcs(&rhs.node, out);
        }
        Expr::Not(inner) | Expr::Paren(inner) | Expr::Old(inner) => {
            collect_app_funcs(&inner.node, out);
        }
        Expr::Quant { body, .. } | Expr::Sum { body, .. } => {
            collect_app_funcs(&body.node, out);
        }
        Expr::MulDivFloor { a, b, d } | Expr::MulDivCeil { a, b, d } => {
            collect_app_funcs(&a.node, out);
            collect_app_funcs(&b.node, out);
            collect_app_funcs(&d.node, out);
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_app_funcs(&cond.node, out);
            collect_app_funcs(&then_branch.node, out);
            collect_app_funcs(&else_branch.node, out);
        }
        Expr::Match { scrutinee, arms } => {
            collect_app_funcs(&scrutinee.node, out);
            for arm in arms {
                collect_app_funcs(&arm.body.node, out);
            }
        }
        Expr::Let { value, body, .. } => {
            collect_app_funcs(&value.node, out);
            collect_app_funcs(&body.node, out);
        }
        Expr::Ctor {
            payload: Some(p), ..
        } => {
            collect_app_funcs(&p.node, out);
        }
        Expr::RecordLit(fields) => {
            for (_, v) in fields {
                collect_app_funcs(&v.node, out);
            }
        }
        Expr::RecordUpdate { base, updates } => {
            collect_app_funcs(&base.node, out);
            for (_, v) in updates {
                collect_app_funcs(&v.node, out);
            }
        }
        Expr::IsVariant { scrutinee, .. } => {
            collect_app_funcs(&scrutinee.node, out);
        }
        Expr::Field { base, .. } => {
            collect_app_funcs(&base.node, out);
        }
        _ => {}
    }
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
    if lhs_type == "Pubkey" {
        if let Some(v) = numeric_literal_value(&stmt.rhs.node, const_literals) {
            anyhow::bail!(
                "handler `{}` effect `{} := {}`: Pubkey field cannot be assigned a numeric literal. \
                 The DSL has no Pubkey-literal syntax — use a handler parameter, a constant, \
                 or the spec's `program_id` as the source pubkey.",
                handler_name,
                render_path_human(&stmt.lhs),
                v
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
        if t != "Pubkey" {
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
    let mut out = p.root.clone();
    for seg in &p.segments {
        match seg {
            a::PathSeg::Field(f) => {
                out.push('.');
                out.push_str(f);
            }
            a::PathSeg::Index(i) => {
                out.push('[');
                out.push_str(i);
                out.push(']');
            }
        }
    }
    out
}
