/// Shared helpers for generating Rust code from qedspec IR.
///
/// Used by both `proptest_gen` and `kani` to avoid duplicating
/// the qedspec-to-Rust translation logic.
use crate::check::{ParsedHandler, ParsedProperty, ParsedSpec};

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
    if let Some(pos) = expr.rfind(" + ") {
        let lhs = &expr[..pos];
        let rhs = &expr[pos + 3..];
        format!("{}.wrapping_add({})", lhs.trim(), rhs.trim())
    } else if let Some(pos) = expr.rfind(" - ") {
        let lhs = &expr[..pos];
        let rhs = &expr[pos + 3..];
        format!("{}.wrapping_sub({})", lhs.trim(), rhs.trim())
    } else {
        expr.to_string()
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
    } else {
        "u64"
    }
}

/// Resolve an effect value to a Rust expression (param name, constant, or literal).
pub fn resolve_value(value: &str, op: &ParsedHandler, spec: &ParsedSpec) -> String {
    if op.takes_params.iter().any(|(n, _)| n == value) {
        value.to_string()
    } else if let Some((_, const_val)) = spec.constants.iter().find(|(n, _)| n == value) {
        const_val.clone()
    } else {
        value.to_string()
    }
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

/// Filter state fields to mutable-only (skip Pubkey identity fields).
pub fn mutable_fields(fields: &[(String, String)]) -> Vec<&(String, String)> {
    fields.iter().filter(|(_, t)| t != "Pubkey").collect()
}

/// Collect all guard conditions from a handler (guard_str + requires clauses)
/// as a single Rust expression. Returns None if no guards exist.
pub fn collect_full_guard(op: &ParsedHandler, wrapping: bool) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ref guard) = op.guard_str {
        parts.push(translate_guard_to_rust(guard, wrapping));
    }
    for req in &op.requires {
        parts.push(translate_guard_to_rust(&req.rust_expr, wrapping));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

// ============================================================================
// Shared emitters
// ============================================================================

/// Emit constant declarations from spec constants.
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

/// Emit a State struct with configurable `#[derive(...)]` attributes.
/// `map_type_fn` converts DSL types (U64, Pubkey, etc.) to Rust types.
pub fn emit_state_struct(
    out: &mut String,
    fields: &[&(String, String)],
    derives: &str,
    map_type_fn: fn(&str) -> &str,
) {
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str("struct State {\n");
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type_fn(ftype)));
    }
    out.push_str("}\n\n");
}

/// Emit property predicate functions from spec properties.
/// `wrapping` controls whether arithmetic expressions use wrapping_add/wrapping_sub.
pub fn emit_property_predicates(out: &mut String, properties: &[ParsedProperty], wrapping: bool) {
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            let rust_expr = translate_property_to_rust(expr, wrapping);
            out.push_str(&format!("/// {}: {}\n", prop.name, expr));
            out.push_str(&format!("fn {}(s: &State) -> bool {{\n", prop.name));
            out.push_str(&format!("    {}\n", rust_expr));
            out.push_str("}\n\n");
        }
    }
}

/// Emit transition functions for handlers. Each returns true if guard passes.
/// `wrapping` controls whether add/sub effects use wrapping arithmetic.
pub fn emit_transition_fn(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    map_type_fn: fn(&str) -> &str,
) {
    if let Some(ref doc) = op.doc {
        out.push_str(&format!("/// {}\n", doc.trim()));
    }

    let params: String = op
        .takes_params
        .iter()
        .map(|(n, t)| format!(", {}: {}", n, map_type_fn(t)))
        .collect();
    out.push_str(&format!(
        "fn {}(s: &mut State{}) -> bool {{\n",
        op.name, params
    ));

    // Guard check (merges guard_str + requires clauses)
    if let Some(guard_expr) = collect_full_guard(op, wrapping) {
        if let Some(ref raw) = op.guard_str {
            out.push_str(&format!("    // guard: {}\n", raw));
        }
        out.push_str(&format!("    if !({}) {{\n", guard_expr));
        out.push_str("        return false;\n");
        out.push_str("    }\n");
    }

    // Apply effects
    for (field, op_kind, value) in &op.effects {
        let rust_value = resolve_value(value, op, spec);
        match op_kind.as_str() {
            "set" => {
                out.push_str(&format!("    s.{} = {};\n", field, rust_value));
            }
            "add" => {
                if wrapping {
                    out.push_str(&format!(
                        "    s.{} = s.{}.wrapping_add({});\n",
                        field, field, rust_value
                    ));
                } else {
                    out.push_str(&format!("    s.{} += {};\n", field, rust_value));
                }
            }
            "sub" => {
                if wrapping {
                    out.push_str(&format!(
                        "    s.{} = s.{}.wrapping_sub({});\n",
                        field, field, rust_value
                    ));
                } else {
                    out.push_str(&format!("    s.{} -= {};\n", field, rust_value));
                }
            }
            _ => {
                out.push_str(&format!(
                    "    // unknown effect: {} {} {}\n",
                    field, op_kind, value
                ));
            }
        }
    }

    out.push_str("    true\n");
    out.push_str("}\n\n");
}
