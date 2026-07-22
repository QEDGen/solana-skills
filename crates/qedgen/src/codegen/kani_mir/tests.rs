//! `kani_mir` unit tests — moved verbatim from the pre-split `kani_mir.rs`
//! test module.

use super::*;
use crate::check;
use std::path::Path;

/// Conditional-effect handlers get per-arm conformance harnesses:
/// each arm pinned via `kani::assume(<scrutinee> == <pattern>)` with
/// the frame check scoped to that arm's effects; the wildcard arm
/// pins via negated assumes over every literal pattern. Flat
/// per-effect harnesses self-skip for Branch handlers — their
/// unconditional assertions are invalid under match semantics.
#[test]
fn branch_handlers_get_per_arm_conformance_harnesses() {
    let (mir, parsed) = lower_fixture(
        "crates/qedgen/tests/fixtures/regressions/issue-42-conditional/fee_router.qedspec",
    );
    let out = render(&mir, &parsed);

    // Per-arm harnesses with scrutinee pins.
    assert!(
        out.contains("fn verify_collect_fees_arm0_effect_fees_a_withdrawn()"),
        "arm-0 harness missing:\n{out}"
    );
    assert!(
        out.contains("    kani::assume(fee_type == 0);"),
        "arm-0 scrutinee pin missing:\n{out}"
    );
    // Wildcard arm: negated pins + the set-effect assertion.
    assert!(
        out.contains("fn verify_collect_fees_default_effect_fees_d_accumulated()"),
        "default-arm harness missing:\n{out}"
    );
    for pin in [
        "    kani::assume(fee_type != 0);",
        "    kani::assume(fee_type != 1);",
        "    kani::assume(fee_type != 2);",
    ] {
        assert!(out.contains(pin), "default pin `{pin}` missing:\n{out}");
    }
    // Frame scoped to the arm: under the arm-0 pin, every other
    // field must be asserted unchanged (including the other arms'
    // targets).
    let arm0 = out
        .split("fn verify_collect_fees_arm0_effect_fees_a_withdrawn()")
        .nth(1)
        .and_then(|rest| rest.split("#[kani::proof]").next())
        .expect("arm-0 harness body");
    for sibling in [
        "fees_b_withdrawn must not change",
        "fees_c_accumulated must not change",
        "fees_d_accumulated must not change",
    ] {
        assert!(
            arm0.contains(sibling),
            "frame check `{sibling}` missing:\n{arm0}"
        );
    }
    // No unconditional flat harness for a Branch handler.
    assert!(
        !out.contains("fn verify_collect_fees_effect_"),
        "flat per-effect harness must self-skip for Branch handlers:\n{out}"
    );
}

fn lower_fixture(rel_path: &str) -> (Mir, ParsedSpec) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qedgen/ under repo root");
    let spec_path = root.join(rel_path);
    let parsed = check::parse_spec_file(&spec_path).expect("fixture parses");
    let mir = crate::mir::lower(&parsed);
    (mir, parsed)
}

fn lower_inline(src: &str) -> (Mir, ParsedSpec) {
    let parsed = crate::chumsky_adapter::parse_str(src).expect("inline fixture parses");
    let mir = crate::mir::lower(&parsed);
    (mir, parsed)
}

const ENVIRONMENT_SPEC_HEAD: &str = r#"spec EnvironmentHarness
program_id "11111111111111111111111111111111"

type State = { rate : U64, }
type Error | Bad

property rate_positive : state.rate > 0 preserved_by all
"#;

#[test]
fn environment_binary_constraint_routes_pre_and_post_receivers() {
    let src = format!(
        "{}{}",
        ENVIRONMENT_SPEC_HEAD,
        r#"
environment rate_change {
  mutates rate : U64
  constraint state.rate >= old(state.rate)
}
"#
    );
    let (mir, parsed) = lower_inline(&src);
    let out = render(&mir, &parsed);
    let harness = out
        .split("fn verify_rate_positive_under_rate_change()")
        .nth(1)
        .expect("environment harness");

    assert!(harness.contains("    let pre = s.clone();\n"), "{harness}");
    assert!(harness.contains("    let post = &s;\n"), "{harness}");
    assert!(
        harness.contains("    kani::assume(post.rate >= pre.rate);\n"),
        "binary constraint must keep old/new state distinct:\n{harness}"
    );
    let pre = harness.find("let pre = s.clone();").unwrap();
    let mutation = harness.find("s.rate = kani::any();").unwrap();
    let post = harness.find("let post = &s;").unwrap();
    let assumption = harness
        .find("kani::assume(post.rate >= pre.rate);")
        .unwrap();
    assert!(
        pre < mutation && mutation < post && post < assumption,
        "pre/post bindings must bracket the external mutation:\n{harness}"
    );
}

