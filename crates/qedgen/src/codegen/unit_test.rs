use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedSpec};
use crate::codegen_shared::{map_type, write_generated_file};

/// `(field, op_kind, rust_value)` — the unit-test view of one effect site.
type EffectTriple = (String, &'static str, String);

/// Generate unit tests from a spec file (.lean or .qedspec).
/// Tests exercise effects, guards, and properties directly on a plain state
/// struct — no SVM, no Quasar runtime, just `cargo test`.
pub fn generate(spec_path: &Path, output_path: &Path) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No operations found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    crate::rust_codegen_util::check_effect_targets(&spec)?;

    // Effect iteration runs over the lowered MIR body via the shared
    // `stmt_effect_triple` projection (#66) instead of string-matching
    // raw `op.effects` (F7).
    let mir = crate::mir::lower(&spec);

    let fp = crate::fingerprint::compute_fingerprint(&spec);

    let is_multi = spec.account_types.len() > 1;
    let mut out = String::new();

    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        &fp,
        "src/tests.rs",
    ));
    out.push_str("// Unit tests generated from qedspec.\n");
    out.push_str("// These test effects, guards, and properties on a plain state struct.\n");
    out.push_str("// No SVM or Quasar runtime required — just `cargo test`.\n\n");

    // Type alias for Address (Pubkey → [u8; 32] for standalone testing)
    let all_fields: Vec<&(String, String)> = if is_multi {
        spec.account_types
            .iter()
            .flat_map(|a| a.fields.iter())
            .collect()
    } else {
        spec.state_fields.iter().collect()
    };
    if all_fields.iter().any(|(_, t)| t == "Pubkey")
        || spec.handlers.iter().any(|op| op.who.is_some())
    {
        out.push_str("type Address = [u8; 32];\n\n");
    }

    // User-defined records/enums referenced by State fields must be
    // declared first so the State struct compiles.
    crate::rust_codegen_util::emit_record_structs(
        &mut out,
        &spec,
        "Debug, Clone, Copy, PartialEq",
        |t| map_type(t, &spec),
    )?;
    crate::rust_codegen_util::emit_unit_enum_sums(
        &mut out,
        &spec,
        "Debug, Clone, Copy, PartialEq, Eq",
    )?;

    if is_multi {
        // Multi-account: one struct + status enum per account type
        for acct in &spec.account_types {
            let state_name = format!("{}State", acct.name);
            emit_state_struct(&mut out, &state_name, &acct.fields, &spec)?;

            if !acct.lifecycle.is_empty() {
                let status_name = format!("{}Status", acct.name);
                out.push_str(&format!(
                    "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum {} {{\n",
                    status_name
                ));
                for state in &acct.lifecycle {
                    out.push_str(&format!("    {},\n", state));
                }
                out.push_str("}\n\n");
            }
        }
    } else {
        let state_name = format!(
            "{}State",
            crate::codegen_shared::to_pascal_case(&spec.program_name)
        );
        emit_state_struct(&mut out, &state_name, &spec.state_fields, &spec)?;

        // Status enum for state machine tests
        if !spec.lifecycle_states.is_empty() {
            out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum Status {\n");
            for state in &spec.lifecycle_states {
                out.push_str(&format!("    {},\n", state));
            }
            out.push_str("}\n\n");
        }
    }

    // Helper: apply effects to state
    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (op_state_name, _) = resolve_state_for_op(op, &spec, is_multi);
        let triples = effect_triples(&op.name, &mir, &spec);
        // Prefix unused params with _ to suppress warnings. A param is used
        // when any effect references it — in the target path (subscripts
        // like `voted[member_index]`) or the RHS.
        let params: Vec<String> = op
            .takes_params
            .iter()
            .map(|(n, t)| {
                let used = triples
                    .iter()
                    .any(|(f, _, v)| f.contains(n.as_str()) || v.contains(n.as_str()));
                let rt = map_type(t, &spec)?;
                Ok(if used {
                    format!("{}: {}", n, rt)
                } else {
                    format!("_{}: {}", n, rt)
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let param_sig = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        out.push_str(&format!("/// Apply `{}` effects to state.\n", op.name));
        out.push_str(&format!(
            "fn apply_{}(state: &mut {}{}) {{\n",
            op.name, op_state_name, param_sig
        ));
        for (field, kind, value) in &triples {
            match *kind {
                "set" => {
                    out.push_str(&format!("    state.{} = {};\n", field, value));
                }
                "add" => {
                    out.push_str(&format!("    state.{} += {};\n", field, value));
                }
                "sub" => {
                    out.push_str(&format!("    state.{} -= {};\n", field, value));
                }
                "add_sat" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.saturating_add({});\n",
                        field, field, value
                    ));
                }
                "sub_sat" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.saturating_sub({});\n",
                        field, field, value
                    ));
                }
                "add_wrap" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.wrapping_add({});\n",
                        field, field, value
                    ));
                }
                "sub_wrap" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.wrapping_sub({});\n",
                        field, field, value
                    ));
                }
                other => {
                    out.push_str(&format!(
                        "    // unknown effect: {} {} {}\n",
                        field, other, value
                    ));
                }
            }
        }
        out.push_str("}\n\n");
    }

    // Helper: guard predicates. Handlers whose requires are all
    // account-suppressed get no guard fn (and no guard tests below) — a
    // `true` predicate would make the rejects-test assert `!true`.
    for op in &spec.handlers {
        let Some(guard_rust) = guard_predicate_rust(op) else {
            continue;
        };
        let (op_state_name, _) = resolve_state_for_op(op, &spec, is_multi);
        let params: Vec<String> = op
            .takes_params
            .iter()
            .map(|(n, t)| map_type(t, &spec).map(|rt| format!("{}: {}", n, rt)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let param_sig = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        // If the guard doesn't reference state fields, prefix with _
        let state_param = if guard_rust.contains("state.") {
            "state"
        } else {
            "_state"
        };
        out.push_str(&format!("/// Guard predicate for `{}`.\n", op.name));
        out.push_str(&format!(
            "fn guard_{}({}: &{}{}) -> bool {{\n",
            op.name, state_param, op_state_name, param_sig
        ));
        out.push_str(&format!("    {}\n", guard_rust));
        out.push_str("}\n\n");
    }

    out.push_str("#[cfg(test)]\nmod tests {\n    use super::*;\n\n");

    out.push_str("    // ====================================================================\n");
    out.push_str("    // Effect tests — verify state mutations match spec\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        let triples = effect_triples(&op.name, &mir, &spec);
        generate_effect_test(&mut out, op, &triples, fields, &sn, &spec)?;
    }

    out.push_str("    // ====================================================================\n");
    out.push_str("    // Guard tests — verify boundary conditions\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        let Some(guard_rust) = guard_predicate_rust(op) else {
            continue;
        };
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        generate_guard_tests(&mut out, op, &guard_rust, fields, &sn, &spec)?;
    }

    if !spec.properties.is_empty() {
        out.push_str(
            "    // ====================================================================\n",
        );
        out.push_str("    // Property tests — verify invariants hold after effects\n");
        out.push_str(
            "    // ====================================================================\n\n",
        );

        for prop in &spec.properties {
            // Resolve property's state type based on expression field references
            let (prop_sn, prop_fields) = resolve_state_for_property(prop, &spec, is_multi);
            for op_name in &prop.preserved_by {
                if let Some(op) = spec.handlers.iter().find(|o| &o.name == op_name) {
                    if !op.has_effect() {
                        continue;
                    }
                    // For multi-account: skip if op targets a different account than the property
                    if is_multi {
                        let (op_sn, _) = resolve_state_for_op(op, &spec, true);
                        if op_sn != prop_sn {
                            // Cross-account: this property is trivially preserved since
                            // the operation doesn't modify the property's state.
                            out.push_str(&format!(
                                "    // {}.{} skipped — {} operates on {}, not {}\n\n",
                                prop.name, op.name, op.name, op_sn, prop_sn
                            ));
                            continue;
                        }
                    }
                    generate_property_test(&mut out, op, prop, prop_fields, &prop_sn, &spec)?;
                }
            }
        }
    }

    out.push_str("    // ====================================================================\n");
    out.push_str("    // Unchanged field tests — fields not in effects must not change\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        let triples = effect_triples(&op.name, &mir, &spec);
        generate_unchanged_test(&mut out, op, &triples, fields, &sn, &spec)?;
    }

    let transition_ops: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
        .filter(|op| op.pre_status.is_some() && op.post_status.is_some())
        .collect();
    if !transition_ops.is_empty() {
        out.push_str(
            "    // ====================================================================\n",
        );
        out.push_str("    // State machine tests — verify lifecycle transitions\n");
        out.push_str(
            "    // ====================================================================\n\n",
        );

        for op in &transition_ops {
            let status_enum = if is_multi {
                let target = op
                    .on_account
                    .as_deref()
                    .unwrap_or(&spec.account_types[0].name);
                format!("{}Status", target)
            } else {
                "Status".to_string()
            };
            generate_state_machine_test(&mut out, op, &status_enum);
        }
    }

    out.push_str("}\n");

    // Count tests
    let effect_count = spec.handlers.iter().filter(|o| o.has_effect()).count();
    let guard_count = spec
        .handlers
        .iter()
        .filter(|o| guard_predicate_rust(o).is_some())
        .count()
        * 2; // pass + fail
    let prop_count: usize = spec
        .properties
        .iter()
        .map(|p| {
            p.preserved_by
                .iter()
                .filter(|name| {
                    spec.handlers
                        .iter()
                        .find(|o| &&o.name == name)
                        .is_some_and(|o| o.has_effect())
                })
                .count()
        })
        .sum();
    let unchanged_count = effect_count;
    let sm_count = transition_ops.len();
    let total = effect_count + guard_count + prop_count + unchanged_count + sm_count;

    write_generated_file(output_path, &out)?;

    eprintln!(
        "Generated {} unit tests in {}",
        total,
        output_path.display()
    );
    eprintln!("  {} effect test(s)", effect_count);
    eprintln!("  {} guard test(s)", guard_count);
    eprintln!("  {} property preservation test(s)", prop_count);
    eprintln!("  {} unchanged field test(s)", unchanged_count);
    eprintln!("  {} state machine test(s)", sm_count);

    Ok(())
}

