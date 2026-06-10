/// Shared helpers for generating Rust code from qedspec IR.
///
/// Used by both `proptest_gen` and `kani` to avoid duplicating
/// the qedspec-to-Rust translation logic.
use crate::check::{ParsedHandler, ParsedProperty, ParsedSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardTermSource {
    Guard,
    Requires { error_name: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardTerm {
    pub source: GuardTermSource,
    pub rust_expr: String,
}

/// Translate a qedspec guard expression to Rust syntax.
///
/// Handles: state.field → s.field, Unicode operators → ASCII,
/// Lean `=` equality → Rust `==`.
pub fn translate_guard_to_rust(guard: &str, wrapping: bool) -> String {
    let result = guard
        .replace("state.", "s.")
        .replace('≤', "<=")
        .replace('≥', ">=")
        .replace('∧', "&&")
        .replace('∨', "||")
        .replace('≠', "!=")
        .replace(" and ", " && ")
        .replace(" or ", " || ");
    // Lean uses `=` for equality; Rust needs `==`. Replace standalone ` = `
    // that isn't part of `<=`, `>=`, `!=`, or `==`.
    let result = fix_equality_operator(&result);
    if wrapping {
        wrap_arithmetic(&result)
    } else {
        result
    }
}

/// Rewrite handler-account pubkey references into a generated account
/// environment. `foo.pubkey` becomes `<binder>.foo.pubkey`; `foo.key()` is
/// normalized to the same pubkey field.
pub fn rewrite_account_pubkey_refs(
    expr: &str,
    accounts: &[crate::check::ParsedHandlerAccount],
    binder: &str,
) -> String {
    let mut out = expr.to_string();
    for account in accounts {
        let key_call = format!("{}.key()", account.name);
        let pubkey_ref = format!("{}.pubkey", account.name);
        let replacement = format!("{}.{}.pubkey", binder, account.name);
        out = out.replace(&key_call, &replacement);
        out = out.replace(&pubkey_ref, &replacement);
    }
    out
}

pub fn emit_kani_pubkey_helpers(out: &mut String) {
    out.push_str("#[allow(dead_code)]\n");
    out.push_str("fn pubkey_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {\n");
    out.push_str("    ");
    for i in 0..32 {
        if i > 0 {
            out.push_str(" && ");
        }
        out.push_str(&format!("a[{i}] == b[{i}]"));
    }
    out.push_str("\n}\n\n");
    out.push_str("#[allow(dead_code)]\n");
    out.push_str("fn pubkey_ne(a: &[u8; 32], b: &[u8; 32]) -> bool {\n");
    out.push_str("    !pubkey_eq(a, b)\n");
    out.push_str("}\n\n");
}

pub fn spec_uses_pubkey(spec: &ParsedSpec) -> bool {
    spec.state_fields
        .iter()
        .any(|(_, ty)| type_is_or_contains_pubkey(ty))
        || spec.account_types.iter().any(|acct| {
            acct.fields
                .iter()
                .any(|(_, ty)| type_is_or_contains_pubkey(ty))
                || acct.variants.iter().any(|variant| {
                    variant
                        .fields
                        .iter()
                        .any(|(_, ty)| type_is_or_contains_pubkey(ty))
                })
        })
        || spec.handlers.iter().any(|op| {
            op.takes_params
                .iter()
                .chain(op.abstract_binders.iter())
                .any(|(_, ty)| type_is_or_contains_pubkey(ty))
        })
}

pub fn rewrite_kani_pubkey_comparisons(
    expr: &str,
    op: &ParsedHandler,
    spec: &ParsedSpec,
) -> String {
    let mut out = String::with_capacity(expr.len());
    let mut cursor = 0;

    while let Some((op_start, cmp)) = find_next_equality_op(expr, cursor) {
        let op_end = op_start + cmp.len();
        let lhs_start = find_cmp_lhs_start(expr, op_start);
        let rhs_end = find_cmp_rhs_end(expr, op_end);

        if lhs_start < cursor || rhs_end <= op_end {
            out.push_str(&expr[cursor..op_end]);
            cursor = op_end;
            continue;
        }

        let lhs = expr[lhs_start..op_start].trim();
        let rhs = expr[op_end..rhs_end].trim();
        if kani_operand_is_pubkey(lhs, op, spec) && kani_operand_is_pubkey(rhs, op, spec) {
            out.push_str(&expr[cursor..lhs_start]);
            let helper = if cmp.trim() == "==" {
                "pubkey_eq"
            } else {
                "pubkey_ne"
            };
            out.push_str(&format!("{helper}(&{lhs}, &{rhs})"));
            cursor = rhs_end;
        } else {
            out.push_str(&expr[cursor..op_end]);
            cursor = op_end;
        }
    }

    out.push_str(&expr[cursor..]);
    rewrite_kani_guard_arithmetic(&out)
}

pub fn rewrite_kani_guard_arithmetic(expr: &str) -> String {
    let expr = rewrite_kani_bps_mul_div(expr);
    rewrite_kani_checked_add_equality(&expr)
}

pub fn rewrite_kani_bps_mul_div(expr: &str) -> String {
    let parenthesized = regex::Regex::new(
        r"\((?P<a>[A-Za-z_][A-Za-z0-9_\.]*)\s*\*\s*(?P<b>[A-Za-z_][A-Za-z0-9_\.]*)\)\s*/\s*10000\b",
    )
    .expect("valid bps mul/div regex");
    let rewritten = parenthesized
        .replace_all(expr, "mul_bps_floor_u128($a, $b)")
        .to_string();

    let bare = regex::Regex::new(
        r"\b(?P<a>[A-Za-z_][A-Za-z0-9_\.]*)\s*\*\s*(?P<b>[A-Za-z_][A-Za-z0-9_\.]*)\s*/\s*10000\b",
    )
    .expect("valid bare bps mul/div regex");
    bare.replace_all(&rewritten, "mul_bps_floor_u128($a, $b)")
        .to_string()
}

pub fn rewrite_kani_checked_add_equality(expr: &str) -> String {
    let add_eq = regex::Regex::new(
        r"\b(?P<a>[A-Za-z_][A-Za-z0-9_\.]*)\s*\+\s*(?P<b>[A-Za-z_][A-Za-z0-9_\.]*)\s*(?P<op>==|!=)\s*(?P<c>[A-Za-z_][A-Za-z0-9_\.]*|\d+)\b",
    )
    .expect("valid checked add equality regex");
    add_eq
        .replace_all(expr, "$a.checked_add($b) $op Some($c)")
        .to_string()
}

pub fn spec_uses_kani_bps_mul_div_helper(spec: &ParsedSpec) -> bool {
    let uses_helper = |expr: &str| rewrite_kani_bps_mul_div(expr) != expr;
    spec.handlers.iter().any(|op| {
        op.guard_str
            .as_deref()
            .map(|guard| uses_helper(&translate_guard_to_rust(guard, false)))
            .unwrap_or(false)
            || op.requires.iter().any(|req| uses_helper(&req.rust_expr))
            || op
                .aborts_if
                .iter()
                .any(|abort| uses_helper(&abort.rust_expr))
            || op
                .ensures
                .iter()
                .any(|ensures| uses_helper(&ensures.rust_expr_binary))
            || op
                .let_bindings
                .iter()
                .any(|(_, _, rust_expr)| uses_helper(rust_expr))
    }) || spec
        .properties
        .iter()
        .any(|property| property.rust_expression.as_deref().is_some_and(uses_helper))
}

pub fn negate_simple_top_level_comparison(expr: &str) -> Option<String> {
    let trimmed = strip_balanced_outer_parens(expr.trim());
    if contains_top_level_logical_op(trimmed) {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            _ => {}
        }

        if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 {
            for (op, negated) in [
                ("==", "!="),
                ("!=", "=="),
                (">=", "<"),
                ("<=", ">"),
                (">", "<="),
                ("<", ">="),
            ] {
                if trimmed[i..].starts_with(op) {
                    let lhs = trimmed[..i].trim();
                    let rhs = trimmed[i + op.len()..].trim();
                    if !lhs.is_empty() && !rhs.is_empty() {
                        return Some(format!("{lhs} {negated} {rhs}"));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn contains_top_level_logical_op(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'&' | b'|'
                if i + 1 < bytes.len()
                    && bytes[i + 1] == b
                    && paren_depth == 0
                    && bracket_depth == 0
                    && brace_depth == 0 =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn strip_balanced_outer_parens(mut expr: &str) -> &str {
    loop {
        let trimmed = expr.trim();
        if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
            return trimmed;
        }
        let inner = &trimmed[1..trimmed.len() - 1];
        if split_top_level_and(inner).len() == 1 && outer_parens_are_balanced(trimmed) {
            expr = inner;
        } else {
            return trimmed;
        }
    }
}

fn outer_parens_are_balanced(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, b) in bytes.iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && idx + 1 < bytes.len() {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn find_next_equality_op(expr: &str, from: usize) -> Option<(usize, &'static str)> {
    let eq = expr[from..].find(" == ").map(|p| (from + p, " == "));
    let ne = expr[from..].find(" != ").map(|p| (from + p, " != "));
    match (eq, ne) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn find_cmp_lhs_start(expr: &str, op_start: usize) -> usize {
    let bytes = expr.as_bytes();
    let mut i = op_start;
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }

    let mut depth = 0usize;
    while i > 0 {
        if depth == 0 && i >= 2 && (&expr[i - 2..i] == "&&" || &expr[i - 2..i] == "||") {
            break;
        }

        let b = bytes[i - 1];
        match b {
            b')' => {
                depth += 1;
                i -= 1;
            }
            b'(' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                i -= 1;
            }
            b',' if depth == 0 => break,
            _ => i -= 1,
        }
    }
    i
}

fn find_cmp_rhs_end(expr: &str, op_end: usize) -> usize {
    let bytes = expr.as_bytes();
    let mut i = op_end;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }

    let mut depth = 0usize;
    while i < bytes.len() {
        if depth == 0 && i + 1 < bytes.len() && (&expr[i..i + 2] == "&&" || &expr[i..i + 2] == "||")
        {
            break;
        }

        match bytes[i] {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                i += 1;
            }
            b',' if depth == 0 => break,
            _ => i += 1,
        }
    }
    i
}

fn kani_operand_is_pubkey(operand: &str, op: &ParsedHandler, spec: &ParsedSpec) -> bool {
    let operand = operand.trim().trim_start_matches('&').trim();
    if operand.ends_with(".pubkey") || operand.ends_with(".key()") {
        return true;
    }

    // `pubkey_eq` takes `&[u8; 32]`, so it only applies to a *scalar*
    // pubkey operand: a `Pubkey` field, or an indexed element of a
    // pubkey collection (`members[i]`). A bare reference to a whole
    // collection (`s.members: [[u8; 32]; 32]`) must stay on `==`
    // (derived array `PartialEq`) or it won't typecheck under Kani.
    let indexed = operand.contains('[');

    for prefix in ["s.", "pre.", "post."] {
        if let Some(field) = operand.strip_prefix(prefix) {
            return spec_field_is_scalar_pubkey(field, spec, indexed);
        }
    }

    if let Some(field) = operand.strip_prefix("pre_") {
        return spec_field_is_scalar_pubkey(field, spec, indexed);
    }

    if op
        .takes_params
        .iter()
        .chain(op.abstract_binders.iter())
        .any(|(name, ty)| name == operand && type_is_scalar_pubkey(ty))
    {
        return true;
    }

    spec_field_is_scalar_pubkey(operand, spec, indexed)
}

/// Resolve the declared spec type of a (possibly variant-prefixed,
/// possibly subscripted) field path.
fn spec_field_type<'a>(field: &str, spec: &'a ParsedSpec) -> Option<&'a str> {
    let field = strip_variant_prefix_for_flat_state(field, spec);
    let base = effect_target_base(field.as_str()).to_string();
    if let Some((_, ty)) = spec.state_fields.iter().find(|(name, _)| *name == base) {
        return Some(ty.as_str());
    }
    for acct in &spec.account_types {
        if let Some((_, ty)) = acct.fields.iter().find(|(name, _)| *name == base) {
            return Some(ty.as_str());
        }
        for variant in &acct.variants {
            if let Some((_, ty)) = variant.fields.iter().find(|(name, _)| *name == base) {
                return Some(ty.as_str());
            }
        }
    }
    None
}

/// True when the operand denotes a single `[u8; 32]` pubkey value —
/// eligible for the `pubkey_eq`/`pubkey_ne` rewrite. A pubkey
/// *collection* qualifies only when the operand indexes into it.
fn spec_field_is_scalar_pubkey(field: &str, spec: &ParsedSpec, indexed: bool) -> bool {
    match spec_field_type(field, spec) {
        Some(ty) => type_is_scalar_pubkey(ty) || (indexed && type_is_or_contains_pubkey(ty)),
        None => false,
    }
}

fn type_is_or_contains_pubkey(ty: &str) -> bool {
    ty.contains("Pubkey")
}

/// A single `Pubkey`, as opposed to a collection of them
/// (`Map[N] Pubkey`, `[Pubkey; N]`, `Vec<Pubkey>`, …). Only scalar
/// pubkeys lower to `[u8; 32]` and can be compared with `pubkey_eq`.
fn type_is_scalar_pubkey(ty: &str) -> bool {
    type_is_or_contains_pubkey(ty) && !type_is_pubkey_collection(ty)
}

fn type_is_pubkey_collection(ty: &str) -> bool {
    let ty = ty.trim();
    ty.contains('[') || ty.contains("Map") || ty.contains("Vec") || ty.contains("Array")
}

pub fn is_account_pubkey_ref(expr: &str, accounts: &[crate::check::ParsedHandlerAccount]) -> bool {
    accounts
        .iter()
        .any(|a| expr == format!("{}.pubkey", a.name) || expr == format!("{}.key()", a.name))
}

pub fn handler_needs_account_env(op: &ParsedHandler) -> bool {
    op.requires
        .iter()
        .any(|r| mentions_handler_account_pubkey(&r.rust_expr, &op.accounts))
        || op
            .guard_str
            .as_ref()
            .is_some_and(|g| mentions_handler_account_pubkey(g, &op.accounts))
        || op
            .effects
            .iter()
            .any(|(_, _, value)| is_account_pubkey_ref(value.trim(), &op.accounts))
        || op.effect_branches.as_ref().is_some_and(|branches| {
            branches.arms.iter().any(|arm| {
                arm.effects
                    .iter()
                    .any(|(_, _, value)| is_account_pubkey_ref(value.trim(), &op.accounts))
            })
        })
}

pub fn handler_account_env_struct_name(op_name: &str) -> String {
    let sanitized = crate::codegen_shared::sanitize_ident(op_name);
    let mut out = String::new();
    for part in sanitized.split('_').filter(|p| !p.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        "Handler".to_string()
    } else {
        format!("{}Accounts", out)
    }
}

/// Translate a qedspec property expression to Rust.
pub fn translate_property_to_rust(expr: &str, wrapping: bool) -> String {
    let result = expr
        .replace("state.", "s.")
        .replace('≤', "<=")
        .replace('≥', ">=")
        .replace('∧', "&&")
        .replace('∨', "||")
        .replace('≠', "!=")
        .replace(" and ", " && ")
        .replace(" or ", " || ");
    let result = fix_equality_operator(&result);
    if wrapping {
        wrap_arithmetic(&result)
    } else {
        result
    }
}

/// Fix standalone ` = ` (Lean equality) to ` == ` (Rust equality),
/// without touching compound operators like `<=`, `>=`, `!=`.
fn fix_equality_operator(input: &str) -> String {
    let mut safe = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'='
            && i > 0
            && i + 1 < bytes.len()
            && bytes[i - 1] == b' '
            && bytes[i + 1] == b' '
            && (i < 2 || (bytes[i - 2] != b'<' && bytes[i - 2] != b'>' && bytes[i - 2] != b'!'))
            && (i + 2 >= bytes.len() || bytes[i + 1] != b'=')
        {
            safe.push_str("==");
        } else {
            safe.push(bytes[i] as char);
        }
        i += 1;
    }
    safe
}

/// Convert infix `a + b` and `a - b` to `a.wrapping_add(b)` and `a.wrapping_sub(b)`
/// within comparison sub-expressions. Only transforms arithmetic within individual
/// conjuncts/disjuncts — doesn't break boolean structure.
fn wrap_arithmetic(expr: &str) -> String {
    let parts: Vec<&str> = expr.split(" && ").collect();
    let wrapped: Vec<String> = parts
        .iter()
        .map(|part| {
            let sub_parts: Vec<&str> = part.split(" || ").collect();
            sub_parts
                .iter()
                .map(|sub| wrap_arithmetic_atom(sub.trim()))
                .collect::<Vec<_>>()
                .join(" || ")
        })
        .collect();
    wrapped.join(" && ")
}

fn wrap_arithmetic_atom(atom: &str) -> String {
    for cmp in &[" <= ", " >= ", " < ", " > ", " == ", " != "] {
        if let Some(pos) = atom.find(cmp) {
            let lhs = &atom[..pos];
            let rhs = &atom[pos + cmp.len()..];
            let lhs_wrapped = wrap_arith_expr(lhs.trim());
            let rhs_wrapped = wrap_arith_expr(rhs.trim());
            return format!("{}{}{}", lhs_wrapped, cmp, rhs_wrapped);
        }
    }
    atom.to_string()
}

fn wrap_arith_expr(expr: &str) -> String {
    // Split on the RIGHTMOST top-level (paren-depth-0) ` + ` / ` - ` and
    // recurse on the LHS, so a chained expression lowers left-associatively:
    //   `cap - used - reserved`
    //     → `cap.saturating_sub(used).saturating_sub(reserved)`
    // This matches Lean's left-associative `Nat` subtraction. The previous
    // `rfind` form was both non-recursive (it left the inner operator as a
    // bare infix, e.g. `cap - used.wrapping_sub(reserved)`, mis-grouping the
    // expression) and used `wrapping_sub`, which diverged from the Lean tier
    // on underflow — issue #79 Bug B. Subtraction lowers to `saturating_sub`
    // because Lean models guard/property arithmetic over `Nat`, whose `-`
    // truncates at 0; the proptest predicate must test the same relation the
    // Lean proof discharges. Addition keeps `wrapping_add` (grouping fixed).
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    let mut split: Option<(usize, u8)> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            op @ (b'+' | b'-')
                if depth == 0
                    && i > 0
                    && bytes[i - 1] == b' '
                    && i + 1 < bytes.len()
                    && bytes[i + 1] == b' ' =>
            {
                split = Some((i, op));
            }
            _ => {}
        }
        i += 1;
    }
    match split {
        Some((pos, op)) => {
            let lhs = wrap_arith_expr(expr[..pos].trim());
            let rhs = expr[pos + 1..].trim();
            if op == b'+' {
                format!("{}.wrapping_add({})", lhs, rhs)
            } else {
                format!("{}.saturating_sub({})", lhs, rhs)
            }
        }
        None => expr.to_string(),
    }
}

/// For a field with an "add" effect, find its upper-bound field in property expressions.
/// Property expressions are in Lean form (e.g. `s.approval_count ≤ s.member_count`).
/// Returns the bounding field name if a `field ≤ bound` pattern is found.
pub fn find_upper_bound_field(field: &str, properties: &[ParsedProperty]) -> Option<String> {
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            let norm = expr.replace('\u{2264}', "<=").replace('\u{2265}', ">=");
            let field_pat = format!("s.{}", field);
            if !norm.contains(&field_pat) && !norm.contains(field) {
                continue;
            }
            for segment in norm.split("&&").chain(norm.split('\u{2227}')) {
                let segment = segment.trim();
                if let Some((lhs, rhs)) = segment.split_once("<=") {
                    let lhs = lhs.trim();
                    let rhs = rhs.trim();
                    if lhs.ends_with(field) || lhs == format!("s.{}", field) {
                        let bound = rhs
                            .strip_prefix("s.")
                            .or_else(|| rhs.strip_prefix("state."))
                            .unwrap_or(rhs)
                            .trim();
                        if bound.chars().all(|c| c.is_alphanumeric() || c == '_')
                            && !bound.is_empty()
                            && !bound.chars().next().unwrap().is_ascii_digit()
                        {
                            return Some(bound.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Emit assume statements for add effects with bounded properties.
/// `assume_fmt` controls the output syntax, e.g.:
///   - proptest: `"        prop_assume!(s.{field} < s.{bound}); // strict bound for add\n"`
///   - kani:     `"    kani::assume(s.{field} < s.{bound}); // strict bound: {field} increments\n"`
pub fn emit_add_strict_bounds(
    out: &mut String,
    op: &ParsedHandler,
    properties: &[ParsedProperty],
    assume_fmt: &str,
) {
    for (field, eff_op, _) in &op.effects {
        if eff_op == "add" {
            if let Some(bound) = find_upper_bound_field(field, properties) {
                out.push_str(
                    &assume_fmt
                        .replace("{field}", field)
                        .replace("{bound}", &bound),
                );
            }
        }
    }
}

/// Infer a Rust integer type from a constant's value magnitude.
pub fn infer_const_type(value: &str) -> &'static str {
    let clean_val = value.replace('_', "");
    // v2.29 Slice A (#3): try unsigned first so positive literals
    // keep their pre-v2.29 type (u8 / u16 / …). Fall through to
    // signed only when the leading `-` rules out the unsigned path,
    // matching the smallest signed type that fits.
    if let Ok(v) = clean_val.parse::<u128>() {
        if v <= u8::MAX as u128 {
            "u8"
        } else if v <= u16::MAX as u128 {
            "u16"
        } else if v <= u32::MAX as u128 {
            "u32"
        } else if v <= u64::MAX as u128 {
            "u64"
        } else {
            "u128"
        }
    } else if let Ok(v) = clean_val.parse::<i128>() {
        if v >= i8::MIN as i128 && v <= i8::MAX as i128 {
            "i8"
        } else if v >= i16::MIN as i128 && v <= i16::MAX as i128 {
            "i16"
        } else if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
            "i32"
        } else if v >= i64::MIN as i128 && v <= i64::MAX as i128 {
            "i64"
        } else {
            "i128"
        }
    } else {
        "u64"
    }
}

/// Pick a CBMC backend solver for a Kani effect-conformance harness based on
/// the LHS field type and the RHS expression.
///
/// Returns the content of the `#[kani::solver(...)]` attribute (without the
/// attribute wrapper). The three tiers:
///
/// * **cadical** — scalar / linear effects (no `*` or `/` reachable from the
///   RHS). Default Kani solver; fast on bit-blasted boolean and linear-arith
///   problems.
/// * **minisat** — narrow-type multiplication/division (u8, u16, u32, bool).
///   SAT-level solver that outperforms cadical on multiplication-heavy
///   bit-blasts at narrow widths.
/// * **bin = "z3"** — wide-type multiplication/division (u64, u128, i128).
///   CBMC hands the problem to z3 as an SMT2 solver; z3's bit-vector theory
///   handles nested `*`/`/` chains on 64+ bit types that SAT backends blow up
///   on (the `amount * 125 / 10000 * N / 10000` pattern is the canonical
///   wedge case). Requires `z3` on `PATH` when running `cargo kani --tests`.
///
/// `dsl_field_type` is the DSL-level type string from the spec
/// (`U64`, `U128`, `I128`, `U8`, etc.), pre-`map_type`.
fn pick_arith_solver(dsl_field_type: &str, rhs_is_arithmetic: bool) -> &'static str {
    if !rhs_is_arithmetic {
        return "cadical";
    }
    let is_wide = matches!(dsl_field_type, "U64" | "U128" | "I128");
    if is_wide {
        // CBMC / Kani accepts an external SMT solver via `bin = "<path>"`.
        // Z3 solves bit-vector arithmetic (especially nested mul/div on 64/128
        // bit types) far faster than any SAT backend here.
        "bin = \"z3\""
    } else {
        "minisat"
    }
}

/// Pick a solver for an effect RHS, chasing through the handler's `let`
/// bindings. The canonical heavy-arith pattern hides behind a binding:
///
///     let total_fee = amount * 125 / 10000
///     let net = amount - total_fee
///     effect { pool += net, fees += total_fee }
///
/// Both effect RHSs are bare identifiers. A purely syntactic
/// `pick_kani_solver("U64", "net")` returns cadical and wedges CBMC on
/// a u64 mul/div symbolic exploration. Transitively resolving through the
/// bindings exposes `total_fee`'s mul/div and routes the wide-LHS fields
/// to z3.
pub fn pick_kani_solver_for_effect(
    dsl_field_type: &str,
    rhs: &str,
    op: &ParsedHandler,
) -> &'static str {
    // Compute the set of "arith-tainted" let bindings — bindings whose
    // (transitive) RHS contains a `*` or `/`. Fixed-point iteration: start
    // from direct syntactic hits, then propagate by whole-word containment
    // of an already-tainted name in another binding's RHS. Bounded by the
    // binding count (each pass adds at least one or converges).
    let mut tainted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, _, bound_rhs) in &op.let_bindings {
        if bound_rhs.contains('*') || bound_rhs.contains('/') {
            tainted.insert(name.as_str());
        }
    }
    for _ in 0..op.let_bindings.len() {
        let mut changed = false;
        for (name, _, bound_rhs) in &op.let_bindings {
            if tainted.contains(name.as_str()) {
                continue;
            }
            if tainted.iter().any(|t| contains_whole_word(bound_rhs, t)) {
                tainted.insert(name.as_str());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // An effect RHS is arithmetic if it directly contains `*`/`/` OR it
    // mentions any tainted binding.
    let rhs_is_arith = rhs.contains('*')
        || rhs.contains('/')
        || tainted.iter().any(|t| contains_whole_word(rhs, t));
    pick_arith_solver(dsl_field_type, rhs_is_arith)
}

/// True if `hay` contains `needle` as a whole word (not a substring of a
/// longer identifier). `net` in `amount - net` matches; `net` in `network`
/// does not.
fn contains_whole_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    let n = needle.as_bytes();
    let mut i = 0;
    while i + n.len() <= bytes.len() {
        if &bytes[i..i + n.len()] == n {
            let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after_ok = i + n.len() == bytes.len() || !is_ident_byte(bytes[i + n.len()]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Resolve an effect value to a Rust expression: handler param name,
/// declared constant, state field (rebound to `<state_binder>X` when
/// provided), or pass-through literal.
///
/// Why state fields need a binder: by the time effect-value rendering
/// reaches this fn, upstream effect-RHS rendering has already stripped
/// the `state.` prefix (see chumsky_adapter::render_effect — it unwraps
/// `Expr::FieldAccess { base: state, .. }` to the bare field name so each
/// backend can apply its own state binder). Different targets bind state
/// differently:
///
///   - proptest fn body binds state as `s` (`fn op(s: &mut State, ...)`)
///   - Anchor handler body accesses state via `self.<acct>.<field>`
///   - Lean / Kani may bind differently again
///
/// Without a target-aware binder, a bare `X` for a state field becomes
/// E0425 "cannot find value `X` in this scope" at compile time. Each
/// caller passes the binder appropriate to its emission target; pass
/// `None` to keep the legacy pass-through behavior (bare identifier).
pub fn resolve_value(
    value: &str,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    state_binder: Option<&str>,
) -> String {
    if op.takes_params.iter().any(|(n, _)| n == value) {
        value.to_string()
    } else if let Some((_, const_val)) = spec.constants.iter().find(|(n, _)| n == value) {
        const_val.clone()
    } else if op
        .calls
        .iter()
        .any(|c| c.result_binding.as_deref() == Some(value))
    {
        // v2.24 #11 — `let <name> = call …` binding is in scope for
        // subsequent effects / requires. Render as the bare ident
        // so the generated Rust references the let-bound local.
        value.to_string()
    } else if let Some(binder) = state_binder {
        if is_state_field(value, spec) {
            format!("{}{}", binder, value)
        } else {
            value.to_string()
        }
    } else {
        value.to_string()
    }
}

pub fn resolve_value_with_account_env(
    value: &str,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    state_binder: Option<&str>,
    account_binder: Option<&str>,
) -> String {
    if let Some(binder) = account_binder {
        let rewritten = rewrite_account_pubkey_refs(value, &op.accounts, binder);
        if rewritten != value {
            return rewritten;
        }
    }
    resolve_value(value, op, spec, state_binder)
}

/// True when the bare identifier names a state field in the flat
/// `state_fields` list or any `account_types[*].fields` (multi-account).
fn is_state_field(name: &str, spec: &ParsedSpec) -> bool {
    if spec.state_fields.iter().any(|(n, _)| n == name) {
        return true;
    }
    for acct in &spec.account_types {
        if acct.fields.iter().any(|(n, _)| n == name) {
            return true;
        }
    }
    false
}

// ============================================================================
// Shared helpers — used by kani, proptest, unit_test, integration generators
// ============================================================================

/// Resolve state fields for the spec, handling multi-account layout.
/// Returns the fields for the primary account type.
pub fn resolve_state_fields(spec: &ParsedSpec) -> &[(String, String)] {
    if spec.account_types.len() > 1 {
        &spec.account_types[0].fields
    } else {
        &spec.state_fields
    }
}

/// Filter state fields to mutable-only.
///
/// v2.21 Slice 3: Pubkey fields used to be filtered out (the v2.20 P6
/// workaround) because the proptest / Kani State struct couldn't carry
/// them. With Standalone context now lowering `Pubkey → [u8; 32]`, the
/// fields are first-class and stay in the mutable set — proptest's
/// existing 32-byte-array strategy generates them. The "mutable" naming
/// is historical; today every declared state field flows through here.
pub fn mutable_fields(fields: &[(String, String)]) -> Vec<&(String, String)> {
    fields.iter().collect()
}

/// True when the named field's declared type is `Pubkey`. Looks in the
/// handler's target account first (multi-account specs), then falls back
/// to global `state_fields`. Returns `false` if the field isn't found
/// (callers default to "not Pubkey" — emit normally) so unknown fields
/// surface as compile errors at the right line, not as silent skips.
pub fn field_type_is_pubkey(field: &str, op: &ParsedHandler, spec: &ParsedSpec) -> bool {
    // v2.24 S5d — variant-prefixed paths (`Active.owner`) resolve
    // against the variant's payload, not the wrapper. Look up the
    // type there first; fall through to the flat schema otherwise.
    if let Some(dot) = field.find('.') {
        let head = &field[..dot];
        let rest = &field[dot + 1..];
        let nested_base = effect_target_base(rest);
        for at in &spec.account_types {
            if let Some(variant) = at.variants.iter().find(|v| v.name == head) {
                if let Some((_, t)) = variant.fields.iter().find(|(n, _)| n == nested_base) {
                    return t == "Pubkey";
                }
            }
        }
    }
    let base = effect_target_base(field);
    if let Some(ref acct_name) = op.on_account {
        if let Some(acct) = spec.account_types.iter().find(|a| a.name == *acct_name) {
            if let Some((_, t)) = acct.fields.iter().find(|(n, _)| n == base) {
                return t == "Pubkey";
            }
        }
    }
    spec.state_fields
        .iter()
        .find(|(n, _)| n == base)
        .map(|(_, t)| t == "Pubkey")
        .unwrap_or(false)
}

/// The base field name an effect targets. `accounts[i].active` → `accounts`;
/// `foo.bar` → `foo`; `plain` → `plain`. Used by `check_effect_targets` to
/// look up the target in the declared state schema.
pub fn effect_target_base(path: &str) -> &str {
    let path = path.trim();
    let end = path.find(['[', '.']).unwrap_or(path.len());
    &path[..end]
}

/// v2.24 S5d — strip a leading `<Variant>.` prefix from an effect path
/// when the root names a multi-variant ADT variant on the spec's state.
/// Returns the path unchanged otherwise. Used by proptest / Kani /
/// integration_test harnesses whose flat-`State` model carries fields
/// in their union form (not under variant constructors), so
/// `Active.balance := …` must lower to `s.balance = …`. Owned-string
/// return so callers can pass the result through `&str`-only APIs
/// without lifetime juggling.
pub fn strip_variant_prefix_for_flat_state(path: &str, spec: &ParsedSpec) -> String {
    if let Some(dot) = path.find('.') {
        let head = &path[..dot];
        let is_variant = spec
            .account_types
            .iter()
            .any(|a| a.variants.iter().any(|v| v.name == head));
        if is_variant {
            return path[dot + 1..].to_string();
        }
    }
    path.to_string()
}

/// Render a single `(field, op_kind, value)` triple into Rust at the given
/// indent. Shared between unconditional effect lowering and v2.20's
/// match-arm lowering. The helper writes the trailing newline; the caller
/// controls where the statement sits relative to its surrounding block.
#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &str,
    indent: &str,
) {
    emit_one_effect_inner(out, op, spec, wrapping, field, op_kind, value, indent, None);
}

#[allow(clippy::too_many_arguments)]
fn emit_one_effect_inner(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &str,
    indent: &str,
    account_binder: Option<&str>,
) {
    // v2.24 S5d — proptest / Kani / integration_test all run against a
    // flat `State` struct (the spec's union-of-variant-fields view). A
    // `Variant.field := …` effect from a multi-variant ADT spec must
    // strip the variant prefix here so the body emits `s.field = …`
    // instead of `s.Variant.field = …` (which doesn't compile). The
    // proptest model tracks the variant via `s.status: u8`, set by
    // `emit_transition_fn`'s post-status write — no enum needed in
    // this harness layer.
    let field_owned = strip_variant_prefix_for_flat_state(field, spec);
    let field = field_owned.as_str();
    // proptest / kani body binds state as `s` — pass that binder so a
    // bare state-field RHS (e.g. `bid_buyer := state.rfp_buyer` after
    // upstream strips `state.`) renders as `s.rfp_buyer`. (PR #45 fix #2,
    // generalized to all callers via emit_one_effect rather than per-arm.)
    let rust_value = resolve_value_with_account_env(value, op, spec, Some("s."), account_binder);
    match op_kind {
        "set" => {
            out.push_str(&format!("{indent}s.{field} = {rust_value};\n"));
        }
        "add" => {
            if wrapping {
                out.push_str(&format!(
                    "{indent}s.{field} = s.{field}.wrapping_add({rust_value});\n"
                ));
            } else {
                out.push_str(&format!(
                    "{indent}match s.{field}.checked_add({rust_value}) {{\n\
                     {indent}    Some(__v) => s.{field} = __v,\n\
                     {indent}    None => return false,\n\
                     {indent}}}\n"
                ));
            }
        }
        "add_sat" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.saturating_add({rust_value});\n"
            ));
        }
        "add_wrap" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.wrapping_add({rust_value});\n"
            ));
        }
        "sub" => {
            if wrapping {
                out.push_str(&format!(
                    "{indent}s.{field} = s.{field}.wrapping_sub({rust_value});\n"
                ));
            } else {
                out.push_str(&format!(
                    "{indent}match s.{field}.checked_sub({rust_value}) {{\n\
                     {indent}    Some(__v) => s.{field} = __v,\n\
                     {indent}    None => return false,\n\
                     {indent}}}\n"
                ));
            }
        }
        "sub_sat" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.saturating_sub({rust_value});\n"
            ));
        }
        "sub_wrap" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.wrapping_sub({rust_value});\n"
            ));
        }
        _ => {
            out.push_str(&format!(
                "{indent}// unknown effect: {field} {op_kind} {value}\n"
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect_with_account_env(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &str,
    indent: &str,
    account_binder: &str,
) {
    emit_one_effect_inner(
        out,
        op,
        spec,
        wrapping,
        field,
        op_kind,
        value,
        indent,
        Some(account_binder),
    );
}

/// Verify that every field referenced as an effect target in any handler is
/// declared somewhere in the state schema — either `state_fields` (flat) or
/// one of the per-account `account_types[*].fields` (multi-account) or any
/// sum-type variant payload. Returns a clear error naming the handler and
/// field when a mismatch is found.
///
/// Motivated by v2.6.1 eval (qedgen-bug-report §2, PRD-v2.6.2 G3): the
/// `init_market` handler wrote `admin := p_admin` but `admin` only appeared
/// in a sum-type variant payload that the flat-state renderer didn't see,
/// so codegen emitted `s.admin = …` referencing an undeclared struct field.
/// Catching this at codegen time beats a `cargo check` error 1000 lines
/// into the generated harness.
pub fn check_effect_targets(spec: &ParsedSpec) -> anyhow::Result<()> {
    use std::collections::HashSet;

    // Collect every declared field name from every place fields can live.
    let mut declared: HashSet<&str> = HashSet::new();
    for (n, _) in &spec.state_fields {
        declared.insert(n.as_str());
    }
    for acct in &spec.account_types {
        for (n, _) in &acct.fields {
            declared.insert(n.as_str());
        }
    }
    for rec in &spec.records {
        for (n, _) in &rec.fields {
            declared.insert(n.as_str());
        }
    }
    for sum in &spec.sum_types {
        for variant in &sum.variants {
            for (n, _) in &variant.fields {
                declared.insert(n.as_str());
            }
        }
    }

    // v2.24 S5c — variant-prefixed effect targets (`Active.balance`) are
    // legal under the wrapper-struct + inner-enum codegen. The base
    // matches a variant name on a multi-variant ADT account; the second
    // segment is the actual field. Build a variant-fields index so the
    // check can re-target at the field beneath the variant prefix
    // instead of false-positive-bailing on the variant name.
    let mut variant_fields: std::collections::HashMap<&str, HashSet<&str>> =
        std::collections::HashMap::new();
    for acct in &spec.account_types {
        for variant in &acct.variants {
            let entry = variant_fields.entry(variant.name.as_str()).or_default();
            for (n, _) in &variant.fields {
                entry.insert(n.as_str());
            }
        }
    }

    for handler in &spec.handlers {
        for (field, _kind, _value) in &handler.effects {
            let base = effect_target_base(field);
            // Variant-prefixed: the root is a variant name, so check
            // the field beneath it against that variant's payload.
            if let Some(variant_payload) = variant_fields.get(base) {
                let after = field.trim_start_matches(base).trim_start_matches('.');
                let nested_base = effect_target_base(after);
                if !nested_base.is_empty()
                    && !variant_payload.contains(nested_base)
                    && !declared.contains(nested_base)
                {
                    anyhow::bail!(
                        "handler `{}` writes effect target `{}` but `{}` is not declared in variant `{}`'s payload — add it to the variant or rename the effect",
                        handler.name,
                        field,
                        nested_base,
                        base,
                    );
                }
                continue;
            }
            if !declared.contains(base) {
                // v2.29 Slice C — `state := .Variant { … }` whole-state
                // assignment desugars to per-field variant-prefixed
                // effects at the adapter (chumsky_adapter.rs::
                // render_effect_or_expand_variant_promotion), but
                // non-RecordLit RHS shapes (e.g. `state := .Active some_var`)
                // and unit-variant shapes that survive into codegen
                // still surface a single bare-state effect tuple here.
                // Accept `state` as a base whenever the spec has any
                // multi-variant ADT account type — the cross-variant
                // promotion path either handles it (RecordLit) or
                // bails to a `todo!()` (other shapes) downstream.
                if base == "state" && spec.account_types.iter().any(|a| !a.variants.is_empty()) {
                    continue;
                }
                anyhow::bail!(
                    "handler `{}` writes effect target `{}` but `{}` is not declared in any state, account, record, or sum-variant payload — add it to the state declaration or remove the effect",
                    handler.name,
                    field,
                    base,
                );
            }
        }
    }
    Ok(())
}

/// Collect all guard conditions from a handler (guard_str + requires clauses)
/// as a single Rust expression. Returns None if no guards exist.
///
/// Skips `requires` clauses whose body references `<handler-account>.pubkey`.
/// The proptest / Kani / integration-test models use a simplified `State`
/// struct that drops Pubkey-typed fields (they're not exercisable from a
/// property strategy), so a `requires acct.pubkey == state.pubkey_field`
/// references a state field the model doesn't carry, producing a compile
/// error in the generated harness. The runtime-side check still emits in
/// the real Rust handler via `codegen.rs`; only the property-test
/// projection drops it. Same shape as the lean_gen drop for handler-
/// account pubkey refs.
pub fn collect_full_guard(op: &ParsedHandler, wrapping: bool) -> Option<String> {
    collect_full_guard_with_account_env(op, wrapping, None)
}

pub fn collect_full_guard_with_account_env(
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ref guard) = op.guard_str {
        let translated = translate_guard_to_rust(guard, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        parts.push(format!("({})", translated));
    }
    for req in &op.requires {
        if account_binder.is_none() && mentions_handler_account_pubkey(&req.rust_expr, &op.accounts)
        {
            continue;
        }
        let translated = translate_guard_to_rust(&req.rust_expr, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        parts.push(format!("({})", translated));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

pub fn collect_guard_terms_with_account_env(
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> Vec<GuardTerm> {
    let mut terms = Vec::new();
    if let Some(ref guard) = op.guard_str {
        let translated = translate_guard_to_rust(guard, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        push_split_guard_terms(&mut terms, GuardTermSource::Guard, &translated);
    }
    for req in &op.requires {
        if account_binder.is_none() && mentions_handler_account_pubkey(&req.rust_expr, &op.accounts)
        {
            continue;
        }
        let translated = translate_guard_to_rust(&req.rust_expr, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        push_split_guard_terms(
            &mut terms,
            GuardTermSource::Requires {
                error_name: req.error_name.clone(),
            },
            &translated,
        );
    }
    terms
}

fn push_split_guard_terms(terms: &mut Vec<GuardTerm>, source: GuardTermSource, expr: &str) {
    for term in split_top_level_and(expr) {
        terms.push(GuardTerm {
            source: source.clone(),
            rust_expr: term,
        });
    }
}

pub fn split_top_level_and(expr: &str) -> Vec<String> {
    let bytes = expr.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'&' if i + 1 < bytes.len()
                && bytes[i + 1] == b'&'
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0 =>
            {
                let part = expr[start..i].trim();
                if !part.is_empty() {
                    parts.push(part.to_string());
                }
                i += 2;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    let tail = expr[start..].trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    parts
}

/// True when `expr` mentions `<handler_account>.pubkey` (or `.key()`)
/// anywhere in its body — used to suppress `requires` clauses from
/// property-test guard collection when they reference a handler account
/// (no scope in the simplified State model). The runtime-side check
/// still emits in the real Rust handler.
fn mentions_handler_account_pubkey(
    expr: &str,
    accounts: &[crate::check::ParsedHandlerAccount],
) -> bool {
    accounts.iter().any(|a| {
        let needle_pubkey = format!("{}.pubkey", a.name);
        let needle_key = format!("{}.key()", a.name);
        expr.contains(&needle_pubkey) || expr.contains(&needle_key)
    })
}

// ============================================================================
// Shared emitters
// ============================================================================

/// Emit constant declarations from spec constants.
/// v2.29 Slice A (#8) — emit `let <name>: <T> = <source>;` lines for
/// each `abstract <name> : <T>` clause on a handler, mapping the
/// DSL type through the caller-supplied resolver. `indent` lets
/// callers match their surrounding block (4 / 8 / 12 spaces);
/// `source` is the per-backend symbolic-input expression
/// (`kani::any()` for Kani, `todo!("...")` for Rust scaffolds, etc.).
///
/// Call this after the equivalent takes_params emission so the
/// abstract binders are in scope when the following
/// `kani::assume(<requires>)` / `prop_assume!(...)` reads them.
pub fn emit_abstract_binders(
    out: &mut String,
    handler: &crate::check::ParsedHandler,
    indent: &str,
    source: &str,
    map_ty: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    for (name, ty_str) in &handler.abstract_binders {
        let ty = map_ty(ty_str)?;
        out.push_str(&format!("{}let {}: {} = {};\n", indent, name, ty, source));
    }
    Ok(())
}

/// Emit `let mut s = State { ... };` with every mutable field bound to
/// `kani::any()`. When the per-account lifecycle has ≥2 states, the
/// synthetic `status` field is also `kani::any()` so callers can layer
/// `kani::assume(s.status == Status::<X>)` on top.
///
/// Promoted to `rust_codegen_util` in v2.30 Phase 3c1 so both
/// `kani::generate` and `kani_mir::generate` call the same emitter
/// (byte-equivalence guarantee). Behavior is unchanged from the
/// original `kani.rs`-local copy.
pub fn emit_state_init_symbolic(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
) {
    out.push_str("    let mut s = State {\n");
    for (fname, _) in mutable_fields {
        out.push_str(&format!("        {}: kani::any(),\n", fname));
    }
    if lifecycle_states.len() >= 2 {
        out.push_str("        status: kani::any(),\n");
    }
    out.push_str("    };\n");
}

/// Emit `let mut s = State { ... };` with every mutable field zeroed and the
/// `status` field set to the section's initial lifecycle state. Used by init-
/// handler harnesses (effect/preservation), where the pre-state is the
/// canonical "before initialization" state. Type-aware defaults via
/// `proptest_gen::default_value_for_field`. Promoted to
/// `rust_codegen_util` alongside `emit_state_init_symbolic` so kani.rs +
/// kani_mir.rs share a single source of truth.
pub fn emit_state_init_zeroed(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
    spec: &crate::check::ParsedSpec,
) {
    out.push_str("    let mut s = State {\n");
    for (fname, ftype) in mutable_fields {
        if let Some(default) = crate::proptest_gen_mir::default_value_for_field(ftype, spec) {
            out.push_str(&format!("        {}: {},\n", fname, default));
        }
    }
    if let Some(initial) = lifecycle_states.first() {
        if lifecycle_states.len() >= 2 {
            out.push_str(&format!("        status: Status::{},\n", initial));
        }
    }
    out.push_str("    };\n");
}

/// Append `kani::assume(s.status == Status::<pre>);` when the handler has a
/// pre-status declaration AND this section has a lifecycle. No-op otherwise.
/// Without this, guard-rejection / abort harnesses for lifecycle-gated
/// handlers can pass for the wrong reason — the handler rejects because the
/// symbolic status didn't match the pre-state, not because the requires/
/// guard fired. Promoted from `kani.rs` for kani.rs + kani_mir.rs sharing.
pub fn emit_pre_status_assume(
    out: &mut String,
    op: &crate::check::ParsedHandler,
    lifecycle_states: &[String],
) {
    if lifecycle_states.len() < 2 {
        return;
    }
    if let Some(ref pre) = op.pre_status {
        out.push_str(&format!("    kani::assume(s.status == Status::{});\n", pre));
    }
}

pub fn emit_constants(out: &mut String, constants: &[(String, String)]) {
    for (name, value) in constants {
        let upper = name.to_uppercase();
        let const_type = infer_const_type(value);
        out.push_str(&format!("const {}: {} = {};\n", upper, const_type, value));
    }
    if !constants.is_empty() {
        out.push('\n');
    }
}

/// Emit Rust struct declarations for every user-defined record type in the
/// spec. Called before `emit_state_struct` so the record types are in scope
/// when the State struct references them (e.g. `accounts: [Account; N]`).
///
/// `derives` is the `#[derive(...)]` list to apply to each record. Kani
/// harnesses want `Clone, Copy, kani::Arbitrary`; proptest harnesses want
/// `Debug, Clone, Copy`; unit_test harnesses want `Debug, Clone, PartialEq`.
///
/// Empty-record edge case (records with no fields) are skipped — they're
/// degenerate and not something our specs produce.
pub fn emit_record_structs(
    out: &mut String,
    spec: &crate::check::ParsedSpec,
    derives: &str,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    for rec in &spec.records {
        if rec.fields.is_empty() {
            continue;
        }
        // The `state { … }` / `type State = { … }` flat forms produce a
        // record literally named `State`, which the adapter keeps in
        // `records` for other consumers. The state-machine `struct State`
        // is emitted separately (with lifecycle + ghost fields), so skip
        // the value-record here to avoid a duplicate `struct State`.
        if rec.name == "State" {
            continue;
        }
        out.push_str(&format!("#[derive({})]\n", derives));
        out.push_str(&format!("struct {} {{\n", rec.name));
        for (fname, ftype) in &rec.fields {
            out.push_str(&format!("    {}: {},\n", fname, map_type_fn(ftype)?));
        }
        out.push_str("}\n\n");
    }
    Ok(())
}

/// Emit Rust enum declarations for every sum-type in the spec whose variants
/// are ALL unit (no payload). Classic example: `type Error | NotAdmin | …`
/// becomes `enum Error { NotAdmin, … }`.
///
/// Sum-types with at least one payload-carrying variant (like `type State |
/// Active of { … }`) are intentionally skipped here — the existing codegen
/// path flattens those into a single `struct State { … }` using the first
/// variant's fields, and emitting a conflicting `enum State` would collide.
/// Full enum-State modeling is v2.7 scope.
pub fn emit_unit_enum_sums(
    out: &mut String,
    spec: &crate::check::ParsedSpec,
    derives: &str,
) -> anyhow::Result<()> {
    for sum in &spec.sum_types {
        let all_unit = sum.variants.iter().all(|v| v.fields.is_empty());
        if !all_unit || sum.variants.is_empty() {
            continue;
        }
        out.push_str(&format!("#[derive({})]\n", derives));
        out.push_str(&format!("enum {} {{\n", sum.name));
        for variant in &sum.variants {
            out.push_str(&format!("    {},\n", variant.name));
        }
        out.push_str("}\n\n");
    }
    Ok(())
}

/// True when the spec declares a multi-state lifecycle that the harness layer
/// should model as a `Status` enum + `status: Status` field on the State
/// struct. A single-state lifecycle (or no lifecycle at all) doesn't need a
/// discriminator — the State struct's user fields are the entire model.
pub fn has_lifecycle(spec: &crate::check::ParsedSpec) -> bool {
    spec.lifecycle_states.len() >= 2
}

/// Emit the synthetic `Status` enum derived from `spec.lifecycle_states`.
/// Idempotent: no-op when the spec lacks a multi-state lifecycle.
///
/// The enum is *synthetic* — it isn't declared by the user as `type Status |
/// ...`; it's derived from the variants of the State sum-type (via
/// `lifecycle_states`). This is what lets cover/liveness/effect harnesses
/// actually constrain reachable behavior: without a status field, a
/// lifecycle-only handler's transition function has nothing to write, so
/// every harness against it is vacuous.
/// Emit a Status enum from a per-account or per-spec lifecycle slice.
/// Used by per-account codegen (kani + proptest multi-ADT modes) where
/// each `mod <acct> { ... }` needs its own Status enum populated from
/// `acct.lifecycle` rather than the (single-ADT-flavored) spec-level
/// lifecycle. Fixes a v2.21 regression where multi-ADT specs (lending)
/// emitted `enum Status` with Pool's variants inside both `mod pool` AND
/// `mod loan`, breaking compilation when Loan's transitions referenced its
/// own variant names.
pub fn emit_lifecycle_status_enum_from(
    out: &mut String,
    lifecycle_states: &[String],
    derives: &str,
) {
    if lifecycle_states.len() < 2 {
        return;
    }
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str("enum Status {\n");
    for state in lifecycle_states {
        out.push_str(&format!("    {},\n", state));
    }
    out.push_str("}\n\n");
}

/// Emit a State struct with configurable `#[derive(...)]` attributes.
/// `map_type_fn` converts DSL types (U64, Pubkey, etc.) to Rust types; it
/// returns an error on unrecognized types so codegen fails loudly rather
/// than emitting broken Rust.
///
/// `has_lifecycle` is the multi-state-lifecycle discriminator. Multi-ADT
/// codegen threads the per-account lifecycle (`acct.lifecycle.len() >= 2`)
/// rather than the spec-level one, so each module's State struct gets a
/// `status: Status` field iff that ADT actually has a lifecycle. Callers
/// must have already emitted the `Status` enum via
/// `emit_lifecycle_status_enum_from`.
pub fn emit_state_struct_with_lifecycle(
    out: &mut String,
    fields: &[&(String, String)],
    derives: &str,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
    has_lifecycle: bool,
) -> anyhow::Result<()> {
    // v2.21 Slice 3: Pubkey state fields are now lowered to `[u8; 32]`
    // by `primitive_map` (Standalone context). The v2.20 belt-and-
    // suspenders bail is gone; the field flows through `map_type_fn`
    // and lands as a 32-byte array in the emitted struct.
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str("struct State {\n");
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type_fn(ftype)?));
    }
    if has_lifecycle && !fields.iter().any(|(n, _)| n == "status") {
        out.push_str("    status: Status,\n");
    }
    out.push_str("}\n\n");
    Ok(())
}

/// Emit property predicate functions from spec properties.
/// `wrapping` controls whether arithmetic expressions use wrapping_add/wrapping_sub.
/// Emit `fn {inv_name}(s: &State) -> bool { <rust_expr> }` for each invariant
/// that has a Rust body and is referenced by at least one handler. v2.17.x
/// wire-up: prior to this, `ParsedInvariant.rust_expr` was populated by the
/// adapter but never consumed by any backend; only the Lean theorem path
/// emitted. Description-only invariants (no `rust_expr`) and unsupported
/// quantifier bodies are skipped silently. The caller is expected to
/// pre-filter to invariants that are actually relevant for the current
/// account section / state shape; this fn just emits what it's given.
pub fn emit_invariant_predicates(out: &mut String, invariants: &[&crate::check::ParsedInvariant]) {
    for inv in invariants {
        let Some(rust_expr) = inv.rust_expr.as_deref() else {
            continue;
        };
        if crate::check::rust_expr_is_unsupported(rust_expr) {
            continue;
        }
        let doc_body = inv
            .lean_expr
            .as_deref()
            .map(|le| format!(" — {}", le))
            .unwrap_or_default();
        out.push_str(&format!("/// Invariant: {}{}\n", inv.name, doc_body));
        out.push_str(&format!("fn {}(s: &State) -> bool {{\n", inv.name));
        out.push_str(&format!("    {}\n", rust_expr));
        out.push_str("}\n\n");
    }
}

/// Emit property predicate functions. Threads a `map_type` closure so the
/// per-slot `<prop>_at(s, <binder>)` predicate (v2.20 §S1.1) can render a
/// target-specific binder type — Quasar Pod vs native Rust differ for
/// non-primitive binders.
///
/// v2.20 emission shape:
///   - Always emit `fn <prop>(s: &State) -> bool` — body is the real
///     expression when there's no quantifier (legacy path), or `true` when
///     the body has a quantifier (the harness layer now drives the check
///     via `<prop>_at` instead).
///   - When `prop.per_slot` is Some, additionally emit
///     `fn <prop>_at(s: &State, <binder>: <ty>) -> bool` — body is the
///     `forall` inner expression with the binder kept free. Harnesses
///     declare `<binder>` symbolically and call this predicate, giving the
///     non-vacuous check that was missing pre-v2.20.
pub fn emit_property_predicates_with(
    out: &mut String,
    properties: &[ParsedProperty],
    wrapping: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) {
    for prop in properties {
        // Prefer the AST-rendered Rust form (handles implies/forall correctly,
        // embeds the `QEDGEN_UNSUPPORTED_QUANTIFIER` marker when a body can't
        // lower to a boolean-valued fn). Fall back to the Lean form through
        // `translate_property_to_rust` for callers constructing ParsedProperty
        // without an AST (legacy / tests).
        let rendered = prop
            .rust_expression
            .as_deref()
            .map(|r| r.to_string())
            .or_else(|| {
                prop.expression
                    .as_deref()
                    .map(|e| translate_property_to_rust(e, wrapping))
            });
        let Some(rust_expr) = rendered else { continue };
        let doc = prop.expression.as_deref().unwrap_or("");
        out.push_str(&format!("/// {}: {}\n", prop.name, doc));
        // v2.23 Slice 4: binary properties (body contains `old(...)`)
        // emit `fn p(pre: &State, post: &State) -> bool` — the rust_expression
        // is rendered with `state.x` → `post.x` and `old(state.x)` → `pre.x`
        // by the adapter (see chumsky_adapter `TopItem::Property` arm). Unary
        // properties keep today's single-state signature. Kani's
        // preservation harness (kani.rs::emit_preservation_proofs) captures
        // pre-state and dispatches the assertion arity on `prop.class`.
        let is_binary = prop.class == crate::check::PropertyClass::Binary;
        let sig = if is_binary {
            format!("fn {}(pre: &State, post: &State) -> bool", prop.name)
        } else {
            format!("fn {}(s: &State) -> bool", prop.name)
        };
        // Stubs (unsupported quantifier path) underscore the params so the
        // body `true` doesn't trip `unused_variables`.
        let stub_sig = if is_binary {
            format!("fn {}(_pre: &State, _post: &State) -> bool", prop.name)
        } else {
            format!("fn {}(_s: &State) -> bool", prop.name)
        };
        if crate::check::rust_expr_is_unsupported(&rust_expr) {
            // Body contains `forall`/`exists`. Emit the function with a
            // `unimplemented!()` that cites the limitation — the harness
            // preamble (see kani.rs) skips calling into these predicates.
            out.push_str(&format!("{} {{\n", stub_sig));
            out.push_str(&format!(
                "    // {} — property uses a quantifier; lower at the harness level.\n",
                rust_expr.trim()
            ));
            out.push_str("    true\n");
            out.push_str("}\n\n");
        } else {
            out.push_str(&format!("{} {{\n", sig));
            out.push_str(&format!("    {}\n", rust_expr));
            out.push_str("}\n\n");
        }
        // v2.20 §S1.1: per-slot predicate. The chumsky_adapter populates
        // `per_slot` whenever the property is `forall <binder> : <ty>, body`
        // and the binder type is mechanically lowerable. The harness layer
        // binds `<binder>` symbolically (kani::any / proptest any) and calls
        // `<prop>_at(&s, <binder>)` — sidestepping the "predicate must be
        // bool-valued" constraint that produced the silent `true` stub.
        if let Some(slot) = &prop.per_slot {
            let rust_ty =
                map_type_fn(&slot.binder_type).unwrap_or_else(|_| slot.binder_type.clone());
            out.push_str(&format!(
                "/// {}: per-slot check at `{}: {}` (v2.20 forall lowering)\n",
                prop.name, slot.binder_name, slot.binder_type
            ));
            out.push_str("#[allow(unused_variables)]\n");
            out.push_str(&format!(
                "fn {}_at(s: &State, {}: {}) -> bool {{\n",
                prop.name, slot.binder_name, rust_ty
            ));
            out.push_str(&format!("    {}\n", slot.rust_body));
            out.push_str("}\n\n");
        }
    }
}

/// Emit transition functions for handlers. Each returns true if guard passes.
/// `wrapping` controls whether add/sub effects use wrapping arithmetic.
/// Issue #67 item 4 — emit any `hook after_store(<field>)` assertions that
/// fire after a store to `field`. Anchored right after the field's effect in
/// the runtime transition, so the assertion sees the post-store state. A
/// failed assertion panics, which both the proptest and Kani harnesses
/// surface as a failure / verification violation. On-chain codegen doesn't
/// use this transition emitter, so hooks never reach the program.
fn emit_after_store_hooks(out: &mut String, spec: &ParsedSpec, field: &str, indent: &str) {
    let base = effect_target_base(field);
    for hook in &spec.hooks {
        if let crate::check::ParsedHookKind::AfterStore(f) = &hook.kind {
            if f == base {
                for a in &hook.asserts {
                    out.push_str(&format!(
                        "{}assert!({}, \"hook after_store({}) violated\");\n",
                        indent, a.rust, base
                    ));
                }
            }
        }
    }
}

pub fn emit_transition_fn(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    emit_transition_fn_inner(out, op, spec, wrapping, None, false, map_type_fn)
}

pub fn emit_transition_fn_for_kani(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    let account_env =
        handler_needs_account_env(op).then(|| handler_account_env_struct_name(&op.name));
    emit_transition_fn_inner(
        out,
        op,
        spec,
        wrapping,
        account_env.as_deref(),
        true,
        map_type_fn,
    )
}

fn emit_transition_fn_inner(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    account_env_struct: Option<&str>,
    rewrite_pubkey_comparisons: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    if let Some(ref doc) = op.doc {
        out.push_str(&format!("/// {}\n", doc.trim()));
    }

    let mut params = String::new();
    if let Some(account_env_struct) = account_env_struct {
        params.push_str(&format!(", accounts: &{}", account_env_struct));
    }
    params.push_str(
        &op.takes_params
            .iter()
            .chain(op.abstract_binders.iter())
            .map(|(n, t)| map_type_fn(t).map(|rt| format!(", {}: {}", n, rt)))
            .collect::<anyhow::Result<Vec<_>>>()?
            .concat(),
    );
    // v2.29 Slice A (#8) — abstract binders ride alongside the
    // real handler params in the spec-model transition signature.
    // Callers (Kani / proptest harnesses, integration tests) pass
    // a symbolic / arbitrary value for each binder, and the
    // transition body references it the same way it would
    // reference a real param.
    out.push_str(&format!(
        "fn {}(s: &mut State{}) -> bool {{\n",
        op.name, params
    ));

    // Guard check (merges guard_str + requires clauses)
    if let Some(guard_expr) =
        collect_full_guard_with_account_env(op, wrapping, account_env_struct.map(|_| "accounts"))
    {
        if let Some(ref raw) = op.guard_str {
            out.push_str(&format!("    // guard: {}\n", raw));
        }

        let guard_terms = collect_guard_terms_with_account_env(
            op,
            wrapping,
            account_env_struct.map(|_| "accounts"),
        );
        if rewrite_pubkey_comparisons && guard_terms.len() > 8 {
            for term in guard_terms {
                let term_expr = rewrite_kani_pubkey_comparisons(&term.rust_expr, op, spec);
                if let Some(negated) = negate_simple_top_level_comparison(&term_expr) {
                    out.push_str(&format!("    if {} {{\n", negated));
                } else {
                    out.push_str(&format!("    if !({}) {{\n", term_expr));
                }
                out.push_str("        return false;\n");
                out.push_str("    }\n");
            }
        } else {
            let guard_expr = if rewrite_pubkey_comparisons {
                rewrite_kani_pubkey_comparisons(&guard_expr, op, spec)
            } else {
                guard_expr
            };
            out.push_str(&format!("    if !({}) {{\n", guard_expr));
            out.push_str("        return false;\n");
            out.push_str("    }\n");
        }
    }

    // Pre-status check — handlers declared `State.X -> State.Y` must reject
    // when the current lifecycle state isn't `X`. Without this, lifecycle-
    // only handlers (whose effects don't touch user fields) would have
    // empty bodies and every cover/liveness harness against them would
    // pass tautologically.
    if has_lifecycle(spec) {
        if let Some(ref pre) = op.pre_status {
            out.push_str(&format!("    if s.status != Status::{} {{\n", pre));
            out.push_str("        return false;\n");
            out.push_str("    }\n");
        }
    }

    // Spec-level `let` bindings (`let total_fee = amount * 125 / 10000`)
    // declared in the handler body. Emit them as Rust `let` statements BEFORE
    // the effect block — without this the effect RHS (e.g. `pool += net`)
    // would reference an undefined `net`.
    for (binding_name, _lean_expr, rust_expr) in &op.let_bindings {
        out.push_str(&format!("    let {} = {};\n", binding_name, rust_expr));
    }

    // Apply effects.
    //
    // v2.7 G3 introduces per-effect arithmetic semantics:
    //   `+=`  ("add")       → checked_add, short-circuit via `return false`
    //                         (matches deployed `checked_add(..).ok_or(err)?`)
    //   `+=!` ("add_sat")   → saturating_add
    //   `+=?` ("add_wrap")  → wrapping_add
    //
    // (same three tiers for `-=` / `-=!` / `-=?`).
    //
    // The `wrapping` flag is kept for backward compatibility with proptest's
    // "explore the full state space" mode — when set, default `+=` / `-=`
    // still use wrapping instead of checked. Explicit `+=!` / `+=?` always
    // honor their declared semantics regardless of the caller's mode.
    //
    // Skip effects targeting `Pubkey` fields: `mutable_fields` (the State
    // struct's source of truth) filters them out, and the spec-level
    // RHS (`maker.pubkey` etc.) doesn't have a value in proptest's pure
    // model — accounts aren't carried into the predicate layer. Pubkey
    // identity is validated by the Anchor accounts struct at handler
    // entry, not in the random-state machine. Matches v2.11 brownfield
    // findings on token-fundraiser.
    // v2.20 §S1.2: when the spec uses `match` inside `effect { … }`, the
    // adapter populates `op.effect_branches` and `op.effects` carries the
    // *union* of every arm's effects (for back-compat with pre-v2.20
    // readers). Emit a real Rust `match` block from `effect_branches`
    // when present; otherwise fall through to the flat list as before.
    if let Some(branches) = &op.effect_branches {
        out.push_str(&format!("    match {} {{\n", branches.scrutinee_rust));
        let has_wildcard = branches.arms.iter().any(|a| a.is_wildcard);
        for arm in &branches.arms {
            out.push_str(&format!("        {} => {{\n", arm.pattern_rust));
            for (field, op_kind, value) in &arm.effects {
                if account_env_struct.is_none() && field_type_is_pubkey(field, op, spec) {
                    continue;
                }
                if account_env_struct.is_some() {
                    emit_one_effect_with_account_env(
                        out,
                        op,
                        spec,
                        wrapping,
                        field,
                        op_kind,
                        value,
                        "            ",
                        "accounts",
                    );
                } else {
                    emit_one_effect(
                        out,
                        op,
                        spec,
                        wrapping,
                        field,
                        op_kind,
                        value,
                        "            ",
                    );
                }
                emit_after_store_hooks(out, spec, field, "            ");
            }
            out.push_str("        }\n");
        }
        if !has_wildcard {
            // Without a `_` arm Rust requires exhaustive match. Spec
            // patterns are literal-only in v2.20, so we synthesize a
            // wildcard that no-ops — codegen guarantees the harness
            // compiles even if the spec author forgot the catch-all.
            // The drift hash still records the spec's actual arms.
            out.push_str("        _ => {}\n");
        }
        out.push_str("    }\n");
    } else {
        // PR #45 fix #2: `emit_one_effect` resolves state-field idents
        // via `resolve_value(..., Some("s."))` so a bare state-field RHS
        // (e.g. `bid_buyer := state.rfp_buyer` after upstream strips
        // `state.`) renders as `s.rfp_buyer` in the proptest body.
        for (field, op_kind, value) in &op.effects {
            if account_env_struct.is_none() && field_type_is_pubkey(field, op, spec) {
                continue;
            }
            if account_env_struct.is_some() {
                emit_one_effect_with_account_env(
                    out, op, spec, wrapping, field, op_kind, value, "    ", "accounts",
                );
            } else {
                emit_one_effect(out, op, spec, wrapping, field, op_kind, value, "    ");
            }
            emit_after_store_hooks(out, spec, field, "    ");
        }
    }

    // Post-status assignment — drives the lifecycle transition declared in
    // the handler signature (`State.X -> State.Y`). Combined with the pre-
    // status check above, this turns lifecycle-only handlers into real
    // state machines instead of `fn h() -> bool { true }` stubs.
    if has_lifecycle(spec) {
        if let Some(ref post) = op.post_status {
            out.push_str(&format!("    s.status = Status::{};\n", post));
        }
    }

    // Issue #67 item 3 — ghost (spec-only) field updates. A ghost with an
    // `on <this handler>` clause assigns its new value after the normal
    // effects; ghosts without a clause are left unchanged (frame). The
    // value reads `s.<ghost>` + handler params, matching the Lean
    // transition. Arithmetic wraps under `cargo test --release` (the
    // `verify --proptest` path), so an arbitrary-state aggregate never
    // panics on model overflow.
    for ghost in &spec.ghosts {
        for u in &ghost.updates {
            if u.handler == op.name {
                out.push_str(&format!("    s.{} = {};\n", ghost.name, u.value_rust));
            }
        }
    }

    out.push_str("    true\n");
    out.push_str("}\n\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter::parse_str;

    #[test]
    fn effect_target_base_strips_subscripts_and_dots() {
        assert_eq!(effect_target_base("plain"), "plain");
        assert_eq!(effect_target_base("accounts[i].active"), "accounts");
        assert_eq!(effect_target_base("s.foo"), "s");
        assert_eq!(effect_target_base("map[0]"), "map");
        assert_eq!(effect_target_base("  padded  "), "padded");
    }

    #[test]
    fn chained_subtraction_lowers_left_assoc_saturating() {
        // Issue #79 Bug B: `cap - used - reserved` is left-associative
        // `Nat` subtraction in the Lean tier. The proptest guard must
        // lower it as `cap.saturating_sub(used).saturating_sub(reserved)`
        // — recursive, correctly grouped, and saturating (not the old
        // `cap - used.wrapping_sub(reserved)`, which both mis-grouped and
        // diverged from Lean on underflow).
        let got =
            translate_guard_to_rust("amount <= state.cap - state.used - state.reserved", true);
        assert_eq!(
            got,
            "amount <= s.cap.saturating_sub(s.used).saturating_sub(s.reserved)"
        );
        // Addition keeps left-assoc grouping (wrapping unchanged).
        assert_eq!(
            translate_guard_to_rust("state.a + state.b + state.c <= state.d", true),
            "s.a.wrapping_add(s.b).wrapping_add(s.c) <= s.d"
        );
    }

    #[test]
    fn emit_transition_fn_default_add_emits_checked() {
        // v2.7 G3: `pool += amount` defaults to checked semantics — overflow
        // short-circuits the transition via `return false`. Matches deployed
        // `checked_add(..).ok_or(err)?` in Anchor programs.
        let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool += amount } }
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let mut out = String::new();
        emit_transition_fn(&mut out, op, &spec, false, |t| {
            crate::codegen_shared::map_type(t, &spec)
        })
        .expect("emit");
        assert!(
            out.contains("checked_add(amount)"),
            "default `+=` should emit checked_add: {out}"
        );
        assert!(
            out.contains("None => return false"),
            "checked should short-circuit on None: {out}"
        );
        assert!(
            !out.contains("wrapping_add") && !out.contains("saturating_add"),
            "default `+=` should NOT emit wrapping/saturating: {out}"
        );
    }

    #[test]
    fn emit_transition_fn_saturating_add_emits_saturating() {
        let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool +=! amount } }
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let mut out = String::new();
        emit_transition_fn(&mut out, op, &spec, false, |t| {
            crate::codegen_shared::map_type(t, &spec)
        })
        .expect("emit");
        assert!(
            out.contains("saturating_add(amount)"),
            "`+=!` should emit saturating_add: {out}"
        );
        assert!(
            !out.contains("checked_add") && !out.contains("wrapping_add"),
            "`+=!` should NOT emit checked/wrapping: {out}"
        );
    }

    #[test]
    fn emit_transition_fn_wrapping_add_emits_wrapping() {
        let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool +=? amount } }
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let mut out = String::new();
        emit_transition_fn(&mut out, op, &spec, false, |t| {
            crate::codegen_shared::map_type(t, &spec)
        })
        .expect("emit");
        assert!(
            out.contains("wrapping_add(amount)"),
            "`+=?` should emit wrapping_add: {out}"
        );
        assert!(
            !out.contains("checked_add") && !out.contains("saturating_add"),
            "`+=?` should NOT emit checked/saturating: {out}"
        );
    }

    #[test]
    fn emit_transition_fn_sub_three_tiers() {
        // Mirror: `-=` / `-=!` / `-=?` emit checked / saturating / wrapping.
        for (op_str, expected) in &[
            ("-=", "checked_sub(amount)"),
            ("-=!", "saturating_sub(amount)"),
            ("-=?", "wrapping_sub(amount)"),
        ] {
            let src = format!(
                "spec T\nstate {{ pool : U64 }}\nhandler buy (amount : U64) {{ effect {{ pool {op_str} amount }} }}\n"
            );
            let spec = parse_str(&src).expect("parse");
            let op = &spec.handlers[0];
            let mut out = String::new();
            emit_transition_fn(&mut out, op, &spec, false, |t| {
                crate::codegen_shared::map_type(t, &spec)
            })
            .expect("emit");
            assert!(
                out.contains(expected),
                "`{op_str}` should emit {expected}:\n{out}"
            );
        }
    }

    #[test]
    fn emit_transition_fn_lifecycle_emits_status_guard_and_assignment() {
        // Spec with a multi-state lifecycle. `transition` declares `Open ->
        // Closed`, so the generated transition fn must (1) reject when the
        // current status isn't `Open`, and (2) write `Status::Closed` on
        // success. Without these, lifecycle-only handlers compile to
        // `fn h() -> bool { true }` and every cover/liveness harness
        // against them passes vacuously.
        let src = r#"spec T
type State
  | Open of { x : U64 }
  | Closed
handler close : State.Open -> State.Closed { effect { x := 0 } }
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let mut out = String::new();
        emit_transition_fn(&mut out, op, &spec, false, |t| {
            crate::codegen_shared::map_type(t, &spec)
        })
        .expect("emit");
        assert!(
            out.contains("if s.status != Status::Open"),
            "lifecycle handler must reject when status mismatches pre_status:\n{out}"
        );
        assert!(
            out.contains("s.status = Status::Closed;"),
            "lifecycle handler must drive post_status assignment:\n{out}"
        );
    }

    #[test]
    fn emit_transition_fn_no_lifecycle_skips_status_lines() {
        // Spec without a multi-state lifecycle (single State variant or
        // flat record). emit_transition_fn must NOT emit any status guard
        // or assignment — there's no Status enum to reference.
        let src = r#"spec T
state { balance : U64 }
handler deposit (amount : U64) { effect { balance += amount } }
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let mut out = String::new();
        emit_transition_fn(&mut out, op, &spec, false, |t| {
            crate::codegen_shared::map_type(t, &spec)
        })
        .expect("emit");
        assert!(
            !out.contains("Status::"),
            "lifecycle-free spec must not reference Status:\n{out}"
        );
    }

    #[test]
    fn emit_state_struct_appends_status_when_lifecycle_present() {
        let src = r#"spec T
type State
  | Open of { x : U64 }
  | Closed
handler close : State.Open -> State.Closed { effect { x := 0 } }
"#;
        let spec = parse_str(src).expect("parse");
        let mutable = mutable_fields(&spec.state_fields);
        let mut out = String::new();
        emit_state_struct_with_lifecycle(
            &mut out,
            &mutable,
            "Clone, Copy",
            |t| Ok(t.to_string()),
            has_lifecycle(&spec),
        )
        .expect("emit");
        assert!(
            out.contains("status: Status,"),
            "lifecycle spec must inject `status: Status` field:\n{out}"
        );
    }

    #[test]
    fn kani_pubkey_rewrite_handles_account_and_state_fields() {
        let src = r#"spec T
type State | Active of { admin_key : Pubkey }
type Error | Unauthorized
handler set_admin : State.Active -> State.Active {
  accounts { admin : signer }
  requires admin.pubkey == state.admin_key else Unauthorized
  effect { admin_key := admin.pubkey }
}
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let expr = "(accounts.admin.pubkey == s.admin_key) && (amount > 0)";
        let rewritten = rewrite_kani_pubkey_comparisons(expr, op, &spec);
        assert_eq!(
            rewritten,
            "(pubkey_eq(&accounts.admin.pubkey, &s.admin_key)) && (amount > 0)"
        );
    }

    #[test]
    fn kani_pubkey_rewrite_handles_indexed_pubkey_arrays() {
        let src = r#"spec T
const MAX_MEMBERS = 32
type State | Active of { members : Map[MAX_MEMBERS] Pubkey }
handler approve (member_index : U8) (approver : Pubkey) : State.Active -> State.Active {
  requires state.members[member_index] == approver
}
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let expr = "(s.members[(member_index) as usize] == approver) && (member_index < 32)";
        let rewritten = rewrite_kani_pubkey_comparisons(expr, op, &spec);
        assert_eq!(
            rewritten,
            "(pubkey_eq(&s.members[(member_index) as usize], &approver)) && (member_index < 32)"
        );
    }

    #[test]
    fn split_top_level_and_splits_only_balanced_top_level_terms() {
        assert_eq!(
            split_top_level_and("amount > 0 && fee_bps <= 100 && min_out > 0"),
            vec!["amount > 0", "fee_bps <= 100", "min_out > 0"]
        );
        assert_eq!(
            split_top_level_and("(amount > 0 && fee_bps <= 100) && min_out > 0"),
            vec!["(amount > 0 && fee_bps <= 100)", "min_out > 0"]
        );
        assert_eq!(
            split_top_level_and(
                "is_allowed(mints[(lane) as usize] == mint && lane < 32) && amount > 0"
            ),
            vec![
                "is_allowed(mints[(lane) as usize] == mint && lane < 32)",
                "amount > 0"
            ]
        );
    }

    #[test]
    fn collect_guard_terms_splits_guard_and_requires_without_nested_or_splits() {
        let src = r#"spec T
type State | Active of { admin_key : Pubkey, allowed : Bool }
type Error | Unauthorized | InvalidAmount
handler swap (amount : U64) (min_out : U64) : State.Active -> State.Active {
  accounts { admin : signer }
  requires admin.pubkey == state.admin_key else Unauthorized
  requires amount >= min_out and min_out > 0 else InvalidAmount
}
"#;
        let mut spec = parse_str(src).expect("parse");
        spec.handlers[0].guard_str = Some("state.allowed && (amount > 0 || min_out > 0)".into());
        let op = &spec.handlers[0];
        let terms = collect_guard_terms_with_account_env(op, false, Some("accounts"));
        let exprs = terms
            .into_iter()
            .map(|term| term.rust_expr)
            .collect::<Vec<_>>();
        assert_eq!(
            exprs,
            vec![
                "s.allowed",
                "(amount > 0 || min_out > 0)",
                "accounts.admin.pubkey == s.admin_key",
                "(amount >= min_out)",
                "(min_out > 0)",
            ]
        );
    }

    #[test]
    fn rewrite_kani_bps_mul_div_uses_solver_friendly_helper() {
        assert_eq!(
            rewrite_kani_bps_mul_div(
                "fee_output_normalized >= (fee_input_normalized * retained_value_bps) / 10000"
            ),
            "fee_output_normalized >= mul_bps_floor_u128(fee_input_normalized, retained_value_bps)"
        );
        assert_eq!(
            rewrite_kani_bps_mul_div("amount_in * fee_bps / 10000 <= amount_in"),
            "mul_bps_floor_u128(amount_in, fee_bps) <= amount_in"
        );
        assert_eq!(
            rewrite_kani_bps_mul_div("(a + b) * fee_bps / 10000 <= a"),
            "(a + b) * fee_bps / 10000 <= a"
        );
    }

    #[test]
    fn rewrite_kani_checked_add_equality_avoids_overflow_checks() {
        assert_eq!(
            rewrite_kani_guard_arithmetic("max_fee_bps + retained_value_bps == 10000"),
            "max_fee_bps.checked_add(retained_value_bps) == Some(10000)"
        );
        assert_eq!(
            rewrite_kani_guard_arithmetic(
                "fee_output_normalized >= (fee_input_normalized * retained_value_bps) / 10000 && max_fee_bps + retained_value_bps == 10000"
            ),
            "fee_output_normalized >= mul_bps_floor_u128(fee_input_normalized, retained_value_bps) && max_fee_bps.checked_add(retained_value_bps) == Some(10000)"
        );
    }

    #[test]
    fn negate_simple_top_level_comparison_flips_only_outer_operator() {
        assert_eq!(
            negate_simple_top_level_comparison(
                "fee_output_normalized >= mul_bps_floor_u128(fee_input_normalized, retained_value_bps)"
            ),
            Some(
                "fee_output_normalized < mul_bps_floor_u128(fee_input_normalized, retained_value_bps)"
                    .to_string()
            )
        );
        assert_eq!(
            negate_simple_top_level_comparison(
                "(mul_bps_floor_u128(amount_in, fee_bps) <= amount_in)"
            ),
            Some("mul_bps_floor_u128(amount_in, fee_bps) > amount_in".to_string())
        );
        assert_eq!(
            negate_simple_top_level_comparison(
                "pubkey_eq(&accounts.input_mint.pubkey, &s.allowed_mint_0) || amount > 0"
            ),
            None
        );
    }

    #[test]
    fn check_effect_targets_accepts_declared_fields() {
        let src = r#"spec T
state { balance : U64 }
handler deposit (amount : U64) {
  effect { balance += amount }
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(check_effect_targets(&spec).is_ok());
    }

    #[test]
    fn check_effect_targets_errors_on_undeclared_target() {
        // Effect writes `phantom` but the state declares only `balance` —
        // mirrors the v2.6.1 eval's "writes s.admin but admin not declared"
        // class of bugs. The error must name the handler and the bad field.
        let src = r#"spec T
state { balance : U64 }
handler bogus (amount : U64) {
  effect { phantom := amount }
}"#;
        let spec = parse_str(src).expect("parse");
        let err = check_effect_targets(&spec).unwrap_err().to_string();
        assert!(err.contains("bogus"), "should name handler: {err}");
        assert!(err.contains("phantom"), "should name field: {err}");
    }
}
