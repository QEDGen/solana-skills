//! Generate Lean 4 source from a `ParsedSpec`.
//!
//! Replaces the Lean elaborator as the source of truth when using `.qedspec` files.
//! Produces the same structures: State, Status, transitions, Operation inductive,
//! applyOp, CPI theorems, property predicates, and inductive preservation theorems.

use anyhow::Result;
use std::path::Path;

use crate::check::ParsedSpec;

/// Generate a Lean file from a `ParsedSpec` and write it to `output_path`.
pub fn generate(spec: &ParsedSpec, output_path: &Path) -> Result<()> {
    let content = render(spec);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &content)?;
    eprintln!("  wrote {}", output_path.display());
    Ok(())
}

/// Render a `ParsedSpec` into a complete Lean 4 source string.
pub fn render(spec: &ParsedSpec) -> String {
    // sBPF mode: target is "assembly", or assembly_path is set with instructions
    if spec.target.as_deref() == Some("assembly")
        || (spec.assembly_path.is_some() && !spec.instructions.is_empty())
    {
        return render_sbpf(spec);
    }

    let is_multi_account = spec.account_types.len() > 1;

    if is_multi_account {
        render_multi_account(spec)
    } else {
        render_single_account(spec)
    }
}

/// Single-account rendering — original path, backward-compatible output.
fn render_single_account(spec: &ParsedSpec) -> String {
    let mut out = String::new();

    // Header
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.State\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");

    let name = &spec.program_name;

    out.push_str(&format!("namespace {}\n\n", name));
    out.push_str("open QEDGen.Solana\n\n");

    // Status inductive (if lifecycle states exist)
    let has_lifecycle = !spec.lifecycle_states.is_empty();
    if has_lifecycle {
        out.push_str("inductive Status where\n");
        for s in &spec.lifecycle_states {
            out.push_str(&format!("  | {}\n", s));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
    }

    // State structure
    out.push_str("structure State where\n");
    for (fname, ftype) in &spec.state_fields {
        out.push_str(&format!("  {} : {}\n", safe_name(fname), map_type(ftype)));
    }
    if has_lifecycle {
        out.push_str("  status : Status\n");
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

    // Transition functions
    let ops_refs: Vec<&crate::check::ParsedHandler> = spec.handlers.iter().collect();
    render_transitions(
        &mut out,
        spec,
        &ops_refs,
        &spec.state_fields,
        "State",
        "Status",
    );

    // CPI theorems
    render_cpi_theorems(&mut out, &ops_refs);

    // Invariants
    for (inv_name, _desc) in &spec.invariants {
        out.push_str(&format!(
            "/-- Invariant: {}. -/\ntheorem {} : True := sorry\n\n",
            inv_name, inv_name
        ));
    }

    // Operation inductive + applyOp
    render_operation_inductive(&mut out, &ops_refs, "State");

    // Property predicates and inductive theorems
    render_properties(&mut out, &spec.properties, &ops_refs, "State");

    // Abort theorems (aborts_if clauses)
    render_aborts_if(&mut out, &ops_refs, "State");

    // Post-condition theorems (ensures clauses)
    render_ensures(&mut out, &ops_refs, "State");

    // Frame condition theorems (modifies clauses)
    render_frame_conditions(&mut out, &ops_refs, &spec.state_fields, "State");

    // Cover properties (reachability)
    render_covers(&mut out, spec, "State");

    // Liveness properties (leads-to)
    render_liveness(&mut out, spec, "State");

    // Environment blocks (external state)
    render_environments(&mut out, spec, "State");

    // Overflow obligations for operations with add effects
    render_overflow_obligations(&mut out, spec, &ops_refs, &spec.state_fields, "State");

    out.push_str(&format!("end {}\n", name));
    out
}

/// Multi-account rendering — per-account sections with scoped types.
fn render_multi_account(spec: &ParsedSpec) -> String {
    let mut out = String::new();

    // Header
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.State\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");

    let name = &spec.program_name;

    out.push_str(&format!("namespace {}\n\n", name));
    out.push_str("open QEDGen.Solana\n\n");

    // Per-account sections
    for acct in &spec.account_types {
        let acct_name = &acct.name;
        let status_name = format!("{}Status", acct_name);
        let state_name = format!("{}State", acct_name);

        // Status inductive
        if !acct.lifecycle.is_empty() {
            out.push_str(&format!("inductive {} where\n", status_name));
            for s in &acct.lifecycle {
                out.push_str(&format!("  | {}\n", s));
            }
            out.push_str("  deriving Repr, DecidableEq, BEq\n\n");
        }

        // State structure
        out.push_str(&format!("structure {} where\n", state_name));
        for (fname, ftype) in &acct.fields {
            out.push_str(&format!("  {} : {}\n", safe_name(fname), map_type(ftype)));
        }
        if !acct.lifecycle.is_empty() {
            out.push_str(&format!("  status : {}\n", status_name));
        }
        out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

        // Operations targeting this account
        let ops: Vec<&crate::check::ParsedHandler> = spec
            .handlers
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(acct_name.as_str())
                    || (op.on_account.is_none() && acct_name == &spec.account_types[0].name)
            })
            .collect();

        if ops.is_empty() {
            continue;
        }

        // Transition functions
        render_transitions(
            &mut out,
            spec,
            &ops,
            &acct.fields,
            &state_name,
            &status_name,
        );

        // CPI theorems
        render_cpi_theorems(&mut out, &ops);

        // Operation inductive + applyOp per account
        render_operation_inductive(&mut out, &ops, &state_name);
    }

    // Invariants
    for (inv_name, _desc) in &spec.invariants {
        out.push_str(&format!(
            "/-- Invariant: {}. -/\ntheorem {} : True := sorry\n\n",
            inv_name, inv_name
        ));
    }

    // Properties — for multi-account, reference the state type from the first account
    // that has matching fields. Properties using `state.X` bind to the account whose
    // fields contain X.
    render_properties_multi(&mut out, spec);

    // v2.0 features: aborts_if, covers, liveness, environments, overflow
    // Per-account: aborts_if and overflow need the ops for each account
    for acct in &spec.account_types {
        let state_name = format!("{}State", acct.name);
        let ops: Vec<&crate::check::ParsedHandler> = spec
            .handlers
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(acct.name.as_str())
                    || (op.on_account.is_none() && acct.name == spec.account_types[0].name)
            })
            .collect();
        if ops.is_empty() {
            continue;
        }
        render_aborts_if(&mut out, &ops, &state_name);
        render_ensures(&mut out, &ops, &state_name);
        render_frame_conditions(&mut out, &ops, &acct.fields, &state_name);
        render_overflow_obligations(&mut out, spec, &ops, &acct.fields, &state_name);
    }

    // Spec-level: covers, liveness, environments use the first account's state type
    let primary_state = if spec.account_types.is_empty() {
        "State".to_string()
    } else {
        format!("{}State", spec.account_types[0].name)
    };
    render_covers(&mut out, spec, &primary_state);
    render_liveness(&mut out, spec, &primary_state);
    render_environments(&mut out, spec, &primary_state);

    out.push_str(&format!("end {}\n", name));
    out
}

/// Render transition functions for a set of handlers.
fn render_transitions(
    out: &mut String,
    spec: &ParsedSpec,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
    _status_type: &str,
) {
    for op in ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);

        // Build condition parts
        let mut cond_parts: Vec<String> = Vec::new();
        if let Some(ref who) = op.who {
            cond_parts.push(format!("signer = s.{}", safe_name(who)));
        }
        if let Some(ref pre) = op.pre_status {
            cond_parts.push(format!("s.status = .{}", pre));
        }
        // Auto-guards for sub effects (underflow prevention)
        for (field, op_kind, _value) in &op.effects {
            if op_kind == "sub" {
                let ftype = fields
                    .iter()
                    .find(|(n, _)| n == field)
                    .or_else(|| spec.state_fields.iter().find(|(n, _)| n == field))
                    .map(|(_, t)| t.as_str())
                    .unwrap_or("");
                if map_type(ftype) != "Int" {
                    let val = &op
                        .effects
                        .iter()
                        .find(|(f, o, _)| f == field && o == "sub")
                        .unwrap()
                        .2;
                    cond_parts.push(format!("{} \u{2264} s.{}", val, safe_name(field)));
                }
            }
        }
        if let Some(ref guard) = op.guard_str {
            cond_parts.push(guard.clone());
        }
        // Requires clauses contribute their positive form as guard conditions
        for req in &op.requires {
            cond_parts.push(req.lean_expr.clone());
        }
        // Auto-guards for add effects (overflow prevention, type-aware).
        // Only insert if no existing guard/requires already bounds the sum.
        for (field, op_kind, value) in &op.effects {
            if op_kind == "add" {
                let ftype = fields
                    .iter()
                    .find(|(n, _)| n == field)
                    .or_else(|| spec.state_fields.iter().find(|(n, _)| n == field))
                    .map(|(_, t)| t.as_str())
                    .unwrap_or("");
                if let Some(max_const) = type_max_const(ftype) {
                    let sf = safe_name(field);
                    // Check if any existing condition already bounds this field's addition
                    let already_guarded = cond_parts.iter().any(|c| {
                        c.contains(&format!("s.{} + {}", sf, value))
                            || c.contains(&format!("{} + s.{}", value, sf))
                    });
                    if !already_guarded {
                        cond_parts
                            .push(format!("s.{} + {} \u{2264} {}", sf, value, max_const));
                    }
                }
            }
        }

        let has_cond = !cond_parts.is_empty();
        let if_cond = cond_parts.join(" \u{2227} "); // ∧

        // Build state update
        let mut with_parts: Vec<String> = Vec::new();
        for (field, op_kind, value) in &op.effects {
            let sf = safe_name(field);
            match op_kind.as_str() {
                "add" => with_parts.push(format!("{} := s.{} + {}", sf, sf, value)),
                "sub" => with_parts.push(format!("{} := s.{} - {}", sf, sf, value)),
                "set" => with_parts.push(format!("{} := {}", sf, value)),
                _ => {}
            }
        }
        if let Some(ref post) = op.post_status {
            with_parts.push(format!("status := .{}", post));
        }

        let then_body = if with_parts.is_empty() {
            "some s".to_string()
        } else {
            format!("some {{ s with {} }}", with_parts.join(", "))
        };

        out.push_str(&format!(
            "def {} (s : {}) (signer : Pubkey){} : Option {} :=\n",
            trans_name, state_type, param_sig, state_type
        ));

        // Emit let bindings before the if condition
        for (binding_name, lean_expr, _rust_expr) in &op.let_bindings {
            out.push_str(&format!("  let {} := {}\n", safe_name(binding_name), lean_expr));
        }

        if has_cond {
            out.push_str(&format!("  if {} then\n", if_cond));
            out.push_str(&format!("    {}\n", then_body));
            out.push_str("  else none\n\n");
        } else {
            out.push_str(&format!("  {}\n\n", then_body));
        }
    }
}

