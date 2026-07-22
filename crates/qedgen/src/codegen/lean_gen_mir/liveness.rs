use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder};

/// Concrete state used for cover-trace proof synthesis. Field values
/// are strings rather than typed Lean terms — Pubkey fields hold `"pk"`
/// (the binding the proof scope introduces), Bool fields hold
/// `"false"`, numeric fields hold a numeric string.
pub(super) struct WitnessState {
    fields: Vec<(String, String)>,
    status: Option<String>,
}

impl WitnessState {
    fn new(state: &crate::mir::StateAdt) -> Self {
        // Union of all variant fields forms the witness's flat-field view;
        // the first variant defines order, later variants append.
        let fields: Vec<(String, String)> = state_field_union(state)
            .into_iter()
            .map(|(name, ty)| {
                let val = match ty {
                    crate::mir::Ty::Pubkey => "pk".to_string(),
                    crate::mir::Ty::Bool => "false".to_string(),
                    _ => "0".to_string(),
                };
                (name, val)
            })
            .collect();
        WitnessState {
            fields,
            status: state.lifecycle_states.first().cloned(),
        }
    }

    /// Render as a positional struct literal `⟨pk, pk, 0, …, .Status⟩`
    /// — flat-state shape. Multi-variant ADT specs route through
    /// `witness_state_to_adt` instead.
    fn to_lean(&self) -> String {
        let mut parts: Vec<String> = self.fields.iter().map(|(_, v)| v.clone()).collect();
        if let Some(ref s) = self.status {
            parts.push(format!(".{}", s));
        }
        format!("\u{27E8}{}\u{27E9}", parts.join(", "))
    }

    /// Walk `handler.body.stmts` and update field values + lifecycle
    /// status. Saturating arithmetic so witnesses don't underflow when
    /// the proof has unsatisfiable conditions.
    fn apply(
        &mut self,
        h: &crate::mir::HandlerMir,
        params: &[(String, String)],
        constants: &[(crate::mir::Symbol, String)],
        mir: &Mir,
    ) {
        use crate::mir::Stmt;
        for stmt in &h.body.stmts {
            match stmt {
                Stmt::Assign { path, rhs } => {
                    let rhs_rust = crate::rust_codegen_util::mir_expr_rust(rhs);
                    if is_account_pubkey_ref(&rhs_rust) {
                        continue;
                    }
                    let key = strip_variant_prefix(path, mir);
                    let resolved = self.resolve_value(&rhs_rust, params, constants);
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == &key) {
                        f.1 = resolved;
                    }
                }
                Stmt::CheckedAdd { path, delta, .. }
                | Stmt::WrapAdd { path, delta }
                | Stmt::SatAdd { path, delta } => {
                    let key = strip_variant_prefix(path, mir);
                    let resolved = self.resolve_value(
                        &crate::rust_codegen_util::mir_expr_rust(delta),
                        params,
                        constants,
                    );
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == &key) {
                        let cur: u128 = f.1.parse().unwrap_or(0);
                        let add: u128 = resolved.parse().unwrap_or(0);
                        f.1 = cur.saturating_add(add).to_string();
                    }
                }
                Stmt::CheckedSub { path, delta, .. }
                | Stmt::WrapSub { path, delta }
                | Stmt::SatSub { path, delta } => {
                    let key = strip_variant_prefix(path, mir);
                    let resolved = self.resolve_value(
                        &crate::rust_codegen_util::mir_expr_rust(delta),
                        params,
                        constants,
                    );
                    if let Some(f) = self.fields.iter_mut().find(|(n, _)| n == &key) {
                        let cur: u128 = f.1.parse().unwrap_or(0);
                        let sub: u128 = resolved.parse().unwrap_or(0);
                        f.1 = cur.saturating_sub(sub).to_string();
                    }
                }
                Stmt::RequireOrAbort { .. }
                | Stmt::TokenTransfer { .. }
                | Stmt::VariantPromote { .. }
                | Stmt::Branch { .. }
                | Stmt::Cpi { .. }
                | Stmt::Emit { .. } => {}
            }
        }
        if let Some((_, post)) = &h.transition {
            self.status = Some(post.clone());
        }
    }

    /// Resolve a value reference: caller-bound parameter → numeric
    /// literal → spec-constant lookup → self-field lookup → fallback.
    fn resolve_value(
        &self,
        value: &str,
        params: &[(String, String)],
        constants: &[(crate::mir::Symbol, String)],
    ) -> String {
        let v = value.trim();
        if let Some((_, x)) = params.iter().find(|(n, _)| n == v) {
            return x.clone();
        }
        if v.parse::<u128>().is_ok() {
            return v.to_string();
        }
        if let Some(f) = self.fields.iter().find(|(n, _)| n == v) {
            return f.1.clone();
        }
        if let Some((_, x)) = constants.iter().find(|(n, _)| n == v) {
            return x.clone();
        }
        "1".to_string()
    }
}

