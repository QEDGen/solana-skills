use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedProperty, ParsedSpec};
use crate::codegen::map_type;
use crate::rust_codegen_util;

/// Return the proptest strategy string for a DSL primitive type. For compound
/// types (`Map[N] T`, records, sum types) use `strategy_for_field` instead —
/// it dispatches here once it's unwrapped the compound.
fn strategy_for_type(dsl_type: &str) -> &str {
    match dsl_type {
        "U8" => "0u8..=255u8",
        "U16" => "0u16..=u16::MAX",
        "U32" => "0u32..=u32::MAX",
        "U64" => "0u64..=u64::MAX",
        "U128" => "0u128..=u128::MAX",
        "I8" => "i8::MIN..=i8::MAX",
        "I16" => "i16::MIN..=i16::MAX",
        "I32" => "i32::MIN..=i32::MAX",
        "I64" => "i64::MIN..=i64::MAX",
        "I128" => "any::<i128>()",
        "Bool" => "any::<bool>()",
        "Pubkey" => "prop::array::uniform32(0u8..)",
        // Fin[N] falls through here after the compound-type detector in
        // strategy_for_field strips the `Fin[N]` wrapper — it's modelled as
        // usize with a small range since real usage is as an index.
        "Fin" => "0usize..=1024usize",
        _ => "0u64..=u64::MAX",
    }
}

/// Boundary-biased strategy for guard rejection tests. Mixes small values (near 0)
/// with large values (near MAX) so that guards like `> 0` AND guards like `<= LARGE_CONST`
/// both have reasonable rejection rates.
fn boundary_strategy_for_type(dsl_type: &str) -> &str {
    match dsl_type {
        "U8" => "prop_oneof![0u8..=3u8, 252u8..=255u8]",
        "U16" => "prop_oneof![0u16..=3u16, (u16::MAX - 3)..=u16::MAX]",
        "U32" => "prop_oneof![0u32..=3u32, (u32::MAX - 3)..=u32::MAX]",
        "U64" => "prop_oneof![0u64..=3u64, (u64::MAX - 3)..=u64::MAX]",
        "U128" => "prop_oneof![0u128..=3u128, (u128::MAX - 3)..=u128::MAX]",
        "I8" => "prop_oneof![i8::MIN..=(i8::MIN + 3), (i8::MAX - 3)..=i8::MAX]",
        "I16" => "prop_oneof![i16::MIN..=(i16::MIN + 3), (i16::MAX - 3)..=i16::MAX]",
        "I32" => "prop_oneof![i32::MIN..=(i32::MIN + 3), (i32::MAX - 3)..=i32::MAX]",
        "I64" => "prop_oneof![i64::MIN..=(i64::MIN + 3), (i64::MAX - 3)..=i64::MAX]",
        "I128" => "any::<i128>()",
        "Bool" => "any::<bool>()",
        "Pubkey" => "prop::array::uniform32(0u8..1u8)",
        "Fin" => "prop_oneof![0usize..=3usize, 1020usize..=1024usize]",
        _ => "prop_oneof![0u64..=3u64, (u64::MAX - 3)..=u64::MAX]",
    }
}

/// Dispatch table for per-field strategy rendering. Handles compound types
/// (`Map[N] T` → fixed-size array via strict-length vec + try_into; records
/// → `arb_<Name>()`; unit-variant sum types → `arb_<Name>()`) and falls back
/// to the primitive `strategy_for_type` / `boundary_strategy_for_type`
/// helpers once the compound layer is peeled off.
///
/// v2.6.2 S3 taught `map_type` to resolve record/sum/alias/Fin names. v2.7
/// G1 teaches the strategy emitter the matching shape so `arb_state()`
/// doesn't bail into `0u64..=u64::MAX` when a field is `[Account; N]`.
fn strategy_for_field(
    dsl_type: &str,
    spec: &ParsedSpec,
    mode: StrategyMode,
    field_bound: Option<&str>,
) -> Result<String> {
    let dsl_type = dsl_type.trim();

    // Map[BOUND] T → strict-length Vec<T> → [T; N] via TryInto.
    // proptest's `prop::array::uniform*` combinators only go up to 32; the
    // vec-with-prop_map form works for any N.
    if let Some(rest) = dsl_type.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound_src = rest[..close].trim();
                let inner_src = rest[close + 1..].trim();
                let n = resolve_map_bound_local(bound_src, &spec.constants)?;
                let inner_strategy = strategy_for_field(inner_src, spec, mode, None)?;
                return Ok(format!(
                    "prop::collection::vec({inner_strategy}, {n}..={n}).prop_map(|v| v.try_into().ok().unwrap())"
                ));
            }
        }
        anyhow::bail!(
            "malformed Map type in strategy: `{}` — expected `Map[BOUND] T`",
            dsl_type
        );
    }

    // Fin[N] → usize. Bound is informational; use a bounded-ish strategy so
    // array indices stay within typical ranges.
    if dsl_type.starts_with("Fin[") {
        return Ok(match mode {
            StrategyMode::Full => strategy_for_type("Fin").to_string(),
            StrategyMode::Boundary => boundary_strategy_for_type("Fin").to_string(),
        });
    }

    // Record type → arb_<Name>() — emitted by emit_record_prop_composes.
    if spec.records.iter().any(|r| r.name == dsl_type) {
        return Ok(format!("arb_{}()", dsl_type));
    }

    // Unit-variant sum type → arb_<Name>() — emitted by emit_unit_sum_prop_oneofs.
    // Sum types with payload variants are S3 narrow's flattened-struct case and
    // don't appear as field types (the flattened struct's own field becomes
    // the one referenced).
    if spec.sum_types.iter().any(|s| {
        s.name == dsl_type
            && !s.variants.is_empty()
            && s.variants.iter().all(|v| v.fields.is_empty())
    }) {
        return Ok(format!("arb_{}()", dsl_type));
    }

    // Type alias: resolve transitively and recurse.
    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return strategy_for_field(rhs, spec, mode, field_bound);
    }

    // Primitive path — apply field bound if one was extracted from
    // property expressions.
    if let Some(bound) = field_bound {
        let rust_type = map_type(dsl_type, spec)?;
        return Ok(match mode {
            StrategyMode::Boundary => {
                let n: u128 = bound.parse().unwrap_or(u128::MAX);
                if n < 3 {
                    format!("0{rt}..={b}{rt}", rt = rust_type, b = bound)
                } else {
                    format!(
                        "prop_oneof![0{rt}..=3{rt}, ({b} - 3)..={b}{rt}]",
                        rt = rust_type,
                        b = bound
                    )
                }
            }
            StrategyMode::Full => format!("0{rt}..={b}{rt}", rt = rust_type, b = bound),
        });
    }
    Ok(match mode {
        StrategyMode::Boundary => boundary_strategy_for_type(dsl_type).to_string(),
        StrategyMode::Full => strategy_for_type(dsl_type).to_string(),
    })
}