/// Render transfer correctness theorems from handler transfers blocks.
fn render_cpi_theorems(out: &mut String, ops: &[&crate::check::ParsedHandler]) {
    for op in ops {
        if !op.has_calls() {
            continue;
        }

        for (i, transfer) in op.transfers.iter().enumerate() {
            let suffix = if op.transfers.len() > 1 {
                format!("_{}", i)
            } else {
                String::new()
            };
            let theorem_name = safe_name(&format!("{}_transfer{}_correct", op.name, suffix));

            out.push_str(&format!(
                "/-- {} transfer: {} → {}",
                op.name, transfer.from, transfer.to
            ));
            if let Some(ref amt) = transfer.amount {
                out.push_str(&format!(" amount {}", amt));
            }
            if let Some(ref auth) = transfer.authority {
                out.push_str(&format!(" authority {}", auth));
            }
            out.push_str(". -/\n");
            out.push_str(&format!("theorem {} : True := sorry\n\n", theorem_name));
        }
    }
}

/// Render Operation inductive and applyOp dispatcher.
fn render_operation_inductive(
    out: &mut String,
    ops: &[&crate::check::ParsedHandler],
    state_type: &str,
) {
    if ops.is_empty() {
        return;
    }

    // For multi-account, prefix with account name to avoid name collisions
    let prefix = if state_type != "State" {
        // e.g., "PoolState" -> "Pool"
        state_type.strip_suffix("State").unwrap_or(state_type)
    } else {
        ""
    };
    let op_type = if prefix.is_empty() {
        "Operation".to_string()
    } else {
        format!("{}Operation", prefix)
    };
    let apply_name = if prefix.is_empty() {
        "applyOp".to_string()
    } else {
        format!("apply{}Op", prefix)
    };

    out.push_str(&format!("inductive {} where\n", op_type));
    for op in ops {
        let ctor = safe_name(&op.name);
        if op.takes_params.is_empty() {
            out.push_str(&format!("  | {}\n", ctor));
        } else {
            let params: Vec<String> = op
                .takes_params
                .iter()
                .map(|(pn, pt)| format!("({} : {})", pn, map_type(pt)))
                .collect();
            out.push_str(&format!("  | {} {}\n", ctor, params.join(" ")));
        }
    }
    out.push_str("  deriving Repr, DecidableEq, BEq\n\n");

    // applyOp dispatcher
    out.push_str(&format!(
        "def {} (s : {}) (signer : Pubkey) : {} \u{2192} Option {}\n",
        apply_name, state_type, op_type, state_type
    ));
    for op in ops {
        let ctor = safe_name(&op.name);
        let trans = safe_name(&format!("{}Transition", op.name));
        let param_names: Vec<String> = op.takes_params.iter().map(|(n, _)| n.clone()).collect();
        let param_args = if param_names.is_empty() {
            String::new()
        } else {
            format!(" {}", param_names.join(" "))
        };
        let call_args = if param_names.is_empty() {
            String::new()
        } else {
            format!(" {}", param_names.join(" "))
        };
        out.push_str(&format!(
            "  | .{}{} => {} s signer{}\n",
            ctor, param_args, trans, call_args
        ));
    }
    out.push('\n');
}

/// Render properties for single-account specs.
fn render_properties(
    out: &mut String,
    properties: &[crate::check::ParsedProperty],
    ops: &[&crate::check::ParsedHandler],
    state_type: &str,
) {
    render_properties_inner(out, properties, ops, state_type, "Operation", "applyOp");
}

/// Render properties for multi-account specs.
fn render_properties_multi(out: &mut String, spec: &ParsedSpec) {
    // Group properties by target account, then delegate to render_properties_inner.
    // Heuristic: look at the expression's `s.field` references and match against account fields.

    // Collect properties by target account
    let mut groups: std::collections::HashMap<String, Vec<&crate::check::ParsedProperty>> =
        std::collections::HashMap::new();
    let mut acct_for_prop: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for prop in &spec.properties {
        let target_name = if let Some(ref expr) = prop.expression {
            spec.account_types
                .iter()
                .find(|a| {
                    a.fields
                        .iter()
                        .any(|(f, _)| expr.contains(&format!("s.{}", f)))
                })
                .map(|a| a.name.clone())
                .unwrap_or_else(|| spec.account_types[0].name.clone())
        } else {
            spec.account_types[0].name.clone()
        };
        acct_for_prop.insert(prop.name.clone(), target_name.clone());
        groups.entry(target_name).or_default().push(prop);
    }

    for (acct_name, props) in &groups {
        let state_type = format!("{}State", acct_name);
        let op_type = format!("{}Operation", acct_name);
        let apply_name = format!("apply{}Op", acct_name);

        let acct_ops: Vec<&crate::check::ParsedHandler> = spec
            .handlers
            .iter()
            .filter(|op| {
                op.on_account.as_deref() == Some(acct_name.as_str())
                    || (op.on_account.is_none() && acct_name == &spec.account_types[0].name)
            })
            .collect();

        // Convert &[&ParsedProperty] to &[ParsedProperty] by cloning
        let owned_props: Vec<crate::check::ParsedProperty> = props
            .iter()
            .map(|p| crate::check::ParsedProperty {
                name: p.name.clone(),
                expression: p.expression.clone(),
                preserved_by: p.preserved_by.clone(),
            })
            .collect();

        render_properties_inner(
            out,
            &owned_props,
            &acct_ops,
            &state_type,
            &op_type,
            &apply_name,
        );
    }
}

/// Inner helper for property rendering.
///
/// Emits per-operation sub-lemmas (with sorry) and a master theorem that
/// is auto-proven by case split over the Operation type.
fn render_properties_inner(
    out: &mut String,
    properties: &[crate::check::ParsedProperty],
    ops: &[&crate::check::ParsedHandler],
    state_type: &str,
    op_type: &str,
    apply_name: &str,
) {
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            out.push_str(&format!(
                "def {} (s : {}) : Prop := {}\n\n",
                prop.name, state_type, expr
            ));
        }

        // Determine which operations this property covers
        let covered_ops: Vec<&&crate::check::ParsedHandler> = ops
            .iter()
            .filter(|op| prop.preserved_by.contains(&op.name))
            .collect();

        // Emit per-operation sub-lemmas (one sorry per op)
        for op in &covered_ops {
            let trans_name = safe_name(&format!("{}Transition", op.name));
            let param_sig = param_sig_str(&op.takes_params);

            let sub_lemma_name = safe_name(&format!(
                "{}_preserved_by_{}",
                prop.name, op.name
            ));
            out.push_str(&format!(
                "theorem {} (s s' : {}) (signer : Pubkey){}\n",
                sub_lemma_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    (h_inv : {} s) (h : {} s signer{} = some s') :\n",
                prop.name,
                trans_name,
                param_args_str(&op.takes_params)
            ));
            out.push_str(&format!("    {} s' := sorry\n\n", prop.name));
        }

        // Emit master theorem auto-proven by case split
        out.push_str(&format!(
            "/-- {} is preserved by every operation. Auto-proven by case split. -/\n",
            prop.name
        ));
        out.push_str(&format!(
            "theorem {}_inductive (s s' : {}) (signer : Pubkey) (op : {})\n    (h_inv : {} s) (h : {} s signer op = some s') : {} s' := by\n",
            prop.name, state_type, op_type, prop.name, apply_name, prop.name
        ));
        out.push_str("  cases op with\n");
        for op in ops {
            let ctor = safe_name(&op.name);
            let param_names: Vec<String> = op.takes_params.iter().map(|(n, _)| n.clone()).collect();
            let param_bind = if param_names.is_empty() {
                String::new()
            } else {
                format!(" {}", param_names.join(" "))
            };
            let param_pass = if param_names.is_empty() {
                String::new()
            } else {
                format!(" {}", param_names.join(" "))
            };

            if prop.preserved_by.contains(&op.name) {
                let ref_name = safe_name(&format!(
                    "{}_preserved_by_{}",
                    prop.name, op.name
                ));
                out.push_str(&format!(
                    "  | {}{} => exact {} s s' signer{} h_inv h\n",
                    ctor, param_bind, ref_name, param_pass
                ));
            } else {
                // Operation not in preserved_by — attempt inline proof if trivial.
                // Collect field names referenced in the property expression.
                let prop_fields: Vec<&str> = if let Some(ref expr) = prop.expression {
                    fields_referenced_in_expr(expr)
                } else {
                    Vec::new()
                };
                // Check if the operation touches any field the property references.
                let touches_prop_field = op.effects.iter().any(|(f, _, _)| {
                    prop_fields.iter().any(|pf| *pf == f.as_str())
                });
                let trans_name = safe_name(&format!("{}Transition", op.name));
                if !touches_prop_field {
                    // Operation doesn't modify any field in the property → trivially preserved.
                    out.push_str(&format!(
                        "  | {}{} =>\n    simp [applyOp, {}] at h\n    obtain \u{27E8}_, h_eq\u{27E9} := h\n    subst h_eq; exact h_inv\n",
                        ctor, param_bind, trans_name
                    ));
                } else {
                    // Operation modifies property fields but isn't in preserved_by —
                    // can't safely auto-prove without understanding the inequality direction.
                    out.push_str(&format!(
                        "  | {}{} => sorry -- {} not in preserved_by; prove manually if needed\n",
                        ctor,
                        param_bind,
                        op.name
                    ));
                }
            }
        }
        out.push('\n');
    }
}

