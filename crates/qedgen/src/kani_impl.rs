//! Impl-targeted Kani harness emission (v2.26 Batch 2 — Track H).
//!
//! The v2.25 ensures-preservation harness (`kani.rs`) verifies the spec's
//! own translated transition fn against its declared `ensures` clauses. That
//! catches spec-internal inconsistency — useful, but doesn't validate that
//! the *user's Rust handler* satisfies the contract.
//!
//! This module emits a parallel harness shape that calls the user's REAL
//! Anchor handler against a symbolic `Accounts` context. Pre/post account-
//! field snapshots replace the spec-model `pre = s.clone()`; the assertion
//! body reuses `ParsedEnsures.rust_expr_binary` so the same `pre.x` / `post.x`
//! rendering applies — but `pre`/`post` are now flat `pre_<field>` /
//! `post_<field>` locals reading from account data instead of `State` copies.
//!
//! ## Triggers (opt-in)
//!
//! 1. User passes `--kani-impl` to `qedgen codegen`.
//! 2. Auto-trigger: any handler has `modifies` listing fields not present in
//!    the effect block's LHS (the v2.25 LP-shape signal indicating the impl
//!    is expected to fill those fields via the agent-fill `todo!()` site).
//!    When auto-triggered, the file header carries a comment naming the
//!    triggering handler(s).
//!
//! ## CPI ensures-as-fact (Track I)
//!
//! When the handler does `call Foo.bar(...)` and the callee declares
//! `ensures`, we splice `kani::assume(<callee_ensures, substituted>)` lines
//! between `if result.is_ok()` and the first caller `assert!`. The
//! substitution maps each callee param to the caller's call-site expression
//! via `crate::cpi_substitute::substitute_callee_ensures_rust_binary` — the
//! same helper `kani.rs`'s spec-model harness uses. The substituted clauses
//! come back in `pre.X` / `post.X` form (from `rust_expr_binary`); we then
//! flatten those to the harness-local `pre_X` / `post_X` snapshots via
//! `rewrite_pre_post_paths`.
//!
//! Tier-0 callees (no `ensures` declared) emit nothing — same fallback as
//! the spec-model variant and `lean_gen.rs::render_cpi_theorems`'s
//! `:= by sorry`. The `cpi_no_callee_ensures` lint surfaces the gap at
//! check time.
//!
//! ## Per-target shapes
//!
//! - Anchor / Quasar — struct-based: build a symbolic `crate::<Pascal>`
//!   accounts struct and call its `handler(&mut self, …)` method (the
//!   Quasar `#[program]` / `Ctx<X>` dispatcher just forwards to that same
//!   method, so the two share `emit_symbolic_accounts_module` +
//!   `emit_handler_harness`). See `emit_kani_impl_quasar` (slice 5).
//! - Pinocchio — stack-allocated `AccountInfo` via a `#[repr(C)]` layout
//!   mirror + transmute, calling the real `process_<handler>` against raw
//!   account slices. See `emit_kani_impl_pinocchio` (slice 8).
//! - native targets — not yet emitted; the per-target dispatch in
//!   `generate_from_spec` is the seam a future arm plugs into.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedHandlerAccount, ParsedSpec};
use crate::codegen_shared::{map_type, to_pascal_case, to_snake_case};
use crate::pinocchio_profile::{
    PinocchioAccountRole, PinocchioHandlerProfile, PinocchioLayoutField,
    PinocchioLocalKeyDerivation, PinocchioParamField, PinocchioPdaDerivation, PinocchioPdaSeed,
    PinocchioProofProfile, PinocchioRecordLayout, PinocchioRepeatField,
    PinocchioTokenAccountBinding,
};
use crate::Target;

/// Predicate: a handler triggers auto-emission of an impl-targeted harness
/// when its `modifies` clause lists at least one field that does NOT appear
/// as the LHS of any effect in its `effect` block. This is the LP-shape
/// signal — the agent-fill `todo!()` site expects the user's Rust impl to
/// satisfy the contract for that field.
///
/// Mirrors the diff logic in `codegen.rs` Phase A so the trigger here and
/// the agent-fill emission there stay in lock step.
pub fn handler_triggers_impl_harness(handler: &ParsedHandler) -> bool {
    let Some(modifies) = &handler.modifies else {
        return false;
    };
    let effect_lhs: std::collections::BTreeSet<String> = handler
        .effects
        .iter()
        .map(|(lhs, _, _)| {
            // Strip array index suffix the same way Phase A does, so
            // `lp_supply[i]` doesn't false-positive against a bare
            // `lp_supply` in `modifies`.
            let bare = crate::rust_codegen_util::effect_target_base(lhs);
            bare.to_string()
        })
        .collect();
    modifies.iter().any(|f| !effect_lhs.contains(f))
}

/// Predicate: any handler in the spec triggers the auto-emission. The CLI
/// consults this before emitting the impl harness file when `--kani-impl`
/// was NOT passed explicitly.
///
/// Two trigger conditions:
///   1. Handler `modifies ⊋ effect.lhs` — the LP-shape signal (Track H).
///   2. Any `ref_impl` carries potentially-overflowing arithmetic over
///      bounded-numeric params (`ref_impl_has_overflow_risk`). Lean
///      proves on unbounded `Nat`/`Int`; Kani is the only verification
///      surface that catches the `u64`/`i64` overflow.
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
pub fn generate(
    spec_path: &Path,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;
    generate_from_spec_with_context(&spec, output_path, explicit_flag, target, Some(spec_path))
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
    generate_from_spec_with_context(spec, output_path, explicit_flag, target, None)
}

fn generate_from_spec_with_context(
    spec: &ParsedSpec,
    output_path: &Path,
    explicit_flag: bool,
    target: Target,
    spec_path: Option<&Path>,
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

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Body dispatch. The gating above (auto-trigger, ensures-present,
    // emit-targets) is target-agnostic; only the harness body shape
    // differs per framework.
    match target {
        Target::Anchor => emit_kani_impl_anchor(
            spec,
            output_path,
            &emit_targets,
            &auto_handlers,
            explicit_flag,
        ),
        Target::Pinocchio => emit_kani_impl_pinocchio(
            spec,
            output_path,
            &emit_targets,
            &auto_handlers,
            explicit_flag,
            spec_path,
        ),
        Target::Quasar => emit_kani_impl_quasar(
            spec,
            output_path,
            &emit_targets,
            &auto_handlers,
            explicit_flag,
        ),
    }
}

/// Emit the Anchor impl-targeted harness: symbolic `Context<X>` +
/// `crate::<Pascal>` accounts struct, calling the user's real handler.
fn emit_kani_impl_anchor(
    spec: &ParsedSpec,
    output_path: &Path,
    emit_targets: &[&ParsedHandler],
    auto_handlers: &[&str],
    explicit_flag: bool,
) -> Result<()> {
    let fp = crate::fingerprint::compute_fingerprint(spec);
    let hash = fp
        .file_hashes
        .get("tests/kani_impl.rs")
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();

    // ── File header ──────────────────────────────────────────────────────
    out.push_str(&crate::banner::banner(None, &hash));
    out.push_str("//\n");
    out.push_str("// Impl-targeted Kani harnesses — call the user's real Anchor handler\n");
    out.push_str("// against a symbolic `Accounts` context and assert the spec's\n");
    out.push_str("// `ensures` clauses against pre/post account-field snapshots.\n");
    out.push_str("//\n");
    out.push_str("// Pairs with `tests/kani.rs` (spec-model harness) — that file checks\n");
    out.push_str("// the spec's effect block satisfies its own ensures; this file checks\n");
    out.push_str("// the user's Rust impl does. A counterexample here blames the impl,\n");
    out.push_str("// not the spec.\n");
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered: the following handlers declare `modifies` fields\n");
        out.push_str("// that are NOT written in their `effect` block (the v2.25 LP-shape\n");
        out.push_str("// signal). The agent-fill `todo!()` site is expected to compute\n");
        out.push_str("// those fields against the spec's ensures; this harness verifies\n");
        out.push_str("// the result.\n");
        for name in auto_handlers {
            out.push_str(&format!("//   - {}\n", name));
        }
        out.push_str("//\n");
        out.push_str("// Pass `--kani-impl` to `qedgen codegen` to force emission for\n");
        out.push_str("// every handler with `ensures`, regardless of the modifies-diff.\n");
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n\n");

    // ── Symbolic-accounts builder module ────────────────────────────────
    //
    // One `build_<handler>()` per emit target. Each builds a fully-symbolic
    // Anchor accounts context: PDA-derived addresses bind to the spec's
    // declared `pda <name> [seeds]`; account-data fields are `kani::any()`.
    //
    // The shape mirrors what `tests/integration_tests.rs` builds at runtime,
    // but with `kani::any()` substituted for concrete keypair init. The
    // user's handler is called via `accounts.handler(<params>)` — same
    // method signature the integration test invokes.
    emit_symbolic_accounts_module(&mut out, spec, emit_targets, "Anchor")?;

    // ── Per-handler proof harnesses ─────────────────────────────────────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Impl-targeted ensures-preservation proofs\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let mut emitted_count = 0;
    for handler in emit_targets {
        for (idx, ensures) in handler.ensures.iter().enumerate() {
            emit_handler_harness(&mut out, handler, idx, ensures, spec)?;
            emitted_count += 1;
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    std::fs::write(output_path, &out)?;

    eprintln!(
        "Generated {} impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );

    Ok(())
}

// ============================================================================
// Quasar impl-targeted harness (slice 5)
// ============================================================================

/// Emit the Quasar impl-targeted harness. The Quasar scaffold emits the
/// user's handler as `impl <Pascal> { pub fn handler(&mut self, …) ->
/// Result<(), ProgramError> }` — byte-identical in shape to the Anchor
/// scaffold — and the `#[program]` mod's `Ctx<X>` dispatcher just forwards
/// `ctx.accounts.handler(…)`. So this harness reuses the same struct-based
/// symbolic-accounts builder (`emit_symbolic_accounts_module`) and
/// per-handler proof emitter (`emit_handler_harness`) as the Anchor
/// branch; only the file header (framework name, placement note, the
/// `Ctx<X>` forwarding context) differs.
///
/// **Placement** (per design doc §11a, target-independent): the harness
/// lives in the program crate's `src/` as `mod kani_impl`, NOT `tests/`.
/// `cargo kani` only discovers `#[kani::proof]` in the lib, and the
/// harness's `crate::<Pascal>` references only resolve from inside the lib
/// crate. The CLI's `redirect_kani_impl_to_src` rewrites the default
/// `…/tests/kani_impl.rs` path to `…/src/kani_impl.rs` for Quasar the same
/// way it does for Pinocchio. Unlike Pinocchio, a Quasar crate is `std`
/// on the host target Kani builds for (its `no_std` cfg gates on
/// `target_os = "solana"`), so no `extern crate kani` is required.
fn emit_kani_impl_quasar(
    spec: &ParsedSpec,
    output_path: &Path,
    emit_targets: &[&ParsedHandler],
    auto_handlers: &[&str],
    explicit_flag: bool,
) -> Result<()> {
    let fp = crate::fingerprint::compute_fingerprint(spec);
    let hash = fp
        .file_hashes
        .get("src/kani_impl.rs")
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();

    // ── File header ──────────────────────────────────────────────────────
    out.push_str(&crate::banner::banner(None, &hash));
    out.push_str("//\n");
    out.push_str("// Impl-targeted Kani harnesses for a Quasar (`#[program]`) program.\n");
    out.push_str("// Calls the user's real handler — the `impl <Pascal> { fn handler\n");
    out.push_str("// (&mut self, …) }` method that the `Ctx<X>` dispatcher forwards to —\n");
    out.push_str("// against a symbolic accounts struct, asserting the spec's `ensures`\n");
    out.push_str("// clauses against pre/post account-field snapshots.\n");
    out.push_str("//\n");
    out.push_str("// Pairs with `tests/kani.rs` (spec-model harness) — that file checks\n");
    out.push_str("// the spec's effect block satisfies its own ensures; this file checks\n");
    out.push_str("// the user's Rust impl does. A counterexample here blames the impl,\n");
    out.push_str("// not the spec.\n");
    out.push_str("//\n");
    out.push_str("// PLACEMENT (per the M1 smoke-test findings, design doc §11a):\n");
    out.push_str("//   1. This file lives in the program crate's `src/` (NOT `tests/`):\n");
    out.push_str("//      `cargo kani` only discovers `#[kani::proof]` in the lib, and\n");
    out.push_str("//      the `crate::<Pascal>` references below only resolve there.\n");
    out.push_str("//   2. Add this line to the crate root (`src/lib.rs`):\n");
    out.push_str("//        #[cfg(kani)] mod kani_impl;\n");
    out.push_str("//      A Quasar crate is `std` on the host target Kani builds for, so\n");
    out.push_str("//      — unlike Pinocchio — no `extern crate kani` is needed.\n");
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered: the following handlers declare `modifies` fields\n");
        out.push_str("// that are NOT written in their `effect` block (the v2.25 LP-shape\n");
        out.push_str("// signal). The agent-fill `todo!()` site is expected to compute\n");
        out.push_str("// those fields against the spec's ensures; this harness verifies\n");
        out.push_str("// the result.\n");
        for name in auto_handlers {
            out.push_str(&format!("//   - {}\n", name));
        }
        out.push_str("//\n");
        out.push_str("// Pass `--kani-impl` to `qedgen codegen` to force emission for\n");
        out.push_str("// every handler with `ensures`, regardless of the modifies-diff.\n");
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n\n");

    // ── Symbolic-accounts builder module (shared with the Anchor branch) ──
    emit_symbolic_accounts_module(&mut out, spec, emit_targets, "Quasar")?;

    // ── Per-handler proof harnesses (shared with the Anchor branch) ──────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Impl-targeted ensures-preservation proofs\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let mut emitted_count = 0;
    for handler in emit_targets {
        for (idx, ensures) in handler.ensures.iter().enumerate() {
            emit_handler_harness(&mut out, handler, idx, ensures, spec)?;
            emitted_count += 1;
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    std::fs::write(output_path, &out)?;

    eprintln!(
        "Generated {} impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );

    Ok(())
}

// ============================================================================
// Pinocchio impl-targeted harness (slice 8 M3)
// ============================================================================

/// Emit the Pinocchio impl-targeted harness. Unlike the Anchor branch
/// (symbolic `Context<X>` + accounts struct), Pinocchio handlers take
/// raw `&[AccountInfo]`, so the harness builds `Account`-layout structs
/// directly on the stack and transmutes pointers into `AccountInfo`.
///
/// The shape is validated by
/// `crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/src/kani_impl.rs` (M2),
/// which caught a real token-overflow bug in 1.1s. The design pivots to
/// stack allocation over the wire-format approach that blew BMC budget.
///
/// **Key correctness lever**: Kani's *automatic* arithmetic-overflow /
/// UB checks run on every path through the real handler. The M2 bug
/// fired as "attempt to add with overflow" before any explicit
/// assertion. So the base harness — build symbolic accounts, call the
/// handler — already catches arithmetic bugs without an explicit
/// `ensures` translation. The spec's `ensures` clauses are emitted as
/// reference comments + an optional agent-fill assertion block.
fn emit_kani_impl_pinocchio(
    spec: &ParsedSpec,
    output_path: &Path,
    emit_targets: &[&ParsedHandler],
    auto_handlers: &[&str],
    explicit_flag: bool,
    spec_path: Option<&Path>,
) -> Result<()> {
    let fp = crate::fingerprint::compute_fingerprint(spec);
    let hash = fp
        .file_hashes
        .get("src/kani_impl.rs")
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();

    // ── File header ──────────────────────────────────────────────────────
    out.push_str(&crate::banner::banner(None, &hash));
    out.push_str("//\n");
    out.push_str("// Impl-targeted Kani harnesses for a Pinocchio (#![no_std]) program.\n");
    out.push_str("// Builds symbolic `AccountInfo` values on the stack and calls the\n");
    out.push_str("// user's real `process_instruction` dispatcher. Kani's automatic\n");
    out.push_str("// overflow / underflow / UB checks run on every path through the\n");
    out.push_str("// real handler — a counterexample blames the impl.\n");
    out.push_str("//\n");
    out.push_str("// PLACEMENT (per the M1 smoke-test findings, design doc §11a):\n");
    out.push_str("//   1. This file lives in the program crate's `src/` (NOT `tests/`):\n");
    out.push_str("//      `cargo kani` only discovers `#[kani::proof]` in the lib, not\n");
    out.push_str("//      in integration tests.\n");
    out.push_str("//   2. Add these two lines to the crate root (`src/lib.rs`):\n");
    out.push_str("//        #[cfg(kani)] extern crate kani;\n");
    out.push_str("//        #[cfg(kani)] mod kani_impl;\n");
    out.push_str("//      The `extern crate kani` is required for `cargo kani` to find\n");
    out.push_str("//      its instrumentation entry in a #![no_std] crate.\n");
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered for handlers with modifies-not-in-effect:\n");
        for name in auto_handlers {
            out.push_str(&format!("//   - {}\n", name));
        }
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n");
    out.push_str("#![allow(dead_code, unused_imports, unused_parens, unused_variables)]\n\n");

    emit_pinocchio_scaffold(&mut out);

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Per-handler proof harnesses\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let profile =
        output_path.parent().and_then(
            |src_dir| match crate::pinocchio_profile::infer_from_context(src_dir, spec_path) {
                Ok(profile) => Some(profile),
                Err(err) => {
                    eprintln!("warning: failed to infer Pinocchio proof profile: {err}");
                    None
                }
            },
        );

    let mut emitted_count = 0;
    for handler in emit_targets {
        let handler_profile = profile.as_ref().and_then(|p| p.handler(&handler.name));
        emit_pinocchio_handler_harness(&mut out, handler, spec, profile.as_ref(), handler_profile)?;
        emitted_count += 1;
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    std::fs::write(output_path, &out)?;
    eprintln!(
        "Generated {} Pinocchio impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );
    Ok(())
}

/// Emit the deterministic shared scaffold: the `Account`-layout mirror,
/// the stack-account container, the SPL Token layout constants, and the
/// build / transmute / read helpers. Byte-for-byte the same regardless
/// of the spec (the per-handler harnesses below reference these).
fn emit_pinocchio_scaffold(out: &mut String) {
    out.push_str(PINOCCHIO_SCAFFOLD);
    out.push('\n');
}

/// The shared scaffold, validated against the M2 reference harness.
const PINOCCHIO_SCAFFOLD: &str = r#"extern crate alloc;

use core::mem::ManuallyDrop;
use pinocchio::account_info::AccountInfo;

/// Layout-mirror of `pinocchio::account_info::Account` (pinocchio 0.8.x).
/// Drift causes immediate UB on first field access; the size assertion
/// catches the common add/remove-field form.
#[repr(C)]
struct AccountLayout {
    borrow_state: u8,
    is_signer: u8,
    is_writable: u8,
    executable: u8,
    original_data_len: u32,
    key: [u8; 32],
    owner: [u8; 32],
    lamports: u64,
    data_len: u64,
}
const _: () = assert!(core::mem::size_of::<AccountLayout>() == 88);

/// 88-byte header followed contiguously by the account's data region.
#[repr(C, align(8))]
struct StackAccount<const DATA_LEN: usize> {
    hdr: AccountLayout,
    data: [u8; DATA_LEN],
}

// SPL Token `TokenAccount` data-region offsets (pinocchio-token 0.3.0).
const TOKEN_MINT_OFF: usize = 0;
const TOKEN_OWNER_OFF: usize = 32;
const TOKEN_AMOUNT_OFF: usize = 64;
const TOKEN_STATE_OFF: usize = 108;
const TOKEN_DATA_LEN: usize = 165;
const MINT_DECIMALS_OFF: usize = 44;
const MINT_STATE_OFF: usize = 45;
const MINT_DATA_LEN: usize = 82;

/// SPL Token program ID — the `from_account_info` owner check target.
const SPL_TOKEN_PROGRAM_ID: [u8; 32] = [
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79,
    0xac, 0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff,
    0x00, 0xa9,
];
const STATE_INITIALIZED: u8 = 1;
/// Pinocchio tracks borrow availability with set bits. At instruction entry,
/// all lamport/data mutable and immutable borrow slots are available.
const BORROW_STATE_CLEAR: u8 = 0xff;

/// Build a stack-resident SPL Token account. `amount` is the field a
/// harness wires up as `kani::any()`.
fn build_token_account(
    key: [u8; 32],
    is_writable: bool,
    is_signer: bool,
    mint_in_data: [u8; 32],
    owner_in_data: [u8; 32],
    amount: u64,
) -> StackAccount<TOKEN_DATA_LEN> {
    let mut acct = StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner: SPL_TOKEN_PROGRAM_ID,
            lamports: 0,
            data_len: TOKEN_DATA_LEN as u64,
        },
        data: [0u8; TOKEN_DATA_LEN],
    };
    write_fixed_32(&mut acct.data, TOKEN_MINT_OFF, mint_in_data);
    write_fixed_32(&mut acct.data, TOKEN_OWNER_OFF, owner_in_data);
    write_fixed_u64(&mut acct.data, TOKEN_AMOUNT_OFF, amount);
    acct.data[TOKEN_STATE_OFF] = STATE_INITIALIZED;
    acct
}

/// Build a stack-resident SPL Token mint. This is enough for
/// `Mint::from_account_info(..)?.decimals()` and initialized-state checks.
fn build_mint_account(key: [u8; 32], is_signer: bool, is_writable: bool, decimals: u8) -> StackAccount<MINT_DATA_LEN> {
    let mut acct = StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner: SPL_TOKEN_PROGRAM_ID,
            lamports: 0,
            data_len: MINT_DATA_LEN as u64,
        },
        data: [0u8; MINT_DATA_LEN],
    };
    acct.data[MINT_DECIMALS_OFF] = decimals;
    acct.data[MINT_STATE_OFF] = STATE_INITIALIZED;
    acct
}

/// Build a non-token account (mint / authority / signer slot). No data
/// region — the handler only reads `is_signer` / `key`.
fn build_minimal_account(key: [u8; 32], is_signer: bool, is_writable: bool) -> StackAccount<0> {
    StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner: [0u8; 32],
            lamports: 0,
            data_len: 0,
        },
        data: [],
    }
}

/// Build a non-token account with an ABI-profiled data region. The data is
/// symbolic in generated harnesses; ABI layout facts only fix the byte length.
fn build_data_account<const DATA_LEN: usize>(
    key: [u8; 32],
    owner: [u8; 32],
    is_signer: bool,
    is_writable: bool,
    data: [u8; DATA_LEN],
) -> StackAccount<DATA_LEN> {
    StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner,
            lamports: 0,
            data_len: DATA_LEN as u64,
        },
        data,
    }
}

/// Transmute a `*mut StackAccount<N>::hdr` to `AccountInfo`.
///
/// SAFETY: `AccountInfo` is `#[repr(C)] struct { raw: *mut Account }` —
/// a single-field pointer wrapper. `StackAccount<N>::hdr` mirrors
/// `Account`'s layout (asserted above). The caller must keep `stack`
/// alive for the lifetime of the returned `AccountInfo`.
unsafe fn account_info_from_stack<const N: usize>(stack: &mut StackAccount<N>) -> AccountInfo {
    let hdr_ptr: *mut AccountLayout = &mut stack.hdr;
    core::mem::transmute::<*mut AccountLayout, AccountInfo>(hdr_ptr)
}

/// Read the `amount` from a stack token account's data region.
fn read_token_amount<const N: usize>(stack: &StackAccount<N>) -> u64 {
    u64::from_le_bytes([
        stack.data[TOKEN_AMOUNT_OFF],
        stack.data[TOKEN_AMOUNT_OFF + 1],
        stack.data[TOKEN_AMOUNT_OFF + 2],
        stack.data[TOKEN_AMOUNT_OFF + 3],
        stack.data[TOKEN_AMOUNT_OFF + 4],
        stack.data[TOKEN_AMOUNT_OFF + 5],
        stack.data[TOKEN_AMOUNT_OFF + 6],
        stack.data[TOKEN_AMOUNT_OFF + 7],
    ])
}

fn read_state_pubkey<const N: usize>(stack: &StackAccount<N>, offset: usize) -> [u8; 32] {
    [
        stack.data[offset],
        stack.data[offset + 1],
        stack.data[offset + 2],
        stack.data[offset + 3],
        stack.data[offset + 4],
        stack.data[offset + 5],
        stack.data[offset + 6],
        stack.data[offset + 7],
        stack.data[offset + 8],
        stack.data[offset + 9],
        stack.data[offset + 10],
        stack.data[offset + 11],
        stack.data[offset + 12],
        stack.data[offset + 13],
        stack.data[offset + 14],
        stack.data[offset + 15],
        stack.data[offset + 16],
        stack.data[offset + 17],
        stack.data[offset + 18],
        stack.data[offset + 19],
        stack.data[offset + 20],
        stack.data[offset + 21],
        stack.data[offset + 22],
        stack.data[offset + 23],
        stack.data[offset + 24],
        stack.data[offset + 25],
        stack.data[offset + 26],
        stack.data[offset + 27],
        stack.data[offset + 28],
        stack.data[offset + 29],
        stack.data[offset + 30],
        stack.data[offset + 31],
    ]
}

fn write_state_pubkey<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: [u8; 32]) {
    write_fixed_32(&mut stack.data, offset, value);
}

fn read_state_bool<const N: usize>(stack: &StackAccount<N>, offset: usize) -> bool {
    stack.data[offset] != 0
}

fn write_state_bool<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: bool) {
    stack.data[offset] = u8::from(value);
}

fn read_state_u8<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u8 {
    stack.data[offset]
}

fn write_state_u8<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u8) {
    stack.data[offset] = value;
}

