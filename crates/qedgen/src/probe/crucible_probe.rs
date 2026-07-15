//! Crucible-as-probe-engine: coverage-guided fuzzing of the deployed `.so`,
//! converting each crash into a `Finding` with `Reproducer::Crucible` — the
//! same surface the static pattern-match engine in `probe.rs` emits into.
//!
//! Pipeline: IDL discovery → build → smoke pre-flight (stops early when smoke
//! already surfaces findings; re-finding the same bug class burns budget) →
//! full run → bounded tmin/replay sampling → categorize → dedupe by
//! `(handler, dedupe_key)`.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::crucible_gen::InvariantMode;
use crate::probe::{Category, CrucibleCrashMetadata, Finding, Reproducer, Severity};

const DOMAIN_FACT_ARRAYS: &[&str] = &[
    "asset_flows",
    "quantities",
    "paired_operations",
    "lifecycle_edges",
    "authority_capabilities",
    "economic_equations",
    "external_assumptions",
];

pub const DOMAIN_REPLAY_REPORT_SCHEMA_URI: &str =
    "https://qedgen.dev/schemas/auditor/domain-replay-report-v1.schema.json";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DomainReplayReport {
    pub schema_version: u32,
    pub schema_uri: String,
    pub resolved_document_sha256: String,
    pub account_binding_overlay_sha256: String,
    pub harness_sha256: String,
    pub records: Vec<DomainReplayRecord>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DomainReplayRecord {
    pub plan_id: String,
    pub seed_path: String,
    pub seed_sha256: String,
    pub action_count: usize,
    pub command: Vec<String>,
    pub status: DomainReplayStatus,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainReplayStatus {
    CompletedZeroExit,
    CompletedNonzeroExit,
    /// The replay child was killed by a signal (SIGSEGV/SIGABRT/OOM), so it
    /// has no numeric exit code. This is a REPRODUCED crash — exactly what a
    /// fuzzer cares about — not a spawn failure.
    TerminatedBySignal,
    SpawnFailed,
}

/// Validate the domain handoff before Crucible claims semantic coverage.
/// Only facts explicitly assigned to the Crucible lane participate. Every
/// participating fact must be ratified, and an empty domain lane is blocked.
/// If an adjacent run manifest exists, failures are persisted there so an
/// audit remains resumable even though the fuzz process never starts.
pub fn require_ratified_domain_facts(dossier_path: &Path, spec_path: &Path) -> Result<usize> {
    let result = inspect_ratified_domain_facts(dossier_path);
    if let Err(error) = &result {
        let _ = mark_domain_lane_blocked(dossier_path, spec_path, &error.to_string());
    }
    result
}

/// Refuse a domain run whose spec compiles to no executable Crucible
/// assertions. Ratified prose is valuable audit evidence, but it is not fuzz
/// coverage until represented by a Rust-renderable invariant or property.
pub fn require_executable_domain_assertions(
    dossier_path: &Path,
    spec_path: &Path,
    assertion_count: usize,
) -> Result<()> {
    if assertion_count > 0 {
        return Ok(());
    }
    let reason = "domain spec has no Rust-renderable linked invariants or properties; author executable assertions before domain fuzzing";
    let _ = mark_domain_lane_blocked(dossier_path, spec_path, reason);
    bail!(reason)
}

/// Clear this lane's previous readiness block once both the dossier and spec
/// gates pass. This records readiness, not a successful fuzz result.
pub fn mark_domain_lane_ready(dossier_path: &Path) -> Result<()> {
    let Some(parent) = dossier_path.parent() else {
        return Ok(());
    };
    let manifest_path = parent.join("run-manifest.json");
    if !manifest_path.exists() {
        return Ok(());
    }
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
    let lanes = manifest
        .get_mut("lanes")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("run manifest has no lanes array"))?;
    if let Some(lane) = lanes.iter_mut().find(|lane| {
        lane.get("name").and_then(serde_json::Value::as_str) == Some("crucible-domain")
    }) {
        lane["status"] = serde_json::Value::String("queued".to_string());
        lane["reason"] = serde_json::Value::Null;
        lane["resume_command"] = serde_json::Value::Null;
    } else {
        lanes.push(serde_json::json!({
            "name": "crucible-domain",
            "status": "queued",
            "reason": null,
            "resume_command": null,
            "started_at": null,
            "finished_at": null,
            "artifact_paths": [dossier_path.display().to_string()]
        }));
    }
    let any_blocked = lanes.iter().any(|lane| lane["status"] == "blocked");
    if !any_blocked && manifest["status"] == "tooling-blocked" {
        manifest["status"] = serde_json::Value::String("running".to_string());
    }
    std::fs::write(
        &manifest_path,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(())
}

fn inspect_ratified_domain_facts(dossier_path: &Path) -> Result<usize> {
    let raw = std::fs::read_to_string(dossier_path)
        .with_context(|| format!("reading domain dossier {}", dossier_path.display()))?;
    let dossier: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing domain dossier {}", dossier_path.display()))?;
    if dossier
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || dossier
            .get("schema_uri")
            .and_then(serde_json::Value::as_str)
            != Some("https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json")
    {
        bail!(
            "domain mode requires a canonical schema-v1 domain dossier; validate it with `check-domain-artifacts.sh --dossier {}`",
            dossier_path.display()
        );
    }

    let mut eligible = 0usize;
    let mut pending = Vec::new();
    for array_name in DOMAIN_FACT_ARRAYS {
        let facts = dossier
            .get(array_name)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("domain dossier is missing `{array_name}` array"))?;
        for fact in facts {
            let metadata = fact.get("metadata").ok_or_else(|| {
                anyhow::anyhow!("domain fact in `{array_name}` is missing metadata")
            })?;
            let targets_crucible = metadata
                .get("verification_lanes")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|lanes| lanes.iter().any(|lane| lane.as_str() == Some("crucible")));
            if !targets_crucible {
                continue;
            }
            eligible += 1;
            let ratification = metadata
                .get("ratification")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("missing");
            if !matches!(ratification, "auto" | "user") {
                pending.push(
                    fact.get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("<missing-id>")
                        .to_string(),
                );
            }
        }
    }

    if eligible == 0 {
        bail!(
            "domain dossier has no facts assigned to the Crucible verification lane; ratify domain intent and add `crucible` to the selected facts' verification_lanes"
        );
    }
    if !pending.is_empty() {
        bail!(
            "domain dossier has unratified Crucible facts: {}; resolve each to `auto` or `user` before domain fuzzing",
            pending.join(", ")
        );
    }
    Ok(eligible)
}

fn mark_domain_lane_blocked(dossier_path: &Path, spec_path: &Path, reason: &str) -> Result<()> {
    let Some(parent) = dossier_path.parent() else {
        return Ok(());
    };
    let manifest_path = parent.join("run-manifest.json");
    if !manifest_path.exists() {
        return Ok(());
    }
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
    manifest["status"] = serde_json::Value::String("tooling-blocked".to_string());
    let lanes = manifest
        .get_mut("lanes")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("run manifest has no lanes array"))?;
    let resume = format!(
        "qedgen probe --fuzz 300 --crucible-mode domain --spec {} --domain-dossier {}",
        spec_path.display(),
        dossier_path.display()
    );
    let lane = lanes.iter_mut().find(|lane| {
        lane.get("name").and_then(serde_json::Value::as_str) == Some("crucible-domain")
    });
    let blocked = serde_json::json!({
        "name": "crucible-domain",
        "status": "blocked",
        "reason": reason,
        "resume_command": resume,
        "started_at": null,
        "finished_at": null,
        "artifact_paths": [dossier_path.display().to_string()]
    });
    if let Some(lane) = lane {
        *lane = blocked;
    } else {
        lanes.push(blocked);
    }
    std::fs::write(
        &manifest_path,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(())
}

/// `crucible tmin --all` cap — keeps minimization from eating the user's
/// budget after a productive fuzz run.
pub const TMIN_BUDGET_PER_CRASH: Duration = Duration::from_secs(30);

