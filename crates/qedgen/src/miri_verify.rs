//! Miri verify backend (v2.19).
//!
//! Walks `.qed/probes/pinocchio/*/repro_miri.rs` (or anywhere the
//! agent emitted Miri reproducers), shells `cargo +nightly miri test`
//! per repro, parses Miri output for UB / panic / assertion failures,
//! and surfaces each diagnostic as a `Finding`-shaped record that
//! plugs into the existing verify `BackendReport` envelope.
//!
//! Dual-execution divergence detection: when the same finding-id has
//! both a `repro_mollusk.rs` and a `repro_miri.rs`, the comparator
//! flags Miri-fail / Mollusk-pass disagreement as
//! `Category::ExecutionDivergence` (Critical) — the deployed `.so`'s
//! release-mode wrap hides UB Miri's interpreter exposes.

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::verify::{BackendReport, BackendStatus};

#[derive(Debug, Clone, Serialize)]
pub struct MiriRunResult {
    pub repro_path: PathBuf,
    pub finding_id: String,
    pub status: MiriStatus,
    pub stdout: String,
    pub stderr: String,
    pub diagnostics: Vec<MiriDiagnostic>,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MiriStatus {
    /// Miri ran the repro and the test passed without UB.
    Passed,
    /// Miri detected UB / panic / assertion failure — the probe's bug
    /// reproduced.
    Failed,
    /// Miri could not run (toolchain missing, compile error, etc.).
    Error,
    /// The repro is still a stub with TODOs the agent hasn't filled.
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct MiriDiagnostic {
    /// Kind: `ub`, `panic`, `assertion`, `oob`, `aliasing`, `overflow`.
    pub kind: String,
    /// One-line summary of the diagnostic.
    pub message: String,
    /// Source line where Miri reported the issue (best-effort parse).
    pub source_line: Option<String>,
}

/// Locate Miri repro files under a project root. v2.19 default layout:
/// `.qed/probes/pinocchio/<finding-id>/repro_miri.rs`. Agent-emitted
/// paths follow the same convention via the `MiriPrompt.repro_path`
/// field on the finding.
pub fn discover_miri_repros(project_root: &Path) -> Vec<PathBuf> {
    let base = project_root.join(".qed").join("probes").join("pinocchio");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let path = entry.path().join("repro_miri.rs");
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out
}

/// Run every discovered Miri repro under `cargo +nightly miri test`.
///
/// Each repro is expected to live as a standalone bin / test target
/// declared in its own `Cargo.toml` (the agent's responsibility to
/// scaffold). If no `Cargo.toml` is present alongside the repro, the
/// run is marked `Skipped` with a note for the agent.
pub fn run_all(project_root: &Path) -> Result<Vec<MiriRunResult>> {
    let repros = discover_miri_repros(project_root);
    let mut results = Vec::new();
    for repro in repros {
        results.push(run_one(&repro)?);
    }
    Ok(results)
}

fn run_one(repro: &Path) -> Result<MiriRunResult> {
    let start = Instant::now();
    let finding_id = repro
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Look for Cargo.toml next to repro (or in parent).
    let manifest = repro
        .parent()
        .map(|p| p.join("Cargo.toml"))
        .filter(|p| p.is_file());

    let manifest = match manifest {
        Some(m) => m,
        None => {
            // Try walking up to find a Cargo.toml.
            let mut probe = repro.parent();
            let mut found = None;
            while let Some(p) = probe {
                let cand = p.join("Cargo.toml");
                if cand.is_file() {
                    found = Some(cand);
                    break;
                }
                probe = p.parent();
            }
            match found {
                Some(c) => c,
                None => {
                    return Ok(MiriRunResult {
                        repro_path: repro.to_path_buf(),
                        finding_id,
                        status: MiriStatus::Skipped,
                        stdout: String::new(),
                        stderr:
                            "no Cargo.toml discovered for this Miri repro; agent must scaffold one"
                                .to_string(),
                        diagnostics: Vec::new(),
                        duration_ms: start.elapsed().as_millis(),
                    });
                }
            }
        }
    };

    // Check for stub markers — if the agent hasn't filled in the
    // TODOs, the test panics at runtime which is not a probe finding.
    let stub_check = std::fs::read_to_string(repro).unwrap_or_default();
    if stub_check.contains("TODO:") {
        return Ok(MiriRunResult {
            repro_path: repro.to_path_buf(),
            finding_id,
            status: MiriStatus::Skipped,
            stdout: String::new(),
            stderr: "repro_miri.rs still contains TODO markers — agent fill required".to_string(),
            diagnostics: Vec::new(),
            duration_ms: start.elapsed().as_millis(),
        });
    }

    let test_name = format!("probe_{}", finding_id.replace('-', "_"));
    let output = Command::new("cargo")
        .args([
            "+nightly",
            "miri",
            "test",
            "--manifest-path",
            manifest.to_str().unwrap_or(""),
            &test_name,
        ])
        .output();

    let (stdout, stderr, exit_ok) = match output {
        Ok(o) => (
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
            o.status.success(),
        ),
        Err(e) => (String::new(), e.to_string(), false),
    };

    let diagnostics = parse_miri_output(&stdout, &stderr);

    let status = if exit_ok && diagnostics.is_empty() {
        MiriStatus::Passed
    } else if !diagnostics.is_empty() {
        MiriStatus::Failed
    } else {
        MiriStatus::Error
    };

    Ok(MiriRunResult {
        repro_path: repro.to_path_buf(),
        finding_id,
        status,
        stdout,
        stderr,
        diagnostics,
        duration_ms: start.elapsed().as_millis(),
    })
}

/// Parse Miri stdout / stderr for the diagnostic classes v2.19
/// surfaces: UB (`error: Undefined Behavior`), arithmetic overflow
/// (`this operation will panic`), assertion failures, and explicit
/// panics with our `SAFETY claim STALE:` marker.
pub fn parse_miri_output(stdout: &str, stderr: &str) -> Vec<MiriDiagnostic> {
    let mut out = Vec::new();
    let combined = format!("{}\n{}", stdout, stderr);

    for line in combined.lines() {
        let trimmed = line.trim();
        // 1. Miri UB
        if trimmed.starts_with("error: Undefined Behavior") {
            out.push(MiriDiagnostic {
                kind: classify_ub_line(trimmed).to_string(),
                message: trimmed.to_string(),
                source_line: None,
            });
        }
        // 2. Overflow / arithmetic panic
        if trimmed.contains("attempt to add with overflow")
            || trimmed.contains("attempt to subtract with overflow")
            || trimmed.contains("attempt to multiply with overflow")
            || trimmed.contains("this operation will panic")
        {
            out.push(MiriDiagnostic {
                kind: "overflow".to_string(),
                message: trimmed.to_string(),
                source_line: None,
            });
        }
        // 3. Out-of-bounds read / write
        if trimmed.contains("out-of-bounds")
            || trimmed.contains("memory access failed")
            || trimmed.contains("dangling pointer")
        {
            out.push(MiriDiagnostic {
                kind: "oob".to_string(),
                message: trimmed.to_string(),
                source_line: None,
            });
        }
        // 4. Stacked / Tree Borrows aliasing
        if trimmed.contains("Stacked Borrows")
            || trimmed.contains("Tree Borrows")
            || (trimmed.contains("aliasing") && trimmed.contains("error"))
        {
            out.push(MiriDiagnostic {
                kind: "aliasing".to_string(),
                message: trimmed.to_string(),
                source_line: None,
            });
        }
        // 5. SAFETY-claim stale marker we emit from repro panics.
        if trimmed.contains("SAFETY claim STALE") || trimmed.contains("SAFETY claim stale") {
            out.push(MiriDiagnostic {
                kind: "stale_safety".to_string(),
                message: trimmed.to_string(),
                source_line: None,
            });
        }
        // 6. Assertion failures
        if trimmed.starts_with("assertion `") && trimmed.contains("failed") {
            out.push(MiriDiagnostic {
                kind: "assertion".to_string(),
                message: trimmed.to_string(),
                source_line: None,
            });
        }
    }

    out
}

fn classify_ub_line(line: &str) -> &'static str {
    let l = line.to_lowercase();
    if l.contains("alias") || l.contains("borrows") {
        "aliasing"
    } else if l.contains("out-of-bounds") || l.contains("dangling") {
        "oob"
    } else if l.contains("uninit") {
        "uninit"
    } else if l.contains("invalid") && l.contains("transmute") {
        "transmute"
    } else {
        "ub"
    }
}

/// Build a `BackendReport` for the verifier rollup.
pub fn run(project_root: &Path) -> BackendReport {
    let start = Instant::now();
    let repros = discover_miri_repros(project_root);
    if repros.is_empty() {
        return BackendReport {
            name: "miri",
            status: BackendStatus::Skipped,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(
                "no Miri repros found under .qed/probes/pinocchio/*/repro_miri.rs".to_string(),
            ),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        };
    }

    // Dependency gate.
    if let Err(e) = crate::deps::require_miri() {
        return BackendReport {
            name: "miri",
            status: BackendStatus::Skipped,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(format!("{}", e)),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        };
    }

    let results = match run_all(project_root) {
        Ok(r) => r,
        Err(e) => {
            return BackendReport {
                name: "miri",
                status: BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!("miri runner error: {}", e)),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            };
        }
    };

