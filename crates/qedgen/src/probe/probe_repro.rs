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
//! The reproducer vertical slice (#228) lands real reproducers category by
//! category. The first is `ArithmeticOverflowWrapping`, handled by
//! [`build_arith_overflow_harness`] (a generated boundary program, run only
//! under `--execute-repros`) rather than [`construct_reproducer`], whose
//! per-category constructors are still stubs (`NotImplemented`) — those
//! categories surface as candidates until their reproducer lands. The auditor
//! SKILL writes Mollusk repros directly in the meantime.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::check::{ParsedEffect, ParsedSpec};
use crate::probe::{Category, Finding, ReproHarness, Reproducer};
use crate::repro_gen;

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
        // #235: drift is born as a candidate in `idl_overlay` — a
        // deterministic cross-check has no runnable reproducer, so it never
        // flows through this dispatcher.
        Category::IdlSourceDrift => Err(ConstructFailure::NotImplemented),
        // #240: an unwired error variant is an *absence* (a guard never
        // called) — born as a candidate in `dead_guard_probe`, no reproducer.
        Category::UnwiredErrorVariant => Err(ConstructFailure::NotImplemented),
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

// ---------------------------------------------------------------------------
// ArithmeticOverflowWrapping — the first category with a real, executable
// reproducer (#228). Generation is mechanical (codegen::repro_gen);
// execution is opt-in.
// ---------------------------------------------------------------------------

/// A generated boundary reproducer for a wrapping arithmetic effect: source,
/// where it lives, and how to build + run it.
pub struct ArithHarness {
    /// Path relative to the project root (goes into the JSON envelope).
    pub rel_path: String,
    /// Absolute path where the source is written.
    pub abs_path: PathBuf,
    /// The generated Rust source.
    pub source: String,
    /// Exact build-and-run command; exits 0 iff the wrap reproduces.
    pub invocation: String,
    /// The concrete boundary input the harness drives.
    pub failing_input: String,
}

impl ArithHarness {
    /// The candidate-facing pointer (default path — harness generated, not run).
    pub fn as_repro_harness(&self) -> ReproHarness {
        ReproHarness {
            path: self.rel_path.clone(),
            invocation: self.invocation.clone(),
            kind: "boundary_value".to_string(),
            failing_input: self.failing_input.clone(),
        }
    }

    /// The confirmed-finding reproducer (after a successful `--execute-repros`).
    pub fn as_reproducer(&self) -> Reproducer {
        Reproducer::BoundaryValue {
            harness_path: self.rel_path.clone(),
            invocation: self.invocation.clone(),
            failing_input: self.failing_input.clone(),
        }
    }
}

/// Outcome of building + running a reproducer harness under `--execute-repros`.
pub enum ExecOutcome {
    /// Harness exited 0 — the violation reproduces. Promote to a finding.
    Reproduced,
    /// Harness ran but did not reproduce (exit non-zero from an assert).
    /// Keep as a candidate — we could not confirm.
    NotReproduced,
    /// Could not build or spawn (rustc missing, compile error, timeout).
    /// Not the user's problem; keep as a candidate + engine note.
    BuildError(String),
}

/// Locate the wrapping effect that produced an `ArithmeticOverflowWrapping`
/// finding, by re-deriving the predicate's stable id. Robust — no parsing of
/// human-readable finding text. Returns the effect and its integer type; only
/// wrapping ops (`add_wrap` / `sub_wrap`) are harnessable here (saturating
/// clamps rather than silently overflowing, so it stays an un-harnessed
/// candidate).
fn wrapping_effect_for_finding<'a>(
    spec: &'a ParsedSpec,
    finding: &Finding,
) -> Option<(&'a ParsedEffect, &'static str)> {
    let handler = spec.handlers.iter().find(|h| h.name == finding.handler)?;
    let tag = Category::ArithmeticOverflowWrapping.tag();
    for eff in &handler.effects {
        if !matches!(eff.op.as_str(), "add_wrap" | "sub_wrap") {
            continue;
        }
        let id = crate::probe::spec_predicates::stable_id(
            &format!("{}::{}::{}", handler.name, eff.field, eff.op),
            tag,
        );
        if id == finding.id {
            let rust_ty = repro_gen::rust_int_type(&resolve_field_dsl_type(spec, &eff.field))
                // Amount fields are integer by construction; default to u64
                // when the declared type can't be resolved from the spec.
                .unwrap_or("u64");
            return Some((eff, rust_ty));
        }
    }
    None
}

/// Resolve a field path's DSL type (`U64`, …) from the spec's account/state
/// field declarations. Strips `Variant.` prefixes and `[i]` subscripts to the
/// base identifier. Returns `"U64"` when unresolved (the dominant amount type).
fn resolve_field_dsl_type(spec: &ParsedSpec, field_path: &str) -> String {
    let base = field_path
        .rsplit('.')
        .next()
        .unwrap_or(field_path)
        .split('[')
        .next()
        .unwrap_or(field_path)
        .trim();
    for at in &spec.account_types {
        for (name, ty) in &at.fields {
            if name == base {
                return ty.clone();
            }
        }
    }
    "U64".to_string()
}

