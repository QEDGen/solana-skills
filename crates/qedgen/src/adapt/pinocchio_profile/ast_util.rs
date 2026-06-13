//! Low-level `syn`-expr helpers: statement/expression walking and the small
//! extractors that pull idents, calls, ranges, and seed shapes out of an AST.

use super::*;

pub(super) fn stmt_expr(stmt: &Stmt) -> Option<&Expr> {
    match stmt {
        Stmt::Expr(expr, _) => Some(expr),
        Stmt::Local(local) => local.init.as_ref().map(|init| init.expr.as_ref()),
        _ => None,
    }
}

pub(super) fn walk_exprs_in_stmts<'a>(stmts: &'a [Stmt], visit: &mut impl FnMut(&'a Expr)) {
    for stmt in stmts {
        if let Some(expr) = stmt_expr(stmt) {
            walk_expr(expr, visit);
        }
    }
}

fn walk_expr<'a>(expr: &'a Expr, visit: &mut impl FnMut(&'a Expr)) {
    visit(expr);
    match expr {
        Expr::Array(array) => {
            for elem in &array.elems {
                walk_expr(elem, visit);
            }
        }
        Expr::Assign(assign) => {
            walk_expr(&assign.left, visit);
            walk_expr(&assign.right, visit);
        }
        Expr::Async(expr_async) => walk_exprs_in_stmts(&expr_async.block.stmts, visit),
        Expr::Await(await_expr) => walk_expr(&await_expr.base, visit),
        Expr::Binary(binary) => {
            walk_expr(&binary.left, visit);
            walk_expr(&binary.right, visit);
        }
        Expr::Block(block) => walk_exprs_in_stmts(&block.block.stmts, visit),
        Expr::Break(expr_break) => {
            if let Some(value) = &expr_break.expr {
                walk_expr(value, visit);
            }
        }
        Expr::Call(call) => {
            walk_expr(&call.func, visit);
            for arg in &call.args {
                walk_expr(arg, visit);
            }
        }
        Expr::Cast(cast) => walk_expr(&cast.expr, visit),
        Expr::Closure(closure) => walk_expr(&closure.body, visit),
        Expr::Field(field) => walk_expr(&field.base, visit),
        Expr::ForLoop(loop_expr) => walk_exprs_in_stmts(&loop_expr.body.stmts, visit),
        Expr::Group(group) => walk_expr(&group.expr, visit),
        Expr::If(expr_if) => {
            walk_expr(&expr_if.cond, visit);
            walk_exprs_in_stmts(&expr_if.then_branch.stmts, visit);
            if let Some((_else, else_expr)) = &expr_if.else_branch {
                walk_expr(else_expr, visit);
            }
        }
        Expr::Index(index) => {
            walk_expr(&index.expr, visit);
            walk_expr(&index.index, visit);
        }
        Expr::Let(expr_let) => walk_expr(&expr_let.expr, visit),
        Expr::Loop(loop_expr) => walk_exprs_in_stmts(&loop_expr.body.stmts, visit),
        Expr::Match(expr_match) => {
            walk_expr(&expr_match.expr, visit);
            for arm in &expr_match.arms {
                walk_expr(&arm.body, visit);
            }
        }
        Expr::MethodCall(call) => {
            walk_expr(&call.receiver, visit);
            for arg in &call.args {
                walk_expr(arg, visit);
            }
        }
        Expr::Paren(paren) => walk_expr(&paren.expr, visit),
        Expr::Range(range) => {
            if let Some(start) = &range.start {
                walk_expr(start, visit);
            }
            if let Some(end) = &range.end {
                walk_expr(end, visit);
            }
        }
        Expr::Reference(reference) => walk_expr(&reference.expr, visit),
        Expr::Repeat(repeat) => {
            walk_expr(&repeat.expr, visit);
            walk_expr(&repeat.len, visit);
        }
        Expr::Return(ret) => {
            if let Some(value) = &ret.expr {
                walk_expr(value, visit);
            }
        }
        Expr::Struct(expr_struct) => {
            for field in &expr_struct.fields {
                walk_expr(&field.expr, visit);
            }
            if let Some(rest) = &expr_struct.rest {
                walk_expr(rest, visit);
            }
        }
        Expr::Try(expr_try) => walk_expr(&expr_try.expr, visit),
        Expr::TryBlock(try_block) => walk_exprs_in_stmts(&try_block.block.stmts, visit),
        Expr::Tuple(tuple) => {
            for elem in &tuple.elems {
                walk_expr(elem, visit);
            }
        }
        Expr::Unary(unary) => walk_expr(&unary.expr, visit),
        Expr::Unsafe(unsafe_expr) => walk_exprs_in_stmts(&unsafe_expr.block.stmts, visit),
        Expr::While(while_expr) => {
            walk_expr(&while_expr.cond, visit);
            walk_exprs_in_stmts(&while_expr.body.stmts, visit);
        }
        Expr::Yield(yield_expr) => {
            if let Some(value) = &yield_expr.expr {
                walk_expr(value, visit);
            }
        }
        _ => {}
    }
}

