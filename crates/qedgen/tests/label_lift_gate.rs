//! `qedgen lift` end to end on the label-vault fixture (#450). Ignored by default: it
//! needs Docker, `solana-verify` 0.5.2+, elan, and a clean qedsvm checkout at the pinned
//! revision (`QEDSVM_LIFT_REV`) in `QEDSVM_DIR`. A cold Lean build of qedsvm takes about 30 minutes.
//!
//! ```bash
//! QEDSVM_DIR=../qedsvm cargo test -p qedgen-solana-skills --test label_lift_gate -- --ignored --nocapture
//! ```
//!
//! One test does both checks, because the Docker build is the slow part: a fresh
//! `qedgen build` of the fixture source must give the committed pair byte for byte, and
//! the lift of that build must pass.

use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

const FIXTURE: &str = "tests/fixtures/pinocchio-fixtures/label-vault";
/// The committed pair (see the fixture README and qedsvm `qedsvm-rs/tests/fixtures/`).
const SO_SHA256: &str = "914f0f0e58ec5c19b7eaba9a0f7d9eb978f5053fcd2383db91985c63368dfc8d";
const DEBUG_SHA256: &str = "9bf8e24b541c99650017ec3546cfdc55221dc0617b2f3ff7616f35cb5525f4d3";

fn run(dir: &Path, program: &str, args: &[&str]) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{program} {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn committed_copy() -> tempfile::TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    for f in ["Cargo.toml", "Cargo.lock", ".gitignore", "src/lib.rs"] {
        std::fs::copy(src.join(f), d.path().join(f)).unwrap();
    }
    run(d.path(), "git", &["init", "-q"]);
    run(d.path(), "git", &["add", "-A"]);
    run(
        d.path(),
        "git",
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    d
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

#[test]
#[ignore = "needs Docker, solana-verify, elan and QEDSVM_DIR; tens of minutes"]
fn s3_label_vault_lift_end_to_end() {
    let qedsvm = std::env::var("QEDSVM_DIR").expect("set QEDSVM_DIR to a qedsvm checkout");
    let repo = committed_copy();
    // No `--qedsvm-dev`: the checkout must be clean and at the pinned revision.
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(["lift", "--json", "--qedsvm", &qedsvm, "--program"])
        .arg(repo.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "qedgen lift failed:\n{stderr}\n{stdout}"
    );

    let build = repo.path().join(".qed/build/label_vault");
    assert_eq!(
        sha256(&build.join("label_vault.so")),
        SO_SHA256,
        "deploy .so drifted"
    );
    assert_eq!(
        sha256(&build.join("label_vault.debug.so")),
        DEBUG_SHA256,
        "debug .so drifted"
    );

    // With --json, stdout is the report and nothing else.
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON report ({e}):\n{stdout}"));
    assert_eq!(report["passed"], true);
    assert_eq!(report["coverage"]["instructions"], 3);
    assert_eq!(report["coverage"]["proven_edges"], 1);
    assert_eq!(report["axioms"]["disallowed"], serde_json::json!([]));
    let gaps = report["gaps"].to_string();
    assert!(gaps.contains("native_decide"), "{gaps}");
    let authority = serde_json::json!(["account 1 key == account 0 data[8..40]"]);
    let initialize = &report["instructions"][0];
    assert_eq!(initialize["discriminator"], 0);
    assert_eq!(initialize["signers"], serde_json::json!([1]));
    assert_eq!(initialize["key_compares"], serde_json::json!([]));
    for (i, disc) in [(1, 1), (2, 2)] {
        let ix = &report["instructions"][i];
        assert_eq!(ix["discriminator"], disc);
        assert_eq!(ix["signers"], serde_json::json!([1]), "{ix}");
        assert_eq!(ix["key_compares"], authority, "{ix}");
    }
    let facts = report["coverage"]["route_facts"].as_array().unwrap();
    assert_eq!(facts.len(), 3, "{facts:?}");
    let cov = &report["coverage"];
    assert_eq!(cov["routes"], 3);
    assert_eq!(cov["guards"], 49);
    assert_eq!(cov["pinned_guards"], 47);
    assert_eq!(cov["refused_labels"], 0);
    assert_eq!(cov["unsupported_edges"], 0);
    assert!(!repo.path().join(".qed/verify-evidence.json").exists());
}