fn read_state_u16<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u16 {
    u16::from_le_bytes([stack.data[offset], stack.data[offset + 1]])
}

fn write_state_u16<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u16) {
    let bytes = value.to_le_bytes();
    stack.data[offset] = bytes[0];
    stack.data[offset + 1] = bytes[1];
}

fn read_state_u64<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u64 {
    u64::from_le_bytes([
        stack.data[offset],
        stack.data[offset + 1],
        stack.data[offset + 2],
        stack.data[offset + 3],
        stack.data[offset + 4],
        stack.data[offset + 5],
        stack.data[offset + 6],
        stack.data[offset + 7],
    ])
}

fn write_state_u64<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u64) {
    write_fixed_u64(&mut stack.data, offset, value);
}

fn read_state_u128<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u128 {
    u128::from_le_bytes([
        stack.data[offset],
        stack.data[offset + 1],
        stack.data[offset + 2],
        stack.data[offset + 3],
        stack.data[offset + 4],
        stack.data[offset + 5],
        stack.data[offset + 6],
        stack.data[offset + 7],
        stack.data[offset + 8],
        stack.data[offset + 9],
        stack.data[offset + 10],
        stack.data[offset + 11],
        stack.data[offset + 12],
        stack.data[offset + 13],
        stack.data[offset + 14],
        stack.data[offset + 15],
    ])
}

fn write_state_u128<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u128) {
    let bytes = value.to_le_bytes();
    stack.data[offset] = bytes[0];
    stack.data[offset + 1] = bytes[1];
    stack.data[offset + 2] = bytes[2];
    stack.data[offset + 3] = bytes[3];
    stack.data[offset + 4] = bytes[4];
    stack.data[offset + 5] = bytes[5];
    stack.data[offset + 6] = bytes[6];
    stack.data[offset + 7] = bytes[7];
    stack.data[offset + 8] = bytes[8];
    stack.data[offset + 9] = bytes[9];
    stack.data[offset + 10] = bytes[10];
    stack.data[offset + 11] = bytes[11];
    stack.data[offset + 12] = bytes[12];
    stack.data[offset + 13] = bytes[13];
    stack.data[offset + 14] = bytes[14];
    stack.data[offset + 15] = bytes[15];
}

fn write_fixed_32<const N: usize>(data: &mut [u8; N], offset: usize, value: [u8; 32]) {
    data[offset] = value[0];
    data[offset + 1] = value[1];
    data[offset + 2] = value[2];
    data[offset + 3] = value[3];
    data[offset + 4] = value[4];
    data[offset + 5] = value[5];
    data[offset + 6] = value[6];
    data[offset + 7] = value[7];
    data[offset + 8] = value[8];
    data[offset + 9] = value[9];
    data[offset + 10] = value[10];
    data[offset + 11] = value[11];
    data[offset + 12] = value[12];
    data[offset + 13] = value[13];
    data[offset + 14] = value[14];
    data[offset + 15] = value[15];
    data[offset + 16] = value[16];
    data[offset + 17] = value[17];
    data[offset + 18] = value[18];
    data[offset + 19] = value[19];
    data[offset + 20] = value[20];
    data[offset + 21] = value[21];
    data[offset + 22] = value[22];
    data[offset + 23] = value[23];
    data[offset + 24] = value[24];
    data[offset + 25] = value[25];
    data[offset + 26] = value[26];
    data[offset + 27] = value[27];
    data[offset + 28] = value[28];
    data[offset + 29] = value[29];
    data[offset + 30] = value[30];
    data[offset + 31] = value[31];
}

fn write_fixed_u64<const N: usize>(data: &mut [u8; N], offset: usize, value: u64) {
    let bytes = value.to_le_bytes();
    data[offset] = bytes[0];
    data[offset + 1] = bytes[1];
    data[offset + 2] = bytes[2];
    data[offset + 3] = bytes[3];
    data[offset + 4] = bytes[4];
    data[offset + 5] = bytes[5];
    data[offset + 6] = bytes[6];
    data[offset + 7] = bytes[7];
}

fn normalized_fee_decimal_scale(decimals: u64) -> u128 {
    match decimals {
        0 => 1_000_000_000_000_000_000,
        1 => 100_000_000_000_000_000,
        2 => 10_000_000_000_000_000,
        3 => 1_000_000_000_000_000,
        4 => 100_000_000_000_000,
        5 => 10_000_000_000_000,
        6 => 1_000_000_000_000,
        7 => 100_000_000_000,
        8 => 10_000_000_000,
        9 => 1_000_000_000,
        10 => 100_000_000,
        11 => 10_000_000,
        12 => 1_000_000,
        13 => 100_000,
        14 => 10_000,
        15 => 1_000,
        16 => 100,
        17 => 10,
        18 => 1,
        _ => 0,
    }
}
"#;

/// Instruction tag constant expected in the committed Pinocchio crate.
///
/// For arity-specialized spec models such as `batch_16`, strip the numeric
/// suffix so related models dispatch through the same runtime instruction tag
/// (`BATCH`).
fn pinocchio_instruction_tag_const(handler_name: &str) -> String {
    let snake = to_snake_case(handler_name);
    let base = snake
        .rsplit_once('_')
        .and_then(|(prefix, suffix)| suffix.parse::<usize>().ok().map(|_| prefix))
        .unwrap_or(&snake);
    base.to_ascii_uppercase()
}

/// Rust primitive for a numeric DSL type, used to declare the symbolic
/// param so `to_le_bytes()` packs the right width. Returns None for
/// non-numeric / unsupported types.
fn numeric_param_rust_type(dsl_type: &str) -> Option<&'static str> {
    match dsl_type {
        "U8" => Some("u8"),
        "I8" => Some("i8"),
        "U16" => Some("u16"),
        "I16" => Some("i16"),
        "U32" => Some("u32"),
        "I32" => Some("i32"),
        "U64" => Some("u64"),
        "I64" => Some("i64"),
        "U128" => Some("u128"),
        "I128" => Some("i128"),
        _ => None,
    }
}

fn pinocchio_param_decl_type(dsl_type: &str) -> Option<&'static str> {
    numeric_param_rust_type(dsl_type).or(match dsl_type {
        "Pubkey" => Some("[u8; 32]"),
        _ => None,
    })
}

fn pinocchio_emit_profile_field_pack(out: &mut String, field: &PinocchioParamField, ident: &str) {
    if pinocchio_profile_field_is_unsupported(field) {
        out.push_str(&format!(
            "    // TODO: unsupported instruction field `{}` type `{}` at offset {}..{}; use absolute-offset packing before treating this as proof evidence\n",
            field.name,
            pinocchio_unsupported_profile_type(field),
            field.start,
            field.end
        ));
    } else if field.rust_type.eq_ignore_ascii_case("pubkey") {
        out.push_str(&format!(
            "    instruction_data.extend_from_slice(&{});\n",
            ident
        ));
    } else if field.rust_type.eq_ignore_ascii_case("bool") {
        out.push_str(&format!(
            "    instruction_data.extend_from_slice(&[u8::from({} != 0)]);\n",
            ident
        ));
    } else {
        out.push_str(&format!(
            "    instruction_data.extend_from_slice(&({} as {}).to_le_bytes());\n",
            ident, field.rust_type
        ));
    }
}

fn pinocchio_emit_profile_field_write(
    out: &mut String,
    field: &PinocchioParamField,
    ident: &str,
    offset: usize,
) {
    if pinocchio_profile_field_is_unsupported(field) {
        out.push_str(&format!(
            "    // TODO: unsupported instruction field `{}` type `{}` at offset {}..{}; leaving bytes symbolic/zeroed until profile support is added\n",
            field.name,
            pinocchio_unsupported_profile_type(field),
            offset,
            offset + (field.end - field.start)
        ));
    } else if field.rust_type.eq_ignore_ascii_case("pubkey") {
        out.push_str(&format!(
            "    write_fixed_32(&mut instruction_data, {offset}, {});\n",
            ident
        ));
    } else if field.rust_type.eq_ignore_ascii_case("bool") {
        out.push_str(&format!(
            "    instruction_data[{offset}] = u8::from({} != 0);\n",
            ident
        ));
    } else {
        pinocchio_emit_numeric_array_write(
            out,
            "instruction_data",
            offset,
            &format!("({ident} as {})", field.rust_type),
            field.end - field.start,
        );
    }
}

fn pinocchio_profile_field_is_unsupported(field: &PinocchioParamField) -> bool {
    field.rust_type.starts_with("unsupported:")
}

fn pinocchio_unsupported_profile_type(field: &PinocchioParamField) -> &str {
    field
        .rust_type
        .strip_prefix("unsupported:")
        .unwrap_or(&field.rust_type)
}

fn pinocchio_emit_numeric_array_write(
    out: &mut String,
    target: &str,
    offset: usize,
    value_expr: &str,
    width: usize,
) {
    if width == 1 {
        out.push_str(&format!("    {target}[{offset}] = {value_expr} as u8;\n"));
        return;
    }

    let tmp = format!(
        "generated_{}_{}_bytes",
        target
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect::<String>(),
        offset
    );
    out.push_str(&format!("    let {tmp} = {value_expr}.to_le_bytes();\n"));
    for byte_index in 0..width {
        out.push_str(&format!(
            "    {target}[{}] = {tmp}[{byte_index}];\n",
            offset + byte_index
        ));
    }
}

fn pinocchio_emit_fixed_array_bytes_write(
    out: &mut String,
    target: &str,
    offset: usize,
    bytes: &[u8],
) {
    for (byte_index, byte) in bytes.iter().enumerate() {
        out.push_str(&format!(
            "    {target}[{}] = {}u8;\n",
            offset + byte_index,
            byte
        ));
    }
}

fn pinocchio_profile_instruction_data_len(
    handler: &ParsedHandler,
    profile: &PinocchioHandlerProfile,
) -> Option<usize> {
    let mut len = 1usize;
    for param in &profile.params {
        if pinocchio_profile_param_name(handler, &param.name).is_some()
            || pinocchio_profile_field_is_unsupported(param)
        {
            len = len.max(1 + param.end);
        }
    }
    for repeat in &profile.repeats {
        let count = pinocchio_repeat_count(handler, repeat)?;
        len = len.max(1 + repeat.offset + (count * repeat.item_len));
    }
    Some(len)
}

fn emit_pinocchio_instruction_data_fixed_with_profile(
    out: &mut String,
    handler: &ParsedHandler,
    profile: &PinocchioHandlerProfile,
    instruction_tag_expr: &str,
) -> bool {
    let Some(len) = pinocchio_profile_instruction_data_len(handler, profile) else {
        return false;
    };
    if len <= 1 {
        return false;
    }

    out.push_str(&format!("    let mut instruction_data = [0u8; {len}];\n"));
    out.push_str(&format!(
        "    instruction_data[0] = {instruction_tag_expr};\n"
    ));

    for param in &profile.params {
        if pinocchio_profile_field_is_unsupported(param) {
            pinocchio_emit_profile_field_write(out, param, &param.name, 1 + param.start);
        } else if let Some(param_name) = pinocchio_profile_param_name(handler, &param.name) {
            pinocchio_emit_profile_field_write(out, param, &param_name, 1 + param.start);
        } else {
            out.push_str(&format!(
                "    // Profile note: source references param `{}` absent from the spec handler\n",
                param.name
            ));
        }
    }
    for repeat in &profile.repeats {
        let Some(count) = pinocchio_repeat_count(handler, repeat) else {
            return false;
        };
        out.push_str(&format!(
            "    instruction_data[{}] = {count}u8;\n",
            1 + repeat.offset.saturating_sub(1)
        ));
        for index in 0..count {
            for field in &repeat.item_fields {
                let param_name = repeat_item_param_name(handler, &field.name, index, count);
                if param_name.is_none() && !pinocchio_profile_field_is_unsupported(field) {
                    out.push_str(&format!(
                        "    // Profile note: repeat field `{}` item {} absent from the spec handler\n",
                        field.name, index
                    ));
                    continue;
                }
                let offset = 1 + repeat.offset + (index * repeat.item_len) + field.start;
                let ident = param_name
                    .as_deref()
                    .map(to_snake_case)
                    .unwrap_or_else(|| format!("unsupported_{}_{}", field.name, index));
                pinocchio_emit_profile_field_write(out, field, &ident, offset);
            }
        }
    }
    true
}

/// Emit committed Pinocchio dispatcher argument packing.
///
/// The fallback path packs directly declared numeric and Pubkey spec
/// parameters in declared order. Runtime-specific ABI narrowing and
/// repeated-record layouts belong in the source/ABI-derived profile.
fn emit_pinocchio_instruction_data_pack_with_profile(
    out: &mut String,
    handler: &ParsedHandler,
    profile: Option<&PinocchioHandlerProfile>,
) {
    let instruction_tag_expr = if let Some(tag) = profile.and_then(|p| p.instruction_tag) {
        out.push_str(&format!("    let instruction_tag: u8 = {tag}u8;\n"));
        "instruction_tag".to_string()
    } else {
        let tag_const = pinocchio_instruction_tag_const(&handler.name);
        out.push_str(&format!(
            "    let instruction_tag: u8 = crate::{};\n",
            tag_const
        ));
        "instruction_tag".to_string()
    };

    if let Some(profile) = profile {
        if (!profile.params.is_empty() || !profile.repeats.is_empty())
            && emit_pinocchio_instruction_data_fixed_with_profile(
                out,
                handler,
                profile,
                &instruction_tag_expr,
            )
        {
            return;
        }
    }

    out.push_str("    let mut instruction_data = alloc::vec::Vec::new();\n");
    out.push_str("    instruction_data.push(instruction_tag);\n");

    if let Some(profile) = profile {
        if !profile.params.is_empty() || !profile.repeats.is_empty() {
            for param in &profile.params {
                if pinocchio_profile_field_is_unsupported(param) {
                    pinocchio_emit_profile_field_pack(out, param, &param.name);
                } else if let Some(param_name) = pinocchio_profile_param_name(handler, &param.name)
                {
                    pinocchio_emit_profile_field_pack(out, param, &param_name);
                } else {
                    out.push_str(&format!(
                        "    // Profile note: source references param `{}` absent from the spec handler\n",
                        param.name
                    ));
                }
            }
            for repeat in &profile.repeats {
                emit_pinocchio_repeat_pack(out, handler, repeat);
            }
            return;
        }
    }

    for (pname, ptype) in &handler.takes_params {
        match numeric_param_rust_type(ptype) {
            Some(_) => {
                out.push_str(&format!(
                    "    instruction_data.extend_from_slice(&{}.to_le_bytes());\n",
                    to_snake_case(pname)
                ));
            }
            None if ptype == "Pubkey" => {
                out.push_str(&format!(
                    "    instruction_data.extend_from_slice(&{});\n",
                    to_snake_case(pname)
                ));
            }
            None => {
                out.push_str(&format!(
                    "    // TODO: pack param `{}` (spec type {}) into instruction_data\n",
                    pname, ptype
                ));
            }
        }
    }
}

fn pinocchio_profile_param_name(handler: &ParsedHandler, profile_name: &str) -> Option<String> {
    handler
        .takes_params
        .iter()
        .map(|(name, _)| to_snake_case(name))
        .find(|name| {
            name == profile_name
                || name == &format!("{profile_name}_value")
                || name == &format!("new_{profile_name}")
                || name.strip_prefix("new_") == Some(profile_name)
                || name.strip_suffix("_value") == Some(profile_name)
        })
}

fn emit_pinocchio_requires_assumptions(out: &mut String, handler: &ParsedHandler) {
    let param_names: BTreeSet<String> = handler
        .takes_params
        .iter()
        .map(|(name, _)| to_snake_case(name))
        .collect();
    let has_fee_normalization_shape = pinocchio_has_fee_normalization_shape(handler);
    for requires in &handler.requires {
        let expr = requires.rust_expr.trim();
        if expr.is_empty()
            || expr.contains("s.")
            || expr.contains(".pubkey")
            || expr.contains("old(")
        {
            continue;
        }
        if has_fee_normalization_shape
            && (expr.contains("fee_input_normalized")
                || expr.contains("fee_output_normalized")
                || expr.contains("retained_value_bps"))
        {
            continue;
        }
        let state_lowered = pinocchio_lower_simple_state_requires_expr(expr);
        let lowered = pinocchio_lower_param_expr(&state_lowered, &param_names);
        if lowered.contains("state.") || lowered.contains("s.") || lowered.contains(".pubkey") {
            if expr.contains("lane_count") {
                for lane_param in &param_names {
                    let lane_base = strip_numeric_suffix(lane_param)
                        .map_or(lane_param.as_str(), |(base, _)| base);
                    if matches!(lane_base, "lane_id" | "from_lane_id" | "to_lane_id") {
                        out.push_str(&format!("    kani::assume({lane_param} < 2);\n"));
                    }
                }
            }
            continue;
        }
        out.push_str(&format!("    kani::assume({lowered});\n"));
    }
}

fn pinocchio_has_fee_normalization_shape(handler: &ParsedHandler) -> bool {
    let param_names: BTreeSet<String> = handler
        .takes_params
        .iter()
        .map(|(name, _)| to_snake_case(name))
        .collect();
    [
        "amount_in",
        "amount_out",
        "max_fee_bps",
        "input_decimals",
        "output_decimals",
        "fee_input_normalized",
        "fee_output_normalized",
    ]
    .iter()
    .all(|name| param_names.contains(*name))
}

fn pinocchio_is_fee_normalization_ghost_param(handler: &ParsedHandler, name: &str) -> bool {
    pinocchio_has_fee_normalization_shape(handler)
        && matches!(
            name,
            "retained_value_bps" | "fee_input_normalized" | "fee_output_normalized"
        )
}

fn pinocchio_lower_simple_state_requires_expr(expr: &str) -> String {
    expr.replace("state.max_fee_bps", "10000")
        .replace("state.lane_count", "2")
        .replace("state.mint_count", "2")
        .replace("state.paused", "0")
}

fn emit_pinocchio_profile_width_assumptions(
    out: &mut String,
    handler: &ParsedHandler,
    profile: Option<&PinocchioHandlerProfile>,
) {
    let Some(profile) = profile else {
        return;
    };
    for param in &profile.params {
        if pinocchio_profile_field_is_unsupported(param) {
            continue;
        }
        let Some(param_name) = pinocchio_profile_param_name(handler, &param.name) else {
            continue;
        };
        emit_pinocchio_width_assumption_for_param(out, handler, &param_name, &param.rust_type);
    }
    for repeat in &profile.repeats {
        let Some(count) = pinocchio_repeat_count(handler, repeat) else {
            continue;
        };
        for index in 0..count {
            for item in &repeat.item_fields {
                if pinocchio_profile_field_is_unsupported(item) {
                    continue;
                }
                let Some(item_name) = repeat_item_param_name(handler, &item.name, index, count)
                else {
                    continue;
                };
                emit_pinocchio_width_assumption_for_param(
                    out,
                    handler,
                    &item_name,
                    &item.rust_type,
                );
            }
        }
    }
}

fn emit_pinocchio_fee_normalization_assumptions(
    out: &mut String,
    handler: &ParsedHandler,
    handler_profile: Option<&PinocchioHandlerProfile>,
) {
    let param_names: BTreeSet<String> = handler
        .takes_params
        .iter()
        .map(|(name, _)| to_snake_case(name))
        .collect();
    let required = [
        "amount_in",
        "amount_out",
        "max_fee_bps",
        "input_decimals",
        "output_decimals",
        "fee_input_normalized",
        "fee_output_normalized",
    ];
    if required.iter().any(|name| !param_names.contains(*name)) {
        return;
    }
    let has_input_mint = handler.accounts.iter().any(|account| {
        pinocchio_mint_decimal_param(handler_profile, &to_snake_case(&account.name))
            == Some("input_decimals".to_string())
    });
    let has_output_mint = handler.accounts.iter().any(|account| {
        pinocchio_mint_decimal_param(handler_profile, &to_snake_case(&account.name))
            == Some("output_decimals".to_string())
    });
    if !has_input_mint || !has_output_mint {
        return;
    }
    out.push_str(
        "    // Bind fee-normalization ghost params to real amount/mint-decimal inputs.\n",
    );
    out.push_str("    kani::assume(input_decimals <= 18);\n");
    out.push_str("    kani::assume(output_decimals <= 18);\n");
    if let (Some(input_decimals), Some(output_decimals)) = (
        pinocchio_literal_requires_eq(handler, "input_decimals"),
        pinocchio_literal_requires_eq(handler, "output_decimals"),
    ) {
        if input_decimals <= 18 && output_decimals <= 18 {
            let input_scale = 10u128.pow((18 - input_decimals) as u32);
            let output_scale = 10u128.pow((18 - output_decimals) as u32);
            if input_scale == output_scale
                && pinocchio_has_requires_expr(handler, "amount_out >= amount_in")
            {
                out.push_str("    kani::assume(amount_out >= amount_in);\n");
                return;
            }
            if input_scale == output_scale {
                out.push_str(
                    "    let generated_fee_retained_bps = 10000u128 - max_fee_bps as u128;\n",
                );
                out.push_str("    let generated_fee_min_output = ((amount_in as u128) * generated_fee_retained_bps) / 10000u128;\n");
                out.push_str(
                    "    kani::assume((amount_out as u128) >= generated_fee_min_output);\n",
                );
                return;
            }
            out.push_str(&format!(
                "    kani::assume((amount_in as u128) <= u128::MAX / {input_scale}u128);\n"
            ));
            out.push_str(&format!(
                "    kani::assume((amount_out as u128) <= u128::MAX / {output_scale}u128);\n"
            ));
            out.push_str(&format!(
                "    let generated_fee_input_normalized = (amount_in as u128) * {input_scale}u128;\n"
            ));
            out.push_str(&format!(
                "    let generated_fee_output_normalized = (amount_out as u128) * {output_scale}u128;\n"
            ));
        } else {
            out.push_str("    let generated_fee_input_scale = normalized_fee_decimal_scale(input_decimals as u64);\n");
            out.push_str("    let generated_fee_output_scale = normalized_fee_decimal_scale(output_decimals as u64);\n");
            out.push_str("    kani::assume(generated_fee_input_scale != 0);\n");
            out.push_str("    kani::assume(generated_fee_output_scale != 0);\n");
            out.push_str(
                "    kani::assume((amount_in as u128) <= u128::MAX / generated_fee_input_scale);\n",
            );
            out.push_str("    kani::assume((amount_out as u128) <= u128::MAX / generated_fee_output_scale);\n");
            out.push_str("    let generated_fee_input_normalized = (amount_in as u128) * generated_fee_input_scale;\n");
            out.push_str("    let generated_fee_output_normalized = (amount_out as u128) * generated_fee_output_scale;\n");
        }
    } else {
        out.push_str(
            "    let generated_fee_input_scale = normalized_fee_decimal_scale(input_decimals as u64);\n",
        );
        out.push_str(
            "    let generated_fee_output_scale = normalized_fee_decimal_scale(output_decimals as u64);\n",
        );
        out.push_str("    kani::assume(generated_fee_input_scale != 0);\n");
        out.push_str("    kani::assume(generated_fee_output_scale != 0);\n");
        out.push_str(
            "    kani::assume((amount_in as u128) <= u128::MAX / generated_fee_input_scale);\n",
        );
        out.push_str(
            "    kani::assume((amount_out as u128) <= u128::MAX / generated_fee_output_scale);\n",
        );
        out.push_str("    let generated_fee_input_normalized = (amount_in as u128) * generated_fee_input_scale;\n");
        out.push_str("    let generated_fee_output_normalized = (amount_out as u128) * generated_fee_output_scale;\n");
    }
    out.push_str("    let generated_fee_retained_bps = 10000u128 - max_fee_bps as u128;\n");
    out.push_str("    kani::assume(generated_fee_input_normalized <= u128::MAX / 10000u128);\n");
    out.push_str("    let Some(generated_fee_product) = generated_fee_input_normalized.checked_mul(generated_fee_retained_bps) else {\n");
    out.push_str("        kani::assume(false);\n");
    out.push_str("        return;\n");
    out.push_str("    };\n");
    out.push_str("    let generated_fee_threshold_holds = match generated_fee_output_normalized.checked_add(1u128) {\n");
    out.push_str("        Some(generated_fee_next_output) => match generated_fee_next_output.checked_mul(10000u128) {\n");
    out.push_str(
        "            Some(generated_fee_threshold) => generated_fee_product < generated_fee_threshold,\n",
    );
    out.push_str("            None => true,\n");
    out.push_str("        },\n");
    out.push_str("        None => true,\n");
    out.push_str("    };\n");
    out.push_str("    kani::assume(generated_fee_threshold_holds);\n");
}

fn pinocchio_has_requires_expr(handler: &ParsedHandler, expected: &str) -> bool {
    handler
        .requires
        .iter()
        .any(|requires| requires.rust_expr.trim() == expected)
}

fn pinocchio_literal_requires_eq(handler: &ParsedHandler, param_name: &str) -> Option<u64> {
    for requires in &handler.requires {
        let expr = requires.rust_expr.trim();
        let Some((left, right)) = expr.split_once("==") else {
            continue;
        };
        let left = left.trim();
        let right = right.trim();
        if left == param_name {
            if let Ok(value) = right.parse::<u64>() {
                return Some(value);
            }
        }
        if right == param_name {
            if let Ok(value) = left.parse::<u64>() {
                return Some(value);
            }
        }
    }
    None
}

fn emit_pinocchio_width_assumption_for_param(
    out: &mut String,
    handler: &ParsedHandler,
    param_name: &str,
    abi_rust_type: &str,
) {
    let Some(spec_rust_type) = pinocchio_spec_param_rust_type(handler, param_name) else {
        return;
    };
    let Some(max_value) = pinocchio_abi_max_value(abi_rust_type) else {
        return;
    };
    out.push_str(&format!(
        "    kani::assume(({param_name} as {spec_rust_type}) <= {max_value} as {spec_rust_type});\n"
    ));
}