pub(super) fn simple_pat_ident(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(normalize_schema_name(&ident.ident.to_string())),
        Pat::Type(typed) => simple_pat_ident(&typed.pat),
        _ => None,
    }
}

pub(super) fn local_init_calls(init: &Option<syn::LocalInit>, name: &str) -> bool {
    init.as_ref()
        .is_some_and(|init| expr_contains_call_name(&init.expr, name))
}

fn expr_contains_call_name(expr: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |expr| {
        if let Expr::Call(call) = expr {
            if call_name(&call.func).as_deref() == Some(name) {
                found = true;
            }
        }
    });
    found
}

pub(super) fn call_name(func: &Expr) -> Option<String> {
    match func {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Expr::MethodCall(call) => Some(call.method.to_string()),
        _ => None,
    }
}

pub(super) fn expr_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(path) if path.path.segments.len() == 1 => Some(normalize_schema_name(
            &path.path.segments.first()?.ident.to_string(),
        )),
        Expr::Reference(reference) => expr_ident(&reference.expr),
        Expr::Paren(paren) => expr_ident(&paren.expr),
        _ => None,
    }
}

pub(super) fn expr_ref_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Reference(reference) => expr_ident(&reference.expr),
        Expr::Paren(paren) => expr_ref_ident(&paren.expr),
        _ => None,
    }
}

pub(super) fn expr_key_receiver(expr: &Expr) -> Option<String> {
    match expr {
        Expr::MethodCall(call) if call.method == "key" && call.args.is_empty() => {
            expr_ident(&call.receiver)
        }
        Expr::Reference(reference) => expr_key_receiver(&reference.expr),
        Expr::Paren(paren) => expr_key_receiver(&paren.expr),
        _ => None,
    }
}

pub(super) fn expr_mentions_token_program(expr: &Expr) -> bool {
    let rendered = normalize_expr_tokens(expr);
    rendered.contains("SPL_TOKEN_ID")
        || rendered.contains("TOKEN_PROGRAM_ID")
        || rendered.contains("pinocchio_tkn")
}

pub(super) fn derive_call_from_expr(expr: &Expr) -> Option<PinocchioLocalKeyDerivation> {
    match expr {
        Expr::Reference(reference) => derive_call_from_expr(&reference.expr),
        Expr::Paren(paren) => derive_call_from_expr(&paren.expr),
        Expr::Try(expr_try) => derive_call_from_expr(&expr_try.expr),
        Expr::Field(field) => {
            if matches!(&field.member, syn::Member::Unnamed(index) if index.index == 0) {
                derive_call_from_expr(&field.base)
            } else {
                None
            }
        }
        Expr::Call(call) => {
            let fn_name = call_name(&call.func)?;
            let derivation = fn_name.strip_prefix("derive_")?;
            Some(PinocchioLocalKeyDerivation {
                derivation: normalize_schema_name(derivation),
                args: call.args.iter().map(normalize_expr_tokens).collect(),
            })
        }
        _ => None,
    }
}

pub(super) fn wrapper_ctor_arg(expr: &Expr) -> Option<String> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let Expr::Path(path) = &*call.func else {
        return None;
    };
    if path.path.segments.len() != 1 || call.args.len() != 1 {
        return None;
    }
    Some(normalize_ast_expr_alias(call.args.first()?))
}

pub(super) fn normalize_ast_expr_alias(expr: &Expr) -> String {
    match expr {
        Expr::Reference(reference) => normalize_ast_expr_alias(&reference.expr),
        Expr::Unary(unary) => normalize_ast_expr_alias(&unary.expr),
        Expr::Paren(paren) => normalize_ast_expr_alias(&paren.expr),
        Expr::Call(call) if call.args.len() == 1 => normalize_ast_expr_alias(&call.args[0]),
        _ => normalize_expr_tokens(expr),
    }
}

pub(super) fn from_le_bytes_instruction_slice(expr: &Expr) -> Option<(String, usize, usize)> {
    let Expr::Call(call) = peel_expr(expr) else {
        return None;
    };
    let Expr::Path(path) = &*call.func else {
        return None;
    };
    if path.path.segments.last()?.ident != "from_le_bytes" {
        return None;
    }
    let rust_type = path.path.segments.iter().rev().nth(1)?.ident.to_string();
    integer_rust_type(&rust_type)?;
    let arg = call.args.first()?;
    let (start, end) = instruction_data_range(arg)?;
    Some((rust_type, start, end))
}

