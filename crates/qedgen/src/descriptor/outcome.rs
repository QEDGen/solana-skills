//! Discharge verdicts (#406): read qedlift's structured refinement outcome and
//! combine it with the Lean check into one verdict.
//!
//! qedlift prints one `refinement outcome: {json}` line to stderr. `emitted`
//! only means the Lean module was generated. A property is `verified` only
//! after Lean accepts the generated modules. Older qedlift builds print no
//! outcome line; for those the verdict falls back to the exit status and the
//! emitted files.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const OUTCOME_PREFIX: &str = "refinement outcome: ";

/// JSON report schema version (`qedgen discharge --json`).
const REPORT_SCHEMA_VERSION: u32 = 1;

/// qedlift's `LiftResult.refinement_outcome`, as printed on stderr.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum QedliftOutcome {
    NotRequested,
    Emitted,
    Rejected { reason: String, message: String },
    Unsupported { reason: String, message: String },
}

/// Find the outcome line in qedlift's stderr. `Ok(None)` means an older
/// qedlift that prints no outcome line. A line that is present but does not
/// parse, or more than one line, is an error: an unknown status must never
/// pass.
pub(crate) fn parse_outcome(stderr: &str) -> Result<Option<QedliftOutcome>> {
    let lines: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.trim().strip_prefix(OUTCOME_PREFIX))
        .collect();
    match lines.as_slice() {
        [] => Ok(None),
        [json] => serde_json::from_str(json)
            .map(Some)
            .with_context(|| format!("unrecognized qedlift refinement outcome: {json}")),
        many => bail!(
            "qedlift printed {} refinement outcome lines; expected one",
            many.len()
        ),
    }
}

/// The discharge verdict. Only `verified` means Lean accepted the proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// qedlift emitted the refinement and Lean accepted it.
    Verified,
    /// qedlift emitted the refinement, but no Lean check ran.
    Emitted,
    /// The obligation conflicts with the layout or bytecode evidence.
    Rejected,
    /// Required binding information or a supported shape is missing.
    Unsupported,
    /// qedlift lifted the program but did not act on the descriptor.
    ModelOnly,
    /// `--transition`: a spec-expected path has no trace, or qedlift gave no per-path
    /// outcome, so the discovered paths cannot stand for the whole transition.
    Incomplete,
    /// qedlift or the Lean check failed.
    Failed,
}

impl Verdict {
    /// Whether the command exits successfully. `emitted` passes because the
    /// report says plainly that it is not verified; CI that needs a proof
    /// gates on `verdict == "verified"` in the JSON report.
    pub(crate) fn passes(self) -> bool {
        match self {
            Verdict::Verified | Verdict::Emitted => true,
            Verdict::Rejected
            | Verdict::Unsupported
            | Verdict::ModelOnly
            | Verdict::Incomplete
            | Verdict::Failed => false,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::Emitted => "emitted",
            Verdict::Rejected => "rejected",
            Verdict::Unsupported => "unsupported",
            Verdict::ModelOnly => "model_only",
            Verdict::Incomplete => "incomplete",
            Verdict::Failed => "failed",
        }
    }
}

/// qedlift's result before any Lean check. `Verdict::Emitted` here means
/// "go on to the Lean check".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiftVerdict {
    pub verdict: Verdict,
    pub reason: Option<String>,
    pub message: Option<String>,
}

impl LiftVerdict {
    fn emitted() -> Self {
        LiftVerdict {
            verdict: Verdict::Emitted,
            reason: None,
            message: None,
        }
    }

    pub(crate) fn with(verdict: Verdict, reason: &str, message: impl Into<String>) -> Self {
        LiftVerdict {
            verdict,
            reason: Some(reason.to_string()),
            message: Some(message.into()),
        }
    }
}

