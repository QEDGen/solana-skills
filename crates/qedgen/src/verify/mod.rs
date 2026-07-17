// `verify` runs the generated harnesses against the generated implementation
// (check validates the spec; verify validates the code it produced).
// Backends: proptest (cargo test), kani (cargo kani), lean (lake build).
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
    /// Structured counterexamples from the per-backend parser. Empty for
    /// `Passed` / `Skipped`, and for `Failed` where parsing found nothing
    /// (`detail` still carries the human summary). `omitempty` for JSON
    /// back-compat.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub counterexamples: Vec<Counterexample>,
    /// `lean` backend only: unverified axioms each top-level theorem in
    /// Spec.lean / Proofs.lean depends on — the trust surface
    /// (`*.ensures_axiom_*` from bundled callees + `sorryAx`). Empty for
    /// other backends, failed builds, no theorems, or builtin-only closures.
    /// `omitempty` for JSON back-compat.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub axioms: Vec<AxiomDependency>,
}

impl BackendReport {
    /// Base constructor: `duration_ms` measured from `start`, no log path,
    /// empty counterexample/axiom lists. Status-specific wrappers below.
    pub fn new(
        name: &'static str,
        status: BackendStatus,
        start: Instant,
        detail: Option<String>,
    ) -> Self {
        BackendReport {
            name,
            status,
            duration_ms: start.elapsed().as_millis(),
            detail,
            log_path: None,
            counterexamples: Vec::new(),
            axioms: Vec::new(),
        }
    }

    pub fn passed(name: &'static str, start: Instant, detail: Option<String>) -> Self {
        Self::new(name, BackendStatus::Passed, start, detail)
    }

    pub fn failed(name: &'static str, start: Instant, detail: Option<String>) -> Self {
        Self::new(name, BackendStatus::Failed, start, detail)
    }

    pub fn skipped(name: &'static str, start: Instant, detail: Option<String>) -> Self {
        Self::new(name, BackendStatus::Skipped, start, detail)
    }

    /// Attach structured counterexamples (Failed reports).
    pub fn with_counterexamples(mut self, cxs: Vec<Counterexample>) -> Self {
        self.counterexamples = cxs;
        self
    }

    /// Attach the axiom trust-surface report (`lean` backend).
    pub fn with_axioms(mut self, axioms: Vec<AxiomDependency>) -> Self {
        self.axioms = axioms;
        self
    }
}

/// One theorem's dependence on unverified axioms. Lean built-ins
/// (`LEAN_BUILTIN_AXIOMS`) are filtered before construction — what remains is
/// the user-meaningful trust surface: bundled-callee `*.ensures_axiom_*`
/// axioms and `sorryAx` from incomplete proofs.
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
    /// Run Miri repros under `.qed/probes/pinocchio/*/repro_miri.rs`.
    pub miri: bool,
    /// Project root for Miri repro discovery (typically the spec's parent dir).
    pub project_root: PathBuf,
}

/// `qedgen verify --require-verified` pre-check: every imported interface
/// with `ensures` clauses must ship a Lake-buildable proof package —
/// results against a not-fully-proven dep graph still rest on Stance-1
/// axiom discharge. Prints the CRIT findings and exits(1) on failure
/// (fail fast, before any backend dispatches).
pub fn require_verified_gate(parsed: &crate::check::ParsedSpec) {
    let findings = crate::check::collect_require_verified_findings(parsed);
    if !findings.is_empty() {
        eprintln!(
            "--require-verified: {} unverified import(s) — every imported interface \
             with `ensures` clauses must ship a Lake-buildable proof package.",
            findings.len(),
        );
        for f in &findings {
            eprintln!("  [CRIT] {}: unverified callee", f.interface_name);
            eprintln!("         {}", f.fix_hint);
        }
        std::process::exit(1);
    }
}

/// `qedgen verify --recursive`: `lake build` every imported proof package
/// (resolver returns DFS-pre-order = bottom-up, cycle-detected). Keeps
/// walking on failure so every breakage shows; aggregate exit(1) at the
/// end. Empty list = no-op success.
pub fn recursive_lake_walk(parsed: &crate::check::ParsedSpec) {
    if parsed.verified_proof_pkgs.is_empty() {
        eprintln!(
            "--recursive: no imported proof packages in this spec's dep graph; \
             nothing to walk."
        );
    } else {
        eprintln!(
            "--recursive: walking {} verified provider proof package(s) bottom-up.",
            parsed.verified_proof_pkgs.len(),
        );
        let mut any_failed = false;
        for (idx, pkg_root) in parsed.verified_proof_pkgs.iter().enumerate() {
            eprintln!(
                "  [{}/{}] lake build — {}",
                idx + 1,
                parsed.verified_proof_pkgs.len(),
                pkg_root.display(),
            );
            match Command::new("lake")
                .arg("build")
                .current_dir(pkg_root)
                .output()
            {
                Ok(out) if out.status.success() => {
                    eprintln!("       PASS");
                }
                Ok(out) => {
                    any_failed = true;
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    eprintln!("       FAIL");
                    // First ~10 lines of each stream — the head
                    // usually identifies the failure.
                    for line in stderr.lines().take(10) {
                        eprintln!("         | {}", line);
                    }
                    for line in stdout.lines().take(10) {
                        eprintln!("         | {}", line);
                    }
                }
                Err(e) => {
                    any_failed = true;
                    eprintln!("       ERROR: failed to spawn `lake build`: {}", e);
                }
            }
        }
        if any_failed {
            eprintln!(
                "--recursive: at least one provider's Lake build failed; the dep \
                 graph is NOT fully proven. Fix the provider(s) above before \
                 trusting this consumer's Stance-2 axioms."
            );
            std::process::exit(1);
        }
        eprintln!("--recursive: every imported proof package built clean.");
    }
}