fn pinocchio_spec_param_rust_type<'a>(
    handler: &'a ParsedHandler,
    param_name: &str,
) -> Option<&'a str> {
    handler
        .takes_params
        .iter()
        .find(|(name, _)| to_snake_case(name) == param_name)
        .and_then(|(_, ty)| numeric_param_rust_type(ty))
}

fn pinocchio_abi_max_value(rust_type: &str) -> Option<&'static str> {
    match rust_type {
        "bool" => Some("1"),
        "u8" => Some("u8::MAX"),
        "u16" => Some("u16::MAX"),
        "u32" => Some("u32::MAX"),
        "u64" => Some("u64::MAX"),
        _ => None,
    }
}

fn pinocchio_lower_param_expr(expr: &str, param_names: &BTreeSet<String>) -> String {
    let mut out = String::new();
    let mut ident = String::new();
    for ch in expr.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ident.push(ch);
            continue;
        }
        if !ident.is_empty() {
            out.push_str(&pinocchio_lower_ident(&ident, param_names));
            ident.clear();
        }
        out.push(ch);
    }
    if !ident.is_empty() {
        out.push_str(&pinocchio_lower_ident(&ident, param_names));
    }
    out
}

fn pinocchio_lower_ident(ident: &str, param_names: &BTreeSet<String>) -> String {
    let snake = to_snake_case(ident);
    if param_names.contains(&snake) {
        snake
    } else {
        ident.to_string()
    }
}

fn emit_pinocchio_repeat_pack(
    out: &mut String,
    handler: &ParsedHandler,
    repeat: &PinocchioRepeatField,
) {
    let Some(count) = pinocchio_repeat_count(handler, repeat) else {
        out.push_str(&format!(
            "    // TODO: infer repeat count for `{}` from spec params\n",
            repeat.name
        ));
        return;
    };
    out.push_str(&format!(
        "    let {}: u8 = {count}u8;\n",
        to_snake_case(&repeat.count_field)
    ));
    out.push_str(&format!(
        "    instruction_data.extend_from_slice(&{}.to_le_bytes());\n",
        to_snake_case(&repeat.count_field)
    ));

    for index in 0..count {
        for field in &repeat.item_fields {
            let Some(param_name) = repeat_item_param_name(handler, &field.name, index, count)
            else {
                out.push_str(&format!(
                    "    // TODO: pack repeat field `{}` item {} absent from the spec handler\n",
                    field.name, index
                ));
                continue;
            };
            pinocchio_emit_profile_field_pack(out, field, &to_snake_case(&param_name));
        }
    }
}

fn pinocchio_repeat_count(handler: &ParsedHandler, repeat: &PinocchioRepeatField) -> Option<usize> {
    if let Some((_, suffix)) = handler.name.rsplit_once('_') {
        if let Ok(count) = suffix.parse::<usize>() {
            if repeat_has_indexed_params(handler, repeat, count) {
                return Some(count);
            }
        }
    }
    if repeat_has_unindexed_params(handler, repeat) {
        return Some(1);
    }

    let mut indexes = BTreeSet::new();
    for (name, _) in &handler.takes_params {
        for field in &repeat.item_fields {
            if let Some(index) = indexed_param_suffix(name, &field.name) {
                indexes.insert(index);
            }
        }
    }
    let count = indexes.iter().next_back().copied()? + 1;
    if repeat_has_indexed_params(handler, repeat, count) {
        Some(count)
    } else {
        None
    }
}

fn repeat_has_unindexed_params(handler: &ParsedHandler, repeat: &PinocchioRepeatField) -> bool {
    repeat.item_fields.iter().all(|field| {
        handler
            .takes_params
            .iter()
            .any(|(name, _)| name == &field.name)
    })
}

fn repeat_has_indexed_params(
    handler: &ParsedHandler,
    repeat: &PinocchioRepeatField,
    count: usize,
) -> bool {
    (0..count).all(|index| {
        repeat.item_fields.iter().all(|field| {
            let indexed = format!("{}_{}", field.name, index);
            handler
                .takes_params
                .iter()
                .any(|(name, _)| name == &indexed)
        })
    })
}

fn repeat_item_param_name(
    handler: &ParsedHandler,
    field_name: &str,
    index: usize,
    count: usize,
) -> Option<String> {
    if count == 1
        && handler
            .takes_params
            .iter()
            .any(|(name, _)| name == field_name)
    {
        return Some(field_name.to_string());
    }
    let indexed = format!("{field_name}_{index}");
    handler
        .takes_params
        .iter()
        .any(|(name, _)| name == &indexed)
        .then_some(indexed)
}

fn indexed_param_suffix(name: &str, field_name: &str) -> Option<usize> {
    let suffix = name.strip_prefix(field_name)?.strip_prefix('_')?;
    suffix.parse::<usize>().ok()
}

#[derive(Debug, Clone)]
struct PinocchioTokenTransferAssertion {
    from: String,
    to: String,
    amount: String,
}

#[derive(Debug, Clone)]
struct PinocchioStateFieldAssertion {
    account: String,
    field: PinocchioLayoutField,
    kind: PinocchioStateAssertionKind,
}

#[derive(Debug, Clone)]
enum PinocchioStateAssertionKind {
    Unchanged,
    Set { value_expr: String },
}

fn call_arg<'a>(call: &'a crate::check::ParsedCall, name: &str) -> Option<&'a str> {
    call.args
        .iter()
        .find(|arg| arg.name == name)
        .map(|arg| arg.rust_expr.as_str())
}

fn pinocchio_token_transfer_assertions(
    handler: &ParsedHandler,
) -> Vec<PinocchioTokenTransferAssertion> {
    let mut assertions = Vec::new();

    for transfer in &handler.transfers {
        if let Some(amount) = &transfer.amount {
            assertions.push(PinocchioTokenTransferAssertion {
                from: to_snake_case(&transfer.from),
                to: to_snake_case(&transfer.to),
                amount: amount.clone(),
            });
        }
    }

    for call in &handler.calls {
        if call.target_interface == "Token" && call.target_handler == "transfer" {
            if let (Some(from), Some(to), Some(amount)) = (
                call_arg(call, "from"),
                call_arg(call, "to"),
                call_arg(call, "amount"),
            ) {
                assertions.push(PinocchioTokenTransferAssertion {
                    from: to_snake_case(from),
                    to: to_snake_case(to),
                    amount: amount.to_string(),
                });
            }
        }
    }

    assertions
}

fn pinocchio_token_amount_witnesses(
    handler: &ParsedHandler,
    assertions: &[PinocchioTokenTransferAssertion],
) -> BTreeMap<String, String> {
    let _ = handler;
    if !pinocchio_transfer_accounts_are_unique(assertions) {
        return BTreeMap::new();
    }

    let mut witnesses = BTreeMap::new();
    for assertion in assertions {
        witnesses.insert(assertion.from.clone(), assertion.amount.clone());
        witnesses
            .entry(assertion.to.clone())
            .or_insert_with(|| "0".to_string());
    }
    witnesses
}

fn pinocchio_transfer_accounts_are_unique(assertions: &[PinocchioTokenTransferAssertion]) -> bool {
    let mut seen = BTreeSet::new();
    for assertion in assertions {
        if !seen.insert(assertion.from.clone()) || !seen.insert(assertion.to.clone()) {
            return false;
        }
    }
    true
}

fn emit_pinocchio_token_pre_snapshots(
    out: &mut String,
    assertions: &[PinocchioTokenTransferAssertion],
) {
    if assertions.is_empty() {
        return;
    }
    if !pinocchio_transfer_accounts_are_unique(assertions) {
        out.push_str(
            "    // Profile note: token transfer delta assertions skipped because transfer accounts alias or chain; aggregate net-delta support required\n\n",
        );
        return;
    }

    out.push_str("    // Pre-token snapshots for generated Token.transfer assertions.\n");
    for (index, assertion) in assertions.iter().enumerate() {
        out.push_str(&format!(
            "    let pre_transfer_{index}_from = read_token_amount(&{});\n",
            assertion.from
        ));
        out.push_str(&format!(
            "    let pre_transfer_{index}_to = read_token_amount(&{});\n",
            assertion.to
        ));
        out.push_str(&format!(
            "    kani::assume(pre_transfer_{index}_from >= {});\n",
            pinocchio_token_amount_expr(&assertion.amount)
        ));
        out.push_str(&format!(
            "    kani::assume(pre_transfer_{index}_to <= u64::MAX - {});\n",
            pinocchio_token_amount_expr(&assertion.amount)
        ));
    }
    out.push('\n');
}

fn emit_pinocchio_token_post_assertions(
    out: &mut String,
    assertions: &[PinocchioTokenTransferAssertion],
) {
    if assertions.is_empty() {
        return;
    }
    if !pinocchio_transfer_accounts_are_unique(assertions) {
        return;
    }

    out.push('\n');
    out.push_str("    if _result.is_ok() {\n");
    for (index, assertion) in assertions.iter().enumerate() {
        out.push_str(&format!(
            "        assert_eq!(read_token_amount(&{}), pre_transfer_{index}_from - {});\n",
            assertion.from,
            pinocchio_token_amount_expr(&assertion.amount)
        ));
        out.push_str(&format!(
            "        assert_eq!(read_token_amount(&{}), pre_transfer_{index}_to + {});\n",
            assertion.to,
            pinocchio_token_amount_expr(&assertion.amount)
        ));
    }
    out.push_str("    }\n");
}

fn pinocchio_token_amount_expr(amount: &str) -> String {
    format!("({} as u64)", amount.trim())
}

fn emit_pinocchio_state_pre_snapshots(
    out: &mut String,
    assertions: &[PinocchioStateFieldAssertion],
) {
    if assertions.is_empty() {
        return;
    }
    out.push_str("    // Pre-state snapshots for ABI-backed account assertions.\n");
    let mut seen = BTreeSet::new();
    for assertion in assertions {
        let key = format!("{}:{}", assertion.account, assertion.field.name);
        if !seen.insert(key) {
            continue;
        }
        let Some(read) = pinocchio_state_read_expr(
            &assertion.account,
            &assertion.field.ty,
            assertion.field.offset,
        ) else {
            out.push_str(&format!(
                "    // TODO: snapshot ABI field `{}` with unsupported type `{}`\n",
                assertion.field.name, assertion.field.ty
            ));
            continue;
        };
        out.push_str(&format!(
            "    let pre_state_{}_{} = {};\n",
            assertion.account, assertion.field.name, read
        ));
    }
    out.push('\n');
}

fn emit_pinocchio_state_post_assertions(
    out: &mut String,
    assertions: &[PinocchioStateFieldAssertion],
) {
    if assertions.is_empty() {
        return;
    }
    out.push_str("\n    if _result.is_ok() {\n");
    let mut seen = BTreeSet::new();
    for assertion in assertions {
        let key = format!("{}:{}", assertion.account, assertion.field.name);
        if !seen.insert(key) {
            continue;
        }
        let Some(read) = pinocchio_state_read_expr(
            &assertion.account,
            &assertion.field.ty,
            assertion.field.offset,
        ) else {
            out.push_str(&format!(
                "        // TODO: assert ABI field `{}` with unsupported type `{}`\n",
                assertion.field.name, assertion.field.ty
            ));
            continue;
        };
        match &assertion.kind {
            PinocchioStateAssertionKind::Unchanged => {
                out.push_str(&format!(
                    "        assert_eq!({}, pre_state_{}_{});\n",
                    read, assertion.account, assertion.field.name
                ));
            }
            PinocchioStateAssertionKind::Set { value_expr } => {
                let expected = pinocchio_state_expected_expr(&assertion.field.ty, value_expr);
                out.push_str(&format!("        assert_eq!({}, {});\n", read, expected));
            }
        }
    }
    out.push_str("    }\n");
}

fn pinocchio_state_assertions(
    handler: &ParsedHandler,
    ordered_accounts: &[&ParsedHandlerAccount],
    proof_profile: Option<&PinocchioProofProfile>,
) -> Vec<PinocchioStateFieldAssertion> {
    let mut assertions = Vec::new();
    let Some((account, layout)) = pinocchio_primary_state_layout(ordered_accounts, proof_profile)
    else {
        return assertions;
    };
    let account = to_snake_case(&account.name);

    for (field, op, value) in &handler.effects {
        if op != "set" {
            continue;
        }
        let field_name = to_snake_case(field);
        if let Some(layout_field) = pinocchio_layout_field(layout, &field_name) {
            assertions.push(PinocchioStateFieldAssertion {
                account: account.clone(),
                field: layout_field.clone(),
                kind: PinocchioStateAssertionKind::Set {
                    value_expr: pinocchio_state_value_expr(value),
                },
            });
        }
    }

    for ensures in &handler.ensures {
        for field in pinocchio_unchanged_ensures_fields(&ensures.rust_expr_binary) {
            if let Some(layout_field) = pinocchio_layout_field(layout, &field) {
                assertions.push(PinocchioStateFieldAssertion {
                    account: account.clone(),
                    field: layout_field.clone(),
                    kind: PinocchioStateAssertionKind::Unchanged,
                });
            }
        }
    }

    if let Some(modifies) = &handler.modifies {
        for layout_field in &layout.fields {
            if layout_field.fixed_bytes.is_some() {
                continue;
            }
            if pinocchio_modifies_layout_field(modifies, layout_field) {
                continue;
            }
            assertions.push(PinocchioStateFieldAssertion {
                account: account.clone(),
                field: layout_field.clone(),
                kind: PinocchioStateAssertionKind::Unchanged,
            });
        }
    }

    assertions
}

fn pinocchio_primary_state_layout<'a>(
    ordered_accounts: &[&'a ParsedHandlerAccount],
    proof_profile: Option<&'a PinocchioProofProfile>,
) -> Option<(&'a ParsedHandlerAccount, &'a PinocchioRecordLayout)> {
    ordered_accounts.iter().find_map(|account| {
        pinocchio_account_layout(proof_profile, account).map(|layout| (*account, layout))
    })
}

fn pinocchio_layout_field<'a>(
    layout: &'a PinocchioRecordLayout,
    field_name: &str,
) -> Option<&'a PinocchioLayoutField> {
    let field_name = to_snake_case(field_name);
    layout.fields.iter().find(|field| {
        field.name == field_name
            || field.name.strip_suffix("_key") == Some(&field_name)
            || field_name.strip_suffix("_key") == Some(field.name.as_str())
    })
}

fn pinocchio_modifies_layout_field(modifies: &[String], field: &PinocchioLayoutField) -> bool {
    modifies.iter().any(|modified| {
        let modified = to_snake_case(modified);
        modified == field.name
            || modified.strip_suffix("_key") == Some(field.name.as_str())
            || field.name.strip_suffix("_key") == Some(modified.as_str())
    })
}

fn pinocchio_state_read_expr(account: &str, ty: &str, offset: usize) -> Option<String> {
    match ty.to_ascii_lowercase().as_str() {
        "pubkey" => Some(format!("read_state_pubkey(&{account}, {offset})")),
        "bool" => Some(format!("read_state_bool(&{account}, {offset})")),
        "u8" => Some(format!("read_state_u8(&{account}, {offset})")),
        "u16" => Some(format!("read_state_u16(&{account}, {offset})")),
        "u64" => Some(format!("read_state_u64(&{account}, {offset})")),
        "u128" => Some(format!("read_state_u128(&{account}, {offset})")),
        _ => None,
    }
}

fn pinocchio_state_write_call(
    account: &str,
    ty: &str,
    offset: usize,
    value_expr: &str,
) -> Option<String> {
    let fn_name = match ty.to_ascii_lowercase().as_str() {
        "pubkey" => "write_state_pubkey",
        "bool" => "write_state_bool",
        "u8" => "write_state_u8",
        "u16" => "write_state_u16",
        "u64" => "write_state_u64",
        "u128" => "write_state_u128",
        _ => return None,
    };
    Some(format!(
        "{fn_name}(&mut {account}, {offset}, {})",
        pinocchio_state_expected_expr(ty, value_expr)
    ))
}

fn pinocchio_state_expected_expr(ty: &str, value_expr: &str) -> String {
    let value_expr = pinocchio_state_value_expr(value_expr);
    match ty.to_ascii_lowercase().as_str() {
        "bool" => {
            if value_expr == "true" || value_expr == "false" {
                value_expr
            } else {
                format!("({value_expr} != 0)")
            }
        }
        "u8" => format!("({value_expr} as u8)"),
        "u16" => format!("({value_expr} as u16)"),
        "u64" => format!("({value_expr} as u64)"),
        "u128" => format!("({value_expr} as u128)"),
        _ => value_expr,
    }
}

fn pinocchio_state_value_expr(value: &str) -> String {
    let value = value.trim();
    if let Some(account) = value.strip_suffix(".pubkey") {
        return format!("{}.hdr.key", to_snake_case(account));
    }
    match value {
        "true" | "false" => value.to_string(),
        _ if value.chars().all(|ch| ch.is_ascii_digit()) => value.to_string(),
        _ => to_snake_case(value),
    }
}

fn pinocchio_kani_unwind_bound() -> usize {
    std::env::var("QEDGEN_PINOCCHIO_KANI_UNWIND")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(34)
}

fn pinocchio_kani_concrete_scalar_probe_mode() -> Option<String> {
    std::env::var("QEDGEN_PINOCCHIO_KANI_CONCRETE_SCALARS")
        .ok()
        .filter(|value| !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false"))
        .map(|value| {
            if value == "1" || value.eq_ignore_ascii_case("true") {
                "all".to_string()
            } else {
                value.to_ascii_lowercase()
            }
        })
}

fn pinocchio_concrete_scalar_probe_value(name: &str, rust_ty: &str, mode: &str) -> Option<String> {
    let base = strip_numeric_suffix(name).map_or(name, |(base, _)| base);
    let concrete_amounts = mode == "all" || mode.contains("amount");
    let concrete_fee = mode == "all" || mode.contains("fee");
    let concrete_lane = mode == "all" || mode.contains("lane");
    let value = match base {
        "amount" | "amount_in" | "amount_out" if concrete_amounts => "1000",
        "min_out" if concrete_amounts => "900",
        "max_fee_bps" if concrete_fee => "0",
        "lane_id" | "from_lane_id" if concrete_lane => "0",
        "to_lane_id" if concrete_lane => "1",
        _ => return None,
    };
    Some(format!("{value} as {rust_ty}"))
}

/// True when `pattern` occurs in `compact` as a complete comparison — i.e.
/// followed by end-of-expression or a boolean connective. A bare substring
/// test misclassifies conservation ensures: `post.fee_pool==pre.fee_pool+fee`
/// contains `post.fee_pool==pre.fee_pool`, but the field is *not* unchanged,
/// and the resulting equality assertion fails on a correct implementation.
fn contains_anchored_comparison(compact: &str, pattern: &str) -> bool {
    let mut search = compact;
    while let Some(pos) = search.find(pattern) {
        match search[pos + pattern.len()..].chars().next() {
            None | Some('&') | Some('|') | Some(')') => return true,
            _ => search = &search[pos + pattern.len()..],
        }
    }
    false
}

fn pinocchio_unchanged_ensures_fields(expr: &str) -> Vec<String> {
    let compact: String = expr.chars().filter(|ch| !ch.is_whitespace()).collect();
    let mut fields = BTreeSet::new();
    let mut rest = compact.as_str();
    while let Some(start) = rest.find("post.") {
        rest = &rest[start + "post.".len()..];
        let field: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if field.is_empty() {
            continue;
        }
        let pattern_a = format!("post.{field}==pre.{field}");
        let pattern_b = format!("pre.{field}==post.{field}");
        if contains_anchored_comparison(&compact, &pattern_a)
            || contains_anchored_comparison(&compact, &pattern_b)
        {
            fields.insert(to_snake_case(&field));
        }
    }
    fields.into_iter().collect()
}

fn emit_pinocchio_state_witness_initializers(
    out: &mut String,
    account: &str,
    layout: &PinocchioRecordLayout,
    handler: &ParsedHandler,
    account_key_exprs: &BTreeMap<String, String>,
) {
    for field in &layout.fields {
        if field.fixed_bytes.is_some() {
            continue;
        }
        let Some(value) =
            pinocchio_state_initial_value_expr(&field.name, &field.ty, handler, account_key_exprs)
        else {
            continue;
        };
        let Some(call) = pinocchio_state_write_call(account, &field.ty, field.offset, &value)
        else {
            continue;
        };
        out.push_str(&format!("    {call};\n"));
    }

    for repeat in &layout.repeats {
        emit_pinocchio_state_repeat_initializers(out, account, layout, repeat, account_key_exprs);
    }
}

fn pinocchio_state_initial_value_expr(
    field_name: &str,
    ty: &str,
    handler: &ParsedHandler,
    account_key_exprs: &BTreeMap<String, String>,
) -> Option<String> {
    let field_name = to_snake_case(field_name);
    if ty.eq_ignore_ascii_case("pubkey") {
        if let Some(expr) = pinocchio_state_pubkey_initial_expr(&field_name, account_key_exprs) {
            return Some(expr);
        }
    }
    if ty.eq_ignore_ascii_case("bool") && field_name == "paused" {
        return Some("false".to_string());
    }
    if let Some(param) = pinocchio_state_param_for_field(handler, &field_name) {
        return Some(param);
    }
    match field_name.as_str() {
        "max_fee_bps" => Some("10000".to_string()),
        "lane_count" => Some("2".to_string()),
        "mint_count" => Some("2".to_string()),
        _ => None,
    }
}

fn pinocchio_state_param_for_field(handler: &ParsedHandler, field_name: &str) -> Option<String> {
    handler
        .takes_params
        .iter()
        .map(|(name, _)| to_snake_case(name))
        .find(|name| {
            name == field_name
                || name == &format!("new_{field_name}")
                || name == &format!("{field_name}_value")
                || name.strip_prefix("new_") == Some(field_name)
                || name.strip_suffix("_value") == Some(field_name)
        })
}

fn pinocchio_state_pubkey_initial_expr(
    field_name: &str,
    account_key_exprs: &BTreeMap<String, String>,
) -> Option<String> {
    let candidates = [
        field_name.to_string(),
        field_name
            .strip_suffix("_key")
            .unwrap_or(field_name)
            .to_string(),
    ];
    for candidate in candidates {
        if let Some(expr) = account_key_exprs.get(&candidate) {
            return Some(expr.clone());
        }
    }
    None
}

fn emit_pinocchio_state_repeat_initializers(
    out: &mut String,
    account: &str,
    layout: &PinocchioRecordLayout,
    repeat: &crate::pinocchio_profile::PinocchioLayoutRepeat,
    account_key_exprs: &BTreeMap<String, String>,
) {
    let Some(count_field) = pinocchio_layout_field(layout, &repeat.count_field) else {
        return;
    };
    let mut repeat_keys: Vec<_> = account_key_exprs
        .iter()
        .filter(|(name, _)| name.contains("mint"))
        .map(|(_name, expr)| expr.clone())
        .take(2)
        .collect();
    if repeat_keys.is_empty() && repeat.item_len == 32 {
        repeat_keys = (0..2)
            .map(|index| format!("[{}u8; 32]", 11 + index))
            .collect();
    }
    if repeat_keys.is_empty() {
        return;
    }
    if let Some(call) = pinocchio_state_write_call(
        account,
        &count_field.ty,
        count_field.offset,
        &repeat_keys.len().to_string(),
    ) {
        out.push_str(&format!("    {call};\n"));
    }
    for (index, key_expr) in repeat_keys.iter().enumerate() {
        out.push_str(&format!(
            "    write_state_pubkey(&mut {account}, {}, {key_expr});\n",
            repeat.offset + (index * repeat.item_len)
        ));
    }
}

fn pinocchio_account_order<'a>(
    handler: &'a ParsedHandler,
    profile: Option<&PinocchioHandlerProfile>,
) -> Vec<&'a ParsedHandlerAccount> {
    let Some(profile) = profile else {
        return handler.accounts.iter().collect();
    };
    if profile.accounts.is_empty() {
        return handler.accounts.iter().collect();
    }

    let mut ordered = Vec::with_capacity(handler.accounts.len());
    for name in &profile.accounts {
        let normalized_profile_name = normalize_pinocchio_profile_name(name);
        let Some(account) = handler
            .accounts
            .iter()
            .find(|acct| normalize_pinocchio_profile_name(&acct.name) == normalized_profile_name)
        else {
            return handler.accounts.iter().collect();
        };
        ordered.push(account);
    }
    for account in &handler.accounts {
        let normalized_account_name = normalize_pinocchio_profile_name(&account.name);
        if !profile
            .accounts
            .iter()
            .any(|name| normalize_pinocchio_profile_name(name) == normalized_account_name)
        {
            ordered.push(account);
        }
    }
    ordered
}

fn pinocchio_unmatched_profile_account<'a>(
    handler: &ParsedHandler,
    profile: &'a PinocchioHandlerProfile,
) -> Option<&'a str> {
    profile.accounts.iter().find_map(|name| {
        let normalized_profile_name = normalize_pinocchio_profile_name(name);
        let matched = handler
            .accounts
            .iter()
            .any(|acct| normalize_pinocchio_profile_name(&acct.name) == normalized_profile_name);
        (!matched).then_some(name.as_str())
    })
}

fn normalize_pinocchio_profile_name(name: &str) -> String {
    to_snake_case(name)
}