/// Emit a state struct definition with Default impl.
fn emit_state_struct(
    out: &mut String,
    state_name: &str,
    fields: &[(String, String)],
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    out.push_str(&format!("struct {} {{\n", state_name));
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type(ftype, spec)?));
    }
    out.push_str("}\n\n");

    out.push_str(&format!("impl Default for {} {{\n", state_name));
    out.push_str("    fn default() -> Self {\n");
    out.push_str(&format!("        {} {{\n", state_name));
    for (fname, ftype) in fields {
        let default_val = match ftype.as_str() {
            "Pubkey" => "[0u8; 32]",
            "U64" => "0u64",
            "U128" => "0u128",
            "U8" => "0u8",
            "I128" => "0i128",
            "Bool" => "false",
            _ => "Default::default()",
        };
        out.push_str(&format!("            {}: {},\n", fname, default_val));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Resolve the state name and fields for an operation.
fn resolve_state_for_op<'a>(
    op: &ParsedHandler,
    spec: &'a ParsedSpec,
    is_multi: bool,
) -> (String, &'a [(String, String)]) {
    if is_multi {
        let target = op
            .on_account
            .as_deref()
            .unwrap_or(&spec.account_types[0].name);
        let acct = spec
            .account_types
            .iter()
            .find(|a| a.name == target)
            .unwrap_or(&spec.account_types[0]);
        (format!("{}State", acct.name), &acct.fields)
    } else {
        (
            format!(
                "{}State",
                crate::codegen_shared::to_pascal_case(&spec.program_name)
            ),
            &spec.state_fields,
        )
    }
}