/// Local copy of codegen::resolve_map_bound (private there) — same rule: bound
/// is either a numeric literal or a declared spec constant.
fn resolve_map_bound_local(bound: &str, constants: &[(String, String)]) -> Result<String> {
    let bound = bound.trim();
    if bound.chars().all(|c| c.is_ascii_digit()) && !bound.is_empty() {
        return Ok(bound.to_string());
    }
    match constants.iter().find(|(n, _)| n == bound) {
        Some((_, value)) => Ok(value.clone()),
        None => anyhow::bail!(
            "Map bound `{}` is not a numeric literal and not declared as a `const` in the spec",
            bound
        ),
    }
}

/// Emit a `prop_compose!` strategy block per spec record — the generator
/// that lets fields of type `Account` synthesize arbitrary values. Must be
/// called after `emit_record_structs` (the struct must exist first) and
/// before `emit_state_strategy` (the strategy references `arb_<Name>()`).
fn emit_record_prop_composes(out: &mut String, spec: &ParsedSpec) -> Result<()> {
    for rec in &spec.records {
        if rec.fields.is_empty() {
            continue;
        }
        out.push_str("prop_compose! {\n");
        out.push_str(&format!("    fn arb_{}()(", rec.name));
        for (i, (fname, ftype)) in rec.fields.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let strategy = strategy_for_field(ftype, spec, StrategyMode::Full, None)?;
            out.push_str(&format!("{fname} in {strategy}"));
        }
        out.push_str(&format!(") -> {} {{\n", rec.name));
        out.push_str(&format!("        {} {{\n", rec.name));
        for (fname, _) in &rec.fields {
            out.push_str(&format!("            {fname},\n"));
        }
        out.push_str("        }\n    }\n");
        out.push_str("}\n\n");
    }
    Ok(())
}

/// Emit a `prop_oneof!` strategy per unit-variant sum type. Sum types with
/// payload variants are skipped here — they're either flattened into the
/// State struct (S3 narrow) or become a Rust `enum` with their own strategy
/// (v2.7 G2).
fn emit_unit_sum_prop_oneofs(out: &mut String, spec: &ParsedSpec) -> Result<()> {
    for sum in &spec.sum_types {
        let all_unit = sum.variants.iter().all(|v| v.fields.is_empty());
        if !all_unit || sum.variants.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "fn arb_{}() -> impl Strategy<Value = {}> {{\n",
            sum.name, sum.name
        ));
        out.push_str("    prop_oneof![\n");
        for variant in &sum.variants {
            out.push_str(&format!("        Just({}::{}),\n", sum.name, variant.name));
        }
        out.push_str("    ]\n}\n\n");
    }
    Ok(())
}

/// Return the Rust type max value for overflow testing.
fn type_max(dsl_type: &str) -> Option<&str> {
    match dsl_type {
        "U8" => Some("u8::MAX"),
        "U16" => Some("u16::MAX"),
        "U32" => Some("u32::MAX"),
        "U64" => Some("u64::MAX"),
        "U128" => Some("u128::MAX"),
        _ => None,
    }
}