/// Smoke pre-flight budget: long enough to dispatch a few actions,
/// short enough that broken harnesses fail fast.
pub const SMOKE_BUDGET: Duration = Duration::from_secs(30);

/// Stop after smoke if it surfaces this many distinct findings
/// (post-dedupe) — burning the full budget to re-find the same bug
/// class is anti-quality.
pub const SMOKE_FINDING_CAP: usize = 4;

/// Hard ceiling on replay work after one fuzz round. Crash storms commonly
/// contain thousands of value variants for the same action shape; replaying
/// every file would defeat the user-requested fuzz budget.
pub const MAX_CRASH_REPLAYS: usize = 32;

/// Preserve a small deterministic sample of value variants per action shape.
/// This retains a chance to surface parameter-dependent invariant markers
/// without allowing one hot shape to consume the entire replay allowance.
pub const MAX_REPLAYS_PER_CRASH_SHAPE: usize = 4;

/// Replay classification is post-fuzz evidence work, but it must remain
/// bounded. Long fuzz requests do not grant an unbounded triage tail.
pub const MAX_REPLAY_TRIAGE_BUDGET: Duration = Duration::from_secs(30);

/// One malformed replay must not consume the whole triage allowance.
pub const MAX_REPLAY_PER_CRASH: Duration = Duration::from_secs(2);

/// Default full-run budget for `--fuzz` without an explicit value
/// (~a few k iterations for a small/medium harness on a laptop).
pub const DEFAULT_FUZZ_BUDGET: Duration = Duration::from_secs(300);

/// Test-fn name emitted by `crucible_gen::emit_invariant_fn`; doubles as
/// the cargo feature gate and the `crucible run` subcommand argument.
const HARNESS_TEST_NAME: &str = "invariant_test";

/// Inputs for one fuzz-probe run.
pub struct FuzzProbeContext<'a> {
    /// Held for future per-finding enrichment (e.g. linking back to the
    /// declared invariant name).
    #[allow(dead_code)]
    pub spec_path: &'a Path,
    /// Repo root — `target/idl/` lives here.
    pub project_root: PathBuf,
    /// Harness directory (`fuzz/<prog>/`) from `qedgen codegen --crucible`.
    pub harness_dir: PathBuf,
    /// Whole-set tmin cap; defaults to TMIN_BUDGET_PER_CRASH.
    pub tmin_cap: Duration,
    /// Smoke pre-flight budget; defaults to SMOKE_BUDGET. Duration::ZERO
    /// skips smoke (`--no-smoke`).
    pub smoke_budget: Duration,
    /// Full-run budget after smoke.
    pub fuzz_budget: Duration,
    /// `--stateful` is a runtime switch — the same harness serves both modes.
    pub stateful: bool,
    /// Invariant family the harness was built against — lets triage label
    /// protocol-only crashes distinctly from spec violations.
    pub invariant_mode: InvariantMode,
    /// Optional byte-exact corpus synthesized from explicitly bound domain
    /// sequences. Each seed is replayed once and written to a durable evidence
    /// report before the same corpus is used for exploratory fuzzing.
    pub domain_seed_report: Option<super::domain_sequence_seed::DomainSeedReport>,
}

impl<'a> FuzzProbeContext<'a> {
    /// Convenience: budget-only constructor with sane defaults.
    pub fn new(spec_path: &'a Path, project_root: PathBuf, harness_dir: PathBuf) -> Self {
        Self {
            spec_path,
            project_root,
            harness_dir,
            tmin_cap: TMIN_BUDGET_PER_CRASH,
            smoke_budget: SMOKE_BUDGET,
            fuzz_budget: DEFAULT_FUZZ_BUDGET,
            stateful: false,
            invariant_mode: InvariantMode::Spec,
            domain_seed_report: None,
        }
    }
}

/// Top-level entry: discovery → build → smoke → run → triage → dedupe.
/// Requires `crucible` on PATH and an existing harness directory
/// (`qedgen codegen --crucible` first).
/// Findings from a fuzz probe plus the replay-triage summary (#229).
pub struct FuzzProbeResult {
    pub findings: Vec<Finding>,
    /// Whether minimized crashes replayed successfully: `None` when there were
    /// no crashes to replay; `Some(true)` when every replayed crash
    /// reproduced; `Some(false)` when at least one did not (so some crashes
    /// were dropped as non-reproducing). Feeds `ProbeCoverage.replay_success`.
    pub replay_success: Option<bool>,
}

pub fn run_fuzz_probe(ctx: &FuzzProbeContext) -> Result<FuzzProbeResult> {
    crate::deps::require_crucible()?;
    if !ctx.harness_dir.exists() {
        bail!(
            "Crucible harness not found at {}. Run `qedgen codegen --crucible` first.",
            ctx.harness_dir.display()
        );
    }

    discover_idl(&ctx.harness_dir, &ctx.project_root)
        .context("auto-discovering Anchor IDL into harness")?;

    build_harness(&ctx.harness_dir).context("building Crucible harness")?;

    let mut acc = HarvestResult::default();

    if let Some(seed_report) = &ctx.domain_seed_report {
        replay_domain_seed_report(&ctx.harness_dir, seed_report)?;
        acc.merge(harvest_crucible_findings(
            ctx,
            replay_triage_budget(ctx.fuzz_budget),
        )?);
    }

    if !ctx.smoke_budget.is_zero() {
        acc.merge(
            run_crucible_round(ctx, ctx.smoke_budget, "smoke")
                .context("running Crucible smoke pre-flight")?,
        );
        if dedupe_findings(acc.findings.clone()).len() >= SMOKE_FINDING_CAP {
            eprintln!(
                "Smoke surfaced {} distinct findings — stopping early. Fix these before re-running with the full budget (or pass --no-smoke to bypass).",
                acc.findings.len()
            );
            return Ok(finalize_fuzz_result(acc));
        }
    }

    acc.merge(
        run_crucible_round(ctx, ctx.fuzz_budget, HARNESS_TEST_NAME)
            .context("running Crucible full fuzz")?,
    );

    Ok(finalize_fuzz_result(acc))
}

/// Dedupe findings and derive the coarse `replay_success` signal.
fn finalize_fuzz_result(acc: HarvestResult) -> FuzzProbeResult {
    let replay_success = if acc.crashes_discovered == 0 {
        None
    } else {
        Some(
            acc.crashes_seen == acc.crashes_discovered
                && acc.crashes_reproduced == acc.crashes_seen,
        )
    };
    FuzzProbeResult {
        findings: dedupe_findings(acc.findings),
        replay_success,
    }
}

/// One round: fuzz → harvest crashes → tmin → replay-classify. Smoke and
/// full passes differ only by budget.
fn run_crucible_round(
    ctx: &FuzzProbeContext,
    budget: Duration,
    label: &str,
) -> Result<HarvestResult> {
    let crash_dir = run_crucible(
        &ctx.harness_dir,
        budget,
        ctx.stateful,
        ctx.domain_seed_report
            .as_ref()
            .map(|report| report.corpus_dir.as_path()),
    )
    .with_context(|| format!("crucible run ({label}) failed"))?;
    harvest_crucible_findings_from(ctx, &crash_dir, replay_triage_budget(budget))
}

fn replay_triage_budget(fuzz_budget: Duration) -> Duration {
    fuzz_budget
        .max(Duration::from_secs(1))
        .min(MAX_REPLAY_TRIAGE_BUDGET)
}

fn harvest_crucible_findings(
    ctx: &FuzzProbeContext,
    replay_budget: Duration,
) -> Result<HarvestResult> {
    harvest_crucible_findings_from(
        ctx,
        &ctx.harness_dir.join("crashes").join(HARNESS_TEST_NAME),
        replay_budget,
    )
}

/// Findings plus replay accounting from harvesting one crash directory.
#[derive(Default)]
struct HarvestResult {
    findings: Vec<Finding>,
    /// Parseable crashes discovered before bounded sampling.
    crashes_discovered: usize,
    /// Crashes we attempted to replay for classification.
    crashes_seen: usize,
    /// Of those, how many reproduced under replay (`reproduces:true`).
    crashes_reproduced: usize,
}