/// Classify one qedlift run. `artifacts_present` is whether this run's
/// temp output directory holds both emitted modules.
pub(crate) fn classify_lift(
    exit_ok: bool,
    outcome: Option<&QedliftOutcome>,
    artifacts_present: bool,
) -> LiftVerdict {
    match outcome {
        Some(QedliftOutcome::Rejected { reason, message }) => {
            LiftVerdict::with(Verdict::Rejected, reason, message.clone())
        }
        Some(QedliftOutcome::Unsupported { reason, message }) => {
            LiftVerdict::with(Verdict::Unsupported, reason, message.clone())
        }
        Some(QedliftOutcome::NotRequested) => LiftVerdict::with(
            Verdict::ModelOnly,
            "not_requested",
            "qedlift lifted the program but did not act on the descriptor",
        ),
        Some(QedliftOutcome::Emitted) if exit_ok && artifacts_present => LiftVerdict::emitted(),
        Some(QedliftOutcome::Emitted) => LiftVerdict::with(
            Verdict::Failed,
            "emitted_without_artifacts",
            "qedlift reported `emitted` but failed or wrote no modules",
        ),
        None if exit_ok && artifacts_present => LiftVerdict::emitted(),
        None if exit_ok => LiftVerdict::with(
            Verdict::Failed,
            "no_refinement",
            "qedlift ran but emitted no refinement (the bytes likely do not realise the \
             claimed obligation)",
        ),
        None => LiftVerdict::with(Verdict::Failed, "qedlift_failed", "qedlift failed"),
    }
}

/// Result of the Lean check on the emitted modules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum LeanCheck {
    NotRun { why: String },
    Passed { project: String },
    Failed { project: String, output: String },
}

/// One discharge result. The human report and the JSON report both render
/// from this struct, so they always show the same verdict.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DischargeReport {
    pub schema_version: u32,
    pub handler: String,
    pub obligation: String,
    pub program: String,
    pub qedlift: String,
    pub verdict: Verdict,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub lean_check: LeanCheck,
    /// Persisted proof modules. Empty unless the verdict passes and
    /// `--out-dir` was given.
    pub artifacts: Vec<String>,
}

impl DischargeReport {
    pub(crate) fn new(
        handler: &str,
        obligation: String,
        program: String,
        qedlift: String,
        lift: LiftVerdict,
    ) -> Self {
        let why = match lift.verdict {
            Verdict::Emitted => "not attempted yet",
            Verdict::Verified
            | Verdict::Rejected
            | Verdict::Unsupported
            | Verdict::ModelOnly
            | Verdict::Incomplete
            | Verdict::Failed => "qedlift did not emit a refinement",
        };
        DischargeReport {
            schema_version: REPORT_SCHEMA_VERSION,
            handler: handler.to_string(),
            obligation,
            program,
            qedlift,
            verdict: lift.verdict,
            reason: lift.reason,
            message: lift.message,
            lean_check: LeanCheck::NotRun {
                why: why.to_string(),
            },
            artifacts: Vec::new(),
        }
    }

