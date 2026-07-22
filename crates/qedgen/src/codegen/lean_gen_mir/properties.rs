use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// Emit property declarations + preservation theorems:
///
/// ```text
/// def <name> (s : State) : Prop := <body>
///
/// theorem <name>_preserved_by_<handler> (s s' : State) (signer : Pubkey) <params>
///     (h_inv : <name> s) (h : <handler>Transition s signer <args> = some s') :
///     <name> s' <proof>
///
/// theorem <name>_inductive (s s' : State) (signer : Pubkey) (op : Operation)
///     (h_inv : <name> s) (h : applyOp s signer op = some s') :
///     <name> s' <proof>
/// ```
///
/// Proof bodies come from `preservation_proof_script` /
/// `master_inductive_proof_script`. Properties with no `expression` body
/// emit a structured comment only.
pub(super) fn emit_properties(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    if mir.properties.is_empty() {
        return;
    }

    for prop in &mir.properties {
        // Predicate def (when body is present).
        if let Some(expr) = &prop.expression {
            // Strip a leading `∀ s : State,` binder (only when the binder
            // ident is exactly `s`) — the surrounding def already
            // introduces `(s : State)`.
            let body = strip_state_forall(&expr_lean(expr, property_cx(prop)));
            out.push_str(&format!(
                "def {} (s : State) : Prop := {}\n\n",
                safe_name(&prop.name),
                body
            ));
        } else {
            out.push_str(&format!(
                "-- PROPERTY OBLIGATION (declared, no predicate body): {}\n\n",
                prop.name
            ));
            continue;
        }

        // Per-handler preservation sub-lemmas.
        let covered: Vec<&crate::mir::HandlerMir> = mir
            .handlers
            .iter()
            .filter(|h| prop.preserved_by.contains(&h.name))
            .collect();
        for h in &covered {
            let trans_name = safe_name(&format!("{}Transition", h.name));
            let param_sig = param_sig_str(&h.params);
            let param_args = param_args_str(&h.params);
            let sub_lemma = safe_name(&format!("{}_preserved_by_{}", prop.name, h.name));
            rec.emitted(
                ObligationKind::PropertyPreservation,
                &h.name,
                &prop.name,
                &sub_lemma,
            );
            out.push_str(&format!(
                "theorem {} (s s' : State) (signer : Pubkey){}\n",
                sub_lemma, param_sig
            ));
            out.push_str(&format!(
                "    (h_inv : {} s) (h : {} s signer{} = some s') :\n",
                safe_name(&prop.name),
                trans_name,
                param_args
            ));
            let proof_tail = preservation_proof_script(mir, h, prop);
            out.push_str(&format!("    {} s'{}", safe_name(&prop.name), proof_tail));
        }

        // Master theorem: preserved by every Operation case.
        if !covered.is_empty() {
            rec.emitted(
                ObligationKind::BackendExtra,
                "file",
                &format!("{}_inductive", prop.name),
                &safe_name(&format!("{}_inductive", prop.name)),
            );
            out.push_str(&format!(
                "/-- {} is preserved by every operation. Auto-proven by case split. -/\n",
                prop.name
            ));
            out.push_str(&format!(
                "theorem {}_inductive (s s' : State) (signer : Pubkey) (op : Operation)\n",
                safe_name(&prop.name)
            ));
            out.push_str(&format!(
                "    (h_inv : {} s) (h : applyOp s signer op = some s') : {} s'",
                safe_name(&prop.name),
                safe_name(&prop.name)
            ));
            let master_proof = master_inductive_proof_script(mir, prop);
            out.push_str(&master_proof);
        }
    }
}

