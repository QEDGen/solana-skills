//! `#[account(...)]` attribute emission for the Anchor / Quasar scaffold.
//!
//! Moved out of `check/model.rs` (which stays a pure data model): these are
//! codegen concerns — they read `crate::Target` and the codegen_shared
//! helpers, and their sole scaffold caller lives in `scaffold.rs`.

use super::*;
use crate::check::ParsedHandlerAccount;

/// True iff the spec is a multi-variant ADT, the field lives inside a variant
/// payload (not on the wrapper), and the spec opted into wrapper-struct +
/// inner-enum codegen (ADT state repr).
///
/// Used by R25's `auth X → has_one = X` lowering and `emit_variant_auth_guard`
/// to decide whether the auth field is reachable from the Anchor wrapper. On
/// the flat-struct path every field sits directly on the wrapper, so `has_one`
/// works and a variant-destructure guard would reference a non-existent `inner` enum.
pub(crate) fn is_multi_variant_adt_with_field_in_variant(spec: &ParsedSpec, field: &str) -> bool {
    let Some(acct) = spec.account_types.first() else {
        return false;
    };
    if acct.variants.len() <= 1 {
        return false;
    }
    if !spec.state_repr_is_adt() {
        return false;
    }
    acct.variants
        .iter()
        .any(|v| v.fields.iter().any(|(n, _)| n == field))
}

/// True if the state struct backing this handler-account has `field`.
/// Multi-state specs walk `spec.account_types`; single-state specs use the
/// union in `spec.state_fields`. Used by R25's `auth X` → `has_one = X` lowering.
fn state_account_has_field(acct: &ParsedHandlerAccount, spec: &ParsedSpec, field: &str) -> bool {
    // Multi-state: match account name → ADT name (lowercase).
    for at in &spec.account_types {
        let lower = at.name.to_lowercase();
        if acct.name == lower || acct.name.starts_with(&lower) {
            return at.fields.iter().any(|(n, _)| n == field);
        }
    }
    // Single-state spec — fields union lives on the spec.
    spec.state_fields.iter().any(|(n, _)| n == field)
}