    pub(crate) fn render_human(&self) -> String {
        let mut s = String::new();
        s.push_str("=== qedgen discharge ===\n");
        s.push_str(&format!("  spec handler : {}\n", self.handler));
        s.push_str(&format!("  obligation   : {}\n", self.obligation));
        s.push_str(&format!("  program      : {}\n", self.program));
        s.push_str(&format!("  qedlift      : {}\n", self.qedlift));
        let summary = match self.verdict {
            Verdict::Verified => "VERIFIED : qedlift emitted the refinement and Lean accepted it.",
            Verdict::Emitted => {
                "EMITTED (not verified) : qedlift emitted the refinement, but no Lean check ran."
            }
            Verdict::Rejected => "REJECTED : the obligation conflicts with the bytes or layout.",
            Verdict::Unsupported => "UNSUPPORTED : qedlift cannot bind this obligation.",
            Verdict::ModelOnly => "MODEL ONLY : qedlift did not act on the descriptor.",
            Verdict::Incomplete => "INCOMPLETE : not every expected path is covered.",
            Verdict::Failed => "FAILED : the discharge did not complete.",
        };
        s.push_str(&format!("  verdict      : {summary}\n"));
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
        match &self.lean_check {
            LeanCheck::NotRun { why } => {
                s.push_str(&format!("  lean check   : not run ({why})\n"));
            }
            LeanCheck::Passed { project } => {
                s.push_str(&format!("  lean check   : passed (project {project})\n"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_outcome_status() {
        let cases = [
            (r#"{"status":"emitted"}"#, QedliftOutcome::Emitted),
            (
                r#"{"status":"not_requested"}"#,
                QedliftOutcome::NotRequested,
            ),
            (
                r#"{"status":"rejected","reason":"mutation_mismatch","message":"m"}"#,
                QedliftOutcome::Rejected {
                    reason: "mutation_mismatch".into(),
                    message: "m".into(),
                },
            ),
            (
                r#"{"status":"unsupported","reason":"missing_parameter_binding","message":"m"}"#,
                QedliftOutcome::Unsupported {
                    reason: "missing_parameter_binding".into(),
                    message: "m".into(),
                },
            ),
        ];
        for (json, want) in cases {
            let stderr = format!("pc=0 noise\nrefinement outcome: {json}\n=== qedlift ===\n");
            assert_eq!(parse_outcome(&stderr).unwrap(), Some(want), "{json}");
        }
    }

    #[test]
    fn missing_outcome_line_is_legacy() {
        assert_eq!(parse_outcome("=== qedlift ===\n").unwrap(), None);
    }

    /// An unknown status or a malformed line must fail, never pass.
    #[test]
    fn unknown_or_duplicate_outcome_fails_closed() {
        assert!(parse_outcome(r#"refinement outcome: {"status":"proven"}"#).is_err());
        assert!(parse_outcome("refinement outcome: not json").is_err());
        let two = "refinement outcome: {\"status\":\"emitted\"}\n\
                   refinement outcome: {\"status\":\"emitted\"}\n";
        assert!(parse_outcome(two).is_err());
    }

    #[test]
    fn rejected_and_unsupported_keep_reason_and_message() {
        let r = classify_lift(
            false,
            Some(&QedliftOutcome::Rejected {
                reason: "parameter_mismatch".into(),
                message: "operand is not amount".into(),
            }),
            false,
        );
        assert_eq!(r.verdict, Verdict::Rejected);
        assert_eq!(r.reason.as_deref(), Some("parameter_mismatch"));
        assert_eq!(r.message.as_deref(), Some("operand is not amount"));

        let u = classify_lift(
            false,
            Some(&QedliftOutcome::Unsupported {
                reason: "missing_parameter_binding".into(),
                message: "requires schema v3".into(),
            }),
            false,
        );
        assert_eq!(u.verdict, Verdict::Unsupported);
        assert!(!u.verdict.passes());
    }

    #[test]
    fn not_requested_is_model_only() {
        let r = classify_lift(true, Some(&QedliftOutcome::NotRequested), false);
        assert_eq!(r.verdict, Verdict::ModelOnly);
        assert!(!r.verdict.passes());
    }

    /// `emitted` needs a clean exit and both modules; otherwise it fails.
    #[test]
    fn emitted_requires_exit_and_artifacts() {
        let e = Some(&QedliftOutcome::Emitted);
        assert_eq!(classify_lift(true, e, true).verdict, Verdict::Emitted);
        assert_eq!(classify_lift(false, e, true).verdict, Verdict::Failed);
        assert_eq!(classify_lift(true, e, false).verdict, Verdict::Failed);
    }

    #[test]
    fn legacy_qedlift_falls_back_to_exit_and_files() {
        assert_eq!(classify_lift(true, None, true).verdict, Verdict::Emitted);
        assert_eq!(classify_lift(true, None, false).verdict, Verdict::Failed);
        assert_eq!(classify_lift(false, None, true).verdict, Verdict::Failed);
    }

    /// Human and JSON output render the same verdict from one report.
    #[test]
    fn human_and_json_share_the_verdict() {
        let mut report = DischargeReport::new(
            "deposit",
            "vault.total += amount".into(),
            "vault.so".into(),
            "qedlift".into(),
            LiftVerdict::with(
                Verdict::Unsupported,
                "missing_parameter_binding",
                "needs v3",
            ),
        );
        report.lean_check = LeanCheck::NotRun {
            why: "qedlift did not emit a refinement".into(),
        };
        let json: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(json["verdict"], "unsupported");
        assert_eq!(json["reason"], "missing_parameter_binding");
        assert_eq!(json["lean_check"]["status"], "not_run");
        let human = report.render_human();
        assert!(human.contains("UNSUPPORTED"), "{human}");
        assert!(human.contains("missing_parameter_binding"), "{human}");
    }
}
