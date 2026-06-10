// The `verify` subcommand runs the generated harnesses against the generated
// implementation. It closes the loop that `check` opens: check validates the
// spec; verify validates the code the spec produced.
//
// Backends: proptest (cargo test), kani (cargo kani — M2), lean (lake build).
// Each runner returns a BackendReport; they roll up into a VerifyReport.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::verify_counterexample::Counterexample;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct BackendReport {
    pub name: &'static str,
    pub status: BackendStatus,
    pub duration_ms: u128,
    pub detail: Option<String>,
    pub log_path: Option<PathBuf>,
    /// Structured counterexamples extracted by the per-backend parser
    /// (PLAN-v2.16 D1/D2). Empty for `Passed` / `Skipped` backends, and
    /// for `Failed` backends whose parser couldn't extract structured
    /// data (in which case `detail` still carries the human summary).
    /// Serialized `omitempty` so consumers pinning the v2.15 shape
    /// continue to work.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub counterexamples: Vec<Counterexample>,
    /// v2.28 — for the `lean` backend, the unverified axioms each
    /// top-level theorem in Spec.lean / Proofs.lean depends on. Surfaces
    /// the trust surface (`*.ensures_axiom_*` from bundled callees +
    /// any `sorryAx` from incomplete proofs) as a first-class artifact.
    /// Empty when the backend isn't `lean`, when the build failed,
    /// when no top-level theorems were found, or when every theorem
    /// only depends on Lean built-ins. `omitempty` so v2.27 JSON
    /// consumers continue to work.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub axioms: Vec<AxiomDependency>,
}

/// One theorem's dependence on unverified axioms (v2.28).
///
/// Lean built-ins (`propext`, `Classical.choice`, `Quot.sound`, the
/// `Lean.ofReduceBool` / `Lean.trustCompiler` pair used by
/// `native_decide`) are filtered out before this is constructed —
/// they're part of every Lean program's trust base and not actionable.
/// What remains is the user-meaningful trust surface: bundled-callee
/// `*.ensures_axiom_*` axioms (Stance-1 codegen module or Stance-2
/// bundled package) and `sorryAx` from incomplete proofs.
#[derive(Debug, Clone, Serialize)]
pub struct AxiomDependency {
    /// Fully-qualified theorem name (`Namespace.theoremName`).
    pub theorem: String,
    /// Axioms the theorem depends on, with Lean built-ins filtered.
    pub axioms: Vec<String>,
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
    pub kani_path: PathBuf,
    pub lean: bool,
    pub lean_dir: PathBuf,
    pub fail_fast: bool,
    /// v2.19: run Miri repros under `.qed/probes/pinocchio/*/repro_miri.rs`.
    pub miri: bool,
    /// Project root for Miri repro discovery (typically the spec's
    /// parent dir).
    pub project_root: PathBuf,
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
        let report = run_kani(&opts.kani_path);
        let failed = matches!(report.status, BackendStatus::Failed);
        backends.push(report);
        if failed && opts.fail_fast {
            return Ok(VerifyReport {
                spec: opts.spec.clone(),
                backends,
            });
        }
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

    if opts.miri {
        let report = crate::miri_verify::run(&opts.project_root);
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
            counterexamples: Vec::new(),
            axioms: Vec::new(),
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
                detail: Some(format!("no Cargo.toml found above {}", harness.display())),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
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
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            // PLAN-v2.16 D2: parse libtest's failure block into structured
            // (harness, var, value) tuples. Then attach the persisted
            // proptest-regressions seed for deterministic re-run. If
            // parsing yields nothing (output shape changed, or failure
            // happened before any property fired), `detail` still carries
            // the existing human summary so nothing regresses.
            let mut cxs = crate::verify_proptest_parse::parse_failures(&stdout);
            for cx in cxs.iter_mut() {
                cx.seed =
                    crate::verify_proptest_parse::read_seed_for_harness(&crate_dir, test_name);
            }
            BackendReport {
                name: "proptest",
                status: BackendStatus::Failed,
                duration_ms,
                detail: Some(summarize_cargo_failure(&stdout, &stderr)),
                log_path: None,
                counterexamples: cxs,
                axioms: Vec::new(),
            }
        }
        Err(e) => BackendReport {
            name: "proptest",
            status: BackendStatus::Failed,
            duration_ms,
            detail: Some(format!("failed to spawn cargo: {}", e)),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        },
    }
}

