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

/// What this handler does to the account, and the facts that follow from
/// it. Three rules that were previously scattered `if is_init` checks are
/// encoded here as reachability, so the illegal combinations cannot be
/// built at all (#311):
///
/// - `has_one` exists only on `Existing`. Anchor allocates and ZEROES an
///   `init` account before evaluating constraints, so a prior-state
///   binding can never hold there — that was #307, where every generated
///   Anchor `init` handler with a matching `auth` field was unopenable.
/// - `token_authority` exists only on `Init`. Anchor and Quasar both
///   reject `token::authority` on an already-existing account.
/// - `mut` exists only on `Existing`. `init` implies `mut`; emitting both
///   trips "mut cannot be provided with init".
pub(crate) enum AccountLifecycle {
    /// Created by this handler.
    Init {
        payer: Option<String>,
        /// `None` on Quasar, whose `init` analogue takes size from the
        /// typed `Account<T>` wrapper.
        space_target: Option<String>,
        token_authority: Option<String>,
    },
    /// Pre-existing when the handler runs.
    Existing {
        writable: bool,
        has_one: Option<String>,
    },
}

/// One canonical decision for how an account's PDA seeds are enforced.
/// Consumers must not re-derive the suppression predicate: the account macro
/// renderer and R28 runtime guard are complementary projections of this enum.
pub(crate) enum SeedPlan {
    /// The account has no declared PDA seeds.
    None,
    /// The target account macro enforces these rendered seed expressions.
    Macro(Vec<String>),
    /// The target macro cannot represent this seed shape; R28 must enforce it.
    Runtime,
}

/// Everything codegen decides about one handler-account, derived once.
///
/// The point is single derivation: before this, the `space =` target, the
/// state struct name, the `has_one` binding, and the `init` decision were
/// each recomputed at their point of use and could disagree — #305 and
/// #307 were both instances of exactly that.
pub(crate) struct AccountPlan {
    pub(crate) lifecycle: AccountLifecycle,
    pub(crate) seeds: SeedPlan,
}

impl AccountPlan {
    /// Derive the plan. This is the ONLY place these facts are decided.
    pub(crate) fn derive(
        acct: &ParsedHandlerAccount,
        handler: &ParsedHandler,
        target: crate::Target,
        spec: &ParsedSpec,
        is_state_account: bool,
    ) -> Self {
        // Infer init from lifecycle. In multi-state specs only the
        // account matching the handler's `on_account` is init'd —
        // sibling writable PDAs in the same handler are pre-existing.
        let lifecycle_is_init = handler.pre_status.as_deref() == Some("Uninitialized")
            || handler.pre_status.as_deref() == Some("Empty");
        let on_account_matches = match handler.on_account.as_deref() {
            // Multi-ACCOUNT spec (≥2 state ADTs): `on_account` is a real
            // per-account discriminator (`Loan.Uninitialized` → only the
            // `loan` account init's). Match the account name.
            Some(adt_name) if spec.account_types.len() > 1 => {
                let lower = adt_name.to_lowercase();
                acct.name == lower || acct.name.starts_with(&lower)
            }
            // Single-account spec written type-qualified
            // (`State.Uninitialized`): `on_account` is `Some("State")` —
            // the sole state TYPE, not an account name — so a name
            // heuristic wrongly rejected a PDA named anything but `state`
            // (e.g. `vault`), and `init/payer/space` never emitted. The
            // resolved state-bearing account (`is_state_account`) is the
            // init target here.
            Some(_) => is_state_account,
            // Unqualified single-state spec: any writable PDA can be the
            // init target (original permissive behavior).
            None => true,
        };
        let is_init =
            lifecycle_is_init && on_account_matches && !acct.is_signer && acct.pda_seeds.is_some();

        let lifecycle = if is_init {
            AccountLifecycle::Init {
                payer: handler.signer_account().map(|s| s.name.clone()),
                space_target: match target {
                    // Shared derivation with `generate_state` — see
                    // `state_struct_name` (#305).
                    crate::Target::Anchor => Some(crate::codegen_shared::state_struct_name(
                        spec,
                        handler.on_account.as_deref(),
                    )),
                    _ => None,
                },
                token_authority: acct.authority.clone(),
            }
        } else {
            // R25: lower `auth X` to `has_one = X` when the state-bearing
            // account has a field named X. Without this binding, every
            // handler taking an authority signer is reachable by ANY
            // signer — the signer check verifies "someone signed", not
            // "the right someone".
            //
            // With multi-variant ADT state the auth field often lives in
            // a variant payload (`Active.owner`); Anchor's `has_one`
            // macro cannot reach into the inner enum. Suppress there —
            // Quasar's flat-struct emission keeps every field at top
            // level, so `has_one = field` works.
            let has_one = if is_state_account {
                handler.who.as_ref().and_then(|who| {
                    let reachable = state_account_has_field(acct, spec, who)
                        && !(matches!(target, crate::Target::Anchor)
                            && is_multi_variant_adt_with_field_in_variant(spec, who));
                    reachable.then(|| who.clone())
                })
            } else {
                None
            };
            AccountLifecycle::Existing {
                writable: acct.is_writable,
                has_one,
            }
        };

        Self {
            lifecycle,
            seeds: plan_seeds(acct, handler, target, spec, is_init),
        }
    }
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
    let plan = AccountPlan::derive(acct, handler, target, spec, is_state_account);
    render_account_attr(&plan)
}

/// Render the `#[account(...)]` attribute from the plan. Pure projection:
/// every decision was made in `AccountPlan::derive`.
fn render_account_attr(plan: &AccountPlan) -> String {
    let mut parts: Vec<String> = Vec::new();

    match &plan.lifecycle {
        AccountLifecycle::Existing { writable, has_one } => {
            if *writable {
                parts.push("mut".to_string());
            }
            if let Some(field) = has_one {
                // Emitted after seeds below to preserve attribute order.
                let _ = field;
            }
        }
        AccountLifecycle::Init {
            payer,
            space_target,
            ..
        } => {
            parts.push("init".to_string());
            if let Some(payer) = payer {
                parts.push(format!("payer = {payer}"));
            }
            // Anchor requires `space = <bytes>` with `init`. Every
            // account type derives `InitSpace`, so the canonical form is
            // `space = 8 + <AccountStruct>::INIT_SPACE` (8 = Anchor
            // discriminator).
            if let Some(target_struct) = space_target {
                parts.push(format!("space = 8 + {target_struct}::INIT_SPACE"));
            }
        }
    }

    if let SeedPlan::Macro(seeds) = &plan.seeds {
        parts.push(format!("seeds = [{}]", seeds.join(", ")));
        parts.push("bump".to_string());
    }

    match &plan.lifecycle {
        AccountLifecycle::Init {
            token_authority, ..
        } => {
            if let Some(auth) = token_authority {
                parts.push(format!("token::authority = {auth}"));
            }
        }
        AccountLifecycle::Existing { has_one, .. } => {
            if let Some(field) = has_one {
                parts.push(format!("has_one = {field}"));
            }
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("    #[account({})]\n", parts.join(", "))
    }
}

/// Decide once whether the macro or R28 owns PDA enforcement for this account.
fn plan_seeds(
    acct: &ParsedHandlerAccount,
    handler: &ParsedHandler,
    target: crate::Target,
    spec: &ParsedSpec,
    is_init: bool,
) -> SeedPlan {
    let Some(seeds) = acct.pda_seeds.as_ref() else {
        return SeedPlan::None;
    };
    {
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

        if suppress_seeds {
            return SeedPlan::Runtime;
        }
        SeedPlan::Macro(
            seeds
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
                .collect(),
        )
    }
}
