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

/// The base field name an effect targets. `accounts[i].active` → `accounts`;
/// `foo.bar` → `foo`; `plain` → `plain`. Used by `check_effect_targets` to
/// look up the target in the declared state schema.
pub fn effect_target_base(path: &str) -> &str {
    let path = path.trim();
    let end = path.find(['[', '.']).unwrap_or(path.len());
    &path[..end]
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

    for handler in &spec.handlers {
        for (field, _kind, _value) in &handler.effects {
            let base = effect_target_base(field);
            if !declared.contains(base) {
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

/// Emit a State struct with configurable `#[derive(...)]` attributes.
/// `map_type_fn` converts DSL types (U64, Pubkey, etc.) to Rust types; it
/// returns an error on unrecognized types so codegen fails loudly rather
/// than emitting broken Rust.
pub fn emit_state_struct(
    out: &mut String,
    fields: &[&(String, String)],
    derives: &str,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str("struct State {\n");
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type_fn(ftype)?));
    }
    out.push_str("}\n\n");
    Ok(())
}

/// Emit property predicate functions from spec properties.
/// `wrapping` controls whether arithmetic expressions use wrapping_add/wrapping_sub.
pub fn emit_property_predicates(out: &mut String, properties: &[ParsedProperty], wrapping: bool) {
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
        if crate::check::rust_expr_is_unsupported(&rust_expr) {
            // Body contains `forall`/`exists`. Emit the function with a
            // `unimplemented!()` that cites the limitation — the harness
            // preamble (see kani.rs) skips calling into these predicates.
            out.push_str(&format!("fn {}(_s: &State) -> bool {{\n", prop.name));
            out.push_str(&format!(
                "    // {} — property uses a quantifier; lower at the harness level.\n",
                rust_expr.trim()
            ));
            out.push_str("    true\n");
            out.push_str("}\n\n");
        } else {
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
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    if let Some(ref doc) = op.doc {
        out.push_str(&format!("/// {}\n", doc.trim()));
    }

    let params: String = op
        .takes_params
        .iter()
        .map(|(n, t)| map_type_fn(t).map(|rt| format!(", {}: {}", n, rt)))
        .collect::<anyhow::Result<Vec<_>>>()?
        .concat();
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

    // Spec-level `let` bindings (`let total_fee = amount * 125 / 10000`)
    // declared in the handler body. Emit them as Rust `let` statements BEFORE
    // the effect block — without this the effect RHS (e.g. `pool += net`)
    // would reference an undefined `net`.
    for (binding_name, _lean_expr, rust_expr) in &op.let_bindings {
        out.push_str(&format!("    let {} = {};\n", binding_name, rust_expr));
    }

    // Apply effects. v2.6 default is `checked` semantics for `+=` / `-=`:
    // on overflow/underflow the transition returns `false`, mirroring the
    // `checked_add(..).ok_or(MathOverflow)?` pattern that real Anchor programs
    // use. Previously this path emitted bare `s.x += val`, which made symbolic
    // BMC (Kani) flag overflow on every unbounded pre-state — a spec-model
    // artifact, not a program bug. See B10 in the v2.6 release notes.
    //
    // `wrapping = true` keeps the old wrapping semantics for proptest
    // exploration, where we want to visit the full state space without the
    // model short-circuiting on arithmetic rejection.
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
                    out.push_str(&format!(
                        "    match s.{}.checked_add({}) {{\n        Some(__v) => s.{} = __v,\n        None => return false,\n    }}\n",
                        field, rust_value, field
                    ));
                }
            }
            "sub" => {
                if wrapping {
                    out.push_str(&format!(
                        "    s.{} = s.{}.wrapping_sub({});\n",
                        field, field, rust_value
                    ));
                } else {
                    out.push_str(&format!(
                        "    match s.{}.checked_sub({}) {{\n        Some(__v) => s.{} = __v,\n        None => return false,\n    }}\n",
                        field, rust_value, field
                    ));
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
