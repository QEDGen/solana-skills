//! Effect-conformance harnesses (per-(handler, field) and per-Branch-arm),
//! overflow-detection proofs for add effects, and the single-mode-only
//! file-level features (covers / liveness / environment).

use super::*;

/// Per-(handler, field) effect-conformance harnesses — one proof per pair
/// so a single stuck mul/div field can't block sibling-field verification.
/// Solver per harness via `pick_kani_solver_for_effect`: cadical (scalar /
/// linear, default), minisat (narrow-type mul/div), z3 (wide-type mul/div).
///
/// Body: skip fields whose base isn't in this section's State (multi-account
/// safety); zeroed/symbolic pre-state; `pre_<F>` snapshots for every mutable
/// field (skipping the target of a `set`); then under `if <handler>(...)`:
/// set → `s.F == <resolved>`, add/sub → `s.F == pre_F.wrapping_{add,sub}(<resolved>)`;
/// sibling fields assert `s.G == pre_G` unless another effect in the same
/// handler mutates them.
pub(crate) fn emit_effect_conformance_harnesses(
    out: &mut String,
    mir: &Mir,
    parsed: &ParsedSpec,
) -> Result<()> {
    use crate::codegen_shared::sanitize_ident;
    use crate::rust_codegen_util as util;

    let handlers: Vec<&crate::check::ParsedHandler> = parsed.handlers.iter().collect();

    let effect_ops: Vec<&crate::check::ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|op| op.has_effect())
        .collect();

    if effect_ops.is_empty() {
        return Ok(());
    }

    // Resolve view.
    let (state_fields, lifecycle): (&[(String, String)], &[String]) =
        if parsed.account_types.len() == 1 {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        } else if parsed.account_types.is_empty() {
            (
                util::resolve_state_fields(parsed),
                parsed.lifecycle_states.as_slice(),
            )
        } else {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        };
    let mutable = util::mutable_fields(state_fields);
    let properties: Vec<&crate::check::ParsedProperty> = parsed.properties.iter().collect();

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Effect conformance — verify transition effects match spec\n");
    out.push_str("//\n");
    out.push_str("// Each proof applies a transition to symbolic state and checks that every\n");
    out.push_str("// field changed/unchanged matches the spec's effect: declarations.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let field_type_lookup: std::collections::HashMap<&str, &str> = mutable
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();

    for op in &effect_ops {
        // Iterate the handler's lowered MIR body projected onto triples
        // (not `op.effects`); the sibling-frame check reads the same list.
        let body = mir
            .handler_block(&op.name)
            .ok_or_else(|| anyhow::anyhow!("MIR has no handler `{}`", op.name))?;
        let triples = util::block_effect_triples(body);
        for (field, op_kind, value) in triples.iter().cloned() {
            let harness_name = format!("verify_{}_effect_{}", op.name, sanitize_ident(&field));
            emit_one_conformance_harness(
                out,
                parsed,
                op,
                &mutable,
                lifecycle,
                &properties,
                &field_type_lookup,
                &harness_name,
                &[],
                (&field, op_kind, value),
                &triples,
            )?;
        }

        // Conditional effects: one harness per (arm, effect) under a
        // `kani::assume(<scrutinee> == <pattern>)` pin, so post-state
        // assertions hold under match semantics (exactly one arm fires).
        // The sibling-frame check is scoped to the arm's own effects —
        // with the arm pinned, no other arm can mutate. The wildcard arm
        // pins via negated assumes over every literal pattern.
        let branch = body.stmts.iter().find_map(|st| match st {
            crate::mir::Stmt::Branch {
                scrutinee,
                arms,
                default,
            } => Some((scrutinee, arms, default)),
            _ => None,
        });
        if let Some((scrutinee, arms, default)) = branch {
            let scrut = match scrutinee {
                crate::mir::BranchScrutinee::Match(e) => e.rust.as_str(),
                crate::mir::BranchScrutinee::Predicate(p) => p.0.rust.as_str(),
            };
            let patterns: Vec<&str> = arms
                .iter()
                .filter_map(|a| a.pattern.as_ref().map(|p| p.rust.as_str()))
                .collect();
            for (idx, arm) in arms.iter().enumerate() {
                let Some(pattern) = arm.pattern.as_ref().map(|p| p.rust.as_str()) else {
                    continue;
                };
                let assume = vec![format!("    kani::assume({} == {});\n", scrut, pattern)];
                let arm_triples = util::block_effect_triples(&arm.block);
                for (field, op_kind, value) in arm_triples.iter().cloned() {
                    let harness_name = format!(
                        "verify_{}_arm{}_effect_{}",
                        op.name,
                        idx,
                        sanitize_ident(&field)
                    );
                    emit_one_conformance_harness(
                        out,
                        parsed,
                        op,
                        &mutable,
                        lifecycle,
                        &properties,
                        &field_type_lookup,
                        &harness_name,
                        &assume,
                        (&field, op_kind, value),
                        &arm_triples,
                    )?;
                }
            }
            if let Some(default_block) = default {
                let assumes: Vec<String> = patterns
                    .iter()
                    .map(|p| format!("    kani::assume({} != {});\n", scrut, p))
                    .collect();
                let default_triples = util::block_effect_triples(default_block);
                for (field, op_kind, value) in default_triples.iter().cloned() {
                    let harness_name = format!(
                        "verify_{}_default_effect_{}",
                        op.name,
                        sanitize_ident(&field)
                    );
                    emit_one_conformance_harness(
                        out,
                        parsed,
                        op,
                        &mutable,
                        lifecycle,
                        &properties,
                        &field_type_lookup,
                        &harness_name,
                        &assumes,
                        (&field, op_kind, value),
                        &default_triples,
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// One effect-conformance harness: symbolic (or zeroed-init) state,
/// symbolic params, optional scrutinee-pin assumes (per-arm sites),
/// transition call, post-state assertion for the target effect, and the
/// frame check over `sibling_triples` (the effect set that can legally
/// fire alongside the target — the whole flat body, or one Branch arm).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_one_conformance_harness(
    out: &mut String,
    parsed: &ParsedSpec,
    op: &crate::check::ParsedHandler,
    mutable: &[&(String, String)],
    lifecycle: &[String],
    properties: &[&crate::check::ParsedProperty],
    field_type_lookup: &std::collections::HashMap<&str, &str>,
    harness_name: &str,
    assume_lines: &[String],
    (field, op_kind, value): (&str, &str, &str),
    sibling_triples: &[(String, &'static str, &str)],
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    let is_init = op.pre_status.as_deref() == Some("Uninitialized");

    let base = util::effect_target_base(field);
    if !field_type_lookup.contains_key(base) {
        return Ok(());
    }

    let field_type = field_type_lookup.get(field).copied().unwrap_or("");
    let solver = util::pick_kani_solver_for_effect(field_type, value, op);

    out.push_str("#[kani::proof]\n");
    out.push_str("#[kani::unwind(2)]\n");
    out.push_str(&format!("#[kani::solver({})]\n", solver));
    out.push_str(&format!("fn {}() {{\n", harness_name));

    if is_init {
        util::emit_state_init_zeroed(out, mutable, lifecycle, parsed);
    } else {
        util::emit_state_init_symbolic(out, mutable, lifecycle);
        util::emit_pre_status_assume(out, op, lifecycle);
    }

    for (pname, ptype) in &op.takes_params {
        out.push_str(&format!(
            "    let {}: {} = kani::any();\n",
            pname,
            map_type(ptype, parsed)?
        ));
    }
    util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;

    // Pin the scrutinee to this arm (or away from every literal pattern,
    // for the wildcard arm) before any state is read.
    for line in assume_lines {
        out.push_str(line);
    }

    // Bounds assumptions for arithmetic safety (non-init only).
    if !is_init {
        if !parsed.constants.is_empty() {
            for (cname, _) in &parsed.constants {
                let upper = cname.to_uppercase();
                if upper.contains("MAX") || upper.contains("MEMBER") {
                    if mutable.iter().any(|(f, _)| f == "member_count") {
                        out.push_str(&format!("    kani::assume(s.member_count <= {});\n", upper));
                    }
                    break;
                }
            }
        }
        let owned_props: Vec<crate::check::ParsedProperty> =
            properties.iter().map(|p| (*p).clone()).collect();
        util::emit_add_strict_bounds(
            out,
            op,
            &owned_props,
            "    kani::assume(s.{field} < s.{bound}); // strict bound: {field} increments\n",
        );
    }

    // Pre-state snapshot — every mutable field except the
    // set-target.
    let needs_pre_for: Vec<&&(String, String)> = mutable
        .iter()
        .filter(|(fname, _)| !(fname.as_str() == field && op_kind == "set"))
        .collect();
    for (fname, _) in &needs_pre_for {
        out.push_str(&format!("    let pre_{} = s.{};\n", fname, fname));
    }

    emit_kani_account_env_binding(out, op, "accounts", "    ");
    let args = transition_call_args(
        op,
        util::handler_needs_account_env(op).then_some("accounts"),
    );
    out.push_str(&format!("    if {}(&mut s{}) {{\n", op.name, args));

    let resolved = util::resolve_value_with_account_env(
        value,
        op,
        parsed,
        Some("pre_"),
        util::handler_needs_account_env(op).then_some("accounts"),
    );
    match op_kind {
        "set" => {
            let assertion = util::rewrite_kani_pubkey_comparisons(
                &format!("s.{field} == {resolved}"),
                op,
                parsed,
            );
            out.push_str(&format!(
                "        assert!({}, \"{} must equal {}\");\n",
                assertion, field, resolved
            ));
        }
        "add" => {
            out.push_str(&format!(
                "        assert!(s.{} == pre_{}.wrapping_add({}), \"{} must increment by {}\");\n",
                field, field, resolved, field, resolved
            ));
        }
        "sub" => {
            out.push_str(&format!(
                "        assert!(s.{} == pre_{}.wrapping_sub({}), \"{} must decrement by {}\");\n",
                field, field, resolved, field, resolved
            ));
        }
        _ => {}
    }

    // Assert sibling fields unchanged (unless mutated by another
    // effect in the same frame — the flat body, or this arm).
    for (fname, _) in mutable {
        if fname.as_str() != field {
            let sibling_mutated = sibling_triples
                .iter()
                .any(|(f, _, _)| f.as_str() == fname.as_str());
            if !sibling_mutated {
                let assertion = util::rewrite_kani_pubkey_comparisons(
                    &format!("s.{fname} == pre_{fname}"),
                    op,
                    parsed,
                );
                out.push_str(&format!(
                    "        assert!({}, \"{} must not change\");\n",
                    assertion, fname
                ));
            }
        }
    }

    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Emit `#[kani::proof] fn verify_<handler>_no_overflow()` per handler
/// with an `add` effect. No explicit assert — Kani's built-in overflow
/// detection fires on `+=` inside the transition body; the proof exists
/// to drive BMC across the parameter space.
pub(crate) fn emit_overflow_detection_harnesses(
    out: &mut String,
    mir: &Mir,
    parsed: &ParsedSpec,
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    let handlers: Vec<&crate::check::ParsedHandler> = parsed.handlers.iter().collect();

    // Checked-add filter reads the lowered MIR body (`Stmt::CheckedAdd`
    // projects to kind "add"), deep-walked: a checked add inside a
    // `Stmt::Branch` arm can still overflow, and the harness just invokes
    // the transition (Kani explores every match arm).
    let overflow_ops: Vec<&crate::check::ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|op| {
            mir.handler_block(&op.name).is_some_and(|body| {
                util::block_effect_triples_deep(body)
                    .iter()
                    .any(|(_, kind, _)| *kind == "add")
            })
        })
        .collect();

    if overflow_ops.is_empty() {
        return Ok(());
    }

    // Resolve view.
    let (state_fields, lifecycle): (&[(String, String)], &[String]) =
        if parsed.account_types.len() == 1 {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        } else if parsed.account_types.is_empty() {
            (
                util::resolve_state_fields(parsed),
                parsed.lifecycle_states.as_slice(),
            )
        } else {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        };
    let mutable = util::mutable_fields(state_fields);

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Overflow detection — Kani catches arithmetic overflow on add effects\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in &overflow_ops {
        out.push_str("#[kani::proof]\n");
        out.push_str("#[kani::unwind(2)]\n");
        out.push_str("#[kani::solver(cadical)]\n");
        out.push_str(&format!("fn verify_{}_no_overflow() {{\n", op.name));

        util::emit_state_init_symbolic(out, &mutable, lifecycle);
        util::emit_pre_status_assume(out, op, lifecycle);

        for (pname, ptype) in &op.takes_params {
            out.push_str(&format!(
                "    let {}: {} = kani::any();\n",
                pname,
                map_type(ptype, parsed)?
            ));
        }
        util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;

        emit_kani_account_env_binding(out, op, "accounts", "    ");
        let args = transition_call_args(
            op,
            util::handler_needs_account_env(op).then_some("accounts"),
        );
        out.push_str(&format!(
            "    {}(&mut s{});  // Kani detects overflow on += internally\n",
            op.name, args
        ));
        out.push_str("}\n\n");
    }

    Ok(())
}

/// Covers / liveness / environment harnesses at file scope. These reference
/// handlers by name and the per-spec `State` directly, so they only fire in
/// single-account mode; multi-account specs skip them.
///
///   1. Covers (reachability) — per `(cover, trace)` pair, nested `if`
///      chain over the trace handlers capped with `kani::cover!(<last_op>(...))`.
///   2. Liveness (bounded reachability) — assume the from-state, loop
///      `0..bound` dispatching via_ops on a non-deterministic `op: u8`,
///      then `kani::cover!(s.status == Status::<to_state>)`. Skipped (with
///      a structured comment) when the spec has no lifecycle.
///   3. Environment — per `(env, property)` cross: assume the property pre,
///      mutate `env.mutates` fields to `kani::any()`, assume the
///      constraints, then `assert!(<prop>(&s))`.
pub(crate) fn emit_file_level_features(out: &mut String, parsed: &ParsedSpec) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    // Resolve view — same logic as `emit_account_section_structural`.
    let (state_fields, lifecycle): (&[(String, String)], &[String]) =
        if parsed.account_types.len() == 1 {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        } else {
            // Zero account types — flat state form.
            (
                util::resolve_state_fields(parsed),
                parsed.lifecycle_states.as_slice(),
            )
        };
    let mutable = util::mutable_fields(state_fields);
    let has_lifecycle = lifecycle.len() >= 2;

    // ── Cover properties ──────────────────────────────────────────
    if !parsed.covers.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Cover properties — reachability via kani::cover!\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for cover in &parsed.covers {
            for (i, trace) in cover.traces.iter().enumerate() {
                let suffix = if cover.traces.len() > 1 {
                    format!("_{}", i)
                } else {
                    String::new()
                };
                out.push_str("#[kani::proof]\n");
                let unwind = trace.len() + 1;
                out.push_str(&format!("#[kani::unwind({})]\n", unwind));
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!("fn cover_{}{}() {{\n", cover.name, suffix));

                util::emit_state_init_symbolic(out, &mutable, lifecycle);

                let mut indent = "    ".to_string();
                for (j, op_name) in trace.iter().enumerate() {
                    let op = parsed.handlers.iter().find(|o| o.name == *op_name);
                    if let Some(op) = op {
                        for (pname, ptype) in &op.takes_params {
                            out.push_str(&format!(
                                "{}let {}_{}: {} = kani::any();\n",
                                indent,
                                pname,
                                j,
                                map_type(ptype, parsed)?
                            ));
                        }
                    }
                    if let Some(o) = op {
                        emit_kani_account_env_binding(out, o, &format!("accounts_{}", j), &indent);
                    }
                    let args: String = op
                        .map(|o| {
                            let mut args = String::new();
                            if util::handler_needs_account_env(o) {
                                args.push_str(&format!(", &accounts_{}", j));
                            }
                            for (n, _) in &o.takes_params {
                                args.push_str(&format!(", {}_{}", n, j));
                            }
                            args
                        })
                        .unwrap_or_default();

                    if j < trace.len() - 1 {
                        out.push_str(&format!("{}if {}(&mut s{}) {{\n", indent, op_name, args));
                        indent.push_str("    ");
                    } else {
                        out.push_str(&format!(
                            "{}kani::cover!({}(&mut s{}), \"{} trace is reachable\");\n",
                            indent, op_name, args, cover.name
                        ));
                    }
                }
                // Close braces (one less than trace length).
                for _ in 0..trace.len().saturating_sub(1) {
                    indent = indent[..indent.len() - 4].to_string();
                    out.push_str(&format!("{}}}\n", indent));
                }
                out.push_str("}\n\n");
            }
        }
    }

    // ── Liveness properties ──────────────────────────────────────
    if !parsed.liveness_props.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Liveness properties — bounded reachability via non-deterministic ops\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for liveness in &parsed.liveness_props {
            let bound = liveness.within_steps.unwrap_or(10) as usize;

            // No lifecycle → no target predicate; skip with a structured comment.
            if !has_lifecycle {
                out.push_str(&format!(
                    "// liveness {}: skipped — spec has no lifecycle, no target predicate to cover\n\n",
                    liveness.name
                ));
                continue;
            }

            out.push_str("#[kani::proof]\n");
            out.push_str(&format!("#[kani::unwind({})]\n", bound + 1));
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!("fn verify_liveness_{}() {{\n", liveness.name));

            util::emit_state_init_symbolic(out, &mutable, lifecycle);

            // Assume the from-state so via-ops can fire.
            out.push_str(&format!(
                "    kani::assume(s.status == Status::{});\n",
                liveness.from_state
            ));

            let via_ops = &liveness.via_ops;
            out.push_str(&format!("    for _ in 0..{} {{\n", bound));
            out.push_str("        let op: u8 = kani::any();\n");
            out.push_str("        match op {\n");
            for (i, op_name) in via_ops.iter().enumerate() {
                let op = parsed.handlers.iter().find(|o| o.name == *op_name);
                let param_decls: String = match op {
                    Some(o) => o
                        .takes_params
                        .iter()
                        .map(|(n, t)| {
                            map_type(t, parsed)
                                .map(|rt| format!("            let {}: {} = kani::any();\n", n, rt))
                        })
                        .collect::<anyhow::Result<String>>()?,
                    None => String::new(),
                };
                let args: String = op
                    .map(|o| {
                        transition_call_args(
                            o,
                            util::handler_needs_account_env(o).then_some("accounts"),
                        )
                    })
                    .unwrap_or_default();

                out.push_str(&format!("            {} => {{\n", i));
                out.push_str(&param_decls);
                if let Some(o) = op {
                    emit_kani_account_env_binding(out, o, "accounts", "            ");
                }
                out.push_str(&format!("                {}(&mut s{});\n", op_name, args));
                out.push_str("            }\n");
            }
            out.push_str("            _ => {}\n");
            out.push_str("        }\n");
            out.push_str("    }\n");

            out.push_str(&format!(
                "    kani::cover!(s.status == Status::{}, \"{} reaches {} within {} steps\");\n",
                liveness.leads_to_state, liveness.name, liveness.leads_to_state, bound
            ));
            out.push_str("}\n\n");
        }
    }

    // ── Environment harnesses ────────────────────────────────────
    if !parsed.environments.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Environment — properties hold under external state changes\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for env in &parsed.environments {
            for prop in &parsed.properties {
                if prop.expression.is_none() {
                    continue;
                }
                let rust_constraints: &[String] = &env.constraints_rust;

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!(
                    "fn verify_{}_under_{}() {{\n",
                    prop.name, env.name
                ));

                util::emit_state_init_symbolic(out, &mutable, lifecycle);
                out.push_str(&format!("    kani::assume({}(&s));\n", prop.name));

                for (field, ftype) in &env.mutates {
                    out.push_str(&format!("    s.{} = kani::any();\n", field));
                    let _ = ftype;
                }
                for constraint in rust_constraints {
                    out.push_str(&format!("    kani::assume({});\n", constraint));
                }

                out.push_str(&format!("    assert!({}(&s),\n", prop.name));
                out.push_str(&format!(
                    "        \"{} must hold after {}\");\n",
                    prop.name, env.name
                ));
                out.push_str("}\n\n");
            }
        }
    }

    Ok(())
}
