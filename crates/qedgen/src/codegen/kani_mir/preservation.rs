//! Preservation harnesses: per-(property, handler) property-preservation,
//! per-(handler, ensures) ensures-preservation (including CPI ensures-as-fact
//! propagation), and per-(handler, invariant) invariant preservation/establish
//! proofs.

use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// Emit `#[kani::proof] fn verify_<handler>_preserves_<property>()` per
/// `(property, handler)` pair named in the property's `preserved_by` list.
///
/// Shape: pre-state zeroed for init handlers, symbolic otherwise; non-init
/// adds pre-status assume, optional per-slot binder, pre-property assumes
/// (unary only), and bound assumes; symbolic params + abstract binders;
/// `emit_add_strict_bounds` for add-effect overflow gating; then
/// `if <handler>(&mut post, args) { assert!(<prop>...); }` dispatched on
/// prop class (Binary → `prop(&pre, &post)`, per-slot Unary →
/// `prop_at(&post, binder)`, plain Unary → `prop(&post)`).
pub(crate) fn emit_property_preservation_harnesses(
    out: &mut String,
    parsed: &ParsedSpec,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    if parsed.properties.is_empty() {
        return Ok(());
    }

    let (state_fields, lifecycle) = resolve_account_view(parsed);
    let mutable = util::field_refs(state_fields);

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Property preservation — invariants hold through all transitions\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let handlers: Vec<&crate::check::ParsedHandler> = parsed.handlers.iter().collect();
    let properties: Vec<&crate::check::ParsedProperty> = parsed.properties.iter().collect();

    for prop in &properties {
        if prop.expression.is_none() {
            continue;
        }

        for op_name in &prop.preserved_by {
            // Skip handlers not in this section's scoped view.
            let Some(op) = handlers.iter().copied().find(|o| &o.name == op_name) else {
                continue;
            };

            rec.emitted(
                ObligationKind::PropertyPreservation,
                op_name,
                &prop.name,
                &format!("verify_{}_preserves_{}", op_name, prop.name),
            );
            out.push_str("#[kani::proof]\n");
            out.push_str("#[kani::unwind(2)]\n");
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!(
                "fn verify_{}_preserves_{}() {{\n",
                op_name, prop.name
            ));

            let is_init = op.pre_status.as_deref() == Some("Uninitialized");

            // Per-slot binder: skip the local binding when the handler
            // param shadows it (same binder pre & post unifies the value).
            let handler_takes_binder = match &prop.per_slot {
                Some(slot) => op
                    .takes_params
                    .iter()
                    .any(|(n, t)| n == &slot.binder_name && t == &slot.binder_type),
                _ => false,
            };
            let needs_local_binder = prop.per_slot.is_some() && !handler_takes_binder;

            if is_init {
                // Init handler — pre-state is zeroed.
                out.push_str("    let pre = ");
                out.push_str("State {\n");
                for (fname, ftype) in &mutable {
                    if let Some(default) = parsed.default_value_for_type(ftype) {
                        out.push_str(&format!("        {}: {},\n", fname, default));
                    }
                }
                if let Some(initial) = lifecycle.first() {
                    if lifecycle.len() >= 2 {
                        out.push_str(&format!("        status: Status::{},\n", initial));
                    }
                }
                out.push_str("    };\n");
                out.push_str("    let mut post = pre;\n");
            } else {
                // Non-init — pre is symbolic.
                out.push_str("    let pre = State {\n");
                for (fname, _) in &mutable {
                    out.push_str(&format!("        {}: kani::any(),\n", fname));
                }
                if lifecycle.len() >= 2 {
                    out.push_str("        status: kani::any(),\n");
                }
                out.push_str("    };\n");
                if lifecycle.len() >= 2 {
                    if let Some(ref pre_status) = op.pre_status {
                        out.push_str(&format!(
                            "    kani::assume(pre.status == Status::{});\n",
                            pre_status
                        ));
                    }
                }

                if needs_local_binder {
                    if let Some(slot) = &prop.per_slot {
                        let rust_ty = map_type(&slot.binder_type, parsed)?;
                        out.push_str(&format!(
                            "    let {}: {} = kani::any();\n",
                            slot.binder_name, rust_ty
                        ));
                    }
                }

                // Assume unary pre-properties hold; skip Binary (their
                // `(pre, post)` shape asserts trivially against `(pre, pre)`).
                for pre_prop in &properties {
                    if pre_prop.expression.is_none() {
                        continue;
                    }
                    if pre_prop.class == crate::check::PropertyClass::Binary {
                        continue;
                    }
                    match &pre_prop.per_slot {
                        Some(slot) if pre_prop.name == prop.name => {
                            out.push_str(&format!(
                                "    kani::assume({}_at(&pre, {}));\n",
                                pre_prop.name, slot.binder_name
                            ));
                        }
                        _ => {
                            out.push_str(&format!("    kani::assume({}(&pre));\n", pre_prop.name));
                        }
                    }
                }

                // Heuristic MAX*/MEMBER* constant bound on member_count.
                if !parsed.constants.is_empty() {
                    for (cname, _cval) in &parsed.constants {
                        let upper = cname.to_uppercase();
                        if upper.contains("MAX") || upper.contains("MEMBER") {
                            if mutable.iter().any(|(f, _)| f == "member_count") {
                                out.push_str(&format!(
                                    "    kani::assume(pre.member_count <= {});\n",
                                    upper
                                ));
                            }
                            break;
                        }
                    }
                }

                out.push_str("    let mut post = pre;\n");
            }

            for (pname, ptype) in &op.takes_params {
                out.push_str(&format!(
                    "    let {}: {} = kani::any();\n",
                    pname,
                    map_type(ptype, parsed)?
                ));
            }
            // Single abstract-binder emit here (not the double-emit of the
            // guard/abort sections).
            util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;

            let owned_props: Vec<crate::check::ParsedProperty> =
                properties.iter().map(|p| (*p).clone()).collect();
            util::emit_add_strict_bounds(
                out,
                op,
                &owned_props,
                "    kani::assume(pre.{field} < pre.{bound}); // strict bound: {field} increments\n",
            );

            emit_kani_account_env_binding(out, op, "accounts", "    ");
            let args = transition_call_args(
                op,
                util::handler_needs_account_env(op).then_some("accounts"),
            );
            out.push_str(&format!("    if {}(&mut post{}) {{\n", op_name, args));
            let is_binary_prop = prop.class == crate::check::PropertyClass::Binary;
            if is_binary_prop {
                out.push_str(&format!("        assert!({}(&pre, &post),\n", prop.name));
                out.push_str(&format!(
                    "            \"{} must hold after {} (binary: pre/post)\");\n",
                    prop.name, op_name
                ));
            } else {
                match &prop.per_slot {
                    Some(slot) => {
                        out.push_str(&format!(
                            "        assert!({}_at(&post, {}),\n",
                            prop.name, slot.binder_name
                        ));
                        out.push_str(&format!(
                            "            \"{} must hold after {} (forall {} : {})\");\n",
                            prop.name, op_name, slot.binder_name, slot.binder_type
                        ));
                    }
                    None => {
                        out.push_str(&format!("        assert!({}(&post),\n", prop.name));
                        out.push_str(&format!(
                            "            \"{} must hold after {}\");\n",
                            prop.name, op_name
                        ));
                    }
                }
            }
            out.push_str("    }\n");
            out.push_str("}\n\n");
        }
    }

    Ok(())
}