fn run_kani(harness: &Path) -> BackendReport {
    let start = Instant::now();

    if !harness.exists() {
        return BackendReport {
            name: "kani",
            status: BackendStatus::Skipped,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(format!(
                "harness not found at {} (run `qedgen codegen --kani`)",
                harness.display()
            )),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        };
    }

    // Point-of-use dep check. `require_kani` returns Err with install text
    // when cargo-kani is missing; surface that as a Failed backend so the
    // user sees the install hint instead of a spawn error.
    if let Err(e) = crate::deps::require_kani() {
        return BackendReport {
            name: "kani",
            status: BackendStatus::Failed,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(format!("{}", e)),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        };
    }

    // If the harness routes any effect to `bin = "z3"` (wide-type mul/div),
    // preflight that z3 is installed. Without this the Kani run fails with
    // an opaque cbmc spawn error; surface the install hint up front.
    if let Err(e) = crate::deps::require_z3_if_kani_harness_needs_it(harness) {
        return BackendReport {
            name: "kani",
            status: BackendStatus::Failed,
            duration_ms: start.elapsed().as_millis(),
            detail: Some(format!("{}", e)),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        };
    }

    let kani_crate = match prepare_standalone_kani_crate(harness) {
        Ok(dir) => dir,
        Err(e) => {
            return BackendReport {
                name: "kani",
                status: BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!("failed to prepare standalone Kani crate: {}", e)),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            };
        }
    };

    // Run the spec-model Kani harness in an isolated crate. The harness is
    // framework-neutral; tying it to the generated program package means
    // unrelated Anchor/Pinocchio scaffold compile errors can prevent Kani
    // from checking the model at all.
    let output = Command::new("cargo")
        .args(["kani", "--tests"])
        .current_dir(kani_crate.path())
        .output();

    let duration_ms = start.elapsed().as_millis();

    match output {
        Ok(out) if out.status.success() => BackendReport {
            name: "kani",
            status: BackendStatus::Passed,
            duration_ms,
            detail: Some(summarize_kani_pass(&String::from_utf8_lossy(&out.stdout))),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            // PLAN-v2.16 D1: parse CBMC counterexample output into
            // structured (harness, var, value, line) tuples. The human
            // `detail` summary stays for backward compat / pretty-print.
            // Counterexamples come exclusively from stdout (cargo-kani
            // routes verdicts there); stderr carries build noise we
            // fold into `detail` only.
            let cxs = crate::verify_kani_parse::parse_failures(&stdout);
            BackendReport {
                name: "kani",
                status: BackendStatus::Failed,
                duration_ms,
                detail: Some(summarize_kani_failure(&stdout, &stderr)),
                log_path: None,
                counterexamples: cxs,
                axioms: Vec::new(),
            }
        }
        Err(e) => BackendReport {
            name: "kani",
            status: BackendStatus::Failed,
            duration_ms,
            detail: Some(format!("failed to spawn cargo kani: {}", e)),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
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
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        };
    }

    let output = Command::new("lake")
        .arg("build")
        .current_dir(lean_dir)
        .output();

    let duration_ms = start.elapsed().as_millis();

    match output {
        Ok(out) if out.status.success() => {
            // v2.28 — surface the unverified trust surface alongside the
            // pass. Soft-failing: if the axiom query can't run (file IO,
            // `lake env lean` not on PATH, regex misses), we silently
            // return an empty list rather than failing the verify.
            let axioms = collect_axiom_report(lean_dir).unwrap_or_default();
            BackendReport {
                name: "lean",
                status: BackendStatus::Passed,
                duration_ms,
                detail: None,
                log_path: None,
                counterexamples: Vec::new(),
                axioms,
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            BackendReport {
                name: "lean",
                status: BackendStatus::Failed,
                duration_ms,
                detail: Some(summarize_lake_failure(&stdout, &stderr)),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            }
        }
        Err(e) => BackendReport {
            name: "lean",
            status: BackendStatus::Failed,
            duration_ms,
            detail: Some(format!(
                "failed to spawn lake: {} (is lean/lake on PATH?)",
                e
            )),
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        },
    }
}