fn pinocchio_account_role<'a>(
    profile: Option<&'a PinocchioHandlerProfile>,
    account: &ParsedHandlerAccount,
) -> Option<&'a PinocchioAccountRole> {
    profile.and_then(|profile| {
        let ident = to_snake_case(&account.name);
        profile
            .account_roles
            .get(&ident)
            .or_else(|| profile.account_roles.get(&account.name))
            .or_else(|| {
                strip_numeric_suffix(&ident)
                    .and_then(|(base, _suffix)| profile.account_roles.get(base))
            })
    })
}

fn strip_numeric_suffix(ident: &str) -> Option<(&str, &str)> {
    let (base, suffix) = ident.rsplit_once('_')?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((base, suffix))
}

fn pinocchio_token_account_binding<'a>(
    profile: Option<&'a PinocchioHandlerProfile>,
    account: &ParsedHandlerAccount,
) -> Option<&'a PinocchioTokenAccountBinding> {
    profile.and_then(|profile| {
        let ident = to_snake_case(&account.name);
        profile
            .token_account_bindings
            .get(&ident)
            .or_else(|| profile.token_account_bindings.get(&account.name))
            .or_else(|| {
                strip_numeric_suffix(&ident)
                    .and_then(|(base, _suffix)| profile.token_account_bindings.get(base))
            })
    })
}

fn pinocchio_account_key_derivation<'a>(
    profile: Option<&'a PinocchioHandlerProfile>,
    account: &ParsedHandlerAccount,
) -> Option<&'a PinocchioLocalKeyDerivation> {
    profile.and_then(|profile| {
        let ident = to_snake_case(&account.name);
        profile
            .account_key_derivations
            .get(&ident)
            .or_else(|| profile.account_key_derivations.get(&account.name))
            .or_else(|| {
                strip_numeric_suffix(&ident)
                    .and_then(|(base, _suffix)| profile.account_key_derivations.get(base))
            })
    })
}

fn pinocchio_bound_key_expr(
    account_key_exprs: &BTreeMap<String, String>,
    current_ident: &str,
    bound_account: &str,
) -> Option<String> {
    account_key_exprs.get(bound_account).cloned().or_else(|| {
        strip_numeric_suffix(current_ident).and_then(|(_base, suffix)| {
            account_key_exprs
                .get(&format!("{bound_account}_{suffix}"))
                .cloned()
        })
    })
}

fn pinocchio_mint_decimal_param(
    profile: Option<&PinocchioHandlerProfile>,
    ident: &str,
) -> Option<String> {
    let profile = profile?;
    profile
        .mint_decimal_bindings
        .get(ident)
        .cloned()
        .or_else(|| {
            ident
                .strip_suffix("_mint")
                .map(|prefix| format!("{prefix}_decimals"))
        })
}

fn pinocchio_param_decl_rust_type<'a>(
    name: &str,
    spec_type: &'a str,
    handler: &'a ParsedHandler,
    handler_profile: Option<&'a PinocchioHandlerProfile>,
) -> Option<&'a str> {
    let spec_rust_type = pinocchio_param_decl_type(spec_type);
    if pinocchio_has_fee_normalization_shape(handler) {
        if name == "max_fee_bps" {
            if let Some(profile_type) = pinocchio_param_rust_type(name, handler, handler_profile) {
                return Some(profile_type);
            }
        }
        if pinocchio_mint_decimal_param_is_used(handler_profile, name) {
            return Some("u8");
        }
    }
    let Some(profile_type) = pinocchio_param_rust_type(name, handler, handler_profile) else {
        return spec_rust_type;
    };

    if pinocchio_profile_rust_type_is_unsupported(profile_type) {
        return spec_rust_type;
    }

    if profile_type == "bool" || pinocchio_param_has_mixed_arithmetic_requires(handler, name) {
        return spec_rust_type;
    }

    Some(profile_type)
}

fn pinocchio_narrow_symbolic_param_type(
    handler: &ParsedHandler,
    param_name: &str,
    rust_ty: &str,
) -> String {
    let Some(upper_bound) = pinocchio_param_upper_bound(handler, param_name) else {
        return rust_ty.to_string();
    };
    let narrowed = match rust_ty {
        "u128" | "u64" | "u32" | "u16" | "u8" if upper_bound <= u8::MAX as u128 => "u8",
        "u128" | "u64" | "u32" | "u16" if upper_bound <= u16::MAX as u128 => "u16",
        "u128" | "u64" | "u32" if upper_bound <= u32::MAX as u128 => "u32",
        "u128" | "u64" if upper_bound <= u64::MAX as u128 => "u64",
        _ => rust_ty,
    };
    narrowed.to_string()
}

fn pinocchio_param_upper_bound(handler: &ParsedHandler, param_name: &str) -> Option<u128> {
    let mut direct = pinocchio_param_direct_upper_bound(handler, param_name);
    for requires in &handler.requires {
        let expr = requires.rust_expr.trim();
        if let Some(other) = expr.strip_suffix(&format!(">= {param_name}")) {
            let other = to_snake_case(other.trim());
            if other != param_name {
                direct = min_option(direct, pinocchio_param_direct_upper_bound(handler, &other));
            }
        }
        if let Some(other) = expr.strip_prefix(&format!("{param_name} <= ")) {
            let other = to_snake_case(other.trim());
            if other != param_name {
                direct = min_option(direct, pinocchio_param_direct_upper_bound(handler, &other));
            }
        }
    }
    direct
}

fn pinocchio_param_direct_upper_bound(handler: &ParsedHandler, param_name: &str) -> Option<u128> {
    handler
        .requires
        .iter()
        .filter_map(|requires| {
            let expr = requires.rust_expr.trim();
            expr.strip_prefix(&format!("{param_name} <= "))
                .and_then(|literal| literal.trim().parse::<u128>().ok())
        })
        .min()
}

fn min_option(left: Option<u128>, right: Option<u128>) -> Option<u128> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn pinocchio_mint_decimal_param_is_used(
    handler_profile: Option<&PinocchioHandlerProfile>,
    name: &str,
) -> bool {
    handler_profile
        .map(|profile| {
            profile
                .mint_decimal_bindings
                .values()
                .any(|param| param == name)
        })
        .unwrap_or(false)
}

fn pinocchio_param_has_mixed_arithmetic_requires(handler: &ParsedHandler, name: &str) -> bool {
    handler.requires.iter().any(|requires| {
        let expr = requires.rust_expr.as_str();
        expr.contains(&format!("{name} +"))
            || expr.contains(&format!("+ {name}"))
            || expr.contains(&format!("{name} -"))
            || expr.contains(&format!("- {name}"))
            || expr.contains(&format!("{name} *"))
            || expr.contains(&format!("* {name}"))
            || expr.contains(&format!("{name} /"))
            || expr.contains(&format!("/ {name}"))
    })
}

fn pinocchio_param_rust_type<'a>(
    name: &str,
    handler: &'a ParsedHandler,
    handler_profile: Option<&'a PinocchioHandlerProfile>,
) -> Option<&'a str> {
    handler_profile
        .and_then(|profile| {
            profile
                .params
                .iter()
                .chain(
                    profile
                        .repeats
                        .iter()
                        .flat_map(|repeat| repeat.item_fields.iter()),
                )
                .find(|param| {
                    param.name == name
                        || pinocchio_profile_param_name(handler, &param.name).as_deref()
                            == Some(name)
                })
                .and_then(|param| {
                    (!pinocchio_profile_field_is_unsupported(param))
                        .then_some(param.rust_type.as_str())
                })
        })
        .or_else(|| {
            handler
                .takes_params
                .iter()
                .find(|(param_name, _)| to_snake_case(param_name) == name)
                .and_then(|(_name, param_type)| numeric_param_rust_type(param_type))
        })
}

fn pinocchio_profile_rust_type_is_unsupported(rust_type: &str) -> bool {
    rust_type.starts_with("unsupported:")
}

fn pinocchio_resolve_source_expr_alias<'a>(
    expr: &'a str,
    handler_profile: Option<&'a PinocchioHandlerProfile>,
) -> String {
    let mut current = expr.trim().to_string();
    for _ in 0..4 {
        let key = current.trim().trim_start_matches('&').trim().to_string();
        let Some(next) = handler_profile.and_then(|profile| profile.source_expr_aliases.get(&key))
        else {
            break;
        };
        if next == &current {
            break;
        }
        current = next.clone();
    }
    current
}

fn pinocchio_source_expr_param_name(
    expr: &str,
    current_ident: &str,
    handler_profile: Option<&PinocchioHandlerProfile>,
) -> Option<String> {
    let resolved = pinocchio_resolve_source_expr_alias(expr, handler_profile);
    let expr = resolved.trim();
    let expr = expr.strip_prefix('&').unwrap_or(expr).trim();
    if let Some(account) = expr.strip_suffix(".key()") {
        return Some(to_snake_case(account));
    }
    if let Some(field) = expr
        .strip_prefix("transfer.")
        .and_then(|expr| expr.strip_suffix(".0"))
    {
        if let Some((_base, suffix)) = strip_numeric_suffix(current_ident) {
            return Some(format!("{}_{suffix}", to_snake_case(field)));
        }
        return Some(to_snake_case(field));
    }
    if expr
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Some(to_snake_case(expr.trim_end_matches(".0")));
    }
    None
}

fn pinocchio_source_expr_account_key(
    expr: &str,
    account_key_exprs: &BTreeMap<String, String>,
    handler_profile: Option<&PinocchioHandlerProfile>,
) -> Option<String> {
    let resolved = pinocchio_resolve_source_expr_alias(expr, handler_profile);
    let expr = resolved
        .trim()
        .strip_prefix('&')
        .unwrap_or(resolved.trim())
        .trim();
    let account = expr
        .strip_suffix(".key()")
        .or_else(|| expr.strip_suffix(".0"))
        .unwrap_or(expr);
    if !account
        .chars()
        .all(|ch| ch == '_' || ch == '.' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    account_key_exprs.get(&to_snake_case(account)).cloned()
}

fn pinocchio_pda_program_expr(program_id: &str) -> Option<String> {
    let program_id = program_id.trim();
    if program_id == "program_id" {
        return Some("&program_id".to_string());
    }
    if let Some(rest) = program_id.strip_prefix("crate::") {
        return Some(format!("&crate::{rest}"));
    }
    if is_upper_const_ident(program_id) {
        return Some(format!("&crate::{program_id}"));
    }
    None
}

fn pinocchio_const_pubkey_seed_expr(expr: &str) -> Option<String> {
    let seed = expr.trim().strip_suffix(".as_ref()")?.trim();
    if seed.starts_with("crate::") {
        return Some(format!("{seed}.as_ref()"));
    }
    if is_upper_const_ident(seed) {
        return Some(format!("crate::{seed}.as_ref()"));
    }
    None
}

fn is_upper_const_ident(input: &str) -> bool {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_uppercase())
        && chars.all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn pinocchio_local_derived_key_seed_exprs(
    proof_profile: Option<&PinocchioProofProfile>,
    handler_profile: Option<&PinocchioHandlerProfile>,
    handler: &ParsedHandler,
    current_ident: &str,
    local: &PinocchioLocalKeyDerivation,
    account_key_exprs: &BTreeMap<String, String>,
    nested_key_exprs: &BTreeMap<String, String>,
) -> Option<Vec<String>> {
    let derivation = proof_profile?.pda_derivations.get(&local.derivation)?;
    let _program_expr = pinocchio_pda_program_expr(&derivation.program_id)?;
    let arg_by_param: BTreeMap<_, _> = derivation
        .params
        .iter()
        .zip(local.args.iter())
        .map(|(param, arg)| (param.as_str(), arg.as_str()))
        .collect();

    derivation
        .seeds
        .iter()
        .map(|seed| {
            if let Some(literal) = &seed.literal {
                return Some(format!("b\"{}\".as_ref()", rust_string_escape(literal)));
            }
            if let Some(const_seed) = pinocchio_const_pubkey_seed_expr(&seed.expr) {
                return Some(const_seed);
            }
            if let Some(seed_param) = seed.expr.trim().strip_suffix(".as_ref()") {
                if let Some(key_expr) = nested_key_exprs.get(seed_param.trim()) {
                    return Some(format!("{key_expr}.as_ref()"));
                }
                let source_expr = arg_by_param.get(seed_param.trim())?;
                let param_name =
                    pinocchio_source_expr_param_name(source_expr, current_ident, handler_profile)?;
                let rust_type = derivation
                    .param_types
                    .get(seed_param.trim())
                    .map(String::as_str)
                    .or_else(|| pinocchio_param_rust_type(&param_name, handler, handler_profile))?;
                return match rust_type {
                    "&Pubkey" | "Pubkey" | "pubkey" => pinocchio_source_expr_account_key(
                        source_expr,
                        account_key_exprs,
                        handler_profile,
                    )
                    .or(Some(param_name))
                    .map(|expr| format!("{expr}.as_ref()")),
                    _ => None,
                };
            }
            let bracketed = seed
                .expr
                .trim()
                .strip_prefix('[')?
                .strip_suffix(']')?
                .trim();
            let source_expr = arg_by_param.get(bracketed)?;
            let param_name =
                pinocchio_source_expr_param_name(source_expr, current_ident, handler_profile)?;
            let rust_type = derivation
                .param_types
                .get(bracketed)
                .map(String::as_str)
                .or_else(|| pinocchio_param_rust_type(&param_name, handler, handler_profile))?;
            match rust_type {
                "u8" | "i8" => Some(format!("&[{} as u8]", param_name)),
                "&Pubkey" | "Pubkey" | "pubkey" => pinocchio_source_expr_account_key(
                    source_expr,
                    account_key_exprs,
                    handler_profile,
                )
                .or(Some(param_name))
                .map(|expr| format!("{expr}.as_ref()")),
                _ => Some(format!("&({} as {}).to_le_bytes()", param_name, rust_type)),
            }
        })
        .collect::<Option<Vec<_>>>()
}

fn pinocchio_local_arg_by_param<'a>(
    derivation: &'a PinocchioPdaDerivation,
    local: &'a PinocchioLocalKeyDerivation,
) -> BTreeMap<&'a str, &'a str> {
    derivation
        .params
        .iter()
        .zip(local.args.iter())
        .map(|(param, arg)| (param.as_str(), arg.as_str()))
        .collect()
}

struct PinocchioNestedKeyBindingCtx<'a> {
    proof_profile: Option<&'a PinocchioProofProfile>,
    handler_profile: Option<&'a PinocchioHandlerProfile>,
    handler: &'a ParsedHandler,
    current_ident: &'a str,
    outer_derivation: &'a PinocchioPdaDerivation,
    outer_arg_by_param: &'a BTreeMap<&'a str, &'a str>,
    account_key_exprs: &'a BTreeMap<String, String>,
}

struct PinocchioPdaKeyBindingsCtx<'a> {
    handler: &'a ParsedHandler,
    proof_profile: Option<&'a PinocchioProofProfile>,
    handler_profile: Option<&'a PinocchioHandlerProfile>,
    ordered_accounts: &'a [&'a ParsedHandlerAccount],
    account_key_exprs: &'a BTreeMap<String, String>,
}

fn emit_pinocchio_nested_key_bindings(
    out: &mut String,
    ctx: PinocchioNestedKeyBindingCtx<'_>,
) -> BTreeMap<String, String> {
    let mut nested_keys = BTreeMap::new();
    let Some(proof_profile) = ctx.proof_profile else {
        return nested_keys;
    };
    for (local_name, local) in &ctx.outer_derivation.local_key_derivations {
        let Some(nested_derivation) = proof_profile.pda_derivations.get(&local.derivation) else {
            continue;
        };
        let Some(program_expr) = pinocchio_pda_program_expr(&nested_derivation.program_id) else {
            continue;
        };
        let nested_args = local
            .args
            .iter()
            .map(|arg| {
                ctx.outer_arg_by_param
                    .get(arg.as_str())
                    .copied()
                    .unwrap_or(arg.as_str())
                    .to_string()
            })
            .collect::<Vec<_>>();
        let nested_local = PinocchioLocalKeyDerivation {
            derivation: local.derivation.clone(),
            args: nested_args,
        };
        let Some(seed_exprs) = pinocchio_local_derived_key_seed_exprs(
            Some(proof_profile),
            ctx.handler_profile,
            ctx.handler,
            ctx.current_ident,
            &nested_local,
            ctx.account_key_exprs,
            &BTreeMap::new(),
        ) else {
            continue;
        };
        let ident = format!("{}_{}", ctx.current_ident, local_name);
        if let Some(helper_call) = pinocchio_pda_helper_call(
            nested_derivation,
            ctx.handler,
            ctx.handler_profile,
            ctx.current_ident,
            Some(&pinocchio_local_arg_by_param(
                nested_derivation,
                &nested_local,
            )),
        ) {
            out.push_str(&format!("    let {ident}_key = {helper_call};\n"));
        } else {
            out.push_str(&format!(
                "    let {ident}_pda = pinocchio::pubkey::try_find_program_address(&[{}], {});\n",
                seed_exprs.join(", "),
                program_expr
            ));
            out.push_str(&format!("    kani::assume({ident}_pda.is_some());\n"));
            out.push_str(&format!("    let {ident}_key = {ident}_pda.unwrap().0;\n"));
        }
        nested_keys.insert(local_name.clone(), format!("{ident}_key"));
    }
    nested_keys
}

fn pinocchio_account_is_program(
    account: &ParsedHandlerAccount,
    role: Option<&PinocchioAccountRole>,
) -> bool {
    role.and_then(|role| role.is_program)
        .unwrap_or(account.is_program)
}

fn pinocchio_account_is_signer(
    account: &ParsedHandlerAccount,
    role: Option<&PinocchioAccountRole>,
) -> bool {
    role.and_then(|role| role.is_signer)
        .unwrap_or(account.is_signer)
}

fn pinocchio_account_is_writable(
    account: &ParsedHandlerAccount,
    role: Option<&PinocchioAccountRole>,
) -> bool {
    role.and_then(|role| role.is_writable)
        .unwrap_or(account.is_writable)
}

fn pinocchio_account_type<'a>(
    account: &'a ParsedHandlerAccount,
    role: Option<&'a PinocchioAccountRole>,
) -> Option<&'a str> {
    role.and_then(|role| role.account_type.as_deref())
        .or(account.account_type.as_deref())
}

fn pinocchio_account_layout<'a>(
    profile: Option<&'a PinocchioProofProfile>,
    account: &ParsedHandlerAccount,
) -> Option<&'a PinocchioRecordLayout> {
    let profile = profile?;
    let account_name = to_snake_case(&account.name);
    let base_account_name = account_name.strip_suffix("_account");
    let record_name = profile
        .account_layouts
        .get(&account_name)
        .or_else(|| profile.account_layouts.get(&account.name))
        .or_else(|| base_account_name.and_then(|name| profile.account_layouts.get(name)))?;
    profile.record_layouts.get(record_name)
}

fn pinocchio_account_pda<'a>(
    profile: Option<&'a PinocchioProofProfile>,
    account: &ParsedHandlerAccount,
) -> Option<&'a PinocchioPdaDerivation> {
    let profile = profile?;
    let account_name = to_snake_case(&account.name);
    profile
        .pda_derivations
        .get(&account_name)
        .or_else(|| profile.pda_derivations.get(&account.name))
}

fn pinocchio_pda_seed_expr(
    seed: &PinocchioPdaSeed,
    handler: &ParsedHandler,
    handler_profile: Option<&PinocchioHandlerProfile>,
    account_key_exprs: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(literal) = &seed.literal {
        return Some(format!("b\"{}\".as_ref()", rust_string_escape(literal)));
    }

    let expr = seed.expr.trim();
    if let Some(const_seed) = pinocchio_const_pubkey_seed_expr(expr) {
        return Some(const_seed);
    }
    if let Some(param_name) = expr.strip_suffix(".as_ref()") {
        let param_name = param_name.trim();
        let (_param_name, param_type) = handler
            .takes_params
            .iter()
            .find(|(name, _)| name == param_name)?;
        return match param_type.as_str() {
            "Pubkey" => account_key_exprs
                .get(&to_snake_case(param_name))
                .cloned()
                .or_else(|| Some(to_snake_case(param_name)))
                .map(|expr| format!("{expr}.as_ref()")),
            _ => None,
        };
    }
    let bracketed = expr.strip_prefix('[')?.strip_suffix(']')?.trim();
    let (param_name, param_type) = handler
        .takes_params
        .iter()
        .find(|(name, _)| name == bracketed)?;
    let rust_type = handler_profile
        .and_then(|profile| {
            profile
                .params
                .iter()
                .find(|param| param.name == *param_name)
                .map(|param| param.rust_type.as_str())
        })
        .or_else(|| numeric_param_rust_type(param_type))?;
    match rust_type {
        "u8" | "i8" => Some(format!("&[{} as u8]", to_snake_case(param_name))),
        "Pubkey" | "pubkey" => account_key_exprs
            .get(&to_snake_case(param_name))
            .cloned()
            .or_else(|| Some(to_snake_case(param_name)))
            .map(|expr| format!("{expr}.as_ref()")),
        _ => Some(format!(
            "&({} as {}).to_le_bytes()",
            to_snake_case(param_name),
            rust_type
        )),
    }
}

fn emit_pinocchio_pda_key_bindings(
    out: &mut String,
    ctx: PinocchioPdaKeyBindingsCtx<'_>,
) -> BTreeSet<String> {
    let mut emitted = BTreeSet::new();
    let mut available_account_key_exprs = ctx.account_key_exprs.clone();
    for account in ctx.ordered_accounts {
        let ident = to_snake_case(&account.name);
        let local_key_derivation = pinocchio_account_key_derivation(ctx.handler_profile, account)
            .or_else(|| pinocchio_owner_account_key_derivation(ctx.handler_profile, &ident));
        let mut local_nested_key_exprs = BTreeMap::new();
        let seed_exprs = pinocchio_account_pda_seed_exprs(
            ctx.proof_profile,
            ctx.handler_profile,
            account,
            ctx.handler,
            &available_account_key_exprs,
        )
        .or_else(|| {
            local_key_derivation.and_then(|local| {
                let derivation = ctx.proof_profile?.pda_derivations.get(&local.derivation)?;
                let arg_by_param = pinocchio_local_arg_by_param(derivation, local);
                local_nested_key_exprs = emit_pinocchio_nested_key_bindings(
                    out,
                    PinocchioNestedKeyBindingCtx {
                        proof_profile: ctx.proof_profile,
                        handler_profile: ctx.handler_profile,
                        handler: ctx.handler,
                        current_ident: &ident,
                        outer_derivation: derivation,
                        outer_arg_by_param: &arg_by_param,
                        account_key_exprs: &available_account_key_exprs,
                    },
                );
                pinocchio_local_derived_key_seed_exprs(
                    ctx.proof_profile,
                    ctx.handler_profile,
                    ctx.handler,
                    &ident,
                    local,
                    &available_account_key_exprs,
                    &local_nested_key_exprs,
                )
            })
        });
        let Some(seed_exprs) = seed_exprs else {
            continue;
        };
        if let Some(owner_ident) = pinocchio_token_binding_owner_ident(ctx.handler_profile, &ident)
        {
            if !emitted.contains(&owner_ident) {
                if let Some(owner_key_expr) =
                    pinocchio_nested_owner_key_expr(&local_nested_key_exprs)
                {
                    out.push_str(&format!("    let {owner_ident}_key = {owner_key_expr};\n"));
                    emitted.insert(owner_ident.clone());
                    let key_var = format!("{owner_ident}_key");
                    available_account_key_exprs.insert(key_var.clone(), key_var.clone());
                    available_account_key_exprs.insert(owner_ident, key_var);
                }
            }
        }
        let derivation = pinocchio_account_pda(ctx.proof_profile, account).or_else(|| {
            let local = pinocchio_account_key_derivation(ctx.handler_profile, account)?;
            ctx.proof_profile?.pda_derivations.get(&local.derivation)
        });
        let Some(program_expr) = derivation.and_then(|d| pinocchio_pda_program_expr(&d.program_id))
        else {
            continue;
        };
        let helper_call = derivation.and_then(|derivation| {
            local_key_derivation
                .and_then(|local| {
                    pinocchio_pda_helper_call_for_local(
                        derivation,
                        local,
                        ctx.handler,
                        ctx.handler_profile,
                        &ident,
                        &available_account_key_exprs,
                        &local_nested_key_exprs,
                    )
                })
                .or_else(|| {
                    pinocchio_pda_helper_call(
                        derivation,
                        ctx.handler,
                        ctx.handler_profile,
                        &ident,
                        None,
                    )
                })
        });
        if let Some(helper_call) = helper_call {
            out.push_str(&format!("    let {ident}_key = {helper_call};\n"));
        } else {
            out.push_str(&format!(
                "    let {ident}_pda = pinocchio::pubkey::try_find_program_address(&[{}], {});\n",
                seed_exprs.join(", "),
                program_expr
            ));
            out.push_str(&format!("    kani::assume({ident}_pda.is_some());\n"));
            out.push_str(&format!("    let {ident}_key = {ident}_pda.unwrap().0;\n"));
        }
        emitted.insert(ident.clone());
        let key_var = format!("{ident}_key");
        available_account_key_exprs.insert(key_var.clone(), key_var.clone());
        available_account_key_exprs.insert(ident, key_var);
    }
    emitted
}

fn pinocchio_token_binding_owner_ident(
    handler_profile: Option<&PinocchioHandlerProfile>,
    token_ident: &str,
) -> Option<String> {
    let handler_profile = handler_profile?;
    let binding = handler_profile
        .token_account_bindings
        .get(token_ident)
        .or_else(|| {
            strip_numeric_suffix(token_ident)
                .and_then(|(base, _suffix)| handler_profile.token_account_bindings.get(base))
        })?;
    let owner = binding.owner_account.as_ref()?;
    let owner = if let Some((_base, suffix)) = strip_numeric_suffix(token_ident) {
        format!("{owner}_{suffix}")
    } else {
        owner.clone()
    };
    Some(owner)
}