/// Resolve the state name and fields for a property based on its expression's field references.
fn resolve_state_for_property<'a>(
    prop: &crate::check::ParsedProperty,
    spec: &'a ParsedSpec,
    is_multi: bool,
) -> (String, &'a [(String, String)]) {
    if !is_multi {
        return (
            format!(
                "{}State",
                crate::codegen_shared::to_pascal_case(&spec.program_name)
            ),
            &spec.state_fields,
        );
    }

    // Find which account type's fields match the property expression
    if let Some(ref expr) = prop.expression {
        for acct in &spec.account_types {
            if acct
                .fields
                .iter()
                .any(|(f, _)| expr.contains(&format!("s.{}", f)))
            {
                return (format!("{}State", acct.name), &acct.fields);
            }
        }
    }

    // Default to first account
    (
        format!("{}State", spec.account_types[0].name),
        &spec.account_types[0].fields,
    )
}

/// The handler's `requires` clauses as one Rust predicate bound to
/// `state` — tree-native render (#156; replaces the legacy `guard_str`
/// read that left requires-only handlers with a vacuous `true` guard fn
/// and an always-failing rejects-test). Requires touching handler-account
/// pubkeys are suppressed: the unit-test state struct carries no
/// accounts, matching the shared harness projection. `None` when nothing
/// is expressible — the caller skips the guard fn and its tests.
fn guard_predicate_rust(op: &ParsedHandler) -> Option<String> {
    let parts: Vec<String> = op
        .requires
        .iter()
        .map(requires_tree)
        .filter(|t| !crate::rust_codegen_util::tree_render::tree_mentions_account_pubkey(t))
        .map(|t| format!("({})", render_for_state(t)))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

/// Render a typed expression tree against the unit-test `state` binder.
fn render_for_state(tree: &crate::mir::ExprTree) -> String {
    use crate::rust_codegen_util::tree_render::{render_rust, Binder, RustCx};
    render_rust(
        tree,
        RustCx::native().with_binder(Binder::SelfAcct("state")),
    )
}

/// The typed tree of a requires clause. Post-#151 every production
/// `ParsedRequires` is adapter-built with `tree: Some(...)`; a `None`
/// here is a hand-built fixture that must be fixed, not worked around.
fn requires_tree(req: &crate::check::ParsedRequires) -> &crate::mir::ExprTree {
    req.tree
        .as_ref()
        .expect("ParsedRequires.tree is always populated by the chumsky adapter (#151/#156)")
}

/// Effect triples for a handler, projected from the lowered MIR body via
/// the shared `stmt_effect_triple` (#66) — the same iteration source the
/// Kani/proptest backends use — instead of string-matching raw
/// `op.effects`. Deep: `effect { match … }` arms flatten to their union,
/// matching the parser's back-compat `op.effects` view this file
/// previously consumed. Fields are flattened to the union-state view
/// (variant prefixes stripped); values carry the adapter-rendered Rust
/// RHS (falls back to the raw spec string for tree-less ingest paths).
fn effect_triples(op_name: &str, mir: &crate::mir::Mir, spec: &ParsedSpec) -> Vec<EffectTriple> {
    let Some(h) = mir.handlers.iter().find(|h| h.name == op_name) else {
        return Vec::new();
    };
    crate::rust_codegen_util::block_effect_triples_deep(&h.body)
        .into_iter()
        .map(|(field, kind, value)| {
            (
                cast_subscripts(
                    &crate::rust_codegen_util::strip_variant_prefix_for_flat_state(&field, spec),
                ),
                kind,
                crate::rust_codegen_util::mir_expr_rust(value),
            )
        })
        .collect()
}

/// Rewrite `[ident]` subscripts to `[ident as usize]` — unit tests bind
/// params at their spec types (`u8` etc.) while Rust arrays index by
/// `usize`. Numeric and compound subscripts pass through unchanged.
fn cast_subscripts(field: &str) -> String {
    let mut out = String::new();
    let mut rest = field;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let Some(j) = rest.find(']') else {
            out.push_str(rest);
            return out;
        };
        let idx = &rest[..j];
        let is_numeric = !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit());
        let is_ident = !idx.is_empty()
            && idx.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !idx.starts_with(|c: char| c.is_ascii_digit());
        if is_ident && !is_numeric {
            out.push_str(&format!("{} as usize", idx));
        } else {
            out.push_str(idx);
        }
        out.push(']');
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    out
}