fn prepare_standalone_kani_crate(harness: &Path) -> Result<tempfile::TempDir> {
    let tmp = tempfile::tempdir().context("create temporary Kani crate")?;
    let src_dir = tmp.path().join("src");
    fs::create_dir_all(&src_dir).context("create temporary Kani src dir")?;
    fs::copy(harness, src_dir.join("lib.rs"))
        .with_context(|| format!("copy Kani harness from {}", harness.display()))?;
    fs::write(
        tmp.path().join("Cargo.toml"),
        r#"[package]
name = "qedgen-kani-harness"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .context("write temporary Kani Cargo.toml")?;
    Ok(tmp)
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

fn summarize_kani_pass(stdout: &str) -> String {
    // On success, Kani prints "VERIFICATION:- SUCCESSFUL" per harness and a
    // summary line. Count them for a tight report.
    let successful = stdout.matches("VERIFICATION:- SUCCESSFUL").count();
    let summary_line = stdout
        .lines()
        .find(|l| l.contains("Complete - ") || l.contains("harnesses"))
        .unwrap_or("");
    if summary_line.is_empty() {
        format!("{} harness(es) verified", successful)
    } else {
        format!("{} verified — {}", successful, summary_line.trim())
    }
}

fn summarize_kani_failure(stdout: &str, stderr: &str) -> String {
    // Pull failed verifications and their counterexample preamble.
    let mut lines: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.contains("VERIFICATION:- FAILED")
                || l.contains("Failed Checks:")
                || l.contains("Failed properties:")
                || l.contains("Check ")
        })
        .take(20)
        .collect();
    if lines.is_empty() {
        // Failure before any harness ran (toolchain missing, cargo metadata
        // refused, etc). `cargo kani` writes some of these to stdout and some
        // to stderr; return whichever has content.
        let tail_err = tail_lines(stderr, 20);
        if !tail_err.trim().is_empty() {
            return tail_err;
        }
        let tail_out = tail_lines(stdout, 20);
        if !tail_out.trim().is_empty() {
            return tail_out;
        }
        return "cargo kani failed with no diagnostic output".into();
    }
    if let Some(summary) = stdout
        .lines()
        .find(|l| l.contains("Complete - ") || l.contains("Summary:"))
    {
        lines.push(summary);
    }
    lines.join("\n")
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

// ---- v2.28 — `#print axioms` trust-surface report ---------------------

/// Lean built-ins that appear in every program's axiom closure. We
/// filter these out before reporting so the user-actionable list isn't
/// drowned in noise. `propext`, `Classical.choice`, `Quot.sound` are
/// the classical-logic trio every Mathlib-using proof transitively
/// pulls in; `Lean.ofReduceBool` + `Lean.trustCompiler` are the pair
/// behind `native_decide` and `decide`-on-reducible-Props, both of
/// which are part of Lean's compiler-trust base, not the user's.
const LEAN_BUILTIN_AXIOMS: &[&str] = &[
    "propext",
    "Classical.choice",
    "Quot.sound",
    "Lean.ofReduceBool",
    "Lean.trustCompiler",
];

/// Discover top-level theorems in Spec.lean / Proofs.lean and query
/// their axiom closure via `lake env lean`. Soft-fails: returns None
/// when file IO breaks, the lake invocation can't spawn, or no
/// theorems are found. `Some(vec![])` means "queried successfully, no
/// theorem depends on a non-builtin axiom" — the all-proven case.
fn collect_axiom_report(lean_dir: &Path) -> Option<Vec<AxiomDependency>> {
    let theorems = collect_theorem_names(lean_dir);
    if theorems.is_empty() {
        return Some(Vec::new());
    }
    run_axiom_query(lean_dir, &theorems)
}

/// Parse Spec.lean + Proofs.lean for top-level `theorem` declarations.
/// Tracks namespace nesting to produce fully-qualified names.
fn collect_theorem_names(lean_dir: &Path) -> Vec<String> {
    let mut result = Vec::new();
    for fname in ["Spec.lean", "Proofs.lean"] {
        let path = lean_dir.join(fname);
        if let Ok(text) = std::fs::read_to_string(&path) {
            collect_theorems_from_text(&text, &mut result);
        }
    }
    result
}

fn collect_theorems_from_text(text: &str, out: &mut Vec<String>) {
    let theorem_re = regex::Regex::new(
        r"^(?:(?:private|protected|noncomputable)\s+)*theorem\s+([A-Za-z_][A-Za-z0-9_']*)",
    )
    .expect("static regex");
    let mut ns: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start();
        if line.starts_with("--") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("namespace ") {
            if let Some(name) = first_ident(rest) {
                ns.push(name);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("end ") {
            if let Some(name) = first_ident(rest) {
                if ns.last().map(|s| s.as_str()) == Some(name.as_str()) {
                    ns.pop();
                }
            }
            continue;
        }
        if let Some(caps) = theorem_re.captures(line) {
            let name = caps[1].to_string();
            let full = if ns.is_empty() {
                name
            } else {
                format!("{}.{}", ns.join("."), name)
            };
            out.push(full);
        }
    }
}

fn first_ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '\'')
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(s[..end].to_string())
    }
}