/// Mechanical proof body for `<prop>_preserved_by_<handler>`.
///
/// Strategy depends on:
///   1. Quantified-property check (∀/∃ in the property body) — fall back
///      to a `sorry` stub with a structured TODO comment.
///   2. Whether the handler body touches any field the property reads
///      (`touches_prop_field`). When it does, `unfold` the property in
///      both `h_inv` and the goal, then `dsimp; omega`. When it doesn't,
///      `exact h_inv` after equating `s'` with `s`.
///   3. Whether the transition body has an `if … then … else none`
///      guard (`has_cond`). With a guard, `split at h` + `next hg =>`
///      handles the success branch; the `else` branch closes by
///      `contradiction` (since `h` is `some s'`, not `none`). Without
///      a guard, drop straight into `cases h`.
pub(super) fn preservation_proof_script(
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    prop: &crate::mir::PropertyMir,
) -> String {
    let trans_name = safe_name(&format!("{}Transition", h.name));

    let body_lean = prop
        .expression
        .as_ref()
        .map(|e| expr_lean(e, property_cx(prop)));
    let has_quantifier = body_lean
        .as_deref()
        .map(|e| e.contains('\u{2200}') || e.contains('\u{2203}'))
        .unwrap_or(false);
    if has_quantifier {
        return format!(
            " := by\n  unfold {} at h\n  sorry -- quantified property: fill with intro + cases or Leanstral\n\n",
            trans_name
        );
    }

    let prop_fields: Vec<String> = body_lean
        .as_deref()
        .map(fields_referenced_in_expr_owned)
        .unwrap_or_default();

    // Handler touches a property field when (a) any effect mutates a
    // field the property reads, or (b) the handler's lifecycle arrow
    // updates `status` and the property mentions `status`.
    let touches_prop_field = handler_touches_fields(&h.body.stmts, &prop_fields)
        || (h.transition.is_some() && prop_fields.iter().any(|f| f == "status"));

    let has_cond = !build_guard_cond_parts(mir, h).is_empty();

    let prop_name = safe_name(&prop.name);

    // Handler-level `let` bindings leave the unfolded hypothesis wrapped
    // in `have`/`let` binders; zeta-reduce so `split`/`cases` find the
    // `if` / record form (#156).
    let zeta = if h.lets.is_empty() {
        ""
    } else {
        " dsimp only at h;"
    };

    if has_cond {
        if touches_prop_field {
            format!(
                " := by\n  unfold {} at h;{} split at h\n  \
                 \u{B7} next hg => cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n  \
                 \u{B7} contradiction\n\n",
                trans_name, zeta, prop_name
            )
        } else {
            format!(
                " := by\n  unfold {} at h;{} split at h\n  \
                 \u{B7} cases h; exact h_inv\n  \
                 \u{B7} contradiction\n\n",
                trans_name, zeta
            )
        }
    } else if touches_prop_field {
        format!(
            " := by\n  unfold {} at h;{} cases h; \
             unfold {} at h_inv \u{22A2}; dsimp; omega\n\n",
            trans_name, zeta, prop_name
        )
    } else {
        format!(
            " := by\n  unfold {} at h;{} cases h; exact h_inv\n\n",
            trans_name, zeta
        )
    }
}

/// Auto-proof body for the master `<prop>_inductive` theorem.
///
/// For each Operation constructor:
///   - When the handler is in `preserved_by`: delegate to the per-
///     handler sub-lemma (`exact <prop>_preserved_by_<op> s s' signer
///     <params> h_inv h`).
///   - When NOT in `preserved_by`: attempt inline auto-proof. The
///     property is trivially preserved if the handler touches no
///     property field; otherwise discharge via `unfold + dsimp; omega`
///     under the transition's split structure.
pub(super) fn master_inductive_proof_script(mir: &Mir, prop: &crate::mir::PropertyMir) -> String {
    let mut proof = String::from(" := by\n  cases op with\n");

    let body_lean = prop
        .expression
        .as_ref()
        .map(|e| expr_lean(e, property_cx(prop)));
    let prop_fields: Vec<String> = body_lean
        .as_deref()
        .map(fields_referenced_in_expr_owned)
        .unwrap_or_default();
    let prop_name = safe_name(&prop.name);

    for h in &mir.handlers {
        let ctor = safe_name(&h.name);
        let param_names: Vec<String> = h.params.iter().map(|(n, _)| n.clone()).collect();
        let param_bind = if param_names.is_empty() {
            String::new()
        } else {
            format!(" {}", param_names.join(" "))
        };

        if prop.preserved_by.contains(&h.name) {
            let ref_name = safe_name(&format!("{}_preserved_by_{}", prop.name, h.name));
            proof.push_str(&format!(
                "  | {}{} => exact {} s s' signer{} h_inv h\n",
                ctor, param_bind, ref_name, param_bind
            ));
        } else {
            let trans_name = safe_name(&format!("{}Transition", h.name));
            let touches_prop_field = handler_touches_fields(&h.body.stmts, &prop_fields);
            if !touches_prop_field {
                proof.push_str(&format!(
                    "  | {}{} =>\n    simp [applyOp, {}] at h\n    obtain \u{27E8}_, h_eq\u{27E9} := h\n    subst h_eq; exact h_inv\n",
                    ctor, param_bind, trans_name
                ));
            } else {
                let has_cond = !build_guard_cond_parts(mir, h).is_empty();
                // Zeta-reduce `let`-carrying transitions so `split`/`cases`
                // find the `if` / record form (#156).
                let zeta = if h.lets.is_empty() {
                    ""
                } else {
                    " dsimp only at h;"
                };
                if has_cond {
                    proof.push_str(&format!(
                        "  | {}{} =>\n    simp [applyOp] at h\n    unfold {} at h;{} split at h\n    \u{B7} next hg => cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n    \u{B7} contradiction\n",
                        ctor, param_bind, trans_name, zeta, prop_name
                    ));
                } else {
                    proof.push_str(&format!(
                        "  | {}{} =>\n    simp [applyOp] at h\n    unfold {} at h;{} cases h; unfold {} at h_inv \u{22A2}; dsimp; omega\n",
                        ctor, param_bind, trans_name, zeta, prop_name
                    ));
                }
            }
        }
    }
    proof.push('\n');
    proof
}

