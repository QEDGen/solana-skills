use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedSpec};
use crate::codegen::map_type;

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

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let hash = fp
        .file_hashes
        .get("src/tests.rs")
        .cloned()
        .unwrap_or_default();

    let is_multi = spec.account_types.len() > 1;
    let mut out = String::new();

    // Header
    out.push_str(&crate::banner::banner(Some("DO NOT EDIT"), &hash));
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

    if is_multi {
        // Multi-account: one struct + status enum per account type
        for acct in &spec.account_types {
            let state_name = format!("{}State", acct.name);
            emit_state_struct(&mut out, &state_name, &acct.fields);

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
            crate::codegen::to_pascal_case(&spec.program_name)
        );
        emit_state_struct(&mut out, &state_name, &spec.state_fields);

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
        // Prefix unused params with _ to suppress warnings
        let effect_values: Vec<&str> = op.effects.iter().map(|(_, _, v)| v.as_str()).collect();
        let params: Vec<String> = op
            .takes_params
            .iter()
            .map(|(n, t)| {
                let used = effect_values.iter().any(|v| v.contains(n.as_str()));
                if used {
                    format!("{}: {}", n, map_type(t))
                } else {
                    format!("_{}: {}", n, map_type(t))
                }
            })
            .collect();
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
        for (field, kind, value) in &op.effects {
            match kind.as_str() {
                "set" => {
                    out.push_str(&format!("    state.{} = {};\n", field, value));
                }
                "add" => {
                    out.push_str(&format!("    state.{} += {};\n", field, value));
                }
                "sub" => {
                    out.push_str(&format!("    state.{} -= {};\n", field, value));
                }
                _ => {
                    out.push_str(&format!(
                        "    // unknown effect: {} {} {}\n",
                        field, kind, value
                    ));
                }
            }
        }
        out.push_str("}\n\n");
    }

    // Helper: guard predicates
    for op in &spec.handlers {
        if !op.has_guard() {
            continue;
        }
        let (op_state_name, _) = resolve_state_for_op(op, &spec, is_multi);
        let guard = op.guard_str.as_deref().unwrap_or("true");
        let guard_rust = translate_guard(guard, "state");
        let params: Vec<String> = op
            .takes_params
            .iter()
            .map(|(n, t)| format!("{}: {}", n, map_type(t)))
            .collect();
        let param_sig = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        // If the guard doesn't reference state fields, prefix with _
        let state_param = if guard.contains("s.") {
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

    // =========================================================================
    // Tests
    // =========================================================================
    out.push_str("#[cfg(test)]\nmod tests {\n    use super::*;\n\n");

    // --- Effect tests: each operation's effects produce correct state ---
    out.push_str("    // ====================================================================\n");
    out.push_str("    // Effect tests — verify state mutations match spec\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        generate_effect_test(&mut out, op, fields, &sn);
    }

    // --- Guard tests: boundary values that pass/fail ---
    out.push_str("    // ====================================================================\n");
    out.push_str("    // Guard tests — verify boundary conditions\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_guard() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        generate_guard_tests(&mut out, op, fields, &sn);
    }

    // --- Property preservation tests ---
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
                    generate_property_test(&mut out, op, prop, prop_fields, &prop_sn);
                }
            }
        }
    }

    // --- Unchanged field tests ---
    out.push_str("    // ====================================================================\n");
    out.push_str("    // Unchanged field tests — fields not in effects must not change\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        generate_unchanged_test(&mut out, op, fields, &sn);
    }

    // --- State machine tests ---
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
            // For multi-account, use per-account status enum
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

    std::fs::write(output_path, &out)?;

    // Count tests
    let effect_count = spec.handlers.iter().filter(|o| o.has_effect()).count();
    let guard_count = spec.handlers.iter().filter(|o| o.has_guard()).count() * 2; // pass + fail
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
fn emit_state_struct(out: &mut String, state_name: &str, fields: &[(String, String)]) {
    out.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    out.push_str(&format!("struct {} {{\n", state_name));
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type(ftype)));
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
                crate::codegen::to_pascal_case(&spec.program_name)
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
                crate::codegen::to_pascal_case(&spec.program_name)
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

