//! Crucible-as-probe-engine. v2.18 P2.
//!
//! Treats Crucible as another probe engine alongside the pattern-match
//! engine in `probe.rs`. The pattern-match path runs static predicates
//! over the `.qedspec`; this path runs coverage-guided fuzzing of the
//! deployed `.so` and converts each crash into a `Finding` with
//! `Reproducer::Crucible`. Both engines emit into the same surface.
//!
//! ## Pipeline
//!
//! 1. **IDL discovery.** Symlink `target/idl/<prog>.json` into
//!    `<harness>/idls/<prog>.json` when present.
//! 2. **Build.** `cargo build --features invariant_test` in the harness
//!    dir. Heavy first-time (LibAFL); incremental thereafter.
//! 3. **Smoke (~30s).** Short `crucible run` to confirm the harness
//!    actually fuzzes (Anchor IDL drops, signer wiring, etc.). If
//!    invariant violations fire at high rate during smoke, surface what
//!    was found and stop early — burning the full budget to re-discover
//!    the same class of bug is anti-quality.
//! 4. **Full run.** `crucible run` for the user budget.
//! 5. **Per-crash post-processing.** For each `<hash>.meta.json`:
//!    - `crucible tmin` with 30s cap (PRD §"Auto-tmin every crash")
//!    - Categorize: invariant violation vs. panic vs. account mismatch
//!    - Build `Finding` with `Reproducer::Crucible`
//! 6. **Dedupe** by `(handler, dedupe_key)`. First crash per pair is
//!    the canonical reproducer; subsequent crashes contribute their
//!    crash file path to `extra_seeds`.
//!
//! ## What's testable in unit tests
//!
//! - `parse_crash_metadata` — JSON → struct (no IO)
//! - `categorize_crash` — meta → (severity, category)
//! - `dedupe_findings` — list collapsing
//! - `derive_handler_for_crash` — last-action heuristic
//! - `dedupe_key_for_crash` — error_code / panic / invariant
//!
//! The shell-out fns (build, run, tmin) need `crucible` on PATH; tests
//! that exercise them are gated behind an `ignored` attribute and run
//! manually.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::crucible_gen::InvariantMode;
use crate::probe::{Category, CrucibleCrashMetadata, Finding, Reproducer, Severity};

/// Per-crash `crucible tmin` cap. Auto-minimization is implicit (PRD
/// "high-quality reproducible vuln out of the gate") but we cap so that
/// running tmin on every crash doesn't eat the user's budget after a
/// productive fuzz run.
pub const TMIN_BUDGET_PER_CRASH: Duration = Duration::from_secs(30);

/// Smoke pre-flight budget. Long enough to confirm the harness can build
/// and dispatch a few actions; short enough that broken harnesses fail
/// fast.
pub const SMOKE_BUDGET: Duration = Duration::from_secs(30);

/// Threshold for stopping after smoke. If smoke surfaces this many
/// distinct findings (post-dedupe), we report them and skip the full
/// run — burning the full budget to find the same class of bug N more
/// ways is anti-quality.
pub const SMOKE_FINDING_CAP: usize = 4;

/// Default full-run budget when the user passes `--fuzz` without an
/// explicit value. Roughly the wall-clock span where a typical small/
/// medium harness has hit a few k iterations on a laptop M-series chip.
pub const DEFAULT_FUZZ_BUDGET: Duration = Duration::from_secs(300);

/// Test-fn name emitted by `crucible_gen::emit_invariant_fn`. Crucible
/// uses this name as both the cargo feature gate and the subcommand
/// argument: `crucible run <prog> invariant_test`.
const HARNESS_TEST_NAME: &str = "invariant_test";