pub fn run(opts: &VerifyOpts) -> Result<VerifyReport> {
    let mut backends = Vec::new();
    let runners: [&dyn VerifyBackend; 4] =
        [&ProptestBackend, &KaniBackend, &LeanBackend, &MiriBackend];

    for runner in runners {
        if !runner.enabled(opts) {
            continue;
        }

        let report = runner.run(opts);
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

trait VerifyBackend {
    fn enabled(&self, opts: &VerifyOpts) -> bool;
    fn run(&self, opts: &VerifyOpts) -> BackendReport;
}

struct ProptestBackend;
struct KaniBackend;
struct LeanBackend;
struct MiriBackend;

impl VerifyBackend for ProptestBackend {
    fn enabled(&self, opts: &VerifyOpts) -> bool {
        opts.proptest
    }

    fn run(&self, opts: &VerifyOpts) -> BackendReport {
        run_proptest(&opts.proptest_path)
    }
}

impl VerifyBackend for KaniBackend {
    fn enabled(&self, opts: &VerifyOpts) -> bool {
        opts.kani
    }

    fn run(&self, opts: &VerifyOpts) -> BackendReport {
        run_kani(&opts.kani_path)
    }
}

impl VerifyBackend for LeanBackend {
    fn enabled(&self, opts: &VerifyOpts) -> bool {
        opts.lean
    }

    fn run(&self, opts: &VerifyOpts) -> BackendReport {
        run_lean(&opts.lean_dir)
    }
}

impl VerifyBackend for MiriBackend {
    fn enabled(&self, opts: &VerifyOpts) -> bool {
        opts.miri
    }

    fn run(&self, opts: &VerifyOpts) -> BackendReport {
        crate::miri_verify::run(&opts.project_root)
    }
}

fn run_proptest(harness: &Path) -> BackendReport {
    let start = Instant::now();

    if !harness.exists() {
        return BackendReport::skipped(
            "proptest",
            start,
            Some(format!(
                "harness not found at {} (run `qedgen codegen --proptest`)",
                harness.display()
            )),
        );
    }

    // Run from the harness's nearest Cargo.toml ancestor.
    let crate_dir = match nearest_cargo_dir(harness) {
        Some(dir) => dir,
        None => {
            return BackendReport::failed(
                "proptest",
                start,
                Some(format!("no Cargo.toml found above {}", harness.display())),
            );
        }
    };

    // Release because proptest cases can be slow under debug.
    let test_name = harness
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("proptest");

    let output = Command::new("cargo")
        .args(["test", "--release", "--test", test_name])
        .current_dir(&crate_dir)
        .output();

    match output {
        Ok(out) if out.status.success() => BackendReport::passed("proptest", start, None),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Parse libtest failures into structured tuples + attach the
            // persisted regression seed. If parsing yields nothing, `detail`
            // still carries the human summary.
            let mut cxs = crate::verify_proptest_parse::parse_failures(&stdout);
            for cx in cxs.iter_mut() {
                cx.seed =
                    crate::verify_proptest_parse::read_seed_for_harness(&crate_dir, test_name);
            }
            BackendReport::failed(
                "proptest",
                start,
                Some(summarize_cargo_failure(&stdout, &stderr)),
            )
            .with_counterexamples(cxs)
        }
        Err(e) => BackendReport::failed(
            "proptest",
            start,
            Some(format!("failed to spawn cargo: {}", e)),
        ),
    }
}

fn run_kani(harness: &Path) -> BackendReport {
    let start = Instant::now();

    if !harness.exists() {
        return BackendReport::skipped(
            "kani",
            start,
            Some(format!(
                "harness not found at {} (run `qedgen codegen --kani`)",
                harness.display()
            )),
        );
    }

    // Surface a missing cargo-kani as a Failed backend with the install hint,
    // not an opaque spawn error.
    if let Err(e) = crate::deps::require_kani() {
        return BackendReport::failed("kani", start, Some(format!("{}", e)));
    }

    // Harnesses routing effects to `bin = "z3"` (wide-type mul/div) need z3
    // preflighted — otherwise Kani dies with an opaque cbmc spawn error.
    if let Err(e) = crate::deps::require_z3_if_kani_harness_needs_it(harness) {
        return BackendReport::failed("kani", start, Some(format!("{}", e)));
    }

    let kani_crate = match prepare_standalone_kani_crate(harness) {
        Ok(dir) => dir,
        Err(e) => {
            return BackendReport::failed(
                "kani",
                start,
                Some(format!("failed to prepare standalone Kani crate: {}", e)),
            );
        }
    };

    // Run in an isolated crate: the harness is framework-neutral, and tying it
    // to the program package lets unrelated scaffold compile errors block Kani.
    let output = Command::new("cargo")
        .args(["kani", "--tests"])
        .current_dir(kani_crate.path())
        .output();

    match output {
        Ok(out) if out.status.success() => BackendReport::passed(
            "kani",
            start,
            Some(summarize_kani_pass(&String::from_utf8_lossy(&out.stdout))),
        ),
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            // Counterexamples come exclusively from stdout (cargo-kani routes
            // verdicts there); stderr is build noise folded into `detail` only.
            let cxs = crate::verify_kani_parse::parse_failures(&stdout);
            BackendReport::failed(
                "kani",
                start,
                Some(summarize_kani_failure(&stdout, &stderr)),
            )
            .with_counterexamples(cxs)
        }
        Err(e) => BackendReport::failed(
            "kani",
            start,
            Some(format!("failed to spawn cargo kani: {}", e)),
        ),
    }
}

