//! Persisted verification evidence — the record `qedgen stamp` gates on
//! (PRD `docs/design/spec-elicitation-prd.md` §5.1).
//!
//! `qedgen verify` appends nothing to source and proves nothing at stamp
//! time; the division of labor is: a source-bound backend establishes the
//! implementation claim, **this record freezes what ran**, `stamp`
//! requires it, and the `#[qed]` proc macro guards the freeze at compile
//! time. Without a durable record, "verified" would be a claim about a
//! run nobody can point to.
//!
//! Assurance honesty (§3.1): only *implementation-bound* backends count
//! toward `implementation_verified` — Miri reproducers and Mollusk
//! probe-repros run the real code by construction; a Kani run counts only
//! when its harness file is an impl harness (`kani_impl*.rs`, the
//! `--kani-impl` codegen output that drives the real handler). Plain
//! proptest/Kani/Lean exercise the spec model and never flip the flag.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::{BackendStatus, VerifyReport};

pub const EVIDENCE_FILENAME: &str = "verify-evidence.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendEvidence {
    pub name: String,
    /// `passed` | `failed` | `skipped`.
    pub status: String,
    /// Whether this backend exercised the real implementation (vs the
    /// spec model).
    pub implementation_bound: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyEvidence {
    pub schema_version: u32,
    /// Spec path as passed to `verify` (display form, for humans).
    pub spec: String,
    /// `sha256_hex16` over the spec file bytes at verify time. `stamp`
    /// refuses on mismatch — an edited spec invalidates the evidence.
    pub spec_hash: String,
    pub recorded_at_unix: u64,
    pub backends: Vec<BackendEvidence>,
    /// True iff at least one implementation-bound backend passed. The
    /// only field `stamp` accepts as license for `#[qed(verified)]`.
    pub implementation_verified: bool,
}

/// Canonical evidence location for a spec: `<spec_dir>/.qed/verify-evidence.json`
/// (specs live at the program root, so this is the project `.qed/`).
pub fn evidence_path_for_spec(spec: &Path) -> PathBuf {
    spec.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".qed")
        .join(EVIDENCE_FILENAME)
}

pub fn spec_file_hash(spec: &Path) -> Result<String> {
    let bytes = std::fs::read_to_string(spec)
        .with_context(|| format!("reading spec {}", spec.display()))?;
    Ok(qedgen_hash_core::sha256_hex16(&bytes))
}

fn status_str(status: BackendStatus) -> &'static str {
    match status {
        BackendStatus::Passed => "passed",
        BackendStatus::Failed => "failed",
        BackendStatus::Skipped => "skipped",
    }
}

/// Build the evidence record from a completed verify run.
///
/// `kani_impl_bound`: whether the Kani harness that ran is an impl
/// harness (`--kani-path` file name starts with `kani_impl`) — the
/// verify layer itself cannot tell a model harness from an impl harness,
/// so the caller classifies by the codegen naming contract.
/// `probe_repros_passed`: outcome of the Mollusk probe-repro stage when
/// it ran (a separate stage outside the backend rollup).
pub fn build(
    spec: &Path,
    report: &VerifyReport,
    kani_impl_bound: bool,
    probe_repros_passed: Option<bool>,
) -> Result<VerifyEvidence> {
    let mut backends: Vec<BackendEvidence> = report
        .backends
        .iter()
        .map(|b| BackendEvidence {
            name: b.name.to_string(),
            status: status_str(b.status).to_string(),
            implementation_bound: match b.name {
                "miri" => true,
                "kani" => kani_impl_bound,
                _ => false,
            },
        })
        .collect();
    if let Some(passed) = probe_repros_passed {
        backends.push(BackendEvidence {
            name: "probe_repros".to_string(),
            status: if passed { "passed" } else { "failed" }.to_string(),
            implementation_bound: true,
        });
    }
    let implementation_verified = backends
        .iter()
        .any(|b| b.implementation_bound && b.status == "passed");
    Ok(VerifyEvidence {
        schema_version: 1,
        spec: spec.display().to_string(),
        spec_hash: spec_file_hash(spec)?,
        recorded_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        backends,
        implementation_verified,
    })
}

/// Write the record to the canonical path; returns where it landed.
/// Best-effort by contract at the call site (a failed write must not turn
/// a green verify red) — the caller logs and continues.
pub fn record(evidence: &VerifyEvidence, spec: &Path) -> Result<PathBuf> {
    let path = evidence_path_for_spec(spec);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(evidence)?),
    )?;
    Ok(path)
}

pub fn load(path: &Path) -> Result<VerifyEvidence> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading verification evidence at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing verification evidence at {}", path.display()))
}