fn run_axiom_query(lean_dir: &Path, theorems: &[String]) -> Option<Vec<AxiomDependency>> {
    let report_path = lean_dir.join("_QedgenAxiomReport.lean");
    let mut content = String::new();
    if lean_dir.join("Spec.lean").exists() {
        content.push_str("import Spec\n");
    }
    if lean_dir.join("Proofs.lean").exists() {
        content.push_str("import Proofs\n");
    }
    content.push('\n');
    for thm in theorems {
        content.push_str(&format!("#print axioms {}\n", thm));
    }
    if std::fs::write(&report_path, &content).is_err() {
        return None;
    }
    let output = Command::new("lake")
        .args(["env", "lean", "_QedgenAxiomReport.lean"])
        .current_dir(lean_dir)
        .output();
    let _ = std::fs::remove_file(&report_path);
    let out = output.ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    Some(parse_axiom_output(&stdout))
}

/// Parse Lean's `#print axioms` output. Two output shapes per theorem:
///   `'<name>' depends on axioms: [a, b, c]`   → captured here
///   `'<name>' does not depend on any axioms`  → silently skipped
/// (the second shape means the theorem's trust closure is empty after
/// our built-in filter; nothing to surface).
fn parse_axiom_output(stdout: &str) -> Vec<AxiomDependency> {
    let re =
        regex::Regex::new(r"'([^']+)' depends on axioms:\s*\[([^\]]*)\]").expect("static regex");
    let mut result = Vec::new();
    for cap in re.captures_iter(stdout) {
        let theorem = cap[1].to_string();
        let axioms: Vec<String> = cap[2]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|a| !a.is_empty() && !LEAN_BUILTIN_AXIOMS.contains(&a.as_str()))
            .collect();
        if !axioms.is_empty() {
            result.push(AxiomDependency { theorem, axioms });
        }
    }
    result
}

pub fn print_human(report: &VerifyReport) {
    eprint!("{}", format_human(report));
}

/// Format the full human-readable verify report. Separated from `print_human`
/// so tests can pin the exact rendering without stderr capture; `print_human`
/// is the side-effecting thin wrapper. Returns a string ending in a newline.
pub fn format_human(report: &VerifyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("qedgen verify — {}\n", report.spec.display()));
    for b in &report.backends {
        let marker = match b.status {
            BackendStatus::Passed => "PASS",
            BackendStatus::Failed => "FAIL",
            BackendStatus::Skipped => "SKIP",
        };
        out.push_str(&format!(
            "  [{}] {:<10} ({} ms)\n",
            marker, b.name, b.duration_ms
        ));
        if let Some(d) = &b.detail {
            for line in d.lines() {
                out.push_str(&format!("         {}\n", line));
            }
        }
        format_counterexamples(&mut out, &b.counterexamples);
        format_axioms(&mut out, &b.axioms);
    }
    if report.ok() {
        out.push_str("OK\n");
    } else {
        out.push_str("FAILED\n");
    }
    out
}

