use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// Emit a transition per handler under the multi-variant ADT shape:
/// `match s with | .Pre <bindings> => … | _ => none`, the arm constructing
/// `.Post <args>` from effects + pre-variant bindings. Post-variant fields
/// not derivable from the spec (effects, auth, carry-over) fall back to
/// type defaults with a `todo!()` comment.
pub(super) fn emit_transitions_adt(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    for h in &mir.handlers {
        emit_handler_transition_adt(out, mir, h, rec);
    }
}

pub(super) fn emit_handler_transition_adt(
    out: &mut String,
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    rec: &mut ObligationRecorder,
) {
    use crate::mir::Stmt;

    let trans_name = safe_name(&format!("{}Transition", h.name));
    let param_sig = param_sig_str(&h.params);

    out.push_str(&format!(
        "def {} (s : State) (signer : Pubkey){} : Option State :=\n",
        trans_name, param_sig
    ));

    let Some((pre_name, post_name)) = h.transition.clone() else {
        out.push_str(
            "  -- todo!(): handler has no declared pre-variant; emitting structural rejection.\n",
        );
        out.push_str("  none\n\n");
        return;
    };
    let pre = match mir.state.variants.iter().find(|v| v.tag == pre_name) {
        Some(p) => p,
        None => {
            out.push_str(&format!(
                "  -- todo!(): unknown pre-variant `{}` in spec\n  none\n\n",
                pre_name
            ));
            return;
        }
    };
    let post = match mir.state.variants.iter().find(|v| v.tag == post_name) {
        Some(p) => p,
        None => {
            out.push_str(&format!(
                "  -- todo!(): unknown post-variant `{}` in spec\n  none\n\n",
                post_name
            ));
            return;
        }
    };

    // Pre-variant pattern with field-name bindings.
    let pre_pat = if pre.fields.is_empty() {
        format!(".{}", pre.tag)
    } else {
        let bindings: Vec<String> = pre.fields.iter().map(|f| safe_name(&f.name)).collect();
        format!(".{} {}", pre.tag, bindings.join(" "))
    };

    // Auth alias: when `auth <who>` is not a pre-variant field, bind
    // `who := signer` so downstream references resolve. When it IS a
    // pre field, the variant-arm binding scopes it and we emit a
    // signer-equality check instead.
    let mut pre_let_lines: Vec<String> = Vec::new();
    let mut cond_parts: Vec<String> = Vec::new();
    if let Some(auth_name) = handler_auth_name(h) {
        let pre_has_who = pre.fields.iter().any(|f| f.name == auth_name);
        if pre_has_who {
            cond_parts.push(format!("signer = {}", safe_name(&auth_name)));
        } else {
            pre_let_lines.push(format!("    let {} := signer", safe_name(&auth_name)));
        }
    }

    // Handler-level `let` bindings — bound inside the pre-variant arm so
    // guards and effect parts can reference them (#156).
    for (name, rhs) in &h.lets {
        pre_let_lines.push(format!(
            "    let {} := {}",
            safe_name(name),
            expr_lean(rhs, tree_render::LeanCx::guard())
        ));
    }

    // User-declared requires (`pre` + `requires_or_abort`, combined so the
    // original spec ordering survives). Clauses mentioning a handler-account
    // `.pubkey` / `.key()` projection are filtered: those identifiers have
    // no Lean scope and would leave free variables in the statement.
    for (i, p) in h.pre.iter().enumerate() {
        let lean = expr_lean(&p.0, tree_render::LeanCx::guard());
        if mentions_handler_account_pubkey(&lean, &h.accounts) {
            rec.unsupported(
                ObligationKind::TransitionGuard,
                &h.name,
                &format!("pre_{i}"),
                UnsupportedReason::LeanHandlerAccountPubkey,
            );
            continue;
        }
        cond_parts.push(lean);
    }
    for (i, r) in h.requires_or_abort.iter().enumerate() {
        let lean = expr_lean(&r.pred.0, tree_render::LeanCx::guard());
        if mentions_handler_account_pubkey(&lean, &h.accounts) {
            rec.unsupported(
                ObligationKind::Abort,
                &h.name,
                &format!("abort_{i}"),
                UnsupportedReason::LeanHandlerAccountPubkey,
            );
            continue;
        }
        cond_parts.push(lean);
    }

    // Conditional effects (`effect { match … }`) lower to `Stmt::Branch`.
    // Render a nested Lean `match` on the scrutinee so the ADT transition
    // applies exactly one arm — the per-arm analogue of the flat-state
    // `emit_handler_transition` (applying the *union* of arms unconditionally
    // is semantically wrong). Returns early; the unconditional tail below
    // handles the no-branch case.
    if let Some((scrutinee, arms, default)) = h.body.stmts.iter().find_map(|st| match st {
        Stmt::Branch {
            scrutinee,
            arms,
            default,
        } => Some((scrutinee, arms, default)),
        _ => None,
    }) {
        emit_adt_branch(
            out,
            mir,
            h,
            pre,
            post,
            &pre_pat,
            &pre_let_lines,
            &cond_parts,
            scrutinee,
            arms,
            default,
        );
        return;
    }

    // Effect-derived bound checks: only `CheckedAdd`/`CheckedSub` gain
    // bounds (`Wrap*`/`Sat*` handle the boundary without aborting); the
    // field's pre-variant type picks the bound (unsigned add → overflow,
    // unsigned sub → underflow, signed skips).
    cond_parts.extend(adt_bound_conds(mir, pre, &h.body.stmts));

    // Effect map → post-variant constructor (carry-over for unmentioned
    // matching fields; type defaults for the rest).
    let effect_map = adt_effect_map(mir, &h.body.stmts);
    let (post_ctor, unconstrained) = adt_post_ctor(pre, post, h, &effect_map);

    if !unconstrained.is_empty() {
        out.push_str(&format!(
            "  -- todo!(): post-variant `{}` has unconstrained field(s) not derivable from spec: {}\n",
            post.tag,
            unconstrained.join(", ")
        ));
        out.push_str(
            "  -- Using type defaults; add effects or handler params to constrain these.\n",
        );
    }

    out.push_str("  match s with\n");
    let let_block: String = if pre_let_lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", pre_let_lines.join("\n"))
    };
    if cond_parts.is_empty() {
        if pre_let_lines.is_empty() {
            out.push_str(&format!("  | {} => some ({})\n", pre_pat, post_ctor));
        } else {
            out.push_str(&format!("  | {} =>\n", pre_pat));
            out.push_str(&let_block);
            out.push_str(&format!("    some ({})\n", post_ctor));
        }
    } else {
        let if_cond = cond_parts
            .iter()
            .map(|p| paren_low_prec(p))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        out.push_str(&format!("  | {} =>\n", pre_pat));
        if !pre_let_lines.is_empty() {
            out.push_str(&let_block);
        }
        out.push_str(&format!(
            "    if {} then some ({}) else none\n",
            if_cond, post_ctor
        ));
    }
    out.push_str("  | _ => none\n\n");
}