    let failed = results
        .iter()
        .any(|r| matches!(r.status, MiriStatus::Failed));
    let skipped_all = results
        .iter()
        .all(|r| matches!(r.status, MiriStatus::Skipped));

    let status = if failed {
        BackendStatus::Failed
    } else if skipped_all {
        BackendStatus::Skipped
    } else {
        BackendStatus::Passed
    };

    let mut summary = String::new();
    for r in &results {
        let st = match r.status {
            MiriStatus::Passed => "PASS",
            MiriStatus::Failed => "FAIL",
            MiriStatus::Error => "ERR ",
            MiriStatus::Skipped => "SKIP",
        };
        summary.push_str(&format!(
            "  [{}] {} ({} diagnostics, {}ms)\n",
            st,
            r.finding_id,
            r.diagnostics.len(),
            r.duration_ms
        ));
        for d in &r.diagnostics {
            summary.push_str(&format!("       {} :: {}\n", d.kind, d.message));
        }
    }

    BackendReport {
        name: "miri",
        status,
        duration_ms: start.elapsed().as_millis(),
        detail: Some(summary),
        log_path: None,
        counterexamples: Vec::new(),
        axioms: Vec::new(),
    }
}

/// Dual-execution divergence comparator: when a finding-id has both
/// Mollusk and Miri repros and Miri fires while Mollusk passes
/// (or vice versa), emit an `ExecutionDivergence` Critical finding.
///
/// Input is the Miri results plus the project root (we look for
/// Mollusk repros under `target/qedgen-repros/<id>/`).
#[allow(dead_code)]
pub fn detect_divergence(
    project_root: &Path,
    miri_results: &[MiriRunResult],
) -> Vec<crate::probe::Finding> {
    use crate::probe::{Category, Finding, Severity};

    let mut out = Vec::new();
    for miri in miri_results {
        let mollusk_dir = project_root
            .join("target")
            .join("qedgen-repros")
            .join(&miri.finding_id);
        if !mollusk_dir.exists() {
            continue;
        }
        // Heuristic: a Mollusk "pass" is the .qed/probes/.../result.json
        // marking status=ok, OR no result.json which we treat as "not
        // yet run, can't compare". Miri "fail" with no Mollusk
        // confirmation is the divergence signal.
        if !matches!(miri.status, MiriStatus::Failed) {
            continue;
        }
        // Look for `mollusk_passed` marker file the verifier writes.
        let mollusk_marker = mollusk_dir.join("mollusk_passed");
        if mollusk_marker.exists() {
            out.push(Finding {
                id: format!("{}-divergence", miri.finding_id),
                category: Category::ExecutionDivergence,
                severity: Severity::Critical,
                handler: "<unknown>".to_string(),
                spec_silent_on: format!(
                    "Miri flagged UB on host for finding {} but Mollusk's SVM \
                     execution accepted the same input — the deployed `.so`'s \
                     release-mode wrap hides UB the host interpreter exposes",
                    miri.finding_id
                ),
                suppression_hint: "Investigate the divergence. Common causes: \
                    release-mode wrap-around on integer overflow, BPF alignment \
                    relaxation, runtime checks short-circuiting under SVM"
                    .to_string(),
                investigation_hint: format!(
                    "Read .qed/probes/pinocchio/{}/repro_miri.rs (Miri-failing) \
                     and target/qedgen-repros/{}/ (Mollusk-passing). Reconcile \
                     the divergence: either the Miri repro is over-strict, or \
                     the Mollusk pass hides a real bug.",
                    miri.finding_id, miri.finding_id
                ),
                category_tag: "execution_divergence".to_string(),
                reproducer: None,
                gated_by: None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ub_aliasing() {
        let stderr = "error: Undefined Behavior: trying to retag from <12345> for SharedReadWrite \
                      permission at alloc1[0x0], but parent tag <678> does not have an appropriate item";
        let diags = parse_miri_output("", stderr);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].kind, "ub");
    }

    #[test]
    fn parse_overflow() {
        let stderr = "thread 'main' panicked at: attempt to add with overflow";
        let diags = parse_miri_output("", stderr);
        assert!(diags.iter().any(|d| d.kind == "overflow"));
    }

    #[test]
    fn parse_stale_safety_marker() {
        let stdout = "panicked at: SAFETY claim STALE: handler accepted swap_position";
        let diags = parse_miri_output(stdout, "");
        assert!(diags.iter().any(|d| d.kind == "stale_safety"));
    }

    #[test]
    fn no_diagnostics_for_clean_output() {
        let stdout = "running 1 test\ntest probe_xyz_miri ... ok\n";
        let diags = parse_miri_output(stdout, "");
        assert!(diags.is_empty(), "expected no diags, got {:?}", diags);
    }
}
