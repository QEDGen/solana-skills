//! `qedgen probe` reproducer construction pipeline.
//!
//! Every emitted `Finding` must carry a concrete `Reproducer` (Kani trace,
//! proptest seed, or sandbox tx) — the reproducer-only contract on
//! `findings[]` still holds. What changed in v3 (#227): a predicate hit
//! whose reproducer can't be constructed is no longer *silently dropped* —
//! `run_probe` demotes it to an investigation `Candidate` (carrying
//! [`describe_failure`]'s reason), so a spec with live predicate hits is
//! never indistinguishable from a clean one. Candidates make no
//! exploitability claim, so the "no advisory tier" rule on `findings[]` is
//! preserved. Artifacts live under `target/qedgen-repros/<finding.id>/` —
//! ephemeral, never committed.
//!
//! All constructors are currently stubs (`NotImplemented`), so today every
//! predicate hit surfaces as a candidate. The reproducer vertical slice
//! (#228) lands real constructors category by category; the auditor SKILL
//! writes Mollusk repros directly in the meantime.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::check::ParsedSpec;
use crate::probe::{Category, Finding, Reproducer};

/// Per-finding symbolic-execution budget: caps a typical 10-20-finding
/// run at ~10-20 min wall clock — thorough enough for Kani, fast enough
/// for CI.
pub const DEFAULT_KANI_BUDGET: Duration = Duration::from_secs(60);

/// Why a candidate failed to acquire a reproducer. Every variant drops
/// the candidate — none surfaces as "advisory" or "possibly vulnerable."
#[derive(Debug, Clone)]
#[allow(dead_code)] // Variants populated as categories retrofit
pub enum ConstructFailure {
    /// Category constructor not yet implemented.
    NotImplemented,
    /// Kani ran but exhausted the budget without a counterexample. The
    /// bug may still exist — no evidence, so drop.
    KaniTimeout { budget: Duration },
    /// Kani exhausted its search depth within budget: either a predicate
    /// false-positive or insufficient BMC depth. Either way: no
    /// reproducer, no finding.
    KaniNoCounterexample,
    /// No proptest seed constructible — typically the spec slice lacks a
    /// closed input shape we can drive.
    ProptestNoFailure,
    /// Harness / test / sandbox-tx build failed. Build flakiness is not
    /// the user's problem.
    BuildError(String),
    /// I/O writing artifacts — drop rather than emit half-written
    /// artifacts.
    Io(String),
}

/// Human-readable `reason` for a candidate demoted from a finding — the
/// text that travels in `Candidate.reason` so a consumer can tell "no
/// constructor yet" from "Kani found nothing" from "the build broke".
pub fn describe_failure(failure: &ConstructFailure) -> String {
    match failure {
        ConstructFailure::NotImplemented => {
            "no reproducer constructor implemented for this category yet".to_string()
        }
        ConstructFailure::KaniTimeout { budget } => {
            format!(
                "Kani exhausted its {}s budget without a counterexample",
                budget.as_secs()
            )
        }
        ConstructFailure::KaniNoCounterexample => {
            "Kani found no counterexample within its search depth".to_string()
        }
        ConstructFailure::ProptestNoFailure => {
            "no proptest seed reproduced the predicate".to_string()
        }
        ConstructFailure::BuildError(e) => format!("reproducer build failed: {e}"),
        ConstructFailure::Io(e) => format!("reproducer I/O error: {e}"),
    }
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
    /// Project root = the spec's directory — where `target/qedgen-repros/`
    /// lives.
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

    /// `<project_root>/target/qedgen-repros/<finding_id>/`. Repros are
    /// ephemeral (regenerated every run) so they live under `target/`
    /// (cargo-ignored), not committed `.qed/`.
    #[allow(dead_code)] // Used by category constructors as they retrofit
    pub fn repro_dir(&self, finding_id: &str) -> PathBuf {
        self.project_root
            .join("target")
            .join("qedgen-repros")
            .join(finding_id)
    }
}

