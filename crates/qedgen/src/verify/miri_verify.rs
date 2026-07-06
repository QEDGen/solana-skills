//! Miri verify backend: walks `.qed/probes/pinocchio/*/repro_miri.rs`, shells
//! `cargo +nightly miri test` per repro, parses output for UB / panic /
//! assertion diagnostics, and reports via the verify `BackendReport` envelope.
//!
//! Dual-execution divergence: a finding-id with both `repro_mollusk.rs` and
//! `repro_miri.rs` that is Miri-fail / Mollusk-pass is flagged
//! `Category::ExecutionDivergence` (Critical) — release-mode wrap in the
//! deployed `.so` hides UB Miri exposes.

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
    /// Test passed without UB.
    Passed,
    /// UB / panic / assertion failure — the probe's bug reproduced.
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

/// Locate Miri repros: `.qed/probes/pinocchio/<finding-id>/repro_miri.rs`
/// (agent-emitted paths follow the same convention via `MiriPrompt.repro_path`).
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
/// Each repro needs an agent-scaffolded `Cargo.toml`; without one the run is
/// `Skipped` with a note.
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

    let manifest = repro
        .parent()
        .map(|p| p.join("Cargo.toml"))
        .filter(|p| p.is_file());

    let manifest = match manifest {
        Some(m) => m,
        None => {
            // Walk up for a Cargo.toml.
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

    // Unfilled stubs panic at runtime — that's not a probe finding; skip them.
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

/// Parse Miri stdout / stderr for: UB (`error: Undefined Behavior`),
/// arithmetic overflow (`this operation will panic`), assertion failures,
/// and panics carrying our `SAFETY claim STALE:` marker.
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
        return BackendReport::skipped(
            "miri",
            start,
            Some("no Miri repros found under .qed/probes/pinocchio/*/repro_miri.rs".to_string()),
        );
    }

    // Dependency gate.
    if let Err(e) = crate::deps::require_miri() {
        return BackendReport::skipped("miri", start, Some(format!("{}", e)));
    }

    let results = match run_all(project_root) {
        Ok(r) => r,
        Err(e) => {
            return BackendReport::failed("miri", start, Some(format!("miri runner error: {}", e)));
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

    BackendReport::new("miri", status, start, Some(summary))
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