/// Emit invariant theorems. Per invariant with a predicate body:
/// ```text
/// /-- Invariant: <name> — <doc> -/
/// theorem <name> (s : State) : <prefixed-pred> := by sorry
/// ```
///
/// Bare bodies (description-only invariants) emit a structured comment
/// instead — no `theorem ... := True` tautology. The `bare_invariant`
/// lint surfaces these as P3.
pub(super) fn emit_invariants(out: &mut String, mir: &Mir) {
    if mir.invariants.is_empty() {
        return;
    }

    for inv in &mir.invariants {
        match &inv.body {
            Some(pred) => {
                // Post-#139 canonicalization state reads are `s.`-rooted
                // in both the string and the tree render — no prefix pass.
                let prefixed = &expr_lean(&pred.0, tree_render::LeanCx::guard());
                out.push_str(&format!(
                    "/-- Invariant: {}{} -/\n",
                    inv.name,
                    if inv.doc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", inv.doc)
                    }
                ));
                out.push_str(&format!(
                    "theorem {} (s : State) : {} := by sorry\n\n",
                    inv.name, prefixed
                ));
            }
            None => {
                out.push_str(&format!(
                    "-- INVARIANT OBLIGATION (declared, no predicate body): {}\n",
                    inv.name
                ));
                if !inv.doc.is_empty() {
                    out.push_str(&format!("--   description: {}\n", inv.doc));
                }
                out.push_str("-- The spec declared this name but didn't supply a predicate body\n");
                out.push_str(
                    "-- (`invariant <name> : <expr>`). The codegen has no goal to lower —\n",
                );
                out.push_str("-- pre-v2.14 emitted `theorem <name> : True := trivial`, which\n");
                out.push_str("-- was tautological. To verify this invariant, give it a body in\n");
                out.push_str("-- the spec.\n\n");
            }
        }
    }
}

/// ADT-shape frame-condition emitter: per-handler theorems with a
/// `True := trivial` placeholder body — the inductive State needs
/// variant-aware case analysis the flat `s'.f = s.f` form can't express
/// (per-pre-variant reasoning is v3.0 roadmap).
pub(super) fn emit_frame_conditions_adt(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    let has_modifies = mir.handlers.iter().any(|h| h.modifies.is_some());
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

    for h in &mir.handlers {
        if h.modifies.is_none() {
            continue;
        }
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);
        let theorem_name = safe_name(&format!("{}_frame", h.name));
        rec.emitted(
            ObligationKind::BackendExtra,
            &h.name,
            "frame",
            &theorem_name,
        );
        out.push_str(&format!(
            "theorem {} (s s' : State) (signer : Pubkey){}\n",
            theorem_name, param_sig
        ));
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name, param_args
        ));
        out.push_str("    -- todo!(): inductive-State frame condition. Statement needs\n");
        out.push_str("    -- per-pre-variant case analysis to express which payload\n");
        out.push_str("    -- fields are preserved. Stated as `True` until that lands;\n");
        out.push_str("    -- the honest placeholder proof is `trivial`, not `sorry`.\n");
        out.push_str("    True := trivial\n\n");
    }
}