impl HarvestResult {
    fn merge(&mut self, other: HarvestResult) {
        self.findings.extend(other.findings);
        self.crashes_discovered += other.crashes_discovered;
        self.crashes_seen += other.crashes_seen;
        self.crashes_reproduced += other.crashes_reproduced;
    }
}

fn harvest_crucible_findings_from(
    ctx: &FuzzProbeContext,
    crash_dir: &Path,
    replay_budget: Duration,
) -> Result<HarvestResult> {
    let mut crashes = load_crash_candidates(crash_dir)?;
    let discovered = crashes.len();
    let started = Instant::now();
    if discovered <= MAX_CRASH_REPLAYS && !crashes.is_empty() {
        // tmin failure is non-fatal — raw crashes are still valid
        // reproducers, we just lose minimization.
        // Reserve at least half the triage allowance for the evidence-bearing
        // replay step; minimization is useful but non-authoritative.
        let tmin_budget = ctx.tmin_cap.min(replay_budget / 2);
        let _ = auto_tmin_all(&ctx.harness_dir, tmin_budget);
        // tmin may rewrite metadata in place; classification must use the
        // minimized evidence rather than the pre-minimization snapshot.
        crashes = load_crash_candidates(crash_dir)?;
    } else if discovered > MAX_CRASH_REPLAYS {
        eprintln!(
            "[crucible] {discovered} crashes exceed the bounded replay threshold; skipping `tmin --all` and sampling at most {MAX_CRASH_REPLAYS}."
        );
    }
    let crashes = select_crashes_for_replay(crashes);
    let mut result = HarvestResult {
        crashes_discovered: discovered,
        ..HarvestResult::default()
    };
    for CrashCandidate { path: crash, meta } in crashes {
        let remaining = replay_budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            eprintln!(
                "[crucible] replay triage exhausted its {}s budget after {}/{} parseable crashes.",
                replay_budget.as_secs(),
                result.crashes_seen,
                discovered
            );
            break;
        }
        result.crashes_seen += 1;
        // #229: classify from replay evidence, not the last-action heuristic.
        match replay_capture(
            &ctx.harness_dir,
            &crash,
            remaining.min(MAX_REPLAY_PER_CRASH),
        ) {
            Some(ev) if !ev.reproduces => {
                // The recorded crash did not re-fire on replay — no
                // reproducible evidence, so it must NOT become a finding.
                // Demote to a visible note rather than silently dropping.
                eprintln!(
                    "[crucible] crash {} did not reproduce on replay — dropped (not a finding).",
                    ev.crash_id
                );
            }
            Some(ev) => {
                result.crashes_reproduced += 1;
                let class = crate::probe::crucible_replay::classify_reproduced(&ev.summary);
                result.findings.push(finding_from_crash(
                    &ctx.harness_dir,
                    &crash,
                    &meta,
                    Some(&class),
                )?);
            }
            None => {
                // Replay unavailable or marker unparseable — fall back to the
                // heuristic so we don't lose the crash, but the finding text
                // flags that its class is not replay-confirmed.
                eprintln!(
                    "[crucible] replay produced no parseable marker for {} — using heuristic classification.",
                    crash.display()
                );
                result
                    .findings
                    .push(finding_from_crash(&ctx.harness_dir, &crash, &meta, None)?);
            }
        }
    }
    if result.crashes_seen < discovered {
        eprintln!(
            "[crucible] bounded replay classified {}/{} parseable crashes; coverage.replay_success is false until every crash is replayed.",
            result.crashes_seen, discovered
        );
    }
    Ok(result)
}

#[derive(Clone)]
struct CrashCandidate {
    path: PathBuf,
    meta: CrucibleCrashMetadata,
}

type CrashShapeKey = (String, String, u32, Vec<(String, bool, Option<u32>)>);

fn crash_shape_key(meta: &CrucibleCrashMetadata) -> CrashShapeKey {
    let (_, tag) = categorize_crash(meta);
    let error_code = meta
        .actions
        .last()
        .and_then(|action| action.error_code)
        .unwrap_or(0);
    let actions = meta
        .actions
        .iter()
        .map(|action| (action.name.clone(), action.success, action.error_code))
        .collect();
    (
        derive_handler_for_crash(meta),
        tag.to_string(),
        error_code,
        actions,
    )
}

fn load_crash_candidates(crash_dir: &Path) -> Result<Vec<CrashCandidate>> {
    let mut candidates = Vec::new();
    for path in collect_crash_files(crash_dir)? {
        let Ok(raw) = std::fs::read(&path) else {
            continue;
        };
        let Ok(meta) = parse_crash_metadata(&raw) else {
            continue;
        };
        candidates.push(CrashCandidate { path, meta });
    }
    Ok(candidates)
}

/// Deterministically round-robin across distinct action shapes, retaining at
/// most a few value variants per shape and a fixed total number of replays.
fn select_crashes_for_replay(crashes: Vec<CrashCandidate>) -> Vec<CrashCandidate> {
    let mut buckets: BTreeMap<CrashShapeKey, VecDeque<CrashCandidate>> = BTreeMap::new();
    for crash in crashes {
        buckets
            .entry(crash_shape_key(&crash.meta))
            .or_default()
            .push_back(crash);
    }

    let mut selected = Vec::new();
    for _ in 0..MAX_REPLAYS_PER_CRASH_SHAPE {
        for bucket in buckets.values_mut() {
            if selected.len() == MAX_CRASH_REPLAYS {
                return selected;
            }
            if let Some(crash) = bucket.pop_front() {
                selected.push(crash);
            }
        }
    }
    selected
}

// ============================================================================
// Pure helpers — unit-testable without shelling crucible
// ============================================================================

pub fn parse_crash_metadata(json: &[u8]) -> Result<CrucibleCrashMetadata> {
    serde_json::from_slice::<CrucibleCrashMetadata>(json).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse Crucible crash metadata: {e}. \
             This usually means the pinned Crucible version's schema drifted — \
             re-pin and re-run."
        )
    })
}

/// FALLBACK classifier (superseded by replay evidence, #229). Maps crash
/// characteristics to (severity, category_tag) using only the recorded action
/// sequence: "no error code on the last action means the post-action assert
/// fired." This is unreliable — it can't name the invariant and misclassifies
/// any crash whose violating action isn't the last one — which is exactly why
/// the harvest path now replays each crash and classifies from the
/// `[FUZZ_FINDING]` marker (`crucible_replay`). This function is used ONLY when
/// replay is unavailable or its marker can't be parsed, and the resulting
/// finding is flagged as heuristic rather than replay-confirmed.
pub fn categorize_crash(meta: &CrucibleCrashMetadata) -> (Severity, &'static str) {
    let last = meta.actions.last();
    match last {
        Some(a) if !a.success && a.error_code.is_some() => {
            // Anchor error-code abort: genuine bug or spec-silent error
            // path — Medium until we know which.
            (Severity::Medium, "runtime_abort")
        }
        Some(a) if !a.success => {
            // No error code → not a clean Anchor abort. Likely a panic
            // (zero-div, slice out-of-bounds) or runtime fault.
            (Severity::Medium, "runtime_panic")
        }
        _ => {
            // Last action succeeded but a crash was recorded — the
            // post-action `fuzz_assert!` fired. Canonical spec-invariant
            // violation.
            (Severity::High, "invariant_violation")
        }
    }
}