/// Generate the #[account(...)] attribute for codegen, target-aware.
///
/// Anchor and Quasar both spell the attribute `#[account(...)]` but
/// disagree on:
///
/// - **Pubkey accessor**: Anchor uses `<acct>.key()`; Quasar uses
///   `<acct>.address()`. Quasar's `#[account]` macro also auto-handles
///   bare-ident seeds matching field names (expanding to
///   `<ident>.to_account_view().address().as_ref()`), so Quasar bare
///   idents are preferred over `.key().as_ref()`.
/// - **State-field seeds in non-init handlers**: Anchor's macro evaluates
///   `<pda>.<field>.as_ref()` in a scope where `<pda>` is bound to the
///   parsed account. Quasar re-uses the same expression in a `Bumps::seeds()`
///   method where only `self` is in scope, so `vault.creator.as_ref()`
///   fails with E0425. For Quasar we omit the `seeds = [...]` directive
///   entirely on non-init handlers when seeds reference state fields —
///   `Account<T>`'s owner+discriminator check still protects type
///   confusion. Anchor keeps the original behavior.
pub(crate) fn quasar_account_attr(
    acct: &ParsedHandlerAccount,
    handler: &ParsedHandler,
    state_name: &str,
    target: crate::Target,
    spec: &ParsedSpec,
    is_state_account: bool,
) -> String {
    let _ = state_name;
    let mut parts = Vec::new();

    // Infer init from lifecycle. In multi-state specs only the account
    // matching the handler's `on_account` is init'd — sibling writable
    // PDAs in the same handler are pre-existing.
    let lifecycle_is_init = handler.pre_status.as_deref() == Some("Uninitialized")
        || handler.pre_status.as_deref() == Some("Empty");
    let on_account_matches = match handler.on_account.as_deref() {
        // Multi-state: only the named state account init's.
        Some(adt_name) => {
            let lower = adt_name.to_lowercase();
            acct.name == lower || acct.name.starts_with(&lower)
        }
        // Single-state spec: any writable PDA can be the init target.
        None => true,
    };
    let is_init =
        lifecycle_is_init && on_account_matches && !acct.is_signer && acct.pda_seeds.is_some();

    // `mut` is mutually exclusive with `init` in Anchor (init implies
    // mut) — emitting both trips `mut cannot be provided with init`.
    if acct.is_writable && !is_init {
        parts.push("mut".to_string());
    }

    if is_init {
        parts.push("init".to_string());
        if let Some(signer) = handler.signer_account() {
            parts.push(format!("payer = {}", signer.name));
        }
        // Anchor requires `space = <bytes>` with `init`. We derive
        // `InitSpace` on every account type / inner enum / record, so
        // the canonical form is `space = 8 + <AccountStruct>::INIT_SPACE`
        // (8 = Anchor discriminator). The struct name must match what
        // `generate_state` emits.
        let space_target = match target {
            // Shared derivation with `generate_state` — see
            // `state_struct_name`. Keying on `on_account` alone emitted
            // `<Adt>Account` for single-account specs whose ADT name
            // differs from the program name, against a struct actually
            // named `<Program>Account` (E0433, scaffold did not
            // compile).
            crate::Target::Anchor => {
                crate::codegen_shared::state_struct_name(spec, handler.on_account.as_deref())
            }
            // Quasar handles space differently — its `init`
            // analogue takes size from the typed `Account<T>`
            // wrapper. Skip the `space` attribute on Quasar.
            _ => String::new(),
        };
        if !space_target.is_empty() {
            parts.push(format!("space = 8 + {}::INIT_SPACE", space_target));
        }
    }

    if let Some(ref seeds) = acct.pda_seeds {
        let bound_account_names: std::collections::HashSet<&str> =
            handler.accounts.iter().map(|a| a.name.as_str()).collect();

        // Detect the case-3 (state-field) seeds. For Quasar non-init
        // handlers these don't survive the `Bumps::<acct>_seeds(self)`
        // method generation because `self.<seed>` isn't auto-captured —
        // omit `seeds`/`bump` on the per-handler attribute and rely on
        // owner+discriminator from `Account<T>`.
        let needs_state_field_seed = seeds.iter().any(|seed| {
            let is_literal = seed.starts_with('"') && seed.ends_with('"');
            !is_literal && !bound_account_names.contains(seed.as_str())
        });

        // v2.29 — extend the suppress to Anchor too when the
        // seed references a field that lives in a variant payload
        // of a multi-variant ADT. Anchor's `#[account(seeds =
        // […])]` macro requires syntactic field access; the
        // accessor `inner.<field>()` we emit for multi-variant
        // ADTs returns a `&Pubkey` via a method call which the
        // macro can't parse. Drop the macro-side `seeds = [...]`
        // for those accounts; the generic-guards.rs R28 pass
        // (below) emits a runtime PDA check that uses the
        // accessor directly.
        let anchor_variant_field_seed = matches!(target, crate::Target::Anchor)
            && !is_init
            && needs_state_field_seed
            && is_multi_variant_adt_state(spec)
            && seeds.iter().any(|seed| {
                let is_literal = seed.starts_with('"') && seed.ends_with('"');
                if is_literal || bound_account_names.contains(seed.as_str()) {
                    return false;
                }
                // Is this a variant-payload field?
                spec.account_types.iter().any(|a| {
                    a.variants
                        .iter()
                        .any(|v| v.fields.iter().any(|(n, _)| n == seed))
                })
            });
        let suppress_seeds =
            (matches!(target, crate::Target::Quasar) && !is_init && needs_state_field_seed)
                || anchor_variant_field_seed;

        if !suppress_seeds {
            let seed_parts: Vec<String> = seeds
                .iter()
                .map(|seed| {
                    if let Some(inner) = seed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                        format!("b\"{}\"", inner)
                    } else if bound_account_names.contains(seed.as_str()) {
                        // Quasar auto-handles bare idents matching field
                        // names; Anchor needs the explicit `.key().as_ref()`
                        // call.
                        match target {
                            crate::Target::Quasar => seed.clone(),
                            _ => format!("{}.key().as_ref()", seed),
                        }
                    } else {
                        // State-field seed (only reached on Anchor or on
                        // init handlers — non-init Quasar suppresses the
                        // whole seeds directive above).
                        format!("{}.{}.as_ref()", acct.name, seed)
                    }
                })
                .collect();
            parts.push(format!("seeds = [{}]", seed_parts.join(", ")));
            parts.push("bump".to_string());
        }
    }

    // `token::authority = X` is only valid on accounts that are also
    // `init` / `init_if_needed` — quasar (and anchor) reject it on
    // already-existing accounts. The spec authority annotation
    // captures "this token account should belong to this authority";
    // for non-init accounts that's already enforced at init time and
    // doesn't need re-emission. For init accounts we emit it so the
    // macro can wire up the SPL InitToken CPI correctly.
    if is_init {
        if let Some(ref auth) = acct.authority {
            parts.push(format!("token::authority = {}", auth));
        }
    }

    // R25: lower `auth X` to `has_one = X` when the state-bearing
    // account has a field named X. Without this binding, every handler
    // taking an authority signer is reachable by ANY signer — the
    // signer check verifies "someone signed", not "the right someone".
    // Anchor and Quasar both accept `has_one = field`.
    //
    // With multi-variant ADT state the auth field often lives in a
    // variant payload (`Active.owner`); Anchor's `has_one` macro can't
    // reach into the inner enum ("no field `owner` on `Account<…>`").
    // Skip emission there — the auth gap surfaces via a TODO line next
    // to the handler body rather than being dropped silently.
    if is_state_account {
        if let Some(ref who) = handler.who {
            if state_account_has_field(acct, spec, who) {
                // Suppress only on Anchor: its wrapper-struct +
                // inner-enum emission hides variant-payload fields from
                // `has_one`; Quasar's flat-struct emission keeps every
                // field at top level so `has_one = field` works.
                let suppress_for_anchor_variant = matches!(target, crate::Target::Anchor)
                    && is_multi_variant_adt_with_field_in_variant(spec, who);
                if !suppress_for_anchor_variant {
                    parts.push(format!("has_one = {}", who));
                }
            }
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("    #[account({})]\n", parts.join(", "))
    }
}
