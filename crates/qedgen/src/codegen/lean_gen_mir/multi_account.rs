use super::*;
use crate::obligations::{ObligationKind, ObligationRecorder, UnsupportedReason};

/// Multi-account renderer. Per `type <Account>`: `<Account>State` struct,
/// `<Account>Status` inductive, transitions, `<Account>Operation` +
/// `apply<Account>Op`, with CPI theorems interleaved in the owning
/// account's block. Invariants lower as structured comments (variant-typed
/// binders need a richer lowering — v3.0). Properties group by the account
/// whose fields they touch; aborts / ensures / overflow emit per account;
/// covers / liveness / environments bind to the primary account
/// (cross-account cover traces emit skip-comments).
///
/// Strategy: build a per-account *scoped Mir*, call the existing
/// single-account section emitters, then rewrite the bare identifiers
/// (`State`, `Status`, `Operation`, `applyOp`, `applyOps`) to per-account
/// form via `rename_state_idents` — avoids duplicating every emitter.
pub(super) fn render_multi_account(mir: &Mir, rec: &mut ObligationRecorder) -> String {
    let mut out = String::new();
    emit_header(&mut out, mir);
    emit_namespace_open(&mut out, mir);
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);
    emit_constants(&mut out, mir);

    // Pass 1 — per-account: Status, State, Transitions, CPI theorems,
    // Operation + applyOp.
    for acct in &mir.account_states {
        let scoped = scope_mir_to_account(mir, acct);
        if scoped.handlers.is_empty() {
            rec.unsupported(
                ObligationKind::AccountModel,
                &acct.name,
                &acct.name,
                UnsupportedReason::AccountHasNoHandlers,
            );
            continue;
        }
        let mut block = String::new();
        emit_lifecycle_marker(&mut block, &scoped);
        emit_state_struct(&mut block, &scoped);
        emit_transitions(&mut block, &scoped);
        let _pinned = emit_cpi_theorems(&mut block, &scoped, rec);
        emit_operation_inductive(&mut block, &scoped);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }

    // Invariants — multi-account translation deferred; emit as
    // structured comments.
    emit_invariants_as_comments(&mut out, mir);

    emit_properties_multi(&mut out, mir, rec);

    // Pass 2 — per-account: abort theorems, ensures, frame, overflow.
    // Overflow needs each account's properties on the scoped Mir so the
    // `h_inv_<prop>` hypothesis threads correctly.
    let prop_groups = group_properties_by_account(mir);
    for acct in &mir.account_states {
        let mut scoped = scope_mir_to_account(mir, acct);
        if scoped.handlers.is_empty() {
            // Same identity as the pass-1 record — the duplicate collapse
            // keeps this a single entry.
            rec.unsupported(
                ObligationKind::AccountModel,
                &acct.name,
                &acct.name,
                UnsupportedReason::AccountHasNoHandlers,
            );
            continue;
        }
        if let Some(props) = prop_groups.get(&acct.name) {
            scoped.properties = props.clone();
        }
        let mut block = String::new();
        emit_aborts_if(&mut block, &scoped, rec);
        emit_ensures(&mut block, &scoped, rec);
        emit_frame_conditions(&mut block, &scoped, rec);
        emit_overflow(&mut block, &scoped, rec);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }

    // Spec-level covers: cross-account traces become skip-comments;
    // single-account traces emit through the regular cover-witness
    // machinery scoped to the primary account.
    let primary = &mir.account_states[0];
    let primary_scoped = scope_mir_to_account(mir, primary);
    {
        let mut tail = String::new();
        emit_covers_multi(&mut tail, mir, &primary_scoped, rec);
        out.push_str(&rename_state_idents(&tail, &primary.name));
    }

    // Liveness binds to the account owning the via-ops (resolved via
    // `via_ops[0].on_account`).
    emit_liveness_multi(&mut out, mir, rec);

    // Environments — each property × environment cross emits its
    // preservation theorem against the account-scoped state type.
    emit_environments_multi(&mut out, mir, rec);

    emit_namespace_close(&mut out, mir);
    out
}

