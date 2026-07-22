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
//! toward `implementation_verified` — Miri runs the real code by
//! construction; a Kani run counts only
//! when its harness file is an impl harness (`kani_impl*.rs`, the
//! `--kani-impl` codegen output that drives the real handler). Plain
//! proptest/Kani/Lean exercise the spec model and never flip the flag.
//! Mollusk probe reproducers confirm that bug findings fire; they are not
//! conformance evidence and likewise never flip it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    /// Program crate whose source was present for the implementation-bound
    /// verification run. Display-only; `program_hash` is the binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Deterministic digest of the program's Rust sources and Cargo manifest.
    /// `stamp` recomputes this before emitting attributes, preventing a
    /// verified implementation from being replaced between verify and stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_hash: Option<String>,
    pub recorded_at_unix: u64,
    pub backends: Vec<BackendEvidence>,
    /// True iff at least one implementation-bound backend passed. The
    /// only field `stamp` accepts as license for `#[qed(verified)]`.
    pub implementation_verified: bool,
    /// `sha256_hex16` over the reconciled backend-obligation entries at
    /// verify time (#332) — lets `stamp` and audits see which backend
    /// coverage a verify run was measured against. Absent on sBPF specs
    /// and records written before schema v3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obligations_digest: Option<String>,
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

/// Hash the implementation inputs that determine the stamped handler and
/// Accounts shapes. Generated/build outputs and audit artifacts are excluded;
/// paths are sorted and included in the digest so renames cannot collide.
pub fn program_source_hash(program: &Path) -> Result<String> {
    let root = program
        .canonicalize()
        .with_context(|| format!("resolving program crate {}", program.display()))?;
    let mut files = Vec::new();
    collect_program_inputs(&root, &mut files)?;
    files.sort();
    if files.is_empty() {
        anyhow::bail!(
            "program crate {} contains no Rust sources or Cargo.toml",
            program.display()
        );
    }

    let mut hasher = Sha256::new();
    hasher.update(b"qedgen-program-source-v1\n");
    for path in files {
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?);
        hasher.update(b"\0");
    }
    let hex = format!("{:x}", hasher.finalize());
    Ok(hex[..16].to_string())
}

