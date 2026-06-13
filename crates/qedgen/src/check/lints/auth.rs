//! Authorization and value-transfer lints: the R25 `auth → has_one`
//! predicate plus the unbound-auth and unconditional-value-transfer rules.

use super::*;

/// True iff this handler's `auth X` will be lowered to `has_one = X` by
/// R25 — that is, `X` is a field on a state account this handler
/// touches. Used by terminal-transition and value-transfer lints to
/// avoid false positives on auth-bound handlers (the signer identity
/// IS the gate).
pub(crate) fn r25_will_bind_auth(handler: &ParsedHandler, spec: &ParsedSpec) -> bool {
    let Some(ref who) = handler.who else {
        return false;
    };
    if spec.account_types.is_empty() {
        return spec.state_fields.iter().any(|(n, _)| n == who);
    }
    spec.account_types
        .iter()
        .any(|at| at.fields.iter().any(|(n, _)| n == who))
}

// ============================================================================
// Spec-authoring lints (audit follow-up)
//
// Complement codegen fixes R25–R28 by surfacing the *spec shapes* behind
// under-specified auth, value transfer, and lifecycle transitions. Each
// lint maps 1:1 to a post-codegen audit finding; catching them at check
// time means routine spec gaps don't wait for an auditor invocation.
// ============================================================================

/// `[unbound_auth]` — `auth X` doesn't match a state field, so codegen's
/// `auth → has_one` lowering (R25) can't fire. The signer check verifies
/// "someone signed," not "the right someone."
///
/// Closed by R25 when `X` IS a state field. Catches the percolator-CRIT
/// shape — auth name without a state-side anchor.
pub(crate) fn check_unbound_auth(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        if handler.permissionless {
            continue;
        }
        let Some(ref who) = handler.who else {
            // `no_access_control` already covers the no-auth case; don't
            // double-flag.
            continue;
        };
        // Skip handlers without a discoverable state account — single-
        // signer admin handlers without state aren't this lint's target.
        if handler.accounts.is_empty() {
            continue;
        }
        // The state-bearing account in this handler — same logic as
        // codegen.rs::find_state_account, but we only need to know
        // *whether* one exists for field lookup. A handler with multiple
        // state candidates falls back to single-state field set.
        let has_who_field = if spec.account_types.is_empty() {
            spec.state_fields.iter().any(|(n, _)| n == who)
        } else {
            spec.account_types
                .iter()
                .any(|at| at.fields.iter().any(|(n, _)| n == who))
        };
        if has_who_field {
            continue;
        }
        // The auth name might still have a state-side binding via an
        // explicit `requires` clause. If any `requires` references both
        // `who` and a state field, treat the spec as deliberately
        // self-binding and skip the warning.
        let manually_bound = handler
            .requires
            .iter()
            .any(|r| r.lean_expr.contains(who) && r.lean_expr.contains("s."));
        if manually_bound {
            continue;
        }
        // Also accept the dotted-auth desugar / cross-program shape, where
        // the binding clause reads an imported-account field: pattern
        // `<acct>.<field> = <who>.pubkey` (Lean form) with `<acct>` a
        // non-signer account. Covers both the `auth <acct>.<field>` sugar
        // (adapt() rewrites it to this synthesized clause) and the
        // hand-written `requires` longhand.
        let who_pubkey = format!("{who}.pubkey");
        let auth_bound_via_account = handler.requires.iter().any(|r| {
            if !r.lean_expr.contains(&who_pubkey) {
                return false;
            }
            handler
                .accounts
                .iter()
                .any(|a| !a.is_signer && r.lean_expr.contains(&format!("{}.", a.name)))
        });
        if auth_bound_via_account {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unbound_auth".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' declares `auth {who}` but no state field is named `{who}`. R25's `auth → has_one` lowering only fires when the auth name matches a state field — as written, any signer can call this handler against any program-owned account.",
                handler = handler.name,
                who = who,
            ),
            subject: Some(handler.name.clone()),
            fix: format!(
                "Either (a) add `{who} : Pubkey` to the state account so codegen emits `has_one = {who}`, (b) add an explicit `requires state.<field> == {who} else Unauthorized` clause that binds the signer to a stored value, or (c) mark the handler `permissionless` if it's deliberately open.",
                who = who,
            ),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[unconditional_value_transfer]` — handler has a `transfers` clause
/// where the source account is owned by program state (i.e. has
/// `authority X` with X being a handler-bound account that's program-
/// derived), AND the handler has no `requires` clause that constrains
/// who can call it. Catches the lending::liquidate vault-drain shape.
pub(crate) fn check_unconditional_value_transfer(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        for transfer in &handler.transfers {
            // Look up the `from` account in the handler's accounts list.
            // If it has a token authority that points at a writable
            // PDA-typed account in this handler, the source is program-
            // owned.
            let Some(from_acct) = handler.accounts.iter().find(|a| a.name == transfer.from) else {
                continue;
            };
            let Some(ref auth_name) = from_acct.authority else {
                continue;
            };
            let auth_is_program_owned = handler
                .accounts
                .iter()
                .any(|a| &a.name == auth_name && a.is_writable && a.pda_seeds.is_some());
            if !auth_is_program_owned {
                continue;
            }
            // Does the handler have a constraining requires beyond
            // amount-validity? We treat "amount > 0" / "amount < ..." as
            // not constraining caller identity.
            let has_caller_requires = handler.requires.iter().any(|r| {
                let e = &r.lean_expr;
                // Heuristic: caller-binding requires reference state.<field>
                // rather than just the amount param.
                e.contains("s.") || e.contains("state.")
            });
            if has_caller_requires {
                continue;
            }
            // R25 has_one binding counts as a caller gate — escrow::exchange
            // and ::cancel are both auth-bound (`auth taker` / `auth
            // initializer` matching state fields), so the transfer is
            // already gated by signer identity even without an explicit
            // `requires`.
            if r25_will_bind_auth(handler, spec) {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "unconditional_value_transfer".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{handler}' transfers from program-owned `{from}` (authority `{auth}`) with no `requires` clauses constraining who can call it. Value-extracting handlers usually need an authority binding or a precondition that gates the transfer.",
                    handler = handler.name,
                    from = transfer.from,
                    auth = auth_name,
                ),
                subject: Some(handler.name.clone()),
                fix: "Either bind the auth to a state field (so R25 emits `has_one = X`) or add a precondition that gates the transfer (e.g. health check, redemption ratio, allowance). Without one, any signer can extract value from the program-owned account.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            break; // one warning per handler
        }
    }
    warnings
}