/// Translate a Lean guard expression to Rust.
fn translate_guard(guard: &str, state_var: &str) -> String {
    guard
        .replace("s.", &format!("{}.", state_var))
        .replace('≤', "<=")
        .replace('≥', ">=")
        .replace('∧', "&&")
        .replace('∨', "||")
        .replace('≠', "!=")
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
    fields: &[(String, String)],
    state_name: &str,
) {
    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_effects() {{\n", op.name));

    // Set up state with concrete values that satisfy the guard
    out.push_str(&format!("        let mut state = {} {{\n", state_name));
    for (fname, ftype) in fields {
        let val = sensible_default(fname, ftype, op);
        out.push_str(&format!("            {}: {},\n", fname, val));
    }
    out.push_str("        };\n");

    // Declare params with concrete values
    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype, op);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype),
            val
        ));
    }

    // Snapshot pre-state
    for (field, kind, _) in &op.effects {
        if kind == "add" || kind == "sub" {
            out.push_str(&format!("        let pre_{} = state.{};\n", field, field));
        }
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    // Assert effects
    for (field, kind, value) in &op.effects {
        match kind.as_str() {
            "set" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {});\n",
                    field, value
                ));
            }
            "add" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, pre_{} + {});\n",
                    field, field, value
                ));
            }
            "sub" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, pre_{} - {});\n",
                    field, field, value
                ));
            }
            _ => {}
        }
    }

    out.push_str("    }\n\n");
}

/// Generate pass/fail guard tests with boundary values.
fn generate_guard_tests(
    out: &mut String,
    op: &ParsedHandler,
    fields: &[(String, String)],
    state_name: &str,
) {
    let guard_str = op.guard_str.as_deref().unwrap_or("true");

    // --- Test: guard PASSES with valid inputs ---
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_guard_accepts_valid() {{\n",
        op.name
    ));
    out.push_str(&format!("        let state = {} {{\n", state_name));
    for (fname, ftype) in fields {
        let val = sensible_default(fname, ftype, op);
        out.push_str(&format!("            {}: {},\n", fname, val));
    }
    out.push_str("        };\n");
    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype, op);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype),
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
    let (state_overrides, param_overrides) = derive_guard_violation(guard_str, op);

    out.push_str(&format!("        let state = {} {{\n", state_name));
    for (fname, ftype) in fields {
        if let Some(val) = state_overrides.iter().find(|(n, _)| n == fname) {
            out.push_str(&format!("            {}: {},\n", fname, val.1));
        } else {
            let val = sensible_default(fname, ftype, op);
            out.push_str(&format!("            {}: {},\n", fname, val));
        }
    }
    out.push_str("        };\n");
    for (pname, ptype) in &op.takes_params {
        if let Some(val) = param_overrides.iter().find(|(n, _)| n == pname) {
            out.push_str(&format!(
                "        let {}: {} = {};\n",
                pname,
                map_type(ptype),
                val.1
            ));
        } else {
            let val = sensible_param(pname, ptype, op);
            out.push_str(&format!(
                "        let {}: {} = {};\n",
                pname,
                map_type(ptype),
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
}

/// Generate a property preservation test for a specific operation.
fn generate_property_test(
    out: &mut String,
    op: &ParsedHandler,
    prop: &crate::check::ParsedProperty,
    fields: &[(String, String)],
    state_name: &str,
) {
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_preserves_{}() {{\n",
        op.name, prop.name
    ));

    // Set up state that satisfies the property
    out.push_str(&format!("        let mut state = {} {{\n", state_name));
    for (fname, ftype) in fields {
        let val = sensible_default(fname, ftype, op);
        out.push_str(&format!("            {}: {},\n", fname, val));
    }
    out.push_str("        };\n");

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype, op);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype),
            val
        ));
    }

    // Apply effects
    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    // Assert property still holds
    let prop_name_upper = prop.name.replace('_', " ");
    out.push_str(&format!(
        "        // Property: {} must hold after {}\n",
        prop_name_upper, op.name
    ));

    if let Some(ref expr) = prop.expression {
        let rust_expr = translate_guard(expr, "state");
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
}

/// Generate unchanged field tests — fields not in effects must not change.
fn generate_unchanged_test(
    out: &mut String,
    op: &ParsedHandler,
    fields: &[(String, String)],
    state_name: &str,
) {
    let affected: Vec<&str> = op.effects.iter().map(|(f, _, _)| f.as_str()).collect();
    let unchanged: Vec<&(String, String)> = fields
        .iter()
        .filter(|(f, t)| !affected.contains(&f.as_str()) && t != "Pubkey")
        .collect();

    if unchanged.is_empty() {
        return;
    }

    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_unchanged_fields() {{\n", op.name));

    out.push_str(&format!("        let mut state = {} {{\n", state_name));
    for (fname, ftype) in fields {
        let val = sensible_default(fname, ftype, op);
        out.push_str(&format!("            {}: {},\n", fname, val));
    }
    out.push_str("        };\n");

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype, op);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype),
            val
        ));
    }

    // Snapshot
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