/// Group properties by the account whose fields they touch. Same
/// heuristic as `emit_properties_multi` but returned as a map so the
/// pass-2 overflow theorems can re-use it.
pub(super) fn group_properties_by_account(
    mir: &Mir,
) -> std::collections::BTreeMap<String, Vec<crate::mir::PropertyMir>> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<crate::mir::PropertyMir>> = BTreeMap::new();
    if mir.account_states.is_empty() {
        return groups;
    }
    let primary_name = mir.account_states[0].name.clone();
    for prop in &mir.properties {
        let target = if let Some(expr) = &prop.expression {
            let lean = expr_lean(expr, tree_render::LeanCx::guard());
            mir.account_states
                .iter()
                .find(|a| {
                    a.fields
                        .iter()
                        .any(|f| lean.contains(&format!("s.{}", f.name)))
                })
                .map(|a| a.name.clone())
                .unwrap_or_else(|| primary_name.clone())
        } else {
            primary_name.clone()
        };
        groups.entry(target).or_default().push(prop.clone());
    }
    groups
}

/// Per-liveness account resolution + section emit. The header is
/// emitted once at the top; each liveness then runs through the
/// existing single-account `emit_liveness` against a Mir scoped to its
/// owning account, with token renames applied to the per-liveness
/// block.
pub(super) fn emit_liveness_multi(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    if mir.liveness_props.is_empty() || mir.account_states.is_empty() {
        return;
    }

    let by_handler: std::collections::HashMap<String, Option<String>> = mir
        .handlers
        .iter()
        .map(|h| (h.name.clone(), h.on_account.clone()))
        .collect();
    let primary_name = mir.account_states[0].name.clone();

    let resolve = |via_ops: &[String]| -> String {
        if let Some(first) = via_ops.first() {
            if let Some(Some(acct)) = by_handler.get(first) {
                return acct.clone();
            }
        }
        primary_name.clone()
    };

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Liveness properties \u{2014} bounded reachability (leads-to)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    let mut emitted_helpers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for liveness in &mir.liveness_props {
        let acct_name = resolve(&liveness.via_ops);
        let acct = match mir.account_states.iter().find(|a| a.name == acct_name) {
            Some(a) => a,
            None => {
                // Structural gap: the via-ops resolved to an account name
                // with no scoped state — the theorem silently disappears
                // without this record.
                rec.failed(
                    ObligationKind::Liveness,
                    "file",
                    &liveness.name,
                    "liveness via-ops resolve to an account not present in account_states",
                );
                continue;
            }
        };
        let mut scoped = scope_mir_to_account(mir, acct);
        scoped.liveness_props = vec![liveness.clone()];

        let mut block = String::new();
        emit_liveness_inner_body(&mut block, &scoped, &mut emitted_helpers, &acct.name, rec);
        out.push_str(&block);
    }
}

/// Emit the body of one liveness theorem against a scoped Mir,
/// tracking which `apply<Account>Ops` helpers we've already emitted so
/// we don't repeat them. The helper itself + the theorem are written
/// with bare `State` / `Operation` / `applyOp` identifiers, then
/// renamed in one pass at the end. Theorem rendering is the shared
/// `emit_liveness_body` (single source with `emit_liveness_inner`).
pub(super) fn emit_liveness_inner_body(
    out: &mut String,
    scoped: &Mir,
    emitted_helpers: &mut std::collections::BTreeSet<String>,
    account_name: &str,
    rec: &mut ObligationRecorder,
) {
    // Buffer raw output (bare identifiers) so we can rename before pushing
    // to the caller. The applyOps helper emits at most once per account.
    let mut buf = String::new();
    let helper_key = format!("apply{}Ops", account_name);
    if !emitted_helpers.contains(&helper_key) {
        buf.push_str(
            "def applyOps (s : State) (signer : Pubkey) : List Operation \u{2192} Option State\n",
        );
        buf.push_str("  | [] => some s\n");
        buf.push_str("  | op :: ops => match applyOp s signer op with\n");
        buf.push_str("    | some s' => applyOps s' signer ops\n");
        buf.push_str("    | none => none\n\n");
        emitted_helpers.insert(helper_key);
    }

    // Shared theorem rendering; the section header (already written by
    // the caller) and the applyOps helper (managed above) are skipped.
    emit_liveness_body(&mut buf, scoped, /* adt_form */ false, rec);

    out.push_str(&rename_state_idents(&buf, account_name));
}

