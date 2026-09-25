//! `qedgen discharge --transition` verdicts (#405).
//!
//! qedlift lifts every `<stem>_<label>.pcs` trace beside the `.so` and emits one module per
//! path plus a bundle theorem. This module turns that run into one report:
//!
//! - qedlift writes into a fresh temp dir, so modules left in `--out-dir` by an earlier run
//!   never count. They are copied into `<out-dir>/Generated/` only when the verdict passes.
//! - Each path gets a row: its kind (a return with an exit code, or a VM fault), whether the
//!   spec expected it, and its status.
//! - The expected paths come from the spec: `success`, plus one path per `requires ... else E`
//!   in the handler, labeled with the snake_case of `E`. An expected path with no trace is
//!   `incomplete`, never a success.
//! - Lean checks every emitted module before anything is `verified` (#406).
//!
//! qedlift reports path kinds in a `transition outcome: {json}` stderr line (requested in
//! QEDGen/qedsvm#70). A qedlift without that line still lifts, but qedgen cannot confirm the
//! kinds, so the verdict is at most `incomplete`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::lean_check::{check_modules, LeanResult};
use super::outcome::{LeanCheck, Verdict};

const OUTCOME_PREFIX: &str = "transition outcome: ";
const REPORT_SCHEMA_VERSION: u32 = 1;

/// Trace coverage is not control-flow coverage. Printed in every report.
pub(crate) const COVERAGE_NOTE: &str = "Trace coverage is not whole-CFG coverage: only the \
     paths with a trace were lifted. `all_expected_paths_verified` covers the paths the spec \
     names, not every path through the bytecode.";

/// qedlift's `transition outcome` line (QEDGen/qedsvm#70).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct QedliftTransition {
    pub status: String,
    #[serde(default)]
    pub bundle: Option<String>,
    #[serde(default)]
    pub paths: Vec<QedliftPath>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct QedliftPath {
    pub label: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub tracked_written: Option<bool>,
    #[serde(default)]
    pub vm_error: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Find the outcome line. `Ok(None)` means a qedlift that prints none. A line that does not
/// parse, or more than one, is an error: an unknown shape must never pass.
pub(crate) fn parse_transition_outcome(stderr: &str) -> Result<Option<QedliftTransition>> {
    let lines: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.trim().strip_prefix(OUTCOME_PREFIX))
        .collect();
    match lines.as_slice() {
        [] => Ok(None),
        [json] => serde_json::from_str(json)
            .map(Some)
            .with_context(|| format!("unrecognized qedlift transition outcome: {json}")),
        many => bail!(
            "qedlift printed {} transition outcome lines; expected one",
            many.len()
        ),
    }
}

/// What a path does, as qedlift reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PathKind {
    /// A clean return with an exit code. A spec rejection is usually this, with a non-zero
    /// code and no tracked write.
    Return {
        exit_code: Option<i64>,
        tracked_written: Option<bool>,
    },
    /// A VM fault (`abort`, `access_violation`): a fault obligation, not a clean return.
    Fault { vm_error: Option<String> },
    /// qedlift gave no kind for this path.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PathStatus {
    /// Lifted, and consistent with what the spec expects of it.
    Lifted,
    /// Lifted, but qedgen cannot confirm its kind.
    Unconfirmed,
    /// The spec expects this path, but no trace exists.
    Incomplete,
    /// qedlift refused it, or its kind contradicts the spec.
    Failed,
}

/// One row of the per-path report.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PathRow {
    pub label: String,
    pub expected: bool,
    pub traced: bool,
    pub module: Option<String>,
    #[serde(flatten)]
    pub kind: PathKind,
    pub status: PathStatus,
    pub reason: Option<String>,
    pub message: Option<String>,
}

/// The `--transition` report. Human and JSON output render from this struct.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TransitionReport {
    pub schema_version: u32,
    pub mode: &'static str,
    pub handler: String,
    pub tracked: String,
    pub program: String,
    /// SHA-256 of the `.so` the modules were lifted from. The modules embed its `.text` bytes.
    pub program_sha256: String,
    pub qedlift: String,
    pub verdict: Verdict,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub paths: Vec<PathRow>,
    pub all_discovered_paths_verified: bool,
    pub all_expected_paths_verified: bool,
    pub coverage_note: &'static str,
    pub lean_check: LeanCheck,
    pub artifacts: Vec<String>,
}