/// Inputs for one fuzz-probe run. Caller assembles paths and budget;
/// the engine does discovery / build / fuzz / triage.
pub struct FuzzProbeContext<'a> {
    /// Path to the `.qedspec` (used for spec-aware finding context).
    /// Held so future per-finding enrichment (e.g. linking back to the
    /// declared invariant name) has the spec to look up.
    #[allow(dead_code)]
    pub spec_path: &'a Path,
    /// Repo root — `target/idl/` lives here.
    pub project_root: PathBuf,
    /// Harness directory (`fuzz/<prog>/`). Already exists from
    /// `qedgen codegen --crucible`.
    pub harness_dir: PathBuf,
    /// Per-crash tmin cap; defaults to TMIN_BUDGET_PER_CRASH.
    pub tmin_cap: Duration,
    /// Smoke pre-flight budget; defaults to SMOKE_BUDGET. Pass
    /// Duration::ZERO to skip smoke (e.g. via `--no-smoke`).
    pub smoke_budget: Duration,
    /// Full-run budget after smoke.
    pub fuzz_budget: Duration,
    /// Stateful mode flag (default false). Crucible's same harness
    /// compiles for either; `--stateful` is a runtime switch.
    pub stateful: bool,
    /// Which invariant family the emitted harness was built against.
    /// Carried through to per-finding context so triage can label
    /// protocol-only crashes distinctly from spec violations.
    /// Default `InvariantMode::Spec` matches v2.20 callers.
    pub invariant_mode: InvariantMode,
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
        }
    }
}

/// Top-level entry — drives the full discovery → build → smoke → run →
/// triage → dedupe pipeline. Returns the deduplicated finding list.
///
/// Shell-outs to `crucible` are gated on `deps::require_crucible()`.
/// Callers are expected to have already validated the harness directory
/// exists (run `qedgen codegen --crucible` first).
pub fn run_fuzz_probe(ctx: &FuzzProbeContext) -> Result<Vec<Finding>> {
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

    let mut findings = Vec::new();

    if !ctx.smoke_budget.is_zero() {
        let smoke = run_crucible_round(ctx, ctx.smoke_budget, "smoke")
            .context("running Crucible smoke pre-flight")?;
        findings.extend(smoke);
        if dedupe_findings(findings.clone()).len() >= SMOKE_FINDING_CAP {
            eprintln!(
                "Smoke surfaced {} distinct findings — stopping early. Fix these before re-running with the full budget (or pass --no-smoke to bypass).",
                findings.len()
            );
            return Ok(dedupe_findings(findings));
        }
    }

    let full = run_crucible_round(ctx, ctx.fuzz_budget, HARNESS_TEST_NAME)
        .context("running Crucible full fuzz")?;
    findings.extend(full);

    Ok(dedupe_findings(findings))
}