/// Multi-account environment emit. Each property × environment cross
/// emits a preservation theorem against the property's owning account
/// (grouped by the account whose fields the property touches).
pub(super) fn emit_environments_multi(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    if mir.environments.is_empty() || mir.properties.is_empty() {
        return;
    }

    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Environment \u{2014} properties hold under external state changes\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    let groups = group_properties_by_account(mir);
    for acct in &mir.account_states {
        let props = match groups.get(&acct.name) {
            Some(p) => p,
            None => continue,
        };
        let mut scoped = scope_mir_to_account(mir, acct);
        scoped.properties = props.clone();
        scoped.environments = mir.environments.clone();
        let mut block = String::new();
        emit_environments_body(&mut block, &scoped, rec);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }
}

/// Build a Mir whose `state` is a single `StateAdt` derived from the
/// given account, whose `handlers` are filtered to those targeting
/// this account (per-handler `on_account` match, with the primary
/// account also collecting handlers that didn't qualify). Used by
/// `render_multi_account` to drive the existing single-account
/// emitters per-account.
pub(super) fn scope_mir_to_account(mir: &Mir, acct: &crate::mir::AccountStateMir) -> Mir {
    let is_primary = mir
        .account_states
        .first()
        .map(|a| a.name == acct.name)
        .unwrap_or(false);

    let handlers: Vec<crate::mir::HandlerMir> = mir
        .handlers
        .iter()
        .filter(|h| match &h.on_account {
            Some(name) => name == &acct.name,
            None => is_primary,
        })
        .cloned()
        .collect();

    // Build a StateAdt for this account: variants from the ADT decl
    // (when present), else a synthetic single-variant carrying the
    // flat-record fields. lifecycle_states drives the `Status` emit.
    let state = if !acct.variants.is_empty() {
        crate::mir::StateAdt {
            variants: acct.variants.clone(),
            lifecycle_states: acct.lifecycle_states.clone(),
        }
    } else {
        crate::mir::StateAdt {
            variants: vec![crate::mir::StateVariant {
                tag: acct.name.clone(),
                fields: acct.fields.clone(),
            }],
            lifecycle_states: acct.lifecycle_states.clone(),
        }
    };

    Mir {
        name: mir.name.clone(),
        state,
        // Single-account view — scoped emitters that re-enter the
        // dispatch (none do today, but keep is_multi_account honest).
        account_states: vec![acct.clone()],
        accounts: mir.accounts.clone(),
        errors: mir.errors.clone(),
        imports: mir.imports.clone(),
        handlers,
        invariants: Vec::new(), // emit_invariants_as_comments handles
        events: mir.events.clone(),
        constants: Vec::new(), // already emitted at top
        hooks: mir.hooks.clone(),
        uninterpreted_helpers: Vec::new(), // already emitted
        ref_impls: Vec::new(),             // already emitted
        properties: Vec::new(),            // emit_properties_multi handles
        covers: Vec::new(),                // emit_covers_multi handles
        liveness_props: mir.liveness_props.clone(),
        environments: mir.environments.clone(),
        ghosts: mir.ghosts.clone(),
        records: mir.records.clone(),
        is_assembly: mir.is_assembly,
        adt_state: mir.adt_state,
    }
}

/// Rewrite bare type / function identifiers (`State`, `Status`,
/// `Operation`, `applyOp`, `applyOps`) to their per-account form
/// (`PoolState`, `PoolStatus`, `PoolOperation`, `applyPoolOp`,
/// `applyPoolOps`). Word-boundary regex protects field names that
/// happen to share a prefix.
///
/// Safe because the renamed identifiers are emitter-internal type and
/// function names: spec field names are lowercase by convention, and
/// the type names (`State`, `Status`, `Operation`) never appear as
/// values inside Lean expressions emitted by these helpers.
pub(super) fn rename_state_idents(text: &str, account_name: &str) -> String {
    let renames: [(&str, String); 5] = [
        (r"\bapplyOps\b", format!("apply{}Ops", account_name)),
        (r"\bapplyOp\b", format!("apply{}Op", account_name)),
        (r"\bOperation\b", format!("{}Operation", account_name)),
        (r"\bStatus\b", format!("{}Status", account_name)),
        (r"\bState\b", format!("{}State", account_name)),
    ];

    let mut out = text.to_string();
    for (pat, replacement) in &renames {
        let re = regex::Regex::new(pat).expect("static regex");
        out = re
            .replace_all(&out, regex::NoExpand(replacement))
            .into_owned();
    }
    out
}