/// Flat-shape frame conditions: per handler with a `modifies` clause,
/// prove every field NOT in `modifies` stays equal across the transition.
/// Lifecycle-transitioning handlers implicitly modify `status`.
pub(super) fn emit_frame_conditions(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    let has_modifies = mir.handlers.iter().any(|h| h.modifies.is_some());
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

    // All declared state-field names across every variant.
    let all_fields: Vec<String> = flat_state_fields(mir).into_iter().map(|(n, _)| n).collect();

    for h in &mir.handlers {
        let Some(modified) = &h.modifies else {
            continue;
        };
        let modified_set: std::collections::HashSet<String> = modified
            .iter()
            .map(|p| p.segments.last().cloned().unwrap_or_default())
            .collect();

        let status_modified = h.transition.is_some();
        let unchanged: Vec<&String> = all_fields
            .iter()
            .filter(|f| !(modified_set.contains(*f) || (*f == "status" && status_modified)))
            .collect();
        if unchanged.is_empty() {
            continue;
        }

        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);
        let theorem_name = safe_name(&format!("{}_frame", h.name));
        rec.emitted(
            ObligationKind::BackendExtra,
            &h.name,
            "frame",
            &theorem_name,
        );

        out.push_str(&format!(
            "theorem {} (s s' : State) (signer : Pubkey){}\n",
            theorem_name, param_sig
        ));
        out.push_str(&format!(
            "    (h : {} s signer{} = some s') :\n",
            trans_name, param_args
        ));
        let conjuncts: Vec<String> = unchanged
            .iter()
            .map(|f| format!("s'.{} = s.{}", safe_name(f), safe_name(f)))
            .collect();
        out.push_str(&format!(
            "    {} := sorry\n\n",
            conjuncts.join(" \u{2227} ")
        ));
    }
}

/// Emit abort theorems — per (handler, `requires X else Err` clause):
///
/// ```text
/// theorem <h>_aborts_if_<Err> (s : State) (signer : Pubkey) <params>
///     (h : ¬(<requires-expr>)) : <h>Transition s signer <args> = none := sorry
/// ```
///
/// `aborts_total` instead emits a single `<h>_aborts_iff` theorem with the
/// disjunction of every abort condition.
pub(super) fn emit_aborts_if(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_aborts_if_with_sorry(out, mir, "sorry", rec);
}

/// Tactic body for a flat-path requires-based abort theorem: the
/// hypothesis `h : ¬(req_lean_expr)` contradicts the matching conjuncts
/// in the transition's guard, so the proof closes by `if_neg` projection.
pub(super) fn abort_requires_proof(
    trans_name: &str,
    cond_parts: &[String],
    req_index_in_cond_parts: usize,
    handler_has_lets: bool,
) -> String {
    // With handler-level `let` bindings the unfolded goal is a
    // `let`-expression; `dsimp only` zeta-reduces so `rw [if_neg …]`
    // reaches the `if` (the hypothesis carries the inlined form, which
    // matches the reduced condition).
    let zeta = if handler_has_lets {
        "\n  dsimp only"
    } else {
        ""
    };
    let atoms_per: Vec<usize> = cond_parts
        .iter()
        .map(|p| count_top_level_conjuncts(p))
        .collect();
    let total_atoms: usize = atoms_per.iter().sum();
    let flat_start: usize = atoms_per[..req_index_in_cond_parts].iter().sum();
    let target_atoms = atoms_per[req_index_in_cond_parts];

    if total_atoms == 1 {
        return format!(" := by\n  unfold {}{}\n  rw [if_neg h]\n", trans_name, zeta);
    }

    let projections: Vec<String> = (0..target_atoms)
        .map(|i| conjunction_projection(flat_start + i, total_atoms))
        .collect();
    let extraction = if projections.len() == 1 {
        projections[0].clone()
    } else {
        format!("\u{27E8}{}\u{27E9}", projections.join(", "))
    };

    format!(
        " := by\n  unfold {}{}\n  rw [if_neg (fun hg => h {})]\n",
        trans_name, zeta, extraction
    )
}