#[test]
fn environment_external_clock_uses_distinct_pre_post_values() {
    let src = format!(
        "{}{}",
        ENVIRONMENT_SPEC_HEAD,
        r#"
environment clock_advance {
  external clock.slot : U64
  constraint clock.slot >= old(clock.slot)
}
"#
    );
    let (mir, parsed) = lower_inline(&src);
    let out = render(&mir, &parsed);
    let harness = out
        .split("fn verify_rate_positive_under_clock_advance()")
        .nth(1)
        .expect("external environment harness");
    assert!(
        harness.contains("let pre_clock_slot: u64 = kani::any();"),
        "{harness}"
    );
    assert!(
        harness.contains("let post_clock_slot: u64 = kani::any();"),
        "{harness}"
    );
    assert!(
        harness.contains("kani::assume(post_clock_slot >= pre_clock_slot);"),
        "{harness}"
    );
    assert!(!harness.contains("s.slot ="), "{harness}");
}

#[test]
fn environment_unary_state_constraint_with_external_binds_post() {
    // Regression: an external field forces the two-state (PrePost) binder, so
    // a UNARY state constraint renders `post.rate` — the harness must bind
    // `post` even though there is no `old(...)`/`pre`. Previously `post` was
    // emitted only for binary constraints, leaving `post` unbound → the
    // generated Kani harness failed to compile.
    let src = format!(
        "{}{}",
        ENVIRONMENT_SPEC_HEAD,
        r#"
environment clock_check {
  external clock.slot : U64
  constraint state.rate > 0
}
"#
    );
    let (mir, parsed) = lower_inline(&src);
    let out = render(&mir, &parsed);
    let harness = out
        .split("fn verify_rate_positive_under_clock_check()")
        .nth(1)
        .expect("external+unary environment harness");
    assert!(
        harness.contains("    kani::assume(post.rate > 0);\n"),
        "unary state read renders through the post receiver:\n{harness}"
    );
    assert!(
        harness.contains("    let post = &s;\n"),
        "post must be bound for the unary state read:\n{harness}"
    );
    assert!(
        !harness.contains("    let pre = s.clone();\n"),
        "no old(...) read, so pre must not be emitted:\n{harness}"
    );
}

#[test]
fn environment_unary_constraint_renders_live_state_binder() {
    let src = format!(
        "{}{}",
        ENVIRONMENT_SPEC_HEAD,
        r#"
environment rate_change {
  mutates rate : U64
  constraint state.rate > 0
}
"#
    );
    let (mir, parsed) = lower_inline(&src);
    let typed_out = render(&mir, &parsed);

    // Unary post-state assumptions read the live `s` binder — no pre/post
    // snapshots emitted for a constraint with no `old(...)` read.
    assert!(typed_out.contains("    kani::assume(s.rate > 0);\n"));
    assert!(!typed_out.contains("    let pre = s.clone();\n"));
    assert!(!typed_out.contains("    let post = &s;\n"));
}

#[test]
fn render_emits_file_header_and_cfg_kani() {
    // The structural prefix is deterministic — every pilot
    // fixture produces the same banner + `#![cfg(kani)]` line.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.starts_with("// ---- GENERATED BY QEDGEN"),
        "expected banner-style first line; got:\n{}",
        &out[..out.len().min(200)]
    );
    assert!(
        out.contains("#![cfg(kani)]"),
        "expected #![cfg(kani)] attribute"
    );
    assert!(
        out.contains("Self-contained Kani proof harnesses for the spec."),
        "expected legacy file-header docstring"
    );
}