/// Build " param1 param2" string for calling a transition function.
fn param_args_str(params: &[(String, String)]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!(
            " {}",
            params
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

/// Render cover properties — existential reachability proofs.
fn render_covers(out: &mut String, spec: &ParsedSpec, state_type: &str) {
    if spec.covers.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Cover properties — reachability (existential proofs)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // Helper: resolve the state type for a handler
    let resolve_state_type = |op_name: &str| -> String {
        let op = spec.handlers.iter().find(|o| o.name == op_name);
        if let Some(op) = op {
            if let Some(ref acct) = op.on_account {
                return format!("{}State", acct);
            }
        }
        state_type.to_string()
    };

    for cover in &spec.covers {
        for (i, trace) in cover.traces.iter().enumerate() {
            let suffix = if cover.traces.len() > 1 {
                format!("_{}", i)
            } else {
                String::new()
            };

            // For multi-account specs, check if all ops share the same state type
            let trace_state_types: Vec<String> =
                trace.iter().map(|op| resolve_state_type(op)).collect();
            let all_same = trace_state_types.windows(2).all(|w| w[0] == w[1]);
            let effective_type = if all_same && !trace_state_types.is_empty() {
                trace_state_types[0].clone()
            } else {
                // Cross-account trace — skip with a comment
                out.push_str(&format!(
                    "-- cover_{}{}: trace [{}] spans multiple account types, skipped\n\n",
                    cover.name,
                    suffix,
                    trace.join(", ")
                ));
                continue;
            };

            // Generate existential proof: there exists initial state and signer such that
            // the trace sequence produces a valid final state
            out.push_str(&format!(
                "/-- {} — trace [{}] is reachable. -/\n",
                cover.name,
                trace.join(", ")
            ));
            out.push_str(&format!(
                "theorem cover_{}{} : ∃ (s0 : {}) (signer : Pubkey),\n",
                cover.name, suffix, effective_type
            ));
            // Build nested match chain
            let mut indent = "    ".to_string();
            for (j, op_name) in trace.iter().enumerate() {
                let trans = safe_name(&format!("{}Transition", op_name));
                let handler = spec.handlers.iter().find(|o| o.name == *op_name);
                let param_args = handler
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .enumerate()
                            .map(|(k, (_, _))| format!("v{}_{}", j, k))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let extra_exists = handler
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .enumerate()
                            .map(|(k, (_, t))| format!("(v{}_{} : {})", j, k, map_type(t)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                if !extra_exists.is_empty() {
                    out.push_str(&format!("{}∃ {}, ", indent, extra_exists));
                }

                let s_var = if j == 0 {
                    "s0".to_string()
                } else {
                    format!("s{}", j)
                };
                let s_next = format!("s{}", j + 1);

                if j < trace.len() - 1 {
                    let param_str = if param_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", param_args)
                    };
                    out.push_str(&format!(
                        "∃ ({} : {}), {} {} signer{} = some {} ∧\n",
                        s_next, effective_type, trans, s_var, param_str, s_next
                    ));
                    indent.push_str("  ");
                } else {
                    let param_str = if param_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", param_args)
                    };
                    out.push_str(&format!(
                        "{} {} signer{} ≠ none := sorry\n\n",
                        trans, s_var, param_str
                    ));
                }
            }
        }

        for (op_name, when_expr) in &cover.reachable {
            out.push_str(&format!("/-- {} — {} is reachable", cover.name, op_name));
            if let Some(ref expr) = when_expr {
                out.push_str(&format!(" when {}. -/\n", expr));
            } else {
                out.push_str(". -/\n");
            }
            out.push_str(&format!(
                "theorem cover_{}_{} : ∃ (s : {}) (signer : Pubkey),\n",
                cover.name,
                safe_name(op_name),
                state_type
            ));
            if let Some(ref expr) = when_expr {
                out.push_str(&format!("    {} ∧ ", expr));
            } else {
                out.push_str("    ");
            }
            let trans = safe_name(&format!("{}Transition", op_name));
            let handler = spec.handlers.iter().find(|o| o.name == *op_name);
            let param_exists = handler
                .map(|o| {
                    o.takes_params
                        .iter()
                        .map(|(n, t)| format!("({} : {})", n, map_type(t)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let param_args = handler
                .map(|o| param_args_str(&o.takes_params))
                .unwrap_or_default();
            if !param_exists.is_empty() {
                out.push_str(&format!("∃ {}, ", param_exists));
            }
            out.push_str(&format!(
                "{} s signer{} ≠ none := sorry\n\n",
                trans, param_args
            ));
        }
    }
}

/// Render liveness properties — bounded reachability from one state to another.
fn render_liveness(out: &mut String, spec: &ParsedSpec, state_type: &str) {
    if spec.liveness_props.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Liveness properties — bounded reachability (leads-to)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // Helper: resolve state type for a liveness block from its via operations
    let resolve_liveness_state = |via_ops: &[String]| -> String {
        if !spec.account_types.is_empty() && !via_ops.is_empty() {
            // Check the first via op's on_account
            if let Some(op) = spec.handlers.iter().find(|o| o.name == via_ops[0]) {
                if let Some(ref acct) = op.on_account {
                    return format!("{}State", acct);
                }
            }
        }
        state_type.to_string()
    };

    // Track which applyOps helpers we've already emitted
    let mut emitted_helpers: Vec<String> = Vec::new();

    for liveness in &spec.liveness_props {
        let effective_type = resolve_liveness_state(&liveness.via_ops);
        let bound = liveness.within_steps.unwrap_or(10);

        // Derive operation type and applyOp dispatcher
        let (op_type, apply_fn, prefix) = if effective_type == "State" {
            ("Operation".to_string(), "applyOp".to_string(), String::new())
        } else if effective_type.ends_with("State") {
            let p = effective_type[..effective_type.len() - 5].to_string();
            (format!("{}Operation", p), format!("apply{}Op", p), p)
        } else {
            ("Operation".to_string(), "applyOp".to_string(), String::new())
        };

        let apply_ops_fn = format!("apply{}Ops", prefix);

        // Emit applyOps helper if not already emitted for this type
        if !emitted_helpers.contains(&effective_type) {
            out.push_str(&format!(
                "def {} (s : {}) (signer : Pubkey) : List {} → Option {}\n",
                apply_ops_fn, effective_type, op_type, effective_type
            ));
            out.push_str("  | [] => some s\n");
            out.push_str(&format!(
                "  | op :: ops => match {} s signer op with\n",
                apply_fn
            ));
            out.push_str(&format!(
                "    | some s' => {} s' signer ops\n",
                apply_ops_fn
            ));
            out.push_str("    | none => none\n\n");
            emitted_helpers.push(effective_type.clone());
        }

        out.push_str(&format!(
            "/-- {} — from {} leads to {} within {} steps via [{}]. -/\n",
            liveness.name,
            liveness.from_state,
            liveness.leads_to_state,
            bound,
            liveness.via_ops.join(", ")
        ));
        out.push_str(&format!(
            "theorem liveness_{} (s : {}) (signer : Pubkey)\n",
            liveness.name, effective_type
        ));
        out.push_str(&format!(
            "    (h : s.status = .{}) :\n",
            liveness.from_state
        ));
        out.push_str(&format!(
            "    ∃ ops, ops.length ≤ {} ∧ ∀ s', {} s signer ops = some s' → s'.status = .{} := sorry\n\n",
            bound, apply_ops_fn, liveness.leads_to_state
        ));
    }
}

/// Render environment block theorems — properties hold under external state changes.
fn render_environments(out: &mut String, spec: &ParsedSpec, state_type: &str) {
    if spec.environments.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Environment — properties hold under external state changes\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for env in &spec.environments {
        // For each property, generate a theorem showing it holds after env mutation
        for prop in &spec.properties {
            if prop.expression.is_none() {
                continue;
            }

            // Build parameter signature for mutated fields
            let param_sig: String = env
                .mutates
                .iter()
                .map(|(name, typ)| format!(" (new_{} : {})", name, map_type(typ)))
                .collect();

            // Build constraint hypotheses
            let constraint_hyps: String = env
                .constraints
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    // Replace field refs with new_ prefixed versions
                    let mut expr = c.clone();
                    for (field, _) in &env.mutates {
                        expr = expr
                            .replace(&format!("s.{}", field), &format!("new_{}", field))
                            .replace(&format!("state.{}", field), &format!("new_{}", field));
                        // Bare field name in constraint
                        if expr.trim() == *field || expr.contains(field) {
                            expr = expr.replace(field, &format!("new_{}", field));
                        }
                    }
                    format!("\n    (h_c{} : {})", i, expr)
                })
                .collect();

            // Build with-update
            let with_parts: String = env
                .mutates
                .iter()
                .map(|(name, _)| format!("{} := new_{}", safe_name(name), name))
                .collect::<Vec<_>>()
                .join(", ");

            out.push_str(&format!(
                "theorem {}_under_{} (s : {}){}{}\n",
                prop.name, env.name, state_type, param_sig, constraint_hyps
            ));
            out.push_str(&format!("    (h_inv : {} s) :\n", prop.name));
            out.push_str(&format!(
                "    {} {{ s with {} }} := sorry\n\n",
                prop.name, with_parts
            ));
        }
    }
}

/// Render aborts_if theorems — prove that operations reject under specified conditions.
/// Also generates abort theorems from `requires ... else Error` clauses (negated form).
fn render_aborts_if(out: &mut String, ops: &[&crate::check::ParsedHandler], state_type: &str) {
    let has_aborts = ops.iter().any(|op| {
        !op.aborts_if.is_empty() || op.requires.iter().any(|r| r.error_name.is_some())
    });
    if !has_aborts {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Abort conditions — operations must reject under specified conditions\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for op in ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);
        let param_args = param_args_str(&op.takes_params);

        // Collect all abort conditions (negated form)
        let mut all_abort_conditions: Vec<String> = Vec::new();

        // Traditional aborts_if clauses — the expression IS the abort condition
        for abort in &op.aborts_if {
            all_abort_conditions.push(abort.lean_expr.clone());
        }

        // Requires clauses with else Error — negated positive condition
        for req in &op.requires {
            if req.error_name.is_some() {
                all_abort_conditions
                    .push(format!("\u{00AC}({})", req.lean_expr)); // ¬(...)
            }
        }

        if op.aborts_total && !all_abort_conditions.is_empty() {
            // Aborts total: single IFF theorem with disjunction of all conditions
            let theorem_name = safe_name(&format!("{}_aborts_iff", op.name));
            out.push_str(&format!(
                "theorem {} (s : {}) (signer : Pubkey){} :\n",
                theorem_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    {} s signer{} = none \u{2194}\n",
                trans_name, param_args
            ));
            let disjunction = all_abort_conditions.join(" \u{2228} "); // ∨
            out.push_str(&format!("    ({}) := sorry\n\n", disjunction));
        } else {
            // Per-condition abort theorems (original behavior)
            for abort in &op.aborts_if {
                let theorem_name = safe_name(&format!(
                    "{}_aborts_if_{}",
                    op.name, abort.error_name
                ));
                out.push_str(&format!(
                    "theorem {} (s : {}) (signer : Pubkey){}\n",
                    theorem_name, state_type, param_sig
                ));
                out.push_str(&format!(
                    "    (h : {}) : {} s signer{} = none := sorry\n\n",
                    abort.lean_expr, trans_name, param_args
                ));
            }

            for req in &op.requires {
                if let Some(ref error_name) = req.error_name {
                    let theorem_name = safe_name(&format!(
                        "{}_aborts_if_{}",
                        op.name, error_name
                    ));
                    out.push_str(&format!(
                        "theorem {} (s : {}) (signer : Pubkey){}\n",
                        theorem_name, state_type, param_sig
                    ));
                    out.push_str(&format!(
                        "    (h : \u{00AC}({})) : {} s signer{} = none := sorry\n\n",
                        req.lean_expr, trans_name, param_args
                    ));
                }
            }
        }
    }
}

/// Render post-condition theorems from `ensures` clauses.
///
/// Each ensures clause generates a theorem of the form:
/// ```lean
/// theorem handler_ensures_N (s s' : State) (signer : Pubkey) ...
///     (h : handlerTransition s signer ... = some s') :
///     <ensures_expr> := sorry
/// ```
/// In the ensures expression, `state.field` is rendered as `s'.field` (post-state)
/// and `old(state.field)` as `s.field` (pre-state).
fn render_ensures(out: &mut String, ops: &[&crate::check::ParsedHandler], state_type: &str) {
    let has_ensures = ops.iter().any(|op| !op.ensures.is_empty());
    if !has_ensures {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Post-conditions (ensures)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for op in ops {
        for (i, ens) in op.ensures.iter().enumerate() {
            let trans_name = safe_name(&format!("{}Transition", op.name));
            let param_sig = param_sig_str(&op.takes_params);

            let theorem_name = safe_name(&format!("{}_ensures_{}", op.name, i));
            out.push_str(&format!(
                "theorem {} (s s' : {}) (signer : Pubkey){}\n",
                theorem_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    (h : {} s signer{} = some s') :\n",
                trans_name,
                param_args_str(&op.takes_params)
            ));
            out.push_str(&format!("    {} := sorry\n\n", ens.lean_expr));
        }
    }
}

/// Render frame condition theorems from `modifies` clauses.
///
/// For each handler with a `modifies` clause, generates a theorem proving that
/// all fields NOT in the modifies list remain unchanged after the transition.
/// If the handler also transitions lifecycle (pre/post status), `status` is
/// implicitly considered modified.
fn render_frame_conditions(
    out: &mut String,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
) {
    let has_modifies = ops.iter().any(|op| op.modifies.is_some());
    if !has_modifies {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Frame conditions (modifies)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for op in ops {
        if let Some(ref modified_fields) = op.modifies {
            let trans_name = safe_name(&format!("{}Transition", op.name));
            let param_sig = param_sig_str(&op.takes_params);

            // Compute unchanged fields: all fields minus modified ones.
            // If handler transitions lifecycle, status is implicitly modified.
            let status_is_modified =
                op.pre_status.is_some() && op.post_status.is_some();
            let unchanged: Vec<&str> = fields
                .iter()
                .filter(|(name, _)| {
                    !modified_fields.contains(name)
                        && !(name == "status" && status_is_modified)
                })
                .map(|(name, _)| name.as_str())
                .collect();

            if unchanged.is_empty() {
                continue;
            }

            let theorem_name = safe_name(&format!("{}_frame", op.name));
            out.push_str(&format!(
                "theorem {} (s s' : {}) (signer : Pubkey){}\n",
                theorem_name, state_type, param_sig
            ));
            out.push_str(&format!(
                "    (h : {} s signer{} = some s') :\n",
                trans_name,
                param_args_str(&op.takes_params)
            ));

            let frame_conjuncts: Vec<String> = unchanged
                .iter()
                .map(|f| format!("s'.{} = s.{}", safe_name(f), safe_name(f)))
                .collect();
            out.push_str(&format!(
                "    {} := sorry\n\n",
                frame_conjuncts.join(" \u{2227} ") // ∧
            ));
        }
    }
}

/// Render overflow safety obligations for operations with add effects.
///
/// For each operation that has "add" effects on numeric fields, generates a
/// theorem requiring that all numeric fields in the post-state remain valid
/// (within their declared type's bounds).
fn render_overflow_obligations(
    out: &mut String,
    spec: &ParsedSpec,
    ops: &[&crate::check::ParsedHandler],
    fields: &[(String, String)],
    state_type: &str,
) {
    // Collect handlers that have add effects
    let add_ops: Vec<&&crate::check::ParsedHandler> = ops
        .iter()
        .filter(|op| op.effects.iter().any(|(_, kind, _)| kind == "add"))
        .collect();

    if add_ops.is_empty() {
        return;
    }

    // Collect numeric field names for the validity predicate
    let numeric_fields: Vec<&str> = fields
        .iter()
        .filter(|(_, t)| {
            matches!(
                t.as_str(),
                "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
            )
        })
        .map(|(n, _)| n.as_str())
        .collect();

    if numeric_fields.is_empty() {
        return;
    }

    // Determine the appropriate bounds predicate based on field types
    // Use the widest type present to determine the bound
    let valid_fn = |ftype: &str| -> &str {
        match ftype {
            "U8" => "valid_u8",
            "U16" => "valid_u16",
            "U32" => "valid_u32",
            "U64" => "valid_u64",
            "U128" => "valid_u128",
            "I64" => "valid_i64",
            "I128" => "valid_i128",
            _ => "valid_u64",
        }
    };

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str(
        "-- Overflow safety obligations (auto-generated for operations with add effects)\n",
    );
    out.push_str(
        "-- ============================================================================\n\n",
    );

    for op in &add_ops {
        let trans_name = safe_name(&format!("{}Transition", op.name));
        let param_sig = param_sig_str(&op.takes_params);

        // Build pre-condition: all numeric fields are valid
        let pre_parts: Vec<String> = fields
            .iter()
            .filter(|(_, t)| {
                matches!(
                    t.as_str(),
                    "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
                )
            })
            .map(|(n, t)| format!("{} s.{}", valid_fn(t), safe_name(n)))
            .collect();

        // Build post-condition: all numeric fields remain valid
        let post_parts: Vec<String> = fields
            .iter()
            .filter(|(_, t)| {
                matches!(
                    t.as_str(),
                    "U8" | "U16" | "U32" | "U64" | "U128" | "I64" | "I128"
                )
            })
            .map(|(n, t)| format!("{} s'.{}", valid_fn(t), safe_name(n)))
            .collect();

        // Collect invariant hypotheses: all properties that cover this operation
        let inv_hyps: Vec<String> = spec
            .properties
            .iter()
            .filter(|p| p.preserved_by.contains(&op.name) && p.expression.is_some())
            .map(|p| p.name.clone())
            .collect();

        out.push_str(&format!(
            "theorem {}_overflow_safe (s s' : {}) (signer : Pubkey){}\n",
            safe_name(&op.name),
            state_type,
            param_sig
        ));
        out.push_str(&format!("    (h_valid : {})\n", pre_parts.join(" ∧ ")));
        for inv in &inv_hyps {
            out.push_str(&format!("    (h_inv_{} : {} s)\n", safe_name(inv), inv));
        }
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name,
            param_args_str(&op.takes_params)
        ));
        out.push_str(&format!("    {} := sorry\n\n", post_parts.join(" ∧ ")));
    }
}

// ============================================================================
// sBPF rendering — generates qedguards-compatible Lean from sBPF .qedspec
// ============================================================================

/// Render an sBPF spec into Lean 4 source.
///
/// Produces: namespace, error constants, offset constants, ea_* lemmas,
/// guard theorem stubs (with hypotheses derived from checks + layout),
/// and a Spec completeness structure.
fn render_sbpf(spec: &ParsedSpec) -> String {
    let mut out = String::new();

    // Derive Prog module name from spec program_name.
    // E.g., spec Slippage → "SlippageProg", spec Transfer → "TransferProg"
    let prog_module = format!("{}Prog", spec.program_name);

    // Header
    out.push_str(&format!(
        "-- Generated by qedgen lean-gen from {}.qedspec\n\
         -- Source of truth: the .qedspec file. Regenerate with:\n\
         --   qedgen lean-gen --spec <spec>.qedspec --output <this-file>\n\n",
        spec.program_name.to_lowercase()
    ));

    out.push_str("import QEDGen\n");
    out.push_str(&format!("import {}\n\n", prog_module));

    out.push_str("open QEDGen.Solana.SBPF\n");
    out.push_str("open QEDGen.Solana.SBPF.Memory\n\n");

    // ── Global constants ─────────────────────────────────────────────────
    if !spec.constants.is_empty() {
        out.push_str("-- Global constants (from prog module, not re-declared):\n");
        for (name, val) in &spec.constants {
            let clean_val = val.replace('_', "");
            out.push_str(&format!("--   {} = {}\n", name, clean_val));
        }
        out.push('\n');
    }

    // ── Pubkey constants ───────────────────────────────────────────────────
    if !spec.pubkeys.is_empty() {
        out.push_str("-- Known pubkey constants (from prog module, not re-declared):\n");
        for pk in &spec.pubkeys {
            for (i, chunk) in pk.chunks.iter().enumerate() {
                let clean = chunk.replace('_', "");
                out.push_str(&format!(
                    "--   PUBKEY_{}_CHUNK_{} = {}\n",
                    pk.name.to_ascii_uppercase(),
                    i,
                    clean
                ));
            }
        }
        out.push('\n');
    }

    // ── Per-instruction blocks ───────────────────────────────────────────
    for instr in &spec.instructions {
        let ns = &instr.name;
        out.push_str(&format!("namespace {}\n\n", ns));

        // Instruction-level constants
        if !instr.constants.is_empty() {
            out.push_str("-- Instruction-level constants\n");
            for (name, val) in &instr.constants {
                let clean_val = val.replace('_', "");
                out.push_str(&format!("abbrev {} : Nat := {}\n", name, clean_val));
            }
            out.push('\n');
        }

        // Error constants — use instruction-level if present, else global
        let errors = if !instr.errors.is_empty() {
            &instr.errors
        } else {
            &spec.valued_errors
        };
        if !errors.is_empty() {
            out.push_str("-- Error constants\n");
            for err in errors {
                if let Some(val) = err.value {
                    let lean_name = error_to_lean_name(&err.name);
                    out.push_str(&format!("abbrev {} : Nat := {}\n", lean_name, val));
                }
            }
            out.push('\n');
        }

        // Offset constants (from input_layout + insn_layout)
        let all_offsets: Vec<(&str, &str, i64, bool)> = instr
            .input_layout
            .iter()
            .map(|f| (f.name.as_str(), f.field_type.as_str(), f.offset, false))
            .chain(
                instr
                    .insn_layout
                    .iter()
                    .map(|f| (f.name.as_str(), f.field_type.as_str(), f.offset, true)),
            )
            .collect();

        if !all_offsets.is_empty() {
            out.push_str("-- Offset constants\n");
            for (name, _ftype, offset, _is_insn) in &all_offsets {
                let lean_name = offset_to_lean_name(name);
                out.push_str(&format!("abbrev {} : Int := {}\n", lean_name, offset));
            }
            out.push('\n');

            // ea_* lemmas
            out.push_str("-- Effective address lemmas\n");
            for (name, _ftype, offset, _is_insn) in &all_offsets {
                let lean_name = offset_to_lean_name(name);
                let rhs = if *offset == 0 {
                    "b".to_string()
                } else if *offset > 0 {
                    format!("b + {}", offset)
                } else {
                    format!("b - {}", offset.unsigned_abs())
                };
                out.push_str(&format!(
                    "@[simp] theorem ea_{} (b : Nat) : effectiveAddr b {} = {} := by\n  \
                     unfold effectiveAddr {}; omega\n\n",
                    lean_name, lean_name, rhs, lean_name
                ));
            }
        }

        // Entry point
        let entry = instr.entry.unwrap_or(0);
        let has_insn_reg = !instr.insn_layout.is_empty();
        let init_expr = if has_insn_reg {
            format!("initState2 inputAddr insnAddr mem {}", entry)
        } else {
            "initState inputAddr mem".to_string()
        };

        // Guard theorem stubs
        if !instr.guards.is_empty() {
            out.push_str("-- Guard theorem stubs\n");
            out.push_str(
                "-- Hypotheses derived from checks + layout. Fill proofs with wp_exec.\n\n",
            );

            let mut accumulated_after: Vec<(String, String)> = Vec::new();

            for (i, guard) in instr.guards.iter().enumerate() {
                let error_lean = error_to_lean_name(&guard.error);
                let hyps = derive_guard_hypotheses(guard, &all_offsets, instr, spec);

                if let Some(ref doc) = guard.doc {
                    out.push_str(&format!("/-- {} -/\n", doc.trim()));
                }

                out.push_str(&format!("theorem {}\n", guard.name));

                if has_insn_reg {
                    out.push_str("    (inputAddr insnAddr : Nat) (mem : Mem)\n");
                } else {
                    out.push_str("    (inputAddr : Nat) (mem : Mem)\n");
                }

                for (var_decl, _) in &accumulated_after {
                    out.push_str(&format!("    {}\n", var_decl));
                }

                for hyp in &hyps.bindings {
                    out.push_str(&format!("    {}\n", hyp));
                }

                let fuel_str = match guard.fuel {
                    Some(f) => f.to_string(),
                    None => "FUEL".to_string(),
                };
                out.push_str(&format!(
                    "    :\n    (executeFn {}.progAt ({}) {}).exitCode\n      \
                     = some {} := sorry\n\n",
                    prog_module, init_expr, fuel_str, error_lean
                ));

                if let Some(ref after_hyps) = hyps.after {
                    for ah in after_hyps {
                        accumulated_after.push((ah.clone(), String::new()));
                    }
                }

                let _ = i;
            }

            // Spec completeness structure
            out.push_str(
                "-- Completeness structure: fill all fields to prove every guard is covered\n",
            );
            out.push_str("structure Spec (progAt : Nat \u{2192} Option Insn) where\n");

            let mut acc_after_for_spec: Vec<String> = Vec::new();
            for guard in &instr.guards {
                let error_lean = error_to_lean_name(&guard.error);
                let hyps = derive_guard_hypotheses(guard, &all_offsets, instr, spec);

                let mut binders = Vec::new();
                if has_insn_reg {
                    binders.push("(inputAddr insnAddr : Nat)".to_string());
                    binders.push("(mem : Mem)".to_string());
                } else {
                    binders.push("(inputAddr : Nat)".to_string());
                    binders.push("(mem : Mem)".to_string());
                }
                for ah in &acc_after_for_spec {
                    binders.push(prefix_unused_binder(ah));
                }
                for b in &hyps.bindings {
                    if !b.starts_with("--") {
                        binders.push(prefix_unused_binder(b));
                    }
                }

                let binder_str = binders.join(" ");
                let fuel_str = match guard.fuel {
                    Some(f) => f.to_string(),
                    None => "FUEL".to_string(),
                };
                out.push_str(&format!(
                    "  {} :\n    \u{2200} {},\n    \
                     (executeFn progAt ({}) {}).exitCode = some {}\n",
                    guard.name, binder_str, init_expr, fuel_str, error_lean
                ));

                if let Some(ref after_hyps) = hyps.after {
                    for ah in after_hyps {
                        acc_after_for_spec.push(ah.clone());
                    }
                }
            }
            out.push('\n');
        }

        // Property theorem stubs
        if !instr.properties.is_empty() {
            out.push_str("-- Property theorem stubs\n\n");
            for prop in &instr.properties {
                if let Some(ref doc) = prop.doc {
                    out.push_str(&format!("/-- {} -/\n", doc.trim()));
                }
                out.push_str(&format!("theorem {} : True := trivial\n\n", prop.name));
            }
        }

        out.push_str(&format!("end {}\n\n", ns));
    }

    out
}

/// Hypotheses derived from a guard's checks expression and the layout.
struct DerivedHypotheses {
    /// Lean hypothesis binders (e.g., "(disc : Nat)", "(h_disc_val : readU8 mem insnAddr = disc)")
    bindings: Vec<String>,
    /// After-hypotheses for the next guard (what becomes true if this guard passes)
    after: Option<Vec<String>>,
}

/// Derive guard hypotheses from checks expression + input/insn layout.
fn derive_guard_hypotheses(
    guard: &crate::check::ParsedGuard,
    all_offsets: &[(&str, &str, i64, bool)],
    _instr: &crate::check::ParsedInstruction,
    _spec: &ParsedSpec,
) -> DerivedHypotheses {
    // Use raw checks (preserves constant names) for Lean output
    let checks_str = guard.checks_raw.as_ref().or(guard.checks.as_ref());
    let Some(checks) = checks_str else {
        // No checks expression — generate minimal placeholder
        return DerivedHypotheses {
            bindings: vec!["-- TODO: add guard-specific hypotheses".to_string()],
            after: None,
        };
    };

    // Parse checks expression: "field == CONST" or "field >= CONST"
    // Support patterns: X == Y, X >= Y, X == Y (pubkey 4-chunk comparison)
    let parts: Vec<&str> = checks.split_whitespace().collect();

    if parts.len() == 3 {
        let field_name = parts[0];
        let op = parts[1];
        let const_name = parts[2];

        // Look up the field in layouts
        if let Some((_, ftype, offset, is_insn)) = all_offsets
            .iter()
            .find(|(name, _, _, _)| *name == field_name)
        {
            let read_fn = match *ftype {
                "U8" => "readU8",
                "U64" => "readU64",
                "Pubkey" => "readU64", // Pubkey fields are 4-chunk comparisons
                _ => "readU64",
            };

            let base_reg = if *is_insn { "insnAddr" } else { "inputAddr" };
            let addr_expr = if *offset == 0 {
                base_reg.to_string()
            } else if *offset > 0 {
                format!("({} + {})", base_reg, offset)
            } else {
                format!("({} - {})", base_reg, offset.unsigned_abs())
            };

            // Variable name: derive from field name
            let var_name = field_name_to_var(field_name);

            // Check if const_name is also a layout field (field-vs-field comparison)
            let rhs_is_field = all_offsets
                .iter()
                .find(|(name, _, _, _)| *name == const_name);

            // Build RHS: if it's a field, introduce a variable and read hypothesis for it
            let (rhs_var, rhs_bindings) = if let Some((_, rtype, roffset, r_is_insn)) = rhs_is_field
            {
                let rhs_read = match *rtype {
                    "U8" => "readU8",
                    _ => "readU64",
                };
                let rhs_base = if *r_is_insn { "insnAddr" } else { "inputAddr" };
                let rhs_addr = if *roffset == 0 {
                    rhs_base.to_string()
                } else if *roffset > 0 {
                    format!("({} + {})", rhs_base, roffset)
                } else {
                    format!("({} - {})", rhs_base, roffset.unsigned_abs())
                };
                let rhs_vname = field_name_to_var(const_name);
                let binds = vec![
                    format!("({} : Nat)", rhs_vname),
                    format!(
                        "(h_{}_val : {} mem {} = {})",
                        rhs_vname, rhs_read, rhs_addr, rhs_vname
                    ),
                ];
                (rhs_vname, binds)
            } else {
                // RHS is a constant name (preserve as-is from checks_raw)
                (const_name.to_string(), vec![])
            };

            match op {
                "==" => {
                    let mut bindings = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                    ];
                    bindings.extend(rhs_bindings.clone());
                    bindings.push(format!(
                        "(h_{}_ne : {} \u{2260} {})",
                        var_name, var_name, rhs_var
                    ));
                    let after = Some(vec![format!(
                        "(h_{} : {} mem {} = {})",
                        var_name, read_fn, addr_expr, rhs_var
                    )]);
                    DerivedHypotheses { bindings, after }
                }
                ">=" => {
                    let mut bindings = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                    ];
                    bindings.extend(rhs_bindings.clone());
                    bindings.push(format!("(h_{}_lt : {} < {})", var_name, var_name, rhs_var));
                    let mut after_binds = vec![
                        format!("({} : Nat)", var_name),
                        format!(
                            "(h_{}_val : {} mem {} = {})",
                            var_name, read_fn, addr_expr, var_name
                        ),
                    ];
                    after_binds.extend(rhs_bindings);
                    after_binds.push(format!(
                        "(h_{}_ge : \u{00AC}({} < {}))",
                        var_name, var_name, rhs_var
                    ));
                    DerivedHypotheses {
                        bindings,
                        after: Some(after_binds),
                    }
                }
                _ => DerivedHypotheses {
                    bindings: vec![format!("-- TODO: derive hypotheses for checks: {}", checks)],
                    after: None,
                },
            }
        } else {
            // Field not found in layout — generate placeholder
            DerivedHypotheses {
                bindings: vec![format!("-- TODO: derive hypotheses for checks: {}", checks)],
                after: None,
            }
        }
    } else {
        // Complex expression — placeholder
        DerivedHypotheses {
            bindings: vec![format!("-- TODO: derive hypotheses for checks: {}", checks)],
            after: None,
        }
    }
}

/// Prefix hypothesis binder names (starting with `h_`) with `_` to suppress
/// unused-variable warnings in the Spec structure. Value variables like
/// `discriminant`, `nAccounts` etc. must keep their names because hypothesis
/// types reference them (e.g., `readU8 mem addr = discriminant`).
fn prefix_unused_binder(binder: &str) -> String {
    if let Some(rest) = binder.strip_prefix("(h_") {
        return format!("(_h_{}", rest);
    }
    binder.to_string()
}

/// Convert error name from qedspec to Lean constant name.
/// E.g., "InvalidDiscriminant" → "E_INVALID_DISCRIMINANT"
fn error_to_lean_name(name: &str) -> String {
    let mut result = String::from("E_");
    let mut prev_was_upper = false;
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 && !prev_was_upper {
            result.push('_');
        }
        result.push(c.to_ascii_uppercase());
        prev_was_upper = c.is_uppercase();
    }
    result
}

/// Convert layout field name to a Lean variable name.
fn field_name_to_var(name: &str) -> String {
    // Convert snake_case to camelCase for variable names
    let parts: Vec<&str> = name.split('_').collect();
    if parts.len() <= 1 {
        return name.to_string();
    }
    let mut result = parts[0].to_string();
    for part in &parts[1..] {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars);
        }
    }
    result
}

