//! `qedgen verify --probe-repros`: walks `<project_root>/target/qedgen-repros/`,
//! runs each per-finding reproducer test, reports `(finding_id, status)` so
//! consumers can gate which findings surface. Repro layout isn't pinned yet,
//! so both shapes are supported: a single shared crate
//! (`Cargo.toml` + `tests/probe_<finding_id>.rs`) or one crate per finding
//! (`<finding_id>/Cargo.toml`). With no repros dir, emits a structured `note`.
//!
//! Inverted exit semantics — each repro's `#[test]` asserts the bug is
//! observable: cargo test pass → **Fired** (bug reproduced); assertion failure
//! → **Silent** (finding suppressed, reproducible-only contract); build failure
//! → **BuildError** (no verdict, finding stays structural).
//!
//! Does not generate repros, modify the probe report, or run Mollusk directly
//! (repros depend on `qedgen-sandbox`; this just orchestrates cargo).

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // NoRepros reserved for future per-result use; today emitted only via top-level note
pub enum ReproStatus {
    /// cargo test passed — assertion held, bug reproduced.
    Fired,
    /// Assertion failed — bug not reproduced; finding dropped per the
    /// reproducible-only contract.
    Silent,
    /// Build failure — no evidence either way; finding stays structural.
    BuildError,
    /// No repros under `target/qedgen-repros/`.
    NoRepros,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReproResult {
    /// From `tests/probe_<id>.rs` or the `<id>/` directory name.
    pub finding_id: String,
    pub status: ReproStatus,
    /// cargo test output excerpt for triage; `Silent` / `BuildError` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_excerpt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProbeReproReport {
    pub repros_dir: PathBuf,
    pub results: Vec<ReproResult>,
    pub duration_ms: u128,
    /// Note for non-result outcomes (no repros found, shared-crate build failed, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProbeReproReport {
    /// A successful repro-verification run must have executed at least one
    /// reproducer and every result must have fired. No repros and build
    /// errors are inconclusive, never vacuous success.
    pub fn all_fired(&self) -> bool {
        !self.results.is_empty()
            && self
                .results
                .iter()
                .all(|r| matches!(r.status, ReproStatus::Fired))
    }
}

/// Discover and run probe reproducers under `<project_root>/target/qedgen-repros/`.
pub fn run(project_root: &Path) -> Result<ProbeReproReport> {
    let start = Instant::now();
    let repros_dir = project_root.join("target").join("qedgen-repros");

    if !repros_dir.exists() {
        return Ok(ProbeReproReport {
            repros_dir,
            results: Vec::new(),
            duration_ms: start.elapsed().as_millis(),
            note: Some(
                "no repros found at target/qedgen-repros/ — run `qedgen probe --spec <path>` to generate supported category repros, or add an auditor-authored repro under target/qedgen-repros/audit/".to_string(),
            ),
        });
    }

    // Shared crate (`<repros_dir>/Cargo.toml`) vs per-finding crates
    // (`<repros_dir>/<id>/Cargo.toml`).
    if repros_dir.join("Cargo.toml").exists() {
        return Ok(run_shared_crate(&repros_dir, start));
    }

    let entries = std::fs::read_dir(&repros_dir)
        .with_context(|| format!("read_dir {}", repros_dir.display()))?;
    let mut results = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("Cargo.toml").exists() {
            continue;
        }
        let finding_id = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        results.push(run_one_repro_crate(&finding_id, &path));
    }

    let note = if results.is_empty() {
        Some(format!(
            "{} exists but contains no runnable repro crate (expected a shared Cargo.toml or per-repro subdirectories with Cargo.toml)",
            repros_dir.display()
        ))
    } else {
        None
    };

    Ok(ProbeReproReport {
        repros_dir,
        results,
        duration_ms: start.elapsed().as_millis(),
        note,
    })
}

fn run_shared_crate(repros_dir: &Path, start: Instant) -> ProbeReproReport {
    let output = Command::new("cargo")
        .args(["test", "--release"])
        .current_dir(repros_dir)
        .output();

    let duration_ms = start.elapsed().as_millis();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            // TODO(D3): parse libtest per-test lines (`test probe_<id> ... ok|FAILED`)
            // for per-finding results; until then surface a coarse verdict in `note`.
            let (status, note) = if out.status.success() {
                (
                    ReproStatus::Fired,
                    Some(
                        "shared-crate repros all passed — per-finding parsing pending D3"
                            .to_string(),
                    ),
                )
            } else {
                let status = if looks_like_build_error(&stderr) {
                    ReproStatus::BuildError
                } else {
                    ReproStatus::Silent
                };
                (
                    status,
                    Some(format!(
                        "shared-crate repros had failures — per-finding parsing pending D3:\n{}\n{}",
                        tail(&stdout, 20),
                        tail(&stderr, 10)
                    )),
                )
            };
            ProbeReproReport {
                repros_dir: repros_dir.to_path_buf(),
                results: vec![ReproResult {
                    finding_id: "shared-crate".to_string(),
                    status,
                    log_excerpt: None,
                }],
                duration_ms,
                note,
            }
        }
        Err(e) => ProbeReproReport {
            repros_dir: repros_dir.to_path_buf(),
            results: Vec::new(),
            duration_ms,
            note: Some(format!("failed to spawn cargo: {}", e)),
        },
    }
}

