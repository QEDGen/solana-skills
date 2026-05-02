//! `qedgen probe` reproducer construction pipeline.
//!
//! Implements the v2.16 contract: every emitted `Finding` carries a
//! concrete `Reproducer` (Kani trace, proptest seed, or sandbox tx) the
//! user can re-run deterministically. Findings whose reproducer cannot
//! be constructed are **silently dropped** — there is no advisory tier.
//!
//! ## Pipeline shape
//!
//! `run_probe` collects candidate findings from each `predicate_*` fn in
//! `probe.rs` (each with `reproducer: None`), then runs every candidate
//! through `construct_reproducer`. The dispatcher routes by
//! `finding.category` to a per-category constructor that:
//!
//! 1. Builds a Mollusk-driven Rust integration test, Kani harness, or
//!    proptest seed from the spec slice the predicate flagged.
//! 2. Executes it (within `ctx.kani_budget`).
//! 3. Captures the counterexample / failing seed / observed violation.
//! 4. Writes reproducer artifacts under `target/qedgen-repros/<finding.id>/`
//!    — **ephemeral, never committed** (regenerated every probe run; see
//!    PLAN-v2.16 D3).
//! 5. Returns a `Reproducer` whose `invocation` field re-runs the artifact.
//!
//! On any failure (timeout, no counterexample found, build error) the
//! constructor returns `Err(ConstructFailure::*)` and the candidate
//! finding is dropped.
//!
//! ## Why drop-on-fail
//!
//! Per `feedback_probes_reproducible_only.md`: lint pattern matches
//! without a reproducer are auditor-grade noise — users have lived with
//! generic warnings and don't act. A finding with no reproducer also
//! can't defend against "we don't think that's reachable." If a Kani
//! harness times out, the bug *might* exist but we have no evidence:
//! silent is more honest than "possibly vulnerable."
//!
//! ## v2.16 ship status — D3 deferred to v3
//!
//! v2.16 ships this module's **scaffolding only**: the dispatcher,
//! the `ConstructFailure` enum, and per-category constructor stubs
//! that all return `Err(ConstructFailure::NotImplemented)`. The
//! actual per-category file-writing constructors **do not ship in
//! v2.16** — PLAN-v2.16's D3 was deferred to v3 after design review
//! concluded the mechanical-template approach trades too much
//! codegen-bug surface for too little file content (see
//! `feedback_repros_agent_authored.md`).
//!
//! v3's design replaces these constructors with **agent-authored
//! repros via structured prompts** — `qedgen probe` emits a
//! `pending_repros[]` list with one prompt per finding; the
//! in-session agent reads each prompt and writes the test file
//! directly via the Write tool. The dispatcher and stubs in this
//! file get reworked or removed at that point.
//!
//! Until v3 lands: every probe finding is silently dropped under the
//! reproducible-only contract (no constructor produces a real
//! reproducer). This is the correct user-visible behavior. The
//! auditor SKILL (D5) provides an end-to-end path today by writing
//! Mollusk repros directly via Write tool, bypassing this dispatcher.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::check::ParsedSpec;
use crate::probe::{Category, Finding, Reproducer};

/// Default per-finding budget for symbolic execution. Picked so that a
/// full probe run on a typical program (10-20 candidate findings) caps
/// at ~10-20 minutes wall clock — slow enough for thorough Kani search,
/// fast enough that CI doesn't timeout.
pub const DEFAULT_KANI_BUDGET: Duration = Duration::from_secs(60);