/// Render each backend's structured counterexamples below its status line,
/// one block per failing harness with the spec-named `var = value` pairs the
/// per-backend parser extracted. Both the kani parser (CBMC state blocks)
/// and the proptest parser already preserve spec binder names from the
/// generated harness — this fn is just the human surface for that data.
/// JSON consumers see the same data via `BackendReport.counterexamples`.
fn format_counterexamples(out: &mut String, cxs: &[Counterexample]) {
    for cx in cxs {
        out.push_str(&format!("         counterexample: {}\n", cx.harness));
        if let Some(msg) = &cx.failure_message {
            out.push_str(&format!("           {}\n", msg));
        }
        if let Some(loc) = &cx.source_location {
            out.push_str(&format!("           at {}\n", loc));
        }
        if !cx.assignments.is_empty() {
            let name_width = cx
                .assignments
                .iter()
                .map(|a| a.name.len())
                .max()
                .unwrap_or(0);
            for a in &cx.assignments {
                out.push_str(&format!(
                    "             {:<width$} = {}\n",
                    a.name,
                    a.value,
                    width = name_width
                ));
            }
        }
        if let Some(seed) = &cx.seed {
            out.push_str(&format!("           seed: {}\n", seed));
        }
    }
}

/// v2.28 — render the unverified trust surface below the backend's
/// status block. Groups by theorem; one axiom per indented line. Lean
/// built-ins (propext / Classical.choice / Quot.sound / native_decide
/// kernel pair) are filtered upstream so they don't appear here.
/// Empty `axioms` → no section emitted; "all proven" surfaces as
/// silent pass, same as today.
fn format_axioms(out: &mut String, axioms: &[AxiomDependency]) {
    if axioms.is_empty() {
        return;
    }
    out.push_str("         trust surface (unverified axioms each theorem depends on):\n");
    for dep in axioms {
        out.push_str(&format!("           {}\n", dep.theorem));
        for ax in &dep.axioms {
            out.push_str(&format!("             - {}\n", ax));
        }
    }
}