fn collect_program_inputs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        if path.is_dir() {
            if matches!(file_name.to_str(), Some("target" | ".git" | ".qed")) {
                continue;
            }
            collect_program_inputs(&path, out)?;
            continue;
        }
        let is_rust = path.extension().and_then(|e| e.to_str()) == Some("rs");
        let is_manifest = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == "Cargo.toml" || n == "Cargo.lock");
        if is_rust || is_manifest {
            out.push(path);
        }
    }
    Ok(())
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
    program: Option<&Path>,
    report: &VerifyReport,
    kani_impl_bound: bool,
    probe_repros_passed: Option<bool>,
    obligations_digest: Option<String>,
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
            // Probe reproducers assert that a reported bug fires. They are
            // source-bound evidence about a finding, not proof that the
            // implementation conforms to the spec, so they cannot license a
            // `verified` stamp.
            implementation_bound: false,
        });
    }
    let has_impl_pass = backends
        .iter()
        .any(|b| b.implementation_bound && b.status == "passed");
    let program_hash = program.map(program_source_hash).transpose()?;
    let implementation_verified = has_impl_pass && program_hash.is_some();
    Ok(VerifyEvidence {
        schema_version: 3,
        spec: spec.display().to_string(),
        spec_hash: spec_file_hash(spec)?,
        program: program.map(|p| p.display().to_string()),
        program_hash,
        recorded_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        backends,
        implementation_verified,
        obligations_digest,
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
pub fn require_stamp_evidence(
    spec: &Path,
    program: &Path,
    evidence_path: Option<&Path>,
) -> Result<VerifyEvidence> {
    let path = evidence_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| evidence_path_for_spec(spec));
    if !path.exists() {
        anyhow::bail!(
            "no verification evidence at {} — `stamp` only freezes claims a source-bound \
             backend established. Run `qedgen verify --spec {} --program {}` with an \
             implementation-bound backend (miri or a kani_impl harness) first",
            path.display(),
            spec.display(),
            program.display()
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
            .map(|b| {
                format!(
                    "{} ({}{})",
                    b.name,
                    b.status,
                    if b.implementation_bound {
                        ", impl-bound"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        anyhow::bail!(
            "evidence at {} records no passing implementation-bound backend (ran: {}) — \
             checking/model-tested results are not eligible for `#[qed(verified)]`. Run an \
             implementation-bound backend with `verify --program <crate>` (miri or a kani_impl \
             harness via `codegen --kani-impl`) first",
            path.display(),
            ran.join(", ")
        );
    }
    let Some(recorded_program_hash) = evidence.program_hash.as_deref() else {
        anyhow::bail!(
            "evidence at {} is not bound to a program source tree — re-run `qedgen verify \
             --program {}` with an implementation-bound backend before stamping",
            path.display(),
            program.display()
        );
    };
    let current_program_hash = program_source_hash(program)?;
    if recorded_program_hash != current_program_hash {
        anyhow::bail!(
            "verification evidence at {} was recorded for program hash {} but {} now hashes to \
             {} — the implementation changed since it was verified. Re-run `qedgen verify \
             --program {}` and stamp again",
            path.display(),
            recorded_program_hash,
            program.display(),
            current_program_hash,
            program.display()
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

    fn tmp_program(spec: &Path) -> PathBuf {
        let program = spec.parent().unwrap().join("program");
        std::fs::create_dir_all(program.join("src")).unwrap();
        std::fs::write(
            program.join("Cargo.toml"),
            "[package]\nname = \"program\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(program.join("src/lib.rs"), "pub fn handler() {}\n").unwrap();
        program
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
        let program = tmp_program(&spec);
        let e = build(&spec, Some(&program), &r, false, None, None).unwrap();
        assert!(!e.implementation_verified);
        let _ = std::fs::remove_dir_all(spec.parent().unwrap());
    }

    #[test]
    fn miri_pass_and_kani_impl_pass_count() {
        let spec = tmp_spec("impl");
        let program = tmp_program(&spec);
        let r = report(vec![("miri", BackendStatus::Passed)]);
        assert!(
            build(&spec, Some(&program), &r, false, None, None)
                .unwrap()
                .implementation_verified
        );
        assert!(
            !build(&spec, None, &r, false, None, None)
                .unwrap()
                .implementation_verified
        );
        let r = report(vec![("kani", BackendStatus::Passed)]);
        assert!(
            build(&spec, Some(&program), &r, true, None, None)
                .unwrap()
                .implementation_verified
        );
        let r = report(vec![("kani", BackendStatus::Failed)]);
        assert!(
            !build(&spec, Some(&program), &r, true, None, None)
                .unwrap()
                .implementation_verified
        );
        let r = report(vec![]);
        assert!(
            !build(&spec, Some(&program), &r, false, Some(true), None)
                .unwrap()
                .implementation_verified,
            "a fired bug reproducer is not implementation conformance evidence"
        );
        let _ = std::fs::remove_dir_all(spec.parent().unwrap());
    }

    #[test]
    fn stamp_gate_paths() {
        let spec = tmp_spec("gate");
        let program = tmp_program(&spec);
        // No evidence file → refusal naming verify.
        let err = require_stamp_evidence(&spec, &program, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no verification evidence"), "{err}");

        // Model-only evidence → refusal naming eligibility.
        let r = report(vec![("proptest", BackendStatus::Passed)]);
        let e = build(&spec, Some(&program), &r, false, None, None).unwrap();
        record(&e, &spec).unwrap();
        let err = require_stamp_evidence(&spec, &program, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not eligible"), "{err}");

        // Impl-bound pass → gate opens.
        let r = report(vec![("miri", BackendStatus::Passed)]);
        let e = build(&spec, Some(&program), &r, false, None, None).unwrap();
        record(&e, &spec).unwrap();
        assert!(require_stamp_evidence(&spec, &program, None).is_ok());

        // Program edited after verify → source hash mismatch refusal.
        std::fs::write(
            program.join("src/lib.rs"),
            "pub fn handler() { panic!() }\n",
        )
        .unwrap();
        let err = require_stamp_evidence(&spec, &program, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("implementation changed since it was verified"),
            "{err}"
        );

        // Restore the verified source before testing the independent spec leg.
        std::fs::write(program.join("src/lib.rs"), "pub fn handler() {}\n").unwrap();

        // Spec edited after verify → hash mismatch refusal.
        std::fs::write(&spec, "spec Prog\n\ntype Error | New\n").unwrap();
        let err = require_stamp_evidence(&spec, &program, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("spec changed since it was verified"), "{err}");
        let _ = std::fs::remove_dir_all(spec.parent().unwrap());
    }
}