/// Multi-variant ADT counterpart of `WitnessState::to_lean` — emits a
/// `(.Variant arg0 arg1 … : State)` constructor term using the current
/// witness status to pick the variant.
pub(super) fn witness_state_to_adt(
    ws: &WitnessState,
    variants: &[crate::mir::StateVariant],
) -> Option<String> {
    let status = ws.status.as_deref()?;
    let variant = variants.iter().find(|v| v.tag == status)?;
    if variant.fields.is_empty() {
        return Some(format!("(.{} : State)", variant.tag));
    }
    let args: Vec<String> = variant
        .fields
        .iter()
        .map(|f| {
            ws.fields
                .iter()
                .find(|(n, _)| n == &f.name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "0".to_string())
        })
        .collect();
    Some(format!("(.{} {} : State)", variant.tag, args.join(" ")))
}

/// Choose concrete witness values for a handler's parameters:
///   * Pubkey  → `pk`
///   * Bool    → `false`
///   * Index-like numeric params (`param < s.X` without bound) → `0`
///   * Otherwise → `1` (satisfies common `> 0` / `≤ N` guards)
pub(super) fn choose_param_values(h: &crate::mir::HandlerMir) -> Vec<(String, String)> {
    let mut all_exprs: Vec<String> = Vec::new();
    for p in &h.pre {
        all_exprs.push(expr_lean(&p.0, tree_render::LeanCx::guard()));
    }
    for r in &h.requires_or_abort {
        all_exprs.push(expr_lean(&r.pred.0, tree_render::LeanCx::guard()));
    }
    let combined = all_exprs.join(" ");
    h.params
        .iter()
        .map(|(name, ty)| {
            let val = match ty {
                crate::mir::Ty::Pubkey => "pk".to_string(),
                crate::mir::Ty::Bool => "false".to_string(),
                _ => {
                    let is_index_like = combined.contains(&format!("{} < s.", name))
                        && !combined.contains(&format!("{} > 0", name))
                        && !combined.contains(&format!("{} \u{2265}", name))
                        && !combined.contains(&format!("\u{2264} {}", name));
                    if is_index_like {
                        "0".to_string()
                    } else {
                        "1".to_string()
                    }
                }
            };
            (name.clone(), val)
        })
        .collect()
}