/// Route a candidate finding to its per-category constructor.
/// `Err(ConstructFailure)` tells the caller to drop the finding.
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
        Category::CrucibleFuzzCrash => {
            // Crucible findings get their Reproducer::Crucible attached in
            // `crucible_probe.rs`; reaching this dispatcher means the
            // upstream pipeline failed to attach it — drop.
            Err(ConstructFailure::NotImplemented)
        }
        // Pinocchio + arithmetic-symbol categories attach MolluskPrompt /
        // MiriPrompt reproducers at site-discovery time
        // (pinocchio_probe.rs / arithmetic_symbol_probe.rs); flowing
        // through this dispatcher is out-of-band — drop.
        Category::PinocchioUncheckedAccountLoad
        | Category::PinocchioUncheckedArith
        | Category::PinocchioAccountTypeConfusion
        | Category::PinocchioMutableBorrowAliasing
        | Category::PinocchioPositionWithoutTypeTag
        | Category::PinocchioOffsetOverrun
        | Category::PinocchioMissingPdaVerification
        | Category::PinocchioStaleSafetyComment
        | Category::ExecutionDivergence
        | Category::SilentSuccessArithmetic
        | Category::GracefulErrorAsDos
        | Category::UncheckedArithWithFundFlow
        | Category::PairedValidatorInputDomainMismatch
        | Category::ExternalAuthorityNotRevokedOnClose => Err(ConstructFailure::NotImplemented),
    }
}

// ---------------------------------------------------------------------------
// Per-category constructors, all stubbed to `NotImplemented`. When built,
// reproducers are Mollusk-driven sandbox txs that invoke the user's real
// handler with attack inputs and observe state corruption — not synthesized
// witness tests against the operator alone.
// ---------------------------------------------------------------------------

/// Mollusk sandbox tx: drive overflow-triggering params (e.g. `u64::MAX`
/// into a `+=?` field), observe the wrap propagated to post-state.
fn construct_arithmetic_overflow_wrapping(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Kani harness: drive the handler with the declared type's saturated
/// value; assert overflow / drain.
fn construct_unbounded_amount_param(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Proptest seed: invoke in an unintended lifecycle state, assert effects
/// fired anyway.
fn construct_lifecycle_one_shot_violation(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Sandbox tx: invoke from an unauthorized signer (litesvm); observe the
/// state change occurs without auth.
fn construct_missing_signer(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// May need spec-less mode: the spec doesn't carry the impl's CPI list,
/// so a spec-only repro is structurally insufficient.
fn construct_arbitrary_cpi(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Sandbox tx: two concurrent unauthorized calls observe shared-state
/// corruption.
fn construct_permissionless_state_writer(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Sandbox tx: two callers race the same canonical address; observe state
/// collision.
fn construct_init_without_pda(
    _finding: &Finding,
    _ctx: &ReproducerContext,
) -> Result<Reproducer, ConstructFailure> {
    Err(ConstructFailure::NotImplemented)
}

/// Riskiest: zero-init reachability may not be constructible from the
/// spec alone — if so, demote the category from probe to a `check.rs`
/// lint.
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
            gated_by: None,
        }
    }

    /// Categories whose constructors are still stubbed report
    /// `NotImplemented` — the caller demotes them to candidates rather than
    /// dropping them. As #228 lands real constructors, entries move OUT of
    /// this list (and gain their own positive/negative tests); the list
    /// shrinking is the retrofit's progress bar.
    #[test]
    fn stubbed_constructors_report_not_implemented() {
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

    /// `describe_failure` renders a distinct, human reason per variant so a
    /// candidate's `reason` tells "not built yet" apart from "proved
    /// nothing" apart from "build broke".
    #[test]
    fn describe_failure_is_distinct_per_variant() {
        let reasons = [
            describe_failure(&ConstructFailure::NotImplemented),
            describe_failure(&ConstructFailure::KaniNoCounterexample),
            describe_failure(&ConstructFailure::ProptestNoFailure),
            describe_failure(&ConstructFailure::BuildError("boom".into())),
        ];
        let unique: std::collections::HashSet<_> = reasons.iter().collect();
        assert_eq!(
            unique.len(),
            reasons.len(),
            "reasons must be distinguishable"
        );
        assert!(reasons[3].contains("boom"), "build error must carry detail");
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
