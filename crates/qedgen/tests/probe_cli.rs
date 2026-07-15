//! CLI-surface tests for `qedgen probe` mode combinations (#225 Phase 0).
//!
//! Two invariants pinned here:
//! 1. Invalid / ambiguous flag combinations fail loudly through clap —
//!    no engine is ever silently skipped (`--program` used to win over
//!    `--fuzz` by dispatch order, dropping the requested fuzz run).
//! 2. Every probe envelope carries the same canonical schema version,
//!    whichever engine produced it (fuzz-mode outputs shipped a
//!    hardcoded `version: 1` against the v2 schema).

use std::path::PathBuf;
use std::process::{Command, Output};

fn qedgen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(args)
        .output()
        .expect("spawn qedgen")
}

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// `--program` + `--fuzz` used to silently skip the fuzz engine
/// (`--program` dispatches first and returns). Now a clap conflict.
#[test]
fn probe_program_plus_fuzz_is_rejected() {
    let out = qedgen(&["probe", "--program", "some/dir", "--fuzz", "60"]);
    assert!(!out.status.success(), "conflicting flags must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--fuzz") && stderr.contains("cannot be used with"),
        "expected clap conflict naming --fuzz, got:\n{stderr}"
    );
}

/// `--program` ignores `--root`; reject the pair instead of dropping it.
#[test]
fn probe_program_plus_root_is_rejected() {
    let out = qedgen(&["probe", "--program", "some/dir", "--root", "other/dir"]);
    assert!(!out.status.success(), "conflicting flags must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected clap conflict, got:\n{stderr}"
    );
}

#[test]
fn probe_program_plus_spec_is_rejected() {
    let out = qedgen(&["probe", "--program", "some/dir", "--spec", "x.qedspec"]);
    assert!(!out.status.success(), "conflicting flags must fail");
}

/// `--fuzz` without a target has a dedicated (non-clap) error that names
/// both valid pairings.
#[test]
fn probe_fuzz_without_spec_or_root_names_both_options() {
    let out = qedgen(&["probe", "--fuzz", "60"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--spec") && stderr.contains("--root"),
        "error must name both valid pairings, got:\n{stderr}"
    );
}

#[test]
fn probe_bootstrap_requires_root() {
    let out = qedgen(&["probe", "--bootstrap"]);
    assert!(!out.status.success());
}

/// Acceptance criterion (#225): fuzz and non-fuzz outputs use the same
/// canonical schema version. Budget-0 exercises the fuzz-mode envelope
/// without paying the Crucible build cost.
#[test]
fn fuzz_and_spec_probe_agree_on_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = tmp.path().join("counter.qedspec");
    std::fs::copy(fixture("descriptor/counter.qedspec"), &spec).unwrap();
    let spec_str = spec.to_str().unwrap();

    let spec_out = qedgen(&["probe", "--spec", spec_str]);
    assert!(
        spec_out.status.success(),
        "spec-aware probe failed:\n{}",
        String::from_utf8_lossy(&spec_out.stderr)
    );
    let fuzz_out = qedgen(&["probe", "--fuzz", "0", "--spec", spec_str]);
    assert!(
        fuzz_out.status.success(),
        "budget-0 fuzz probe failed:\n{}",
        String::from_utf8_lossy(&fuzz_out.stderr)
    );

    let spec_json: serde_json::Value = serde_json::from_slice(&spec_out.stdout).unwrap();
    let fuzz_json: serde_json::Value = serde_json::from_slice(&fuzz_out.stdout).unwrap();
    assert_eq!(
        spec_json["version"], fuzz_json["version"],
        "fuzz-mode envelope drifted from the canonical probe schema version"
    );
    // Pin the canonical value so both paths can't drift in lockstep by
    // accident; bump alongside probe::SCHEMA_VERSION on a conscious change.
    assert_eq!(spec_json["version"], serde_json::json!(2));
}