/// One round of: fuzz → harvest crashes → tmin → categorize. Used for
/// both smoke and full passes; differ only by budget.
fn run_crucible_round(
    ctx: &FuzzProbeContext,
    budget: Duration,
    label: &str,
) -> Result<Vec<Finding>> {
    let crash_dir = run_crucible(&ctx.harness_dir, budget, ctx.stateful)
        .with_context(|| format!("crucible run ({label}) failed"))?;
    let crashes = collect_crash_files(&crash_dir).unwrap_or_default();
    if !crashes.is_empty() {
        // tmin best-effort: failure is non-fatal — the raw crashes are
        // still valid reproducers, we just lose minimization. `--all`
        // does every crash in a single subprocess.
        let _ = auto_tmin_all(&ctx.harness_dir, ctx.tmin_cap);
    }
    // Re-scan after tmin — minimization may rewrite the .meta.json
    // contents in place. Path list is stable; content is what we read.
    let mut findings = Vec::new();
    for crash in crashes {
        let raw = match std::fs::read(&crash) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let meta = match parse_crash_metadata(&raw) {
            Ok(m) => m,
            Err(_) => continue,
        };
        findings.push(finding_from_crash(&ctx.harness_dir, &crash, &meta)?);
    }
    Ok(findings)
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

/// Map crash characteristics to (severity, category_tag). Spec
/// invariants fire when `error_code` is None and the last action was
/// reported `success: false` *only* because of the post-action assert
/// — we don't have an in-band signal so the heuristic is "no error
/// code on last action means assert tripped." Refinement: re-run the
/// crash via `crucible show --replay` and parse the FUZZ_FINDING line
/// for the actual assertion. Deferred to v2.18.1.
pub fn categorize_crash(meta: &CrucibleCrashMetadata) -> (Severity, &'static str) {
    let last = meta.actions.last();
    match last {
        Some(a) if !a.success && a.error_code.is_some() => {
            // Handler aborted with an Anchor error code. Could be a
            // genuine bug (e.g. overflow path returning Custom(N) on a
            // success path) or a spec-silent error path. Medium until
            // we know which.
            (Severity::Medium, "runtime_abort")
        }
        Some(a) if !a.success => {
            // No error code → not a clean Anchor abort. Likely a panic
            // (zero-div, slice out-of-bounds) or runtime fault.
            (Severity::Medium, "runtime_panic")
        }
        _ => {
            // Last action reported success but a crash was recorded —
            // the post-action `fuzz_assert!` fired. This is the
            // canonical "spec invariant violated" path.
            (Severity::High, "invariant_violation")
        }
    }
}

/// Best-effort handler name from the crash: the last action's name.
/// Stateful chains may have the bug latent earlier in the sequence;
/// the user gets the chain via `action_sequence` for inspection.
pub fn derive_handler_for_crash(meta: &CrucibleCrashMetadata) -> String {
    meta.actions
        .last()
        .map(|a| a.name.trim_start_matches("action_").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Dedupe key: `(handler, category_tag, error_code-or-zero)`. Same
/// handler + same category + same error code → same finding class.
/// v0 over-dedupes when one handler triggers multiple distinct
/// invariant fires (no in-band assertion-message); v0.1 will use the
/// re-run assertion message instead.
/// v0 key used by `dedupe_findings` when it has direct access to the
/// crash metadata. The Finding-side path uses `finding_dedupe_key`
/// which reconstructs a synthetic crash from `Reproducer::Crucible`.
/// Kept public for tests + future direct callers (e.g. an incremental
/// dedupe streaming from `crucible run` stdout).
#[allow(dead_code)]
pub fn dedupe_key_for_crash(meta: &CrucibleCrashMetadata) -> (String, &'static str, u32) {
    let (_, tag) = categorize_crash(meta);
    let last = meta.actions.last();
    let err = last.and_then(|a| a.error_code).unwrap_or(0);
    (derive_handler_for_crash(meta), tag, err)
}

/// Collapse same-class findings: first crash becomes the canonical
/// reproducer; subsequent crashes contribute their `.meta.json` path
/// to `extra_seeds`.
pub fn dedupe_findings(findings: Vec<Finding>) -> Vec<Finding> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<(String, String, u32), Finding> = BTreeMap::new();
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

/// (handler, category_tag, error_code) for the finding's reproducer.
/// Mirrors `dedupe_key_for_crash` but pulls from `Finding` state.
fn finding_dedupe_key(f: &Finding) -> (String, String, u32) {
    let (tag, err) = match &f.reproducer {
        Some(Reproducer::Crucible {
            action_sequence, ..
        }) => {
            let last = action_sequence.last();
            let err = last.and_then(|a| a.error_code).unwrap_or(0);
            let synth = CrucibleCrashMetadata {
                test_name: String::new(),
                timestamp: String::new(),
                iteration: 0,
                seed: None,
                actions: action_sequence.clone(),
            };
            let (_, tag) = categorize_crash(&synth);
            (tag.to_string(), err)
        }
        _ => ("unknown".to_string(), 0),
    };
    (f.handler.clone(), tag, err)
}

fn crash_path_from_reproducer(f: &Finding) -> Option<String> {
    match &f.reproducer {
        Some(Reproducer::Crucible { crash_path, .. }) => Some(crash_path.clone()),
        _ => None,
    }
}

/// Build a `Finding` from a parsed crash. `harness_dir` and `crash_path`
/// are persisted on the reproducer so the user can re-run.
fn finding_from_crash(
    harness_dir: &Path,
    crash_path: &Path,
    meta: &CrucibleCrashMetadata,
) -> Result<Finding> {
    let (severity, tag) = categorize_crash(meta);
    let handler = derive_handler_for_crash(meta);
    let id = stable_finding_id(harness_dir, &handler, tag, meta);
    let invocation = format!(
        "crucible show {} {} --replay",
        harness_dir.display(),
        crash_path.display()
    );
    let crucible_version = crucible_version().unwrap_or_else(|| "unknown".to_string());

    Ok(Finding {
        id,
        category: Category::CrucibleFuzzCrash,
        severity,
        handler,
        spec_silent_on: format!(
            "fuzz-discovered path triggers `{tag}`. The spec is silent on this case."
        ),
        suppression_hint: "add a `requires` / `aborts_if` clause covering this input, \
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
        }),
        gated_by: None,
    })
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

/// Symlink `target/idl/<prog>.json` into `<harness>/idls/<prog>.json`
/// when present. The IDL filename is derived from the harness dir name
/// (which matches the spec's snake-case program_name). Idempotent — a
/// pre-existing IDL file is left alone.
pub fn discover_idl(harness_dir: &Path, project_root: &Path) -> Result<()> {
    let prog = harness_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("harness_dir has no leaf name"))?;
    let target_idl = project_root
        .join("target")
        .join("idl")
        .join(format!("{prog}.json"));
    if !target_idl.exists() {
        return Ok(()); // nothing to discover; user wires manually
    }
    let dest_dir = harness_dir.join("idls");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(format!("{prog}.json"));
    if dest.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target_idl, &dest)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&target_idl, &dest)?;
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