/// Emit `#[kani::proof] fn verify_<handler>_ensures_<idx>()` per
/// `(handler, ensures clause)` pair.
///
/// Shape: symbolic state + params + binders; `kani::assume(<full_guard>)`
/// so only pre-states the transition wouldn't reject are explored
/// (otherwise the ensures passes vacuously); `pre` snapshot AFTER the
/// assumes; then inside `if <handler>(...)`, `let post = &s;` binds the
/// ensures' `post.x` paths and the clause is asserted.
///
/// CPI ensures-as-fact: each `call Iface.foo(args)` whose callee declares
/// ensures gets the callee contract substituted with caller call-site
/// expressions and `kani::assume`d as a fact in the caller's harness.
/// Tier-0 callees (no ensures) emit nothing — the `cpi_no_callee_ensures`
/// lint surfaces this.
///
/// Position load-bearing: must sit between property-preservation and
/// invariant-preservation for snapshot byte-equivalence.
pub(crate) fn emit_ensures_preservation_harnesses(
    out: &mut String,
    parsed: &ParsedSpec,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    use crate::rust_codegen_util as util;

    let handlers: Vec<&crate::check::ParsedHandler> = parsed.handlers.iter().collect();

    let handlers_with_ensures: Vec<&crate::check::ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|h| !h.ensures.is_empty())
        .collect();

    if handlers_with_ensures.is_empty() {
        return Ok(());
    }

    let (state_fields, lifecycle) = resolve_account_view(parsed);
    let mutable = util::field_refs(state_fields);

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Ensures preservation — `ensures <expr>` clauses verified against\n");
    out.push_str("// (pre, post) of the spec-translated transition. Counterexamples here\n");
    out.push_str("// indicate the spec's effect block doesn't satisfy its own ensures —\n");
    out.push_str("// usually because the math lives in the user's Rust impl, behind a\n");
    out.push_str("// `modifies`-driven todo!() fill site. See SKILL.md §ref_impl.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in handlers_with_ensures {
        for (idx, ensures) in op.ensures.iter().enumerate() {
            rec.emitted(
                ObligationKind::EnsuresPreservation,
                &op.name,
                &idx.to_string(),
                &format!("verify_{}_ensures_{}", op.name, idx),
            );
            emit_proof_preamble(
                out,
                parsed,
                Some(op),
                &mutable,
                lifecycle,
                PreambleOpts {
                    harness_name: &format!("verify_{}_ensures_{}", op.name, idx),
                    unwind: 2,
                    solver: "cadical",
                    zeroed_init: false,
                    pre_status_assume: true,
                },
            );
            emit_symbolic_params(out, parsed, op, 1)?;

            // Assume requires hold pre-state (avoid vacuous pass).
            emit_kani_account_env_binding(out, op, "accounts", "    ");
            if let Some(full_guard) = util::collect_full_guard_with_account_env(
                op,
                false,
                util::handler_needs_account_env(op).then_some("accounts"),
            ) {
                let full_guard = util::rewrite_kani_pubkey_comparisons(&full_guard, op, parsed);
                out.push_str(&format!("    kani::assume({});\n", full_guard));
            }

            // Snapshot AFTER assumes — pre reflects the constrained
            // pre-state Kani explores.
            out.push_str("    let pre = s.clone();\n");

            let args = transition_call_args(
                op,
                util::handler_needs_account_env(op).then_some("accounts"),
            );
            out.push_str(&format!("    if {}(&mut s{}) {{\n", op.name, args));
            out.push_str("        let post = &s;\n");

            // CPI ensures-as-fact propagation.
            for call in &op.calls {
                let Some(iface) = parsed
                    .interfaces
                    .iter()
                    .find(|i| i.name == call.target_interface)
                else {
                    continue;
                };
                let Some(callee_handler) = iface
                    .handlers
                    .iter()
                    .find(|h| h.name == call.target_handler)
                else {
                    continue;
                };
                if callee_handler.ensures.is_empty() {
                    continue;
                }
                out.push_str(&format!(
                    "        // CPI ensures-as-fact ({}.{}):\n",
                    call.target_interface, call.target_handler,
                ));
                for callee_ens in &callee_handler.ensures {
                    let ensures_tree = callee_ens.tree.as_ref().expect(
                        "interface ensures tree is always populated by the chumsky adapter (#151/#156)",
                    );
                    let abstract_fields =
                        crate::cpi_substitute::scan_abstract_state_fields(ensures_tree);
                    let missing = crate::cpi_substitute::missing_state_binders(
                        &abstract_fields,
                        &call.state_binders,
                    );
                    if !missing.is_empty() {
                        rec.unsupported(
                            ObligationKind::CpiEnsures,
                            &op.name,
                            &format!("{}.{}", call.target_interface, call.target_handler),
                            UnsupportedReason::CpiMissingStateBinders,
                        );
                        out.push_str(&format!(
                            "        // `{}.{}` ensures skipped: missing `state_binders` for {}.\n",
                            call.target_interface,
                            call.target_handler,
                            missing.join(", "),
                        ));
                        continue;
                    }
                    let substituted = util::tree_render::render_rust(
                        &crate::cpi_substitute::substitute_callee_ensures_tree(
                            ensures_tree,
                            call,
                            callee_handler.result_binder.as_deref(),
                        ),
                        util::tree_render::RustCx::native(),
                    );
                    let substituted =
                        util::rewrite_kani_pubkey_comparisons(&substituted, op, parsed);
                    out.push_str(&format!("        kani::assume({});\n", substituted));
                }
            }

            // Math-exact form preferred: the assert evaluates on symbolic
            // post-state, so internal arithmetic must not overflow-panic
            // (issue #146).
            let ensures_src = if ensures.rust_expr_binary_math.is_empty() {
                &ensures.rust_expr_binary
            } else {
                &ensures.rust_expr_binary_math
            };
            let ensures_expr = util::rewrite_kani_pubkey_comparisons(ensures_src, op, parsed);
            out.push_str(&format!("        assert!({},\n", ensures_expr));
            out.push_str(&format!(
                "            \"ensures clause {} on {} violated by spec-translated transition\");\n",
                idx, op.name
            ));
            out.push_str("    }\n");
            out.push_str("}\n\n");
        }
    }

    Ok(())
}