fn peel_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::Try(expr_try) => peel_expr(&expr_try.expr),
        Expr::Paren(paren) => peel_expr(&paren.expr),
        Expr::Reference(reference) => peel_expr(&reference.expr),
        _ => expr,
    }
}

fn instruction_data_range(expr: &Expr) -> Option<(usize, usize)> {
    match peel_expr(expr) {
        Expr::MethodCall(call) => {
            if call.method == "get" && normalize_expr_tokens(&call.receiver) == "instruction_data" {
                let range = call.args.first()?;
                return literal_range(range);
            }
            if matches!(
                call.method.to_string().as_str(),
                "ok_or" | "map_err" | "try_into"
            ) {
                return instruction_data_range(&call.receiver);
            }
            None
        }
        Expr::Call(call) => call.args.iter().find_map(instruction_data_range),
        _ => None,
    }
}

fn literal_range(expr: &Expr) -> Option<(usize, usize)> {
    let Expr::Range(range) = expr else {
        return None;
    };
    Some((
        usize_lit(range.start.as_deref()?)?,
        usize_lit(range.end.as_deref()?)?,
    ))
}

fn usize_lit(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Lit(lit) => {
            if let syn::Lit::Int(int) = &lit.lit {
                int.base10_parse::<usize>().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(super) fn pat_u8_literal(pat: &Pat) -> Option<u8> {
    match pat {
        Pat::Lit(lit) => {
            if let syn::Lit::Int(int) = &lit.lit {
                return int.base10_parse::<u8>().ok();
            }
            None
        }
        Pat::Or(or) => or.cases.iter().find_map(pat_u8_literal),
        _ => None,
    }
}

pub(super) fn first_process_callee(expr: &Expr) -> Option<String> {
    let mut found = None;
    walk_expr(expr, &mut |expr| {
        if found.is_some() {
            return;
        }
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(fn_name) = call_name(&call.func) else {
            return;
        };
        if let Some(handler) = fn_name.strip_prefix("process_") {
            found = Some(normalize_schema_name(handler));
        }
    });
    found
}

pub(super) fn first_find_program_address_call_from_block(
    block: &syn::Block,
) -> Option<(Vec<String>, String)> {
    let mut found = None;
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        if found.is_some() {
            return;
        }
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(fn_name) = call_name(&call.func) else {
            return;
        };
        if !matches!(
            fn_name.as_str(),
            "find_program_address" | "try_find_program_address"
        ) {
            return;
        }
        if call.args.len() < 2 {
            return;
        }
        let Some(seeds) = seed_exprs_from_arg(&call.args[0]) else {
            return;
        };
        let program_id = normalize_program_id_arg(&call.args[1]);
        if !seeds.is_empty() && !program_id.is_empty() {
            found = Some((seeds, program_id));
        }
    });
    found
}

fn seed_exprs_from_arg(expr: &Expr) -> Option<Vec<String>> {
    let expr = match expr {
        Expr::Reference(reference) => reference.expr.as_ref(),
        _ => expr,
    };
    let Expr::Array(array) = expr else {
        return None;
    };
    Some(array.elems.iter().map(normalize_seed_ast_expr).collect())
}

fn normalize_seed_ast_expr(expr: &Expr) -> String {
    match expr {
        Expr::Reference(reference) => normalize_seed_ast_expr(&reference.expr),
        Expr::Path(path) => normalize_schema_path(&path.path),
        Expr::Array(array) => {
            let inner = array
                .elems
                .iter()
                .map(normalize_expr_tokens)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Expr::MethodCall(call) if call.method == "as_ref" => {
            format!("{}.as_ref()", normalize_seed_ast_expr(&call.receiver))
        }
        _ => normalize_expr_tokens(expr)
            .trim_start_matches("crate::")
            .to_string(),
    }
}

fn normalize_program_id_arg(expr: &Expr) -> String {
    normalize_expr_tokens(expr)
        .trim_start_matches('&')
        .trim()
        .trim_start_matches("crate::")
        .to_string()
}

pub(super) fn parse_syn_fn_params(sig: &syn::Signature) -> Vec<(String, String)> {
    sig.inputs
        .iter()
        .filter_map(|arg| {
            let syn::FnArg::Typed(pat_type) = arg else {
                return None;
            };
            let name = simple_pat_ident(&pat_type.pat)?;
            Some((name, normalize_type_tokens(&pat_type.ty)))
        })
        .collect()
}

fn normalize_type_tokens(ty: &syn::Type) -> String {
    ty.to_token_stream().to_string().replace(' ', "")
}

pub(super) fn normalize_expr_tokens(expr: &Expr) -> String {
    expr.to_token_stream().to_string().replace(' ', "")
}

fn normalize_schema_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
        .trim_start_matches("crate::")
        .to_string()
}