#[test]
fn render_emits_state_model_header_banner() {
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// State model (derived from qedspec"),
        "expected state-model section header"
    );
}

#[test]
fn render_emits_constants_when_spec_declares_them() {
    // The issue-8 pool fixture declares SCHEDULE_LANES,
    // RATE_PRECISION, … — Mir.constants carries them as
    // (name, value) and `emit_constants` lowers them to
    // `pub const NAME: u64 = VALUE;`.
    let (mir, parsed) =
        lower_fixture("crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec");
    let out = render(&mir, &parsed);
    // `rust_codegen_util::emit_constants` writes `const NAME:
    // <ty> = VALUE;` (file-scoped, no `pub` — the per-ADT modules
    // pull them in via `use super::*`).
    assert!(
        out.contains("const SCHEDULE_LANES"),
        "expected SCHEDULE_LANES constant emit"
    );
    assert!(
        out.contains("const RATE_PRECISION"),
        "expected RATE_PRECISION constant emit"
    );
}

#[test]
fn render_emits_no_pending_phase_marker() {
    // The rendered file must not contain any MIR-TODO(phase-...) marker.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        !out.contains("MIR-TODO(phase-"),
        "expected no pending phase-TODO markers; got:\n{}",
        out.lines()
            .filter(|l| l.contains("MIR-TODO"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn render_emits_cover_harnesses_single_mode() {
    // Covers fire in single-account mode. Escrow has
    // `cover initialize_then_close [initialize, exchange]`.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// Cover properties —"),
        "expected cover-properties section header"
    );
    assert!(
        out.contains("fn cover_"),
        "expected at least one cover_<name> harness"
    );
}

#[test]
fn render_skips_file_level_features_for_multi_account() {
    // Lending is multi-account, so even if it declared
    // covers/liveness/env, the section headers must NOT emit.
    let (mir, parsed) = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        !out.contains("// Cover properties — reachability via kani::cover!"),
        "multi-account specs must skip file-level cover section"
    );
    assert!(
        !out.contains("// Liveness properties — bounded reachability via non-deterministic ops"),
        "multi-account specs must skip file-level liveness section"
    );
    assert!(
        !out.contains("// Environment — properties hold under external state changes"),
        "multi-account specs must skip file-level environment section"
    );
}

#[test]
fn render_emits_effect_conformance_harnesses() {
    // Escrow's `initialize` has `:= deposit_amount` /
    // `:= receive_amount` / `:= Open` effects so it emits.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// Effect conformance —"),
        "expected effect-conformance section header"
    );
    assert!(
        out.contains("fn verify_initialize_effect_"),
        "expected initialize_effect_<field> harness"
    );
}

#[test]
fn render_skip_guard_proofs_still_emits_effect_proofs() {
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let mut rec =
        crate::obligations::ObligationRecorder::new(crate::obligations::ObligationBackend::Kani);
    let out = render_inner(&mir, &parsed, false, true, &mut rec);
    assert!(
        !out.contains("// Guard enforcement — transitions reject invalid inputs"),
        "expected guard-rejection section to be skipped"
    );
    assert!(
        !out.contains("fn verify_initialize_rejects_invalid"),
        "expected guard-rejection harnesses to be skipped"
    );
    assert!(
        out.contains("fn verify_initialize_effect_"),
        "expected effect proofs to remain when guard proofs are skipped"
    );
}

#[test]
fn render_emits_overflow_detection_harnesses_for_add_effects() {
    // bundled-stdlib-demo's `deposit` has `total_assets += amount`,
    // so the overflow harness emits.
    let (mir, parsed) = lower_fixture("examples/rust/bundled-stdlib-demo/pool.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// Overflow detection —"),
        "expected overflow-detection section header"
    );
    assert!(
        out.contains("fn verify_deposit_no_overflow()"),
        "expected verify_deposit_no_overflow harness"
    );
}

#[test]
fn render_emits_ensures_preservation_harnesses() {
    // The issue-8 pool fixture has handlers with ensures clauses
    // (with old(...) bindings).
    let (mir, parsed) =
        lower_fixture("crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// Ensures preservation —"),
        "expected ensures-preservation section header"
    );
    assert!(
        out.contains("_ensures_"),
        "expected at least one _ensures_<idx> harness"
    );
}