/// Reasons a candidate finding may fail to acquire a reproducer. All
/// variants result in the candidate being dropped — no variant emits
/// to the user as "advisory" or "possibly vulnerable."
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants populated as categories retrofit
pub enum ConstructFailure {
    /// Category retrofit not yet shipped. Default for all stubs in v2.16
    /// pre-retrofit; replaced with concrete constructors in Tasks 3-6.
    NotImplemented,
    /// Kani harness compiled and ran, but exhausted the budget without
    /// producing a counterexample. The bug may still exist — we have no
    /// evidence, so the finding is dropped.
    KaniTimeout { budget: Duration },
    /// Kani exhausted its search depth without finding a counterexample,
    /// within budget. Either the spec slice is genuinely safe (predicate
    /// false-positive) or Kani's BMC depth is insufficient. Either way:
    /// no reproducer, no finding.
    KaniNoCounterexample,
    /// Proptest seed could not be constructed — typically because the
    /// spec slice doesn't yield a closed input shape we can drive.
    ProptestNoFailure,
    /// Building the harness / test / sandbox tx failed (compile error,
    /// missing dependency, fixture not on disk). Drops the finding —
    /// build flakiness is not the user's problem.
    BuildError(String),
    /// I/O writing reproducer artifacts under `target/qedgen-repros/`.
    /// Drops the finding rather than emitting half-written artifacts.
    Io(String),
}

/// Inputs every category constructor needs. Paths are absolute.
#[allow(dead_code)] // Fields consumed by category constructors as they retrofit
pub struct ReproducerContext<'a> {
    pub spec: &'a ParsedSpec,
    pub spec_path: &'a Path,
    pub project_root: PathBuf,
    pub kani_budget: Duration,
}

impl<'a> ReproducerContext<'a> {
    /// Build a context from the spec path. Project root is the directory
    /// containing the spec — that's where `target/qedgen-repros/` lives.
    pub fn from_spec_path(spec: &'a ParsedSpec, spec_path: &'a Path) -> Self {
        let project_root = spec_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            spec,
            spec_path,
            project_root,
            kani_budget: DEFAULT_KANI_BUDGET,
        }
    }

    /// Directory where this finding's reproducer artifacts live.
    /// Convention: `<project_root>/target/qedgen-repros/<finding_id>/`.
    /// Per PLAN-v2.16 D3, repros are **ephemeral** (regenerated every probe
    /// run) and therefore live under `target/` (cargo-ignored), not under
    /// `.qed/` (which is committed for spec/lock state).
    #[allow(dead_code)] // Used by category constructors as they retrofit
    pub fn repro_dir(&self, finding_id: &str) -> PathBuf {
        self.project_root
            .join("target")
            .join("qedgen-repros")
            .join(finding_id)
    }
}

/// Dispatcher: route a candidate finding to its per-category constructor.
/// Returns `Ok(Reproducer)` if the bug was reproduced concretely, or
/// `Err(ConstructFailure)` to signal the caller should drop the finding.
pub fn construct_reproducer(
    finding: &Finding,
    ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    match finding.category {
        Category::ArithmeticOverflowWrapping => {
            construct_arithmetic_overflow_wrapping(finding, ctx)
        }
        Category::UnboundedAmountParam => construct_unbounded_amount_param(finding, ctx),
        Category::LifecycleOneShotViolation => construct_lifecycle_one_shot_violation(finding, ctx),
        Category::MissingSigner => construct_missing_signer(finding, ctx),
        Category::ArbitraryCpi => construct_arbitrary_cpi(finding, ctx),
        Category::PermissionlessStateWriter => construct_permissionless_state_writer(finding, ctx),
        Category::InitWithoutPda => construct_init_without_pda(finding, ctx),
        Category::StoredFieldNeverWritten => construct_stored_field_never_written(finding, ctx),
    }
}

// ---------------------------------------------------------------------------
// Per-category constructors. Each is stubbed to `NotImplemented` until the
// category retrofits in Tasks 3-6 of the v2.16 plan. Per PLAN-v2.16 D3/D4,
// reproducers are Mollusk-driven sandbox txs that invoke the user's real
// handler with attack inputs and observe state corruption — not synthesized
// witness tests against the operator alone.
// ---------------------------------------------------------------------------