fn run_crucible(harness_dir: &Path, budget: Duration, stateful: bool) -> Result<PathBuf> {
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
    let status = cmd.status().context("spawning `crucible run`")?;
    // Non-zero exit from crucible can mean "found crashes" rather than
    // a runtime failure. Don't bail on non-success; harvest the crashes
    // dir regardless and let the caller decide based on findings count.
    let _ = status;
    Ok(harness_dir.join("crashes").join(HARNESS_TEST_NAME))
}

/// Minimize every crash for this test in one shot via `crucible tmin --all`.
/// Replaces the per-crash invocation we had before — Crucible's tmin
/// expects `<CRASH_FILE>` as a filename relative to the crashes dir,
/// not a full path, and has no `--timeout` flag at all. The `--all`
/// form sidesteps both issues and runs in a single subprocess.
///
/// `_unused_per_crash_cap` is retained for ABI stability with callers
/// that pass `TMIN_BUDGET_PER_CRASH` — Crucible's tmin runs to completion
/// on each crash via forward-pass removal; there is no wall-clock dial
/// today. If real runs surface a need to cap it, wrap the spawn in a
/// `tokio::time::timeout` here.
fn auto_tmin_all(harness_dir: &Path, _unused_per_crash_cap: Duration) -> Result<()> {
    let prog = harness_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("harness_dir has no leaf name"))?;
    let _ = Command::new("crucible")
        .arg("tmin")
        .arg(prog)
        .arg(HARNESS_TEST_NAME)
        .arg("--all")
        .arg("-C")
        .arg(harness_dir)
        .status();
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{CrucibleActionRecord, CrucibleCrashMetadata};
    use serde_json::json;

    /// Real `.meta.json` captured from `crucible run` on Crucible's own
    /// bundled escrow example (commit 689e63a). Validates that our parser
    /// + categorize + handler-derive paths handle real crash output, not
    ///   just synthetic test data.
    const REAL_CRASH_META: &str = include_str!("../../test-fixtures/real-crucible-crash.meta.json");

    #[test]
    fn parses_real_crucible_crash_metadata() {
        let meta = parse_crash_metadata(REAL_CRASH_META.as_bytes()).expect("parse");
        assert_eq!(meta.test_name, "invariant_escrow");
        assert_eq!(meta.actions.len(), 6);
        // Last action: withdraw(amount=3), success=true → seeded
        // bug-firing case (post-action assert tripped, no error_code
        // because the handler returned Ok).
        let last = meta.actions.last().unwrap();
        assert_eq!(last.name, "withdraw");
        assert!(last.success);
        assert!(last.error_code.is_none());
    }

    #[test]
    fn real_crash_categorizes_as_high_invariant_violation() {
        let meta = parse_crash_metadata(REAL_CRASH_META.as_bytes()).expect("parse");
        let (sev, tag) = categorize_crash(&meta);
        assert!(matches!(sev, Severity::High));
        assert_eq!(tag, "invariant_violation");
    }

    #[test]
    fn real_crash_derives_withdraw_as_handler() {
        let meta = parse_crash_metadata(REAL_CRASH_META.as_bytes()).expect("parse");
        // Action names in real Crucible output don't carry the `action_`
        // prefix (we strip it defensively anyway).
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