#[test]
fn render_emits_no_invariant_preservation_section_when_no_clauses() {
    // The section only fires when a handler carries `invariant Name`
    // or `establishes Name`. Escrow's `invariant` declarations exist
    // but aren't claimed by handlers, so the header doesn't emit.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        !out.contains("// Invariant preservation —"),
        "expected no invariant-preservation section for pilots without handler invariant clauses"
    );
}

#[test]
fn render_emits_property_preservation_harnesses() {
    // Multisig declares `property votes_bounded` with `preserved_by`;
    // one `verify_<handler>_preserves_votes_bounded()` per matched
    // handler. Per-pair bodies are covered by the snapshot suite.
    let (mir, parsed) = lower_fixture("examples/rust/multisig/multisig.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// Property preservation —"),
        "expected property-preservation section header"
    );
    assert!(
        out.contains("_preserves_"),
        "expected at least one preserves_<prop> harness"
    );
}

#[test]
fn render_emits_guard_enforcement_harnesses() {
    // Escrow's `initialize` has `requires deposit_amount > 0 &&
    // receive_amount > 0`, so the rejects_invalid harness emits.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("// Guard enforcement"),
        "expected guard-enforcement section header"
    );
    assert!(
        out.contains("fn verify_initialize_rejects_invalid()"),
        "expected initialize rejects_invalid harness"
    );
    assert!(
        out.contains("kani::assume(!("),
        "expected `kani::assume(!(guard))` negation"
    );
    assert!(
        out.contains("\"initialize must reject when guard is violated\""),
        "expected assert message"
    );
}

#[test]
fn render_splits_large_guard_rejection_harnesses() {
    let (mir, parsed) =
        lower_fixture("crates/qedgen/tests/fixtures/kani-cpi-account-bindings/config.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        !out.contains("fn verify_stable_swap_large_guard_rejects_invalid()"),
        "large guard should omit the monolithic rejects_invalid harness"
    );
    assert!(
        out.contains("fn verify_stable_swap_large_guard_rejects_invalid_1_pubkey_eq_accounts_admin_pubkey_s_admin_key()"),
        "expected deterministic pubkey split harness name:\n{out}"
    );
    assert!(
        out.contains("fn verify_stable_swap_large_guard_rejects_invalid_8_mul_bps_floor_u128_amount_in_fee_bps_amount_in()"),
        "expected deterministic fee arithmetic split harness name:\n{out}"
    );
    assert!(
        out.contains("kani::assume(!(pubkey_eq(&accounts.admin.pubkey, &s.admin_key)));"),
        "split term should be individually negated after pubkey rewrite:\n{out}"
    );
    assert!(
        out.contains("let __qed_bps_floor_1 = mul_bps_floor_u128(amount_in, fee_bps);\n    kani::assume(__qed_bps_floor_1 > amount_in);"),
        "split term should bind and negate fee arithmetic by itself:\n{out}"
    );
    // Each split harness must call the handler and prove it rejects —
    // asserting the negated term back would be a vacuous tautology
    // (`assume P; assert P`) that never exercises the transition.
    assert!(
        out.contains(
            "assert!(!stable_swap_large_guard(&mut s, &accounts, amount_in, min_out, fee_bps, lane, input_mint, output_mint),\n        \"stable_swap_large_guard must reject when guard term is violated\");"
        ),
        "split term should prove the handler rejects when the guard term is violated:\n{out}"
    );
    assert!(
        !out.contains(
            "assert!(!(__qed_bps_floor_1 <= amount_in),\n        \"stable_swap_large_guard guard term must be false when violated\");"
        ),
        "split harness must not assert the negated term back (vacuous tautology):\n{out}"
    );
    assert!(
        out.contains("kani::assume(pubkey_eq(&accounts.admin.pubkey, &s.admin_key));\n    kani::assume(amount_in > 0);"),
        "later split terms should assume earlier terms true, partitioning by first failed guard:\n{out}"
    );
    // An arm whose term is implied by its prefix has UNSAT assumptions —
    // the rejection assert is then unreachable and Kani reports SUCCESSFUL
    // while proving nothing for that arm. The cover makes that loud.
    assert!(
        out.contains("kani::cover!(true, \"guard-violation domain is satisfiable\");"),
        "each split arm needs a satisfiability cover so a vacuous arm fails instead of passing silently:\n{out}"
    );
}