fn pinocchio_nested_owner_key_expr(nested_key_exprs: &BTreeMap<String, String>) -> Option<String> {
    nested_key_exprs
        .iter()
        .find(|(name, _)| name.contains("authority") || name.contains("owner"))
        .map(|(_name, expr)| expr.clone())
        .or_else(|| nested_key_exprs.values().next().cloned())
}

fn pinocchio_nested_owner_key_var_for_account(
    proof_profile: Option<&PinocchioProofProfile>,
    handler_profile: Option<&PinocchioHandlerProfile>,
    account: &ParsedHandlerAccount,
    ident: &str,
) -> Option<String> {
    let local = pinocchio_account_key_derivation(handler_profile, account)?;
    let derivation = proof_profile?.pda_derivations.get(&local.derivation)?;
    let nested_name = derivation
        .local_key_derivations
        .keys()
        .find(|name| name.contains("authority") || name.contains("owner"))
        .or_else(|| derivation.local_key_derivations.keys().next())?;
    Some(format!("{ident}_{nested_name}_key"))
}

fn pinocchio_pda_helper_call(
    derivation: &PinocchioPdaDerivation,
    handler: &ParsedHandler,
    handler_profile: Option<&PinocchioHandlerProfile>,
    current_ident: &str,
    arg_by_param: Option<&BTreeMap<&str, &str>>,
) -> Option<String> {
    let mut args = Vec::new();
    for param in &derivation.params {
        if param == "program_id" {
            args.push("&program_id".to_string());
            continue;
        }
        let source_expr = arg_by_param
            .and_then(|args| args.get(param.as_str()).copied())
            .unwrap_or(param);
        let param_name =
            pinocchio_pda_helper_param_name(handler, handler_profile, source_expr, current_ident)?;
        args.push(pinocchio_pda_helper_arg_expr(
            derivation,
            handler,
            param,
            &param_name,
        ));
    }
    Some(pinocchio_pda_helper_key_expr(derivation, args))
}

fn pinocchio_pda_helper_call_for_local(
    derivation: &PinocchioPdaDerivation,
    local: &PinocchioLocalKeyDerivation,
    handler: &ParsedHandler,
    handler_profile: Option<&PinocchioHandlerProfile>,
    current_ident: &str,
    account_key_exprs: &BTreeMap<String, String>,
    nested_key_exprs: &BTreeMap<String, String>,
) -> Option<String> {
    let arg_by_param = pinocchio_local_arg_by_param(derivation, local);
    let mut args = Vec::new();
    for param in &derivation.params {
        if param == "program_id" {
            args.push("&program_id".to_string());
            continue;
        }
        let source_expr = arg_by_param.get(param.as_str()).copied().unwrap_or(param);
        if let Some(param_name) =
            pinocchio_pda_helper_param_name(handler, handler_profile, source_expr, current_ident)
        {
            args.push(pinocchio_pda_helper_arg_expr(
                derivation,
                handler,
                param,
                &param_name,
            ));
            continue;
        }
        let helper_ty = derivation
            .param_types
            .get(param)
            .map(String::as_str)
            .unwrap_or("&Pubkey");
        if !pinocchio_type_is_pubkey_like(helper_ty) {
            return None;
        }
        let key_expr =
            pinocchio_source_expr_account_key(source_expr, account_key_exprs, handler_profile)
                .or_else(|| nested_key_exprs.get(source_expr.trim()).cloned())?;
        if helper_ty.trim().starts_with('&') {
            args.push(format!("&{key_expr}"));
        } else {
            args.push(key_expr);
        }
    }
    Some(pinocchio_pda_helper_key_expr(derivation, args))
}

fn pinocchio_pda_helper_key_expr(derivation: &PinocchioPdaDerivation, args: Vec<String>) -> String {
    let call = format!("crate::derive_{}({})", derivation.name, args.join(", "));
    if derivation.returns_tuple {
        format!("{call}.0")
    } else {
        call
    }
}

fn pinocchio_owner_account_key_derivation<'a>(
    handler_profile: Option<&'a PinocchioHandlerProfile>,
    owner_ident: &str,
) -> Option<&'a PinocchioLocalKeyDerivation> {
    let profile = handler_profile?;
    profile
        .token_account_bindings
        .iter()
        .find_map(|(token_ident, binding)| {
            let owner_account = binding.owner_account.as_deref()?;
            if pinocchio_binding_account_matches(owner_ident, owner_account, token_ident) {
                binding.owner_key_derivation.as_ref()
            } else {
                None
            }
        })
}

fn pinocchio_binding_account_matches(
    account_ident: &str,
    binding_account: &str,
    token_ident: &str,
) -> bool {
    if account_ident == binding_account {
        return true;
    }
    let Some((_token_base, suffix)) = strip_numeric_suffix(token_ident) else {
        return false;
    };
    account_ident == format!("{binding_account}_{suffix}")
}

fn pinocchio_pda_helper_param_name(
    handler: &ParsedHandler,
    handler_profile: Option<&PinocchioHandlerProfile>,
    helper_param: &str,
    current_ident: &str,
) -> Option<String> {
    let param_name = pinocchio_profile_param_name(handler, helper_param).or_else(|| {
        pinocchio_source_expr_param_name(helper_param, current_ident, handler_profile)
    })?;
    let is_handler_param = handler
        .takes_params
        .iter()
        .any(|(name, _)| to_snake_case(name) == param_name);
    is_handler_param.then_some(param_name)
}

fn pinocchio_type_is_pubkey_like(ty: &str) -> bool {
    matches!(
        ty.trim().trim_start_matches('&').trim(),
        "Pubkey" | "pubkey" | "[u8; 32]"
    )
}

fn pinocchio_pda_helper_arg_expr(
    derivation: &PinocchioPdaDerivation,
    handler: &ParsedHandler,
    helper_param: &str,
    spec_param: &str,
) -> String {
    let Some(helper_ty) = derivation.param_types.get(helper_param) else {
        return spec_param.to_string();
    };
    let Some(spec_ty) = pinocchio_spec_param_rust_type(handler, spec_param) else {
        return spec_param.to_string();
    };
    match numeric_param_rust_type(helper_ty).or_else(|| {
        match helper_ty.trim_start_matches('&').trim() {
            "u8" => Some("u8"),
            "u16" => Some("u16"),
            "u32" => Some("u32"),
            "u64" => Some("u64"),
            _ => None,
        }
    }) {
        Some(helper_rust_ty) if helper_rust_ty != spec_ty => {
            format!("{spec_param} as {helper_rust_ty}")
        }
        _ => spec_param.to_string(),
    }
}

fn pinocchio_account_pda_seed_exprs(
    proof_profile: Option<&PinocchioProofProfile>,
    handler_profile: Option<&PinocchioHandlerProfile>,
    account: &ParsedHandlerAccount,
    handler: &ParsedHandler,
    account_key_exprs: &BTreeMap<String, String>,
) -> Option<Vec<String>> {
    let derivation = pinocchio_account_pda(proof_profile, account)?;
    let _program_expr = pinocchio_pda_program_expr(&derivation.program_id)?;
    derivation
        .seeds
        .iter()
        .map(|seed| pinocchio_pda_seed_expr(seed, handler, handler_profile, account_key_exprs))
        .collect::<Option<Vec<_>>>()
}

fn pinocchio_account_key_expr(
    _proof_profile: Option<&PinocchioProofProfile>,
    _handler_profile: Option<&PinocchioHandlerProfile>,
    _handler: &ParsedHandler,
    account: &ParsedHandlerAccount,
    key_byte: u8,
    is_signer: bool,
    emitted_pda_keys: &BTreeSet<String>,
) -> String {
    let ident = to_snake_case(&account.name);
    if emitted_pda_keys.contains(&ident) {
        return format!("{ident}_key");
    }
    if is_signer {
        "authority_key".to_string()
    } else {
        format!("[{}u8; 32]", key_byte)
    }
}

fn rust_string_escape(input: &str) -> String {
    input.escape_default().to_string()
}

fn emit_pinocchio_profile_notes(
    out: &mut String,
    handler: &ParsedHandler,
    proof_profile: Option<&PinocchioProofProfile>,
    handler_profile: Option<&PinocchioHandlerProfile>,
) {
    out.push_str("/// Proof profile notes:\n");
    if let Some(profile) = handler_profile {
        if profile.accounts.is_empty() {
            out.push_str("/// - source account order: not inferred; using spec order\n");
        } else if let Some(unmatched) = pinocchio_unmatched_profile_account(handler, profile) {
            out.push_str(&format!(
                "/// - source account order: inferred order unusable; profile account `{}` did not match spec accounts; using spec order\n",
                rust_string_escape(unmatched)
            ));
        } else {
            out.push_str(&format!(
                "/// - source account order: {}\n",
                profile.accounts.join(", ")
            ));
        }
        match profile.instruction_tag {
            Some(tag) => out.push_str(&format!("/// - ABI/dispatcher tag: {tag}\n")),
            None => out.push_str("/// - ABI/dispatcher tag: not inferred\n"),
        }
        let pda_notes = handler
            .accounts
            .iter()
            .filter_map(|account| {
                let account_name = to_snake_case(&account.name);
                if let Some(derivation) = pinocchio_account_pda(proof_profile, account) {
                    return Some(format!("{} -> {} (found)", account_name, derivation.name));
                }
                let local = pinocchio_account_key_derivation(handler_profile, account)?;
                let status = if proof_profile
                    .and_then(|profile| profile.pda_derivations.get(&local.derivation))
                    .is_some()
                {
                    "found"
                } else {
                    "missing"
                };
                Some(format!(
                    "{} -> {} ({})",
                    account_name, local.derivation, status
                ))
            })
            .collect::<Vec<_>>();
        if pda_notes.is_empty() {
            out.push_str("/// - PDA derivations: none inferred\n");
        } else {
            out.push_str(&format!(
                "/// - PDA derivations: {}\n",
                pda_notes.join("; ")
            ));
        }
        let token_notes = handler
            .accounts
            .iter()
            .filter_map(|account| {
                let account_name = to_snake_case(&account.name);
                let binding = pinocchio_token_account_binding(handler_profile, account)?;
                Some(format!(
                    "{} mint={} owner={} owner_pda={}",
                    account_name,
                    binding.mint_account.as_deref().unwrap_or("missing"),
                    binding.owner_account.as_deref().unwrap_or("missing"),
                    binding
                        .owner_key_derivation
                        .as_ref()
                        .map(|local| local.derivation.as_str())
                        .unwrap_or("missing")
                ))
            })
            .collect::<Vec<_>>();
        if token_notes.is_empty() {
            out.push_str("/// - token owner/mint projections: none inferred\n");
        } else {
            out.push_str(&format!(
                "/// - token owner/mint projections: {}\n",
                token_notes.join("; ")
            ));
        }
    } else {
        out.push_str("/// - source account order: profile unavailable; using spec order\n");
        out.push_str("/// - ABI/dispatcher tag: profile unavailable\n");
        out.push_str("/// - PDA derivations: profile unavailable\n");
        out.push_str("/// - token owner/mint projections: profile unavailable\n");
    }
}

/// Emit one `#[kani::proof]` harness for a Pinocchio handler. Builds
/// symbolic stack accounts from the handler's `accounts {}` block, packs
/// symbolic params into instruction data, and calls the committed
/// `process_instruction` dispatcher. Kani's automatic overflow / UB checks do
/// the verification; spec `ensures` clauses are emitted as reference comments.
fn emit_pinocchio_handler_harness(
    out: &mut String,
    handler: &ParsedHandler,
    _spec: &ParsedSpec,
    proof_profile: Option<&PinocchioProofProfile>,
    handler_profile: Option<&PinocchioHandlerProfile>,
) -> Result<()> {
    let snake = to_snake_case(&handler.name);
    let token_assertions = pinocchio_token_transfer_assertions(handler);
    let token_amount_witnesses = pinocchio_token_amount_witnesses(handler, &token_assertions);

    let transfer_token_accounts: BTreeSet<String> = token_assertions
        .iter()
        .flat_map(|assertion| [assertion.from.clone(), assertion.to.clone()])
        .collect();
    // Classify token accounts from the spec's explicit account type or from
    // Token.transfer resources. A `program, type token` account is the SPL
    // Token program, not an SPL Token account layout.
    let is_token = |a: &ParsedHandlerAccount| {
        let role = pinocchio_account_role(handler_profile, a);
        (!pinocchio_account_is_program(a, role) && pinocchio_account_type(a, role) == Some("token"))
            || transfer_token_accounts.contains(&to_snake_case(&a.name))
    };
    let is_mint = |a: &ParsedHandlerAccount| {
        let role = pinocchio_account_role(handler_profile, a);
        pinocchio_account_type(a, role) == Some("mint")
    };

    // The first signer's key threads through as the token-account owner
    // so the handler's owner check passes. Falls back to a fixed key.
    let authority_idx = handler.accounts.iter().position(|a| a.is_signer);

    let unwind_bound = pinocchio_kani_unwind_bound();

    out.push_str(&format!(
        "/// Impl-targeted harness for `{}`. Kani's automatic\n",
        handler.name
    ));
    out.push_str("/// overflow / underflow / UB checks run on every path through the\n");
    out.push_str(&format!(
        "/// real handler. `#[kani::unwind({unwind_bound})]` bounds Pinocchio's 32-byte\n"
    ));
    out.push_str("/// pubkey/account comparisons plus generated valid-path loops.\n");
    emit_pinocchio_profile_notes(out, handler, proof_profile, handler_profile);
    out.push_str("#[kani::proof]\n");
    if let Some(profile) = handler_profile {
        for stub in &profile.verified_stubs {
            out.push_str(&format!("#[kani::stub_verified({stub})]\n"));
        }
    }
    out.push_str(&format!("#[kani::unwind({unwind_bound})]\n"));
    out.push_str(&format!("fn verify_{}_impl() {{\n", snake));

    // Use a concrete generic program id witness. Pinocchio PDA/key models
    // thread the id through account keys; keeping it symbolic makes otherwise
    // concrete valid-path proofs much larger without adding useful coverage
    // for generated state/token postconditions.
    out.push_str("    let program_id: [u8; 32] = [42u8; 32];\n");

    // Authority key (concrete) — threaded as token owner.
    out.push_str("    let authority_key: [u8; 32] = [7u8; 32];\n");

    // Symbolic amounts for token accounts.
    for (i, a) in handler.accounts.iter().enumerate() {
        let ident = to_snake_case(&a.name);
        if is_token(a) && !token_amount_witnesses.contains_key(&ident) {
            out.push_str(&format!("    let {}_amount: u64 = kani::any();\n", ident));
        }
        let _ = i;
    }

    // Symbolic params, declared with the primitive matching the spec
    // type so `to_le_bytes()` packs the right width below.
    let concrete_scalar_probe_mode = pinocchio_kani_concrete_scalar_probe_mode();
    if let Some(mode) = concrete_scalar_probe_mode.as_deref() {
        out.push_str(
            "    // TRIAGE ONLY: concrete scalar probe mode is not accepted proof evidence.\n",
        );
        out.push_str(&format!("    // Concrete scalar probe family: {mode}\n"));
    }
    for (pname, ptype) in &handler.takes_params {
        let param_name = to_snake_case(pname);
        if pinocchio_is_fee_normalization_ghost_param(handler, &param_name) {
            continue;
        }
        match pinocchio_param_decl_rust_type(&param_name, ptype, handler, handler_profile) {
            Some(rust_ty) => {
                let rust_ty = pinocchio_narrow_symbolic_param_type(handler, &param_name, rust_ty);
                if let Some(value) = pinocchio_literal_requires_eq(handler, &param_name) {
                    out.push_str(&format!(
                        "    let {}: {} = {} as {}; // spec type: {}\n",
                        param_name, rust_ty, value, rust_ty, ptype
                    ));
                } else if let Some(mode) = concrete_scalar_probe_mode.as_deref() {
                    if let Some(value) =
                        pinocchio_concrete_scalar_probe_value(&param_name, &rust_ty, mode)
                    {
                        out.push_str(&format!(
                            "    let {}: {} = {}; // spec type: {}, concrete probe\n",
                            param_name, rust_ty, value, ptype
                        ));
                    } else {
                        out.push_str(&format!(
                            "    let {}: {} = kani::any(); // spec type: {}\n",
                            param_name, rust_ty, ptype
                        ));
                    }
                } else {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any(); // spec type: {}\n",
                        param_name, rust_ty, ptype
                    ));
                }
                let lane_base =
                    strip_numeric_suffix(&param_name).map_or(param_name.as_str(), |(base, _)| base);
                if matches!(lane_base, "lane_id" | "from_lane_id" | "to_lane_id") {
                    out.push_str(&format!("    kani::assume({param_name} < 2);\n"));
                }
            }
            None => {
                out.push_str(&format!(
                    "    // TODO: declare symbolic param `{}` (spec type {})\n",
                    pname, ptype
                ));
            }
        }
    }
    emit_pinocchio_requires_assumptions(out, handler);
    emit_pinocchio_profile_width_assumptions(out, handler, handler_profile);
    emit_pinocchio_fee_normalization_assumptions(out, handler, handler_profile);
    out.push('\n');

    // Build accounts in source order when the profile inferred it; fall
    // back to spec order for greenfield/generated specs without source.
    let ordered_accounts = pinocchio_account_order(handler, handler_profile);
    let state_assertions = pinocchio_state_assertions(handler, &ordered_accounts, proof_profile);
    let mut preliminary_account_key_exprs = BTreeMap::new();
    for (i, a) in ordered_accounts.iter().enumerate() {
        let ident = to_snake_case(&a.name);
        let role = pinocchio_account_role(handler_profile, a);
        let key_expr = if pinocchio_account_is_program(a, role)
            && pinocchio_account_type(a, role) == Some("token")
        {
            "SPL_TOKEN_PROGRAM_ID".to_string()
        } else if pinocchio_account_is_signer(a, role) {
            "authority_key".to_string()
        } else {
            format!("[{}u8; 32]", i + 1)
        };
        preliminary_account_key_exprs.insert(ident, key_expr);
    }
    let emitted_pda_keys = emit_pinocchio_pda_key_bindings(
        out,
        PinocchioPdaKeyBindingsCtx {
            handler,
            proof_profile,
            handler_profile,
            ordered_accounts: &ordered_accounts,
            account_key_exprs: &preliminary_account_key_exprs,
        },
    );
    let mut account_key_exprs = BTreeMap::new();
    for (i, a) in ordered_accounts.iter().enumerate() {
        let ident = to_snake_case(&a.name);
        let key_byte = (i + 1) as u8;
        let role = pinocchio_account_role(handler_profile, a);
        let is_signer = pinocchio_account_is_signer(a, role);
        let mut key_expr = pinocchio_account_key_expr(
            proof_profile,
            handler_profile,
            handler,
            a,
            key_byte,
            is_signer,
            &emitted_pda_keys,
        );
        if pinocchio_account_is_program(a, role) && pinocchio_account_type(a, role) == Some("token")
        {
            key_expr = "SPL_TOKEN_PROGRAM_ID".to_string();
        }
        account_key_exprs.insert(ident, key_expr);
    }

    let mut acct_idents: Vec<String> = Vec::with_capacity(ordered_accounts.len());
    for (i, a) in ordered_accounts.iter().enumerate() {
        let ident = to_snake_case(&a.name);
        acct_idents.push(ident.clone());
        let key_byte = (i + 1) as u8;
        let role = pinocchio_account_role(handler_profile, a);
        let is_writable = pinocchio_account_is_writable(a, role);
        let is_signer = pinocchio_account_is_signer(a, role);
        let key_expr = account_key_exprs
            .get(&ident)
            .cloned()
            .unwrap_or_else(|| format!("[{}u8; 32]", key_byte));
        if is_token(a) {
            let binding = pinocchio_token_account_binding(handler_profile, a);
            let mint_expr = binding
                .and_then(|binding| binding.mint_account.as_ref())
                .and_then(|account| pinocchio_bound_key_expr(&account_key_exprs, &ident, account))
                .unwrap_or_else(|| "[0u8; 32]".to_string());
            let owner_expr = binding
                .and_then(|binding| binding.owner_account.as_ref())
                .and_then(|account| pinocchio_bound_key_expr(&account_key_exprs, &ident, account))
                .or_else(|| {
                    let local = binding.and_then(|binding| binding.owner_key_derivation.as_ref())?;
                    let seed_exprs = pinocchio_local_derived_key_seed_exprs(
                        proof_profile,
                        handler_profile,
                        handler,
                        &ident,
                        local,
                        &account_key_exprs,
                        &BTreeMap::new(),
                    )?;
                    let derivation = proof_profile?.pda_derivations.get(&local.derivation)?;
                    let program_expr = pinocchio_pda_program_expr(&derivation.program_id)?;
                    let arg_by_param = pinocchio_local_arg_by_param(derivation, local);
                    if let Some(helper_call) =
                        pinocchio_pda_helper_call(
                            derivation,
                            handler,
                            handler_profile,
                            &ident,
                            Some(&arg_by_param),
                        )
                    {
                        out.push_str(&format!("    let {ident}_owner_key = {helper_call};\n"));
                    } else {
                        out.push_str(&format!(
                            "    let {ident}_owner_pda = pinocchio::pubkey::try_find_program_address(&[{}], {});\n",
                            seed_exprs.join(", "),
                            program_expr
                        ));
                        out.push_str(&format!("    kani::assume({ident}_owner_pda.is_some());\n"));
                        out.push_str(&format!("    let {ident}_owner_key = {ident}_owner_pda.unwrap().0;\n"));
                    }
                    Some(format!("{ident}_owner_key"))
                })
                .or_else(|| {
                    pinocchio_nested_owner_key_var_for_account(
                        proof_profile,
                        handler_profile,
                        a,
                        &ident,
                    )
                })
                .unwrap_or_else(|| {
                    if authority_idx.is_some() && i == 0 {
                        "authority_key".to_string()
                    } else {
                        "[9u8; 32]".to_string()
                    }
                });
            out.push_str(&format!(
                "    let mut {ident} = build_token_account({key_expr}, {writable}, {signer}, {mint_expr}, {owner_expr}, {amount_expr});\n",
                ident = ident,
                key_expr = key_expr,
                writable = is_writable,
                signer = is_signer,
                mint_expr = mint_expr,
                owner_expr = owner_expr,
                amount_expr = token_amount_witnesses
                    .get(&ident)
                    .map(|amount| pinocchio_token_amount_expr(amount))
                    .unwrap_or_else(|| pinocchio_token_amount_expr(&format!("{ident}_amount"))),
            ));
        } else if is_mint(a) {
            let decimals_expr = pinocchio_mint_decimal_param(handler_profile, &ident)
                .filter(|param| {
                    handler
                        .takes_params
                        .iter()
                        .any(|(name, _)| to_snake_case(name) == *param)
                })
                .map(|param| format!("({param} as u8)"))
                .unwrap_or_else(|| "6u8".to_string());
            out.push_str(&format!(
                "    let mut {ident} = build_mint_account({key_expr}, {signer}, {writable}, {decimals_expr});\n",
                ident = ident,
                key_expr = key_expr,
                signer = is_signer,
                writable = is_writable,
                decimals_expr = decimals_expr,
            ));
        } else if let Some(layout) = pinocchio_account_layout(proof_profile, a) {
            out.push_str(&format!(
                "    // ABI account layout `{}`: {} byte data region.\n",
                layout.name, layout.len
            ));
            out.push_str(&format!(
                "    let mut {ident}_data: [u8; {len}] = [0u8; {len}];\n",
                ident = ident,
                len = layout.len
            ));
            for field in &layout.fields {
                if let Some(bytes) = &field.fixed_bytes {
                    pinocchio_emit_fixed_array_bytes_write(
                        out,
                        &format!("{ident}_data"),
                        field.offset,
                        bytes,
                    );
                }
            }
            out.push_str(&format!(
                "    let mut {ident} = build_data_account({key_expr}, program_id, {signer}, {writable}, {ident}_data);\n",
                ident = ident,
                key_expr = key_expr,
                signer = is_signer,
                writable = is_writable,
            ));
            emit_pinocchio_state_witness_initializers(
                out,
                &ident,
                layout,
                handler,
                &account_key_exprs,
            );
        } else {
            out.push_str(&format!(
                "    let mut {ident} = build_minimal_account({key_expr}, {signer}, {writable});\n",
                ident = ident,
                key_expr = key_expr,
                signer = is_signer,
                writable = is_writable,
            ));
        }
    }
    out.push('\n');

    // Assemble the AccountInfo array.
    let n = acct_idents.len();
    out.push_str(&format!(
        "    let accounts: [ManuallyDrop<AccountInfo>; {n}] = unsafe {{\n        [\n"
    ));
    for ident in &acct_idents {
        out.push_str(&format!(
            "            ManuallyDrop::new(account_info_from_stack(&mut {ident})),\n"
        ));
    }
    out.push_str("        ]\n    };\n");
    out.push_str(&format!(
        "    let accounts_slice: &[AccountInfo] = unsafe {{\n        core::slice::from_raw_parts(&accounts as *const _ as *const AccountInfo, {n})\n    }};\n\n"
    ));

    // Pack instruction data from the committed instruction tag + ABI fields.
    emit_pinocchio_instruction_data_pack_with_profile(out, handler, handler_profile);
    out.push('\n');

    emit_pinocchio_state_pre_snapshots(out, &state_assertions);
    emit_pinocchio_token_pre_snapshots(out, &token_assertions);

    // Call the committed dispatcher.
    out.push_str("    // Call the user's real dispatcher. Kani's automatic checks\n");
    out.push_str("    // (overflow / underflow / pointer UB) verify this path.\n");
    out.push_str("    let _result = crate::process_instruction(&program_id, accounts_slice, &instruction_data);\n");
    out.push_str(
        "    kani::cover!(_result.is_ok(), \"impl success path reachable under generated profile\");\n",
    );
    out.push_str(
        "    assert!(_result.is_ok(), \"generated valid ABI/profile witness should reach success\");\n",
    );

    emit_pinocchio_state_post_assertions(out, &state_assertions);
    emit_pinocchio_token_post_assertions(out, &token_assertions);

    // Reference: spec ensures clauses. Kani's built-in checks already
    // cover arithmetic UB; explicit cross-field invariant assertions go
    // here if the ensures expresses something Kani can't infer.
    if !handler.ensures.is_empty() {
        out.push('\n');
        out.push_str("    // Spec ensures (reference). Kani's automatic checks cover\n");
        out.push_str("    // arithmetic UB; add explicit assertions below for cross-field\n");
        out.push_str("    // invariants (read post-state via read_token_amount(&<acct>)).\n");
        for e in &handler.ensures {
            out.push_str(&format!("    //   ensures {}\n", e.rust_expr_binary.trim()));
        }
    }

    out.push_str("}\n\n");
    Ok(())
}