fn run_lean(lean_dir: &Path) -> BackendReport {
    let start = Instant::now();

    if !lean_dir.join("lakefile.lean").exists() && !lean_dir.join("lakefile.toml").exists() {
        return BackendReport::skipped(
            "lean",
            start,
            Some(format!(
                "no lakefile in {} (run `qedgen codegen --lean`)",
                lean_dir.display()
            )),
        );
    }

    let output = Command::new("lake")
        .arg("build")
        .current_dir(lean_dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            // Soft-failing: if the axiom query can't run, report an empty
            // list rather than failing the verify.
            let axioms = collect_axiom_report(lean_dir).unwrap_or_default();
            BackendReport::passed("lean", start, None).with_axioms(axioms)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            BackendReport::failed(
                "lean",
                start,
                Some(summarize_lake_failure(&stdout, &stderr)),
            )
        }
        Err(e) => BackendReport::failed(
            "lean",
            start,
            Some(format!(
                "failed to spawn lake: {} (is lean/lake on PATH?)",
                e
            )),
        ),
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
    // Kani prints "VERIFICATION:- SUCCESSFUL" per harness plus a summary line.
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
        // Failure before any harness ran; cargo kani splits such errors across
        // stdout and stderr — return whichever has content.
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

// ---- `#print axioms` trust-surface report ---------------------

/// Lean built-ins filtered out of the report: the classical-logic trio every
/// Mathlib proof pulls in, plus the `native_decide` compiler-trust pair —
/// Lean's trust base, not the user's.
const LEAN_BUILTIN_AXIOMS: &[&str] = &[
    "propext",
    "Classical.choice",
    "Quot.sound",
    "Lean.ofReduceBool",
    "Lean.trustCompiler",
];

/// Query the axiom closure of Spec.lean / Proofs.lean theorems via
/// `lake env lean`. `None` = query couldn't run (soft-fail);
/// `Some(vec![])` = queried fine, nothing non-builtin (all-proven).
fn collect_axiom_report(lean_dir: &Path) -> Option<Vec<AxiomDependency>> {
    let theorems = collect_theorem_names(lean_dir);
    if theorems.is_empty() {
        return Some(Vec::new());
    }
    run_axiom_query(lean_dir, &theorems)
}

/// Top-level `theorem` declarations in Spec.lean + Proofs.lean,
/// fully qualified via namespace tracking.
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

/// Parse Lean's `#print axioms` output. Two shapes per theorem:
///   `'<name>' depends on axioms: [a, b, c]`   → captured
///   `'<name>' does not depend on any axioms`  → skipped (nothing to surface)
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

/// Full human-readable verify report (newline-terminated). Separate from
/// `print_human` so tests can pin the rendering without stderr capture.
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

/// Render structured counterexamples below the backend's status line —
/// one block per failing harness with spec-named `var = value` pairs.
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

/// Render the unverified trust surface (grouped by theorem). Built-ins are
/// filtered upstream; empty `axioms` emits no section (silent pass).
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
        // Named-value assignments must render in human output, not just JSON.
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

pub(crate) mod drift;
pub(crate) mod evidence;
pub(crate) mod miri_verify;
pub(crate) mod ratchet;
pub(crate) mod regen_drift;
pub(crate) mod sbpf_verify;
pub(crate) mod upstream_check;
pub(crate) mod verify_counterexample;
pub(crate) mod verify_kani_parse;
pub(crate) mod verify_probe_repros;
pub(crate) mod verify_proptest_parse;