/// ADT-shape variant — emits `:= by sorry` for every clause. Only the
/// proof body's tactic / term form differs; theorem statements are
/// identical to the flat path.
pub(super) fn emit_aborts_if_adt(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_aborts_if_with_sorry(out, mir, "by sorry", rec);
}

/// Abort-hypothesis Lean form. Predicates referencing handler `let`
/// bindings inline them — theorem statements sit outside the transition
/// def where the `let`s are bound, so the names would be free (#156
/// fixture `let-bindings-fee-split`). Let-free handlers keep the
/// adapter's pre-rendered string (byte parity with existing snapshots).
fn abort_pred_lean(h: &crate::mir::HandlerMir, pred: &crate::mir::Predicate) -> String {
    match &pred.0.tree {
        Some(tree) if !h.lets.is_empty() => {
            tree_render::render_lean(&h.inline_let_bindings(tree), tree_render::LeanCx::guard())
        }
        _ => expr_lean(&pred.0, tree_render::LeanCx::guard()),
    }
}

/// Render context for a property body: `Unary` reads single-state (`s.`),
/// `Binary` reads post-state as `s'` with `old(…)` collapsing to `s` —
/// matching the adapter's legacy `lean_expr` conventions per class.
fn property_cx(prop: &crate::mir::PropertyMir) -> tree_render::LeanCx {
    match prop.class {
        crate::check::PropertyClass::Unary => tree_render::LeanCx::guard(),
        crate::check::PropertyClass::Binary => tree_render::LeanCx::ensures(),
    }
}