/// Convert offset field name to a Lean constant name.
/// Uses naming convention matching qedguards: uppercase with prefix.
fn offset_to_lean_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// Map DSL types to Lean types.
fn map_type(t: &str) -> &str {
    match t {
        "U64" | "U128" | "U8" => "Nat",
        "I128" => "Int",
        _ => t,
    }
}

/// Return the Lean numeric literal for the maximum value of a DSL type.
/// Returns None for non-numeric types (Pubkey, Bool, etc.)
fn type_max_const(t: &str) -> Option<&str> {
    match t {
        "U8" => Some("255"),
        "U16" => Some("65535"),
        "U32" => Some("4294967295"),
        "U64" => Some("18446744073709551615"),
        "U128" => Some("340282366920938463463374607431768211455"),
        _ => None,
    }
}

/// Quote Lean keywords as «name».
/// Extract field names referenced in a Lean property expression.
///
/// Looks for patterns like `s.field_name` and returns the field names.
fn fields_referenced_in_expr(expr: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    for (i, _) in expr.match_indices("s.") {
        let rest = &expr[i + 2..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end > 0 {
            let field = &rest[..end];
            if !fields.contains(&field) {
                fields.push(field);
            }
        }
    }
    fields
}

fn safe_name(name: &str) -> String {
    let keywords = [
        "open",
        "close",
        "initialize",
        "import",
        "namespace",
        "end",
        "where",
        "with",
        "do",
        "let",
        "if",
        "then",
        "else",
        "match",
        "return",
        "in",
        "for",
    ];
    if keywords.contains(&name) {
        format!("\u{00AB}{}\u{00BB}", name)
    } else {
        name.to_string()
    }
}

/// Build parameter signature string for transition functions.
fn param_sig_str(params: &[(String, String)]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = params
            .iter()
            .map(|(n, t)| format!(" ({} : {})", n, map_type(t)))
            .collect();
        parts.join("")
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    const MULTISIG_SPEC: &str = include_str!("../../../examples/rust/multisig/multisig.qedspec");

    #[test]
    fn lean_gen_has_namespace() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("namespace Multisig"));
        assert!(lean.contains("end Multisig"));
    }

    #[test]
    fn lean_gen_has_status_inductive() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive Status where"));
        assert!(lean.contains("| Uninitialized"));
        assert!(lean.contains("| Active"));
        assert!(lean.contains("| HasProposal"));
    }

    #[test]
    fn lean_gen_has_state_structure() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure State where"));
        assert!(lean.contains("creator : Pubkey"));
        assert!(lean.contains("threshold : Nat"));
        assert!(lean.contains("status : Status"));
    }

    #[test]
    fn lean_gen_has_transitions() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def create_vaultTransition"));
        assert!(lean.contains("signer = s.creator"));
        assert!(lean.contains("s.status = .Uninitialized"));
        assert!(lean.contains("status := .Active"));
    }

    #[test]
    fn lean_gen_has_operation_inductive() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive Operation where"));
        assert!(lean.contains("| create_vault (threshold : Nat) (member_count : Nat)"));
        assert!(lean.contains("| propose"));
        assert!(lean.contains("| approve (member_index : Nat)"));
    }

    #[test]
    fn lean_gen_has_apply_op() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def applyOp (s : State) (signer : Pubkey)"));
        assert!(lean.contains("| .create_vault threshold member_count => create_vaultTransition s signer threshold member_count"));
        assert!(lean.contains("| .propose => proposeTransition s signer"));
    }

    #[test]
    fn lean_gen_has_properties() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("def threshold_bounded (s : State) : Prop :="));
        assert!(lean.contains("theorem threshold_bounded_inductive"));
        assert!(lean.contains("theorem approvals_bounded_inductive"));
        assert!(lean.contains(":= sorry"));
    }

    #[test]
    fn lean_gen_sub_auto_guard() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // remove_member has effect: member_count -= 1
        // Should auto-generate underflow guard: 1 ≤ s.member_count
        assert!(lean.contains("1 \u{2264} s.member_count"));
    }

    // ========================================================================
    // Multi-account (Lending) tests
    // ========================================================================

    const LENDING_SPEC: &str = include_str!("../../../examples/rust/lending/lending.qedspec");

    #[test]
    fn lean_gen_multi_per_account_status() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive PoolStatus where"));
        assert!(lean.contains("| Uninitialized"));
        assert!(lean.contains("| Paused"));
        assert!(lean.contains("inductive LoanStatus where"));
        assert!(lean.contains("| Empty"));
        assert!(lean.contains("| Liquidated"));
    }

    #[test]
    fn lean_gen_multi_per_account_state() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure PoolState where"));
        assert!(lean.contains("  authority : Pubkey"));
        assert!(lean.contains("  total_deposits : Nat"));
        assert!(lean.contains("  status : PoolStatus"));
        assert!(lean.contains("structure LoanState where"));
        assert!(lean.contains("  borrower : Pubkey"));
        assert!(lean.contains("  status : LoanStatus"));
    }

    #[test]
    fn lean_gen_multi_transitions_use_correct_state() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // Pool operations use PoolState
        assert!(lean.contains("def init_poolTransition (s : PoolState)"));
        assert!(lean.contains("def depositTransition (s : PoolState)"));
        // Loan operations use LoanState
        assert!(lean.contains("def borrowTransition (s : LoanState)"));
        assert!(lean.contains("def repayTransition (s : LoanState)"));
        assert!(lean.contains("def liquidateTransition (s : LoanState)"));
    }

    #[test]
    fn lean_gen_multi_per_account_operation_inductive() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("inductive PoolOperation where"));
        assert!(lean.contains("inductive LoanOperation where"));
        assert!(lean.contains("def applyPoolOp (s : PoolState)"));
        assert!(lean.contains("def applyLoanOp (s : LoanState)"));
    }

    #[test]
    fn lean_gen_multi_property_binds_to_correct_account() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // pool_solvency references total_deposits/total_borrows -> binds to PoolState
        assert!(lean.contains("def pool_solvency (s : PoolState)"));
        assert!(lean.contains("theorem pool_solvency_inductive (s s' : PoolState)"));
    }

    // ========================================================================
    // sBPF (Dropset) tests — inline old-syntax spec for backward compat
    // ========================================================================

    const DROPSET_SPEC: &str = r#"
