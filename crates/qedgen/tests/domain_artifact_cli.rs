use std::process::Command;

#[test]
fn codegen_program_probe_emits_domain_artifacts_without_runtime_override() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("generated-vault");
    let audit_dir = root.path().join("audit");
    std::fs::create_dir_all(program.join("src")).unwrap();
    std::fs::write(
        program.join("Cargo.toml"),
        r#"[package]
name = "generated-vault"
version = "0.1.0"
edition = "2021"

[dependencies]
quasar-lang = "0.0.0"
"#,
    )
    .unwrap();
    std::fs::write(
        program.join("src/lib.rs"),
        r#"// #[qed(verified)]
pub fn update_fee(fee_bps: u16) {
    let _ = fee_bps;
}

pub fn deposit(token_amount: u64) {
    transfer(user_vault, program_vault, token_amount);
}

pub fn withdraw(token_amount: u64) {
    transfer(program_vault, user_vault, token_amount);
}
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args([
            "probe",
            "--program",
            program.to_str().unwrap(),
            "--emit-spec-candidates",
            "--audit-dir",
            audit_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["runtime"], "qedgen_codegen");

    let dossier: serde_json::Value =
        serde_json::from_slice(&std::fs::read(audit_dir.join("domain-dossier.json")).unwrap())
            .unwrap();
    assert_eq!(dossier["schema_version"], 1);
    assert_eq!(
        dossier["schema_uri"],
        "https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json"
    );
    assert_eq!(dossier["target"]["runtime"], "qedgen-codegen");
    assert!(dossier["handlers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|handler| handler["name"] == "update_fee"));
    assert!(!dossier["asset_flows"].as_array().unwrap().is_empty());
    assert!(dossier["quantities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|quantity| quantity["symbol"] == "fee_bps"));
    assert!(dossier["paired_operations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|pair| pair["left_operation"] == "deposit" && pair["right_operation"] == "withdraw"));

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(audit_dir.join("run-manifest.json")).unwrap())
            .unwrap();
    assert!(manifest["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|lane| lane["name"] == "ordinary-probe" && lane["status"] == "passed"));
    assert!(audit_dir.join("domain-dossier.md").is_file());
    let domain_interview: serde_json::Value =
        serde_json::from_slice(&std::fs::read(audit_dir.join("domain-interview.json")).unwrap())
            .unwrap();
    assert!(!domain_interview["questions"].as_array().unwrap().is_empty());
}

#[test]
fn unsupported_runtime_preserves_source_dossier_and_blocked_resume_path() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("custom-runtime");
    let audit_dir = root.path().join("audit");
    std::fs::create_dir_all(program.join("src")).unwrap();
    std::fs::write(
        program.join("src/lib.rs"),
        "pub fn deposit(deposit_amount: u64) { transfer(user, vault, deposit_amount); }\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args([
            "probe",
            "--program",
            program.to_str().unwrap(),
            "--emit-spec-candidates",
            "--audit-dir",
            audit_dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dossier: serde_json::Value =
        serde_json::from_slice(&std::fs::read(audit_dir.join("domain-dossier.json")).unwrap())
            .unwrap();
    assert!(!dossier["asset_flows"].as_array().unwrap().is_empty());
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(audit_dir.join("run-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["status"], "tooling-blocked");
    let lane = manifest["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["name"] == "ordinary-probe")
        .unwrap();
    assert_eq!(lane["status"], "blocked");
    assert!(lane["resume_command"]
        .as_str()
        .unwrap()
        .contains("--runtime native"));
}
