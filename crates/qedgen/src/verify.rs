// The `verify` subcommand runs the generated harnesses against the generated
// implementation. It closes the loop that `check` opens: check validates the
// spec; verify validates the code the spec produced.
//
// Backends: proptest (cargo test), kani (cargo kani — M2), lean (lake build).
// Each runner returns a BackendReport; they roll up into a VerifyReport.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStatus {
    Passed,
    Failed,
    Skipped,
    NotImplemented,
}

#[derive(Debug, Serialize)]
pub struct BackendReport {
    pub name: &'static str,
    pub status: BackendStatus,
    pub duration_ms: u128,
    pub detail: Option<String>,
    pub log_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct VerifyReport {
    pub spec: PathBuf,
    pub backends: Vec<BackendReport>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.backends
            .iter()
            .all(|b| !matches!(b.status, BackendStatus::Failed))
    }
}

pub struct VerifyOpts {
    pub spec: PathBuf,
    pub proptest: bool,
    pub proptest_path: PathBuf,
    pub kani: bool,
    // Reserved for the M2 kani runner; currently only used by the CLI default
    // to decide whether to auto-enable the backend.
    #[allow(dead_code)]
    pub kani_path: PathBuf,
    pub lean: bool,
    pub lean_dir: PathBuf,
    pub fail_fast: bool,
}

pub fn run(opts: &VerifyOpts) -> Result<VerifyReport> {
    let mut backends = Vec::new();

    if opts.proptest {
        let report = run_proptest(&opts.proptest_path);
        let failed = matches!(report.status, BackendStatus::Failed);
        backends.push(report);
        if failed && opts.fail_fast {
            return Ok(VerifyReport {
                spec: opts.spec.clone(),
                backends,
            });
        }
    }

    if opts.kani {
        backends.push(BackendReport {
            name: "kani",
            status: BackendStatus::NotImplemented,
            duration_ms: 0,
            detail: Some("Kani runner lands in v2.4-M2".into()),
            log_path: None,
        });
    }

    if opts.lean {
        let report = run_lean(&opts.lean_dir);
        let failed = matches!(report.status, BackendStatus::Failed);
        backends.push(report);
        if failed && opts.fail_fast {
            return Ok(VerifyReport {
                spec: opts.spec.clone(),
                backends,
            });
        }
    }

    Ok(VerifyReport {
        spec: opts.spec.clone(),
        backends,
    })
}

fn run_proptest(harness: &Path) -> BackendReport {
    let start = Instant::now();

    if !harness.exists() {
        return BackendReport {
            name: "proptest",
            status: BackendStatus::Skipped,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(format!(
                "harness not found at {} (run `qedgen codegen --proptest`)",
                harness.display()
            )),
            log_path: None,
        };
    }

    // The harness is generated into `tests/proptest.rs` at the program root;
    // its containing crate is whatever cargo finds walking up. Run from the
    // harness's nearest Cargo.toml ancestor.
    let crate_dir = match nearest_cargo_dir(harness) {
        Some(dir) => dir,
        None => {
            return BackendReport {
                name: "proptest",
                status: BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!(
                    "no Cargo.toml found above {}",
                    harness.display()
                )),
                log_path: None,
            };
        }
    };

    // `cargo test --release --test proptest` runs just the generated harness.
    // Release because proptest cases can be slow under debug.
    let test_name = harness
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("proptest");

    let output = Command::new("cargo")
        .args(["test", "--release", "--test", test_name])
        .current_dir(&crate_dir)
        .output();

    let duration_ms = start.elapsed().as_millis();

    match output {
        Ok(out) if out.status.success() => BackendReport {
            name: "proptest",
            status: BackendStatus::Passed,
            duration_ms,
            detail: None,
            log_path: None,
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            BackendReport {
                name: "proptest",
                status: BackendStatus::Failed,
                duration_ms,
                detail: Some(summarize_cargo_failure(&stdout, &stderr)),
                log_path: None,
            }
        }
        Err(e) => BackendReport {
            name: "proptest",
            status: BackendStatus::Failed,
            duration_ms,
            detail: Some(format!("failed to spawn cargo: {}", e)),
            log_path: None,
        },
    }
}

fn run_lean(lean_dir: &Path) -> BackendReport {
    let start = Instant::now();

    if !lean_dir.join("lakefile.lean").exists() && !lean_dir.join("lakefile.toml").exists() {
        return BackendReport {
            name: "lean",
            status: BackendStatus::Skipped,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(format!(
                "no lakefile in {} (run `qedgen codegen --lean`)",
                lean_dir.display()
            )),
            log_path: None,
        };
    }

    let output = Command::new("lake")
        .arg("build")
        .current_dir(lean_dir)
        .output();

    let duration_ms = start.elapsed().as_millis();

    match output {
        Ok(out) if out.status.success() => BackendReport {
            name: "lean",
            status: BackendStatus::Passed,
            duration_ms,
            detail: None,
            log_path: None,
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            BackendReport {
                name: "lean",
                status: BackendStatus::Failed,
                duration_ms,
                detail: Some(summarize_lake_failure(&stdout, &stderr)),
                log_path: None,
            }
        }
        Err(e) => BackendReport {
            name: "lean",
            status: BackendStatus::Failed,
            duration_ms,
            detail: Some(format!("failed to spawn lake: {} (is lean/lake on PATH?)", e)),
            log_path: None,
        },
    }
}

fn nearest_cargo_dir(start: &Path) -> Option<PathBuf> {
    let mut cur = if start.is_dir() {
        Some(start.to_path_buf())
    } else {
        start.parent().map(|p| p.to_path_buf())
    };
    while let Some(dir) = cur {
        if dir.join("Cargo.toml").exists() {
            return Some(dir);
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

fn summarize_cargo_failure(stdout: &str, stderr: &str) -> String {
    // Prefer the test-failure lines if present; fall back to the tail of stderr.
    let failures: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("FAILED") || l.contains("test result: FAILED"))
        .take(10)
        .collect();
    if !failures.is_empty() {
        return failures.join("\n");
    }
    tail_lines(stderr, 20)
}

fn summarize_lake_failure(stdout: &str, stderr: &str) -> String {
    let errors: Vec<&str> = stderr
        .lines()
        .chain(stdout.lines())
        .filter(|l| l.contains("error:") || l.contains("sorry"))
        .take(10)
        .collect();
    if !errors.is_empty() {
        return errors.join("\n");
    }
    tail_lines(stderr, 20)
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub fn print_human(report: &VerifyReport) {
    eprintln!("qedgen verify — {}", report.spec.display());
    for b in &report.backends {
        let marker = match b.status {
            BackendStatus::Passed => "PASS",
            BackendStatus::Failed => "FAIL",
            BackendStatus::Skipped => "SKIP",
            BackendStatus::NotImplemented => "TODO",
        };
        eprintln!("  [{}] {:<10} ({} ms)", marker, b.name, b.duration_ms);
        if let Some(d) = &b.detail {
            for line in d.lines() {
                eprintln!("         {}", line);
            }
        }
    }
    if report.ok() {
        eprintln!("OK");
    } else {
        eprintln!("FAILED");
    }
}

pub fn print_json(report: &VerifyReport) -> Result<()> {
    let s = serde_json::to_string_pretty(report).context("serializing verify report")?;
    println!("{}", s);
    Ok(())
}