/// Build (but do not write) the boundary reproducer for a wrapping-arithmetic
/// finding. `None` when the finding isn't a harnessable wrapping op.
pub fn build_arith_overflow_harness(
    spec: &ParsedSpec,
    finding: &Finding,
    ctx: &ReproducerContext,
) -> Option<ArithHarness> {
    let (eff, rust_ty) = wrapping_effect_for_finding(spec, finding)?;
    let operand_desc = if eff.value.is_empty() {
        "operand".to_string()
    } else {
        eff.value.clone()
    };
    let generated = repro_gen::arith_overflow_boundary(
        &finding.handler,
        &eff.field,
        &eff.op,
        &operand_desc,
        rust_ty,
    )?;

    let rel_dir = format!(
        "target/qedgen-repros/{}/{}",
        Category::ArithmeticOverflowWrapping.tag(),
        finding.handler
    );
    let abs_dir = ctx.project_root.join(&rel_dir);
    let rel_path = format!("{rel_dir}/repro.rs");
    let abs_path = abs_dir.join("repro.rs");
    let bin_rel = format!("{rel_dir}/repro");
    let invocation = format!("rustc -O --edition 2021 {rel_path} -o {bin_rel} && ./{bin_rel}");

    Some(ArithHarness {
        rel_path,
        abs_path,
        source: generated.source,
        invocation,
        failing_input: generated.failing_input,
    })
}

/// Write the harness source to disk (creating parent dirs).
pub fn write_harness(harness: &ArithHarness) -> Result<(), ConstructFailure> {
    if let Some(parent) = harness.abs_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConstructFailure::Io(e.to_string()))?;
    }
    std::fs::write(&harness.abs_path, &harness.source)
        .map_err(|e| ConstructFailure::Io(e.to_string()))
}

/// Build + run the harness with `rustc`, under a wall-clock budget. Exit 0 ⇒
/// reproduced. `rustc` is a soft dependency (like rustfmt); its absence
/// surfaces as `BuildError`, never a panic.
pub fn execute_harness(harness: &ArithHarness, _budget: Duration) -> ExecOutcome {
    use std::process::Command;
    let bin_path = harness.abs_path.with_file_name("repro");
    let compile = Command::new("rustc")
        .arg("-O")
        .arg("--edition")
        .arg("2021")
        .arg(&harness.abs_path)
        .arg("-o")
        .arg(&bin_path)
        .output();
    match compile {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            return ExecOutcome::BuildError(format!(
                "rustc failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Err(e) => return ExecOutcome::BuildError(format!("could not run rustc: {e}")),
    }
    match Command::new(&bin_path).output() {
        Ok(out) if out.status.success() => ExecOutcome::Reproduced,
        Ok(_) => ExecOutcome::NotReproduced,
        Err(e) => ExecOutcome::BuildError(format!("could not run harness: {e}")),
    }
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
    /// dropping them. As #228 lands real reproducers, entries move OUT of
    /// this list (and gain their own positive/negative tests); the list
    /// shrinking is the retrofit's progress bar. `ArithmeticOverflowWrapping`
    /// has left the list — it is handled by `build_arith_overflow_harness`
    /// (tested below and in `tests/probe_cli.rs`), not `construct_reproducer`.
    #[test]
    fn stubbed_constructors_report_not_implemented() {
        let categories = [
            (Category::MissingSigner, "missing_signer"),
            (Category::ArbitraryCpi, "arbitrary_cpi"),
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

    fn spec_with_field(field: &str, ty: &str) -> ParsedSpec {
        use crate::check::ParsedAccountType;
        ParsedSpec {
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![(field.to_string(), ty.to_string())],
                lifecycle: Vec::new(),
                pda_ref: None,
                variants: Vec::new(),
            }],
            ..ParsedSpec::default()
        }
    }

    #[test]
    fn resolve_field_type_reads_account_declaration() {
        let spec = spec_with_field("balance", "U128");
        assert_eq!(resolve_field_dsl_type(&spec, "balance"), "U128");
        // Strips `Variant.` prefix and `[i]` subscripts to the base ident.
        assert_eq!(resolve_field_dsl_type(&spec, "Active.balance"), "U128");
        assert_eq!(resolve_field_dsl_type(&spec, "accounts[i].balance"), "U128");
    }

    #[test]
    fn resolve_field_type_defaults_to_u64_when_unknown() {
        let spec = spec_with_field("balance", "U64");
        assert_eq!(resolve_field_dsl_type(&spec, "not_a_field"), "U64");
    }
}