spec Dropset

target assembly
assembly "src/dropset.s"

const DISC_REGISTER_MARKET     = 0
const ACCT_NON_DUP_MARKER      = 255
const DATA_LEN_ZERO             = 0
const SIZE_OF_EMPTY_ACCOUNT     = 10_336
const SIZE_OF_MARKET_HEADER     = 40
const SIZE_OF_ADDRESS           = 32
const SIZE_OF_CREATE_ACCOUNT    = 56

pubkey RENT [
  5_862_609_301_215_225_606,
  9_219_231_539_345_853_473,
  4_971_307_250_928_769_624,
  2_329_533_411
]

errors [
  InvalidDiscriminant         = 1   "Discriminant is not REGISTER_MARKET",
  InvalidInstructionLength    = 2   "Instruction data is not 1 byte",
  InvalidNumberOfAccounts     = 3   "Fewer than 10 accounts provided",
  UserHasData                 = 4   "User account already has data",
  MarketAccountIsDuplicate    = 5   "Market account is a duplicate",
  MarketHasData               = 6   "Market account already has data",
  BaseMintIsDuplicate         = 7   "Base mint account is a duplicate",
  QuoteMintIsDuplicate        = 8   "Quote mint account is a duplicate",
  InvalidMarketPubkey         = 9   "Market pubkey does not match derived PDA",
  SystemProgramIsDuplicate    = 10  "System Program account is a duplicate",
  InvalidSystemProgramPubkey  = 11  "System Program pubkey is wrong",
  RentSysvarIsDuplicate       = 12  "Rent sysvar account is a duplicate",
  InvalidRentSysvarPubkey     = 13  "Rent sysvar pubkey is wrong"
]