#[test]
fn guard_term_slug_is_deterministic_and_sanitized() {
    assert_eq!(
        guard_term_slug("pubkey_eq(&accounts.admin.pubkey, &s.admin_key)"),
        "pubkey_eq_accounts_admin_pubkey_s_admin_key"
    );
    assert_eq!(
        guard_term_slug("mul_bps_floor_u128(amount_in, fee_bps) <= amount_in"),
        "mul_bps_floor_u128_amount_in_fee_bps_amount_in"
    );
}

#[test]
fn render_emits_state_struct_for_single_account() {
    // Escrow has lifecycle states, so `struct State` carries the
    // `status: Status` field.
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir, &parsed);
    assert!(out.contains("struct State {"), "expected State struct");
    assert!(out.contains("status: Status"), "expected status field");
    // Transition fns mirror the spec's handler set.
    assert!(
        out.contains("fn initialize(s: &mut State"),
        "expected initialize transition fn"
    );
    assert!(
        out.contains("fn exchange(s: &mut State"),
        "expected exchange transition fn"
    );
    assert!(
        out.contains("fn cancel(s: &mut State"),
        "expected cancel transition fn"
    );
}

#[test]
fn render_emits_mod_wrapping_for_multi_account() {
    // Lending declares Pool + Loan account types — one
    // `mod <lowercase>` per account_type with `use super::*;`.
    let (mir, parsed) = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir, &parsed);
    assert!(
        out.contains("mod pool {\n    use super::*;"),
        "expected `mod pool {{ use super::*; }}` wrapper"
    );
    assert!(
        out.contains("mod loan {\n    use super::*;"),
        "expected `mod loan {{ use super::*; }}` wrapper"
    );
    assert!(
        out.contains("} // mod pool"),
        "expected `}} // mod pool` close"
    );
    assert!(
        out.contains("} // mod loan"),
        "expected `}} // mod loan` close"
    );
}

/// Issue #139: bare state-field names in `requires` must reach the Kani
/// transition fn and guard-rejection assumes with the `s.` receiver —
/// bare `active` is a compile error inside `fn execute(s: &mut State, …)`.
#[test]
fn bare_state_field_requires_reach_harness_with_receiver() {
    let (mir, parsed) = lower_fixture(
        "crates/qedgen/tests/fixtures/regressions/issue-139-bare-state-refs/generic_vault.qedspec",
    );
    let out = render(&mir, &parsed);
    assert!(
        out.contains("if !(s.active == 0 && amount > 0)"),
        "transition guard must read state through `s`:\n{out}"
    );
    assert!(
        out.contains("kani::assume(!(s.active == 0 && amount > 0));"),
        "guard-rejection assume must read state through `s`:\n{out}"
    );
    assert!(
        !out.contains("(active == 0)"),
        "bare state-field guard leaked into the harness:\n{out}"
    );
}

/// v2.46 (Bug 2) — a `set` effect whose RHS is a comparison
/// (`seat_active := seat_stake > 0`) must parenthesize it in the
/// conformance assert: `s.seat_active == pre_seat_stake > 0` is a chained
/// comparison (a Rust compile error).
#[test]
fn conformance_parenthesizes_comparison_valued_set_rhs() {
    let (mir, parsed) = lower_inline(
        r#"spec Vault
program_id "11111111111111111111111111111111"
type State | Active of { seat_stake : U64, seat_active : Bool }
type Error | Bad
handler settle : State.Active -> State.Active {
  permissionless
  effect { seat_active := seat_stake > 0 }
}
"#,
    );
    let out = render(&mir, &parsed);
    assert!(
        out.contains("s.seat_active == (pre_seat_stake > 0)"),
        "comparison-valued set RHS must be parenthesized in the conformance assert:\n{out}"
    );
    assert!(
        !out.contains("s.seat_active == pre_seat_stake > 0"),
        "unparenthesized chained comparison must not appear:\n{out}"
    );
}