/// The `stamp` gate: recorded evidence must exist, match the spec being
/// stamped byte-for-byte (by hash), and carry a passing
/// implementation-bound backend. Returns the loaded evidence on success;
/// every failure names the exact remediation.
pub fn require_stamp_evidence(spec: &Path, evidence_path: Option<&Path>) -> Result<VerifyEvidence> {
    let path = evidence_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| evidence_path_for_spec(spec));
    if !path.exists() {
        anyhow::bail!(
            "no verification evidence at {} — `stamp` only freezes claims a source-bound \
             backend established. Run `qedgen verify --spec {}` with an implementation-bound \
             backend (miri, a kani_impl harness, or --probe-repros) first",
            path.display(),
            spec.display()
        );
    }
    let evidence = load(&path)?;
    let current = spec_file_hash(spec)?;
    if evidence.spec_hash != current {
        anyhow::bail!(
            "verification evidence at {} was recorded for spec hash {} but {} now hashes to {} — \
             the spec changed since it was verified. Re-run `qedgen verify` and stamp again",
            path.display(),
            evidence.spec_hash,
            spec.display(),
            current
        );
    }
    if !evidence.implementation_verified {
        let ran: Vec<String> = evidence
            .backends
            .iter()
            .map(|b| format!("{} ({}{})", b.name, b.status, if b.implementation_bound { ", impl-bound" } else { "" }))
            .collect();
        anyhow::bail!(
            "evidence at {} records no passing implementation-bound backend (ran: {}) — \
             checking/model-tested results are not eligible for `#[qed(verified)]`. Run an \
             implementation-bound backend (miri, a kani_impl harness via `codegen --kani-impl`, \
             or `verify --probe-repros`) first",
            path.display(),
            ran.join(", ")
        );
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::BackendReport;

    fn report(backends: Vec<(&'static str, BackendStatus)>) -> VerifyReport {
        VerifyReport {
            spec: PathBuf::from("x.qedspec"),
            backends: backends
                .into_iter()
                .map(|(name, status)| BackendReport {
                    name,
                    status,
                    duration_ms: 0,
                    detail: None,
                    log_path: None,
                    counterexamples: Vec::new(),
                    axioms: Vec::new(),
                })
                .collect(),
        }
    }

    fn tmp_spec(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qedgen-evidence-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = dir.join("prog.qedspec");
        std::fs::write(&spec, "spec Prog\n").unwrap();
        spec
    }

    #[test]
    fn model_backends_never_flip_implementation_verified() {
        let spec = tmp_spec("model");
        let r = report(vec![
            ("proptest", BackendStatus::Passed),
            ("kani", BackendStatus::Passed),
            ("lean", BackendStatus::Passed),
        ]);
        // kani ran a MODEL harness (kani_impl_bound = false).
        let e = build(&spec, &r, false, None).unwrap();
        assert!(!e.implementation_verified);
        let _ = std::fs::remove_dir_all(spec.parent().unwrap());
    }

    #[test]
    fn miri_pass_and_kani_impl_pass_count() {
        let spec = tmp_spec("impl");
        let r = report(vec![("miri", BackendStatus::Passed)]);
        assert!(build(&spec, &r, false, None).unwrap().implementation_verified);
        let r = report(vec![("kani", BackendStatus::Passed)]);
        assert!(build(&spec, &r, true, None).unwrap().implementation_verified);
        let r = report(vec![("kani", BackendStatus::Failed)]);
        assert!(!build(&spec, &r, true, None).unwrap().implementation_verified);
        let r = report(vec![]);
        assert!(build(&spec, &r, false, Some(true)).unwrap().implementation_verified);
        let _ = std::fs::remove_dir_all(spec.parent().unwrap());
    }

    #[test]
    fn stamp_gate_paths() {
        let spec = tmp_spec("gate");
        // No evidence file → refusal naming verify.
        let err = require_stamp_evidence(&spec, None).unwrap_err().to_string();
        assert!(err.contains("no verification evidence"), "{err}");

        // Model-only evidence → refusal naming eligibility.
        let r = report(vec![("proptest", BackendStatus::Passed)]);
        let e = build(&spec, &r, false, None).unwrap();
        record(&e, &spec).unwrap();
        let err = require_stamp_evidence(&spec, None).unwrap_err().to_string();
        assert!(err.contains("not eligible"), "{err}");

        // Impl-bound pass → gate opens.
        let r = report(vec![("miri", BackendStatus::Passed)]);
        let e = build(&spec, &r, false, None).unwrap();
        record(&e, &spec).unwrap();
        assert!(require_stamp_evidence(&spec, None).is_ok());

        // Spec edited after verify → hash mismatch refusal.
        std::fs::write(&spec, "spec Prog\n\ntype Error | New\n").unwrap();
        let err = require_stamp_evidence(&spec, None).unwrap_err().to_string();
        assert!(err.contains("spec changed since it was verified"), "{err}");
        let _ = std::fs::remove_dir_all(spec.parent().unwrap());
    }
}