/// Build the auto-proof script for a cover trace theorem. Symbolically
/// evaluates each handler in `trace` against a witness state, then
/// emits `let s0, s1, …` declarations and an `exact ⟨…, by decide,
/// …⟩` term. Returns `None` if any handler in the trace doesn't
/// resolve — the caller falls back to `:= sorry`.
///
/// `adt_form` switches the witness rendering between the flat-state
/// struct literal and the ADT variant-constructor form.
pub(super) fn cover_trace_proof(
    mir: &Mir,
    trace: &[crate::mir::Symbol],
    adt_form: bool,
) -> Option<String> {
    if trace.is_empty() {
        return None;
    }

    let mut state = WitnessState::new(&mir.state);
    type CoverStep = (String, Vec<(String, String)>, WitnessState);
    let mut steps: Vec<CoverStep> = Vec::new();

    for op_name in trace {
        let handler = mir.handlers.iter().find(|h| &h.name == op_name)?;
        let param_values = choose_param_values(handler);
        let state_before = WitnessState {
            fields: state.fields.clone(),
            status: state.status.clone(),
        };
        state.apply(handler, &param_values, &mir.constants, mir);
        steps.push((op_name.clone(), param_values, state_before));
    }

    let render_witness = |ws: &WitnessState| -> Option<String> {
        if adt_form {
            witness_state_to_adt(ws, &mir.state.variants)
        } else {
            Some(ws.to_lean())
        }
    };

    let mut proof = String::new();
    proof.push_str(" := by\n");
    proof.push_str("  let pk : Pubkey := \u{27E8}0, 0, 0, 0\u{27E9}\n");

    if let Some((_, _, ref s0)) = steps.first() {
        let s0_lean = render_witness(s0)?;
        proof.push_str(&format!("  let s0 : State := {}\n", s0_lean));
    }

    for (i, _) in steps.iter().enumerate() {
        if i < steps.len() - 1 {
            let mut s = WitnessState::new(&mir.state);
            for step in steps.iter().take(i + 1) {
                let h = mir.handlers.iter().find(|x| x.name == step.0)?;
                s.apply(h, &step.1, &mir.constants, mir);
            }
            let s_lean = render_witness(&s)?;
            proof.push_str(&format!("  let s{} : State := {}\n", i + 1, s_lean));
        }
    }

    let mut exact_parts: Vec<String> = Vec::new();
    exact_parts.push("s0".to_string());
    exact_parts.push("pk".to_string());
    for (i, (_, param_values, _)) in steps.iter().enumerate() {
        for (_, val) in param_values {
            exact_parts.push(val.clone());
        }
        if i < steps.len() - 1 {
            exact_parts.push(format!("s{}", i + 1));
            exact_parts.push("by decide".to_string());
        } else {
            exact_parts.push("by decide".to_string());
        }
    }
    proof.push_str(&format!(
        "  exact \u{27E8}{}\u{27E9}\n",
        exact_parts.join(", ")
    ));
    Some(proof)
}

/// Emit cover theorems — reachability obligations over a sequence of
/// handler invocations. Each `cover <name> [op_1, ..., op_n]` lowers
/// to a nested existential asserting the trace runs to completion;
/// each `reachable when <expr>` entry lowers to one theorem per
/// `(op, when)` pair.
///
/// Trace theorems try `cover_trace_proof` first (witness construction
/// with `by decide` on each step); they fall back to `:= sorry` when
/// the witness machinery can't synthesize a discharge. `reachable
/// when` entries always emit `:= sorry` — no witness chain is
/// available.
pub(super) fn emit_covers(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_covers_inner(out, mir, false, rec);
}

/// ADT-shape cover emitter — same trace structure but witness terms
/// are rendered as variant constructors via `witness_state_to_adt`.
pub(super) fn emit_covers_adt(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_covers_inner(out, mir, true, rec);
}

pub(super) fn emit_covers_inner(
    out: &mut String,
    mir: &Mir,
    adt_form: bool,
    rec: &mut ObligationRecorder,
) {
    if mir.covers.is_empty() {
        return;
    }
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Cover properties \u{2014} reachability (existential proofs)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    emit_covers_body(out, mir, adt_form, rec);
}

