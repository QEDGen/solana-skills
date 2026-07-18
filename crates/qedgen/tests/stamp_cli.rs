//! `qedgen stamp` — the post-verification attribute emitter and its
//! evidence gate (spec-elicitation PRD §5.1). The gate contract: no
//! attributes are computed, let alone emitted, without recorded
//! implementation-verified evidence whose spec hash matches the spec
//! being stamped.

use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Command, Output};

fn qedgen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(args)
        .output()
        .expect("qedgen binary runs")
}

fn write_evidence(
    dir: &Path,
    spec_hash: &str,
    implementation_verified: bool,
    program: Option<&Path>,
) {
    let qed = dir.join(".qed");
    std::fs::create_dir_all(&qed).unwrap();
    let backends = if implementation_verified {
        serde_json::json!([{ "name": "miri", "status": "passed", "implementation_bound": true }])
    } else {
        serde_json::json!([{ "name": "proptest", "status": "passed", "implementation_bound": false }])
    };
    std::fs::write(
        qed.join("verify-evidence.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 2,
            "spec": "demo.qedspec",
            "spec_hash": spec_hash,
            "program": program.map(|p| p.display().to_string()),
            "program_hash": program.map(test_program_source_hash),
            "recorded_at_unix": 1_700_000_000u64,
            "backends": backends,
            "implementation_verified": implementation_verified,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn test_program_source_hash(program: &Path) -> String {
    let root = program.canonicalize().unwrap();
    let mut files = vec![root.join("Cargo.toml"), root.join("src/lib.rs")];
    files.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"qedgen-program-source-v1\n");
    for path in files {
        let rel = path.strip_prefix(&root).unwrap();
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(std::fs::read(path).unwrap());
        hasher.update(b"\0");
    }
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_string()
}

const SPEC: &str = "spec Demo\n\ntype State\n  | Active\n\ntype Error\n  | Unauthorized\n\nhandler set_fee (fee : U64) {\n  requires fee <= 100 else Unauthorized\n}\n";

fn write_anchor_project(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nanchor-lang = \"0.31\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        r#"use anchor_lang::prelude::*;

declare_id!("Demo11111111111111111111111111111111111111");

#[program]
pub mod demo {
    use super::*;
    pub fn set_fee(ctx: Context<SetFee>, fee: u64) -> Result<()> {
        ctx.accounts.state.fee = fee;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct SetFee<'info> {
    #[account(mut)]
    pub state: Account<'info, StateAccount>,
}

#[account]
pub struct StateAccount {
    pub fee: u64,
}
"#,
    )
    .unwrap();
}

/// No evidence file → refusal that names `qedgen verify` as the remedy,
/// before any program parsing happens (bogus program path on purpose).
#[test]
fn stamp_refuses_without_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("demo.qedspec");
    std::fs::write(&spec, SPEC).unwrap();
    let out = qedgen(&[
        "stamp",
        "--program",
        "/nonexistent/prog",
        "--spec",
        spec.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no verification evidence"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("qedgen verify"), "stderr: {stderr}");
}

/// Evidence recorded for a different spec content → hash-mismatch
/// refusal (an edited spec invalidates the evidence).
#[test]
fn stamp_refuses_on_spec_hash_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("demo.qedspec");
    std::fs::write(&spec, SPEC).unwrap();
    write_evidence(dir.path(), "0000000000000000", true, None);
    let out = qedgen(&[
        "stamp",
        "--program",
        "/nonexistent/prog",
        "--spec",
        spec.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("spec changed since it was verified"),
        "stderr: {stderr}"
    );
}

/// Model-tested-only evidence is not eligible for `#[qed(verified)]`.
#[test]
fn stamp_refuses_model_only_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("demo.qedspec");
    std::fs::write(&spec, SPEC).unwrap();
    write_evidence(
        dir.path(),
        &qedgen_hash_core::sha256_hex16(SPEC),
        false,
        None,
    );
    let out = qedgen(&[
        "stamp",
        "--program",
        "/nonexistent/prog",
        "--spec",
        spec.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not eligible"), "stderr: {stderr}");
}

/// Matching implementation-verified evidence opens the gate and emits
/// the same `#[qed(verified, …)]` attributes the old `adapt --spec` did.
#[test]
fn stamp_emits_attributes_with_matching_impl_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("demo.qedspec");
    std::fs::write(&spec, SPEC).unwrap();
    let prog = dir.path().join("prog");
    write_anchor_project(&prog);
    write_evidence(
        dir.path(),
        &qedgen_hash_core::sha256_hex16(SPEC),
        true,
        Some(&prog),
    );
    let out = qedgen(&[
        "stamp",
        "--program",
        prog.to_str().unwrap(),
        "--spec",
        spec.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("#[qed(verified"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("handler = \"set_fee\"") || stdout.contains("handler=\"set_fee\""),
        "stdout: {stdout}"
    );
    assert!(
        stderr.contains("implementation-verified by miri"),
        "stderr: {stderr}"
    );
}

/// Matching spec evidence cannot be reused after the implementation changes.
#[test]
fn stamp_refuses_program_changed_after_verify() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("demo.qedspec");
    std::fs::write(&spec, SPEC).unwrap();
    let prog = dir.path().join("prog");
    write_anchor_project(&prog);
    write_evidence(
        dir.path(),
        &qedgen_hash_core::sha256_hex16(SPEC),
        true,
        Some(&prog),
    );
    let source = prog.join("src/lib.rs");
    let changed = std::fs::read_to_string(&source).unwrap().replace(
        "ctx.accounts.state.fee = fee;",
        "ctx.accounts.state.fee = fee + 1;",
    );
    std::fs::write(source, changed).unwrap();

    let out = qedgen(&[
        "stamp",
        "--program",
        prog.to_str().unwrap(),
        "--spec",
        spec.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("implementation changed since it was verified"),
        "stderr: {stderr}"
    );
}

/// The deprecated `adapt --spec` alias still works but warns toward
/// `stamp` (v3.0 soft-deprecation pattern).
#[test]
fn adapt_attribute_mode_warns_deprecated() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("demo.qedspec");
    std::fs::write(&spec, SPEC).unwrap();
    let prog = dir.path().join("prog");
    write_anchor_project(&prog);
    let out = qedgen(&[
        "adapt",
        "--program",
        prog.to_str().unwrap(),
        "--spec",
        spec.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("deprecated"), "stderr: {stderr}");
    assert!(stderr.contains("qedgen stamp"), "stderr: {stderr}");
}
