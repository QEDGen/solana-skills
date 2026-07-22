//! Effect-conformance harnesses (per-(handler, field) and per-Branch-arm),
//! overflow-detection proofs for add effects, and the single-mode-only
//! file-level features (covers / liveness / environment).

use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

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
    rec: &mut ObligationRecorder,
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

    let (state_fields, lifecycle) = resolve_account_view(parsed);
    let mutable = util::field_refs(state_fields);
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
            let field_name = util::effect_path_source(field);
            let harness_name = format!("verify_{}_effect_{}", op.name, sanitize_ident(&field_name));
            emit_one_conformance_harness(
                out,
                rec,
                ConformanceHarness {
                    parsed,
                    op,
                    mutable: &mutable,
                    lifecycle,
                    properties: &properties,
                    field_type_lookup: &field_type_lookup,
                    harness_name: &harness_name,
                    assume_lines: &[],
                    effect: (field, op_kind, value),
                    sibling_triples: &triples,
                },
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
                crate::mir::BranchScrutinee::Match(e) => util::mir_expr_rust(e),
                crate::mir::BranchScrutinee::Predicate(p) => util::mir_expr_rust(&p.0),
            };
            let patterns: Vec<String> = arms
                .iter()
                .filter_map(|a| a.pattern.as_ref().map(util::mir_expr_rust))
                .collect();
            for (idx, arm) in arms.iter().enumerate() {
                let Some(pattern) = arm.pattern.as_ref().map(util::mir_expr_rust) else {
                    continue;
                };
                let assume = vec![format!("    kani::assume({} == {});\n", scrut, pattern)];
                let arm_triples = util::block_effect_triples(&arm.block);
                for (field, op_kind, value) in arm_triples.iter().cloned() {
                    let harness_name = format!(
                        "verify_{}_arm{}_effect_{}",
                        op.name,
                        idx,
                        sanitize_ident(&util::effect_path_source(field))
                    );
                    emit_one_conformance_harness(
                        out,
                        rec,
                        ConformanceHarness {
                            parsed,
                            op,
                            mutable: &mutable,
                            lifecycle,
                            properties: &properties,
                            field_type_lookup: &field_type_lookup,
                            harness_name: &harness_name,
                            assume_lines: &assume,
                            effect: (field, op_kind, value),
                            sibling_triples: &arm_triples,
                        },
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
                        sanitize_ident(&util::effect_path_source(field))
                    );
                    emit_one_conformance_harness(
                        out,
                        rec,
                        ConformanceHarness {
                            parsed,
                            op,
                            mutable: &mutable,
                            lifecycle,
                            properties: &properties,
                            field_type_lookup: &field_type_lookup,
                            harness_name: &harness_name,
                            assume_lines: &assumes,
                            effect: (field, op_kind, value),
                            sibling_triples: &default_triples,
                        },
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Carrier for `emit_one_conformance_harness` (same pattern as
/// `GuardRejectionHarness` — the positional-arg list had grown to 11).
pub(crate) struct ConformanceHarness<'a> {
    pub(crate) parsed: &'a ParsedSpec,
    pub(crate) op: &'a crate::check::ParsedHandler,
    pub(crate) mutable: &'a [&'a (String, String)],
    pub(crate) lifecycle: &'a [String],
    pub(crate) properties: &'a [&'a crate::check::ParsedProperty],
    pub(crate) field_type_lookup: &'a std::collections::HashMap<&'a str, &'a str>,
    pub(crate) harness_name: &'a str,
    pub(crate) assume_lines: &'a [String],
    /// The target effect: `(field, op_kind, value)`.
    pub(crate) effect: (&'a crate::mir::Path, &'a str, &'a crate::mir::Expr),
    /// The effect set that can legally fire alongside the target — the
    /// whole flat body, or one Branch arm.
    pub(crate) sibling_triples: &'a [(&'a crate::mir::Path, &'static str, &'a crate::mir::Expr)],
}

