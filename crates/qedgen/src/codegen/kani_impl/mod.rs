//! Impl-targeted Kani harness emission. The spec-model harness checks the
//! spec's translated transition against its own `ensures`; this module
//! emits harnesses that call the user's REAL handler against a symbolic
//! accounts context — a counterexample blames the impl, not the spec.
//! `pre`/`post` in `ParsedEnsures.rust_expr_binary` flatten to
//! harness-local `pre_<field>` / `post_<field>` account-data snapshots
//! (`rewrite_pre_post_paths`).
//!
//! Triggers: explicit `--kani-impl`, or auto when a handler's `modifies`
//! lists fields absent from its effect LHS (the agent-fill signal; the
//! file header names the triggering handlers).
//!
//! CPI ensures-as-fact: callee `ensures` are substituted with call-site
//! expressions (`cpi_substitute::substitute_callee_ensures_rust_binary`)
//! and spliced as `kani::assume(...)` between `if result.is_ok()` and the
//! first caller `assert!`; Tier-0 callees (no ensures) emit nothing — the
//! `cpi_no_callee_ensures` lint surfaces the gap at check time.
//!
//! Per-target shapes: Anchor / Quasar — symbolic `crate::<Pascal>` accounts
//! struct + `.handler(&mut self, …)` call (the Quasar `Ctx<X>` dispatcher
//! just forwards, so both share `emit_symbolic_accounts_module` +
//! `emit_handler_harness`); Pinocchio — stack-allocated `AccountInfo` via a
//! `#[repr(C)]` layout mirror + transmute calling the real dispatcher;
//! native — not yet emitted (the `generate_from_spec` dispatch is the seam).

pub(crate) use anyhow::Result;
pub(crate) use std::collections::{BTreeMap, BTreeSet};
pub(crate) use std::path::Path;

pub(crate) use crate::check::{self, ParsedHandler, ParsedHandlerAccount, ParsedSpec};
pub(crate) use crate::codegen_shared::{map_type, to_pascal_case, to_snake_case};
pub(crate) use crate::pinocchio_profile::{
    PinocchioAccountRole, PinocchioHandlerProfile, PinocchioLayoutField,
    PinocchioLocalKeyDerivation, PinocchioParamField, PinocchioPdaDerivation, PinocchioPdaSeed,
    PinocchioProofProfile, PinocchioRecordLayout, PinocchioRepeatField,
    PinocchioTokenAccountBinding,
};
pub(crate) use crate::Target;

mod brownfield;
mod frameworks;
mod harness;
mod pinocchio;

pub(crate) use brownfield::*;
pub(crate) use frameworks::*;
pub(crate) use harness::*;
pub(crate) use pinocchio::*;

/// A handler triggers auto-emission when its `modifies` clause lists a
/// field that does NOT appear as any effect LHS — the signal that the
/// agent-fill `todo!()` site is expected to satisfy the contract for that
/// field. Keep in lock step with the scaffold's agent-fill diff logic.
pub fn handler_triggers_impl_harness(handler: &ParsedHandler) -> bool {
    let Some(modifies) = &handler.modifies else {
        return false;
    };
    let effect_lhs: std::collections::BTreeSet<String> = handler
        .effects
        .iter()
        .map(|e| {
            // Strip the array-index suffix so `lp_supply[i]` doesn't
            // false-positive against a bare `lp_supply` in `modifies`.
            let bare = crate::rust_codegen_util::effect_target_base(&e.field);
            bare.to_string()
        })
        .collect();
    modifies.iter().any(|f| !effect_lhs.contains(f))
}

/// Any handler triggers auto-emission (consulted by the CLI when
/// `--kani-impl` wasn't passed). Two conditions:
///   1. Handler `modifies ⊋ effect.lhs`.
///   2. Any `ref_impl` carries potentially-overflowing arithmetic over
///      bounded-numeric params — Lean proves on unbounded `Nat`/`Int`,
///      so Kani is the only surface that catches the `u64`/`i64` overflow.
pub fn spec_triggers_impl_harness(spec: &ParsedSpec) -> bool {
    spec.handlers.iter().any(handler_triggers_impl_harness)
        || spec
            .ref_impls
            .iter()
            .any(crate::check::ref_impl_has_overflow_risk)
}

/// Names of handlers whose `modifies ⊋ effect.lhs` causes the auto-trigger.
/// Surfaces in the generated file's header so the user understands why an
/// impl harness appeared without `--kani-impl`.
fn auto_triggered_handlers(spec: &ParsedSpec) -> Vec<&str> {
    spec.handlers
        .iter()
        .filter(|h| handler_triggers_impl_harness(h))
        .map(|h| h.name.as_str())
        .collect()
}

/// Emit `programs/tests/kani_impl.rs` against the user's real Anchor
/// handlers. `explicit_flag` is true when `--kani-impl` was passed; auto-
/// triggered emission stamps a header comment naming the triggering
/// handlers. Non-Anchor targets (Quasar, Pinocchio) are a clean no-op
/// (no file written) — see the target gate in `generate_from_spec`.
///
/// Per-handler emission is gated on the handler having at least one
/// `ensures` clause — without ensures there's nothing to assert.
/// Harness shape for the struct-based (Anchor / Quasar) targets.
/// `Greenfield` (default) emits the `crate::<Pascal>` accounts context +
/// `accounts.handler(param)` shape — correct for qedgen-generated code.
/// `Brownfield` (Anchor only) emits a **state-struct** harness (symbolic
/// state → apply the real effect / call the real invariant/helper → assert
/// `ensures`) — the shape that actually matches an existing Anchor program,
/// where handlers share one Accounts struct and take `Context<T>` + Args.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KaniImplMode {
    Greenfield,
    Brownfield,
}