/// Best-effort handler name: the last action's name. Stateful chains may
/// have the bug latent earlier; the full chain stays in `action_sequence`.
pub fn derive_handler_for_crash(meta: &CrucibleCrashMetadata) -> String {
    meta.actions
        .last()
        .map(|a| a.name.trim_start_matches("action_").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Dedupe key: `(handler, category_tag, error_code-or-zero)`. Over-dedupes
/// when one handler trips multiple distinct invariants (no in-band
/// assertion message). The Finding-side path is `finding_dedupe_key`,
/// which reconstructs a synthetic crash from `Reproducer::Crucible`.
/// Kept public for tests + future direct callers.
#[allow(dead_code)]
pub fn dedupe_key_for_crash(meta: &CrucibleCrashMetadata) -> (String, &'static str, u32) {
    let (_, tag) = categorize_crash(meta);
    let last = meta.actions.last();
    let err = last.and_then(|a| a.error_code).unwrap_or(0);
    (derive_handler_for_crash(meta), tag, err)
}

/// Collapse same-class findings: first crash is the canonical reproducer;
/// later crashes contribute their `.meta.json` path to `extra_seeds`.
pub fn dedupe_findings(findings: Vec<Finding>) -> Vec<Finding> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<(String, String, Option<String>, u32), Finding> = BTreeMap::new();
    for f in findings {
        let key = finding_dedupe_key(&f);
        match by_key.get_mut(&key) {
            Some(canonical) => {
                if let Some(extra) = crash_path_from_reproducer(&f) {
                    if let Reproducer::Crucible { extra_seeds, .. } =
                        canonical.reproducer.as_mut().unwrap()
                    {
                        extra_seeds.push(extra);
                    }
                }
            }
            None => {
                by_key.insert(key, f);
            }
        }
    }
    by_key.into_values().collect()
}

/// Mirrors `dedupe_key_for_crash` but pulls from `Finding` state.
fn finding_dedupe_key(f: &Finding) -> (String, String, Option<String>, u32) {
    // Key off the finding's own (evidence-based) `category_tag` and the
    // replay-named `invariant_id` rather than re-deriving a tag from the
    // action sequence via the heuristic. This is what keeps two distinct
    // invariants tripped by one handler from collapsing into a single finding
    // (the over-dedup the old meta-based key suffered).
    let (invariant_id, err) = match &f.reproducer {
        Some(Reproducer::Crucible {
            action_sequence,
            invariant_id,
            ..
        }) => {
            let err = action_sequence
                .last()
                .and_then(|a| a.error_code)
                .unwrap_or(0);
            (invariant_id.clone(), err)
        }
        _ => (None, 0),
    };
    (f.handler.clone(), f.category_tag.clone(), invariant_id, err)
}

fn crash_path_from_reproducer(f: &Finding) -> Option<String> {
    match &f.reproducer {
        Some(Reproducer::Crucible { crash_path, .. }) => Some(crash_path.clone()),
        _ => None,
    }
}

/// `harness_dir` and `crash_path` are persisted on the reproducer so the
/// user can re-run.
///
/// `class` carries evidence-based classification from a successful replay
/// (#229). When present, its `(severity, tag, detail)` supersede the
/// last-action heuristic and the named invariant/property is woven into the
/// finding text + dedupe id. When `None` (replay unavailable or its marker
/// unparseable), we fall back to `categorize_crash` and flag that the class is
/// heuristic rather than replay-confirmed.
fn finding_from_crash(
    harness_dir: &Path,
    crash_path: &Path,
    meta: &CrucibleCrashMetadata,
    class: Option<&crate::probe::crucible_replay::CrashClass>,
) -> Result<Finding> {
    let (severity, tag, detail, evidence_based) = match class {
        Some(c) => (c.severity.clone(), c.tag, c.detail.clone(), true),
        None => {
            let (sev, tag) = categorize_crash(meta);
            (sev, tag, None, false)
        }
    };
    let handler = derive_handler_for_crash(meta);
    // Fold the named invariant into the dedupe salt so two distinct invariants
    // tripped by the same handler no longer collapse into one finding (the
    // limitation `dedupe_key_for_crash` documented — now fixable with the
    // replay-provided name).
    let id_salt = match &detail {
        Some(name) => format!("{tag}:{name}"),
        None => tag.to_string(),
    };
    let id = stable_finding_id(harness_dir, &handler, &id_salt, meta);
    let invocation = format!(
        "crucible show {} {} --replay",
        harness_dir.display(),
        crash_path.display()
    );
    let crucible_version = crucible_version().unwrap_or_else(|| "unknown".to_string());

    let named = match &detail {
        Some(name) => format!(" (`{name}`)"),
        None => String::new(),
    };
    let spec_silent_on = if evidence_based {
        format!("replay-confirmed `{tag}`{named}: a fuzz-discovered action sequence reproduces the violation. The spec is silent on this case.")
    } else {
        format!("fuzz-discovered path triggers `{tag}` (heuristic classification — replay evidence unavailable). The spec is silent on this case.")
    };

    Ok(Finding {
        id,
        category: Category::CrucibleFuzzCrash,
        severity,
        handler,
        spec_silent_on,
        suppression_hint: "add a `requires` clause covering this input, \
                           or refine the invariant if the violation is real."
            .to_string(),
        investigation_hint: format!(
            "replay with `{invocation}` to see the failing trace; run `crucible tmin` for a smaller chain if needed."
        ),
        category_tag: tag.to_string(),
        reproducer: Some(Reproducer::Crucible {
            harness_path: harness_dir.display().to_string(),
            crash_path: crash_path.display().to_string(),
            invocation,
            action_sequence: meta.actions.clone(),
            extra_seeds: Vec::new(),
            crucible_version,
            invariant_id: detail,
        }),
        gated_by: None,
    })
}

/// Replay one minimized crash and capture its `[FUZZ_FINDING]` marker.
///
/// Uses the same `crucible run … --replay <file>` surface the domain-replay
/// path already drives (`crucible_replay_command`): it re-runs the
/// already-built harness binary in replay mode (`FUZZ_INPUT_FILE` set) and
/// prints the marker to stdout. The harness's replay block reconstructs the
/// action sequence from the `.meta.json` when the raw bytes don't fully
/// deserialize, so passing the crash meta path is sufficient.
///
/// Returns `None` if crucible can't be spawned or emits no parseable marker —
/// the caller then falls back to heuristic classification rather than dropping
/// the crash.
fn replay_capture(
    harness_dir: &Path,
    crash_path: &Path,
    timeout: Duration,
) -> Option<crate::probe::crucible_replay::ReplayEvidence> {
    let command = crucible_replay_command(harness_dir, crash_path).ok()?;
    let mut process = Command::new(&command[0]);
    process.args(&command[1..]);
    let TimedCommandOutput::Completed { stdout } =
        run_command_with_timeout(&mut process, timeout).ok()?
    else {
        eprintln!(
            "[crucible] replay timed out after {}ms for {}.",
            timeout.as_millis(),
            crash_path.display()
        );
        return None;
    };
    let stdout = String::from_utf8_lossy(&stdout);
    crate::probe::crucible_replay::parse_fuzz_finding(&stdout)
}

enum TimedCommandOutput {
    Completed { stdout: Vec<u8> },
    TimedOut,
}

/// Run a subprocess under a hard wall-clock deadline. On Unix the child owns
/// a fresh process group so a timeout kills Crucible and its harness child,
/// preventing orphaned replay processes from surviving the probe.
fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<TimedCommandOutput> {
    let mut stdout = tempfile::tempfile().context("creating replay stdout capture")?;
    command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn().context("spawning bounded subprocess")?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            stdout.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes)?;
            return Ok(TimedCommandOutput::Completed { stdout: bytes });
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            // SAFETY: the child was spawned as leader of a fresh process
            // group, so the negated child PID targets only this command tree.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            // Also target the direct child so a rare process-group signalling
            // failure cannot turn the subsequent wait back into an unbounded
            // operation.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(TimedCommandOutput::TimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn stable_finding_id(
    harness_dir: &Path,
    handler: &str,
    tag: &str,
    meta: &CrucibleCrashMetadata,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(harness_dir.display().to_string());
    hasher.update(handler);
    hasher.update(tag);
    if let Some(seed) = meta.seed {
        hasher.update(seed.to_le_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

// ============================================================================
// IO — shells crucible / cargo / fs
// ============================================================================

/// Wire the program IDL into `<harness>/idls/<prog>.json` when present;
/// `<prog>` is the harness dir leaf (= spec's snake-case program_name).
/// Idempotent — a pre-existing IDL file is left alone.
///
/// Source lookup (first match wins):
/// 1. `<root>/target/idl/<prog>.json` — `anchor build` output.
/// 2. `<root>/idl.json` — committed Codama-convention IDL.
/// 3. `<root>/idl/<prog>.json` — committed Codama default output dir.
///
/// (2) and (3) let a brownfield crate ship a *static* IDL and skip the
/// `anchor build` round-trip entirely — the same committed-IDL
/// convention the Pinocchio path already honours
/// (`crucible_brownfield::discover_pinocchio_idl`). Without it the Anchor
/// path is hard-tied to the ephemeral, un-committable `target/idl/`.
pub fn discover_idl(harness_dir: &Path, project_root: &Path) -> Result<()> {
    let prog = harness_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("harness_dir has no leaf name"))?;
    let candidates = [
        project_root
            .join("target")
            .join("idl")
            .join(format!("{prog}.json")),
        project_root.join("idl.json"),
        project_root.join("idl").join(format!("{prog}.json")),
    ];
    let Some(src_idl) = candidates.iter().find(|p| p.exists()) else {
        return Ok(()); // nothing to discover; user wires manually
    };
    let dest_dir = harness_dir.join("idls");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(format!("{prog}.json"));
    if dest.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src_idl, &dest)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(src_idl, &dest)?;
    }
    Ok(())
}