/// `MathOverflow` -> `math_overflow`, the trace label a rejection path must use.
pub(crate) fn snake_label(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The spec-expected path labels for a handler: `success`, plus one per distinct
/// `requires ... else E`.
pub(crate) fn expected_labels(handler: &crate::check::ParsedHandler) -> BTreeSet<String> {
    let mut labels: BTreeSet<String> = handler
        .requires
        .iter()
        .filter_map(|r| r.error_name.as_deref())
        .map(snake_label)
        .collect();
    labels.insert("success".to_string());
    labels
}

/// Trace labels beside the `.so`: `<stem>_<label>.pcs`, the convention qedlift discovers.
pub(crate) fn traced_labels(so: &Path) -> BTreeSet<String> {
    let stem = so
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir = so.parent().unwrap_or_else(|| Path::new("."));
    let prefix = format!("{stem}_");
    let mut out = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("pcs") {
                continue;
            }
            if let Some(label) = p
                .file_stem()
                .and_then(|x| x.to_str())
                .and_then(|s| s.strip_prefix(&prefix))
            {
                if !label.is_empty() {
                    out.insert(label.to_string());
                }
            }
        }
    }
    out
}

/// Build the per-path rows. `outcome` is qedlift's line when it printed one.
pub(crate) fn path_rows(
    expected: &BTreeSet<String>,
    traced: &BTreeSet<String>,
    outcome: Option<&QedliftTransition>,
) -> Result<Vec<PathRow>> {
    let labels: BTreeSet<&String> = expected.iter().chain(traced).collect();
    let mut rows = Vec::new();
    for label in labels {
        let is_expected = expected.contains(label);
        let is_traced = traced.contains(label);
        let reported = outcome.and_then(|o| o.paths.iter().find(|p| &p.label == label));
        let mut row = PathRow {
            label: label.clone(),
            expected: is_expected,
            traced: is_traced,
            module: reported.and_then(|p| p.module.clone()),
            kind: PathKind::Unknown,
            status: PathStatus::Unconfirmed,
            reason: None,
            message: None,
        };
        if !is_traced {
            row.status = PathStatus::Incomplete;
            row.reason = Some("no_trace".to_string());
            row.message = Some(format!("the spec expects a `{label}` path; add its trace"));
            rows.push(row);
            continue;
        }
        let Some(p) = reported else {
            rows.push(row);
            continue;
        };
        if let Some(status) = p.status.as_deref().filter(|s| *s != "emitted") {
            row.status = PathStatus::Failed;
            row.reason = p.reason.clone().or_else(|| Some(status.to_string()));
            row.message = p.message.clone();
            rows.push(row);
            continue;
        }
        row.kind = match p.kind.as_deref() {
            Some("return") => PathKind::Return {
                exit_code: p.exit_code,
                tracked_written: p.tracked_written,
            },
            Some("fault") => PathKind::Fault {
                vm_error: p.vm_error.clone(),
            },
            Some(other) => bail!("qedlift reported an unknown path kind `{other}` for `{label}`"),
            None => PathKind::Unknown,
        };
        row.status = match (&row.kind, label.as_str(), is_expected) {
            (PathKind::Unknown, _, _) => PathStatus::Unconfirmed,
            // The success path must return 0.
            (PathKind::Return { exit_code, .. }, "success", _) if *exit_code != Some(0) => {
                row.reason = Some("success_path_not_ok".to_string());
                row.message = Some(format!("the success path returned {exit_code:?}, not 0"));
                PathStatus::Failed
            }
            (PathKind::Fault { .. }, "success", _) => {
                row.reason = Some("success_path_faults".to_string());
                row.message = Some("the success path ends in a VM fault".to_string());
                PathStatus::Failed
            }
            // A spec rejection must not succeed or write the tracked field.
            (
                PathKind::Return {
                    exit_code,
                    tracked_written,
                },
                _,
                true,
            ) if label != "success"
                && (*exit_code == Some(0) || *tracked_written == Some(true)) =>
            {
                row.reason = Some("rejection_path_not_rejected".to_string());
                row.message = Some(format!(
                    "the `{label}` rejection path returned {exit_code:?} with tracked write \
                     {tracked_written:?}"
                ));
                PathStatus::Failed
            }
            _ => PathStatus::Lifted,
        };
        rows.push(row);
    }
    Ok(rows)
}

