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

#[test]
fn readiness_empty_idl_still_reports_every_source_handler() {
    let root = fixture_root();
    let temp = tempfile::tempdir().expect("tempdir");
    let idl = temp.path().join("vault.json");
    std::fs::write(
        &idl,
        r#"{
            "address": "Vault1111111111111111111111111111111111111",
            "metadata": {"name": "vault", "version": "0.1.0", "spec": "0.1.0"},
            "instructions": [],
            "accounts": [],
            "types": []
        }"#,
    )
    .expect("write empty IDL");

    let out = qedgen(&[
        "readiness",
        "--idl",
        idl.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(out.status.code(), Some(2));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("readiness JSON report");
    let source_only: Vec<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["rule_id"] == "QED001")
        .filter_map(|finding| finding["path"][0].as_str())
        .collect();
    assert_eq!(
        source_only,
        ["ix:crank", "ix:emergency_withdraw", "ix:initialize"]
    );
}

#[test]
fn readiness_workspace_scopes_source_handlers_to_selected_idl() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    std::fs::write(
        root.join("Anchor.toml"),
        "[workspace]\nmembers = [\"programs/*\"]\n",
    )
    .expect("write Anchor.toml");
    for program in ["alpha", "beta"] {
        let crate_root = root.join("programs").join(program);
        std::fs::create_dir_all(crate_root.join("src")).expect("create program source dir");
        std::fs::write(
            crate_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{program}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
                 [lib]\nname = \"{program}\"\n[dependencies]\nanchor-lang = \"0.30\"\n"
            ),
        )
        .expect("write program manifest");
        std::fs::write(
            crate_root.join("src/lib.rs"),
            format!(
                "use anchor_lang::prelude::*;\n\
                 #[program]\npub mod {program} {{\nuse super::*;\n\
                 pub fn {program}_handler(_ctx: Context<Empty>) -> Result<()> {{ Ok(()) }}\n}}\n\
                 #[derive(Accounts)]\npub struct Empty {{}}\n"
            ),
        )
        .expect("write program source");
    }
    let idl_dir = root.join("target/idl");
    std::fs::create_dir_all(&idl_dir).expect("create IDL dir");
    let idl = idl_dir.join("alpha.json");
    std::fs::write(
        &idl,
        r#"{
            "address": "Alpha1111111111111111111111111111111111111",
            "metadata": {"name": "alpha", "version": "0.1.0", "spec": "0.1.0"},
            "instructions": [{
                "name": "alphaHandler",
                "discriminator": [1,2,3,4,5,6,7,8],
                "accounts": [],
                "args": []
            }],
            "accounts": [],
            "types": []
        }"#,
    )
    .expect("write alpha IDL");

    let out = qedgen(&[
        "readiness",
        "--idl",
        idl.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
        "--json",
    ]);
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("readiness JSON report");
    assert!(
        !report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                finding["rule_id"] == "QED001"
                    && finding["path"] == serde_json::json!(["ix:beta_handler"])
            }),
        "sibling program handler was compared to alpha IDL: {report:#}"
    );
}