/// Effect-derived overflow/underflow bound conjuncts for one ADT statement
/// list — the per-arm analogue used by both the unconditional ADT path and
/// `emit_adt_branch`. Field types come from the pre-variant; only
/// `CheckedAdd`/`CheckedSub` gain bounds (`Wrap*`/`Sat*` saturate/wrap).
/// The #148 effect-RHS tree guards (`effect_tree_bound_conds`) do NOT
/// apply here: ADT arms bind fields as bare pattern binders, while the
/// tree renderer emits `s.<field>` receivers — the guards would carry
/// out-of-scope identifiers. Extend when the ADT renderer grows a
/// binder-aware render context.
pub(super) fn adt_bound_conds(
    mir: &Mir,
    pre: &crate::mir::StateVariant,
    stmts: &[crate::mir::Stmt],
) -> Vec<String> {
    use crate::mir::Stmt;
    let mut conds: Vec<String> = Vec::new();
    for stmt in stmts {
        match stmt {
            Stmt::CheckedAdd { path, delta, .. } => {
                let stripped = strip_variant_prefix(path, mir);
                if let Some(ty) = pre
                    .fields
                    .iter()
                    .find(|f| f.name == stripped)
                    .map(|f| &f.ty)
                {
                    if let Some(max) = ty_max_const(ty) {
                        conds.push(format!(
                            "{} + {} \u{2264} {}",
                            safe_name(&stripped),
                            effect_rhs_lean(delta),
                            max
                        ));
                    }
                }
            }
            Stmt::CheckedSub { path, delta, .. } => {
                let stripped = strip_variant_prefix(path, mir);
                if let Some(ty) = pre
                    .fields
                    .iter()
                    .find(|f| f.name == stripped)
                    .map(|f| &f.ty)
                {
                    if !matches!(ty, crate::mir::Ty::I64 | crate::mir::Ty::I128) {
                        conds.push(format!(
                            "{} \u{2264} {}",
                            effect_rhs_lean(delta),
                            safe_name(&stripped)
                        ));
                    }
                }
            }
            // No bound to check.
            Stmt::Assign { .. }
            | Stmt::WrapAdd { .. }
            | Stmt::WrapSub { .. }
            | Stmt::SatAdd { .. }
            | Stmt::SatSub { .. }
            | Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => {}
        }
    }
    conds
}

