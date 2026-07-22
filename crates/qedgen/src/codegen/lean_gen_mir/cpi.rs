use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// Emit CPI theorems, two halves:
///
/// 1. **Transfer-envelope theorems** — per `Stmt::TokenTransfer`, a
///    `def build_<handler>_transfer<suffix>` CPI constructor over the SPL
///    Token Transfer envelope plus a sibling `_correct` theorem closing by
///    `rfl`. Authorityless transfers skip the theorem and emit a
///    tracked-obligation comment — the 3-account envelope doesn't apply.
///
/// 2. **Call-site ensures-as-axiom theorems** — per `Stmt::Cpi`, one
///    theorem per declared `ensures` clause. Tier-1/2 callees (non-empty
///    `upstream.binary_hash` AND ensures) close via
///    `<Iface>.<method>.ensures_axiom_<idx>`; Tier-0 callees keep `:= by
///    sorry` — the `cpi_no_callee_ensures` lint surfaces them at check time.
///
/// Substitution flows through `cpi_substitute::substitute_callee_ensures_tree`
/// via a synthetic `ParsedCall`; the substituted tree renders under the
/// guard context, so caller args keep their `s.` receivers and mapped
/// callee state fields carry verbatim `pre.` / `post.` spellings.
///
/// Returns the pinned interface names referenced by call sites — the
/// caller decides which sibling `<Iface>.lean` modules to write and which
/// lakefile `require` directives to inject.
pub(super) fn emit_cpi_theorems(
    out: &mut String,
    mir: &Mir,
    rec: &mut ObligationRecorder,
) -> std::collections::BTreeSet<String> {
    use crate::mir::Stmt;

    let mut pinned_interfaces: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for h in &mir.handlers {
        let has_any_cpi = h
            .body
            .stmts
            .iter()
            .any(|s| matches!(s, Stmt::TokenTransfer { .. }) || matches!(s, Stmt::Cpi { .. }));
        if !has_any_cpi {
            continue;
        }

        // ---- (1) Transfer-envelope half ----
        let transfers: Vec<&Stmt> = h
            .body
            .stmts
            .iter()
            .filter(|s| matches!(s, Stmt::TokenTransfer { .. }))
            .collect();
        for (i, ts) in transfers.iter().enumerate() {
            let Stmt::TokenTransfer {
                from,
                to,
                amount,
                authority,
            } = *ts
            else {
                continue;
            };
            let suffix = if transfers.len() > 1 {
                format!("_{}", i)
            } else {
                String::new()
            };
            let build_name = safe_name(&format!("build_{}_transfer{}", h.name, suffix));
            let theorem_name = safe_name(&format!("{}_transfer{}_correct", h.name, suffix));

            let from_label = account_ref_label(from);
            let to_label = account_ref_label(to);
            out.push_str(&format!(
                "/-- {} transfer envelope: {} \u{2192} {}",
                h.name, from_label, to_label,
            ));
            let amount_lean = expr_lean(amount, tree_render::LeanCx::guard());
            if !amount_lean.is_empty() {
                out.push_str(&format!(" amount {}", amount_lean));
            }
            if let Some(auth) = authority {
                out.push_str(&format!(" authority {}", account_ref_label(auth)));
            }
            out.push_str(".\n");
            out.push_str("    Verifies CPI shape (program ID, account list, discriminator).\n");
            out.push_str("    Amount serialization and SPL Token execution are SDK/runtime\n");
            out.push_str("    trust per VERIFICATION_SCOPE.md. -/\n");

            // Authorityless transfers don't fit the 3-account SPL Token
            // envelope. Emit a structured comment instead of a theorem
            // so the obligation is tracked without inventing a proof
            // shape that doesn't match.
            if authority.is_none() {
                rec.unsupported(
                    ObligationKind::TransferEnvelope,
                    &h.name,
                    &format!("transfer{suffix}"),
                    UnsupportedReason::LeanTransferNoAuthority,
                );
                out.push_str(&format!(
                    "-- {} transfer{}: no authority declared; envelope theorem skipped.\n\n",
                    h.name, suffix,
                ));
                continue;
            }
            rec.emitted(
                ObligationKind::TransferEnvelope,
                &h.name,
                &format!("transfer{suffix}"),
                &theorem_name,
            );

            out.push_str(&format!(
                "def {} (from_pk to_pk authority_pk : Pubkey) : CpiInstruction :=\n",
                build_name
            ));
            out.push_str("  { programId := TOKEN_PROGRAM_ID\n");
            out.push_str("  , accounts :=\n");
            out.push_str("      [ \u{27e8}from_pk, false, true\u{27e9}\n");
            out.push_str("      , \u{27e8}to_pk, false, true\u{27e9}\n");
            out.push_str("      , \u{27e8}authority_pk, true, false\u{27e9}\n");
            out.push_str("      ]\n");
            out.push_str("  , data := DISC_TRANSFER }\n\n");

            out.push_str(&format!(
                "theorem {} (from_pk to_pk authority_pk : Pubkey) :\n",
                theorem_name
            ));
            out.push_str(&format!(
                "    let cpi := {} from_pk to_pk authority_pk\n",
                build_name
            ));
            out.push_str("    targetsProgram cpi TOKEN_PROGRAM_ID \u{2227}\n");
            out.push_str("    accountAt cpi 0 from_pk false true \u{2227}\n");
            out.push_str("    accountAt cpi 1 to_pk false true \u{2227}\n");
            out.push_str("    accountAt cpi 2 authority_pk true false \u{2227}\n");
            out.push_str("    hasDiscriminator cpi DISC_TRANSFER := by\n");
            out.push_str(&format!(
                "  unfold {} targetsProgram accountAt hasDiscriminator\n",
                build_name
            ));
            out.push_str("  exact \u{27e8}rfl, rfl, rfl, rfl, rfl\u{27e9}\n\n");
        }

        // ---- (2) Call-site ensures-as-axiom half ----
        let cpi_calls: Vec<&Stmt> = h
            .body
            .stmts
            .iter()
            .filter(|s| matches!(s, Stmt::Cpi { .. }))
            .collect();

        for (call_idx, cs) in cpi_calls.iter().enumerate() {
            let Stmt::Cpi {
                target,
                method,
                args,
                state_binders,
                result_binding,
            } = *cs
            else {
                continue;
            };

            // Resolve the callee through Mir.imports.
            let resolved = mir
                .imports
                .values()
                .filter_map(|imp| {
                    imp.interfaces
                        .get(&target.0)
                        .and_then(|i| i.methods.get(&method.0).map(|m| (imp, i, m)))
                })
                .next();
            let Some((import, _iface_decl, callee)) = resolved else {
                // Unresolved interface — lint surfaces this as
                // `[shape_only_cpi]`. Skip silently here.
                continue;
            };

            let pinned = handler_is_pinned_mir(import, callee);
            if pinned {
                pinned_interfaces.insert(target.0.clone());
            }

            // Marshal MIR data into a `ParsedCall` for the substitution helper.
            let synthetic_call =
                synthesize_parsed_call(target, method, args, state_binders, result_binding);

            let handler_params = param_sig_str(&h.params);

            for (ens_idx, ensures) in callee.ensures.iter().enumerate() {
                let ensures_tree = ensures.0.tree.as_ref().expect(
                    "interface ensures Expr.tree is always populated by the chumsky adapter (#151/#156)",
                );
                let substituted = tree_render::render_lean(
                    &crate::cpi_substitute::substitute_callee_ensures_tree(
                        ensures_tree,
                        &synthetic_call,
                        callee.result_binder.as_deref(),
                    ),
                    tree_render::LeanCx::guard(),
                );

                // Skip when state_binders are missing for abstract fields.
                let abstract_fields =
                    crate::cpi_substitute::scan_abstract_state_fields(ensures_tree);
                if !abstract_fields.is_empty() {
                    let missing = crate::cpi_substitute::missing_state_binders(
                        &abstract_fields,
                        &synthetic_call.state_binders,
                    );
                    if !missing.is_empty() {
                        if missing.len() == abstract_fields.len() {
                            out.push_str(&format!(
                                "-- `{}.{}` ensures #{} ({}): caller supplied no \
                                 `state_binders` for these abstract fields; ensures \
                                 not pulled into caller proof. Bind via \
                                 `state_binders {{ {} = state.<field> }}` to consume.\n",
                                target.0,
                                method.0,
                                ens_idx,
                                abstract_fields.join(", "),
                                abstract_fields[0],
                            ));
                        } else {
                            out.push_str(&format!(
                                "-- `{}.{}` ensures #{} ({}): caller supplied incomplete \
                                 `state_binders`; missing {}; ensures not pulled into caller proof. \
                                 Bind via `state_binders {{ {} = state.<field> }}` to consume.\n",
                                target.0,
                                method.0,
                                ens_idx,
                                abstract_fields.join(", "),
                                missing.join(", "),
                                missing[0],
                            ));
                        }
                        rec.unsupported(
                            ObligationKind::CpiEnsures,
                            &h.name,
                            &format!("{}.{}.call{}.post{}", target.0, method.0, call_idx, ens_idx),
                            UnsupportedReason::CpiMissingStateBinders,
                        );
                        continue;
                    }
                }
                let prefixed = substituted;
                let theorem_name = safe_name(&format!(
                    "{}_{}_{}_call_{}_post_{}",
                    h.name, target.0, method.0, call_idx, ens_idx,
                ));
                // Tier-0 `by sorry` theorems exist as theorems — recorded
                // emitted like the pinned axiom-discharged form.
                rec.emitted(
                    ObligationKind::CpiEnsures,
                    &h.name,
                    &format!("{}.{}.call{}.post{}", target.0, method.0, call_idx, ens_idx),
                    &theorem_name,
                );

                if pinned {
                    let axiom_qualified = format!(
                        "{}.{}.ensures_axiom_{}",
                        safe_name(&target.0),
                        safe_name(&method.0),
                        ens_idx,
                    );
                    let mut apply_args: Vec<String> = Vec::new();
                    let track_a = !abstract_fields.is_empty();
                    if track_a {
                        apply_args.push("pre".to_string());
                        apply_args.push("post".to_string());
                    }
                    // Per callee param: prefer the caller's argument
                    // (tree-rendered under the guard context), else the
                    // formal name. Parens around compound forms.
                    let subst: std::collections::HashMap<&str, String> = args
                        .iter()
                        .map(|a| {
                            let tree = a.value.tree.as_ref().expect(
                                "CallArg Expr.tree is always populated by the chumsky adapter (#151/#156)",
                            );
                            (
                                a.name.as_str(),
                                tree_render::render_lean(tree, tree_render::LeanCx::guard()),
                            )
                        })
                        .collect();
                    for (pn, _) in &callee.params {
                        let rendered_arg = subst
                            .get(pn.as_str())
                            .cloned()
                            .unwrap_or_else(|| pn.clone());
                        let needs_parens = rendered_arg.chars().any(|c| {
                            c.is_whitespace()
                                || c == '+'
                                || c == '-'
                                || c == '*'
                                || c == '/'
                                || c == '<'
                                || c == '>'
                        });
                        if needs_parens {
                            apply_args.push(format!("({})", rendered_arg));
                        } else {
                            apply_args.push(rendered_arg);
                        }
                    }
                    if track_a {
                        for field in &abstract_fields {
                            let caller_field = state_binders
                                .iter()
                                .find(|b| &b.callee_field == field)
                                .map(|b| caller_projection_to_field(&b.caller_projection))
                                .unwrap_or_else(|| field.clone());
                            apply_args.push(format!("(\u{00B7}.{})", caller_field));
                        }
                    }
                    let stance = if import.verified_pkg_root.is_some() {
                        "stance 2: discharged via imported callee proof"
                    } else {
                        "stance 1: discharged via Tier-1 binary-hash axiom; \
                         v3.0 will replace the axiom with an imported callee proof"
                    };
                    out.push_str(&format!(
                        "/-- {}.{}.ensures @ `{}` call #{} ({}). -/\n",
                        target.0, method.0, h.name, call_idx, stance,
                    ));
                    if track_a {
                        out.push_str(&format!(
                            "theorem {} (s : State) (pre post : State){} : {} :=\n",
                            theorem_name, handler_params, prefixed,
                        ));
                    } else {
                        out.push_str(&format!(
                            "theorem {} (s : State){} : {} :=\n",
                            theorem_name, handler_params, prefixed,
                        ));
                    }
                    if apply_args.is_empty() {
                        out.push_str(&format!("  {}\n\n", axiom_qualified));
                    } else {
                        out.push_str(&format!(
                            "  {} {}\n\n",
                            axiom_qualified,
                            apply_args.join(" "),
                        ));
                    }
                } else {
                    out.push_str(&format!(
                        "/-- {}.{}.ensures @ `{}` call #{} (stance 1: axiomatized via sorry; \
                         v3.0 will close via imported callee proofs). -/\n",
                        target.0, method.0, h.name, call_idx,
                    ));
                    out.push_str(&format!(
                        "theorem {} (s : State){} : {} := by sorry\n\n",
                        theorem_name, handler_params, prefixed,
                    ));
                }
            }
        }
    }

    pinned_interfaces
}