/// Extract constant upper bounds for state fields from property expressions.
/// E.g., `state.V <= MAX_VAULT_TVL` where MAX_VAULT_TVL is a known constant yields
/// `("V", "10000000000000000")`. Used to cap arb_state() ranges.
fn extract_field_upper_bounds(
    properties: &[&ParsedProperty],
    constants: &[(String, String)],
) -> std::collections::HashMap<String, String> {
    let mut bounds = std::collections::HashMap::new();
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            // Match patterns like "state.FIELD <= CONST" or "state.FIELD ≤ NUMBER"
            // Split on "and" / "∧" to handle conjunctive properties
            let parts_iter: Vec<&str> = expr.split(" and ").flat_map(|p| p.split('∧')).collect();
            for part in parts_iter {
                let part = part.trim();
                if let Some(rest) = part.strip_suffix(")").or(Some(part)) {
                    for op in &[" ≤ ", " <= "] {
                        if let Some(pos) = rest.find(op) {
                            let lhs = rest[..pos].trim();
                            let rhs = rest[pos + op.len()..].trim();
                            if let Some(field) = lhs
                                .strip_prefix("state.")
                                .or_else(|| lhs.strip_prefix("s."))
                            {
                                // Check if RHS is a constant name or a number
                                let resolved = constants
                                    .iter()
                                    .find(|(n, _)| n == rhs)
                                    .map(|(_, v)| v.replace('_', ""))
                                    .or_else(|| {
                                        let clean = rhs.replace('_', "");
                                        if clean.chars().all(|c| c.is_ascii_digit()) {
                                            Some(clean)
                                        } else {
                                            None
                                        }
                                    });
                                if let Some(val) = resolved {
                                    bounds.insert(field.to_string(), val);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    bounds
}

/// Generate proptest harnesses from a spec file (.qedspec).
///
/// Produces property-based tests that exercise the spec's state machine with
/// random inputs, checking invariants after every transition. Finds
/// counterexamples in milliseconds — the first tier of the verification waterfall.
pub fn generate(spec_path: &Path, output_path: &Path) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No operations found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    rust_codegen_util::check_effect_targets(&spec)?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let hash = fp
        .file_hashes
        .get("tests/proptest.rs")
        .cloned()
        .unwrap_or_default();

    let is_multi = spec.account_types.len() > 1;

    let mut out = String::new();

    // ── File header ─────────────────────────────────────────────────────
    out.push_str(&crate::banner::banner(None, &hash));
    out.push_str("//\n");
    out.push_str("// Proptest harnesses — property-based testing for the spec's state machine.\n");
    out.push_str(
        "// Tier 1 of the verification waterfall: finds counterexamples in milliseconds.\n",
    );
    out.push_str("//\n");
    out.push_str("//   Proptest: random testing, fast counterexamples (~100ms)\n");
    out.push_str("//   Kani:     bounded model checking, exhaustive within bounds (~5-30s)\n");
    out.push_str("//   Lean:     mathematical proof, universal guarantees (minutes-hours)\n");
    out.push_str("//\n");
    out.push_str("// To run:  cargo test --test proptest\n");
    out.push_str("// Deep:    PROPTEST_CASES=10000 cargo test --test proptest\n");
    out.push_str(
        "// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n\n",
    );
    out.push_str("use proptest::prelude::*;\n\n");

    // ── Constants ────────────────────────────────────────────────────────
    rust_codegen_util::emit_constants(&mut out, &spec.constants);

    if is_multi {
        // Multi-account: generate per-account sections in separate modules
        for acct in &spec.account_types {
            let acct_fields = rust_codegen_util::mutable_fields(&acct.fields);
            if acct_fields.is_empty() {
                continue;
            }
            // Filter handlers targeting this account
            let acct_handlers: Vec<&ParsedHandler> = spec
                .handlers
                .iter()
                .filter(|h| h.on_account.as_deref() == Some(&acct.name))
                .collect();
            if acct_handlers.is_empty() {
                continue;
            }
            // Filter properties whose fields are in this account
            let acct_field_names: Vec<&str> = acct_fields.iter().map(|(n, _)| n.as_str()).collect();
            let acct_props: Vec<&ParsedProperty> = spec
                .properties
                .iter()
                .filter(|p| {
                    if let Some(ref expr) = p.expression {
                        acct_field_names.iter().any(|f| expr.contains(f))
                    } else {
                        false
                    }
                })
                .collect();

            let mod_name = acct.name.to_lowercase();
            out.push_str(&format!("mod {} {{\n", mod_name));
            out.push_str("    use super::*;\n\n");

            // Build a minimal ParsedSpec view for this account
            emit_account_section(
                &mut out,
                &acct.name,
                &acct_fields,
                &acct.fields,
                &acct_handlers,
                &acct_props,
                &acct.lifecycle,
                &spec,
            )?;

            out.push_str(&format!("}} // mod {}\n\n", mod_name));
        }
    } else {
        // Single-account: generate flat (no module wrapper)
        let state_fields: &[(String, String)] = &spec.state_fields;
        let mutable_fields = rust_codegen_util::mutable_fields(state_fields);
        let all_handlers: Vec<&ParsedHandler> = spec.handlers.iter().collect();
        let all_props: Vec<&ParsedProperty> = spec.properties.iter().collect();
        emit_account_section(
            &mut out,
            &spec.program_name,
            &mutable_fields,
            state_fields,
            &all_handlers,
            &all_props,
            &spec.lifecycle_states,
            &spec,
        )?;
    }

    std::fs::write(output_path, &out)?;
    eprintln!("Generated proptest harnesses at {}", output_path.display());
    Ok(())
}

/// Emit a complete test section for one account type (or the single account in non-multi specs).
#[allow(clippy::too_many_arguments)]
fn emit_account_section(
    out: &mut String,
    _acct_name: &str,
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    handlers: &[&ParsedHandler],
    properties: &[&ParsedProperty],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
) -> Result<()> {
    // User-defined records/enums referenced by State must be declared first,
    // then their `arb_<Name>()` strategies so `arb_state` can call into them.
    // v2.7 G1 finishes what v2.6.2 S3 started: the struct decls were emitted
    // but the strategy lookup bailed into `0u64..=u64::MAX` for record-typed
    // fields. emit_record_prop_composes + strategy_for_field below fix that.
    rust_codegen_util::emit_record_structs(out, spec, "Debug, Clone, Copy", |t| map_type(t, spec))?;
    rust_codegen_util::emit_unit_enum_sums(out, spec, "Debug, Clone, Copy, PartialEq, Eq")?;
    emit_record_prop_composes(out, spec)?;
    emit_unit_sum_prop_oneofs(out, spec)?;

    // State struct
    out.push_str("#[derive(Debug, Clone, Copy)]\n");
    out.push_str("struct State {\n");
    for (fname, ftype) in mutable_fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type(ftype, spec)?));
    }
    out.push_str("}\n\n");

    // Extract constant upper bounds from properties to cap arb_state() ranges.
    // E.g., `state.V <= MAX_VAULT_TVL` caps V to 10^16 instead of u128::MAX.
    // When bounds exist, also apply them to other numeric fields of the same type
    // so that relational invariants like `V >= C_tot + I` have valid input ranges.
    let mut field_bounds = extract_field_upper_bounds(properties, &spec.constants);
    if !field_bounds.is_empty() {
        // Find the tightest bound and apply it to all unbounded numeric fields
        // of the same type. This ensures relational properties hold in random states.
        let min_bound = field_bounds.values().min_by_key(|v| v.len()).cloned();
        if let Some(ref bound) = min_bound {
            for (fname, ftype) in mutable_fields {
                if ftype.as_str() != "Pubkey" && !field_bounds.contains_key(fname.as_str()) {
                    field_bounds.insert(fname.to_string(), bound.clone());
                }
            }
        }
    }
    emit_state_strategy(out, mutable_fields, all_fields, &field_bounds, spec)?;

    // Property predicates
    let props_with_expr: Vec<&&ParsedProperty> = properties
        .iter()
        .filter(|p| p.expression.is_some())
        .collect();
    if !props_with_expr.is_empty() {
        for prop in &props_with_expr {
            // Prefer the AST-rendered Rust form (handles `implies`/`forall`
            // without mojibake); fall back to text-massaging the Lean body
            // when `rust_expression` is absent (legacy callers).
            let rust_expr = match prop.rust_expression.as_deref() {
                Some(r) => r.to_string(),
                None => match prop.expression.as_deref() {
                    Some(e) => rust_codegen_util::translate_property_to_rust(e, true),
                    None => continue,
                },
            };
            let doc = prop.expression.as_deref().unwrap_or("");
            out.push_str(&format!("/// {}: {}\n", prop.name, doc));
            if crate::check::rust_expr_is_unsupported(&rust_expr) {
                out.push_str(&format!("fn {}(_s: &State) -> bool {{\n", prop.name));
                out.push_str(&format!(
                    "    // {} — property uses a quantifier; not lowerable to a predicate.\n",
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

    // Transition functions
    emit_transition_functions_for(out, handlers, spec)?;

    // Clone properties once for sections that need owned copies
    let owned_props: Vec<ParsedProperty> = properties.iter().map(|p| (*p).clone()).collect();

    // Property preservation tests
    if !props_with_expr.is_empty() {
        emit_preservation_tests_for(out, handlers, &owned_props, mutable_fields, spec)?;
    }

    // Guard enforcement tests
    let guard_ops: Vec<&&ParsedHandler> = handlers.iter().filter(|op| op.has_guard()).collect();
    if !guard_ops.is_empty() {
        let guard_refs: Vec<&ParsedHandler> = guard_ops.iter().map(|op| **op).collect();
        emit_guard_tests(out, &guard_refs, mutable_fields, all_fields);
    }

    // Overflow detection tests
    let overflow_ops: Vec<&&ParsedHandler> = handlers
        .iter()
        .filter(|op| op.effects.iter().any(|(_, k, _)| k == "add"))
        .collect();
    if !overflow_ops.is_empty() {
        let overflow_refs: Vec<&ParsedHandler> = overflow_ops.iter().map(|op| **op).collect();
        emit_overflow_tests_for(
            out,
            &overflow_refs,
            mutable_fields,
            all_fields,
            spec,
            &owned_props,
        )?;
    }

    // Sequence test
    if !owned_props.is_empty() && handlers.len() > 1 {
        emit_sequence_test_for(
            out,
            handlers,
            &owned_props,
            mutable_fields,
            all_fields,
            lifecycle_states,
            spec,
        )?;
    }
    Ok(())
}

/// Emit proptest `Arbitrary`-like strategy for State.
fn emit_state_strategy(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    field_bounds: &std::collections::HashMap<String, String>,
    spec: &ParsedSpec,
) -> Result<()> {
    // Full-range strategy (capped by property bounds when available)
    emit_state_strategy_inner(
        out,
        "arb_state",
        mutable_fields,
        all_fields,
        StrategyMode::Full,
        field_bounds,
        spec,
    )?;
    // Boundary-biased strategy for guard rejection tests
    emit_state_strategy_inner(
        out,
        "arb_boundary_state",
        mutable_fields,
        all_fields,
        StrategyMode::Boundary,
        field_bounds,
        spec,
    )?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum StrategyMode {
    Full,
    Boundary,
}

fn emit_state_strategy_inner(
    out: &mut String,
    fn_name: &str,
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    mode: StrategyMode,
    field_bounds: &std::collections::HashMap<String, String>,
    spec: &ParsedSpec,
) -> Result<()> {
    match mode {
        StrategyMode::Boundary => {
            out.push_str("/// Boundary-biased strategy for guard rejection tests.\n");
        }
        StrategyMode::Full => {
            out.push_str("/// Proptest strategy for generating arbitrary State values.\n");
        }
    }
    out.push_str(&format!(
        "fn {}() -> impl Strategy<Value = State> {{\n",
        fn_name
    ));
    out.push_str("    (\n");
    for (i, (fname, _ftype)) in mutable_fields.iter().enumerate() {
        let dsl_type = all_fields
            .iter()
            .find(|(n, _)| n.as_str() == fname.as_str())
            .map(|(_, t)| t.as_str())
            .unwrap_or("U64");
        // strategy_for_field dispatches on compound types (Map, record, sum,
        // alias) and falls through to the primitive strategy tables. Field
        // bounds only apply to primitive fields (it doesn't make sense to
        // cap a `[Account; N]` to a numeric bound).
        let bound = field_bounds.get(fname.as_str()).map(|s| s.as_str());
        let strategy = strategy_for_field(dsl_type, spec, mode, bound)?;
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str(&format!("        {}", strategy));
    }
    out.push_str(",\n    ).prop_map(|(");
    for (i, (fname, _)) in mutable_fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(fname);
    }
    out.push_str(")| State {\n");
    for (fname, _) in mutable_fields {
        out.push_str(&format!("        {},\n", fname));
    }
    out.push_str("    })\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Emit transition functions for a slice of handlers.
fn emit_transition_functions_for(
    out: &mut String,
    handlers: &[&ParsedHandler],
    spec: &ParsedSpec,
) -> Result<()> {
    for op in handlers {
        rust_codegen_util::emit_transition_fn(out, op, spec, true, |t| map_type(t, spec))?;
    }
    Ok(())
}

/// Emit per-(handler, property) preservation tests.
fn emit_preservation_tests_for(
    out: &mut String,
    handlers: &[&ParsedHandler],
    properties: &[ParsedProperty],
    mutable_fields: &[&(String, String)],
    spec: &ParsedSpec,
) -> Result<()> {
    for prop in properties {
        if prop.expression.is_none() {
            continue;
        }

        for op_name in &prop.preserved_by {
            let op = handlers.iter().find(|o| &o.name == op_name).copied();

            // Skip handlers not in the current account section (multi-account:
            // preserved_by all expands to all handlers, but we only emit tests
            // for handlers belonging to this account type).
            if op.is_none() {
                continue;
            }

            let is_init = op
                .map(|o| o.pre_status.as_deref() == Some("Uninitialized"))
                .unwrap_or(false);

            out.push_str("proptest! {\n");
            // High reject limit: prop_assume on multiple invariants filters aggressively
            out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
            out.push_str("    #[test]\n");

            // Build the parameter list for proptest
            let mut param_parts = Vec::new();
            if is_init {
                // For init handlers, use fixed zero state
            } else {
                param_parts.push("s in arb_state()".to_string());
            }
            if let Some(op) = op {
                for (pname, ptype) in &op.takes_params {
                    let rust_type = map_type(ptype, spec)?;
                    param_parts.push(format!("{} in 0{}..={}::MAX", pname, rust_type, rust_type));
                }
            }

            if param_parts.is_empty() && is_init {
                // Need at least a dummy parameter for proptest
                param_parts.push("_dummy in 0u8..1u8".to_string());
            }

            out.push_str(&format!(
                "    fn {}_preserves_{}({}) {{\n",
                op_name,
                prop.name,
                param_parts.join(", ")
            ));

            if is_init {
                out.push_str("        let mut s = State {\n");
                for (fname, _) in mutable_fields {
                    out.push_str(&format!("            {}: 0,\n", fname));
                }
                out.push_str("        };\n");
            } else {
                out.push_str("        let mut s = s;\n");
                // Assume all declared properties hold before transition
                for pre_prop in properties {
                    if pre_prop.expression.is_some() {
                        out.push_str(&format!("        prop_assume!({}(&s));\n", pre_prop.name));
                    }
                }
            }

            // Emit strict bounds for add effects
            if let Some(op) = op {
                rust_codegen_util::emit_add_strict_bounds(
                    out,
                    op,
                    properties,
                    "        prop_assume!(s.{field} < s.{bound}); // strict bound for add\n",
                );
            }

            // Call transition and assert
            let args: String = op
                .map(|o| {
                    o.takes_params
                        .iter()
                        .map(|(n, _)| format!(", {}", n))
                        .collect()
                })
                .unwrap_or_default();
            out.push_str(&format!("        if {}(&mut s{}) {{\n", op_name, args));
            out.push_str(&format!("            prop_assert!({}(&s),\n", prop.name));
            out.push_str(&format!(
                "                \"{} must hold after {}\");\n",
                prop.name, op_name
            ));
            out.push_str("        }\n");
            out.push_str("    }\n");
            out.push_str("}\n\n");
        }
    }
    Ok(())
}

/// Emit guard enforcement tests.
fn emit_guard_tests(
    out: &mut String,
    guard_ops: &[&ParsedHandler],
    _mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
) {
    for op in guard_ops {
        let rust_guard =
            rust_codegen_util::collect_full_guard(op, true).unwrap_or_else(|| "true".to_string());

        out.push_str("proptest! {\n");
        // High reject limit: guard negation filters most inputs by design
        out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
        out.push_str("    #[test]\n");

        // Use boundary-biased ranges for guard rejection tests so that
        // prop_assume!(negated guard) has a reasonable acceptance rate.
        let mut param_parts = vec!["s in arb_boundary_state()".to_string()];
        for (pname, ptype) in &op.takes_params {
            let boundary = boundary_strategy_for_type(ptype);
            param_parts.push(format!("{} in {}", pname, boundary));
        }

        out.push_str(&format!(
            "    fn {}_rejects_invalid({}) {{\n",
            op.name,
            param_parts.join(", ")
        ));

        out.push_str("        let mut s = s;\n");
        out.push_str(&format!("        prop_assume!(!({rust_guard}));\n"));

        let args: String = op
            .takes_params
            .iter()
            .map(|(n, _)| format!(", {}", n))
            .collect();
        out.push_str(&format!(
            "        prop_assert!(!{}(&mut s{}),\n",
            op.name, args
        ));
        out.push_str(&format!(
            "            \"{} must reject when guard is violated\");\n",
            op.name
        ));
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    let _ = all_fields; // suppress unused
}

/// Emit overflow detection tests for add effects.
fn emit_overflow_tests_for(
    out: &mut String,
    overflow_ops: &[&ParsedHandler],
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    spec: &ParsedSpec,
    properties: &[ParsedProperty],
) -> Result<()> {
    for op in overflow_ops {
        for (field, kind, _value) in &op.effects {
            if kind != "add" {
                continue;
            }

            let dsl_type = all_fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, t)| t.as_str())
                .unwrap_or("U64");
            let max_val = match type_max(dsl_type) {
                Some(m) => m,
                None => continue,
            };
            let rust_type = map_type(dsl_type, spec)?;

            out.push_str("proptest! {\n");
            out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
            out.push_str("    #[test]\n");

            let mut param_parts = vec!["s in arb_state()".to_string()];
            for (pname, ptype) in &op.takes_params {
                let rt = map_type(ptype, spec)?;
                param_parts.push(format!("{} in 0{}..={}::MAX", pname, rt, rt));
            }

            out.push_str(&format!(
                "    fn {}_no_overflow_on_{}({}) {{\n",
                op.name,
                field,
                param_parts.join(", ")
            ));

            out.push_str("        let mut s = s;\n");

            // Assume all properties hold (they constrain valid state space)
            for pre_prop in properties {
                if pre_prop.expression.is_some() {
                    out.push_str(&format!("        prop_assume!({}(&s));\n", pre_prop.name));
                }
            }

            out.push_str(&format!("        let pre = s.{};\n", field));

            let args: String = op
                .takes_params
                .iter()
                .map(|(n, _)| format!(", {}", n))
                .collect();
            out.push_str(&format!("        if {}(&mut s{}) {{\n", op.name, args));
            out.push_str("            // If transition succeeded, the add must not have wrapped\n");
            out.push_str(&format!("            prop_assert!(s.{} >= pre,\n", field));
            out.push_str(&format!(
                "                \"overflow: {}.{} wrapped around after add\");\n",
                op.name, field
            ));
            out.push_str("        }\n");
            out.push_str("    }\n");
            out.push_str("}\n\n");

            let _ = (max_val, rust_type, mutable_fields); // suppress unused
        }
    }
    Ok(())
}

/// Emit state machine sequence test — random op sequences checking invariants.
fn emit_sequence_test_for(
    out: &mut String,
    handlers: &[&ParsedHandler],
    properties: &[ParsedProperty],
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
) -> Result<()> {
    // Emit an Operation enum
    out.push_str("#[derive(Debug, Clone)]\n");
    out.push_str("enum Op {\n");
    for op in handlers {
        let params: String = op
            .takes_params
            .iter()
            .map(|(_, t)| map_type(t, spec))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        if params.is_empty() {
            out.push_str(&format!(
                "    {},\n",
                crate::codegen::to_pascal_case(&op.name)
            ));
        } else {
            out.push_str(&format!(
                "    {}({}),\n",
                crate::codegen::to_pascal_case(&op.name),
                params
            ));
        }
    }
    out.push_str("}\n\n");

    // Strategy for Op
    out.push_str("fn arb_op() -> impl Strategy<Value = Op> {\n");
    out.push_str("    prop_oneof![\n");
    for op in handlers {
        let pascal = crate::codegen::to_pascal_case(&op.name);
        if op.takes_params.is_empty() {
            out.push_str(&format!("        Just(Op::{}),\n", pascal));
        } else {
            let strategies: Vec<String> = op
                .takes_params
                .iter()
                .map(|(_, t)| {
                    map_type(t, spec).map(|rust_type| format!("0{rt}..={rt}::MAX", rt = rust_type))
                })
                .collect::<Result<Vec<_>>>()?;
            out.push_str(&format!("        ({}).prop_map(|", strategies.join(", ")));
            if op.takes_params.len() == 1 {
                out.push_str("v| ");
                out.push_str(&format!("Op::{}(v)", pascal));
            } else {
                out.push('(');
                for (i, (pname, _)) in op.takes_params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(pname);
                }
                out.push_str(")| ");
                out.push_str(&format!("Op::{}(", pascal));
                for (i, (pname, _)) in op.takes_params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(pname);
                }
                out.push(')');
            }
            out.push_str("),\n");
        }
    }
    out.push_str("    ]\n");
    out.push_str("}\n\n");

    // Apply function
    out.push_str("fn apply_op(s: &mut State, op: &Op) -> bool {\n");
    out.push_str("    match op {\n");
    for op in handlers {
        let pascal = crate::codegen::to_pascal_case(&op.name);
        if op.takes_params.is_empty() {
            out.push_str(&format!("        Op::{} => {}(s),\n", pascal, op.name));
        } else {
            let bindings: Vec<String> = op.takes_params.iter().map(|(n, _)| n.clone()).collect();
            out.push_str(&format!(
                "        Op::{}({}) => {}(s, {}),\n",
                pascal,
                bindings.join(", "),
                op.name,
                bindings
                    .iter()
                    .map(|b| format!("*{}", b))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // Assert all properties
    out.push_str("fn assert_all_properties(s: &State, context: &str) {\n");
    for prop in properties {
        if prop.expression.is_some() {
            out.push_str(&format!(
                "    assert!({}(s), \"{{}} violated: {}\", context);\n",
                prop.name, prop.name
            ));
        }
    }
    out.push_str("}\n\n");

    // Lifecycle tracking: if spec has lifecycle states, track current state
    // and only check properties after the first state-modifying transition.
    let has_lifecycle = !lifecycle_states.is_empty();
    let initial_state = lifecycle_states.first().cloned();

    // Emit lifecycle enum if needed
    if has_lifecycle {
        out.push_str("#[derive(Debug, Clone, Copy, PartialEq)]\n");
        out.push_str("enum Lifecycle {\n");
        for state in lifecycle_states {
            out.push_str(&format!("    {},\n", state));
        }
        out.push_str("}\n\n");

        // Lifecycle transition function
        out.push_str(
            "fn lifecycle_transition(current: Lifecycle, op: &Op) -> Option<Lifecycle> {\n",
        );
        out.push_str("    match (current, op) {\n");
        for op in handlers {
            if let (Some(ref pre), Some(ref post)) = (&op.pre_status, &op.post_status) {
                let pascal = crate::codegen::to_pascal_case(&op.name);
                if op.takes_params.is_empty() {
                    out.push_str(&format!(
                        "        (Lifecycle::{}, Op::{}) => Some(Lifecycle::{}),\n",
                        pre, pascal, post
                    ));
                } else {
                    out.push_str(&format!(
                        "        (Lifecycle::{}, Op::{}(..)) => Some(Lifecycle::{}),\n",
                        pre, pascal, post
                    ));
                }
            }
        }
        out.push_str("        _ => None, // transition not allowed in this state\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    // All properties with expressions
    let all_props: Vec<&ParsedProperty> = properties
        .iter()
        .filter(|p| p.expression.is_some())
        .collect();

    // The sequence test
    let seq_len = 20;
    out.push_str("proptest! {\n");
    out.push_str("    #![proptest_config(ProptestConfig::with_cases(256))]\n");
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn state_machine_sequence(ops in proptest::collection::vec(arb_op(), 1..{})) {{\n",
        seq_len
    ));

    // Start from a valid initial state (zeroed — represents Uninitialized)
    out.push_str("        let mut s = State {\n");
    for (fname, _) in mutable_fields {
        out.push_str(&format!("            {}: 0,\n", fname));
    }
    out.push_str("        };\n");

    if has_lifecycle {
        if let Some(ref init) = initial_state {
            out.push_str(&format!(
                "        let mut lifecycle = Lifecycle::{};\n",
                init
            ));
        }
        out.push_str("        let mut initialized = false;\n");
    }

    out.push_str("        for (i, op) in ops.iter().enumerate() {\n");

    if has_lifecycle {
        // Check lifecycle transition is valid before applying
        out.push_str("            let next_lifecycle = lifecycle_transition(lifecycle, op);\n");
        out.push_str("            if next_lifecycle.is_none() {\n");
        out.push_str(
            "                continue; // skip ops not valid in current lifecycle state\n",
        );
        out.push_str("            }\n");
    }

    out.push_str("            if apply_op(&mut s, op) {\n");

    if has_lifecycle {
        out.push_str("                if let Some(next) = next_lifecycle {\n");
        out.push_str("                    lifecycle = next;\n");
        out.push_str("                }\n");
        // Mark as initialized after the first transition out of Uninitialized
        if initial_state.as_deref() == Some("Uninitialized") {
            out.push_str("                if !initialized {\n");
            out.push_str("                    initialized = true;\n");
            out.push_str(
                "                    continue; // skip property checks on init transition\n",
            );
            out.push_str("                }\n");
        }
    }

    // Check all properties after each successful transition
    out.push_str("                // Check all properties after each successful transition\n");
    if !all_props.is_empty() {
        for prop in &all_props {
            out.push_str(&format!(
                "                prop_assert!({}(&s),\n",
                prop.name
            ));
            out.push_str(&format!(
                "                    \"{} violated after op {{:?}} (step {{}})\", op, i);\n",
                prop.name
            ));
        }
    }

    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");

    let _ = all_fields; // suppress unused
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{ParsedRecordType, ParsedSumType, ParsedVariant};
    use crate::chumsky_adapter::parse_str;

    fn spec_with_record(name: &str, fields: &[(&str, &str)]) -> ParsedSpec {
        ParsedSpec {
            records: vec![ParsedRecordType {
                name: name.to_string(),
                fields: fields
                    .iter()
                    .map(|(n, t)| (n.to_string(), t.to_string()))
                    .collect(),
            }],
            ..ParsedSpec::default()
        }
    }

    fn spec_with_unit_sum(name: &str, variants: &[&str]) -> ParsedSpec {
        ParsedSpec {
            sum_types: vec![ParsedSumType {
                name: name.to_string(),
                variants: variants
                    .iter()
                    .map(|v| ParsedVariant {
                        name: v.to_string(),
                        fields: vec![],
                    })
                    .collect(),
            }],
            ..ParsedSpec::default()
        }
    }

    #[test]
    fn strategy_for_field_primitive_routes_through_strategy_for_type() {
        let spec = ParsedSpec::default();
        let s = strategy_for_field("U64", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s, "0u64..=u64::MAX");
        let s = strategy_for_field("U128", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s, "0u128..=u128::MAX");
        let s = strategy_for_field("I128", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s, "any::<i128>()");
    }

    #[test]
    fn strategy_for_field_map_of_primitive_emits_vec_with_try_into() {
        // v2.6.2 bug: `Map[4] U64` fell through `strategy_for_type` and emitted
        // `0[u64; 4]..=u64::MAX[u64; 4]` (pattern-splicing the Rust type into
        // a range literal). v2.7 G1 routes through vec-with-prop_map.
        let spec = ParsedSpec {
            constants: vec![("N".to_string(), "4".to_string())],
            ..ParsedSpec::default()
        };
        let s = strategy_for_field("Map[N] U64", &spec, StrategyMode::Full, None).unwrap();
        assert!(
            s.starts_with("prop::collection::vec(0u64..=u64::MAX, 4..=4)"),
            "unexpected Map-primitive strategy: {s}"
        );
        assert!(
            s.contains(".prop_map(|v| v.try_into().ok().unwrap())"),
            "missing try_into prop_map: {s}"
        );
    }

    #[test]
    fn strategy_for_field_record_routes_to_arb_name() {
        // Percolator case: `Map[MAX_ACCOUNTS] Account` should route through
        // arb_Account() not `0u64..=u64::MAX`.
        let src = r#"spec T
const N = 4
type Account = { active : U8, capital : U128 }
state { accounts : Map[N] Account }
handler noop { }
"#;
        let spec = parse_str(src).expect("parse");
        let s = strategy_for_field("Account", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s, "arb_Account()");

        let s = strategy_for_field("Map[N] Account", &spec, StrategyMode::Full, None).unwrap();
        assert!(
            s.starts_with("prop::collection::vec(arb_Account(), 4..=4)"),
            "Map-record strategy didn't call into arb_Account: {s}"
        );
    }

    #[test]
    fn strategy_for_field_unit_sum_routes_to_arb_name() {
        // ParsedSpec fixture because the adapter only populates `sum_types`
        // for sum types referenced as `Map[N] <SumName>`, not for top-level
        // unit-variant sums. The strategy logic works off the field, so we
        // test it in isolation.
        let spec = spec_with_unit_sum("Status", &["Open", "Closed", "Cancelled"]);
        let s = strategy_for_field("Status", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s, "arb_Status()");
    }

    #[test]
    fn strategy_for_field_type_alias_resolves_transitively() {
        // `type AccountIdx = Fin[N]` — strategy should route through the
        // Fin[N] handler.
        let src = r#"spec T
const N = 4
type AccountIdx = Fin[N]
state { i : AccountIdx }
handler noop { }
"#;
        let spec = parse_str(src).expect("parse");
        let s = strategy_for_field("AccountIdx", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s, "0usize..=1024usize");
    }

    #[test]
    fn emit_record_prop_composes_emits_block_per_record() {
        let spec = spec_with_record("Account", &[("active", "U8"), ("balance", "U128")]);
        let mut out = String::new();
        emit_record_prop_composes(&mut out, &spec).expect("emit");
        assert!(
            out.contains("prop_compose!"),
            "should emit prop_compose! block: {out}"
        );
        assert!(
            out.contains("fn arb_Account()"),
            "should define arb_Account: {out}"
        );
        assert!(
            out.contains("active in 0u8..=255u8"),
            "should strategy active field: {out}"
        );
        assert!(
            out.contains("balance in 0u128..=u128::MAX"),
            "should strategy balance field: {out}"
        );
    }

    #[test]
    fn emit_unit_sum_prop_oneofs_emits_fn_per_sum() {
        let spec = spec_with_unit_sum("Error", &["NotAdmin", "InsufficientFunds", "VaultOverflow"]);
        let mut out = String::new();
        emit_unit_sum_prop_oneofs(&mut out, &spec).expect("emit");
        assert!(
            out.contains("fn arb_Error() -> impl Strategy<Value = Error>"),
            "should define arb_Error: {out}"
        );
        assert!(out.contains("prop_oneof!"), "should use prop_oneof: {out}");
        assert!(
            out.contains("Just(Error::NotAdmin)"),
            "should include variant: {out}"
        );
        assert!(
            out.contains("Just(Error::InsufficientFunds)"),
            "should include variant: {out}"
        );
    }

    #[test]
    fn emit_unit_sum_skips_payload_variants() {
        // A sum type with at least one payload-carrying variant isn't eligible
        // for the unit-enum path — it'd need a real variant-aware strategy
        // (v2.7 G2). Confirm the skip.
        let spec = ParsedSpec {
            sum_types: vec![ParsedSumType {
                name: "State".to_string(),
                variants: vec![
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("v".to_string(), "U64".to_string())],
                    },
                    ParsedVariant {
                        name: "Closed".to_string(),
                        fields: vec![],
                    },
                ],
            }],
            ..ParsedSpec::default()
        };
        let mut out = String::new();
        emit_unit_sum_prop_oneofs(&mut out, &spec).expect("emit");
        assert!(
            !out.contains("arb_State"),
            "payload-variant sum should not get unit-strategy: {out}"
        );
    }
}