/// Task 3 — Mollusk sandbox tx. Invoke the handler with overflow-triggering
/// params (e.g. `u64::MAX` into a `+=?` field), observe wrap propagated to
/// post-state. Repro is a Rust integration test under
/// `target/qedgen-repros/<id>/` (ephemeral, regenerated each probe run).
fn construct_arithmetic_overflow_wrapping(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 4 — Kani harness. Drive the handler with `u64::MAX` (or the
/// declared type's saturated value). Assert overflow / drain.
fn construct_unbounded_amount_param(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 5 — Proptest seed. Generate an invocation in an unintended
/// lifecycle state, assert effects fired anyway.
fn construct_lifecycle_one_shot_violation(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 6 — Sandbox tx. Invoke handler from an unauthorized signer
/// against litesvm; observe the state change occurs without auth.
fn construct_missing_signer(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 6 — May migrate to spec-less mode if a Kani harness on the
/// impl-side CPI list is required (the spec doesn't carry the impl's
/// CPI list, so a spec-only repro is structurally insufficient).
fn construct_arbitrary_cpi(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 6 — Sandbox tx. Two concurrent calls from unauthorized signers
/// observe shared-state corruption.
fn construct_permissionless_state_writer(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 6 — Sandbox tx. Two callers race the same canonical address and
/// observe state collision.
fn construct_init_without_pda(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Task 6 — Riskiest. Zero-init reachability is a Kani problem but may
/// not be constructible from the spec alone. If so, the category gets
/// demoted from probe to a `check.rs` lint (still surfaces, but as a
/// lint diagnostic, not a probe finding).
fn construct_stored_field_never_written(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Severity;

    fn dummy_finding(category: Category, tag: &str) -> Finding {
        Finding {
            id: "deadbeef".to_string(),
            category,
            severity: Severity::High,
            handler: "test_handler".to_string(),
            spec_silent_on: "test".to_string(),
            suppression_hint: "test".to_string(),
            investigation_hint: "test".to_string(),
            category_tag: tag.to_string(),
            reproducer: None,
        }
    }

    /// During the v2.16 retrofit window, every constructor returns
    /// `NotImplemented` and the probe emits zero findings. This test
    /// pins that contract — when a category retrofits, this test gets
    /// updated to assert a real reproducer is constructed.
    #[test]
    fn all_constructors_stub_during_retrofit() {
        let categories = [
            (Category::MissingSigner, "missing_signer"),
            (Category::ArbitraryCpi, "arbitrary_cpi"),
            (
                Category::ArithmeticOverflowWrapping,
                "arithmetic_overflow_wrapping",
            ),
            (
                Category::LifecycleOneShotViolation,
                "lifecycle_one_shot_violation",
            ),
            (Category::UnboundedAmountParam, "unbounded_amount_param"),
            (
                Category::PermissionlessStateWriter,
                "permissionless_state_writer",
            ),
            (Category::InitWithoutPda, "init_without_pda"),
            (
                Category::StoredFieldNeverWritten,
                "stored_field_never_written",
            ),
        ];
        for (cat, tag) in categories {
            let f = dummy_finding(cat, tag);
            let spec = ParsedSpec::default();
            let spec_path = Path::new("test.qedspec");
            let ctx = ReproducerContext::from_spec_path(&spec, spec_path);
            let result = construct_reproducer(&f, &ctx);
            assert!(
                matches!(result, Err(ConstructFailure::NotImplemented)),
                "category {:?} should be NotImplemented during retrofit, got {:?}",
                f.category,
                result
            );
        }
    }

    #[test]
    fn repro_dir_matches_convention() {
        let spec = ParsedSpec::default();
        let spec_path = Path::new("/tmp/foo/program.qedspec");
        let ctx = ReproducerContext::from_spec_path(&spec, spec_path);
        let dir = ctx.repro_dir("abc12345");
        assert_eq!(dir, PathBuf::from("/tmp/foo/target/qedgen-repros/abc12345"));
    }
}