/// True iff the callee has a non-empty `upstream.binary_hash` pin AND
/// at least one `ensures` clause (MIR twin of `lean_sidecars::
/// handler_is_pinned` — the pin predicate is `lean_names::
/// binary_hash_is_pinned`).
pub(super) fn handler_is_pinned_mir(
    import: &crate::mir::ImportedSpecMir,
    callee: &crate::mir::InterfaceMethod,
) -> bool {
    !callee.ensures.is_empty()
        && crate::lean_names::binary_hash_is_pinned(
            import
                .upstream
                .as_ref()
                .and_then(|u| u.binary_hash.as_deref()),
        )
}

/// Build a label for an `AccountRef` suitable for doc-comment use.
pub(super) fn account_ref_label(r: &crate::mir::AccountRef) -> String {
    use crate::mir::AccountRef;
    match r {
        AccountRef::ByBinding(s) => s.clone(),
        AccountRef::SelfState => "self".to_string(),
    }
}

/// Extract the caller-side field name from a `StateBinder.caller_projection`.
/// Pilot scope: the path is always a single segment (`state.<ident>`
/// at the surface lowered to `Path::single("<ident>")`). Multi-segment
/// projections are reserved for v3.0 — pick the last segment as the
/// best approximation for the axiom-application slot.
pub(super) fn caller_projection_to_field(p: &crate::mir::Path) -> String {
    p.segments.last().cloned().unwrap_or_default()
}

/// Synthesize a `ParsedCall` from `Stmt::Cpi` data so the `cpi_substitute`
/// helper (which consumes parse-layer types) can run unchanged.
pub(super) fn synthesize_parsed_call(
    target: &crate::mir::InterfaceRef,
    method: &crate::mir::MethodRef,
    args: &[crate::mir::CallArg],
    state_binders: &[crate::mir::StateBinder],
    result_binding: &Option<crate::mir::Symbol>,
) -> crate::check::ParsedCall {
    crate::check::ParsedCall {
        target_interface: target.0.clone(),
        target_handler: method.0.clone(),
        args: args
            .iter()
            .map(|a| crate::check::ParsedCallArg {
                name: a.name.clone(),
                rust_expr: crate::rust_codegen_util::mir_expr_rust(&a.value),
                tree: a.value.tree.clone(),
            })
            .collect(),
        result_binding: result_binding.clone(),
        state_binders: state_binders
            .iter()
            .map(|b| crate::check::ParsedStateBinder {
                callee_field: b.callee_field.clone(),
                caller_field: caller_projection_to_field(&b.caller_projection),
            })
            .collect(),
    }
}