/// Emit `mod symbolic_accounts { ... }` with one `build_<handler>` ctor per
/// emit target. The body is a `todo!()` skeleton that lists each account
/// field with its derivation rule (PDA seed expression vs `kani::any()`)
/// as inline comments. The agent (or user) replaces the body with the
/// concrete `crate::<HandlerPascal> { ... }` construction.
fn emit_symbolic_accounts_module(
    out: &mut String,
    spec: &ParsedSpec,
    targets: &[&ParsedHandler],
    framework: &str,
) -> Result<()> {
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str(&format!(
        "// Symbolic {} `Accounts` context builders.\n",
        framework
    ));
    out.push_str("//\n");
    out.push_str("// Each ctor returns a context with:\n");
    out.push_str("//   - PDA-derived pubkeys computed from the spec's `pda` declarations\n");
    out.push_str("//   - `kani::any()` for non-PDA addresses + account-data fields\n");
    out.push_str("//   - Well-known program IDs for `token_program`, `system_program`, etc.\n");
    out.push_str("//\n");
    out.push_str("// The ctors are AGENT-FILL skeletons: the data-bearing fields\n");
    out.push_str("// (state struct contents, token amounts, mints) get populated to\n");
    out.push_str("// match the user's handler signature. Without that fill, the file\n");
    out.push_str("// won't compile — by design — so it surfaces as a `todo!()` to address.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    out.push_str("mod symbolic_accounts {\n");
    out.push_str(&format!(
        "    // The user's program crate is the host for this harness. {}\n",
        framework
    ));
    out.push_str("    // re-exports `#[derive(Accounts)]` structs at crate root via\n");
    out.push_str("    // `#[program]`, so the handler's accounts struct resolves via\n");
    out.push_str("    // `crate::<HandlerPascal>`.\n");
    out.push_str("    #![allow(unused_imports, dead_code)]\n");

    for handler in targets {
        emit_symbolic_accounts_ctor(out, handler, spec, framework)?;
    }

    out.push_str("} // mod symbolic_accounts\n\n");
    Ok(())
}

/// Emit a single `pub fn build_<handler>() -> crate::<Pascal>` constructor.
/// Body is a `todo!()` skeleton with per-account-field derivation comments
/// for agent fill-in.
fn emit_symbolic_accounts_ctor(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    framework: &str,
) -> Result<()> {
    let pascal = to_pascal_case(&handler.name);
    out.push_str(&format!(
        "\n    /// Symbolic `Accounts` context for the user's `{}` handler.\n",
        handler.name
    ));
    out.push_str("    ///\n");
    out.push_str("    /// AGENT-FILL: replace the `todo!()` body with the concrete\n");
    out.push_str("    /// construction. Each account field is annotated with its\n");
    out.push_str("    /// derivation rule below.\n");
    out.push_str(&format!(
        "    pub fn build_{}() -> crate::{} {{\n",
        handler.name, pascal
    ));

    if handler.accounts.is_empty() {
        if handler.who.is_some() {
            out.push_str(
                "        // No explicit accounts; spec declares an `auth` actor → signer.\n",
            );
            out.push_str(&format!(
                "        todo!(\"Construct crate::{} with a symbolic signer\")\n",
                pascal
            ));
        } else {
            out.push_str("        // No accounts declared on this handler.\n");
            out.push_str(&format!(
                "        todo!(\"Construct crate::{} with the handler's account context\")\n",
                pascal
            ));
        }
    } else {
        for acct in &handler.accounts {
            emit_account_field_skeleton(out, acct, handler, spec);
        }
        out.push_str("        //\n");
        out.push_str("        // AGENT: assemble the fields above into the concrete\n");
        out.push_str(&format!(
            "        // `crate::{}` struct. The {} `#[derive(Accounts)]`\n",
            pascal, framework
        ));
        out.push_str("        // expansion gives the exact field layout.\n");
        out.push_str(&format!("        todo!(\"assemble crate::{}\")\n", pascal));
    }

    out.push_str("    }\n");
    Ok(())
}

/// `true` for the DSL's integer scalar types — these serialize to a seed
/// via `to_le_bytes()` rather than `as_ref()`.
fn is_integer_dsl_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" | "Nat"
    )
}

/// Emit one commented-out line per account field with its derivation rule.
/// PDA-bound accounts get a `Pubkey::find_program_address` template using
/// the spec's `pda <name> [seeds]` declaration; non-PDA fields default to
/// `kani::any()`; programs use their well-known IDs.
fn emit_account_field_skeleton(
    out: &mut String,
    acct: &crate::check::ParsedHandlerAccount,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) {
    if acct.is_program {
        out.push_str(&format!(
            "        // `{}`: well-known program ID (e.g. token / system / rent)\n",
            acct.name
        ));
        return;
    }
    if let Some(seeds) = &acct.pda_seeds {
        // Prefer the top-level `pda <name> [seeds]` declaration when it
        // matches by name; fall back to the inline seeds otherwise.
        let pda_seeds: Vec<String> = spec
            .pdas
            .iter()
            .find(|p| p.name == acct.name)
            .map(|p| p.seeds.clone())
            .unwrap_or_else(|| seeds.clone());
        let seed_exprs: Vec<String> = pda_seeds
            .iter()
            .map(|s| {
                if (s.starts_with('"') && s.ends_with('"'))
                    || (s.starts_with('\'') && s.ends_with('\''))
                {
                    let inner = &s[1..s.len() - 1];
                    format!("b\"{}\"", inner)
                } else if handler
                    .takes_params
                    .iter()
                    .any(|(n, t)| n == s && is_integer_dsl_type(t))
                {
                    // An integer handler param used as a seed must be
                    // serialized to bytes — `u64::as_ref()` doesn't exist.
                    // (`Pubkey` params / account keys keep `.as_ref()`.)
                    format!("{}.to_le_bytes().as_ref()", s)
                } else {
                    format!("{}.as_ref()", s)
                }
            })
            .collect();
        out.push_str(&format!(
            "        // `{}`: PDA derived from `[{}]`\n",
            acct.name,
            seed_exprs.join(", ")
        ));
        out.push_str(&format!(
            "        //   let ({0}_key, _bump) = solana_program::pubkey::Pubkey::find_program_address(&[{1}], &crate::ID);\n",
            acct.name,
            seed_exprs.join(", ")
        ));
        return;
    }
    if acct.is_signer {
        out.push_str(&format!(
            "        // `{}`: signer — symbolic address via `kani::any()`\n",
            acct.name
        ));
        return;
    }
    out.push_str(&format!(
        "        // `{}`: non-PDA account — symbolic address + data via `kani::any()`\n",
        acct.name
    ));
}

/// Emit one `#[kani::proof]` for a (handler, ensures) pair. Shape:
///   1. Build symbolic accounts context via the `symbolic_accounts` module.
///   2. Snapshot pre-state fields (the modifies set, plus any field the
///      ensures' `rust_expr_binary` reads via `pre.<field>`).
///   3. Declare symbolic params + `kani::assume` the handler's requires.
///   4. Call the user's real handler method.
///   5. On `Ok`, snapshot post-state fields, splice CPI ensures-as-fact
///      `kani::assume` lines for each `call Iface.foo(...)` whose callee
///      declares ensures (Track I), then assert the caller's own ensures.
fn emit_handler_harness(
    out: &mut String,
    handler: &ParsedHandler,
    idx: usize,
    ensures: &crate::check::ParsedEnsures,
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("#[kani::proof]\n");
    out.push_str("#[kani::unwind(2)]\n");
    out.push_str("#[kani::solver(cadical)]\n");
    out.push_str(&format!(
        "fn verify_{}_impl_ensures_{}() {{\n",
        handler.name, idx
    ));

    // 1. Build the symbolic accounts context.
    out.push_str(&format!(
        "    let mut accounts = symbolic_accounts::build_{}();\n",
        handler.name
    ));

    // 2. Pre-snapshot. Snapshot every field the ensures clause may compare
    //    across the call (union of `modifies` and effect-LHS bare field
    //    names). Path is `accounts.<state_account>.<field>` when the
    //    state account is uniquely identifiable; otherwise the snapshot
    //    falls back to a `todo!()` placeholder for the agent.
    let state_acct = find_state_account_name(handler);
    let snapshot_fields = collect_snapshot_fields(handler);
    if !snapshot_fields.is_empty() {
        out.push_str(
            "    // Pre-state snapshot — fields the ensures clause reads via `pre.<x>`.\n",
        );
        for field in &snapshot_fields {
            match state_acct {
                Some(acct) => {
                    out.push_str(&format!(
                        "    let pre_{0} = accounts.{1}.{0};\n",
                        field, acct
                    ));
                }
                None => {
                    out.push_str(&format!(
                        "    let pre_{0} = todo!(\"snapshot pre.{0} from the symbolic accounts context\");\n",
                        field
                    ));
                }
            }
        }
    }

    // 3. Symbolic params + preconditions.
    for (pname, ptype) in &handler.takes_params {
        out.push_str(&format!(
            "    let {}: {} = kani::any();\n",
            pname,
            map_type(ptype, spec)?
        ));
    }
    // Apply the handler's `requires` clauses as Kani assumptions so we
    // explore inputs the user's handler would actually accept (otherwise
    // it returns Err and the ensures don't fire — vacuous pass).
    if let Some(full_guard) = crate::rust_codegen_util::collect_full_guard(handler, false) {
        out.push_str(&format!("    kani::assume({});\n", full_guard));
    }

    // 4. Call the user's real handler. Anchor handler methods take
    //    `&mut self` and the param list — same shape `cargo build`
    //    expands `#[derive(Accounts)]` + `#[program]` into.
    let args: String = handler
        .takes_params
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("    let result = accounts.handler({});\n", args));

    // 5. Post-snapshot + assertion. The Track I splice point sits between
    //    `if result.is_ok()` and the `assert!` so CPI ensures can be
    //    layered in as `kani::assume` facts.
    out.push_str("    if result.is_ok() {\n");
    if !snapshot_fields.is_empty() {
        out.push_str(
            "        // Post-state snapshot — same fields, read from post-call accounts.\n",
        );
        for field in &snapshot_fields {
            match state_acct {
                Some(acct) => {
                    out.push_str(&format!(
                        "        let post_{0} = accounts.{1}.{0};\n",
                        field, acct
                    ));
                }
                None => {
                    out.push_str(&format!(
                        "        let post_{0} = todo!(\"snapshot post.{0} from the symbolic accounts context\");\n",
                        field
                    ));
                }
            }
        }
    }

    // ── CPI ensures-as-fact (Track I) ──────────────────────────────────
    // For every `call Iface.foo(args)` site whose callee declares its own
    // `ensures`, splice a `kani::assume(<callee_ensures, substituted>)`
    // line so the caller's later assert! can rely on the CPI's
    // contract. Tier-0 callees (no ensures declared) emit nothing —
    // matching the spec-model harness behavior in `kani.rs` and the
    // `lean_gen.rs::render_cpi_theorems` `:= by sorry` fallback.
    //
    // The substituted clauses come back in `pre.X` / `post.X` form (from
    // `rust_expr_binary`); we flatten those to the harness-local
    // `pre_X` / `post_X` snapshots via the same `rewrite_pre_post_paths`
    // helper used on the caller's own ensures below.
    emit_cpi_ensures_as_assume(out, handler, spec);

    // The ensures clause's `rust_expr_binary` uses `pre.<field>` and
    // `post.<field>` paths. Our snapshots are flat `pre_<field>` /
    // `post_<field>` locals (no struct), so we rewrite the path
    // separators. The chumsky_adapter renders `state.x` / `old(state.x)`
    // into exactly `post.x` / `pre.x` — no other source produces these
    // tokens in `rust_expr_binary`, so a string-replace is safe.
    let lowered = rewrite_pre_post_paths(&ensures.rust_expr_binary);
    out.push_str(&format!("        assert!(\n            {},\n", lowered));
    out.push_str(&format!(
        "            \"ensures clause {} on {} (impl) violated\"\n",
        idx, handler.name
    ));
    out.push_str("        );\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Walk `handler.calls` and, for each CPI whose callee declares ensures,
/// emit a `// CPI ensures-as-fact (Iface.handler):` comment followed by one
/// `kani::assume(<substituted_clause>);` per ensures clause. Tier-0 callees
/// (empty ensures) emit nothing — same fallback as the spec-model harness
/// in `kani.rs` and `lean_gen.rs::render_cpi_theorems`'s `:= by sorry`.
///
/// Substitution reuses `crate::cpi_substitute::substitute_callee_ensures_rust_binary`
/// — the same helper the spec-model harness uses, so the two backends
/// agree on the `let X = call ...` `result` convention and word-boundary
/// param matching. After substitution we apply `rewrite_pre_post_paths`
/// (same transformation step the caller's own `assert!` emission uses)
/// to flatten `pre.X` / `post.X` paths to the harness-local
/// `pre_X` / `post_X` snapshots.
///
/// **Track J breadcrumb**: when `check::multi_cpi_shared_fields` reports any
/// shared `pre.X` / `post.X` reference across two callees of this handler,
/// emit a WARNING comment above the assume block. The lint
/// `multi_cpi_same_field` carries the structured guidance; this is a
/// reader-of-generated-code breadcrumb so the harness itself flags the
/// over-constraint risk without the user needing to cross-reference the
/// lint output.
fn emit_cpi_ensures_as_assume(out: &mut String, handler: &ParsedHandler, spec: &ParsedSpec) {
    // Track J — emit the breadcrumb once, above the entire CPI assume block,
    // when the lint predicate fires for this handler.
    let shared = check::multi_cpi_shared_fields(spec, handler);
    if !shared.is_empty() {
        out.push_str("        // WARNING: multi-CPI ordering — this handler has ≥2 calls whose\n");
        out.push_str(
            "        // ensures reference the same caller-state field. Both kani::assume\n",
        );
        out.push_str("        // lines fire at the same splice point against one (pre, post)\n");
        out.push_str("        // snapshot pair, which may over-constrain. See lint\n");
        out.push_str("        // `multi_cpi_same_field` for context.\n");
    }
    for call in &handler.calls {
        let Some(iface) = spec
            .interfaces
            .iter()
            .find(|i| i.name == call.target_interface)
        else {
            continue;
        };
        let Some(callee) = iface
            .handlers
            .iter()
            .find(|h| h.name == call.target_handler)
        else {
            continue;
        };
        if callee.ensures.is_empty() {
            // Tier-0 callee — `cpi_no_callee_ensures` lint surfaces the gap.
            continue;
        }
        out.push_str(&format!(
            "        // CPI ensures-as-fact ({}.{}):\n",
            call.target_interface, call.target_handler,
        ));
        for callee_ens in &callee.ensures {
            let substituted = crate::cpi_substitute::substitute_callee_ensures_rust_binary(
                &callee_ens.rust_expr_binary,
                call,
                &callee.params,
                // v2.26 Track K — propagate the declared return-binder
                // name. `None` keeps the literal "result" convention.
                callee.result_binder.as_deref(),
            );
            let lowered = rewrite_pre_post_paths(&substituted);
            out.push_str(&format!("        kani::assume({});\n", lowered));
        }
    }
}

/// Find the handler's writable state account by name. v2.26 Slice 1 uses a
/// simple heuristic: the unique writable non-program, non-signer, non-token,
/// non-mint account. Matches the integration_test scaffolding convention
/// (the program's state PDA is the canonical "state" account; signers /
/// mints / token accounts are separate). Returns `None` when the heuristic
/// can't pick a unique state account — the harness then emits per-field
/// `todo!()` snapshot placeholders for the agent to resolve.
fn find_state_account_name(handler: &ParsedHandler) -> Option<&str> {
    let candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| {
            a.is_writable
                && !a.is_program
                && !a.is_signer
                && a.account_type.as_deref() != Some("token")
                && a.account_type.as_deref() != Some("mint")
        })
        .collect();
    if candidates.len() == 1 {
        Some(candidates[0].name.as_str())
    } else {
        None
    }
}

/// The union of `modifies`, effect-LHS bare field names, and v2.27
/// Track A state-binder caller fields — every field the ensures clause
/// might read across the pre/post boundary. Used to drive snapshot
/// emission.
///
/// Track A: when a `call X.y(state_binders { from_balance = state.X })`
/// is present, the CPI assume splice references `pre.X` / `post.X`
/// (the substitution rewrote `pre.from_balance` → `pre.X`). The
/// `rewrite_pre_post_paths` flatten then turns those into `pre_X` /
/// `post_X` locals — which only exist if the snapshot emitter
/// captured `X`. Including binder caller fields here closes that loop.
fn collect_snapshot_fields(handler: &ParsedHandler) -> Vec<String> {
    let mut fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(modifies) = &handler.modifies {
        for f in modifies {
            fields.insert(f.clone());
        }
    }
    for (lhs, _, _) in &handler.effects {
        let bare = crate::rust_codegen_util::effect_target_base(lhs);
        fields.insert(bare.to_string());
    }
    // v2.27 Track A — pick up every caller-side field referenced by a
    // state binder on any CPI call in this handler. Without this the
    // CPI assume splice would reference `pre_X` / `post_X` locals the
    // snapshot block never declared, producing a compile error in the
    // generated harness.
    for call in &handler.calls {
        for binder in &call.state_binders {
            fields.insert(binder.caller_field.clone());
        }
    }
    fields.into_iter().collect()
}

