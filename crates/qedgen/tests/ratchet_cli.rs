//! CLI gates for source-aware `readiness` / `check-upgrade` (#398).

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/probe-corpus/specless/anchor-idl")
}

fn qedgen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(args)
        .output()
        .expect("spawn qedgen")
}

#[test]
fn readiness_root_json_reports_source_only_handler() {
    let root = fixture_root();
    let idl = root.join("target/idl/vault.json");
    let out = qedgen(&[
        "readiness",
        "--idl",
        idl.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "source-only handler must make readiness unsafe:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("readiness JSON report");
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                finding["rule_id"] == "QED001"
                    && finding["path"] == serde_json::json!(["ix:emergency_withdraw"])
            }),
        "missing source-only finding: {report:#}"
    );
}

#[test]
fn check_upgrade_root_applies_to_candidate_side() {
    let root = fixture_root();
    let idl = root.join("target/idl/vault.json");
    let out = qedgen(&[
        "check-upgrade",
        "--old",
        idl.to_str().unwrap(),
        "--new",
        idl.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(out.status.code(), Some(2));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("check-upgrade JSON report");
    assert!(report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["rule_id"] == "QED001"));
}

#[test]
fn readiness_invalid_root_is_a_qedgen_configuration_error() {
    let root = fixture_root();
    let idl = root.join("target/idl/vault.json");
    let missing = root.join("does-not-exist");
    let out = qedgen(&[
        "readiness",
        "--idl",
        idl.to_str().unwrap(),
        "--root",
        missing.to_str().unwrap(),
    ]);

    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("project root does not exist"),
        "expected contextual root error, got:\n{stderr}"
    );
}

#[test]
fn readiness_source_only_handler_can_be_explicitly_acknowledged() {
    let root = fixture_root();
    let idl = root.join("target/idl/vault.json");
    let out = qedgen(&[
        "readiness",
        "--idl",
        idl.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
        "--unsafe",
        "allow-no-signer",
        "--unsafe",
        "allow-source-only-emergency_withdraw",
        "--json",
    ]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "all acknowledged findings should be additive:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("readiness JSON report");
    assert!(report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|finding| finding["severity"] == "additive"));
}