fn run_one_repro_crate(finding_id: &str, crate_dir: &Path) -> ReproResult {
    let output = Command::new("cargo")
        .args(["test", "--release"])
        .current_dir(crate_dir)
        .output();

    match output {
        Ok(out) if out.status.success() => ReproResult {
            finding_id: finding_id.to_string(),
            status: ReproStatus::Fired,
            log_excerpt: None,
        },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let status = if looks_like_build_error(&stderr) {
                ReproStatus::BuildError
            } else {
                ReproStatus::Silent
            };
            ReproResult {
                finding_id: finding_id.to_string(),
                status,
                log_excerpt: Some(format!("{}\n---\n{}", tail(&stdout, 10), tail(&stderr, 10))),
            }
        }
        Err(e) => ReproResult {
            finding_id: finding_id.to_string(),
            status: ReproStatus::BuildError,
            log_excerpt: Some(format!("failed to spawn cargo: {}", e)),
        },
    }
}

fn looks_like_build_error(stderr: &str) -> bool {
    // cargo prints `error[E…]:` / `error: could not compile` for compile errors;
    // test failures show up in stdout, not these markers.
    stderr.contains("error[E") || stderr.contains("could not compile")
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub fn print_human(report: &ProbeReproReport) {
    eprintln!(
        "qedgen verify --probe-repros — {}",
        report.repros_dir.display()
    );
    if let Some(note) = &report.note {
        eprintln!("  note: {}", note);
    }
    for r in &report.results {
        let marker = match r.status {
            ReproStatus::Fired => "FIRED",
            ReproStatus::Silent => "silent",
            ReproStatus::BuildError => "BUILD?",
            ReproStatus::NoRepros => "—",
        };
        eprintln!("  [{}] {}", marker, r.finding_id);
        if let Some(log) = &r.log_excerpt {
            for line in log.lines().take(5) {
                eprintln!("         {}", line);
            }
        }
    }
    eprintln!(
        "  total: {} repros, {} ms",
        report.results.len(),
        report.duration_ms
    );
}

pub fn print_json(report: &ProbeReproReport) -> Result<()> {
    let s = serde_json::to_string_pretty(report).context("serializing probe-repros report")?;
    println!("{}", s);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_repros_dir_returns_no_repros_note() {
        let tmp = tempdir().unwrap();
        let report = run(tmp.path()).unwrap();
        assert!(report.results.is_empty());
        assert!(report.note.is_some());
        assert!(report.note.as_ref().unwrap().contains("no repros found"));
    }

    #[test]
    fn empty_repros_dir_explains_expected_layout() {
        let tmp = tempdir().unwrap();
        let repros_dir = tmp.path().join("target").join("qedgen-repros");
        fs::create_dir_all(&repros_dir).unwrap();
        let report = run(tmp.path()).unwrap();
        assert!(report.results.is_empty());
        assert!(report
            .note
            .as_ref()
            .unwrap()
            .contains("no runnable repro crate"));
    }

    #[test]
    fn looks_like_build_error_recognizes_error_codes() {
        assert!(looks_like_build_error(
            "error[E0432]: unresolved import\n  --> src/lib.rs"
        ));
        assert!(looks_like_build_error(
            "error: could not compile `foo` due to 3 previous errors"
        ));
        assert!(!looks_like_build_error(
            "test result: FAILED. 0 passed; 1 failed"
        ));
    }

    #[test]
    fn all_fired_rejects_build_errors() {
        let report = ProbeReproReport {
            repros_dir: PathBuf::from("/tmp"),
            results: vec![
                ReproResult {
                    finding_id: "a".to_string(),
                    status: ReproStatus::Fired,
                    log_excerpt: None,
                },
                ReproResult {
                    finding_id: "b".to_string(),
                    status: ReproStatus::BuildError,
                    log_excerpt: None,
                },
            ],
            duration_ms: 0,
            note: None,
        };
        assert!(!report.all_fired());
    }

    #[test]
    fn all_fired_fails_on_silent() {
        let report = ProbeReproReport {
            repros_dir: PathBuf::from("/tmp"),
            results: vec![ReproResult {
                finding_id: "a".to_string(),
                status: ReproStatus::Silent,
                log_excerpt: None,
            }],
            duration_ms: 0,
            note: None,
        };
        assert!(!report.all_fired());
    }

    #[test]
    fn all_fired_requires_at_least_one_reproducer() {
        let report = ProbeReproReport {
            repros_dir: PathBuf::from("/tmp"),
            results: Vec::new(),
            duration_ms: 0,
            note: Some("no repros".to_string()),
        };
        assert!(!report.all_fired());
    }
}
