//! Pubkey reference rewriting and Kani arithmetic normalization: account
//! `.pubkey`/`.key()` rebinding, the `pubkey_eq`/`pubkey_ne` helper
//! emission/rewrites, bps mul/div and checked-add-equality solver-friendly
//! forms, plus the spec-type pubkey classification helpers.

use super::*;

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
        op.requires.iter().any(|req| uses_helper(&req.rust_expr))
            || op
                .ensures
                .iter()
                .any(|ensures| uses_helper(&ensures.rust_expr_binary))
            || op.let_bindings.iter().any(|b| uses_helper(&b.rust_expr))
    }) || spec
        .properties
        .iter()
        .any(|property| property.rust_expression.as_deref().is_some_and(uses_helper))
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

/// True when the named field's declared type is `Pubkey`. Looks in the
/// handler's target account first (multi-account specs), then falls back
/// to global `state_fields`. Returns `false` if the field isn't found
/// (callers default to "not Pubkey" — emit normally) so unknown fields
/// surface as compile errors at the right line, not as silent skips.
pub fn field_type_is_pubkey(field: &str, op: &ParsedHandler, spec: &ParsedSpec) -> bool {
    // Variant-prefixed paths (`Active.owner`) resolve against the variant's
    // payload first; fall through to the flat schema otherwise.
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