/// Per-cover theorem rendering — shared body of `emit_covers_inner`
/// (which writes the section header) and the multi-account
/// `emit_covers_multi` (which writes its own header before filtering
/// cross-account traces, then calls this directly — no more
/// render-then-strip).
pub(super) fn emit_covers_body(
    out: &mut String,
    mir: &Mir,
    adt_form: bool,
    rec: &mut ObligationRecorder,
) {
    for cover in &mir.covers {
        for (trace_idx, trace) in cover.traces.iter().enumerate() {
            let suffix = if cover.traces.len() > 1 {
                format!("_{}", trace_idx)
            } else {
                String::new()
            };

            rec.emitted(
                ObligationKind::Cover,
                "file",
                &format!("{}::{}", cover.name, trace_idx),
                &format!("cover_{}{}", cover.name, suffix),
            );
            out.push_str(&format!(
                "/-- {} \u{2014} trace [{}] is reachable. -/\n",
                cover.name,
                trace.join(", ")
            ));
            out.push_str(&format!(
                "theorem cover_{}{} : \u{2203} (s0 : State) (signer : Pubkey),\n",
                cover.name, suffix
            ));

            // Nested `∃ s_{j+1}, <trans> s_j signer args = some s_{j+1}
            // ∧ ...` chain; the terminal step uses `≠ none`.
            let mut indent = "    ".to_string();
            for (j, op_name) in trace.iter().enumerate() {
                let handler = mir.handlers.iter().find(|h| h.name == *op_name);
                let trans = safe_name(&format!("{}Transition", op_name));
                let extra_exists = handler
                    .map(|h| {
                        h.params
                            .iter()
                            .enumerate()
                            .map(|(k, (_, t))| format!("(v{}_{} : {})", j, k, render_ty(t)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                // Call sites use the existentially-bound `v{j}_{k}` names.
                let positional_args = handler
                    .map(|h| {
                        h.params
                            .iter()
                            .enumerate()
                            .map(|(k, _)| format!("v{}_{}", j, k))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                let s_var = if j == 0 {
                    "s0".to_string()
                } else {
                    format!("s{}", j)
                };

                if !extra_exists.is_empty() {
                    out.push_str(&format!("{}\u{2203} {}, ", indent, extra_exists));
                }

                if j < trace.len() - 1 {
                    let s_next = format!("s{}", j + 1);
                    let arg_str = if positional_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", positional_args)
                    };
                    out.push_str(&format!(
                        "\u{2203} ({} : State), {} {} signer{} = some {} \u{2227}\n",
                        s_next, trans, s_var, arg_str, s_next
                    ));
                    indent.push_str("  ");
                } else {
                    let arg_str = if positional_args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", positional_args)
                    };
                    // Try witness construction; fall back to `:= sorry`
                    // when the witness machinery can't synthesize a
                    // closed term (handler not found, unsupported
                    // effect shape, etc.).
                    let proof_script = cover_trace_proof(mir, trace, adt_form);
                    match proof_script {
                        Some(script) => {
                            out.push_str(&format!(
                                "{} {} signer{} \u{2260} none{}\n",
                                trans, s_var, arg_str, script
                            ));
                        }
                        None => {
                            out.push_str(&format!(
                                "{} {} signer{} \u{2260} none := sorry\n\n",
                                trans, s_var, arg_str
                            ));
                        }
                    }
                }
            }
        }

        for (op_name, when_pred) in &cover.reachable {
            rec.emitted(
                ObligationKind::Cover,
                "file",
                &format!("{}::reachable::{}", cover.name, op_name),
                &format!("cover_{}_{}", cover.name, safe_name(op_name)),
            );
            let handler = mir.handlers.iter().find(|h| h.name == *op_name);
            let trans = safe_name(&format!("{}Transition", op_name));
            let param_exists = handler
                .map(|h| {
                    h.params
                        .iter()
                        .map(|(n, t)| format!("({} : {})", n, render_ty(t)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let param_args = handler
                .map(|h| param_args_str(&h.params))
                .unwrap_or_default();

            out.push_str(&format!(
                "/-- {} \u{2014} {} is reachable",
                cover.name, op_name
            ));
            if let Some(p) = when_pred {
                out.push_str(&format!(
                    " when {}. -/\n",
                    expr_lean(&p.0, tree_render::LeanCx::guard())
                ));
            } else {
                out.push_str(". -/\n");
            }
            out.push_str(&format!(
                "theorem cover_{}_{} : \u{2203} (s : State) (signer : Pubkey),\n",
                cover.name,
                safe_name(op_name)
            ));
            if let Some(p) = when_pred {
                out.push_str(&format!(
                    "    {} \u{2227} ",
                    expr_lean(&p.0, tree_render::LeanCx::guard())
                ));
            } else {
                out.push_str("    ");
            }
            if !param_exists.is_empty() {
                out.push_str(&format!("\u{2203} {}, ", param_exists));
            }
            out.push_str(&format!(
                "{} s signer{} \u{2260} none := sorry\n\n",
                trans, param_args
            ));
        }
    }
}

/// Emit `liveness` (bounded leads-to) theorems — see `emit_liveness_inner`.
pub(super) fn emit_liveness(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_liveness_inner(out, mir, false, rec);
}

/// ADT-shape liveness emitter. Statement form: `∃ ops, … ∧ ∀ s',
/// applyOps … = some s' → s'.status = .Target` (any successful
/// evaluation reaches the target), whereas the flat-state form is
/// `∃ ops s', …` (existential over both the ops sequence and the
/// resulting state). Both are valid liveness statements; the split is
/// snapshot-pinned.
pub(super) fn emit_liveness_adt(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    emit_liveness_inner(out, mir, true, rec);
}

pub(super) fn emit_liveness_inner(
    out: &mut String,
    mir: &Mir,
    adt_form: bool,
    rec: &mut ObligationRecorder,
) {
    if mir.liveness_props.is_empty() {
        return;
    }
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Liveness properties \u{2014} bounded reachability (leads-to)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    // One shared `applyOps` helper; indexed and multi-account variants
    // manage their own.
    let needs_helper = mir.handlers.iter().any(|_| true);
    if needs_helper {
        out.push_str(
            "def applyOps (s : State) (signer : Pubkey) : List Operation \u{2192} Option State\n",
        );
        out.push_str("  | [] => some s\n");
        out.push_str("  | op :: ops => match applyOp s signer op with\n");
        out.push_str("    | some s' => applyOps s' signer ops\n");
        out.push_str("    | none => none\n\n");
    }

    emit_liveness_body(out, mir, adt_form, rec);
}

/// Per-liveness theorem rendering — shared body of `emit_liveness_inner`
/// (single-account: header + `applyOps` helper above) and the
/// multi-account `emit_liveness_inner_body` (per-account helper + token
/// renames handled by the caller).
pub(super) fn emit_liveness_body(
    out: &mut String,
    mir: &Mir,
    adt_form: bool,
    rec: &mut ObligationRecorder,
) {
    for liveness in &mir.liveness_props {
        let bound = liveness.within_steps.unwrap_or(10);
        rec.emitted(
            ObligationKind::Liveness,
            "file",
            &liveness.name,
            &format!("liveness_{}", liveness.name),
        );
        out.push_str(&format!(
            "/-- {} \u{2014} from {} leads to {} within {} steps via [{}]. -/\n",
            liveness.name,
            liveness.from_state,
            liveness.leads_to_state,
            bound,
            liveness.via_ops.join(", ")
        ));
        out.push_str(&format!(
            "theorem liveness_{} (s : State) (signer : Pubkey)\n",
            liveness.name
        ));
        out.push_str(&format!(
            "    (h : s.status = .{}) :\n",
            liveness.from_state
        ));
        if adt_form {
            // ADT-shape liveness: keep the universal-implication form
            // with `by sorry`; the auto-discharge script pattern-matches
            // on flat-state if-guards and isn't valid against the
            // per-variant pattern-match transitions.
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', applyOps s signer ops = some s' \u{2192} s'.status = .{} := by sorry\n\n",
                bound, liveness.leads_to_state
            ));
            continue;
        }

        // Flat-state path: when a concrete via-op path through the
        // lifecycle exists, emit the universal-implication form +
        // auto-proof script; else fall back to the existential form
        // with sorry (non-vacuous obligation).
        let path = find_liveness_path(
            &liveness.from_state,
            &liveness.leads_to_state,
            &liveness.via_ops,
            &mir.handlers,
        );

        if let Some(ref ops_path) = path {
            let proof = liveness_proof_script(ops_path, &mir.handlers);
            out.push_str(&format!(
                "    \u{2203} ops, ops.length \u{2264} {} \u{2227} \u{2200} s', applyOps s signer ops = some s' \u{2192} s'.status = .{}{}\n",
                bound, liveness.leads_to_state, proof
            ));
        } else {
            out.push_str(&format!(
                "    \u{2203} ops s', ops.length \u{2264} {} \u{2227} applyOps s signer ops = some s' \u{2227} s'.status = .{} := by sorry\n\n",
                bound, liveness.leads_to_state
            ));
        }
    }
}

/// BFS through the lifecycle graph defined by `via_ops`'
/// `(pre_status, post_status)` arrows, returning the first sequence from
/// `from_state` to `to_state` (single-step shortcut + BFS bounded by
/// `via_ops.len()`).
pub(super) fn find_liveness_path(
    from_state: &str,
    to_state: &str,
    via_ops: &[String],
    handlers: &[crate::mir::HandlerMir],
) -> Option<Vec<String>> {
    for op_name in via_ops {
        if let Some(h) = handlers.iter().find(|h| h.name == *op_name) {
            if let Some((pre, post)) = &h.transition {
                if pre == from_state && post == to_state {
                    return Some(vec![op_name.clone()]);
                }
            }
        }
    }

    let mut queue: Vec<(String, Vec<String>)> = vec![(from_state.to_string(), Vec::new())];
    let max_depth = via_ops.len();

    while let Some((current, path)) = queue.first().cloned() {
        queue.remove(0);
        if path.len() >= max_depth {
            continue;
        }
        for op_name in via_ops {
            if let Some(h) = handlers.iter().find(|h| h.name == *op_name) {
                if let Some((pre, post)) = &h.transition {
                    if pre == &current && !post.is_empty() {
                        let mut new_path = path.clone();
                        new_path.push(op_name.clone());
                        if post == to_state {
                            return Some(new_path);
                        }
                        queue.push((post.clone(), new_path));
                    }
                }
            }
        }
    }
    None
}

/// Generate the Lean tactic body for a liveness theorem along an
/// already-found `ops_path`. Shape:
///   1. Optional `let pk : Pubkey := ⟨0,0,0,0⟩` when any constructor
///      takes a Pubkey witness.
///   2. `refine ⟨[<ops>], by decide, fun s' h_apply => ?_⟩`
///   3. `simp only [applyOps, applyOp, …]` then `split at h_apply` /
///      `subst` / `rfl` mechanics (one nest per step, single-step is
///      special-cased).
///
/// `needs_split[i]` is true when handler i's if-guard is non-trivial
/// (auth, requires clause, or lifecycle gate).
pub(super) fn liveness_proof_script(
    ops_path: &[String],
    handlers: &[crate::mir::HandlerMir],
) -> String {
    let n = ops_path.len();

    // Build the ops list literal: `[.op1 arg1, .op2, ...]`. Each
    // constructor needs a witness arg per `params`; bare `.op` would
    // mistype handlers whose Operation constructor takes parameters.
    let mut needs_pk_binding = false;
    let ops_list: Vec<String> = ops_path
        .iter()
        .map(|name| {
            let handler = handlers.iter().find(|h| &h.name == name);
            let args: Vec<String> = match handler {
                Some(h) => h
                    .params
                    .iter()
                    .map(|(_, ty)| match ty {
                        crate::mir::Ty::Pubkey => {
                            needs_pk_binding = true;
                            "pk".to_string()
                        }
                        crate::mir::Ty::Bool => "false".to_string(),
                        _ => "0".to_string(),
                    })
                    .collect(),
                None => Vec::new(),
            };
            if args.is_empty() {
                format!(".{}", safe_name(name))
            } else {
                format!(".{} {}", safe_name(name), args.join(" "))
            }
        })
        .collect();
    let ops_literal = format!("[{}]", ops_list.join(", "));

    // Per-step "guard is non-trivial": auth, any RequireOrAbort,
    // lifecycle gate, or pre clauses.
    let needs_split: Vec<bool> = ops_path
        .iter()
        .map(|name| {
            handlers
                .iter()
                .find(|h| &h.name == name)
                .map(|h| {
                    handler_auth_name(h).is_some()
                        || !h.requires_or_abort.is_empty()
                        || h.transition.is_some()
                        || !h.pre.is_empty()
                })
                .unwrap_or(false)
        })
        .collect();

    let trans_names: Vec<String> = ops_path
        .iter()
        .map(|name| safe_name(&format!("{}Transition", name)))
        .collect();

    let mut proof = String::new();
    proof.push_str(" := by\n");
    if needs_pk_binding {
        proof.push_str("  let pk : Pubkey := \u{27E8}0, 0, 0, 0\u{27E9}\n");
    }
    proof.push_str(&format!(
        "  refine \u{27E8}{}, by decide, fun s' h_apply => ?\u{5F}\u{27E9}\n",
        ops_literal
    ));

    if n == 1 {
        let trans = &trans_names[0];
        if needs_split[0] {
            proof.push_str(&format!(
                "  simp only [applyOps, applyOp, {}] at h_apply\n",
                trans
            ));
            proof.push_str("  split at h_apply\n");
            proof.push_str("  \u{B7} next heq =>\n");
            proof.push_str("    split at heq\n");
            proof.push_str(
                "    \u{B7} next hg => simp at heq h_apply; subst heq; subst h_apply; rfl\n",
            );
            proof.push_str("    \u{B7} simp at heq\n");
            proof.push_str("  \u{B7} simp at h_apply\n");
        } else {
            proof.push_str(&format!(
                "  simp only [applyOps, applyOp, {}, h, \u{2193}reduceIte] at h_apply\n",
                trans
            ));
            proof.push_str("  cases h_apply; rfl\n");
        }
    } else {
        proof.push_str("  simp only [applyOps, applyOp] at h_apply\n");
        liveness_multi_step_proof(&mut proof, &trans_names, &needs_split, 0, "  ");
    }

    proof
}

/// Recursive nested-split builder for multi-step liveness. Indentation
/// grows by two spaces per nesting depth so the emitted Lean is readable.
#[allow(clippy::only_used_in_recursion)]
pub(super) fn liveness_multi_step_proof(
    proof: &mut String,
    trans_names: &[String],
    needs_split: &[bool],
    step: usize,
    indent: &str,
) {
    if step >= trans_names.len() {
        return;
    }
    let trans = &trans_names[step];
    let is_last = step == trans_names.len() - 1;

    proof.push_str(&format!("{}simp only [{}] at h_apply\n", indent, trans));
    proof.push_str(&format!("{}split at h_apply\n", indent));

    if is_last {
        if needs_split[step] {
            proof.push_str(&format!("{}\u{B7} next heq =>\n", indent));
            let inner = format!("{}  ", indent);
            proof.push_str(&format!("{}split at heq\n", inner));
            proof.push_str(&format!(
                "{}\u{B7} next hg => simp at heq h_apply; subst heq; subst h_apply; rfl\n",
                inner
            ));
            proof.push_str(&format!("{}\u{B7} simp at heq\n", inner));
        } else {
            proof.push_str(&format!("{}\u{B7} cases h_apply; rfl\n", indent));
        }
    } else if needs_split[step] {
        proof.push_str(&format!("{}\u{B7} next heq =>\n", indent));
        let inner = format!("{}  ", indent);
        proof.push_str(&format!("{}split at heq\n", inner));
        proof.push_str(&format!("{}\u{B7} next hg =>\n", inner));
        let inner2 = format!("{}  ", inner);
        proof.push_str(&format!("{}simp at heq\n", inner2));
        proof.push_str(&format!("{}subst heq\n", inner2));
        liveness_multi_step_proof(proof, trans_names, needs_split, step + 1, &inner2);
        proof.push_str(&format!("{}\u{B7} simp at heq\n", inner));
    } else {
        proof.push_str(&format!("{}\u{B7}\n", indent));
        let next_indent = format!("{}  ", indent);
        liveness_multi_step_proof(proof, trans_names, needs_split, step + 1, &next_indent);
    }
}

/// Emit `environment` preservation theorems. Per (property × environment)
/// pair: `theorem <prop>_under_<env> (s : State) <new-field params>
/// <constraint hyps> (h_inv : <prop> s) :
/// <prop> { s with <field := new_field>... } := <proof>`.
///
/// Proof body auto-discharges with `unfold <prop> at h_inv ⊢; dsimp;
/// exact h_inv` when the mutated fields don't appear in the property
/// expression; otherwise emits `:= sorry`.
pub(super) fn emit_environments(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    if mir.environments.is_empty() {
        return;
    }
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Environment \u{2014} properties hold under external state changes\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    emit_environments_body(out, mir, rec);
}

/// Per-(environment × property) theorem rendering — shared body of
/// `emit_environments` (single-account) and the multi-account
/// `emit_environments_multi` (which scopes and renames per account).
/// Includes the bare-field-name constraint rewrite the spec's
/// `constraint <field> > 0` form needs — historically only the
/// multi-account clone had it (drift healed in T4).
pub(super) fn emit_environments_body(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    for env in &mir.environments {
        for prop in &mir.properties {
            let prop_expr = match &prop.expression {
                Some(e) => e,
                None => continue,
            };
            rec.emitted(
                ObligationKind::Environment,
                "file",
                &format!("{}::{}", prop.name, env.name),
                &format!("{}_under_{}", prop.name, env.name),
            );

            let param_sig: String = env
                .mutates
                .iter()
                .map(|(name, ty)| format!(" (new_{} : {})", name, render_ty(ty)))
                .chain(env.external_fields.iter().flat_map(|(object, field, ty)| {
                    [
                        format!(" (pre_{}_{} : {})", object, field, render_ty(ty)),
                        format!(" (post_{}_{} : {})", object, field, render_ty(ty)),
                    ]
                }))
                .collect();

            // Rewrite `s.<field>` / `state.<field>` / bare `<field>` in
            // each constraint to refer to the new value.
            let constraint_hyps: String = env
                .typed_constraints
                .iter()
                .enumerate()
                .map(|(i, constraint)| {
                    // Render through the typed tree with the single-state
                    // (`s.`) binder: the theorem binds `s`, `new_<mutated>`,
                    // and `pre_/post_<external>` — but never `s'`. State reads
                    // render as `s.<field>` so the mutates rewrite below maps
                    // mutated fields to `new_<field>` and leaves the rest as
                    // `s.<field>`; external reads render as `pre_/post_<…>`
                    // (binder-independent).
                    let mut expr = expr_lean(&constraint.predicate.0, tree_render::LeanCx::guard());
                    for (field, _) in &env.mutates {
                        expr = expr
                            .replace(&format!("s.{}", field), &format!("new_{}", field))
                            .replace(&format!("state.{}", field), &format!("new_{}", field));
                        // Bare field-name reference (e.g.
                        // `constraint interest_rate > 0`). Use word
                        // boundary so `interest_rate_pct` isn't
                        // captured by `interest_rate`.
                        let pat = format!(r"\b{}\b", regex::escape(field));
                        let re = regex::Regex::new(&pat).expect("static regex");
                        expr = re
                            .replace_all(&expr, regex::NoExpand(&format!("new_{}", field)))
                            .into_owned();
                    }
                    format!("\n    (h_c{} : {})", i, expr)
                })
                .collect();

            let with_parts: String = env
                .mutates
                .iter()
                .map(|(name, _)| format!("{} := new_{}", safe_name(name), name))
                .collect::<Vec<_>>()
                .join(", ");
            let post_state = if with_parts.is_empty() {
                "s".to_string()
            } else {
                format!("{{ s with {} }}", with_parts)
            };

            out.push_str(&format!(
                "theorem {}_under_{} (s : State){}{}\n",
                prop.name, env.name, param_sig, constraint_hyps
            ));
            out.push_str(&format!("    (h_inv : {} s) :\n", prop.name));

            // Trivial-preservation shortcut: if no mutated field
            // appears in the property's lean expression, the property
            // holds by reflexivity after the struct update.
            let prop_body_lean = expr_lean(prop_expr, tree_render::LeanCx::guard());
            let mutated_overlap = env.mutates.iter().any(|(field, _)| {
                prop_body_lean.contains(&format!("s.{}", safe_name(field)))
                    || prop_body_lean.contains(&format!("state.{}", field))
            });

            if !mutated_overlap {
                out.push_str(&format!(
                    "    {} {} := by\n  unfold {} at h_inv \u{22A2}; dsimp; exact h_inv\n\n",
                    prop.name, post_state, prop.name
                ));
            } else {
                out.push_str(&format!("    {} {} := sorry\n\n", prop.name, post_state));
            }
        }
    }
}