/// Rewrite `pre.<field>` → `pre_<field>` and `post.<field>` → `post_<field>`
/// in the rendered ensures expression. The chumsky_adapter renders
/// `state.x` / `old(state.x)` into exactly `post.x` / `pre.x` in the
/// binary-mode form — no other source produces these tokens — so a plain
/// string replace is safe.
fn rewrite_pre_post_paths(expr: &str) -> String {
    expr.replace("pre.", "pre_").replace("post.", "post_")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter::parse_str;

    /// A conservation ensures (`post.X == pre.X + delta`) must NOT classify
    /// `X` as unchanged: the rendered string contains `post.X==pre.X` as a
    /// substring, and an unanchored match emits an equality assertion that
    /// fails on a correct implementation.
    #[test]
    fn unchanged_fields_exclude_pre_plus_delta_ensures() {
        assert_eq!(
            pinocchio_unchanged_ensures_fields(
                "post.fee_pool == pre.fee_pool + fee && post.admin == pre.admin"
            ),
            vec!["admin".to_string()],
        );
        // Reversed orientation continues the same way.
        assert_eq!(
            pinocchio_unchanged_ensures_fields("pre.fee_pool == post.fee_pool - fee"),
            Vec::<String>::new(),
        );
        // Genuinely-unchanged claims still match at every anchored position:
        // end of expression, before `&&`, and inside parens.
        assert_eq!(
            pinocchio_unchanged_ensures_fields(
                "(post.vault == pre.vault) && post.total == pre.total"
            ),
            vec!["total".to_string(), "vault".to_string()],
        );
    }

    /// Auto-trigger fires when a handler has `modifies` listing a field
    /// that's absent from the effect block's LHS set (the LP-deposit
    /// shape).
    #[test]
    fn auto_trigger_fires_on_lp_shape() {
        let src = r#"spec Pool
state { pool_balance : U64, lp_supply : U64 }
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  ensures state.pool_balance == old(state.pool_balance) + amount
  effect {
    pool_balance += amount
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(
            handler_triggers_impl_harness(h),
            "modifies = [pool_balance, lp_supply] but effect only writes pool_balance → trigger",
        );
        assert!(spec_triggers_impl_harness(&spec));
    }

    /// Auto-trigger does NOT fire when modifies matches the effect-LHS
    /// set (no LP-shape gap — the spec's effect block covers every
    /// declared write).
    #[test]
    fn auto_trigger_silent_when_modifies_matches_effects() {
        let src = r#"spec Counter
state { count : U64 }
handler bump (delta : U64) {
  requires delta > 0 else InvalidAmount
  modifies [count]
  ensures state.count == old(state.count) + delta
  effect {
    count += delta
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(
            !handler_triggers_impl_harness(h),
            "modifies = [count] = effect LHS = {{count}} → no trigger",
        );
        assert!(!spec_triggers_impl_harness(&spec));
    }

    /// Auto-trigger silent when no `modifies` clause is declared at all.
    /// Bundled examples today take this path.
    #[test]
    fn auto_trigger_silent_without_modifies() {
        let src = r#"spec NoModifies
state { x : U64 }
handler set_x (v : U64) {
  ensures state.x == v
  effect { x := v }
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(!spec_triggers_impl_harness(&spec));
    }

    /// Slice 5: Quasar emits the struct-based impl harness, reusing the
    /// Anchor symbolic-accounts builder + per-handler proof emitter (the
    /// Quasar scaffold's `Ctx<X>` dispatcher forwards to the same
    /// `impl <Pascal> { fn handler(&mut self, …) }` method). The header
    /// must be Quasar-flavored and must NOT leak the Anchor framework
    /// crates or the Pinocchio stack scaffold.
    #[test]
    fn quasar_target_emits_handler_harness() {
        let src = r#"spec QuasarBump
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
        let spec = parse_str(src).expect("parse");

        let tmp = std::env::temp_dir().join(format!("kani_impl_quasar_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Quasar)
            .expect("Quasar kani_impl must emit");
        assert!(tmp.is_file(), "Quasar target must write a harness file");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // Quasar-flavored header.
        assert!(
            body.contains("Quasar (`#[program]`) program"),
            "header must name the Quasar framework; got:\n{body}"
        );
        assert!(
            body.contains("#[cfg(kani)] mod kani_impl;"),
            "header must document the src/lib.rs placement line; got:\n{body}"
        );

        // Struct-based shape, shared with Anchor.
        assert!(
            body.contains("mod symbolic_accounts {")
                && body.contains("pub fn build_bump() -> crate::Bump"),
            "must emit the symbolic accounts builder for crate::Bump; got:\n{body}"
        );
        assert!(
            body.contains("fn verify_bump_impl_ensures_0()")
                && body.contains("accounts.handler(delta)"),
            "must emit the per-handler proof calling the real .handler(); got:\n{body}"
        );

        // The shared module comment must say "Quasar", not "Anchor".
        assert!(
            body.contains("host for this harness. Quasar"),
            "symbolic_accounts comment must be Quasar-flavored; got:\n{body}"
        );

        // Must NOT leak the word "Anchor" anywhere — the framework label
        // threads through every shared-emitter comment.
        assert!(
            !body.contains("Anchor"),
            "Quasar harness must not leak the Anchor framework name; got:\n{body}"
        );
        assert!(
            !body.contains("struct AccountLayout") && !body.contains("build_token_account"),
            "Quasar harness must not emit the Pinocchio stack scaffold; got:\n{body}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Slice 8 M3: Pinocchio emits a stack-allocated `AccountInfo`
    /// harness. Validates the deterministic scaffold + per-handler
    /// proof shape that the M2 reference
    /// (crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer/src/kani_impl.rs)
    /// proved catches real overflow bugs.
    #[test]
    fn pinocchio_target_emits_stack_harness() {
        // SPL-transfer-shaped handler: two explicit token accounts
        // (source, destination), a readonly mint, a signer authority.
        let src = r#"spec PtokenTransfer
state { dummy : U64 }
handler transfer (amount : U64) {
  accounts {
    source : writable, token
    mint : readonly
    destination : writable, token
    authority : signer
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
        let spec = parse_str(src).expect("parse");

        let tmp =
            std::env::temp_dir().join(format!("kani_impl_pinocchio_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
            .expect("Pinocchio kani_impl must emit");
        assert!(tmp.is_file(), "Pinocchio target must write a harness file");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // Deterministic scaffold present.
        assert!(
            body.contains("struct AccountLayout"),
            "must emit the Account layout mirror; got:\n{body}"
        );
        assert!(
            body.contains("assert!(core::mem::size_of::<AccountLayout>() == 88)"),
            "must emit the layout size assertion; got:\n{body}"
        );
        assert!(
            body.contains("fn build_token_account")
                && body.contains("fn build_minimal_account")
                && body.contains("fn account_info_from_stack"),
            "must emit the build + transmute helpers; got:\n{body}"
        );

        // Per-handler proof.
        assert!(
            body.contains("#[kani::proof]") && body.contains("#[kani::unwind(34)]"),
            "must emit the proof attribute + memcmp unwind bound; got:\n{body}"
        );
        assert!(
            body.contains("fn verify_transfer_impl()"),
            "must emit the per-handler proof fn; got:\n{body}"
        );

        // Account classification: explicit token accounts -> token account;
        // signer/readonly → minimal.
        assert!(
            body.contains("let mut source = build_token_account(")
                && body.contains("let mut destination = build_token_account("),
            "explicit token accounts must build as token accounts; got:\n{body}"
        );
        assert!(
            body.contains("let mut mint = build_minimal_account(")
                && body.contains("let mut authority = build_minimal_account("),
            "readonly + signer accounts must build as minimal accounts; got:\n{body}"
        );

        // Param packing + real dispatcher call.
        assert!(
            body.contains("let amount: u64 = kani::any();")
                && body.contains("let instruction_tag: u8 = crate::TRANSFER;")
                && body.contains("instruction_data.push(instruction_tag);")
                && body.contains("instruction_data.extend_from_slice(&amount.to_le_bytes());"),
            "U64 param must be symbolic + tag/LE-packed; got:\n{body}"
        );
        assert!(
            body.contains(
                "crate::process_instruction(&program_id, accounts_slice, &instruction_data)"
            ),
            "must call the real process_instruction dispatcher; got:\n{body}"
        );

        // Must NOT leak the Anchor shape.
        assert!(
            !body.contains("Context<") && !body.contains("symbolic_accounts"),
            "Pinocchio harness must not leak the Anchor Context shape; got:\n{body}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn pinocchio_dispatcher_packs_numeric_params_in_spec_order() {
        let src = r#"spec Pool
state { lane_count : U64 }
handler batch_16
  (amount_0 : U64) (from_lane_id_0 : U64) (to_lane_id_0 : U64)
  (amount_1 : U64) (from_lane_id_1 : U64) (to_lane_id_1 : U64)
  (amount_2 : U64) (from_lane_id_2 : U64) (to_lane_id_2 : U64)
  (amount_3 : U64) (from_lane_id_3 : U64) (to_lane_id_3 : U64)
  (amount_4 : U64) (from_lane_id_4 : U64) (to_lane_id_4 : U64)
  (amount_5 : U64) (from_lane_id_5 : U64) (to_lane_id_5 : U64)
  (amount_6 : U64) (from_lane_id_6 : U64) (to_lane_id_6 : U64)
  (amount_7 : U64) (from_lane_id_7 : U64) (to_lane_id_7 : U64)
  (amount_8 : U64) (from_lane_id_8 : U64) (to_lane_id_8 : U64)
  (amount_9 : U64) (from_lane_id_9 : U64) (to_lane_id_9 : U64)
  (amount_10 : U64) (from_lane_id_10 : U64) (to_lane_id_10 : U64)
  (amount_11 : U64) (from_lane_id_11 : U64) (to_lane_id_11 : U64)
  (amount_12 : U64) (from_lane_id_12 : U64) (to_lane_id_12 : U64)
  (amount_13 : U64) (from_lane_id_13 : U64) (to_lane_id_13 : U64)
  (amount_14 : U64) (from_lane_id_14 : U64) (to_lane_id_14 : U64)
  (amount_15 : U64) (from_lane_id_15 : U64) (to_lane_id_15 : U64) {
  accounts {
    config : readonly
    inventory_rebalancer : signer
    token_program : readonly
    mint : readonly
    source_authority_0 : readonly
    source_inventory_0 : writable
    destination_inventory_0 : writable
  }
  ensures state.lane_count == old(state.lane_count)
  effect { lane_count := lane_count }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp =
            std::env::temp_dir().join(format!("kani_impl_batch_pack_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
            .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("let instruction_tag: u8 = crate::BATCH;")
                && body.contains("instruction_data.extend_from_slice(&amount_0.to_le_bytes());")
                && body.contains(
                    "instruction_data.extend_from_slice(&from_lane_id_15.to_le_bytes());"
                )
                && body
                    .contains("instruction_data.extend_from_slice(&to_lane_id_15.to_le_bytes());")
                && body.contains("instruction_data.extend_from_slice(&amount_15.to_le_bytes());"),
            "generic Pinocchio packing must use the base tag and declared numeric params; got:\n{body}"
        );
        assert!(
            !body.contains("instruction_data.push(16u8);")
                && !body.contains("from_lane_id_15 as u8")
                && !body.contains("to_lane_id_15 as u8")
                && !body.contains("crate::BATCH_16"),
            "runtime-specific arity bytes and narrowing casts require an ABI profile; got:\n{body}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn pinocchio_impl_packs_abi_repeated_records_from_indexed_params() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
limit MAX_ITEMS 4
instruction BATCH 4

record TRANSFER
field FROM_LANE_ID u8
field TO_LANE_ID u8
field AMOUNT u64
end

record BATCH_ARGS
field ITEM_COUNT u8
repeat ITEM transfer MAX_ITEMS ITEM_COUNT
end

instruction_record BATCH BATCH_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec Pool
state { lane_count : U64 }
handler batch_2
  (amount_0 : U64) (from_lane_id_0 : U64) (to_lane_id_0 : U64)
  (amount_1 : U64) (from_lane_id_1 : U64) (to_lane_id_1 : U64) {
  accounts {
    config : readonly
    source_0 : writable
    destination_0 : writable
  }
  ensures state.lane_count == old(state.lane_count)
  effect { lane_count := lane_count }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let instruction_tag: u8 = 4u8;")
                && body.contains("instruction_data[1] = 2u8;")
                && body.contains("instruction_data[2] = (from_lane_id_0 as u8) as u8;")
                && body.contains("instruction_data[3] = (to_lane_id_0 as u8) as u8;")
                && body.contains(
                    "let generated_instruction_data_4_bytes = (amount_0 as u64).to_le_bytes();"
                )
                && body.contains("instruction_data[12] = (from_lane_id_1 as u8) as u8;")
                && body.contains("instruction_data[13] = (to_lane_id_1 as u8) as u8;")
                && body.contains(
                    "let generated_instruction_data_14_bytes = (amount_1 as u64).to_le_bytes();"
                ),
            "ABI repeat profile must pack count and indexed item fields in ABI order; got:\n{body}"
        );
        assert!(
            !body.contains("source profile references param `item_count` absent"),
            "repeat count should be derived from indexed params, not treated as a missing param; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_emits_verified_stubs_for_contracted_source_helpers() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            "mod processor;\nmod validation;\n",
        )
        .unwrap();
        std::fs::write(
            program_root.join("src/validation.rs"),
            r#"
#[cfg_attr(kani, kani::requires(amount > 0))]
#[cfg_attr(kani, kani::ensures(|result| result.is_ok()))]
pub fn check_amount(amount: u64) -> Result<(), ()> {
    if amount == 0 { Err(()) } else { Ok(()) }
}
"#,
        )
        .unwrap();
        std::fs::write(
            program_root.join("src/processor.rs"),
            r#"
use crate::validation::check_amount;

pub fn process_transfer(_accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    check_amount(amount)?;
    Ok(())
}
"#,
        )
        .unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
instruction TRANSFER 1

record TRANSFER_ARGS
field AMOUNT u64
end

instruction_record TRANSFER TRANSFER_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec Pool
state { dummy : U64 }
handler transfer (amount : U64) {
  accounts { payer : signer }
  requires amount > 0 else InvalidAmount
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("#[kani::stub_verified(crate::validation::check_amount)]")
                && body.contains("fn verify_transfer_impl()")
                && body.contains("crate::process_instruction(&program_id"),
            "contracted source helper calls should emit verified stubs on the real-dispatcher harness; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_packs_abi_repeated_pubkey_fields_from_indexed_params() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
limit MAX_ITEMS 4
instruction BATCH 4

record TRANSFER
field MINT pubkey
field AMOUNT u64
end

record BATCH_ARGS
field ITEM_COUNT u8
repeat ITEM transfer MAX_ITEMS ITEM_COUNT
end

instruction_record BATCH BATCH_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec PubkeyBatch
state { total : U64 }
handler batch_2
  (mint_0 : Pubkey) (amount_0 : U64)
  (mint_1 : Pubkey) (amount_1 : U64) {
  accounts { config : readonly }
  ensures state.total == old(state.total)
  effect { total := total }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let mint_0: [u8; 32] = kani::any(); // spec type: Pubkey")
                && body.contains("let mint_1: [u8; 32] = kani::any(); // spec type: Pubkey"),
            "indexed Pubkey repeat fields must be declared as symbolic 32-byte arrays; got:\n{body}"
        );
        assert!(
            body.contains("instruction_data[1] = 2u8;")
                && body.contains("write_fixed_32(&mut instruction_data, 2, mint_0);")
                && body.contains(
                    "let generated_instruction_data_34_bytes = (amount_0 as u64).to_le_bytes();"
                )
                && body.contains("write_fixed_32(&mut instruction_data, 42, mint_1);")
                && body.contains(
                    "let generated_instruction_data_74_bytes = (amount_1 as u64).to_le_bytes();"
                ),
            "ABI repeat profile must pack indexed Pubkey fields in ABI order; got:\n{body}"
        );
        assert!(
            !body.contains("TODO: pack repeat field `mint`"),
            "Pubkey repeat fields should no longer be dropped from the ABI profile; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_emits_token_transfer_balance_assertions() {
        let src = r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    source : writable
    destination : writable
    authority : signer
  }
  call Token.transfer(
    from = source,
    to = destination,
    amount = amount,
    authority = authority,
  )
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp = std::env::temp_dir().join(format!(
            "kani_impl_token_assertions_{}.rs",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
            .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("let pre_transfer_0_from = read_token_amount(&source);")
                && body.contains("let pre_transfer_0_to = read_token_amount(&destination);")
                && body.contains("kani::assume(pre_transfer_0_from >= (amount as u64));")
                && body.contains("kani::assume(pre_transfer_0_to <= u64::MAX - (amount as u64));"),
            "must snapshot and constrain Token.transfer balances; got:\n{body}"
        );
        assert!(
            body.contains("if _result.is_ok() {")
                && body.contains(
                    "assert_eq!(read_token_amount(&source), pre_transfer_0_from - (amount as u64));"
                )
                && body.contains(
                    "assert_eq!(read_token_amount(&destination), pre_transfer_0_to + (amount as u64));"
                ),
            "must assert Token.transfer balance deltas on success; got:\n{body}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn pinocchio_impl_does_not_classify_all_writable_accounts_as_tokens() {
        let src = r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    config : writable
    source : writable, token
    destination : writable, token
    authority : signer
    token_program : program, type token
  }
  call Token.transfer(
    from = source,
    to = destination,
    amount = amount,
    authority = authority,
  )
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp =
            std::env::temp_dir().join(format!("kani_impl_token_roles_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
            .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            body.contains("let mut config = build_minimal_account(")
                && body.contains("let mut source = build_token_account(")
                && body.contains("let mut destination = build_token_account(")
                && body.contains("let mut token_program = build_minimal_account(")
                && !body.contains("let config_amount: u64 = kani::any();"),
            "only explicit token accounts or Token.transfer resources should use token layout; got:\n{body}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn pinocchio_impl_uses_abi_account_roles_for_token_projection() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
instruction MOVE_TOKENS 8
account MOVE_TOKENS SOURCE 0 writable type token
account MOVE_TOKENS DESTINATION 1 writable type token
account MOVE_TOKENS MINT 2 type mint
account MOVE_TOKENS TOKEN_PROGRAM 3 program type token

record MOVE_TOKENS_ARGS
field AMOUNT u64
end

instruction_record MOVE_TOKENS MOVE_TOKENS_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    source : readonly
    destination : readonly
    mint : readonly
    token_program : program
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let mut source = build_token_account([1u8; 32], true, false")
                && body
                    .contains("let mut destination = build_token_account([2u8; 32], true, false")
                && body.contains("let mut mint = build_mint_account([3u8; 32], false, false, 6u8);")
                && body.contains("let mut token_program = build_minimal_account(SPL_TOKEN_PROGRAM_ID, false, false)")
                && !body.contains("let token_program_amount: u64 = kani::any();"),
            "ABI account roles should project token accounts and mints without treating token_program as token data; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_projects_source_inferred_token_account_mint_and_owner() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
pub fn process_instruction(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        8 => process_move_tokens(accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_move_tokens(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let source = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let authority = next_account_info(account_info_iter)?;
    require_token_account(source, mint.key(), authority.key())?;
    let decimals = read_mint_decimals(mint)?;
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec TokenProjection
state { dummy : U64 }
handler move_tokens (amount : U64) {
  accounts {
    source : writable
    mint : readonly
    authority : signer
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let source_amount: u64 = kani::any();")
                && body.contains("let mut source = build_token_account([1u8; 32], true, false, [2u8; 32], authority_key, (source_amount as u64));")
                && body.contains("let mut mint = build_mint_account([2u8; 32], false, false, 6u8);"),
            "source-inferred token account bindings should project mint and owner bytes; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_projects_repeated_token_binding_from_key_alias() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(program_root.join("schema")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
pub fn derive_authority(program_id: &pinocchio::pubkey::Pubkey, lane_id: u8) -> ([u8; 32], u8) {
    pinocchio::pubkey::try_find_program_address(&[AUTHORITY_SEED, &[lane_id]], program_id).unwrap()
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        9 => process_move_tokens(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_move_tokens(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let source = next_account_info(account_info_iter)?;
    let destination = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let source_authority = next_account_info(account_info_iter)?;
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(8..9)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let source_authority_key = derive_authority(program_id, 0).0;
    let destination_authority_key = derive_authority(program_id, lane_id).0;
    require_key(source_authority, &source_authority_key)?;
    require_token_account(source, mint.key(), &source_authority_key)?;
    require_token_account(destination, mint.key(), &destination_authority_key)?;
    let decimals = read_mint_decimals(mint)?;
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
        )
        .unwrap();
        std::fs::write(
            program_root.join("schema/program.schema"),
            "seed AUTHORITY_SEED authority\n",
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec TokenProjection
state { dummy : U64 }
handler move_tokens (amount : U64) (lane_id : U64) {
  accounts {
    source_0 : writable
    destination_0 : writable
    mint : readonly
    source_authority_0 : signer
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let source_authority_0_key = crate::derive_authority(&program_id, lane_id as u8).0;")
                && body.contains("let source_0_amount: u64 = kani::any();")
                && body.contains("let mut source_0 = build_token_account([1u8; 32], true, false, [3u8; 32], source_authority_0_key, (source_0_amount as u64));"),
            "repeated token account should inherit source loop binding and owner key alias; got:\n{body}"
        );
        assert!(
            body.contains("let destination_0_owner_key = crate::derive_authority(&program_id, lane_id as u8).0;")
                && body.contains("let destination_0_amount: u64 = kani::any();")
                && body.contains("let mut destination_0 = build_token_account([2u8; 32], true, false, [3u8; 32], destination_0_owner_key, (destination_0_amount as u64));"),
            "repeated token account should project owner bytes from a source-derived key; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_uses_abi_account_layout_for_symbolic_data_account() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
instruction UPDATE_CONFIG 9
account UPDATE_CONFIG CONFIG 0 writable

record CONFIG_ACCOUNT
field MAGIC bytes8
field ADMIN pubkey
field MAX_FEE_BPS u16
field PAUSED bool
end

record UPDATE_CONFIG_ARGS
field MAX_FEE_BPS u16
end

magic CONFIG_MAGIC CFGMAGIC
instruction_record UPDATE_CONFIG UPDATE_CONFIG_ARGS
account_record CONFIG CONFIG_ACCOUNT
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec ConfigProgram
state { max_fee_bps : U64 }
handler update_config (max_fee_bps : U64) {
  accounts {
    config : readonly
  }
  ensures state.max_fee_bps == old(state.max_fee_bps)
  effect { max_fee_bps := max_fee_bps }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("fn build_data_account")
                && body.contains("// ABI account layout `config_account`: 43 byte data region.")
                && body.contains("let mut config_data: [u8; 43] = [0u8; 43];")
                && body.contains("config_data[0] = 67u8;")
                && body.contains("config_data[7] = 67u8;")
                && body.contains("let mut config = build_data_account([1u8; 32], program_id, false, true, config_data);")
                && body.contains("write_state_u16(&mut config, 40, (max_fee_bps as u16));")
                && !body.contains("let mut config = build_minimal_account("),
            "ABI account layouts should emit program-owned data accounts with profiled byte length and state witnesses; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_binds_profiled_pda_account_keys() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(
            program_root.join("src/state.rs"),
            r#"
pub fn derive_config(program_id: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[CONFIG_SEED], program_id)
}

pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[VAULT_AUTHORITY_SEED, &[lane_id]], program_id)
}
"#,
        )
        .unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
seed CONFIG_SEED config
seed VAULT_AUTHORITY_SEED vault-authority
instruction ROUTE 3
account ROUTE CONFIG 0 writable
account ROUTE VAULT_AUTHORITY 1

record ROUTE_ARGS
field LANE_ID u8
end

instruction_record ROUTE ROUTE_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec RouteProgram
state { dummy : U64 }
handler route (lane_id : U64) {
  accounts {
    config : writable
    vault_authority : readonly
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let config_key = crate::derive_config(&program_id).0;")
                && body.contains("let vault_authority_key = crate::derive_vault_authority(&program_id, lane_id as u8).0;")
                && body.contains(
                    "let mut config = build_minimal_account(config_key, false, true);"
                )
                && body.contains(
                    "let mut vault_authority = build_minimal_account(vault_authority_key, false, false);"
                ),
            "profiled PDA derivations should bind exact account keys generically; got:\n{body}"
        );
        assert!(
            body.contains(
                "/// - PDA derivations: config -> config (found); vault_authority -> vault_authority (found)"
            ),
            "generated impl harness should report inferred PDA derivations; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_binds_account_keys_from_source_require_key_derivation() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(program_root.join("schema")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::try_find_program_address(&[VAULT_AUTHORITY_SEED, &[lane_id]], program_id).unwrap()
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_route(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let vault = next_account_info(account_info_iter)?;
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let vault_key = derive_vault_authority(program_id, lane_id).0;
    require_key(vault, &vault_key)?;
    Ok(())
}
"#,
        )
        .unwrap();
        std::fs::write(
            program_root.join("schema/program.schema"),
            "seed VAULT_AUTHORITY_SEED vault-authority\n",
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec RouteProgram
state { dummy : U64 }
handler route (lane_id : U64) {
  accounts {
    vault : readonly
  }
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains(
                "let vault_key = crate::derive_vault_authority(&program_id, lane_id as u8).0;"
            ) && body.contains("let mut vault = build_minimal_account(vault_key, false, false);"),
            "source require_key derived-key guards should bind exact account keys; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_binds_non_program_id_pda_from_source_require_key_derivation() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
use pinocchio::{account_info::AccountInfo, pubkey::Pubkey, ProgramResult};

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];
pub const TOKEN_PROGRAM_ID: Pubkey = [9u8; 32];

pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Ok(()),
    }
}

fn process_route(_program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], _instruction_data: &[u8]) -> ProgramResult {
    let [authority, mint, vault, ..] = accounts else {
        return Ok(());
    };
    require_key(vault, &derive_token_vault(authority.key(), mint.key()).0)?;
    Ok(())
}
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec NonProgramPda
state { balance : U64 }
handler route (nonce : U64) {
  accounts {
    authority : readonly
    mint      : readonly
    vault     : writable
  }
  ensures state.balance == old(state.balance)
  effect { balance := balance }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let vault_key = crate::derive_token_vault(&[1u8; 32], &[2u8; 32]).0;")
                && body.contains("let mut vault = build_minimal_account(vault_key, false, true);"),
            "non-program-id PDA account keys should render from source require_key derivations; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_binds_non_program_id_pda_with_nested_derived_key_seed() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(program_root.join("schema")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
use pinocchio::{account_info::AccountInfo, pubkey::Pubkey, ProgramResult};

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];

pub fn derive_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[AUTHORITY_SEED, &[lane_id]], program_id)
}

pub fn derive_token_vault(program_id: &Pubkey, mint: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    let authority = derive_authority(program_id, lane_id).0;
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), crate::TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Ok(()),
    }
}

fn process_route(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [mint, vault, ..] = accounts else {
        return Ok(());
    };
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let descriptor = VaultDescriptor {
        lane_id,
        mint: MintKey(*mint.key()),
    };
    require_key(
        vault,
        &derive_token_vault(program_id, &descriptor.mint.0, descriptor.lane_id.0).0,
    )?;
    Ok(())
}
"#,
        )
        .unwrap();
        std::fs::write(
            program_root.join("schema/program.schema"),
            "seed AUTHORITY_SEED authority\n",
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec NestedPda
state { balance : U64 }
handler route (lane_id : U64) {
  accounts {
    mint  : readonly
    vault : writable
  }
  ensures state.balance == old(state.balance)
  effect { balance := balance }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let vault_authority_key = crate::derive_authority(&program_id, lane_id as u8).0;")
                && body.contains("let vault_key = crate::derive_token_vault(&program_id, &[1u8; 32], lane_id as u8).0;")
                && body.contains("let mut vault = build_minimal_account(vault_key, false, true);"),
            "nested derived-key PDA seeds should render before the outer non-program-id PDA; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_binds_repeated_loop_account_derivations_from_source() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(program_root.join("schema")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
use pinocchio::{account_info::AccountInfo, pubkey::Pubkey, ProgramResult};

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];
pub const TOKEN_PROGRAM_ID: Pubkey = [9u8; 32];

pub fn derive_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[AUTHORITY_SEED, &[lane_id]], program_id)
}

pub fn derive_token_vault(program_id: &Pubkey, mint: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    let authority = derive_authority(program_id, lane_id).0;
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), crate::TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data.split_first().unwrap();
    match *tag {
        3 => process_route(program_id, accounts, data),
        _ => Ok(()),
    }
}

fn process_route(program_id: &pinocchio::pubkey::Pubkey, accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let args = RouteArgs::try_from(instruction_data)?;
    let account_info_iter = &mut accounts.iter();
    let mint = next_account_info(account_info_iter)?;
    let route_mint = MintKey(*mint.key());
    for transfer in args.transfers {
        let source_vault = next_account_info(account_info_iter)?;
        let destination_vault = next_account_info(account_info_iter)?;
        require_key(
            source_vault,
            &derive_token_vault(program_id, &route_mint.0, transfer.from_lane_id.0).0,
        )?;
        require_key(
            destination_vault,
            &derive_token_vault(program_id, &route_mint.0, transfer.to_lane_id.0).0,
        )?;
    }
    Ok(())
}
"#,
        )
        .unwrap();
        std::fs::write(
            program_root.join("schema/program.schema"),
            r#"seed AUTHORITY_SEED authority
field FROM_LANE_ID u8
field TO_LANE_ID u8
record TRANSFER
field FROM_LANE_ID u8
field TO_LANE_ID u8
record ROUTE_ARGS
field TRANSFER_COUNT u8
repeat TRANSFER transfer 2 TRANSFER_COUNT
instruction ROUTE 3
instruction_record ROUTE ROUTE_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec RepeatedLoopPda
state { balance : U64 }
handler route_2
  (from_lane_id_0 : U64)
  (to_lane_id_0 : U64)
  (from_lane_id_1 : U64)
  (to_lane_id_1 : U64) {
  accounts {
    mint                  : readonly
    source_vault_0        : writable
    destination_vault_0   : writable
    source_vault_1        : writable
    destination_vault_1   : writable
  }
  ensures state.balance == old(state.balance)
  effect { balance := balance }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let source_vault_0_authority_key = crate::derive_authority(&program_id, from_lane_id_0 as u8).0;")
                && body.contains("let source_vault_0_key = crate::derive_token_vault(&program_id, &[1u8; 32], from_lane_id_0 as u8).0;")
                && body.contains("let destination_vault_1_authority_key = crate::derive_authority(&program_id, to_lane_id_1 as u8).0;")
                && body.contains("let destination_vault_1_key = crate::derive_token_vault(&program_id, &[1u8; 32], to_lane_id_1 as u8).0;")
                && body.contains("let mut source_vault_0 = build_minimal_account(source_vault_0_key, false, true);")
                && body.contains("let mut destination_vault_1 = build_minimal_account(destination_vault_1_key, false, true);"),
            "repeated loop account-key derivations should bind suffixed accounts from source; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_uses_source_profile_for_tag_accounts_and_payload_widths() {
        let src = r#"spec TokenMove
state { dummy : U64 }
handler move_tokens (amount : U64) (lane : U64) {
  accounts {
    source : writable
    destination : writable
    authority : signer
  }
  call Token.transfer(
    from = source,
    to = destination,
    amount = amount,
    authority = authority,
  )
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
        let spec = parse_str(src).expect("parse");
        let dir = tempfile::tempdir().expect("tempdir");
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(src_dir.join("instructions")).unwrap();
        std::fs::write(
            src_dir.join("lib.rs"),
            r#"
pub fn process_instruction(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminant, data) = instruction_data.split_first().unwrap();
    match *discriminant {
        9 => instructions::move_tokens::process_move_tokens(accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
"#,
        )
        .unwrap();
        std::fs::write(
            src_dir.join("instructions/move_tokens.rs"),
            r#"
pub fn process_move_tokens(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [destination, authority, source, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let lane = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let amount = u64::from_le_bytes(
        instruction_data
            .get(1..9)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
        )
        .unwrap();

        let output = src_dir.join("kani_impl.rs");
        generate_from_spec(
            &spec,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let instruction_tag: u8 = 9u8;"),
            "must use source-inferred dispatcher tag; got:\n{body}"
        );
        assert!(
            body.contains("/// - source account order: destination, authority, source")
                && body.contains("/// - ABI/dispatcher tag: 9")
                && body.contains("/// - PDA derivations: none inferred"),
            "generated impl harness should explain profile facts and fallbacks; got:\n{body}"
        );
        let destination_pos = body
            .find("ManuallyDrop::new(account_info_from_stack(&mut destination))")
            .unwrap();
        let authority_pos = body
            .find("ManuallyDrop::new(account_info_from_stack(&mut authority))")
            .unwrap();
        let source_pos = body
            .find("ManuallyDrop::new(account_info_from_stack(&mut source))")
            .unwrap();
        assert!(
            destination_pos < authority_pos && authority_pos < source_pos,
            "must use source-inferred account order; got:\n{body}"
        );
        assert!(
            body.contains("instruction_data[1] = (lane as u8) as u8;")
                && body.contains(
                    "let generated_instruction_data_2_bytes = (amount as u64).to_le_bytes();"
                ),
            "must use source-inferred payload order and widths; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_keeps_non_trailing_unsupported_abi_fields_visible() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        let abi_root = workspace.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
instruction UPLOAD 12

record UPLOAD_ARGS
field MEMO bytes8
field AMOUNT u64
end

instruction_record UPLOAD UPLOAD_ARGS
"#,
        )
        .unwrap();

        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(
            &spec_path,
            r#"spec Upload
state { total : U64 }
handler upload (amount : U64) {
  accounts { payer : signer }
  ensures state.total == old(state.total)
  effect { total := total }
}"#,
        )
        .unwrap();

        let output = program_root.join("src/kani_impl.rs");
        generate(
            &spec_path,
            &output,
            /*explicit_flag=*/ true,
            Target::Pinocchio,
        )
        .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&output).unwrap();

        assert!(
            body.contains("let mut instruction_data = [0u8; 17];")
                && body.contains(
                    "TODO: unsupported instruction field `memo` type `bytes8` at offset 1..9"
                )
                && body.contains(
                    "let generated_instruction_data_9_bytes = (amount as u64).to_le_bytes();"
                ),
            "non-trailing unsupported ABI fields must keep absolute layout visible and pack later fields at the right offset; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_token_delta_assertions_skip_aliasing_transfers() {
        let chained = vec![
            PinocchioTokenTransferAssertion {
                from: "account_a".to_string(),
                to: "account_b".to_string(),
                amount: "amount_0".to_string(),
            },
            PinocchioTokenTransferAssertion {
                from: "account_b".to_string(),
                to: "account_c".to_string(),
                amount: "amount_1".to_string(),
            },
        ];
        let mut body = String::new();
        emit_pinocchio_token_pre_snapshots(&mut body, &chained);
        emit_pinocchio_token_post_assertions(&mut body, &chained);
        assert!(
            body.contains("token transfer delta assertions skipped")
                && !body.contains("assert_eq!(read_token_amount"),
            "chained transfers should not emit independent per-transfer final assertions; got:\n{body}"
        );

        let self_transfer = vec![PinocchioTokenTransferAssertion {
            from: "account_a".to_string(),
            to: "account_a".to_string(),
            amount: "amount".to_string(),
        }];
        let mut body = String::new();
        emit_pinocchio_token_pre_snapshots(&mut body, &self_transfer);
        emit_pinocchio_token_post_assertions(&mut body, &self_transfer);
        assert!(
            body.contains("token transfer delta assertions skipped")
                && !body.contains("assert_eq!(read_token_amount"),
            "self-transfer aliases should not emit independent debit/credit assertions; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_account_order_matches_normalized_names() {
        let src = r#"spec AccountOrder
state { total : U64 }
handler route {
  accounts {
    user_vault : signer
    token_program : program
    output_mint : readonly
  }
  ensures state.total == old(state.total)
  effect { total := total }
}"#;
        let spec = parse_str(src).expect("parse");
        let handler = &spec.handlers[0];
        let profile = PinocchioHandlerProfile {
            name: "route".to_string(),
            instruction_tag: None,
            accounts: vec![
                "outputMint".to_string(),
                "tokenProgram".to_string(),
                "userVault".to_string(),
            ],
            account_roles: BTreeMap::new(),
            token_account_bindings: BTreeMap::new(),
            mint_decimal_bindings: BTreeMap::new(),
            account_key_derivations: BTreeMap::new(),
            source_expr_aliases: BTreeMap::new(),
            verified_stubs: Vec::new(),
            params: Vec::new(),
            repeats: Vec::new(),
        };

        let ordered = pinocchio_account_order(handler, Some(&profile));
        let names: Vec<_> = ordered
            .iter()
            .map(|account| account.name.as_str())
            .collect();
        assert_eq!(names, ["output_mint", "token_program", "user_vault"]);
    }

    #[test]
    fn pinocchio_profile_notes_explain_unusable_account_order() {
        let src = r#"spec AccountOrder
state { total : U64 }
handler route {
  accounts {
    user_vault : signer
    token_program : program
  }
  ensures state.total == old(state.total)
  effect { total := total }
}"#;
        let spec = parse_str(src).expect("parse");
        let handler = &spec.handlers[0];
        let profile = PinocchioHandlerProfile {
            name: "route".to_string(),
            instruction_tag: None,
            accounts: vec!["userVault".to_string(), "tokenProgramExtra".to_string()],
            account_roles: BTreeMap::new(),
            token_account_bindings: BTreeMap::new(),
            mint_decimal_bindings: BTreeMap::new(),
            account_key_derivations: BTreeMap::new(),
            source_expr_aliases: BTreeMap::new(),
            verified_stubs: Vec::new(),
            params: Vec::new(),
            repeats: Vec::new(),
        };

        let mut notes = String::new();
        emit_pinocchio_profile_notes(&mut notes, handler, None, Some(&profile));
        assert!(
            notes.contains(
                "source account order: inferred order unusable; profile account `tokenProgramExtra` did not match spec accounts; using spec order"
            ),
            "unusable inferred order should leave a generated breadcrumb; got:\n{notes}"
        );
    }

    #[test]
    fn pinocchio_fee_normalization_equal_literal_decimals_uses_checked_threshold() {
        let src = r#"spec FeeSwap
state { max_fee_bps : U128 }
handler swap
  (amount_in : U64)
  (amount_out : U64)
  (max_fee_bps : U128)
  (input_decimals : U64)
  (output_decimals : U64)
  (fee_input_normalized : U128)
  (fee_output_normalized : U128) {
  accounts {
    input_mint  : readonly
    output_mint : readonly
  }
  requires amount_in > 0 else InvalidAmount
  requires amount_out > 0 else InvalidAmount
  requires max_fee_bps <= 10000 else InvalidFee
  requires input_decimals == 6 else InvalidMint
  requires output_decimals == 6 else InvalidMint
  requires fee_input_normalized == amount_in * 1000000000000 else InvalidAmount
  requires fee_output_normalized == amount_out * 1000000000000 else InvalidAmount
  ensures state.max_fee_bps == old(state.max_fee_bps)
}"#;
        let spec = parse_str(src).expect("parse");
        let handler = &spec.handlers[0];
        let mut mint_decimal_bindings = BTreeMap::new();
        mint_decimal_bindings.insert("input_mint".to_string(), "input_decimals".to_string());
        mint_decimal_bindings.insert("output_mint".to_string(), "output_decimals".to_string());
        let profile = PinocchioHandlerProfile {
            name: "swap".to_string(),
            instruction_tag: None,
            accounts: Vec::new(),
            account_roles: BTreeMap::new(),
            token_account_bindings: BTreeMap::new(),
            mint_decimal_bindings,
            account_key_derivations: BTreeMap::new(),
            source_expr_aliases: BTreeMap::new(),
            verified_stubs: Vec::new(),
            params: Vec::new(),
            repeats: Vec::new(),
        };

        let mut body = String::new();
        emit_pinocchio_fee_normalization_assumptions(&mut body, handler, Some(&profile));

        assert!(
            body.contains(
                "let generated_fee_min_output = ((amount_in as u128) * generated_fee_retained_bps) / 10000u128;"
            ) && body.contains(
                "kani::assume((amount_out as u128) >= generated_fee_min_output);"
            ),
            "equal literal decimals should cancel the shared normalization scale and emit the bounded fee floor; got:\n{body}"
        );
        assert!(
            !body.contains("generated_fee_input_normalized"),
            "equal literal decimals must not emit normalization-scale multiplication; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_emits_effect_only_state_harnesses_without_project_specifics() {
        let src = r#"spec ProjectSpecificConfig
state { max_fee_bps : U128 }
handler update_limit (new_max_fee_bps : U128) {
  accounts {
    config : writable, pda ["config"]
    admin  : signer
  }
  modifies [max_fee_bps]
  effect { max_fee_bps := new_max_fee_bps }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp = std::env::temp_dir().join(format!(
            "kani_impl_project_config_{}.rs",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
            .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("fn verify_update_limit_impl")
                && body.contains("kani::cover!(_result.is_ok()")
                && body.contains("let program_id: [u8; 32] = [42u8; 32];")
                && body.contains("crate::process_instruction(&program_id"),
            "effect-only handlers should emit generic state assertions without project-specific branches; got:\n{body}"
        );
    }

    #[test]
    fn pinocchio_impl_declares_and_packs_pubkey_params() {
        let src = r#"spec PubkeyParam
state { dummy : U64 }
handler register (member : Pubkey) {
  accounts { config : writable }
  modifies [dummy]
  ensures state.dummy == old(state.dummy)
  effect { dummy := dummy }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp = std::env::temp_dir().join(format!("kani_impl_pubkey_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Pinocchio)
            .expect("Pinocchio kani_impl must emit");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            body.contains("let member: [u8; 32] = kani::any(); // spec type: Pubkey"),
            "Pubkey params must be declared as symbolic 32-byte arrays; got:\n{body}"
        );
        assert!(
            body.contains("instruction_data.extend_from_slice(&member);"),
            "Pubkey params must pack raw 32-byte values into instruction data; got:\n{body}"
        );
        assert!(
            !body.contains("TODO: declare symbolic param `member`")
                && !body.contains("TODO: pack param `member`"),
            "Pubkey params should no longer fall through to TODOs; got:\n{body}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// `--kani-impl` flag explicitly forces emission for every handler
    /// with ensures, regardless of the modifies-diff.
    #[test]
    fn explicit_flag_forces_emission_for_handlers_with_ensures() {
        let src = r#"spec ExplicitFlag
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
        let spec = parse_str(src).expect("parse");
        // Auto-trigger silent (no modifies declared).
        assert!(!spec_triggers_impl_harness(&spec));

        let tmp =
            std::env::temp_dir().join(format!("kani_impl_explicit_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Anchor).expect("generate");
        assert!(tmp.is_file(), "explicit flag must emit the file");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("fn verify_bump_impl_ensures_0()"),
            "explicit flag must emit per-handler harness; got:\n{}",
            body
        );
        assert!(
            body.contains("accounts.handler(delta)"),
            "harness must call the user's real handler; got:\n{}",
            body
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// PDA-derived account addresses bind to the spec-declared seeds
    /// rather than `kani::any()`.
    #[test]
    fn pda_derived_accounts_bind_seed_expressions() {
        let src = r#"spec EscrowLite
state { initializer : Pubkey, amount : U64 }
pda escrow ["escrow", initializer]
handler open (deposit_amount : U64) {
  accounts {
    initializer : signer, writable
    escrow      : writable, pda ["escrow", initializer]
  }
  modifies [amount, initializer]
  ensures state.amount == deposit_amount
  effect { amount := deposit_amount }
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_pda_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("find_program_address(&[b\"escrow\", initializer.as_ref()]"),
            "PDA derivation must come from the spec's `pda` declaration; got:\n{}",
            body
        );
        assert!(
            body.contains("`initializer`: signer"),
            "signer account must appear in the symbolic builder; got:\n{}",
            body
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// Issue #71: an integer handler param used as a PDA seed must
    /// serialize via `to_le_bytes()` — `u64::as_ref()` does not exist.
    /// Pubkey seeds / account keys keep `.as_ref()`.
    #[test]
    fn integer_param_seed_serializes_via_to_le_bytes() {
        let src = r#"spec Pool
state { lane_count : U64 }
pda vault_authority ["vault-authority", lane_id]
handler swap (lane_id : U64) {
  accounts {
    vault_authority : writable, pda ["vault-authority", lane_id]
    caller        : signer
  }
  modifies [lane_count]
  ensures state.lane_count == lane_id
  effect { lane_count := lane_id }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp = std::env::temp_dir().join(format!("kani_impl_intseed_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true, Target::Anchor).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("lane_id.to_le_bytes().as_ref()"),
            "integer-param seed must serialize via to_le_bytes; got:\n{}",
            body
        );
        assert!(
            !body.contains("[b\"vault-authority\", lane_id.as_ref()"),
            "must not emit bare `lane_id.as_ref()` for a u64 param; got:\n{}",
            body
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// No emit when neither the explicit flag is on NOR any handler
    /// triggers auto-emission.
    #[test]
    fn no_emit_when_neither_flag_nor_auto_trigger() {
        let src = r#"spec Silent
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp = std::env::temp_dir().join(format!("kani_impl_silent_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        assert!(
            !tmp.is_file(),
            "no flag + no auto-trigger must skip file emission"
        );
    }

    // ========================================================================
    // v2.26 Batch 2 Track I — CPI ensures-as-fact in impl-targeted harness
    // ========================================================================

    /// A handler with its own `ensures` AND a `call Iface.foo(args)` to an
    /// interface that declares ensures must emit `kani::assume(...)` lines
    /// between `if result.is_ok()` and the first caller `assert!`,
    /// substituting the callee's param names with the caller's call-site
    /// expressions. Mirror of `kani.rs`'s
    /// `cpi_ensures_lowers_to_kani_assume_in_preservation_harness` for the
    /// impl-targeted variant.
    #[test]
    fn cpi_ensures_as_assume_emits_at_splice_point() {
        let src = r#"spec CpiImplTest
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures amount > 0
  }
}

state { pool : U64 }

handler deposit (amt : U64) {
  permissionless
  requires amt > 0 else InvalidAmount
  modifies [pool, lp_supply]
  call Token.transfer(from = 0, to = 0, amount = amt, authority = 0)
  effect { pool += amt }
  ensures state.pool == old(state.pool) + amt
}"#;
        let spec = parse_str(src).expect("parse");
        // The LP-shape diff (modifies = {pool, lp_supply}, effect-LHS = {pool})
        // triggers auto-emission.
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_track_i_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // 1. The splice-marker comment from Track H must be GONE — Track I
        //    replaces it with the actual emission, no stale marker.
        assert!(
            !body.contains("<Track I CPI ensures-as-fact splice point>"),
            "Track H's splice marker must be removed once Track I has emitted; got:\n{}",
            body
        );

        // 2. The CPI ensures-as-fact comment + assume line must be present,
        //    with `amount` substituted to the caller's `amt` expression.
        assert!(
            body.contains("// CPI ensures-as-fact (Token.transfer):"),
            "missing CPI ensures-as-fact comment for Token.transfer; got:\n{}",
            body
        );
        assert!(
            body.contains("kani::assume(amt > 0)"),
            "missing substituted kani::assume(amt > 0); got:\n{}",
            body
        );

        // 3. Ordering: assume must sit between `if result.is_ok()` and the
        //    caller's first `assert!`.
        let is_ok_pos = body
            .find("if result.is_ok()")
            .expect("harness must have `if result.is_ok()`");
        let assume_pos = body
            .find("kani::assume(amt > 0)")
            .expect("assume present (just asserted above)");
        let assert_pos = body[is_ok_pos..]
            .find("assert!")
            .map(|i| is_ok_pos + i)
            .expect("caller's assert! must follow");
        assert!(
            is_ok_pos < assume_pos && assume_pos < assert_pos,
            "CPI assume must sit between is_ok() and assert!; got:\n{}",
            body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.26 Track K — impl-targeted variant of the spec-model
    /// `named_return_binder_substitutes_into_kani_assume` test.
    /// `let p = call Oracle.quote(…)` with `-> price : U64` declared
    /// must rewrite `price` to `p` in the emitted `kani::assume`.
    #[test]
    fn named_return_binder_substitutes_in_impl_harness() {
        let src = r#"spec NamedBinderImpl
program_id "11111111111111111111111111111111"

interface Oracle {
  program_id "11111111111111111111111111111111"
  handler quote (base : U64) -> price : U64 {
    ensures price > 0
  }
}

state { last_price : U64, lp_supply : U64 }

handler refresh (b : U64) {
  permissionless
  modifies [last_price, lp_supply]
  let p = call Oracle.quote(base = b)
  effect { last_price := b }
  ensures state.last_price == b
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_track_k_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            body.contains("// CPI ensures-as-fact (Oracle.quote):"),
            "missing CPI ensures-as-fact comment for Oracle.quote; got:\n{}",
            body,
        );
        // The callee uses `price` as its return binder; the caller's
        // `let p = …` makes `p` the substituted form.
        assert!(
            body.contains("kani::assume(p > 0)"),
            "expected `kani::assume(p > 0)` from named binder substitution; got:\n{}",
            body,
        );
        assert!(
            !body.contains("price > 0"),
            "binder name `price` must be substituted away; got:\n{}",
            body,
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.27 Track A — `state_binders { ... }` rewrites
    /// `pre.<callee_field>` / `post.<callee_field>` to the caller's
    /// `pre.<caller_field>` / `post.<caller_field>` in the substituted
    /// `kani::assume`, which then flattens through
    /// `rewrite_pre_post_paths` to the harness-local
    /// `pre_<caller_field>` / `post_<caller_field>` snapshots.
    #[test]
    fn state_binders_rewrite_through_impl_snapshot_locals() {
        let src = r#"spec StateBindersImpl
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures post.from_balance + amount == pre.from_balance
  }
}

state { pool_balance : U64, lp_supply : U64 }

handler deposit (amt : U64) {
  permissionless
  requires amt > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  call Token.transfer(
    from = 0,
    to = 0,
    amount = amt,
    authority = 0,
    state_binders { from_balance = state.pool_balance },
  )
  effect { pool_balance -=! amt }
  ensures state.pool_balance == old(state.pool_balance) - amt
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_track_a_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // The substitution rewrites `pre.from_balance` /
        // `post.from_balance` → `pre.pool_balance` / `post.pool_balance`
        // (via state_binders), then `rewrite_pre_post_paths` flattens
        // those to the snapshot locals `pre_pool_balance` /
        // `post_pool_balance`.
        assert!(
            body.contains("kani::assume(post_pool_balance + amt == pre_pool_balance)"),
            "expected flat snapshot locals in kani::assume; got:\n{}",
            body,
        );
        // The callee abstract field name must NOT survive.
        assert!(
            !body.contains("from_balance"),
            "abstract callee field `from_balance` must be substituted; got:\n{}",
            body,
        );
        // The snapshot block must capture `pool_balance` (the caller-
        // side binder field) — otherwise `pre_pool_balance` /
        // `post_pool_balance` references the assume emits don't compile.
        assert!(
            body.contains("let pre_pool_balance"),
            "snapshot block must capture pre_pool_balance; got:\n{}",
            body,
        );
        assert!(
            body.contains("let post_pool_balance"),
            "snapshot block must capture post_pool_balance; got:\n{}",
            body,
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Tier-0 callees (interface declares no `ensures`) must not emit any
    /// `kani::assume` lines in the impl harness. Mirrors the spec-model
    /// variant's `tier0_callee_emits_no_kani_assume_lines` test.
    #[test]
    fn tier0_callee_emits_no_kani_assume_lines() {
        let src = r#"spec Tier0Impl
program_id "11111111111111111111111111111111"

interface Logger {
  program_id "11111111111111111111111111111111"
  handler log (msg : U64) {
    accounts {
      sink : writable
    }
  }
}

state { counter : U64 }

handler tick (val : U64) {
  permissionless
  requires val > 0 else Bad
  modifies [counter, shadow]
  call Logger.log(msg = val)
  effect { counter += val }
  ensures state.counter == old(state.counter) + val
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_tier0_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            !body.contains("CPI ensures-as-fact (Logger.log)"),
            "Tier-0 callee (no ensures) must not emit any CPI assume block; got:\n{}",
            body
        );
        // Caller's own assert! still emits.
        assert!(
            body.contains("assert!("),
            "caller's own assert! must still emit; got:\n{}",
            body
        );
        // And no `kani::assume(` introduced by Track I — the only assumes
        // that may appear are the caller's own requires-guard assume (none
        // here, since `val > 0` is the requires).
        // (We check by counting: the requires-guard assume is `val > 0`,
        // so a Logger-derived assume would appear separately.)
        let assume_count = body.matches("kani::assume(").count();
        // Exactly one assume — the caller's own requires-guard
        // (`val > 0 else Bad`).
        assert_eq!(
            assume_count, 1,
            "Tier-0 callee must not add any kani::assume lines; got {} assumes in:\n{}",
            assume_count, body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// `let X = call Foo.bar(...)` puts `X` in scope in the substituted
    /// ensures via the `result` convention (v2.24 #11). Mirrors the
    /// spec-model variant's `let_call_binding_participates_in_substitution`
    /// test.
    #[test]
    fn let_binding_participates_in_substitution() {
        let src = r#"spec LetCallImpl
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb (amount : U64) -> U64 {
    accounts {
      vault : writable
    }
    requires amount > 0
    ensures result <= amount
  }
}

state { total_loss : U64 }

handler liquidate (loss : U64) {
  permissionless
  requires loss > 0 else Bad
  modifies [total_loss, shadow]
  let burned = call Pool.absorb(amount = loss)
  effect { total_loss += loss }
  ensures state.total_loss == old(state.total_loss) + loss
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_letcall_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            body.contains("// CPI ensures-as-fact (Pool.absorb):"),
            "missing CPI ensures-as-fact for Pool.absorb; got:\n{}",
            body
        );
        // `result <= amount` substitutes `amount → loss` and
        // `result → burned`.
        assert!(
            body.contains("kani::assume(burned <= loss)"),
            "let-binding result must substitute to caller's binder; got:\n{}",
            body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.26 Track J — when `multi_cpi_shared_fields` fires for a
    /// handler, the impl harness emits a WARNING breadcrumb comment
    /// above the CPI assume block so a reader of the generated file
    /// sees the over-constraint risk without cross-referencing the lint
    /// output. The breadcrumb sits between the post-snapshot and the
    /// first `kani::assume` from any CPI.
    #[test]
    fn multi_cpi_breadcrumb_emits_above_assume_block() {
        let src = r#"spec MultiCpiKaniImpl
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.vault_balance == old(state.vault_balance) - amount
  }
}

state { vault_balance : U64 }

handler split (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  modifies [vault_balance, shadow]
  call Token.transfer(from = 0, to = 1, amount = a, authority = 0)
  call Token.transfer(from = 0, to = 2, amount = b, authority = 0)
  effect { vault_balance -= a }
  ensures state.vault_balance == old(state.vault_balance) - a - b
}"#;
        let spec = parse_str(src).expect("parse");
        // LP-shape gap (modifies = {vault_balance, shadow}, effect-LHS =
        // {vault_balance}) triggers auto-emission.
        assert!(spec_triggers_impl_harness(&spec));

        let tmp =
            std::env::temp_dir().join(format!("kani_impl_multi_cpi_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false, Target::Anchor)
            .expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // 1. Two CPI assume lines must be present (one per call).
        let assume_count = body
            .matches("// CPI ensures-as-fact (Token.transfer):")
            .count();
        assert_eq!(
            assume_count, 2,
            "two CPI assume blocks must emit; got {} in:\n{}",
            assume_count, body
        );

        // 2. The breadcrumb WARNING must appear in the harness body
        //    (above the CPI assume block).
        assert!(
            body.contains("WARNING: multi-CPI ordering"),
            "Track J breadcrumb must emit when multi_cpi_shared_fields fires; got:\n{}",
            body
        );
        assert!(
            body.contains("`multi_cpi_same_field`"),
            "breadcrumb must reference the lint rule name; got:\n{}",
            body
        );

        // 3. Ordering: WARNING sits between the `if result.is_ok()`
        //    branch open and the first `kani::assume` of the CPI block.
        let is_ok_pos = body
            .find("if result.is_ok()")
            .expect("`if result.is_ok()` must be present");
        let warn_pos = body
            .find("WARNING: multi-CPI ordering")
            .expect("breadcrumb present (just asserted)");
        let first_cpi_assume = body[is_ok_pos..]
            .find("// CPI ensures-as-fact")
            .map(|i| is_ok_pos + i)
            .expect("CPI assume block must follow is_ok()");
        assert!(
            is_ok_pos < warn_pos && warn_pos < first_cpi_assume,
            "WARNING breadcrumb must sit between is_ok() and the first \
             CPI ensures-as-fact comment; positions: is_ok={} warn={} cpi={}; got:\n{}",
            is_ok_pos,
            warn_pos,
            first_cpi_assume,
            body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.26 fold-in — a spec with no LP-shape handler but a ref_impl
    /// that carries potentially-overflowing arithmetic still auto-triggers
    /// the impl-targeted harness. Lean proves on `Nat`; Kani is the only
    /// verification surface that catches the `u64` overflow.
    #[test]
    fn ref_impl_overflow_risk_auto_triggers_impl_harness() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl scaled (a : U64) (b : U64) : U64 = a * b

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
  ensures state.x == scaled(old(state.x), amt)
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        // No handler trips the LP-shape signal (`set` declares no modifies).
        assert!(
            !spec.handlers.iter().any(handler_triggers_impl_harness),
            "no handler should trip the modifies-driven trigger in this fixture"
        );
        // But the ref_impl `scaled` has `*` over U64, so the auto-trigger
        // still fires through the ref_impl overflow-risk predicate.
        assert!(
            spec_triggers_impl_harness(&spec),
            "ref_impl with multiplication over bounded-numeric params \
             must auto-trigger the impl harness"
        );
    }

    /// Symmetric negative: ref_impl with only division (no overflow risk)
    /// AND no LP-shape handler — auto-trigger stays quiet.
    #[test]
    fn ref_impl_without_overflow_risk_does_not_auto_trigger() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl half (a : U64) : U64 = a / 2

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        assert!(
            !spec_triggers_impl_harness(&spec),
            "ref_impl with only division must not auto-trigger \
             (no overflow risk, nothing for Kani to catch)"
        );
    }
}