fn build_harness(harness_dir: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .args(["build", "--features", HARNESS_TEST_NAME])
        .current_dir(harness_dir)
        .status()
        .context("spawning `cargo build` for harness")?;
    if !status.success() {
        bail!(
            "Crucible harness build failed in {}. \
             Common causes: missing IDL at idls/{}.json, mismatched Anchor version, \
             or unfilled todo!() in action bodies.",
            harness_dir.display(),
            harness_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("<prog>"),
        );
    }
    Ok(())
}

fn run_crucible(
    harness_dir: &Path,
    budget: Duration,
    stateful: bool,
    corpus_in: Option<&Path>,
) -> Result<PathBuf> {
    let prog = harness_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("harness_dir has no leaf name"))?;
    let mut cmd = Command::new("crucible");
    cmd.arg("run")
        .arg(prog)
        .arg(HARNESS_TEST_NAME)
        .arg("-C")
        .arg(harness_dir)
        .arg("--timeout")
        .arg(budget.as_secs().to_string());
    if stateful {
        cmd.arg("--stateful");
    }
    if let Some(corpus_in) = corpus_in {
        cmd.arg("--corpus-in").arg(corpus_in);
    }
    let status = cmd.status().context("spawning `crucible run`")?;
    // Non-zero exit can mean "found crashes", not failure — harvest the
    // crashes dir regardless.
    let _ = status;
    Ok(harness_dir.join("crashes").join(HARNESS_TEST_NAME))
}

fn crucible_replay_command(harness_dir: &Path, seed: &Path) -> Result<Vec<String>> {
    let prog = harness_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("harness_dir has no leaf name"))?;
    Ok(vec![
        "crucible".to_string(),
        "run".to_string(),
        prog.to_string(),
        HARNESS_TEST_NAME.to_string(),
        "-C".to_string(),
        harness_dir.display().to_string(),
        "--replay".to_string(),
        seed.display().to_string(),
    ])
}

fn run_crucible_replay(harness_dir: &Path, seed: &Path) -> Result<Option<i32>> {
    let command = crucible_replay_command(harness_dir, seed)?;
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .context("spawning `crucible run --replay`")?;
    // As in fuzz mode, a non-zero status may mean that the replay reproduced
    // a violation. Crash harvesting is the source of truth.
    Ok(status.code())
}

fn replay_domain_seed_report(
    harness_dir: &Path,
    seeds: &super::domain_sequence_seed::DomainSeedReport,
) -> Result<PathBuf> {
    replay_domain_seed_report_with(harness_dir, seeds, run_crucible_replay)
}

fn replay_domain_seed_report_with<F>(
    harness_dir: &Path,
    seeds: &super::domain_sequence_seed::DomainSeedReport,
    mut replay: F,
) -> Result<PathBuf>
where
    F: FnMut(&Path, &Path) -> Result<Option<i32>>,
{
    let mut report = DomainReplayReport {
        schema_version: 1,
        schema_uri: DOMAIN_REPLAY_REPORT_SCHEMA_URI.to_string(),
        resolved_document_sha256: seeds.resolved_document_sha256.clone(),
        account_binding_overlay_sha256: seeds.account_binding_overlay_sha256.clone(),
        harness_sha256: seeds.harness_sha256.clone(),
        records: Vec::with_capacity(seeds.seeds.len()),
    };
    let report_path = harness_dir
        .join(".qedgen")
        .join("domain-replay-report.json");

    for seed in &seeds.seeds {
        let command = crucible_replay_command(harness_dir, &seed.path)?;
        match replay(harness_dir, &seed.path) {
            Ok(exit_code) => {
                report.records.push(DomainReplayRecord {
                    plan_id: seed.plan_id.clone(),
                    seed_path: seed.path.display().to_string(),
                    seed_sha256: seed.seed_sha256.clone(),
                    action_count: seed.action_count,
                    command,
                    status: match exit_code {
                        Some(0) => DomainReplayStatus::CompletedZeroExit,
                        Some(_) => DomainReplayStatus::CompletedNonzeroExit,
                        // No code → killed by a signal (a reproduced crash).
                        None => DomainReplayStatus::TerminatedBySignal,
                    },
                    exit_code,
                    error: None,
                });
                write_domain_replay_report(&report_path, &report)?;
            }
            Err(error) => {
                report.records.push(DomainReplayRecord {
                    plan_id: seed.plan_id.clone(),
                    seed_path: seed.path.display().to_string(),
                    seed_sha256: seed.seed_sha256.clone(),
                    action_count: seed.action_count,
                    command,
                    status: DomainReplayStatus::SpawnFailed,
                    exit_code: None,
                    error: Some(format!("{error:#}")),
                });
                write_domain_replay_report(&report_path, &report)?;
                return Err(error).with_context(|| {
                    format!(
                        "replaying domain plan `{}`; evidence written to {}",
                        seed.plan_id,
                        report_path.display()
                    )
                });
            }
        }
    }
    eprintln!(
        "Crucible domain replay evidence written to {}.",
        report_path.display()
    );
    Ok(report_path)
}

fn write_domain_replay_report(path: &Path, report: &DomainReplayReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(report)?))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Minimize the crash set in one shot via `crucible tmin --all`, under the
/// same hard subprocess deadline used by replay. Per-crash invocation doesn't
/// work: tmin wants `<CRASH_FILE>` relative to the crashes dir and exposes no
/// native timeout flag. Crash storms skip minimization before calling here.
fn auto_tmin_all(harness_dir: &Path, timeout: Duration) -> Result<()> {
    let prog = harness_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("harness_dir has no leaf name"))?;
    let mut command = Command::new("crucible");
    command
        .arg("tmin")
        .arg(prog)
        .arg(HARNESS_TEST_NAME)
        .arg("--all")
        .arg("-C")
        .arg(harness_dir);
    if matches!(
        run_command_with_timeout(&mut command, timeout),
        Ok(TimedCommandOutput::TimedOut)
    ) {
        eprintln!(
            "[crucible] `tmin --all` exceeded its {}s budget; replaying raw sampled crashes.",
            timeout.as_secs()
        );
    }
    Ok(())
}