/// One effect-conformance harness: symbolic (or zeroed-init) state,
/// symbolic params, optional scrutinee-pin assumes (per-arm sites),
/// transition call, post-state assertion for the target effect, and the
/// frame check over `sibling_triples`.
pub(crate) fn emit_one_conformance_harness(
    out: &mut String,
    rec: &mut ObligationRecorder,
    ctx: ConformanceHarness<'_>,
) -> Result<()> {
    use crate::rust_codegen_util as util;

    let ConformanceHarness {
        parsed,
        op,
        mutable,
        lifecycle,
        properties,
        field_type_lookup,
        harness_name,
        assume_lines,
        effect: (field, op_kind, value),
        sibling_triples,
    } = ctx;

    let is_init = op.pre_status.as_deref() == Some("Uninitialized");
    let effect_path = field;
    let field_owned = util::effect_path_source(effect_path);
    let field = field_owned.as_str();

    let base = util::effect_target_base(field);
    if !field_type_lookup.contains_key(base) {
        return Ok(());
    }
    rec.emitted(
        ObligationKind::EffectConformance,
        &op.name,
        harness_name,
        harness_name,
    );

    let field_type = field_type_lookup.get(field).copied().unwrap_or("");
    let solver = util::pick_kani_solver_for_effect(field_type, &util::mir_expr_rust(value), op);

    emit_proof_preamble(
        out,
        parsed,
        Some(op),
        mutable,
        lifecycle,
        PreambleOpts {
            harness_name,
            unwind: 2,
            solver,
            zeroed_init: is_init,
            pre_status_assume: true,
        },
    );
    emit_symbolic_params(out, parsed, op, 1)?;

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

    // Expected-value RHS reads PRE-state — the flat `pre_<field>` snapshot
    // locals taken before the transition call. Tree-native (#151 Slice 4):
    // the legacy `resolve_value` path only rebound BARE field names, so a
    // compound or indexed RHS leaked unbound (`accounts[i].capital`) or
    // post-state (`s.x`) reads into the assert.
    let tree = util::mir_expr_tree(value);
    let resolved = {
        use crate::rust_codegen_util::tree_render::{ArithMode, Binder, RustCx};
        crate::rust_codegen_util::tree_render::render_rust(
            tree,
            RustCx::native()
                .with_binder(Binder::PreLocal)
                .with_arith(ArithMode::Checked)
                .with_acct_env(util::handler_needs_account_env(op).then_some("accounts")),
        )
    };
    // Checked-expression RHS carries `?` ops (see `RustOpts::checked_arith`):
    // compare inside an `Option` context — the harness only reaches this
    // assert when the transition returned true, so the RHS must be `Some`.
    let has_try = crate::rust_codegen_util::tree_render::contains_fallible_arith(tree);
    // Subscripts in the effect target must be cast for the READ side of
    // the assertion: harness params bind at their spec types (`u8`)
    // while Rust arrays index by `usize`. The raw `field` stays in scope
    // for type lookups and assertion messages. Same rewrite the
    // transition emitter applies to the write side (#295 (c)); it was
    // missing here, so `s.voted[member_index] == 1` was an E0277 that
    // only surfaced once the Kani compile gate began to run for this
    // example.
    let field_read = util::render_effect_target(effect_path, parsed, "s");
    let expected_eq = |expected: &str| -> String {
        if has_try {
            format!("Some({field_read}) == (|| Some({expected}))()")
        } else {
            format!("{field_read} == {expected}")
        }
    };
    match op_kind {
        "set" => {
            // A `set` RHS that is itself a comparison / boolean op
            // (`seat_active := seat_stake > 0`) must be parenthesized:
            // `s.seat_active == pre_seat_stake > 0` is a chained
            // comparison (a Rust compile error), and an `&&`/`||` RHS
            // would bind at the wrong precedence. Arithmetic / method-call
            // RHSs (the common case) render at a tighter precedence and
            // need no wrapping.
            let needs_paren = matches!(
                tree,
                crate::mir::ExprTree::Cmp { .. } | crate::mir::ExprTree::BoolOp { .. }
            );
            let rhs = if needs_paren {
                format!("({resolved})")
            } else {
                resolved.clone()
            };
            let assertion = util::rewrite_kani_pubkey_comparisons(&expected_eq(&rhs), op, parsed);
            out.push_str(&format!(
                "        assert!({}, \"{} must equal {}\");\n",
                assertion,
                field,
                resolved.escape_default()
            ));
        }
        "add" => {
            out.push_str(&format!(
                "        assert!({}, \"{} must increment by {}\");\n",
                expected_eq(&format!("pre_{}.wrapping_add({})", field, resolved)),
                field,
                resolved.escape_default()
            ));
        }
        "sub" => {
            out.push_str(&format!(
                "        assert!({}, \"{} must decrement by {}\");\n",
                expected_eq(&format!("pre_{}.wrapping_sub({})", field, resolved)),
                field,
                resolved.escape_default()
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
                .any(|(f, _, _)| util::effect_path_source(f) == fname.as_str());
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
    rec: &mut ObligationRecorder,
) -> Result<()> {
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

    let (state_fields, lifecycle) = resolve_account_view(parsed);
    let mutable = util::field_refs(state_fields);

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Overflow detection — Kani catches arithmetic overflow on add effects\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in &overflow_ops {
        rec.emitted(
            ObligationKind::Overflow,
            &op.name,
            &op.name,
            &format!("verify_{}_no_overflow", op.name),
        );
        emit_proof_preamble(
            out,
            parsed,
            Some(op),
            &mutable,
            lifecycle,
            PreambleOpts {
                harness_name: &format!("verify_{}_no_overflow", op.name),
                unwind: 2,
                solver: "cadical",
                zeroed_init: false,
                pre_status_assume: true,
            },
        );
        emit_symbolic_params(out, parsed, op, 1)?;

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
pub(crate) fn emit_file_level_features(
    out: &mut String,
    mir: &Mir,
    parsed: &ParsedSpec,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    let (state_fields, lifecycle) = resolve_account_view(parsed);
    let mutable = util::field_refs(state_fields);
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
                rec.emitted(
                    ObligationKind::Cover,
                    "file",
                    &format!("{}::{}", cover.name, i),
                    &format!("cover_{}{}", cover.name, suffix),
                );
                emit_proof_preamble(
                    out,
                    parsed,
                    None,
                    &mutable,
                    lifecycle,
                    PreambleOpts {
                        harness_name: &format!("cover_{}{}", cover.name, suffix),
                        unwind: trace.len() + 1,
                        solver: "cadical",
                        zeroed_init: false,
                        pre_status_assume: false,
                    },
                );

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
                rec.unsupported(
                    ObligationKind::Liveness,
                    "file",
                    &liveness.name,
                    UnsupportedReason::KaniLivenessNoLifecycle,
                );
                out.push_str(&format!(
                    "// liveness {}: skipped — spec has no lifecycle, no target predicate to cover\n\n",
                    liveness.name
                ));
                continue;
            }

            rec.emitted(
                ObligationKind::Liveness,
                "file",
                &liveness.name,
                &format!("verify_liveness_{}", liveness.name),
            );
            emit_proof_preamble(
                out,
                parsed,
                None,
                &mutable,
                lifecycle,
                PreambleOpts {
                    harness_name: &format!("verify_liveness_{}", liveness.name),
                    unwind: bound + 1,
                    solver: "cadical",
                    zeroed_init: false,
                    pre_status_assume: false,
                },
            );

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
            let mir_env = mir
                .environments
                .iter()
                .find(|candidate| candidate.name == env.name);
            for prop in &parsed.properties {
                if prop.expression.is_none() {
                    continue;
                }
                let (rust_constraints, needs_pre, needs_post) =
                    render_environment_constraints(mir_env, !env.external_fields.is_empty());

                rec.emitted(
                    ObligationKind::Environment,
                    "file",
                    &format!("{}::{}", prop.name, env.name),
                    &format!("verify_{}_under_{}", prop.name, env.name),
                );
                emit_proof_preamble(
                    out,
                    parsed,
                    None,
                    &mutable,
                    lifecycle,
                    PreambleOpts {
                        harness_name: &format!("verify_{}_under_{}", prop.name, env.name),
                        unwind: 2,
                        solver: "cadical",
                        zeroed_init: false,
                        pre_status_assume: false,
                    },
                );
                out.push_str(&format!("    kani::assume({}(&s));\n", prop.name));

                // `pre` snapshots the pre-mutation state for `old(state.x)`
                // reads; it must be taken BEFORE the `s.<field> = kani::any()`
                // mutations below. `post` (emitted after) aliases the mutated
                // state for `state.x` reads. Emit each only when a rendered
                // constraint actually references it.
                if needs_pre {
                    out.push_str("    let pre = s.clone();\n");
                }

                for (object, field, field_type) in &env.external_fields {
                    let rust_type = crate::codegen_shared::map_type(field_type, parsed)?;
                    out.push_str(&format!(
                        "    let pre_{}_{}: {} = kani::any();\n",
                        object, field, rust_type
                    ));
                    out.push_str(&format!(
                        "    let post_{}_{}: {} = kani::any();\n",
                        object, field, rust_type
                    ));
                }

                for (field, ftype) in &env.mutates {
                    out.push_str(&format!("    s.{} = kani::any();\n", field));
                    let _ = ftype;
                }
                if needs_post {
                    out.push_str("    let post = &s;\n");
                }
                for constraint in &rust_constraints {
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

/// Render environment constraints from typed MIR when it is complete, and
/// report which snapshot bindings the rendered constraints require.
///
/// Every constraint renders from its typed tree (#156): Binary relations
/// (and any environment with typed external fields) under the two-state
/// `PrePost` binder so `old(state.x)` / `state.x` route to distinct
/// `pre` / `post` receivers; Unary post-state assumptions under the live
/// `s` binder.
///
/// Returns `(constraints, needs_pre, needs_post)`. A state-field read renders
/// as a `pre.` / `post.` receiver (external fields render as `pre_` / `post_`,
/// so the trailing dot cleanly selects state receivers). `pre` appears only
/// for `old(state.x)`; `post` for any two-state state read — including a Unary
/// constraint over `state.x` once the environment has external fields, which
/// forces the two-state (`PrePost`) binder. The caller must emit exactly the
/// bindings the constraints reference, or the harness fails to compile.
fn render_environment_constraints(
    mir_env: Option<&crate::mir::EnvironmentMir>,
    has_external_fields: bool,
) -> (Vec<String>, bool, bool) {
    use crate::rust_codegen_util::tree_render::{render_rust, Binder, RustCx};

    let constraints: Vec<String> = mir_env
        .map(|mir_env| {
            mir_env
                .typed_constraints
                .iter()
                .map(|constraint| {
                    let tree = constraint.predicate.0.tree.as_ref().expect(
                        "environment constraint tree is always populated by the chumsky adapter (#156)",
                    );
                    // Binary relations (and any environment with typed
                    // external fields) read pre/post snapshots; unary
                    // post-state assumptions read the live state binder.
                    let binder = if constraint.class == crate::check::PropertyClass::Binary
                        || has_external_fields
                    {
                        Binder::PrePost
                    } else {
                        Binder::S
                    };
                    render_rust(tree, RustCx::native().with_binder(binder))
                })
                .collect()
        })
        .unwrap_or_default();

    let needs_pre = constraints.iter().any(|c| c.contains("pre."));
    let needs_post = constraints.iter().any(|c| c.contains("post."));
    (constraints, needs_pre, needs_post)
}