/// Combine the rows and the Lean check into the overall verdict.
pub(crate) fn overall_verdict(rows: &[PathRow], lean: &LeanCheck) -> (Verdict, Option<String>) {
    if rows.iter().any(|r| r.status == PathStatus::Failed) {
        return (Verdict::Failed, Some("path_failed".to_string()));
    }
    if matches!(lean, LeanCheck::Failed { .. }) {
        return (Verdict::Failed, Some("lean_check_failed".to_string()));
    }
    if rows.iter().any(|r| r.status == PathStatus::Incomplete) {
        return (
            Verdict::Incomplete,
            Some("expected_path_missing".to_string()),
        );
    }
    if rows.iter().any(|r| r.status == PathStatus::Unconfirmed) {
        return (Verdict::Incomplete, Some("no_path_outcomes".to_string()));
    }
    match lean {
        LeanCheck::Passed { .. } => (Verdict::Verified, None),
        LeanCheck::NotRun { .. } | LeanCheck::Failed { .. } => (Verdict::Emitted, None),
    }
}

/// Every `.lean` file qedlift wrote into `dir`.
pub(crate) fn emitted_modules(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("lean"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Run the Lean check on `modules`, or say why it did not run.
pub(crate) fn lean_check(project: Option<PathBuf>, modules: &[PathBuf]) -> LeanCheck {
    match project {
        None => LeanCheck::NotRun {
            why: "no Lake project; pass --lean-project, or put --out-dir inside a Lake project \
                  that requires qedsvm"
                .to_string(),
        },
        Some(project) => {
            let shown = project.display().to_string();
            match check_modules(&project, modules) {
                Ok(LeanResult::Passed) => LeanCheck::Passed { project: shown },
                Ok(LeanResult::Failed(output)) => LeanCheck::Failed {
                    project: shown,
                    output,
                },
                Err(e) => LeanCheck::Failed {
                    project: shown,
                    output: format!("{e:#}"),
                },
            }
        }
    }
}

impl TransitionReport {
    pub(crate) fn render_human(&self) -> String {
        let mut s = String::new();
        s.push_str("=== qedgen discharge (whole-transition) ===\n");
        s.push_str(&format!("  spec handler : {}\n", self.handler));
        s.push_str(&format!("  tracked      : {}\n", self.tracked));
        s.push_str(&format!(
            "  program      : {} (sha256 {})\n",
            self.program,
            &self.program_sha256[..16.min(self.program_sha256.len())]
        ));
        s.push_str(&format!("  qedlift      : {}\n", self.qedlift));
        s.push_str(&format!("  verdict      : {}\n", self.verdict.label()));
        if let Some(reason) = &self.reason {
            s.push_str(&format!("  reason       : {reason}\n"));
        }
        if let Some(message) = &self.message {
            for (i, line) in message.lines().enumerate() {
                let label = if i == 0 {
                    "  message      : "
                } else {
                    "                 "
                };
                s.push_str(&format!("{label}{line}\n"));
            }
        }
        s.push_str("  paths        :\n");
        for r in &self.paths {
            let kind = match &r.kind {
                PathKind::Return { exit_code, .. } => match exit_code {
                    Some(code) => format!("return {code}"),
                    None => "return".to_string(),
                },
                PathKind::Fault { vm_error } => {
                    format!("fault {}", vm_error.as_deref().unwrap_or("?"))
                }
                PathKind::Unknown => "kind unknown".to_string(),
            };
            let origin = match (r.expected, r.traced) {
                (true, true) => "expected, traced",
                (true, false) => "expected, NO TRACE",
                (false, true) => "traced, not in spec",
                (false, false) => "?",
            };
            let status = match r.status {
                PathStatus::Lifted => "lifted",
                PathStatus::Unconfirmed => "lifted, unconfirmed",
                PathStatus::Incomplete => "INCOMPLETE",
                PathStatus::Failed => "FAILED",
            };
            s.push_str(&format!(
                "    {:<18} {:<20} {:<22} {status}\n",
                r.label, kind, origin
            ));
            if let Some(m) = &r.message {
                s.push_str(&format!("                       {m}\n"));
            }
        }
        s.push_str(&format!(
            "  discovered   : {}\n",
            if self.all_discovered_paths_verified {
                "all discovered paths verified"
            } else {
                "not all discovered paths verified"
            }
        ));
        s.push_str(&format!(
            "  spec paths   : {}\n",
            if self.all_expected_paths_verified {
                "all spec-expected paths verified"
            } else {
                "not all spec-expected paths verified"
            }
        ));
        s.push_str(&format!("  coverage     : {}\n", self.coverage_note));
        match &self.lean_check {
            LeanCheck::NotRun { why } => s.push_str(&format!("  lean check   : not run ({why})\n")),
            LeanCheck::Passed { project } => {
                s.push_str(&format!("  lean check   : passed (project {project})\n"))
            }
            LeanCheck::Failed { project, output } => {
                s.push_str(&format!("  lean check   : FAILED (project {project})\n"));
                for line in output.lines() {
                    s.push_str(&format!("                 {line}\n"));
                }
            }
        }
        for (i, a) in self.artifacts.iter().enumerate() {
            let label = if i == 0 {
                "  persisted    : "
            } else {
                "                 "
            };
            s.push_str(&format!("{label}{a}\n"));
        }
        s
    }
}

pub(crate) fn new_report(
    handler: &str,
    tracked: String,
    so: &Path,
    qedlift: &Path,
) -> Result<TransitionReport> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(so).with_context(|| format!("reading {}", so.display()))?;
    Ok(TransitionReport {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: "transition",
        handler: handler.to_string(),
        tracked,
        program: so.display().to_string(),
        program_sha256: format!("{:x}", Sha256::digest(&bytes)),
        qedlift: qedlift.display().to_string(),
        verdict: Verdict::Failed,
        reason: None,
        message: None,
        paths: Vec::new(),
        all_discovered_paths_verified: false,
        all_expected_paths_verified: false,
        coverage_note: COVERAGE_NOTE,
        lean_check: LeanCheck::NotRun {
            why: "qedlift did not emit a transition bundle".to_string(),
        },
        artifacts: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn outcome(paths: serde_json::Value) -> QedliftTransition {
        serde_json::from_value(serde_json::json!({ "status": "emitted", "paths": paths })).unwrap()
    }

    #[test]
    fn snake_label_matches_spec_error_names() {
        assert_eq!(snake_label("MathOverflow"), "math_overflow");
        assert_eq!(snake_label("ZeroAmount"), "zero_amount");
        assert_eq!(snake_label("already_snake"), "already_snake");
    }

    #[test]
    fn parses_outcome_line_and_fails_closed() {
        let stderr = "noise\ntransition outcome: {\"status\":\"emitted\",\"paths\":[]}\n";
        assert_eq!(
            parse_transition_outcome(stderr).unwrap().unwrap().status,
            "emitted"
        );
        assert!(parse_transition_outcome("nothing here").unwrap().is_none());
        assert!(parse_transition_outcome("transition outcome: not json").is_err());
    }

    /// Success plus a rejection path, both traced and reported: verified once Lean passes.
    /// The rejection is a clean return with a non-zero code; a VM fault is its own kind.
    #[test]
    fn rows_report_return_and_fault_paths() {
        let expected = set(&["success", "zero_amount"]);
        let traced = set(&["success", "zero_amount", "oob"]);
        let o = outcome(serde_json::json!([
            { "label": "success", "kind": "return", "exit_code": 0, "tracked_written": true },
            { "label": "zero_amount", "kind": "return", "exit_code": 1, "tracked_written": false },
            { "label": "oob", "kind": "fault", "vm_error": "access_violation" }
        ]));
        let rows = path_rows(&expected, &traced, Some(&o)).unwrap();
        assert!(
            rows.iter().all(|r| r.status == PathStatus::Lifted),
            "{rows:?}"
        );
        let oob = rows.iter().find(|r| r.label == "oob").unwrap();
        assert!(!oob.expected);
        assert_eq!(
            oob.kind,
            PathKind::Fault {
                vm_error: Some("access_violation".into())
            }
        );
        let lean = LeanCheck::Passed {
            project: "p".into(),
        };
        assert_eq!(overall_verdict(&rows, &lean).0, Verdict::Verified);
    }

    /// A spec-expected path with no trace is incomplete, never a success.
    #[test]
    fn missing_expected_trace_is_incomplete() {
        let o = outcome(serde_json::json!([
            { "label": "success", "kind": "return", "exit_code": 0, "tracked_written": true }
        ]));
        let rows = path_rows(
            &set(&["success", "zero_amount"]),
            &set(&["success"]),
            Some(&o),
        )
        .unwrap();
        let missing = rows.iter().find(|r| r.label == "zero_amount").unwrap();
        assert_eq!(missing.status, PathStatus::Incomplete);
        let lean = LeanCheck::Passed {
            project: "p".into(),
        };
        assert_eq!(overall_verdict(&rows, &lean).0, Verdict::Incomplete);
    }

    /// Without qedlift's outcome line the kinds are unknown, so the verdict is capped at
    /// `incomplete` even when Lean passes.
    #[test]
    fn no_outcome_line_caps_the_verdict() {
        let rows = path_rows(&set(&["success"]), &set(&["success", "abort"]), None).unwrap();
        assert!(rows.iter().all(|r| r.status == PathStatus::Unconfirmed));
        let lean = LeanCheck::Passed {
            project: "p".into(),
        };
        assert_eq!(
            overall_verdict(&rows, &lean),
            (Verdict::Incomplete, Some("no_path_outcomes".to_string()))
        );
    }

    /// A rejection path that succeeds or writes the tracked field, a faulting success path,
    /// and a path qedlift refused all fail.
    #[test]
    fn contradictions_fail() {
        let cases = [
            serde_json::json!({ "label": "zero_amount", "kind": "return", "exit_code": 0 }),
            serde_json::json!({ "label": "zero_amount", "kind": "return", "exit_code": 1, "tracked_written": true }),
            serde_json::json!({ "label": "success", "kind": "fault", "vm_error": "abort" }),
            serde_json::json!({ "label": "success", "status": "rejected", "reason": "mutation_mismatch" }),
        ];
        for case in cases {
            let label = case["label"].as_str().unwrap().to_string();
            let o = outcome(serde_json::json!([case]));
            let rows = path_rows(
                &set(&["success", "zero_amount"]),
                &set(&["success", "zero_amount"]),
                Some(&o),
            )
            .unwrap();
            let row = rows.iter().find(|r| r.label == label).unwrap();
            assert_eq!(row.status, PathStatus::Failed, "{row:?}");
            let lean = LeanCheck::Passed {
                project: "p".into(),
            };
            assert_eq!(overall_verdict(&rows, &lean).0, Verdict::Failed);
        }
    }

    #[test]
    fn unknown_path_kind_fails_closed() {
        let o = outcome(serde_json::json!([{ "label": "success", "kind": "teleport" }]));
        assert!(path_rows(&set(&["success"]), &set(&["success"]), Some(&o)).is_err());
    }

    #[test]
    fn finds_traces_beside_the_program() {
        let tmp = tempfile::tempdir().unwrap();
        for f in [
            "prog.so",
            "prog_success.pcs",
            "prog_zero_amount.pcs",
            "other_x.pcs",
        ] {
            std::fs::write(tmp.path().join(f), "").unwrap();
        }
        assert_eq!(
            traced_labels(&tmp.path().join("prog.so")),
            set(&["success", "zero_amount"])
        );
    }
}