/// Emit `#[kani::proof] fn verify_<handler>_(preserves|establishes)_<invariant>()`
/// per handler × invariant-clause pair (`op.invariants` ∪ `op.establishes`).
/// Skips invariants with missing/unsupported `rust_expr`. Pre-state zeroed
/// for init handlers, else symbolic; preserves assumes `<inv>(&s)` pre to
/// scope BMC to states where it already holds; establishes deliberately
/// skips that pre-assume (the handler must *make* it true regardless).
/// Then `if <handler>(&mut s, ...) { assert!(<inv>(&s)); }`.
pub(crate) fn emit_invariant_preservation_harnesses(
    out: &mut String,
    parsed: &ParsedSpec,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    use crate::rust_codegen_util as util;

    let handlers: Vec<&crate::check::ParsedHandler> = parsed.handlers.iter().collect();

    // Invariants referenced by at least one handler in this section.
    let linked_invs: Vec<&crate::check::ParsedInvariant> = parsed
        .invariants
        .iter()
        .filter(|i| {
            handlers
                .iter()
                .any(|h| h.invariants.contains(&i.name) || h.establishes.contains(&i.name))
        })
        .collect();

    if linked_invs.is_empty() {
        return Ok(());
    }

    let (state_fields, lifecycle) = resolve_account_view(parsed);
    let mutable = util::field_refs(state_fields);

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Invariant preservation — `invariant Name` on a handler asserts the named\n");
    out.push_str("// top-level invariant holds before AND after the handler runs. Each pair\n");
    out.push_str("// becomes its own BMC proof.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in &handlers {
        let pairs: Vec<(&String, bool)> = op
            .invariants
            .iter()
            .map(|n| (n, false))
            .chain(op.establishes.iter().map(|n| (n, true)))
            .collect();

        for (inv_name, is_establish) in pairs {
            let verb = if is_establish {
                "establishes"
            } else {
                "preserves"
            };
            let obligation_key = format!("{}_{}", verb, inv_name);
            let Some(inv) = linked_invs.iter().find(|i| &i.name == inv_name) else {
                continue;
            };
            // Skip missing/unsupported bodies (e.g. QEDGEN_UNSUPPORTED_QUANTIFIER).
            if inv
                .rust_expr
                .as_deref()
                .map(crate::check::rust_expr_is_unsupported)
                .unwrap_or(true)
            {
                rec.unsupported(
                    ObligationKind::InvariantPreservation,
                    &op.name,
                    &obligation_key,
                    UnsupportedReason::UnsupportedPredicateBody,
                );
                continue;
            }

            let is_init = op.pre_status.as_deref() == Some("Uninitialized");

            rec.emitted(
                ObligationKind::InvariantPreservation,
                &op.name,
                &obligation_key,
                &format!("verify_{}_{}_{}", op.name, verb, inv.name),
            );
            emit_proof_preamble(
                out,
                parsed,
                Some(op),
                &mutable,
                lifecycle,
                PreambleOpts {
                    harness_name: &format!("verify_{}_{}_{}", op.name, verb, inv.name),
                    unwind: 2,
                    solver: "cadical",
                    zeroed_init: is_init,
                    pre_status_assume: true,
                },
            );
            if !is_init && !is_establish {
                out.push_str(&format!("    kani::assume({}(&s));\n", inv.name));
            }

            emit_symbolic_params(out, parsed, op, 1)?;

            let args: String = op
                .takes_params
                .iter()
                .chain(op.abstract_binders.iter())
                .map(|(n, _)| format!(", {}", n))
                .collect();
            out.push_str(&format!("    if {}(&mut s{}) {{\n", op.name, args));
            out.push_str(&format!("        assert!({}(&s),\n", inv.name));
            out.push_str(&format!(
                "            \"invariant {} must hold after {}\");\n",
                inv.name, op.name
            ));
            out.push_str("    }\n");
            out.push_str("}\n\n");
        }
    }

    Ok(())
}
