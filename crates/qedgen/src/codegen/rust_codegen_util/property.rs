//! Property translation, arithmetic wrapping, and CBMC solver selection:
//! qedspec property expressions → Rust, `+`/`-` → wrapping/saturating, and
//! the per-effect Kani solver tier picker.

use super::*;

/// A property body as a Rust predicate — tree-native (#151 Slice 1).
/// Renders from the typed tree under the math-exact policy (`Widened`)
/// with the class-appropriate state binder (`Unary` → `s`, `Binary` →
/// `pre`/`post`); falls back to the pre-rendered strings for properties
/// without a tree (bootstrap-synthesized). `None` = description-only.
pub fn property_predicate_rust(prop: &ParsedProperty) -> Option<String> {
    use super::tree_render::{render_rust, ArithMode, Binder, RustCx};
    if let Some(tree) = &prop.tree {
        let binder = match prop.class {
            crate::check::PropertyClass::Unary => Binder::S,
            crate::check::PropertyClass::Binary => Binder::PrePost,
        };
        return Some(render_rust(
            tree,
            RustCx::native()
                .with_binder(binder)
                .with_arith(ArithMode::Widened),
        ));
    }
    prop.rust_expression_math
        .as_deref()
        .filter(|r| !r.is_empty())
        .or(prop.rust_expression.as_deref())
        .map(|r| r.to_string())
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
    for eff in &op.effects {
        let field = &eff.field;
        if eff.op == "add" {
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
    // Try unsigned first; fall through to signed only when a leading `-`
    // rules out the unsigned path, picking the smallest type that fits.
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
    // Fixed-point taint propagation: a binding is "arith-tainted" when its
    // (transitive) RHS contains `*` or `/`. Bounded by the binding count.
    let mut tainted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for b in &op.let_bindings {
        if b.rust_expr.contains('*') || b.rust_expr.contains('/') {
            tainted.insert(b.name.as_str());
        }
    }
    for _ in 0..op.let_bindings.len() {
        let mut changed = false;
        for b in &op.let_bindings {
            if tainted.contains(b.name.as_str()) {
                continue;
            }
            if tainted.iter().any(|t| contains_whole_word(&b.rust_expr, t)) {
                tainted.insert(b.name.as_str());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let rhs_is_arith = rhs.contains('*')
        || rhs.contains('/')
        || tainted.iter().any(|t| contains_whole_word(rhs, t));
    pick_arith_solver(dsl_field_type, rhs_is_arith)
}