/// Pick a sensible default value for a state field given the operation context.
fn sensible_default(fname: &str, ftype: &str, op: &ParsedHandler) -> String {
    // Try to pick values that satisfy common guards
    match ftype {
        "Pubkey" => "[1u8; 32]".to_string(),
        "U8" | "u8" => {
            // If this field appears in a guard like "threshold > 0", use a non-zero value
            if fname == "threshold" {
                "3".to_string()
            } else if fname == "member_count" {
                "5".to_string()
            } else if fname == "approval_count" {
                // For execute guard (approval_count >= threshold), set appropriately
                if op
                    .guard_str
                    .as_deref()
                    .unwrap_or("")
                    .contains("approval_count")
                {
                    "3".to_string()
                } else {
                    "0".to_string()
                }
            } else {
                "0".to_string()
            }
        }
        "U64" | "u64" => {
            if fname.contains("count") || fname.contains("amount") || fname.contains("value") {
                "100".to_string()
            } else {
                "0".to_string()
            }
        }
        "U128" | "u128" => "0u128".to_string(),
        "I128" | "i128" => "0i128".to_string(),
        "Bool" | "bool" => "false".to_string(),
        _ => "Default::default()".to_string(),
    }
}

/// Pick a sensible param value that satisfies the guard.
fn sensible_param(pname: &str, ptype: &str, _op: &ParsedHandler) -> String {
    match ptype {
        "U8" | "u8" => {
            if pname == "threshold" {
                "3".to_string()
            } else if pname == "member_count" {
                "5".to_string()
            } else if pname.contains("index") {
                "0".to_string()
            } else {
                "1".to_string()
            }
        }
        "U64" | "u64" => {
            if pname.contains("amount") || pname.contains("value") || pname.contains("delta") {
                "100".to_string()
            } else {
                "1".to_string()
            }
        }
        _ => "1".to_string(),
    }
}

/// Try to derive inputs that violate the guard.
/// Returns (state_overrides, param_overrides) — field name → value pairs.
type Overrides = Vec<(String, String)>;

fn derive_guard_violation(guard: &str, op: &ParsedHandler) -> (Overrides, Overrides) {
    let mut state_overrides = Vec::new();
    let mut param_overrides = Vec::new();

    // Simple heuristic: look for common patterns and negate one clause
    // "threshold > 0" → set threshold = 0
    // "member_index < s.member_count" → set member_index = member_count
    // "s.approval_count ≥ s.threshold" → set approval_count = 0, threshold = 3

    if guard.contains("threshold > 0") || guard.contains("threshold>0") {
        // Violate by setting threshold to 0
        if op.takes_params.iter().any(|(n, _)| n == "threshold") {
            param_overrides.push(("threshold".to_string(), "0".to_string()));
        } else {
            state_overrides.push(("threshold".to_string(), "0".to_string()));
        }
    } else if guard.contains("member_index") && guard.contains("member_count") {
        // member_index < s.member_count → set member_index >= member_count
        param_overrides.push(("member_index".to_string(), "5".to_string()));
        state_overrides.push(("member_count".to_string(), "5".to_string()));
    } else if guard.contains("approval_count") && guard.contains("threshold") {
        // s.approval_count >= s.threshold → set approval_count < threshold
        state_overrides.push(("approval_count".to_string(), "0".to_string()));
        state_overrides.push(("threshold".to_string(), "3".to_string()));
    } else if guard.contains("member_count") && guard.contains("threshold") {
        // s.member_count > s.threshold → set member_count == threshold
        state_overrides.push(("member_count".to_string(), "3".to_string()));
        state_overrides.push(("threshold".to_string(), "3".to_string()));
    } else {
        // Generic: just try setting all numeric params to 0
        for (pname, ptype) in &op.takes_params {
            if matches!(ptype.as_str(), "U8" | "U64" | "U128") {
                param_overrides.push((pname.clone(), "0".to_string()));
            }
        }
    }

    (state_overrides, param_overrides)
}