fn collect_crash_files(crash_dir: &Path) -> Result<Vec<PathBuf>> {
    if !crash_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(crash_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn crucible_version() -> Option<String> {
    Command::new("crucible")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{CrucibleActionRecord, CrucibleCrashMetadata};

    fn domain_dossier(ratification: &str, lanes: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "schema_uri": "https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json",
            "asset_flows": [{
                "id": "flow_deposit",
                "metadata": {
                    "ratification": ratification,
                    "verification_lanes": lanes
                }
            }],
            "quantities": [],
            "paired_operations": [],
            "lifecycle_edges": [],
            "authority_capabilities": [],
            "economic_equations": [],
            "external_assumptions": []
        })
    }

    #[test]
    fn domain_mode_accepts_only_ratified_crucible_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let dossier = tmp.path().join("domain-dossier.json");
        std::fs::write(
            &dossier,
            serde_json::to_string(&domain_dossier("user", &["manual", "crucible"])).unwrap(),
        )
        .unwrap();

        assert_eq!(
            require_ratified_domain_facts(&dossier, Path::new("vault.qedspec")).unwrap(),
            1
        );
    }

    #[test]
    fn domain_mode_rejects_empty_crucible_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let dossier = tmp.path().join("domain-dossier.json");
        std::fs::write(
            &dossier,
            serde_json::to_string(&domain_dossier("user", &["manual"])).unwrap(),
        )
        .unwrap();

        let error = require_ratified_domain_facts(&dossier, Path::new("vault.qedspec"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no facts assigned to the Crucible"));
    }

    #[test]
    fn domain_mode_persists_blocked_lane_for_pending_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let dossier = tmp.path().join("domain-dossier.json");
        std::fs::write(
            &dossier,
            serde_json::to_string(&domain_dossier("pending", &["crucible"])).unwrap(),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("run-manifest.json"),
            serde_json::to_string(&serde_json::json!({
                "status": "running",
                "lanes": [{"name": "source-review", "status": "passed"}]
            }))
            .unwrap(),
        )
        .unwrap();

        let error = require_ratified_domain_facts(&dossier, Path::new("vault.qedspec"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("flow_deposit"));
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("run-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["status"], "tooling-blocked");
        let lane = manifest["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|lane| lane["name"] == "crucible-domain")
            .unwrap();
        assert_eq!(lane["status"], "blocked");
        assert!(lane["resume_command"]
            .as_str()
            .unwrap()
            .contains("--crucible-mode domain"));
    }

    #[test]
    fn domain_mode_rejects_ratified_prose_without_executable_assertions() {
        let tmp = tempfile::tempdir().unwrap();
        let dossier = tmp.path().join("domain-dossier.json");
        std::fs::write(&dossier, "{}").unwrap();
        std::fs::write(
            tmp.path().join("run-manifest.json"),
            r#"{"status":"running","lanes":[]}"#,
        )
        .unwrap();

        let error = require_executable_domain_assertions(&dossier, Path::new("vault.qedspec"), 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no Rust-renderable"));
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("run-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["lanes"][0]["name"], "crucible-domain");
    }

    #[test]
    fn domain_mode_clears_previous_readiness_block() {
        let tmp = tempfile::tempdir().unwrap();
        let dossier = tmp.path().join("domain-dossier.json");
        std::fs::write(&dossier, "{}").unwrap();
        std::fs::write(
            tmp.path().join("run-manifest.json"),
            serde_json::to_string(&serde_json::json!({
                "status": "tooling-blocked",
                "lanes": [{
                    "name": "crucible-domain",
                    "status": "blocked",
                    "reason": "pending facts",
                    "resume_command": "retry"
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        mark_domain_lane_ready(&dossier).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("run-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["status"], "running");
        assert_eq!(manifest["lanes"][0]["status"], "queued");
        assert!(manifest["lanes"][0]["reason"].is_null());
    }

    fn seed_report(harness: &Path) -> super::super::domain_sequence_seed::DomainSeedReport {
        super::super::domain_sequence_seed::DomainSeedReport {
            corpus_dir: harness.join("corpus"),
            resolved_document_sha256: "11".repeat(32),
            account_binding_overlay_sha256: "22".repeat(32),
            harness_sha256: "33".repeat(32),
            seeds: vec![
                super::super::domain_sequence_seed::DomainSeed {
                    plan_id: "plan-a".to_string(),
                    path: harness.join("corpus/plan-a.seed"),
                    action_count: 2,
                    seed_sha256: "44".repeat(32),
                },
                super::super::domain_sequence_seed::DomainSeed {
                    plan_id: "plan-b".to_string(),
                    path: harness.join("corpus/plan-b.seed"),
                    action_count: 1,
                    seed_sha256: "55".repeat(32),
                },
            ],
        }
    }

    #[test]
    fn replay_command_uses_native_crucible_replay_surface() {
        let command = crucible_replay_command(
            Path::new("/tmp/vault"),
            Path::new("/tmp/vault/corpus/plan.seed"),
        )
        .unwrap();
        assert_eq!(
            command,
            vec![
                "crucible",
                "run",
                "vault",
                "invariant_test",
                "-C",
                "/tmp/vault",
                "--replay",
                "/tmp/vault/corpus/plan.seed",
            ]
        );
    }

    #[test]
    fn replay_ledger_records_each_plan_and_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let harness = tmp.path().join("vault");
        std::fs::create_dir_all(&harness).unwrap();
        let seeds = seed_report(&harness);
        let mut calls = 0;
        let path = replay_domain_seed_report_with(&harness, &seeds, |_, _| {
            calls += 1;
            Ok(if calls == 1 { Some(0) } else { Some(1) })
        })
        .unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["schema_uri"], DOMAIN_REPLAY_REPORT_SCHEMA_URI);
        assert_eq!(report["resolved_document_sha256"], "11".repeat(32));
        assert_eq!(report["records"][0]["plan_id"], "plan-a");
        assert_eq!(report["records"][0]["status"], "completed_zero_exit");
        assert_eq!(report["records"][1]["status"], "completed_nonzero_exit");
        assert_eq!(report["records"][1]["exit_code"], 1);
        assert_eq!(report["records"][1]["seed_sha256"], "55".repeat(32));
    }

    #[test]
    fn replay_ledger_records_signal_termination_as_reproduced_crash() {
        // A replay killed by a signal (no exit code) is a reproduced crash,
        // not a spawn failure. It must serialize as `terminated_by_signal`
        // with a null exit_code and no error — the shape the shipped
        // validator accepts.
        let tmp = tempfile::tempdir().unwrap();
        let harness = tmp.path().join("vault");
        std::fs::create_dir_all(&harness).unwrap();
        let mut seeds = seed_report(&harness);
        seeds.seeds.truncate(1);
        let path = replay_domain_seed_report_with(&harness, &seeds, |_, _| Ok(None)).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["records"][0]["status"], "terminated_by_signal");
        assert!(report["records"][0]["exit_code"].is_null());
        assert!(report["records"][0]["error"].is_null());
    }

    #[test]
    fn replay_ledger_persists_spawn_failure_before_returning_error() {
        let tmp = tempfile::tempdir().unwrap();
        let harness = tmp.path().join("vault");
        std::fs::create_dir_all(&harness).unwrap();
        let mut seeds = seed_report(&harness);
        seeds.seeds.truncate(1);
        let error = replay_domain_seed_report_with(&harness, &seeds, |_, _| {
            Err(anyhow::anyhow!("fake crucible unavailable"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("plan-a"));
        let path = harness.join(".qedgen/domain-replay-report.json");
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(report["records"][0]["status"], "spawn_failed");
        assert!(report["records"][0]["error"]
            .as_str()
            .unwrap()
            .contains("fake crucible unavailable"));
    }
    use serde_json::json;

    /// Real `.meta.json` from `crucible run` on Crucible's bundled escrow
    /// example (commit 689e63a) — exercises real crash output, not just
    /// synthetic data.
    const REAL_CRASH_META: &str = include_str!("../../test-fixtures/real-crucible-crash.meta.json");

    #[test]
    fn parses_real_crucible_crash_metadata() {
        let meta = parse_crash_metadata(REAL_CRASH_META.as_bytes()).expect("parse");
        assert_eq!(meta.test_name, "invariant_escrow");
        assert_eq!(meta.actions.len(), 6);
        // Last action succeeded with no error_code → post-action assert
        // tripped (handler returned Ok).
        let last = meta.actions.last().unwrap();
        assert_eq!(last.name, "withdraw");
        assert!(last.success);
        assert!(last.error_code.is_none());
    }

    /// Pins the FALLBACK heuristic's behavior on the real fixture: last action
    /// is a successful `withdraw`, so the last-action rule guesses
    /// `invariant_violation` with no name. #229's whole point is that this
    /// guess is unreliable — the crash may originate from the earlier `claim`
    /// (err 6003) — which is why the harvest path prefers replay evidence and
    /// only falls back here. The evidence path is covered by
    /// `crucible_replay::tests::earlier_action_marker_wins_over_last_action`.
    #[test]
    fn real_crash_fallback_heuristic_guesses_high_invariant_violation() {
        let meta = parse_crash_metadata(REAL_CRASH_META.as_bytes()).expect("parse");
        let (sev, tag) = categorize_crash(&meta);
        assert!(matches!(sev, Severity::High));
        assert_eq!(tag, "invariant_violation");
    }

    #[test]
    fn real_crash_derives_withdraw_as_handler() {
        let meta = parse_crash_metadata(REAL_CRASH_META.as_bytes()).expect("parse");
        // Real Crucible action names lack the `action_` prefix; the strip
        // is defensive.
        assert_eq!(derive_handler_for_crash(&meta), "withdraw");
    }

    fn meta_with(actions: Vec<CrucibleActionRecord>) -> CrucibleCrashMetadata {
        CrucibleCrashMetadata {
            test_name: HARNESS_TEST_NAME.into(),
            timestamp: "2026-05-13T00:00:00Z".into(),
            iteration: 42,
            seed: Some(0xdeadbeef),
            actions,
        }
    }

    fn action(name: &str, success: bool, error_code: Option<u32>) -> CrucibleActionRecord {
        CrucibleActionRecord {
            name: name.into(),
            params: json!({}),
            success,
            error_code,
        }
    }

    fn crash_candidate(path: &str, actions: Vec<CrucibleActionRecord>) -> CrashCandidate {
        CrashCandidate {
            path: PathBuf::from(path),
            meta: meta_with(actions),
        }
    }

    #[test]
    fn crash_storm_sampling_is_bounded_and_shape_diverse() {
        let mut crashes = (0..3_747)
            .map(|index| {
                crash_candidate(
                    &format!("accept-{index}.meta.json"),
                    vec![action("accept", true, None)],
                )
            })
            .collect::<Vec<_>>();
        crashes.extend((0..20).map(|index| {
            crash_candidate(
                &format!("withdraw-{index}.meta.json"),
                vec![action("withdraw", false, Some(6001))],
            )
        }));

        let selected = select_crashes_for_replay(crashes);
        assert!(selected.len() <= MAX_CRASH_REPLAYS);
        let accept_count = selected
            .iter()
            .filter(|candidate| derive_handler_for_crash(&candidate.meta) == "accept")
            .count();
        let withdraw_count = selected
            .iter()
            .filter(|candidate| derive_handler_for_crash(&candidate.meta) == "withdraw")
            .count();
        assert_eq!(accept_count, MAX_REPLAYS_PER_CRASH_SHAPE);
        assert_eq!(withdraw_count, MAX_REPLAYS_PER_CRASH_SHAPE);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_subprocess_kills_a_slow_command_tree() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let started = Instant::now();
        let outcome = run_command_with_timeout(&mut command, Duration::from_millis(50)).unwrap();
        assert!(matches!(outcome, TimedCommandOutput::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout helper exceeded its wall-clock allowance"
        );
    }

    #[test]
    fn parse_crash_metadata_roundtrips_real_shape() {
        let json = br#"{
            "test_name": "invariant_test",
            "timestamp": "2026-05-13T12:34:56Z",
            "iteration": 1234,
            "seed": 305419896,
            "actions": [
                {"name": "action_initialize", "params": {"deposit_amount": 100, "receive_amount": 50}, "success": true, "error_code": null},
                {"name": "action_exchange", "params": {}, "success": false, "error_code": 6001}
            ]
        }"#;
        let meta = parse_crash_metadata(json).expect("parse");
        assert_eq!(meta.test_name, "invariant_test");
        assert_eq!(meta.iteration, 1234);
        assert_eq!(meta.seed, Some(305419896));
        assert_eq!(meta.actions.len(), 2);
        assert_eq!(meta.actions[0].name, "action_initialize");
        assert!(meta.actions[0].success);
        assert_eq!(meta.actions[1].error_code, Some(6001));
    }

    #[test]
    fn parse_crash_metadata_tolerates_missing_seed() {
        let json = br#"{
            "test_name": "invariant_test",
            "timestamp": "2026-05-13T12:34:56Z",
            "iteration": 7,
            "actions": []
        }"#;
        let meta = parse_crash_metadata(json).expect("parse");
        assert!(meta.seed.is_none());
        assert!(meta.actions.is_empty());
    }

    #[test]
    fn parse_crash_metadata_surfaces_schema_drift_clearly() {
        let json = br#"{"this_is_not_crucible": true}"#;
        let err = parse_crash_metadata(json).expect_err("malformed should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("schema drifted") || msg.contains("re-pin"),
            "error should hint at schema drift: {msg}"
        );
    }

    #[test]
    fn categorize_clean_invariant_violation_is_high() {
        let meta = meta_with(vec![action("action_increment", true, None)]);
        let (sev, tag) = categorize_crash(&meta);
        assert!(matches!(sev, Severity::High));
        assert_eq!(tag, "invariant_violation");
    }

    #[test]
    fn categorize_anchor_runtime_abort_is_medium() {
        let meta = meta_with(vec![
            action("action_init", true, None),
            action("action_withdraw", false, Some(6001)),
        ]);
        let (sev, tag) = categorize_crash(&meta);
        assert!(matches!(sev, Severity::Medium));
        assert_eq!(tag, "runtime_abort");
    }

    #[test]
    fn categorize_unanchored_panic_is_medium() {
        let meta = meta_with(vec![action("action_divide", false, None)]);
        let (sev, tag) = categorize_crash(&meta);
        assert!(matches!(sev, Severity::Medium));
        assert_eq!(tag, "runtime_panic");
    }

    #[test]
    fn derive_handler_strips_action_prefix() {
        let meta = meta_with(vec![action("action_withdraw", true, None)]);
        assert_eq!(derive_handler_for_crash(&meta), "withdraw");
    }

    #[test]
    fn derive_handler_empty_actions_is_unknown() {
        let meta = meta_with(vec![]);
        assert_eq!(derive_handler_for_crash(&meta), "unknown");
    }

    #[test]
    fn dedupe_key_groups_same_handler_same_outcome() {
        let m1 = meta_with(vec![action("action_w", true, None)]);
        let m2 = meta_with(vec![action("action_w", true, None)]);
        assert_eq!(dedupe_key_for_crash(&m1), dedupe_key_for_crash(&m2));
    }

    #[test]
    fn dedupe_key_distinguishes_anchor_error_codes() {
        let m1 = meta_with(vec![action("action_w", false, Some(6001))]);
        let m2 = meta_with(vec![action("action_w", false, Some(6002))]);
        assert_ne!(dedupe_key_for_crash(&m1), dedupe_key_for_crash(&m2));
    }

    fn synthetic_finding(
        handler: &str,
        tag: &str,
        error_code: Option<u32>,
        crash: &str,
    ) -> Finding {
        synthetic_finding_named(handler, tag, None, error_code, crash)
    }

    fn synthetic_finding_named(
        handler: &str,
        tag: &str,
        invariant_id: Option<&str>,
        error_code: Option<u32>,
        crash: &str,
    ) -> Finding {
        Finding {
            id: format!("{handler}-{tag}-{}", error_code.unwrap_or(0)),
            category: Category::CrucibleFuzzCrash,
            severity: Severity::High,
            handler: handler.to_string(),
            spec_silent_on: String::new(),
            suppression_hint: String::new(),
            investigation_hint: String::new(),
            category_tag: tag.to_string(),
            reproducer: Some(Reproducer::Crucible {
                harness_path: "fuzz/x".into(),
                crash_path: crash.into(),
                invocation: format!("crucible show fuzz/x {crash} --replay"),
                action_sequence: vec![action(
                    &format!("action_{handler}"),
                    error_code.is_none(),
                    error_code,
                )],
                extra_seeds: Vec::new(),
                crucible_version: "test".into(),
                invariant_id: invariant_id.map(str::to_string),
            }),
            gated_by: None,
        }
    }

    #[test]
    fn dedupe_collapses_repeats_and_collects_extra_seeds() {
        let findings = vec![
            synthetic_finding("withdraw", "invariant_violation", None, "a.meta.json"),
            synthetic_finding("withdraw", "invariant_violation", None, "b.meta.json"),
            synthetic_finding("withdraw", "invariant_violation", None, "c.meta.json"),
        ];
        let out = dedupe_findings(findings);
        assert_eq!(out.len(), 1);
        let Reproducer::Crucible { extra_seeds, .. } = out[0].reproducer.as_ref().unwrap() else {
            panic!("expected Crucible reproducer");
        };
        assert_eq!(extra_seeds.len(), 2);
        assert!(extra_seeds.iter().any(|s| s == "b.meta.json"));
        assert!(extra_seeds.iter().any(|s| s == "c.meta.json"));
    }

    #[test]
    fn dedupe_keeps_distinct_handlers_separate() {
        let findings = vec![
            synthetic_finding("withdraw", "invariant_violation", None, "a.meta.json"),
            synthetic_finding("deposit", "invariant_violation", None, "b.meta.json"),
        ];
        let out = dedupe_findings(findings);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn dedupe_keeps_distinct_error_codes_separate() {
        let findings = vec![
            synthetic_finding("withdraw", "runtime_abort", Some(6001), "a.meta.json"),
            synthetic_finding("withdraw", "runtime_abort", Some(6002), "b.meta.json"),
        ];
        let out = dedupe_findings(findings);
        assert_eq!(out.len(), 2);
    }

    /// #229: two DIFFERENT invariants tripped by the same handler must stay
    /// separate findings — the over-dedup the old meta-based key caused
    /// (both would classify `invariant_violation` and collapse). The
    /// replay-provided `invariant_id` now keys them apart.
    #[test]
    fn dedupe_keeps_distinct_invariant_names_separate() {
        let findings = vec![
            synthetic_finding_named(
                "settle",
                "invariant_violation",
                Some("conservation"),
                None,
                "a.meta.json",
            ),
            synthetic_finding_named(
                "settle",
                "invariant_violation",
                Some("no_dilution"),
                None,
                "b.meta.json",
            ),
        ];
        let out = dedupe_findings(findings);
        assert_eq!(out.len(), 2, "distinct invariants must not collapse");
    }

    /// The same invariant tripped by different seeds still collapses, with
    /// the extra seed collected — dedup didn't become too eager.
    #[test]
    fn dedupe_collapses_same_invariant_name_across_seeds() {
        let findings = vec![
            synthetic_finding_named(
                "settle",
                "invariant_violation",
                Some("conservation"),
                None,
                "a.meta.json",
            ),
            synthetic_finding_named(
                "settle",
                "invariant_violation",
                Some("conservation"),
                None,
                "b.meta.json",
            ),
        ];
        let out = dedupe_findings(findings);
        assert_eq!(out.len(), 1);
        let Reproducer::Crucible { extra_seeds, .. } = out[0].reproducer.as_ref().unwrap() else {
            panic!("expected Crucible reproducer");
        };
        assert_eq!(extra_seeds, &vec!["b.meta.json".to_string()]);
    }

    #[test]
    fn stable_finding_id_is_stable_across_runs() {
        let meta = meta_with(vec![action("action_w", true, None)]);
        let id1 = stable_finding_id(Path::new("fuzz/x"), "w", "invariant_violation", &meta);
        let id2 = stable_finding_id(Path::new("fuzz/x"), "w", "invariant_violation", &meta);
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 16);
    }

    #[test]
    fn stable_finding_id_differs_for_different_seeds() {
        let m1 = CrucibleCrashMetadata {
            seed: Some(1),
            ..meta_with(vec![])
        };
        let m2 = CrucibleCrashMetadata {
            seed: Some(2),
            ..meta_with(vec![])
        };
        let id1 = stable_finding_id(Path::new("fuzz/x"), "h", "t", &m1);
        let id2 = stable_finding_id(Path::new("fuzz/x"), "h", "t", &m2);
        assert_ne!(id1, id2);
    }

    #[test]
    fn discover_idl_no_idl_present_is_ok() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let harness = tmp.path().join("fuzz").join("myprog");
        std::fs::create_dir_all(&harness).unwrap();
        // No target/idl/ → noop, returns Ok.
        let res = discover_idl(&harness, tmp.path());
        assert!(res.is_ok());
        assert!(!harness.join("idls").join("myprog.json").exists());
    }

    #[test]
    fn discover_idl_symlinks_when_target_idl_exists() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let harness = tmp.path().join("fuzz").join("myprog");
        std::fs::create_dir_all(&harness).unwrap();
        let idl_dir = tmp.path().join("target").join("idl");
        std::fs::create_dir_all(&idl_dir).unwrap();
        let idl_path = idl_dir.join("myprog.json");
        std::fs::write(&idl_path, r#"{"version":"0.30"}"#).unwrap();

        discover_idl(&harness, tmp.path()).expect("discover");

        let dest = harness.join("idls").join("myprog.json");
        assert!(dest.exists(), "IDL should be discovered");
        // The dest should resolve to the same content as the source.
        let read = std::fs::read_to_string(&dest).unwrap();
        assert!(read.contains("\"version\""));
    }

    #[test]
    fn discover_idl_falls_back_to_committed_root_idl() {
        // No `target/idl/` (no `anchor build`); a committed `<root>/idl.json`
        // is wired in instead — the brownfield no-build path.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let harness = tmp.path().join("fuzz").join("myprog");
        std::fs::create_dir_all(&harness).unwrap();
        std::fs::write(tmp.path().join("idl.json"), r#"{"version":"0.30"}"#).unwrap();

        discover_idl(&harness, tmp.path()).expect("discover");

        let dest = harness.join("idls").join("myprog.json");
        assert!(dest.exists(), "committed idl.json should be discovered");
        let read = std::fs::read_to_string(&dest).unwrap();
        assert!(read.contains("\"version\""));
    }

    #[test]
    fn discover_idl_prefers_target_over_committed() {
        // `anchor build` output wins over a committed copy when both exist.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let harness = tmp.path().join("fuzz").join("myprog");
        std::fs::create_dir_all(&harness).unwrap();
        let idl_dir = tmp.path().join("target").join("idl");
        std::fs::create_dir_all(&idl_dir).unwrap();
        std::fs::write(idl_dir.join("myprog.json"), r#"{"src":"target"}"#).unwrap();
        std::fs::write(tmp.path().join("idl.json"), r#"{"src":"committed"}"#).unwrap();

        discover_idl(&harness, tmp.path()).expect("discover");

        let dest = harness.join("idls").join("myprog.json");
        let read = std::fs::read_to_string(&dest).unwrap();
        assert!(
            read.contains("target"),
            "target/idl should win over committed"
        );
    }

    #[test]
    fn discover_idl_idempotent_skips_pre_existing() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let harness = tmp.path().join("fuzz").join("myprog");
        std::fs::create_dir_all(harness.join("idls")).unwrap();
        let dest = harness.join("idls").join("myprog.json");
        std::fs::write(&dest, "pre-existing").unwrap();
        let idl_dir = tmp.path().join("target").join("idl");
        std::fs::create_dir_all(&idl_dir).unwrap();
        std::fs::write(idl_dir.join("myprog.json"), "from-target").unwrap();

        discover_idl(&harness, tmp.path()).expect("discover");

        // Pre-existing file is preserved.
        let read = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(read, "pre-existing");
    }
}