/// Validates accounts, derives market PDA, creates market account via CPI
instruction RegisterMarket {
  discriminant DISC_REGISTER_MARKET
  entry 24

  const ACCOUNTS_REQUIRED    = 10
  const INSTRUCTION_DATA_LEN = 1

  input_layout {
    n_accounts       : U64    @ 0       "Number of accounts in input buffer"
    user_data_len    : U64    @ 88      "Data length of user account"
    market_dup       : U8     @ 10344   "Market account duplicate flag"
    market_data_len  : U64    @ 10424   "Market account data length"
    market_pubkey    : Pubkey @ 10352   "Market account address (4 chunks)"
    base_mint_dup    : U8     @ 20680   "Base mint duplicate flag"
    base_data_len    : U64    @ 20760   "Base mint data length"
  }

  insn_layout {
    insn_len         : U64    @ -8      "Instruction data length"
    discriminant     : U8     @ 0       "Instruction discriminant byte"
  }

  /// Instruction byte must be REGISTER_MARKET
  guard rejects_invalid_discriminant {
    checks discriminant == DISC_REGISTER_MARKET
    error InvalidDiscriminant
    fuel 8
  }
  guard rejects_invalid_account_count {
    checks n_accounts >= ACCOUNTS_REQUIRED
    error InvalidNumberOfAccounts
    fuel 10
  }
  guard rejects_invalid_instruction_length {
    checks insn_len == INSTRUCTION_DATA_LEN
    error InvalidInstructionLength
    fuel 12
  }
  guard rejects_user_has_data {
    checks user_data_len == DATA_LEN_ZERO
    error UserHasData
    fuel 14
  }
  guard rejects_market_duplicate {
    checks market_dup == ACCT_NON_DUP_MARKER
    error MarketAccountIsDuplicate
    fuel 16
  }
  guard rejects_market_has_data {
    checks market_data_len == DATA_LEN_ZERO
    error MarketHasData
    fuel 18
  }
  guard rejects_base_mint_duplicate {
    checks base_mint_dup == ACCT_NON_DUP_MARKER
    error BaseMintIsDuplicate
    fuel 20
  }
  guard rejects_quote_mint_duplicate {
    error QuoteMintIsDuplicate
    fuel 30
  }
  guard rejects_invalid_market_pubkey {
    checks market_pubkey == derived_pda
    error InvalidMarketPubkey
    fuel 61
  }
  guard rejects_system_program_duplicate {
    error SystemProgramIsDuplicate
    fuel 74
  }
  guard rejects_invalid_system_program_pubkey {
    error InvalidSystemProgramPubkey
    fuel 86
  }
  guard rejects_rent_sysvar_duplicate {
    error RentSysvarIsDuplicate
    fuel 96
  }
  guard rejects_invalid_rent_sysvar_pubkey {
    checks rent_pubkey == RENT
    error InvalidRentSysvarPubkey
    fuel 108
  }

  property memory_safety {
    scope guards
  }
  property pda_derivation {
    flow market_pda from seeds [base_mint_addr, quote_mint_addr]
  }
  property account_pointer_flow {
    flow r9 through [market, system_program, rent_sysvar]
  }
  property cpi_create_account {
    cpi system_program CreateAccount {
      payer        user
      target       market_pda
      space        SIZE_OF_MARKET_HEADER
      signer_seeds [base_mint_addr, quote_mint_addr, bump]
    }
  }
  property accepts_valid_input {
    after all guards
    exit 0
  }
}
"#;

    #[test]
    fn lean_gen_sbpf_routes_to_sbpf_renderer() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Should use sBPF imports, not state-machine imports
        assert!(lean.contains("open QEDGen.Solana.SBPF"));
        assert!(lean.contains("import QEDGen"));
        assert!(!lean.contains("structure State where"));
    }

    #[test]
    fn lean_gen_sbpf_namespace() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("namespace RegisterMarket"));
        assert!(lean.contains("end RegisterMarket"));
    }

    #[test]
    fn lean_gen_sbpf_constants() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Global constants are emitted as comments (avoid conflict with prog module)
        assert!(lean.contains("--   DISC_REGISTER_MARKET = 0"));
        assert!(lean.contains("--   ACCT_NON_DUP_MARKER = 255"));
        assert!(lean.contains("--   DATA_LEN_ZERO = 0"));
        // Instruction-level constants ARE emitted as abbrevs
        assert!(lean.contains("abbrev ACCOUNTS_REQUIRED : Nat := 10"));
        assert!(lean.contains("abbrev INSTRUCTION_DATA_LEN : Nat := 1"));
    }

    #[test]
    fn lean_gen_sbpf_pubkey_chunks() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Pubkey chunks are emitted as comments (avoid conflict with prog module)
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_0 = 5862609301215225606"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_1 = 9219231539345853473"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_2 = 4971307250928769624"));
        assert!(lean.contains("--   PUBKEY_RENT_CHUNK_3 = 2329533411"));
    }

    #[test]
    fn lean_gen_sbpf_error_constants() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Error constants emitted as abbrevs in instruction namespace
        assert!(lean.contains("abbrev E_INVALID_DISCRIMINANT : Nat := 1"));
        assert!(lean.contains("abbrev E_INVALID_NUMBER_OF_ACCOUNTS : Nat := 3"));
        assert!(lean.contains("abbrev E_MARKET_ACCOUNT_IS_DUPLICATE : Nat := 5"));
        assert!(lean.contains("abbrev E_INVALID_RENT_SYSVAR_PUBKEY : Nat := 13"));
    }

    #[test]
    fn lean_gen_sbpf_offset_constants_and_ea_lemmas() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Offset constants
        assert!(lean.contains("abbrev N_ACCOUNTS : Int := 0"));
        assert!(lean.contains("abbrev USER_DATA_LEN : Int := 88"));
        assert!(lean.contains("abbrev MARKET_DUP : Int := 10344"));
        assert!(lean.contains("abbrev MARKET_PUBKEY : Int := 10352"));
        // ea_* lemmas
        assert!(lean
            .contains("@[simp] theorem ea_N_ACCOUNTS (b : Nat) : effectiveAddr b N_ACCOUNTS = b"));
        assert!(lean.contains(
            "@[simp] theorem ea_USER_DATA_LEN (b : Nat) : effectiveAddr b USER_DATA_LEN = b + 88"
        ));
        // Negative offset for insn_layout
        assert!(lean.contains("abbrev INSN_LEN : Int := -8"));
        assert!(lean
            .contains("@[simp] theorem ea_INSN_LEN (b : Nat) : effectiveAddr b INSN_LEN = b - 8"));
    }

    #[test]
    fn lean_gen_sbpf_guard_theorems() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // P1: discriminant check — field "discriminant" → var "discriminant"
        assert!(lean.contains("theorem rejects_invalid_discriminant"));
        assert!(lean.contains("h_discriminant_ne : discriminant ≠ DISC_REGISTER_MARKET"));
        assert!(lean.contains("= some E_INVALID_DISCRIMINANT"));
        // P2: account count check — field "n_accounts" → var "nAccounts"
        assert!(lean.contains("theorem rejects_invalid_account_count"));
        assert!(lean.contains("h_nAccounts_lt : nAccounts < ACCOUNTS_REQUIRED"));
        // P5: market duplicate check (should have accumulated hypotheses from P1-P4)
        assert!(lean.contains("theorem rejects_market_duplicate"));
        assert!(lean.contains("= some E_MARKET_ACCOUNT_IS_DUPLICATE"));
    }

    #[test]
    fn lean_gen_sbpf_hypothesis_accumulation() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // P2 (rejects_invalid_account_count) should have after-hypothesis from P1
        // The after-hyp from P1 is: readU8 at insn addr = DISC_REGISTER_MARKET
        let p2_section = lean
            .split("theorem rejects_invalid_account_count")
            .nth(1)
            .unwrap()
            .split("theorem ")
            .next()
            .unwrap();
        assert!(p2_section.contains("h_disc"));
    }

    #[test]
    fn lean_gen_sbpf_spec_structure() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("structure Spec (progAt : Nat → Option Insn) where"));
        // Should have a field for each guard
        assert!(lean.contains("  rejects_invalid_discriminant :"));
        assert!(lean.contains("  rejects_market_duplicate :"));
        assert!(lean.contains("  rejects_invalid_rent_sysvar_pubkey :"));
    }

    #[test]
    fn lean_gen_sbpf_property_stubs() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem memory_safety : True := trivial"));
        assert!(lean.contains("theorem pda_derivation : True := trivial"));
        assert!(lean.contains("theorem account_pointer_flow : True := trivial"));
        assert!(lean.contains("theorem cpi_create_account : True := trivial"));
        assert!(lean.contains("theorem accepts_valid_input : True := trivial"));
    }

    #[test]
    fn lean_gen_sbpf_initstate2_for_two_pointer() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Dropset has insn_layout, so should use initState2
        assert!(lean.contains("initState2 inputAddr insnAddr mem"));
    }

    #[test]
    fn lean_gen_sbpf_entry_point() {
        let spec = parser::parse(DROPSET_SPEC).unwrap();
        let lean = render(&spec);
        // Dropset entry is 24
        assert!(lean.contains("initState2 inputAddr insnAddr mem 24"));
    }

    // ========================================================================
    // v2.0 feature tests
    // ========================================================================

    const PERCOLATOR_SPEC: &str =
        include_str!("../../../examples/rust/percolator/percolator.qedspec");

    #[test]
    fn lean_gen_proof_decomposition_sub_lemmas() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // Per-operation sub-lemmas for threshold_bounded
        assert!(lean.contains("theorem threshold_bounded_preserved_by_create_vault"));
        assert!(lean.contains("theorem threshold_bounded_preserved_by_propose"));
        assert!(lean.contains("theorem threshold_bounded_preserved_by_approve"));
        // Sub-lemmas have sorry
        assert!(lean.contains("threshold_bounded_preserved_by_create_vault"));
        // Master theorem uses exact
        assert!(lean.contains("exact threshold_bounded_preserved_by_create_vault"));
    }

    #[test]
    fn lean_gen_aborts_if_theorems() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem create_vault_aborts_if_InvalidThreshold"));
        assert!(lean.contains("theorem create_vault_aborts_if_TooManyMembers"));
        assert!(lean.contains("theorem approve_aborts_if_NotAMember"));
        assert!(lean.contains("theorem execute_aborts_if_ThresholdNotMet"));
        // All should prove the transition returns none
        assert!(lean.contains("= none := sorry"));
    }

    #[test]
    fn lean_gen_cover_theorems() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem cover_proposal_lifecycle"));
        assert!(lean.contains("theorem cover_cancel_flow"));
        // Should be existential proofs
        assert!(lean.contains("∃ (s0 : State) (signer : Pubkey)"));
    }

    #[test]
    fn lean_gen_liveness_theorem() {
        let spec = parser::parse(PERCOLATOR_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem liveness_drain_completes"));
        assert!(lean.contains("s.status = .Draining"));
        assert!(lean.contains("s'.status = .Active"));
        assert!(lean.contains("ops.length ≤ 2"));
    }

    #[test]
    fn lean_gen_overflow_obligations() {
        let spec = parser::parse(MULTISIG_SPEC).unwrap();
        let lean = render(&spec);
        // approve has an add effect (approval_count += 1)
        assert!(lean.contains("theorem approve_overflow_safe"));
        assert!(lean.contains("valid_u"));
    }

    #[test]
    fn lean_gen_multi_aborts_if() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        // Pool ops: init_pool and deposit have aborts_if
        assert!(lean.contains("theorem init_pool_aborts_if_InvalidAmount"));
        assert!(lean.contains("theorem deposit_aborts_if_InvalidAmount"));
        // Loan ops: borrow has aborts_if
        assert!(lean.contains("theorem borrow_aborts_if_InvalidAmount"));
    }

    #[test]
    fn lean_gen_multi_environment() {
        let spec = parser::parse(LENDING_SPEC).unwrap();
        let lean = render(&spec);
        assert!(lean.contains("theorem pool_solvency_under_interest_rate_change"));
        assert!(lean.contains("new_interest_rate"));
        assert!(lean.contains("{ s with interest_rate := new_interest_rate }"));
    }
}