/// Emit declared invariants as structured comments. Multi-account
/// variant-typed invariant bodies (e.g. `forall l : Loan.Active, …`) need
/// a richer lowering pass (v3.0); comments preserve name + body for
/// visibility.
pub(super) fn emit_invariants_as_comments(out: &mut String, mir: &Mir) {
    for inv in &mir.invariants {
        out.push_str(&format!(
            "-- INVARIANT OBLIGATION (declared, multi-account translation deferred): {}\n",
            inv.name
        ));
        if let Some(body) = &inv.body {
            out.push_str(&format!(
                "--   predicate body: {}\n",
                expr_lean(&body.0, tree_render::LeanCx::guard())
            ));
        }
        if !inv.doc.is_empty() {
            out.push_str(&format!("--   description: {}\n", inv.doc));
        }
        out.push_str("-- v2.14 emits this as a comment; multi-account invariant\n");
        out.push_str("-- bodies (e.g. `forall l : Loan.Active, ...`) need lowering\n");
        out.push_str("-- to typed-state-with-status-filter form. v2.15 picks it up.\n\n");
    }
}

/// Group properties by which account's fields they reference (via
/// `group_properties_by_account`), then emit each group through the
/// per-account scoped path.
pub(super) fn emit_properties_multi(out: &mut String, mir: &Mir, rec: &mut ObligationRecorder) {
    if mir.properties.is_empty() || mir.account_states.is_empty() {
        return;
    }

    let groups = group_properties_by_account(mir);

    for (acct_name, props) in groups {
        let acct = mir
            .account_states
            .iter()
            .find(|a| a.name == acct_name)
            .expect("account_states contains group key");
        let mut scoped = scope_mir_to_account(mir, acct);
        scoped.properties = props;
        let mut block = String::new();
        emit_properties(&mut block, &scoped, rec);
        out.push_str(&rename_state_idents(&block, &acct.name));
    }
}

/// Emit cover trace theorems, skipping any whose handler sequence targets
/// more than one account; skipped traces emit a structured comment so the
/// spec author can see the obligation was dropped.
pub(super) fn emit_covers_multi(
    out: &mut String,
    mir: &Mir,
    primary_scoped: &Mir,
    rec: &mut ObligationRecorder,
) {
    if mir.covers.is_empty() {
        return;
    }

    let by_handler: std::collections::HashMap<String, Option<String>> = mir
        .handlers
        .iter()
        .map(|h| (h.name.clone(), h.on_account.clone()))
        .collect();
    let primary_name = mir.account_states.first().map(|a| a.name.clone());

    // Section header always written when any covers exist, even if every
    // trace ends up as a skip-comment.
    out.push_str(
        "-- ============================================================================\n",
    );
    out.push_str("-- Cover properties \u{2014} reachability (existential proofs)\n");
    out.push_str(
        "-- ============================================================================\n\n",
    );

    let mut kept = Vec::new();
    for c in &mir.covers {
        let mut spans_multi = false;
        'outer: for trace in &c.traces {
            let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for op in trace {
                let acct = by_handler.get(op).and_then(|o| o.clone()).or_else(|| {
                    if by_handler.contains_key(op) {
                        primary_name.clone()
                    } else {
                        None
                    }
                });
                if let Some(a) = acct {
                    seen.insert(a);
                }
            }
            if seen.len() > 1 {
                spans_multi = true;
                break 'outer;
            }
        }
        if spans_multi {
            // Every trace of the skipped cover disappears from the file —
            // record each requested trace obligation instead of letting
            // it vanish (same keys as the emitted path).
            for i in 0..c.traces.len() {
                rec.unsupported(
                    ObligationKind::Cover,
                    "file",
                    &format!("{}::{}", c.name, i),
                    UnsupportedReason::MultiAccountCrossAccountObligation,
                );
            }
            let label: String = c
                .traces
                .first()
                .map(|t| format!("[{}]", t.join(", ")))
                .unwrap_or_else(|| "[]".to_string());
            out.push_str(&format!(
                "-- cover_{}: trace {} spans multiple account types, skipped\n\n",
                c.name, label
            ));
        } else {
            kept.push(c.clone());
        }
    }

    if !kept.is_empty() {
        let mut scoped = primary_scoped.clone();
        scoped.covers = kept;
        // The section header is already written above; render the
        // theorem bodies directly through the shared emitter.
        emit_covers_body(out, &scoped, /* adt_form */ false, rec);
    }
}

// ----------------------------------------------------------------------
// Section emitters (called from render_single_account)
// ----------------------------------------------------------------------
