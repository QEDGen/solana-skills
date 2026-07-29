//! What `src/errors.rs` will actually declare (#363).
//!
//! Error-variant DECLARATION and USE were derived independently. The enum
//! emitter built its set from `error_codes` plus three synthesis predicates
//! that lived inside `emit_errors` as locals, so no other emitter could ask
//! what the enum would contain. Each one then had to guess, and they guessed
//! differently:
//!
//! - checked arithmetic named `MathOverflow` whether or not the spec
//!   declared it, and the enum did not carry it, so the program did not
//!   compile while `check` reported `0 error(s)`;
//! - `integration_test::authorization_error` went the other way and
//!   deliberately UNDER-approximated, refusing to name `InvalidLifecycle`
//!   because it could not tell whether the enum would carry it — so a spec
//!   whose enum does carry it still got a weakened negative assertion.
//!
//! One resolver here, consumed by every site that needs the answer.
//!
//! ## What this is not
//!
//! Not the input to the `unknown_error_variant` lint. That lint reports
//! USER-WRITTEN names absent from `type Error | …`, and it is right to
//! ignore this function: a misspelled `else Unathorized` must stay an error
//! rather than become a synthesized variant no guard ever raises. Codegen's
//! own defaults below are synthesized precisely because they are not typos.

use super::*;

/// Every variant `emit_errors` will declare, in emission order.
///
/// Derived from `ParsedSpec` alone (the MIR's `errors.variants` is a clone of
/// `error_codes`), so callers that never build a `Mir` can still ask.
pub(crate) fn emitted_error_variants(spec: &ParsedSpec, target: Target) -> Vec<String> {
    let mut codes = spec.error_codes.clone();
    let push = |name: &str, codes: &mut Vec<String>| {
        if !codes.iter().any(|c| c == name) {
            codes.push(name.to_string());
        }
    };

    if needs_lifecycle(spec) {
        push("InvalidLifecycle", &mut codes);
    }
    if needs_invalid_pda(spec, target) {
        push("InvalidPda", &mut codes);
    }
    for variant in checked_arith_error_variants_in_use(spec) {
        push(&variant, &mut codes);
    }
    codes
}

/// R26: a non-init lifecycle pre-status auto-adds `InvalidLifecycle`.
pub(crate) fn needs_lifecycle(spec: &ParsedSpec) -> bool {
    spec.handlers.iter().any(|h| {
        let pre = h.pre_status.as_deref().unwrap_or("");
        let is_init = matches!(pre, "Uninitialized" | "Empty");
        !pre.is_empty() && !is_init
    })
}

/// R28: runtime PDA verification auto-adds `InvalidPda`. Both this and guard
/// emission consume the account plan's `SeedPlan`, so the variant cannot
/// drift from the generated check.
pub(crate) fn needs_invalid_pda(spec: &ParsedSpec, target: Target) -> bool {
    !matches!(target, Target::Pinocchio)
        && spec.handlers.iter().any(|h| {
            let state_acct = resolve_handler_state_account(h, spec);
            h.accounts.iter().any(|acct| {
                let is_state = state_acct.map(|sa| sa.name == acct.name).unwrap_or(false);
                let plan = AccountPlan::derive(acct, h, target, spec, is_state);
                matches!(plan.seeds, SeedPlan::Runtime)
            })
        })
}

/// Will the generated enum carry `name`?
pub(crate) fn declares_error_variant(spec: &ParsedSpec, target: Target, name: &str) -> bool {
    emitted_error_variants(spec, target)
        .iter()
        .any(|c| c == name)
}
