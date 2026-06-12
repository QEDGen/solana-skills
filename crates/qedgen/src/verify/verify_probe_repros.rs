//! `qedgen verify --probe-repros` runner. PLAN-v2.16 D4.
//!
//! Walks `<project_root>/target/qedgen-repros/`, runs each per-finding
//! reproducer test, and reports `(finding_id, status)` so downstream
//! consumers (auditor subagent, the next probe invocation) can gate
//! which findings make it into the surfaced report.
//!
//! ## What lives under `target/qedgen-repros/`
//!
//! Per PLAN-v2.16 D3 (scheduled after D4), each probe finding gets a
//! reproducer materialized as a Rust integration test that calls the
//! user's deployed handler via `qedgen-sandbox` (Mollusk in-process
//! SVM). The directory layout D3 will use isn't yet pinned; this
//! runner supports both shapes:
//!
//! 1. **Single shared crate** — `target/qedgen-repros/Cargo.toml`
//!    with `tests/probe_<finding_id>.rs` per finding. Single
//!    `cargo test` invocation runs all repros (fast).
//! 2. **One crate per finding** — `target/qedgen-repros/<finding_id>/Cargo.toml`
//!    each, with the test inside. One `cargo test` per finding (slow
//!    but isolated).
//!
//! Until D3 ships, the runner is essentially a no-op (the directory
//! doesn't exist), and emits a structured `note` saying so. This
//! matches the v2.16 staging plan: D4 lands the orchestration; D3
//! lands the repros that flow through it.
//!
//! ## What "fired" means
//!
//! Each repro is a `#[test]` whose body asserts the bug is observable
//! (e.g. `assert!(result.program_result.is_err(), "expected
//! MathOverflow")` or `assert_eq!(post_state.balance, 0,
//! "expected wrap to drain balance")`). Mapping cargo test's exit
//! semantics to our model:
//!
//! - cargo test passes → assertion held → **bug reproduced (Fired)**
//! - cargo test fails an assertion → **bug not reproduced (Silent)** →
//!   the corresponding probe finding is suppressed (no advisory tier,
//!   per `feedback_probes_reproducible_only.md`)
//! - cargo test fails to build → **BuildError** → the finding stays
//!   structural (we have no evidence either way; treat as build
//!   flakiness, not a verdict)
//!
//! ## What this module deliberately does NOT do
//!
//! - It does not generate repros (that's D3).
//! - It does not modify the probe report (the consumer reads our JSON
//!   and gates the probe report itself; we don't reach back into
//!   `probe.rs`).
//! - It does not run Mollusk directly. The repros use the
//!   `qedgen-sandbox` crate as their dep — that's the Mollusk surface;
//!   we just orchestrate cargo.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // NoRepros reserved for future per-result use; today emitted only via top-level note
pub enum ReproStatus {
    /// Cargo test passed — the repro's assertion held, bug is reproducible.
    Fired,
    /// Cargo test failed an assertion — repro couldn't reproduce the bug.
    /// Per the reproducible-only contract, the corresponding probe
    /// finding is dropped from the surfaced report.
    Silent,
    /// Cargo test failed to build (compile error, missing dep, etc.).
    /// Insufficient evidence to fire OR drop — finding stays structural.
    BuildError,
    /// No repros under `target/qedgen-repros/`. v2.16-pre-D3 baseline.
    NoRepros,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReproResult {
    /// Finding id — derived from `tests/probe_<id>.rs` or
    /// `<id>/Cargo.toml` directory name.
    pub finding_id: String,
    pub status: ReproStatus,
    /// Short excerpt from cargo test stderr/stdout for human triage.
    /// Only populated for `Silent` and `BuildError`; `Fired` doesn't
    /// need a log because the repro confirmed the bug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_excerpt: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProbeReproReport {
    pub repros_dir: PathBuf,
    pub results: Vec<ReproResult>,
    pub duration_ms: u128,
    /// Human-readable note for non-result outcomes (no repros found,
    /// shared-crate cargo build failed before any test ran, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ProbeReproReport {
    /// True if every repro that ran fired (i.e. every claimed bug was
    /// reproduced). `BuildError` results are treated as "no verdict"
    /// — neither a fire nor a silent — so they don't fail this check.
    /// `NoRepros` is vacuously true.
    pub fn all_fired_or_inconclusive(&self) -> bool {
        self.results
            .iter()
            .all(|r| !matches!(r.status, ReproStatus::Silent))
    }
}

/// Discover and run probe reproducers under `<project_root>/target/qedgen-repros/`.
///
/// Returns a structured report. The CLI prints it as JSON (or a short
/// human summary), and the auditor subagent consumes it via stdin/file.
pub fn run(project_root: &Path) -> Result<ProbeReproReport> {
    let start = Instant::now();
    let repros_dir = project_root.join("target").join("qedgen-repros");

    if !repros_dir.exists() {
        return Ok(ProbeReproReport {
            repros_dir,
            results: Vec::new(),
            duration_ms: start.elapsed().as_millis(),
            note: Some(
                "no repros found at target/qedgen-repros/ — run `qedgen probe` first to generate them (D3 scheduled for v2.16)".to_string(),
            ),
        });
    }

    // Distinguish the two layouts D3 may pick:
    // - shared crate: `<repros_dir>/Cargo.toml` exists
    // - per-finding crate: `<repros_dir>/<id>/Cargo.toml` exists
    if repros_dir.join("Cargo.toml").exists() {
        return Ok(run_shared_crate(&repros_dir, start));
    }

    // Per-finding-crate layout: walk subdirs, run cargo test in each
    // that has a Cargo.toml.
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
            "{} exists but no repro crates found inside — D3 not yet wired",
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
            // Shared-crate parsing requires reading libtest's per-test
            // outcome lines (`test probe_<id> ... ok|FAILED`). For v2.16
            // pre-D3 we leave that as a TODO marker in `note` and
            // surface a coarse-grained verdict so the orchestration
            // works end-to-end. D3 wires a per-test parser when the
            // first repro lands.
            let note = if out.status.success() {
                Some("shared-crate repros all passed — per-finding parsing pending D3".to_string())
            } else {
                Some(format!(
                    "shared-crate repros had failures — per-finding parsing pending D3:\n{}\n{}",
                    tail(&stdout, 20),
                    tail(&stderr, 10)
                ))
            };
            ProbeReproReport {
                repros_dir: repros_dir.to_path_buf(),
                results: Vec::new(),
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
    // Heuristic: cargo prints `error[E0…]:` for compile errors and
    // `error: could not compile` for the final summary. Test failures
    // don't produce these markers — they manifest as a failing test
    // result line in stdout instead.
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
    fn empty_repros_dir_returns_d3_pending_note() {
        let tmp = tempdir().unwrap();
        let repros_dir = tmp.path().join("target").join("qedgen-repros");
        fs::create_dir_all(&repros_dir).unwrap();
        let report = run(tmp.path()).unwrap();
        assert!(report.results.is_empty());
        assert!(report.note.as_ref().unwrap().contains("D3 not yet wired"));
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
    fn all_fired_or_inconclusive_lets_build_errors_through() {
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
        assert!(report.all_fired_or_inconclusive());
    }

    #[test]
    fn all_fired_or_inconclusive_fails_on_silent() {
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
        assert!(!report.all_fired_or_inconclusive());
    }
}