/// Effect map for an ADT statement list, keyed by stripped field name →
/// (op, Lean RHS). Account-binding `.pubkey` assignments (no Lean scope)
/// are skipped. `set`/`add`/`sub` mirror the flat-state op set.
pub(super) fn adt_effect_map(
    mir: &Mir,
    stmts: &[crate::mir::Stmt],
) -> std::collections::HashMap<String, (&'static str, String)> {
    use crate::mir::Stmt;
    let mut effect_map: std::collections::HashMap<String, (&'static str, String)> =
        std::collections::HashMap::new();
    for stmt in stmts {
        match stmt {
            Stmt::Assign { path, rhs } => {
                if is_account_pubkey_ref(&crate::rust_codegen_util::mir_expr_rust(rhs)) {
                    continue;
                }
                effect_map.insert(
                    strip_variant_prefix(path, mir),
                    ("set", effect_rhs_lean(rhs)),
                );
            }
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => {
                effect_map.insert(
                    strip_variant_prefix(path, mir),
                    ("add", effect_rhs_lean(delta)),
                );
            }
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => {
                effect_map.insert(
                    strip_variant_prefix(path, mir),
                    ("sub", effect_rhs_lean(delta)),
                );
            }
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => {}
        }
    }
    effect_map
}

/// Post-variant constructor for the ADT transition from an effect map.
/// `add`/`sub` reference the bound pre-field name; unmentioned fields carry
/// over when the pre-variant has a same-typed field, else fall to a type
/// default (collected into the returned `unconstrained` list).
pub(super) fn adt_post_ctor(
    pre: &crate::mir::StateVariant,
    post: &crate::mir::StateVariant,
    h: &crate::mir::HandlerMir,
    effect_map: &std::collections::HashMap<String, (&'static str, String)>,
) -> (String, Vec<String>) {
    let mut unconstrained: Vec<String> = Vec::new();
    let auth_who = handler_auth_name(h);
    let post_args: Vec<String> = post
        .fields
        .iter()
        .map(|f| {
            if let Some((kind, value)) = effect_map.get(&f.name) {
                return match *kind {
                    "add" => format!("({} + {})", safe_name(&f.name), value),
                    "sub" => format!("({} - {})", safe_name(&f.name), value),
                    _ => value.clone(),
                };
            }
            if let Some(ref who) = auth_who {
                if *who == f.name && matches!(&f.ty, crate::mir::Ty::Pubkey) {
                    return safe_name(who);
                }
            }
            if pre.fields.iter().any(|p| p.name == f.name && p.ty == f.ty) {
                return safe_name(&f.name);
            }
            unconstrained.push(f.name.clone());
            ty_default_literal(&f.ty).to_string()
        })
        .collect();

    let post_ctor = if post_args.is_empty() {
        format!(".{}", post.tag)
    } else {
        format!(".{} {}", post.tag, post_args.join(" "))
    };
    (post_ctor, unconstrained)
}

/// Render a conditional-effect (`Stmt::Branch`) ADT transition as a nested
/// Lean `match` on the scrutinee — each arm builds the post-variant from
/// THAT arm's effects, wrapped in the arm's own bound guards so an untaken
/// arm never aborts. The per-arm analogue of the flat-state branch path.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_adt_branch(
    out: &mut String,
    mir: &Mir,
    h: &crate::mir::HandlerMir,
    pre: &crate::mir::StateVariant,
    post: &crate::mir::StateVariant,
    pre_pat: &str,
    pre_let_lines: &[String],
    cond_parts: &[String],
    scrutinee: &crate::mir::BranchScrutinee,
    arms: &[crate::mir::BranchArm],
    default: &Option<crate::mir::Block>,
) {
    let scrutinee_lean = match scrutinee {
        crate::mir::BranchScrutinee::Match(e) => expr_lean(e, tree_render::LeanCx::guard()),
        crate::mir::BranchScrutinee::Predicate(p) => expr_lean(&p.0, tree_render::LeanCx::guard()),
    };

    // One arm → `some (<post-ctor from this arm's effects>)`, guarded by the
    // arm's own overflow/underflow bounds (an untaken arm must not abort).
    let arm_line = |stmts: &[crate::mir::Stmt]| -> (String, Vec<String>) {
        let effect_map = adt_effect_map(mir, stmts);
        let (post_ctor, unconstrained) = adt_post_ctor(pre, post, h, &effect_map);
        let bounds = adt_bound_conds(mir, pre, stmts);
        let line = if bounds.is_empty() {
            format!("some ({})", post_ctor)
        } else {
            let joined = bounds
                .iter()
                .map(|c| paren_low_prec(c))
                .collect::<Vec<_>>()
                .join(" \u{2227} ");
            format!("if {} then some ({}) else none", joined, post_ctor)
        };
        (line, unconstrained)
    };

    // Render every arm first so unconstrained-field todos surface above the
    // match (matching the non-branch path's comment placement).
    let mut rendered: Vec<(String, String)> = Vec::new();
    let mut unconstrained: Vec<String> = Vec::new();
    for arm in arms {
        let pat = arm
            .pattern
            .as_ref()
            .map(|p| expr_lean(p, tree_render::LeanCx::guard()))
            .unwrap_or_else(|| "_".to_string());
        let (line, mut u) = arm_line(&arm.block.stmts);
        unconstrained.append(&mut u);
        rendered.push((pat, line));
    }
    let default_line = match default {
        Some(block) => {
            let (line, mut u) = arm_line(&block.stmts);
            unconstrained.append(&mut u);
            line
        }
        // No wildcard arm in the spec — untaken scrutinee values leave the
        // state unchanged (mirrors the Rust `_ => {}`).
        None => {
            let (line, mut u) = arm_line(&[]);
            unconstrained.append(&mut u);
            line
        }
    };

    if !unconstrained.is_empty() {
        unconstrained.sort();
        unconstrained.dedup();
        out.push_str(&format!(
            "  -- todo!(): post-variant `{}` has unconstrained field(s) not derivable from spec: {}\n",
            post.tag,
            unconstrained.join(", ")
        ));
        out.push_str(
            "  -- Using type defaults; add effects or handler params to constrain these.\n",
        );
    }

    out.push_str("  match s with\n");
    out.push_str(&format!("  | {} =>\n", pre_pat));
    for line in pre_let_lines {
        out.push_str(line);
        out.push('\n');
    }
    // With outer guards (`requires`/auth) the scrutinee match nests under
    // `if … then … else none`; without them it is the arm body directly.
    let match_indent = if cond_parts.is_empty() {
        "    "
    } else {
        let joined = cond_parts
            .iter()
            .map(|p| paren_low_prec(p))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        out.push_str(&format!("    if {} then\n", joined));
        "      "
    };
    out.push_str(&format!("{}match {} with\n", match_indent, scrutinee_lean));
    for (pat, line) in &rendered {
        out.push_str(&format!("{}| {} => {}\n", match_indent, pat, line));
    }
    out.push_str(&format!("{}| _ => {}\n", match_indent, default_line));
    if !cond_parts.is_empty() {
        out.push_str("    else none\n");
    }
    out.push_str("  | _ => none\n\n");
}

// ----------------------------------------------------------------------
// sBPF assembly renderer. Reads `ParsedSpec` directly: the assembly domain
// (instructions / layouts / guards over `executeFn`) has no representation
// in the state-machine `Stmt` IR, and Lean is the only backend rendering
// sBPF (Kani/proptest skip it). Output gated by `tests/sbpf_lean_parity.rs`.
// ----------------------------------------------------------------------
/// Emit one transition function per handler:
///
/// ```text
/// def <name>Transition (s : State) (signer : Pubkey) <params> : Option State :=
///   let <auth_alias> := signer  -- when `auth <who>` isn't a state field
///   if <require-or-abort-conjunction> then
///     some { s with <assigns>... }
///   else
///     none
/// ```
///
/// `Stmt::RequireOrAbort` lowers into the if-condition; the Assign / Add /
/// Sub family into the `{ s with ... }` record update. `TokenTransfer` and
/// `Cpi` don't affect local state — separate CPI theorems discharge them.
pub(super) fn emit_transitions(out: &mut String, mir: &Mir) {
    for h in &mir.handlers {
        emit_handler_transition(out, mir, h);
    }
}

pub(super) fn emit_handler_transition(out: &mut String, mir: &Mir, h: &crate::mir::HandlerMir) {
    use crate::mir::Stmt;

    let trans_name = safe_name(&format!("{}Transition", h.name));
    let param_sig = param_sig_str(&h.params);

    out.push_str(&format!(
        "def {} (s : State) (signer : Pubkey){} : Option State :=\n",
        trans_name, param_sig
    ));

    let conds = build_guard_cond_parts(mir, h);

    // Auth alias `let <who> := signer` only when `who` is NOT a state
    // field (otherwise the guard conjunct already pins the relationship
    // and an alias would shadow the field name).
    if let Some(who) = handler_auth_name(h) {
        let state_fields = flat_state_fields(mir);
        let who_is_state_field = state_fields.iter().any(|(n, _)| n == &who);
        if !who_is_state_field {
            out.push_str(&format!("  let {} := signer\n", safe_name(&who)));
        }
    }

    // Handler-level `let` bindings — bound before the guard so both the
    // require conjunction and the effect parts can reference them.
    // Without these the transition references the names free and the
    // module doesn't elaborate (#156 fixture `let-bindings-fee-split`).
    for (name, rhs) in &h.lets {
        out.push_str(&format!(
            "  let {} := {}\n",
            safe_name(name),
            expr_lean(rhs, tree_render::LeanCx::guard())
        ));
    }

    // Lifecycle promotion + ghost updates apply on every exit path
    // (appended to the flat `{ s with … }` parts, or to every arm of a
    // conditional-effect `match`).
    let mut common_parts: Vec<String> = Vec::new();
    // Lifecycle promotion: emit `status := .<post>` when the handler
    // declares a transition arrow — but only when the lifecycle is real
    // (≥ 2 states); single-lifecycle specs skip the marker (issue #43).
    if let Some((_, post)) = &h.transition {
        if mir.state.lifecycle_states.len() >= 2 {
            common_parts.push(format!("status := .{}", safe_name(post)));
        }
    }
    // Ghost field updates: a ghost with an `on <this handler>` clause
    // assigns alongside the normal effects; ghosts without one stay
    // unchanged (the `{ s with … }` update preserves unmentioned fields —
    // the frame condition).
    for ghost in &mir.ghosts {
        if let Some(val) = ghost.updates.get(&h.name) {
            common_parts.push(format!(
                "{} := {}",
                safe_name(&ghost.name),
                expr_lean(val, tree_render::LeanCx::guard())
            ));
        }
    }

    let some_body = |effect_parts: Vec<String>| -> String {
        let parts: Vec<String> = effect_parts
            .into_iter()
            .chain(common_parts.iter().cloned())
            .collect();
        if parts.is_empty() {
            "some s".to_string()
        } else {
            format!("some {{ s with {} }}", parts.join(", "))
        }
    };

    // Conditional effects: `effect { match … }` lowers to `Stmt::Branch`;
    // render a real Lean `match` so the model applies exactly one arm
    // (applying the *union* of arms unconditionally is semantically wrong).
    let branch = h.body.stmts.iter().find_map(|st| match st {
        Stmt::Branch {
            scrutinee,
            arms,
            default,
        } => Some((scrutinee, arms, default)),
        _ => None,
    });

    if let Some((scrutinee, arms, default)) = branch {
        let scrutinee_lean = match scrutinee {
            crate::mir::BranchScrutinee::Match(e) => expr_lean(e, tree_render::LeanCx::guard()),
            crate::mir::BranchScrutinee::Predicate(p) => {
                expr_lean(&p.0, tree_render::LeanCx::guard())
            }
        };
        // Per-arm: overflow/underflow bound guards for the arm's own
        // effects move *inside* the arm (an untaken arm's bounds must
        // not abort the transition), then the arm's record update.
        let arm_line = |block: &crate::mir::Block| -> String {
            let body = some_body(flat_effect_with_parts(&block.stmts));
            let bounds = flat_effect_bound_conds(mir, &block.stmts, &conds);
            if bounds.is_empty() {
                body
            } else {
                let joined = bounds
                    .iter()
                    .map(|c| paren_low_prec(c))
                    .collect::<Vec<_>>()
                    .join(" \u{2227} ");
                format!("if {} then {} else none", joined, body)
            }
        };
        let indent = if conds.is_empty() { "  " } else { "    " };
        let mut match_block = String::new();
        match_block.push_str(&format!("{}match {} with\n", indent, scrutinee_lean));
        for arm in arms {
            let pat = arm
                .pattern
                .as_ref()
                .map(|p| expr_lean(p, tree_render::LeanCx::guard()))
                .unwrap_or_else(|| "_".to_string());
            match_block.push_str(&format!(
                "{}| {} => {}\n",
                indent,
                pat,
                arm_line(&arm.block)
            ));
        }
        let default_line = match default {
            Some(block) => arm_line(block),
            // No wildcard arm in the spec — untaken scrutinee values
            // leave the state unchanged (mirrors the Rust `_ => {}`).
            None => some_body(Vec::new()),
        };
        let mut full = match_block;
        full.push_str(&format!("{}| _ => {}\n", indent, default_line));

        if conds.is_empty() {
            out.push_str(&full);
            out.push('\n');
        } else {
            let joined = conds
                .iter()
                .map(|c| paren_low_prec(c))
                .collect::<Vec<_>>()
                .join(" \u{2227} ");
            out.push_str(&format!("  if {} then\n", joined));
            out.push_str(&full);
            out.push_str("  else none\n\n");
        }
        return;
    }

    let then_body = some_body(flat_effect_with_parts(&h.body.stmts));

    if conds.is_empty() {
        out.push_str(&format!("  {}\n\n", then_body));
    } else {
        let joined = conds
            .iter()
            .map(|c| paren_low_prec(c))
            .collect::<Vec<_>>()
            .join(" \u{2227} ");
        out.push_str(&format!("  if {} then\n", joined));
        out.push_str(&format!("    {}\n", then_body));
        out.push_str("  else none\n\n");
    }
}

/// Assign / Add / Sub family → `{ s with … }` record-update parts for a
/// flat-state transition. Shared by the unconditional effect path and
/// the per-arm `Stmt::Branch` rendering.
pub(super) fn flat_effect_with_parts(stmts: &[crate::mir::Stmt]) -> Vec<String> {
    use crate::mir::Stmt;
    let mut with_parts: Vec<String> = Vec::new();
    for stmt in stmts {
        match stmt {
            Stmt::Assign { path, rhs } => {
                // Drop `<field> := <account_binding>.pubkey` — account-
                // binding pubkey refs have no Lean scope.
                if is_account_pubkey_ref(&crate::rust_codegen_util::mir_expr_rust(rhs)) {
                    continue;
                }
                // Tree-native RHS (#151 Slice 2): the resolved tree knows
                // a state read from a param from a literal — the legacy
                // string path re-derived that from shape heuristics.
                with_parts.push(format!(
                    "{} := {}",
                    safe_name(&path_field_name(path)),
                    effect_rhs_lean(rhs)
                ));
            }
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => {
                let f = safe_name(&path_field_name(path));
                let d = effect_rhs_lean(delta);
                with_parts.push(format!("{} := s.{} + {}", f, f, d));
            }
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => {
                let f = safe_name(&path_field_name(path));
                let d = effect_rhs_lean(delta);
                with_parts.push(format!("{} := s.{} - {}", f, f, d));
            }
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => {}
        }
    }
    with_parts
}

/// Underflow guard conjuncts (`<delta> ≤ s.<field>`, unsigned fields
/// only) for every sub-kind statement in `stmts` — item 3 of
/// `build_guard_cond_parts`, shared with the per-arm
/// `flat_effect_bound_conds`.
pub(super) fn sub_underflow_conds(mir: &Mir, stmts: &[crate::mir::Stmt]) -> Vec<String> {
    use crate::mir::Stmt;
    let state_fields = flat_state_fields(mir);
    let mut conds: Vec<String> = Vec::new();

    for stmt in stmts {
        let (path, delta) = match stmt {
            Stmt::CheckedSub { path, delta, .. }
            | Stmt::WrapSub { path, delta }
            | Stmt::SatSub { path, delta } => (path, delta),
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Assign { .. }
            | Stmt::CheckedAdd { .. }
            | Stmt::WrapAdd { .. }
            | Stmt::SatAdd { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => continue,
        };
        let field = path_field_name(path);
        if let Some(ty) = state_fields
            .iter()
            .find(|(n, _)| n == &field)
            .map(|(_, t)| t.clone())
        {
            if ty_max_const(&ty).is_some() {
                let d = effect_rhs_lean(delta);
                conds.push(format!("{} \u{2264} s.{}", d, safe_name(&field)));
            }
        }
    }

    conds
}

/// Overflow guard conjuncts (`s.<field> + <delta> ≤ <max>`, unsigned
/// fields only) for every add-kind statement in `stmts` — item 5 of
/// `build_guard_cond_parts`, shared with the per-arm
/// `flat_effect_bound_conds`. Deduped against both the conjuncts
/// accumulated inside this pass and every slice in `already` (the
/// caller's earlier conjuncts).
pub(super) fn add_overflow_conds(
    mir: &Mir,
    stmts: &[crate::mir::Stmt],
    already: &[&[String]],
) -> Vec<String> {
    use crate::mir::Stmt;
    let state_fields = flat_state_fields(mir);
    let mut conds: Vec<String> = Vec::new();

    for stmt in stmts {
        let (path, delta) = match stmt {
            Stmt::CheckedAdd { path, delta, .. }
            | Stmt::WrapAdd { path, delta }
            | Stmt::SatAdd { path, delta } => (path, delta),
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Assign { .. }
            | Stmt::CheckedSub { .. }
            | Stmt::WrapSub { .. }
            | Stmt::SatSub { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => continue,
        };
        let field = path_field_name(path);
        let ty = match state_fields
            .iter()
            .find(|(n, _)| n == &field)
            .map(|(_, t)| t.clone())
        {
            Some(t) => t,
            None => continue,
        };
        let max = match ty_max_const(&ty) {
            Some(m) => m,
            None => continue,
        };
        let sf = safe_name(&field);
        let d = effect_rhs_lean(delta);
        let needle_a = format!("s.{} + {}", sf, d);
        let needle_b = format!("{} + s.{}", d, sf);
        let dup = conds
            .iter()
            .chain(already.iter().flat_map(|s| s.iter()))
            .any(|c| c.contains(&needle_a) || c.contains(&needle_b));
        if dup {
            continue;
        }
        conds.push(format!("s.{} + {} \u{2264} {}", sf, d, max));
    }

    conds
}

/// Overflow/underflow bound conjuncts for one statement list — the
/// per-arm analogue of `build_guard_cond_parts` items 3, 5, and 6 (same
/// formats, same all-arithmetic-kinds coverage, same dedup against
/// already-present conjuncts). Used by the `Stmt::Branch` rendering so
/// an arm's bounds gate only that arm.
pub(super) fn flat_effect_bound_conds(
    mir: &Mir,
    stmts: &[crate::mir::Stmt],
    outer_conds: &[String],
) -> Vec<String> {
    let mut conds = sub_underflow_conds(mir, stmts);

    let adds = add_overflow_conds(mir, stmts, &[&conds, outer_conds]);
    conds.extend(adds);

    // Item-6 analogue: effect-RHS bound guards for the arm's own bare
    // arithmetic (#148), deduped against both the arm's and the outer
    // guard conjuncts.
    let seen: Vec<String> = conds.iter().chain(outer_conds.iter()).cloned().collect();
    let tree_conds = effect_tree_bound_conds(mir, stmts, &seen);
    conds.extend(tree_conds);

    conds
}

/// Build the if-condition conjuncts for a handler's flat-state transition.
/// Emit-site users (transition body, aborts theorem proof indexing) share
/// this conjunct list, so order and content are load-bearing — the `if`
/// line, `cond_parts.iter().position(...)` lookups, and conjunction-
/// projection paths must agree:
///   1. `signer = s.<who>`  (only if `who` names a state field)
///   2. `s.status = .<pre>` (lifecycle gate)
///   3. sub-effect underflow guards (`<delta> ≤ s.<field>`, unsigned only)
///   4. RequireOrAbort predicates (filtered: skip handler-account pubkey refs)
///   5. add-effect overflow guards (`s.<field> + <delta> ≤ <max>`, unsigned only)
///   6. effect-RHS bound guards for bare arithmetic (#148 — see
///      [`effect_tree_bound_conds`])
pub(super) fn build_guard_cond_parts(mir: &Mir, h: &crate::mir::HandlerMir) -> Vec<String> {
    use crate::mir::Stmt;
    let mut conds: Vec<String> = Vec::new();

    let state_fields = flat_state_fields(mir);

    let auth_name = handler_auth_name(h);
    let who_is_state_field = auth_name
        .as_deref()
        .map(|w| state_fields.iter().any(|(n, _)| n == w))
        .unwrap_or(false);
    if let Some(who) = &auth_name {
        if who_is_state_field {
            conds.push(format!("signer = s.{}", safe_name(who)));
        }
    }

    if let Some((pre, _)) = &h.transition {
        if mir.state.lifecycle_states.len() >= 2 {
            conds.push(format!("s.status = .{}", safe_name(pre)));
        }
    }

    conds.extend(sub_underflow_conds(mir, &h.body.stmts));

    for stmt in &h.body.stmts {
        if let Stmt::RequireOrAbort { pred, .. } = stmt {
            let lean = expr_lean(&pred.0, tree_render::LeanCx::guard());
            if mentions_handler_account_pubkey(&lean, &h.accounts) {
                continue;
            }
            conds.push(lean);
        }
    }

    let adds = add_overflow_conds(mir, &h.body.stmts, &[&conds]);
    conds.extend(adds);

    let tree_conds = effect_tree_bound_conds(mir, &h.body.stmts, &conds);
    conds.extend(tree_conds);

    conds
}

/// #148 — auto bound-guards for bare arithmetic inside effect values.
///
/// The Kani/proptest lane renders every effect value under
/// `ArithMode::Checked` (`rust_codegen_util::effect::emit_one_effect`):
/// bare `-` lowers to `checked_sub(..)?`, `/`/`%` to
/// `checked_div/checked_rem(..)?`, and `+`/`*` to `checked_add/mul(..)?`
/// — the harness transition REJECTS on underflow / overflow /
/// division-by-zero. The Lean model computes over `Nat`, where `-` is
/// monus (truncates at 0) and `/`, `%` are total (`x / 0 = 0`,
/// `x % 0 = x`); without matching guard conjuncts the two models diverge
/// on the rejection path (#146 / #148). Per effect value carrying a typed
/// tree (tree-less `Expr`s — IDL ingest, probes — keep the legacy
/// no-guard behavior):
///
///   * every `-` node whose kind joins to `Nat` → `<rhs> ≤ <lhs>`
///     (Int subtraction doesn't truncate in Lean — skipped);
///   * every `/` / `%` node (and the `mul_div_floor/ceil` divisor) →
///     `<divisor> ≠ 0`, unless the divisor is a nonzero literal/const;
///   * an `Assign` RHS containing `+` / `*` / `mul_div_*` when the target
///     field's type is bounded → `<rhs> ≤ <MAX>` (final-value bound —
///     the checked ops abort above the field width; Lean `Nat` doesn't).
///     Approximate for mixed shapes (an intermediate `checked_add`
///     overflow under a later `-` isn't captured) — issue #148 specifies
///     the final-value bound.
///
/// Add/sub-family deltas are walked too (their compound arithmetic
/// renders checked in the harness just like an `Assign` RHS); their
/// OUTER op is already guarded by items 3/5 of
/// [`build_guard_cond_parts`], so only interior nodes contribute here —
/// no duplication, and no extra MAX bound (item 5 owns the add bound).
///
/// Only unconditionally-evaluated positions are walked: arithmetic in an
/// `if`/`match` arm is checked in Rust only when that arm is taken, so an
/// unconditional Lean guard would over-constrain the model (reject
/// executions the harness accepts). Binder-introducing bodies (`let … in`
/// bodies, `sum`, quantifiers, match arms) are skipped for scope: a guard
/// rendered from under a binder would carry an out-of-scope identifier.
pub(super) fn effect_tree_bound_conds(
    mir: &Mir,
    stmts: &[crate::mir::Stmt],
    existing: &[String],
) -> Vec<String> {
    use crate::mir::expr_tree::NumKind;
    use crate::mir::Stmt;

    let cx = tree_render::LeanCx::guard().with_application_subscripts();
    let state_fields = flat_state_fields(mir);
    let mut conds: Vec<String> = Vec::new();

    for stmt in stmts {
        let (value, assign_target_ty): (&crate::mir::Expr, Option<crate::mir::Ty>) = match stmt {
            Stmt::Assign { path, rhs } => {
                // Account-binding pubkey refs are dropped from the effect
                // body (no Lean scope) — no guard either.
                if is_account_pubkey_ref(&crate::rust_codegen_util::mir_expr_rust(rhs)) {
                    continue;
                }
                let field = path_field_name(path);
                (
                    rhs,
                    state_fields
                        .iter()
                        .find(|(n, _)| n == &field)
                        .map(|(_, t)| t.clone()),
                )
            }
            Stmt::CheckedAdd { delta, .. }
            | Stmt::CheckedSub { delta, .. }
            | Stmt::WrapAdd { delta, .. }
            | Stmt::WrapSub { delta, .. }
            | Stmt::SatAdd { delta, .. }
            | Stmt::SatSub { delta, .. } => (delta, None),
            // Branch arms are guarded per-arm (`flat_effect_bound_conds`);
            // the rest carry no effect value.
            Stmt::RequireOrAbort { .. }
            | Stmt::TokenTransfer { .. }
            | Stmt::VariantPromote { .. }
            | Stmt::Branch { .. }
            | Stmt::Cpi { .. }
            | Stmt::Emit { .. } => continue,
        };
        let Some(tree) = &value.tree else { continue };

        let saw_growth = collect_tree_bound_guards(tree, cx, existing, &mut conds);

        if saw_growth && tree.num_kind() == NumKind::Nat {
            if let Some(ty) = &assign_target_ty {
                if let Some(max) = ty_max_const(ty) {
                    push_unique_cond(
                        format!("{} \u{2264} {}", tree_render::render_lean(tree, cx), max),
                        existing,
                        &mut conds,
                    );
                }
            }
        }
    }

    conds
}

/// Post-order walk collecting the #148 guard conjuncts for one effect
/// value; returns whether an unconditionally-evaluated growth node
/// (`+` / `*` / `mul_div_*`) was seen (drives the caller's final-value
/// MAX bound). Post-order keeps chained guards cumulative: `a - b - c`
/// yields `b ≤ a` then `c ≤ a - b`. Exhaustive over `ExprTree` — no `_`
/// arms (see the enum doc in `mir::expr_tree`).
fn collect_tree_bound_guards(
    e: &crate::mir::expr_tree::ExprTree,
    cx: tree_render::LeanCx,
    existing: &[String],
    conds: &mut Vec<String>,
) -> bool {
    use crate::mir::expr_tree::{ExprTree, NumKind, TreeArithOp};

    let walk = |sub: &ExprTree, conds: &mut Vec<String>| -> bool {
        collect_tree_bound_guards(sub, cx, existing, conds)
    };

    match e {
        ExprTree::Int(_) | ExprTree::Bool(_) | ExprTree::Path(_) => false,
        ExprTree::Old(inner) | ExprTree::Not(inner) => walk(inner, conds),
        // Binder-introducing bodies — out-of-scope identifiers in a guard.
        ExprTree::Sum { .. } | ExprTree::Quant { .. } | ExprTree::QuantIn { .. } => false,
        // Rust `&&` / `||` short-circuit: only the lhs is unconditional.
        ExprTree::BoolOp { lhs, .. } => walk(lhs, conds),
        ExprTree::Cmp { lhs, rhs, .. } => {
            let l = walk(lhs, conds);
            let r = walk(rhs, conds);
            l || r
        }
        ExprTree::Contains { coll, elem } => {
            let l = walk(coll, conds);
            let r = walk(elem, conds);
            l || r
        }
        ExprTree::Len(coll) => walk(coll, conds),
        ExprTree::Arith { op, lhs, rhs } => {
            let l = walk(lhs, conds);
            let r = walk(rhs, conds);
            let growth = l || r;
            match op {
                TreeArithOp::Sub => {
                    if e.num_kind() == NumKind::Nat {
                        push_unique_cond(
                            format!(
                                "{} \u{2264} {}",
                                tree_render::render_lean(rhs, cx),
                                tree_render::render_lean(lhs, cx)
                            ),
                            existing,
                            conds,
                        );
                    }
                    growth
                }
                TreeArithOp::Div | TreeArithOp::Mod => {
                    push_divisor_guard(rhs, cx, existing, conds);
                    growth
                }
                TreeArithOp::Add | TreeArithOp::Mul => true,
            }
        }
        // The u128-widened helpers narrow back to the declared width
        // fallibly (`try_into()?`) and divide checked — growth + divisor
        // guard, same divergence class as bare `*` / `/`.
        ExprTree::MulDivFloor { a, b, d }
        | ExprTree::MulDivCeil { a, b, d }
        | ExprTree::MulDivRoundHalfUp { a, b, d } => {
            walk(a, conds);
            walk(b, conds);
            walk(d, conds);
            push_divisor_guard(d, cx, existing, conds);
            true
        }
        // Scrutinee is unconditional; arm bodies are taken conditionally
        // AND may bind payload identifiers — skipped.
        ExprTree::Match { scrutinee, .. } => walk(scrutinee, conds),
        ExprTree::Ctor { payload, .. } => match payload {
            Some(p) => walk(p, conds),
            None => false,
        },
        ExprTree::RecordLit { fields, .. } => {
            let mut growth = false;
            for (_, v) in fields {
                growth |= walk(v, conds);
            }
            growth
        }
        ExprTree::RecordUpdate { base, updates, .. } => {
            let mut growth = walk(base, conds);
            for (_, v) in updates {
                growth |= walk(v, conds);
            }
            growth
        }
        ExprTree::IsVariant { scrutinee, .. } => walk(scrutinee, conds),
        ExprTree::App { args, .. } => {
            let mut growth = false;
            for a in args {
                growth |= walk(a, conds);
            }
            growth
        }
        ExprTree::Field { base, .. } => walk(base, conds),
        // The let VALUE is unconditional; the body references the binder
        // (out of guard scope) — skipped.
        ExprTree::Let { value, .. } => walk(value, conds),
        // Branches are conditionally evaluated — only the condition walks.
        ExprTree::IfThenElse { cond, .. } => walk(cond, conds),
    }
}

/// `<divisor> ≠ 0` unless the divisor renders to a nonzero integer
/// literal (covers `x / 2` and `x / CONST` — `Const` bindings render
/// their resolved value).
fn push_divisor_guard(
    d: &crate::mir::expr_tree::ExprTree,
    cx: tree_render::LeanCx,
    existing: &[String],
    conds: &mut Vec<String>,
) {
    let rendered = tree_render::render_lean(d, cx);
    if let Ok(v) = rendered.replace('_', "").parse::<u128>() {
        if v != 0 {
            return;
        }
    }
    push_unique_cond(format!("{} \u{2260} 0", rendered), existing, conds);
}

/// Push `cond` unless an identical conjunct is already present (either
/// collected in this pass or in the caller's existing guard list — e.g.
/// a user-written `requires` spelling the same bound).
fn push_unique_cond(cond: String, existing: &[String], conds: &mut Vec<String>) {
    if existing.iter().any(|c| c == &cond) || conds.iter().any(|c| c == &cond) {
        return;
    }
    conds.push(cond);
}