/// Issues #143–#146: compound effect RHS and predicate arithmetic reach
/// the Kani harness as compilable, model-faithful Rust.
///   #143 — ref_impl calls in effect RHS render Rust call syntax with the
///          `s.` receiver, not ML application syntax.
///   #144 — `if … then … else` RHS lowers to a Rust conditional.
///   #145 — the `mul_div_floor_u128` helper is emitted when only a
///          ref_impl body references it, and the ref_impl narrows back
///          to its declared return width.
///   #146 — bare `-` in effect RHS lowers checked (reject on underflow);
///          bare `+` inside guard / property comparisons widens to u128
///          so predicate evaluation can't overflow-panic.
#[test]
fn compound_effect_rhs_and_arith_predicates_render_soundly() {
    let (mir, parsed) = lower_fixture(
        "crates/qedgen/tests/fixtures/regressions/issues-143-146-kani-arith/vault.qedspec",
    );
    let out = render(&mir, &parsed);

    // #143 — Rust call syntax, state-qualified args.
    assert!(
        out.contains("s.fee = bps_mul(amount, s.rate);"),
        "ref_impl call in effect RHS must render as Rust:\n{out}"
    );
    assert!(
        !out.contains("(bps_mul (amount)"),
        "ML application syntax leaked into the harness:\n{out}"
    );

    // #144 — Rust conditional expression. v2.44: `fee` is written earlier
    // in the same block, so the read routes through the parallel-semantics
    // snapshot (`pre_fee`), matching the Lean model's record update;
    // unwritten fields (`flag`, `rate`) keep the `s.` read.
    assert!(
        out.contains("let pre_fee = s.fee;"),
        "read-after-write field must be snapshotted:\n{out}"
    );
    assert!(
        out.contains("s.cut = (if s.flag == 1 { bps_mul(pre_fee, s.rate) } else { 0 });"),
        "conditional effect RHS must lower to a Rust if-else over pre-state:\n{out}"
    );
    assert!(
        !out.contains(" then "),
        "ML `then` keyword leaked into the harness:\n{out}"
    );

    // #145 — helper referenced for a ref_impl-only use; #182 — imported from the
    // soundness-proven crate rather than inlined.
    assert!(
        out.contains("use qedgen_kani_prelude::{mul_div_ceil_u128, mul_div_floor_u128, mul_div_round_half_up_u128};"),
        "mul_div helpers must be imported from qedgen_kani_prelude:\n{out}"
    );
    assert!(
        out.contains(
            "(mul_div_floor_u128(((amount) as u128), ((bps) as u128), ((10000) as u128))) as u64"
        ),
        "ref_impl body must narrow the u128 helper to the declared width:\n{out}"
    );

    // #146 — checked effect subtraction rejects instead of panicking …
    // (v2.44: both operands are block-written, so they read pre-state.)
    assert!(
        out.contains(
            "s.residual = match (|| -> Option<u64> { Some((pre_fee).checked_sub(pre_cut)?) })() \
             { Some(__rhs) => __rhs, None => return false };"
        ),
        "bare `-` in effect RHS must lower to a checked rejection:\n{out}"
    );
    // … guard addition evaluates in u128 (matches the Lean Nat model) …
    assert!(
        out.contains("((now) as u128) >= ((s.start) as u128) + ((s.period) as u128)"),
        "guard arithmetic must widen to u128:\n{out}"
    );
    // … and the property predicate can't overflow while being evaluated.
    // (No parens around the sum: redundant source parens don't survive the
    // #151 tree — grouping is structural.)
    assert!(
        out.contains("((s.cut) as u128) + ((s.residual) as u128) == ((s.fee) as u128)"),
        "property arithmetic must widen to u128:\n{out}"
    );

    // The whole harness must be syntactically valid Rust — the four
    // issues above all shipped as emitted-code parse errors.
    if let Err(e) = syn::parse_file(&out) {
        panic!("emitted Kani harness fails to parse as Rust: {e}\n{out}");
    }
}