/// Identifier-safe form of an effect-target path for `pre_*` snapshot
/// bindings: `voted[member_index]` → `voted_member_index_`.
fn pre_ident(field: &str) -> String {
    field
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// Build the argument list for calling apply_op / guard_op.
fn call_args(op: &ParsedHandler) -> String {
    if op.takes_params.is_empty() {
        return String::new();
    }
    let args: Vec<&str> = op.takes_params.iter().map(|(n, _)| n.as_str()).collect();
    format!(", {}", args.join(", "))
}

/// Generate a test that applies an operation's effects and checks the result.
fn generate_effect_test(
    out: &mut String,
    op: &ParsedHandler,
    triples: &[EffectTriple],
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_effects() {{\n", op.name));

    // Set up state with concrete values that satisfy the guard
    emit_state_literal(out, state_name, fields, op, &[], true);

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }

    // Snapshot pre-state for arithmetic effects
    for (field, kind, _) in triples {
        if *kind != "set" {
            out.push_str(&format!(
                "        let pre_{} = state.{};\n",
                pre_ident(field),
                field
            ));
        }
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    for (field, kind, value) in triples {
        let pre = format!("pre_{}", pre_ident(field));
        match *kind {
            "set" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {});\n",
                    field, value
                ));
            }
            "add" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {} + {});\n",
                    field, pre, value
                ));
            }
            "sub" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {} - {});\n",
                    field, pre, value
                ));
            }
            "add_sat" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.saturating_add({}));\n",
                    field, pre, value
                ));
            }
            "sub_sat" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.saturating_sub({}));\n",
                    field, pre, value
                ));
            }
            "add_wrap" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.wrapping_add({}));\n",
                    field, pre, value
                ));
            }
            "sub_wrap" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.wrapping_sub({}));\n",
                    field, pre, value
                ));
            }
            _ => {}
        }
    }

    out.push_str("    }\n\n");
    Ok(())
}

/// Generate pass/fail guard tests with boundary values.
fn generate_guard_tests(
    out: &mut String,
    op: &ParsedHandler,
    guard_rust: &str,
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    // --- Test: guard PASSES with valid inputs ---
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_guard_accepts_valid() {{\n",
        op.name
    ));
    emit_state_literal(out, state_name, fields, op, &[], false);
    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }
    out.push_str(&format!(
        "        assert!(guard_{}(&state{}));\n",
        op.name,
        call_args(op)
    ));
    out.push_str("    }\n\n");

    // --- Test: guard REJECTS invalid inputs ---
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_guard_rejects_invalid() {{\n",
        op.name
    ));

    // Try to derive a violating input from the guard
    let (state_overrides, param_overrides) = derive_guard_violation(guard_rust, op, fields);

    emit_state_literal_with(out, state_name, fields, op, &[], &state_overrides, false);
    for (pname, ptype) in &op.takes_params {
        if let Some(val) = param_overrides.iter().find(|(n, _)| n == pname) {
            out.push_str(&format!(
                "        let {}: {} = {};\n",
                pname,
                map_type(ptype, spec)?,
                val.1
            ));
        } else {
            let val = sensible_param(pname, ptype);
            out.push_str(&format!(
                "        let {}: {} = {};\n",
                pname,
                map_type(ptype, spec)?,
                val
            ));
        }
    }
    out.push_str(&format!(
        "        assert!(!guard_{}(&state{}));\n",
        op.name,
        call_args(op)
    ));
    out.push_str("    }\n\n");
    Ok(())
}