pub fn print_json(report: &VerifyReport) -> Result<()> {
    let s = serde_json::to_string_pretty(report).context("serializing verify report")?;
    println!("{}", s);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_counterexample::{Counterexample, CounterexampleVar};

    fn cx_kani_overflow() -> Counterexample {
        Counterexample {
            harness: "probe_overflow_transfer".into(),
            status: "failed".into(),
            assignments: vec![
                CounterexampleVar {
                    name: "pre".into(),
                    value: "18446744073709551615ul".into(),
                    line: Some(38),
                },
                CounterexampleVar {
                    name: "amount".into(),
                    value: "1ul".into(),
                    line: Some(39),
                },
                CounterexampleVar {
                    name: "post".into(),
                    value: "0ul".into(),
                    line: Some(40),
                },
            ],
            seed: None,
            failure_message: Some(
                "assertion failed: post == pre.checked_add(amount).unwrap_or(0)".into(),
            ),
            source_location: Some("tests/kani.rs:42:5".into()),
        }
    }

    fn cx_proptest_lifecycle() -> Counterexample {
        Counterexample {
            harness: "deposit_preserves_lifecycle".into(),
            status: "failed".into(),
            assignments: vec![
                CounterexampleVar {
                    name: "deposit_amount".into(),
                    value: "0".into(),
                    line: None,
                },
                CounterexampleVar {
                    name: "receive_amount".into(),
                    value: "42".into(),
                    line: None,
                },
            ],
            seed: Some("proptest-regressions/lib.txt::cc 0123".into()),
            failure_message: Some("deposit must reject zero amount".into()),
            source_location: None,
        }
    }

    #[test]
    fn prepares_standalone_kani_crate_from_harness() {
        let src = tempfile::tempdir().expect("tempdir");
        let harness = src.path().join("kani.rs");
        fs::write(&harness, "#[kani::proof]\nfn generated_harness() {}\n").expect("write harness");

        let kani_crate = prepare_standalone_kani_crate(&harness).expect("standalone crate");

        let cargo_toml =
            fs::read_to_string(kani_crate.path().join("Cargo.toml")).expect("Cargo.toml");
        assert!(cargo_toml.contains("name = \"qedgen-kani-harness\""));
        assert!(cargo_toml.contains("path = \"src/lib.rs\""));

        let copied = fs::read_to_string(kani_crate.path().join("src/lib.rs")).expect("lib.rs");
        assert!(copied.contains("generated_harness"));
    }

    #[test]
    fn renders_kani_counterexample_with_named_assignments() {
        let report = VerifyReport {
            spec: PathBuf::from("program.qedspec"),
            backends: vec![BackendReport {
                name: "kani",
                status: BackendStatus::Failed,
                duration_ms: 1234,
                detail: Some("1 of 2 failed".into()),
                log_path: None,
                counterexamples: vec![cx_kani_overflow()],
                axioms: Vec::new(),
            }],
        };
        let out = format_human(&report);
        // Named-value assignments render in human output, not just JSON
        // (this is the v2.17 fix — the kani parser already extracted them,
        // print_human just wasn't rendering them).
        assert!(out.contains("counterexample: probe_overflow_transfer"));
        assert!(out.contains("at tests/kani.rs:42:5"));
        assert!(out.contains("pre    = 18446744073709551615ul"));
        assert!(out.contains("amount = 1ul"));
        assert!(out.contains("post   = 0ul"));
        // Width-aligned columns: shorter name gets right-padding to longest.
        // Filter for assignment rows specifically (13-space indent + name) so
        // we don't match the failure-message line that contains `post == ...`.
        let pre_line = out
            .lines()
            .find(|l| l.starts_with("             pre"))
            .unwrap();
        let post_line = out
            .lines()
            .find(|l| l.starts_with("             post"))
            .unwrap();
        let pre_eq = pre_line.find('=').unwrap();
        let post_eq = post_line.find('=').unwrap();
        assert_eq!(pre_eq, post_eq, "name column should be width-aligned");
    }

    #[test]
    fn renders_proptest_counterexample_with_seed() {
        let report = VerifyReport {
            spec: PathBuf::from("program.qedspec"),
            backends: vec![BackendReport {
                name: "proptest",
                status: BackendStatus::Failed,
                duration_ms: 50,
                detail: None,
                log_path: None,
                counterexamples: vec![cx_proptest_lifecycle()],
                axioms: Vec::new(),
            }],
        };
        let out = format_human(&report);
        assert!(out.contains("counterexample: deposit_preserves_lifecycle"));
        assert!(out.contains("deposit must reject zero amount"));
        assert!(out.contains("deposit_amount = 0"));
        assert!(out.contains("receive_amount = 42"));
        assert!(out.contains("seed: proptest-regressions/lib.txt::cc 0123"));
    }

    #[test]
    fn renders_multiple_backends_with_mixed_status() {
        let report = VerifyReport {
            spec: PathBuf::from("program.qedspec"),
            backends: vec![
                BackendReport {
                    name: "proptest",
                    status: BackendStatus::Passed,
                    duration_ms: 12,
                    detail: None,
                    log_path: None,
                    counterexamples: vec![],
                    axioms: Vec::new(),
                },
                BackendReport {
                    name: "kani",
                    status: BackendStatus::Failed,
                    duration_ms: 4567,
                    detail: Some("1 of 1 failed".into()),
                    log_path: None,
                    counterexamples: vec![cx_kani_overflow()],
                    axioms: Vec::new(),
                },
                BackendReport {
                    name: "lean",
                    status: BackendStatus::Skipped,
                    duration_ms: 0,
                    detail: Some("no lakefile".into()),
                    log_path: None,
                    counterexamples: vec![],
                    axioms: Vec::new(),
                },
            ],
        };
        let out = format_human(&report);
        assert!(out.contains("[PASS] proptest"));
        assert!(out.contains("[FAIL] kani"));
        assert!(out.contains("[SKIP] lean"));
        // Only the failed backend renders its counterexample block.
        assert!(out.contains("counterexample: probe_overflow_transfer"));
        // Counterexamples are nested under their backend's block, not at
        // top level — verify the order: kani line, then its counterexample.
        let kani_idx = out.find("[FAIL] kani").unwrap();
        let cx_idx = out.find("counterexample:").unwrap();
        assert!(cx_idx > kani_idx);
        assert!(out.ends_with("FAILED\n"));
    }

    #[test]
    fn passing_report_omits_counterexamples_and_ends_ok() {
        let report = VerifyReport {
            spec: PathBuf::from("program.qedspec"),
            backends: vec![BackendReport {
                name: "kani",
                status: BackendStatus::Passed,
                duration_ms: 100,
                detail: None,
                log_path: None,
                counterexamples: vec![],
                axioms: Vec::new(),
            }],
        };
        let out = format_human(&report);
        assert!(!out.contains("counterexample"));
        assert!(out.ends_with("OK\n"));
    }

    // ---- v2.28 axiom-report tests --------------------------------------

    #[test]
    fn collects_top_level_theorems_with_namespace_prefix() {
        let src = r#"
namespace PoolDemo

def foo (x : Nat) : Nat := x + 1

theorem deposit_Token_transfer_call_0_post_1 (s : State) : True := trivial
theorem deposit_aborts_if_InvalidAmount : 1 = 1 := rfl

end PoolDemo

theorem at_root : True := trivial
"#;
        let mut out = Vec::new();
        collect_theorems_from_text(src, &mut out);
        assert_eq!(
            out,
            vec![
                "PoolDemo.deposit_Token_transfer_call_0_post_1".to_string(),
                "PoolDemo.deposit_aborts_if_InvalidAmount".to_string(),
                "at_root".to_string(),
            ]
        );
    }

    #[test]
    fn collects_theorems_with_modifiers_and_nested_namespaces() {
        let src = r#"
namespace Outer
namespace Inner

private theorem hidden : True := trivial
protected theorem visible (n : Nat) : n = n := rfl
noncomputable theorem chosen : True := trivial

end Inner
end Outer
"#;
        let mut out = Vec::new();
        collect_theorems_from_text(src, &mut out);
        assert_eq!(
            out,
            vec![
                "Outer.Inner.hidden".to_string(),
                "Outer.Inner.visible".to_string(),
                "Outer.Inner.chosen".to_string(),
            ]
        );
    }

    #[test]
    fn parses_lean_print_axioms_output_and_filters_builtins() {
        // Verbatim shape Lean emits for `#print axioms <thm>`. Built-ins
        // (propext, Classical.choice, Quot.sound) must NOT appear in the
        // structured output; user-meaningful axioms (sorryAx, bundled-
        // callee ensures_axiom_*) must.
        let stdout = "\
'PoolDemo.deposit_aborts_if_InvalidAmount' depends on axioms: [propext, Token.transfer.ensures_axiom_1]
'PoolDemo.deposit_frame' depends on axioms: [Classical.choice, Quot.sound, sorryAx]
'PoolDemo.all_proven' does not depend on any axioms
'PoolDemo.only_builtins' depends on axioms: [propext, Classical.choice]
";
        let report = parse_axiom_output(stdout);
        assert_eq!(
            report.len(),
            2,
            "only theorems with non-builtin axioms surface"
        );
        assert_eq!(
            report[0].theorem,
            "PoolDemo.deposit_aborts_if_InvalidAmount"
        );
        assert_eq!(report[0].axioms, vec!["Token.transfer.ensures_axiom_1"]);
        assert_eq!(report[1].theorem, "PoolDemo.deposit_frame");
        assert_eq!(report[1].axioms, vec!["sorryAx"]);
    }

    #[test]
    fn renders_axiom_report_below_lean_backend() {
        let report = VerifyReport {
            spec: PathBuf::from("program.qedspec"),
            backends: vec![BackendReport {
                name: "lean",
                status: BackendStatus::Passed,
                duration_ms: 850,
                detail: None,
                log_path: None,
                counterexamples: vec![],
                axioms: vec![
                    AxiomDependency {
                        theorem: "PoolDemo.deposit_Token_transfer_call_0_post_1".into(),
                        axioms: vec!["Token.transfer.ensures_axiom_1".into()],
                    },
                    AxiomDependency {
                        theorem: "PoolDemo.deposit_frame".into(),
                        axioms: vec!["sorryAx".into()],
                    },
                ],
            }],
        };
        let out = format_human(&report);
        assert!(out.contains("trust surface"));
        assert!(out.contains("PoolDemo.deposit_Token_transfer_call_0_post_1"));
        assert!(out.contains("- Token.transfer.ensures_axiom_1"));
        assert!(out.contains("- sorryAx"));
        assert!(out.ends_with("OK\n"));
    }

    #[test]
    fn empty_axiom_report_emits_no_trust_surface_section() {
        let report = VerifyReport {
            spec: PathBuf::from("program.qedspec"),
            backends: vec![BackendReport {
                name: "lean",
                status: BackendStatus::Passed,
                duration_ms: 850,
                detail: None,
                log_path: None,
                counterexamples: vec![],
                axioms: Vec::new(),
            }],
        };
        let out = format_human(&report);
        assert!(!out.contains("trust surface"));
    }
}