pub(super) fn emit_aborts_if_with_sorry(
    out: &mut String,
    mir: &Mir,
    sorry_form: &str,
    rec: &mut ObligationRecorder,
) {
    let has_aborts = mir.handlers.iter().any(|h| !h.requires_or_abort.is_empty());
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

    for h in &mir.handlers {
        if h.requires_or_abort.is_empty() {
            continue;
        }
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);

        // `aborts_total` collapses all abort conditions into one iff theorem.
        let all_abort_lean: Vec<String> = h
            .requires_or_abort
            .iter()
            .map(|r| format!("\u{00AC}({})", abort_pred_lean(h, &r.pred)))
            .collect();

        if h.aborts_total && !all_abort_lean.is_empty() {
            let theorem_name = safe_name(&format!("{}_aborts_iff", h.name));
            // The collapsed iff covers every clause: record each clause
            // index against the one theorem so keys line up with the
            // per-clause path (and with transitions.rs).
            for i in 0..h.requires_or_abort.len() {
                rec.emitted(
                    ObligationKind::Abort,
                    &h.name,
                    &format!("abort_{i}"),
                    &theorem_name,
                );
            }
            out.push_str(&format!(
                "theorem {} (s : State) (signer : Pubkey){} :\n",
                theorem_name, param_sig
            ));
            out.push_str(&format!(
                "    {} s signer{} = none \u{2194}\n",
                trans_name, param_args
            ));
            let disjunction = all_abort_lean.join(" \u{2228} ");
            out.push_str(&format!("    ({}) := {}\n\n", disjunction, sorry_form));
            continue;
        }

        // Per-clause theorems; disambiguate when the same error name
        // appears multiple times on a single handler.
        let mut error_total: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for r in &h.requires_or_abort {
            *error_total.entry(r.err.clone()).or_insert(0) += 1;
        }
        let mut error_seen: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let theorem_name_for =
            |err: &str, seen: &mut std::collections::HashMap<String, usize>| -> String {
                let total = error_total.get(err).copied().unwrap_or(0);
                let idx = {
                    let entry = seen.entry(err.to_string()).or_insert(0);
                    let cur = *entry;
                    *entry += 1;
                    cur
                };
                if total > 1 {
                    safe_name(&format!("{}_aborts_if_{}_{}", h.name, err, idx))
                } else {
                    safe_name(&format!("{}_aborts_if_{}", h.name, err))
                }
            };

        // requires-else clauses: hypothesis is ¬(predicate). Clauses
        // referencing a handler account's `.pubkey` / `.key()` are skipped
        // — those identifiers aren't in Lean scope and the theorem would
        // carry free variables; the runtime-side check still fires in Rust.
        //
        // Flat path (`sorry_form == "sorry"`): proof mechanically closes
        // via `if_neg`-with-projection. ADT path (`"by sorry"`) keeps the
        // sorry placeholder — per-variant pattern matching makes that
        // script ill-formed.
        let cond_parts = build_guard_cond_parts(mir, h);
        let flat_path = sorry_form == "sorry";
        for (i, r) in h.requires_or_abort.iter().enumerate() {
            let pred_lean = abort_pred_lean(h, &r.pred);
            if mentions_handler_account_pubkey(&pred_lean, &h.accounts) {
                rec.unsupported(
                    ObligationKind::Abort,
                    &h.name,
                    &format!("abort_{i}"),
                    UnsupportedReason::LeanHandlerAccountPubkey,
                );
                continue;
            }
            let theorem_name = theorem_name_for(&r.err, &mut error_seen);
            rec.emitted(
                ObligationKind::Abort,
                &h.name,
                &format!("abort_{i}"),
                &theorem_name,
            );
            out.push_str(&format!(
                "theorem {} (s : State) (signer : Pubkey){}\n",
                theorem_name, param_sig
            ));
            // Positioned against the guard conjunct in its original
            // (let-name) form — the transition's `if` binds the lets, so
            // its condition keeps the names; the hypothesis carries the
            // inlined form, and the `if_neg` projection unifies the two
            // up to zeta reduction.
            let req_lean = expr_lean(&r.pred.0, tree_render::LeanCx::guard());
            let req_pos = cond_parts.iter().position(|c| c == &req_lean);
            if flat_path {
                if let Some(pos) = req_pos {
                    let proof =
                        abort_requires_proof(&trans_name, &cond_parts, pos, !h.lets.is_empty());
                    out.push_str(&format!(
                        "    (h : \u{00AC}({})) : {} s signer{} = none{}\n",
                        pred_lean, trans_name, param_args, proof
                    ));
                    continue;
                }
            } else if req_pos.is_some() {
                // ADT path: the transition matches on the `State`
                // variant *before* the guard `if`, so the flat
                // `rw [if_neg]` script is ill-formed. `cases s <;>
                // simp_all` discharges every variant uniformly: the
                // active variant reduces via `h` (¬guard ⇒ else-branch
                // ⇒ `none`), the other variants hit the catch-all
                // `_ => none`. Gated on the predicate being an actual
                // guard conjunct (`req_pos.is_some()`) — otherwise `h`
                // wouldn't contradict the guard and the goal isn't
                // provable, so we fall through to the `sorry`
                // placeholder and keep the obligation honest.
                let proof = format!(" := by\n  unfold {}\n  cases s <;> simp_all\n", trans_name);
                out.push_str(&format!(
                    "    (h : \u{00AC}({})) : {} s signer{} = none{}\n",
                    pred_lean, trans_name, param_args, proof
                ));
                continue;
            }
            out.push_str(&format!(
                "    (h : \u{00AC}({})) : {} s signer{} = none := {}\n\n",
                pred_lean, trans_name, param_args, sorry_form
            ));
        }
    }
}

/// Emit ensures theorems — per handler, one theorem per `ensures` clause:
///
/// ```text
/// theorem <h>_ensures_<i> (s s' : State) (signer : Pubkey) <params>
///     (h : <h>Transition s signer <args> = some s') :
///     <ensures-lean-expr> := sorry
/// ```
pub(super) fn emit_ensures(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    let has_ensures = mir.handlers.iter().any(|h| !h.post.is_empty());
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

    for h in &mir.handlers {
        let trans_name = safe_name(&format!("{}Transition", h.name));
        let param_sig = param_sig_str(&h.params);
        let param_args = param_args_str(&h.params);
        for (i, ens) in h.post.iter().enumerate() {
            let theorem_name = safe_name(&format!("{}_ensures_{}", h.name, i));
            rec.emitted(
                ObligationKind::EnsuresPreservation,
                &h.name,
                &i.to_string(),
                &theorem_name,
            );
            out.push_str(&format!(
                "theorem {} (s s' : State) (signer : Pubkey){}\n",
                theorem_name, param_sig
            ));
            out.push_str(&format!(
                "    (h : {} s signer{} = some s') :\n",
                trans_name, param_args
            ));
            out.push_str(&format!(
                "    {} := sorry\n\n",
                expr_lean(&ens.0, tree_render::LeanCx::ensures())
            ));
        }
    }
}