pub fn generate(
    spec_path: &Path,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;
    generate_from_spec_with_context(
        &spec,
        output_path,
        explicit_flag,
        target,
        Some(spec_path),
        KaniImplMode::Greenfield,
    )
}

/// `generate` with an explicit harness mode (the CLI's `--kani-impl-brownfield`).
pub fn generate_with_mode(
    spec_path: &Path,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
    mode: KaniImplMode,
) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;
    generate_from_spec_with_context(
        &spec,
        output_path,
        explicit_flag,
        target,
        Some(spec_path),
        mode,
    )
}

/// Same as `generate` but takes a pre-parsed spec. Used by the CLI when
/// it already has a `ParsedSpec` in hand (avoids the second parse).
#[cfg(test)]
pub fn generate_from_spec(
    spec: &ParsedSpec,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
) -> Result<()> {
    generate_from_spec_with_context(
        spec,
        output_path,
        explicit_flag,
        target,
        None,
        KaniImplMode::Greenfield,
    )
}

/// `generate_from_spec` with an explicit harness mode (brownfield tests).
#[cfg(test)]
pub fn generate_from_spec_with_mode(
    spec: &ParsedSpec,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
    mode: KaniImplMode,
) -> Result<()> {
    generate_from_spec_with_context(spec, output_path, explicit_flag, target, None, mode)
}

fn generate_from_spec_with_context(
    spec: &ParsedSpec,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
    spec_path: Option<&Path>,
    mode: KaniImplMode,
) -> Result<()> {
    // Target gate. All three
    // targets now emit; only the harness body shape differs per framework
    // (see the `match target` dispatch below). The gating that follows
    // (auto-trigger, ensures-present, emit-targets) is target-agnostic.
    //   - Anchor    → `crate::<Pascal>` accounts struct + `.handler()`
    //   - Quasar    → same struct-based `.handler()` shape (slice 5); the
    //     `#[program]` / `Ctx<X>` dispatcher just forwards to that method
    //   - Pinocchio → stack-allocated `AccountInfo` via `#[repr(C)]`
    //     layout mirror + transmute (slice 8 M3; the shape validated
    //     by crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/src/kani_impl.rs)
    let auto_handlers = auto_triggered_handlers(spec);

    // Skip emission entirely if neither the explicit flag NOR an auto-
    // trigger applies. Belt-and-suspenders check — the CLI's `want_kani_impl`
    // already gates the call, but keeping the check here lets the regen-drift
    // path call `generate` unconditionally without producing stale files on
    // specs that wouldn't normally emit.
    if !explicit_flag && auto_handlers.is_empty() {
        return Ok(());
    }

    let handlers_with_claims: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
        .filter(|h| !h.ensures.is_empty() || !h.effects.is_empty())
        .collect();

    // No asserted clauses/effects anywhere → nothing meaningful to prove.
    // Auto-trigger could still fire (modifies-only fill without ensures is
    // its own lint), but the harness body needs a concrete postcondition.
    if handlers_with_claims.is_empty() {
        return Ok(());
    }

    // Restrict per-handler emission to handlers that have ensures/effects
    // AND either (a) the explicit flag is on OR (b) the handler itself
    // triggers auto-emission. Without (b), a flag-less invocation with
    // one LP-shape handler in a spec full of other handlers would emit
    // a harness for every handler — noise.
    let emit_targets: Vec<&ParsedHandler> = handlers_with_claims
        .iter()
        .copied()
        .filter(|h| explicit_flag || handler_triggers_impl_harness(h))
        .collect();

    if emit_targets.is_empty() {
        return Ok(());
    }

    crate::codegen_shared::ensure_parent_dir(output_path)?;

    // Body dispatch. The gating above (auto-trigger, ensures-present,
    // emit-targets) is target-agnostic; only the harness body shape
    // differs per framework.
    match target {
        Target::Anchor if mode == KaniImplMode::Brownfield => {
            emit_kani_impl_anchor_brownfield(spec, output_path, &emit_targets, explicit_flag)
        }
        Target::Anchor => emit_kani_impl_struct_framework(
            spec,
            output_path,
            &emit_targets,
            &auto_handlers,
            explicit_flag,
            &ANCHOR_IMPL_FRAMEWORK,
        ),
        Target::Pinocchio => emit_kani_impl_pinocchio(
            spec,
            output_path,
            &emit_targets,
            &auto_handlers,
            explicit_flag,
            spec_path,
        ),
        Target::Quasar => emit_kani_impl_struct_framework(
            spec,
            output_path,
            &emit_targets,
            &auto_handlers,
            explicit_flag,
            &QUASAR_IMPL_FRAMEWORK,
        ),
    }
}

#[cfg(test)]
mod tests;