/// Generate a property preservation test for a specific operation.
fn generate_property_test(
    out: &mut String,
    op: &ParsedHandler,
    prop: &crate::check::ParsedProperty,
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_preserves_{}() {{\n",
        op.name, prop.name
    ));

    // Set up state that satisfies the property: seed values consider the
    // property body alongside the operation's own guards.
    let prop_rust = prop.tree.as_ref().map(render_for_state);
    let extra: Vec<&str> = prop_rust.as_deref().into_iter().collect();
    emit_state_literal(out, state_name, fields, op, &extra, true);

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    let prop_name_upper = prop.name.replace('_', " ");
    out.push_str(&format!(
        "        // Property: {} must hold after {}\n",
        prop_name_upper, op.name
    ));

    if let Some(rust_expr) = &prop_rust {
        out.push_str(&format!(
            "        assert!({}, \"{} must hold after {}\");\n",
            rust_expr, prop.name, op.name
        ));
    } else {
        out.push_str(&format!(
            "        // AGENT: assert property '{}' holds on state\n",
            prop.name
        ));
    }

    out.push_str("    }\n\n");
    Ok(())
}

/// Generate unchanged field tests — fields not in effects must not change.
fn generate_unchanged_test(
    out: &mut String,
    op: &ParsedHandler,
    triples: &[EffectTriple],
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    // Base name of each effect target: `voted[member_index]` affects
    // `voted` (the old raw-string comparison missed subscripted targets
    // and asserted them unchanged).
    let affected: Vec<&str> = triples
        .iter()
        .map(|(f, _, _)| crate::rust_codegen_util::effect_target_base(f))
        .collect();
    let unchanged: Vec<&(String, String)> = fields
        .iter()
        .filter(|(f, t)| !affected.contains(&f.as_str()) && t != "Pubkey")
        .collect();

    if unchanged.is_empty() {
        return Ok(());
    }

    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_unchanged_fields() {{\n", op.name));

    emit_state_literal(out, state_name, fields, op, &[], true);

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }

    for (fname, _) in &unchanged {
        out.push_str(&format!(
            "        let pre_{} = state.{}.clone();\n",
            fname, fname
        ));
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    for (fname, _) in &unchanged {
        out.push_str(&format!(
            "        assert_eq!(state.{}, pre_{}, \"{} must not change after {}\");\n",
            fname, fname, fname, op.name
        ));
    }

    out.push_str("    }\n\n");
    Ok(())
}

/// Generate a state machine test — verify the transition is valid.
fn generate_state_machine_test(out: &mut String, op: &ParsedHandler, status_enum: &str) {
    let pre = op.pre_status.as_ref().unwrap();
    let post = op.post_status.as_ref().unwrap();

    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_transition_{}_to_{}() {{\n",
        op.name,
        pre.to_lowercase(),
        post.to_lowercase()
    ));
    out.push_str(&format!(
        "        // {} requires status == {} and moves to {}\n",
        op.name, pre, post
    ));
    if pre == post {
        out.push_str(&format!(
            "        assert_eq!({}::{}, {}::{}, \"{} is a self-transition\");\n",
            status_enum, pre, status_enum, post, op.name
        ));
    } else {
        out.push_str(&format!(
            "        assert_ne!({}::{}, {}::{}, \"{} changes status\");\n",
            status_enum, pre, status_enum, post, op.name
        ));
    }
    out.push_str(&format!("        let _pre = {}::{};\n", status_enum, pre));
    out.push_str(&format!("        let _post = {}::{};\n", status_enum, post));
    out.push_str("        // AGENT: verify handler transitions status from _pre to _post\n");
    out.push_str("    }\n\n");
}

// ----------------------------------------------------------------------
// Spec-derived seed values (F7). Test-state literals were previously
// seeded by pattern-matching multisig-example field names ("threshold",
// "member_count", …) — hardcoded semantics that leaked into every other
// spec. Values now derive from the spec itself: type-based bases raised
// by the simple comparison atoms of the handler's guard/requires
// conjunction (plus the property body for property tests).
// ----------------------------------------------------------------------

/// One side of a comparison atom, resolved against the spec.
enum AtomSide {
    /// A bare state-field reference.
    Field(String),
    /// A handler param, carrying its `sensible_param` seed value.
    Param(String, u128),
    /// A numeric literal.
    Lit(u128),
}

/// Emit a `let [mut] state = <State> { … }` literal with spec-derived
/// seed values.
fn emit_state_literal(
    out: &mut String,
    state_name: &str,
    fields: &[(String, String)],
    op: &ParsedHandler,
    extra_guard_texts: &[&str],
    mutable: bool,
) {
    emit_state_literal_with(out, state_name, fields, op, extra_guard_texts, &[], mutable);
}

/// Like [`emit_state_literal`], with explicit per-field overrides (used
/// by the guard-rejection test to inject a violating value).
fn emit_state_literal_with(
    out: &mut String,
    state_name: &str,
    fields: &[(String, String)],
    op: &ParsedHandler,
    extra_guard_texts: &[&str],
    overrides: &[(String, String)],
    mutable: bool,
) {
    let seeds = seed_state_values(fields, op, extra_guard_texts);
    let mut_kw = if mutable { "mut " } else { "" };
    out.push_str(&format!(
        "        let {}state = {} {{\n",
        mut_kw, state_name
    ));
    for (fname, ftype) in fields {
        let val = overrides
            .iter()
            .find(|(n, _)| n == fname)
            .map(|(_, v)| v.clone())
            .or_else(|| seeds.get(fname).cloned())
            .unwrap_or_else(|| non_numeric_default(ftype));
        out.push_str(&format!("            {}: {},\n", fname, val));
    }
    out.push_str("        };\n");
}

/// Default literal for field types outside the seedable numeric set.
fn non_numeric_default(ftype: &str) -> String {
    match ftype {
        "Pubkey" => "[1u8; 32]".to_string(),
        "Bool" | "bool" => "false".to_string(),
        "I128" | "i128" => "0i128".to_string(),
        _ => "Default::default()".to_string(),
    }
}

/// Types the constraint seeding understands (unsigned only — the raise
/// rules reason in `u128`).
fn is_seedable_numeric(ftype: &str) -> bool {
    matches!(ftype, "U8" | "u8" | "U64" | "u64" | "U128" | "u128")
}

/// Render a numeric seed with the type's literal suffix convention.
fn render_seed(v: u128, ftype: &str) -> String {
    match ftype {
        "U128" | "u128" => format!("{}u128", v),
        _ => v.to_string(),
    }
}

/// Compute seed values for the numeric state fields: start at the
/// type-based base (`count`/`amount`/`value`-named U64 fields at 100 —
/// the legacy generic heuristic), then walk the comparison atoms of the
/// handler's guards (plus `extra_texts`) and raise values until simple
/// `a > b` / `a >= lit` shapes hold; `f == <lit>` pins exactly. Compound
/// sides are skipped — this is a seeding heuristic, not a solver.
fn seed_state_values(
    fields: &[(String, String)],
    op: &ParsedHandler,
    extra_texts: &[&str],
) -> std::collections::BTreeMap<String, String> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut vals: BTreeMap<String, u128> = BTreeMap::new();
    for (fname, ftype) in fields {
        if !is_seedable_numeric(ftype) {
            continue;
        }
        let base = if matches!(ftype.as_str(), "U64" | "u64")
            && (fname.contains("count") || fname.contains("amount") || fname.contains("value"))
        {
            100
        } else {
            0
        };
        vals.insert(fname.clone(), base);
    }

    // Guard conjunction: requires clauses + extras (property bodies),
    // all rendered from the typed trees against the `state` binder.
    let mut texts: Vec<String> = Vec::new();
    for req in &op.requires {
        texts.push(render_for_state(requires_tree(req)));
    }
    for t in extra_texts {
        texts.push((*t).to_string());
    }

    let atoms: Vec<(String, &'static str, String)> = texts
        .iter()
        .flat_map(|t| split_atoms(t))
        .filter_map(|a| parse_atom(&a))
        .collect();

    // `f == <lit>` pins the value exactly; inequality passes won't move it.
    let mut pinned: BTreeSet<String> = BTreeSet::new();
    for (lhs, cmp, rhs) in &atoms {
        if *cmp != "==" {
            continue;
        }
        match (resolve_side(lhs, fields, op), resolve_side(rhs, fields, op)) {
            (Some(AtomSide::Field(f)), Some(AtomSide::Lit(l)))
            | (Some(AtomSide::Lit(l)), Some(AtomSide::Field(f))) => {
                vals.insert(f.clone(), l);
                pinned.insert(f);
            }
            _ => {}
        }
    }

    // Raise-only fixpoint over the inequality atoms.
    for _ in 0..4 {
        for (lhs, cmp, rhs) in &atoms {
            let (Some(a), Some(b)) = (resolve_side(lhs, fields, op), resolve_side(rhs, fields, op))
            else {
                continue;
            };
            // Normalize to "left cmp right" with resolved values.
            let get = |s: &AtomSide, vals: &BTreeMap<String, u128>| match s {
                AtomSide::Field(f) => vals.get(f).copied(),
                AtomSide::Param(_, v) => Some(*v),
                AtomSide::Lit(l) => Some(*l),
            };
            let (Some(va), Some(vb)) = (get(&a, &vals), get(&b, &vals)) else {
                continue;
            };
            let mut adjust = |f: &str, v: u128, pinned: &BTreeSet<String>| {
                if !pinned.contains(f) {
                    vals.insert(f.to_string(), v);
                }
            };
            match (*cmp, &a, &b) {
                // field-vs-field: push the greater side up.
                ("<", _, AtomSide::Field(f)) if va >= vb => adjust(f, va + 2, &pinned),
                ("<=", _, AtomSide::Field(f)) if va > vb => adjust(f, va, &pinned),
                (">", AtomSide::Field(f), _) if va <= vb => adjust(f, vb + 2, &pinned),
                (">=", AtomSide::Field(f), _) if va < vb => adjust(f, vb, &pinned),
                // field-vs-literal upper bounds: clamp down.
                ("<", AtomSide::Field(f), AtomSide::Lit(l)) if va >= *l => {
                    adjust(f, l.saturating_sub(1), &pinned)
                }
                ("<=", AtomSide::Field(f), AtomSide::Lit(l)) if va > *l => adjust(f, *l, &pinned),
                (">", AtomSide::Lit(l), AtomSide::Field(f)) if *l <= vb => {
                    adjust(f, l.saturating_sub(1), &pinned)
                }
                (">=", AtomSide::Lit(l), AtomSide::Field(f)) if *l < vb => adjust(f, *l, &pinned),
                ("!=", AtomSide::Field(f), AtomSide::Lit(l)) if va == *l => {
                    adjust(f, l + 1, &pinned)
                }
                ("!=", AtomSide::Lit(l), AtomSide::Field(f)) if vb == *l => {
                    adjust(f, l + 1, &pinned)
                }
                _ => {}
            }
        }
    }

    let mut out = BTreeMap::new();
    for (fname, ftype) in fields {
        if let Some(v) = vals.get(fname) {
            out.insert(fname.clone(), render_seed(*v, ftype));
        }
    }
    out
}

/// Split a translated guard conjunction into candidate atoms on the
/// boolean connectives, stripping balanced outer parens.
fn split_atoms(text: &str) -> Vec<String> {
    text.split("&&")
        .flat_map(|p| p.split("||"))
        .map(strip_outer_parens)
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_outer_parens(s: &str) -> String {
    let mut t = s.trim();
    while t.starts_with('(') && t.ends_with(')') {
        // Only strip when the leading '(' matches the trailing ')'.
        let inner = &t[1..t.len() - 1];
        let mut depth = 0i32;
        let mut balanced = true;
        for c in inner.chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth < 0 {
                        balanced = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if balanced && depth == 0 {
            t = inner.trim();
        } else {
            break;
        }
    }
    t.to_string()
}

/// Parse `lhs <cmp> rhs` out of an atom; two-char comparators first so
/// `<=`/`>=` don't mis-split.
fn parse_atom(atom: &str) -> Option<(String, &'static str, String)> {
    for cmp in ["<=", ">=", "==", "!="] {
        if let Some(i) = atom.find(cmp) {
            return Some((
                atom[..i].trim().to_string(),
                cmp,
                atom[i + 2..].trim().to_string(),
            ));
        }
    }
    for cmp in ["<", ">"] {
        if let Some(i) = atom.find(cmp) {
            return Some((
                atom[..i].trim().to_string(),
                if cmp == "<" { "<" } else { ">" },
                atom[i + 1..].trim().to_string(),
            ));
        }
    }
    None
}

/// Resolve one atom side: a `state.`-prefixed or bare state-field name, a
/// numeric literal, or a handler param (folded to its `sensible_param`
/// value). Compound expressions resolve to `None` and skip the atom.
fn resolve_side(side: &str, fields: &[(String, String)], op: &ParsedHandler) -> Option<AtomSide> {
    let t = side.trim();
    let (name, had_state_prefix) = if let Some(rest) = t.strip_prefix("state.") {
        (rest, true)
    } else if let Some(rest) = t.strip_prefix("s.") {
        (rest, true)
    } else {
        (t, false)
    };
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if !had_state_prefix {
        if let Ok(n) = name.parse::<u128>() {
            return Some(AtomSide::Lit(n));
        }
        // A handler param shadows a same-named state field in guard position.
        if let Some((pn, pt)) = op.takes_params.iter().find(|(p, _)| p == name) {
            return sensible_param(pn, pt)
                .parse::<u128>()
                .ok()
                .map(|v| AtomSide::Param(pn.clone(), v));
        }
    }
    if fields.iter().any(|(f, _)| f == name) {
        return Some(AtomSide::Field(name.to_string()));
    }
    None
}

/// Pick a sensible param value — generic name/type heuristics only (the
/// multisig-specific names were removed in F7).
fn sensible_param(pname: &str, ptype: &str) -> String {
    match ptype {
        "Pubkey" => "[1u8; 32]".to_string(),
        "Bool" | "bool" => "true".to_string(),
        _ if pname.contains("index") => "0".to_string(),
        _ if pname.contains("amount") || pname.contains("value") || pname.contains("delta") => {
            "100".to_string()
        }
        _ => "1".to_string(),
    }
}

/// Try to derive inputs that violate the guard.
/// Returns (state_overrides, param_overrides) — field name → value pairs.
type Overrides = Vec<(String, String)>;

/// Negate the first comparison atom with a resolvable target (generic —
/// F7 removed the multisig-specific pattern list). Falls back to zeroing
/// every numeric param.
fn derive_guard_violation(
    guard_rust: &str,
    op: &ParsedHandler,
    fields: &[(String, String)],
) -> (Overrides, Overrides) {
    let mut state_overrides = Vec::new();
    let mut param_overrides = Vec::new();

    for atom in split_atoms(guard_rust) {
        let Some((lhs, cmp, rhs)) = parse_atom(&atom) else {
            continue;
        };
        // Normalize to `<name> cmp <literal>` (mirror literal-first atoms).
        let normalized = match (
            resolve_side(&lhs, fields, op),
            resolve_side(&rhs, fields, op),
        ) {
            (Some(AtomSide::Field(f)), Some(AtomSide::Lit(l))) => Some((f, false, cmp, l)),
            (Some(AtomSide::Param(p, _)), Some(AtomSide::Lit(l))) => Some((p, true, cmp, l)),
            (Some(AtomSide::Lit(l)), Some(AtomSide::Field(f))) => {
                Some((f, false, flip_cmp(cmp), l))
            }
            (Some(AtomSide::Lit(l)), Some(AtomSide::Param(p, _))) => {
                Some((p, true, flip_cmp(cmp), l))
            }
            _ => None,
        };
        let Some((name, is_param, cmp, l)) = normalized else {
            continue;
        };
        // Boundary value that breaks the atom (skip `>= 0`: unsigned).
        let value = match cmp {
            ">" => Some(l),
            ">=" if l > 0 => Some(l - 1),
            "<" => Some(l),
            "<=" => Some(l + 1),
            "==" => Some(l + 1),
            "!=" => Some(l),
            _ => None,
        };
        if let Some(v) = value {
            let entry = (name, v.to_string());
            if is_param {
                param_overrides.push(entry);
            } else {
                state_overrides.push(entry);
            }
            break;
        }
    }

    if state_overrides.is_empty() && param_overrides.is_empty() {
        // Generic fallback: just try setting all numeric params to 0
        for (pname, ptype) in &op.takes_params {
            if matches!(ptype.as_str(), "U8" | "U64" | "U128") {
                param_overrides.push((pname.clone(), "0".to_string()));
            }
        }
    }

    (state_overrides, param_overrides)
}

/// Mirror a comparator across its operands (`0 < x` ⇔ `x > 0`).
fn flip_cmp(cmp: &'static str) -> &'static str {
    match cmp {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #156 regression: the guard predicate renders from the requires
    /// trees. The legacy path read only the deleted `guard_str`, so every
    /// requires-only handler got `fn guard_x { true }` plus a rejects-test
    /// asserting `!true` — a generated test that always failed. Handlers
    /// whose requires are all account-suppressed must get no guard fn and
    /// no guard tests at all (same failure shape, `!(true)`).
    #[test]
    fn guard_predicates_render_requires_and_skip_suppressed_handlers() {
        let src = r#"spec T
type State | Active of { admin_key : Pubkey, pool : U64 }
type Error | Unauthorized | InvalidAmount
handler swap (amount : U64) (min_out : U64) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires amount >= min_out and min_out > 0 else InvalidAmount
  effect { pool += amount }
}
handler close : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires admin.pubkey == state.admin_key else Unauthorized
  effect { pool := 0 }
}
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("t.qedspec");
        std::fs::write(&spec_path, src).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        let out = std::fs::read_to_string(&out_path).expect("read output");

        // Requires-derived guard body, not the vacuous `true`.
        assert!(out.contains("fn guard_swap"), "guard fn emitted:\n{out}");
        assert!(
            out.contains("amount >= min_out") && out.contains("min_out > 0"),
            "guard body renders the requires conjunction:\n{out}"
        );
        // Account-suppressed handler: no guard fn, no failing rejects-test.
        assert!(
            !out.contains("fn guard_close") && !out.contains("test_close_guard_rejects_invalid"),
            "suppressed handler must not emit guard fn or tests:\n{out}"
        );
        // No vacuous `true` guard body anywhere.
        assert!(
            !out.contains("-> bool {\n    true\n}"),
            "no guard predicate may degrade to `true`:\n{out}"
        );
    }
}
