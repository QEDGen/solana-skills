//! `qedgen verify --check-deployed` (#449) through the fake-bytes seam of
//! the `solana program dump` fetcher. A mismatch is a P2: reported, exit 0.

use std::path::Path;
use std::process::Command;

const FAKE_BYTES_ENV: &str = "QEDGEN_UPSTREAM_FAKE_BYTES";

fn program_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    format!("{:x}", Sha256::digest(&bytes[..end]))
}

/// A program crate `p` with its spec, and a build record for library `p`.
fn fixture(dir: &Path, recorded_bytes: &[u8]) -> std::path::PathBuf {
    let spec = dir.join("program.qedspec");
    std::fs::write(&spec, "").unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
    std::fs::create_dir_all(dir.join(".qed/build/p")).unwrap();
    let record = serde_json::json!({
        "schema_version": 1,
        "commit": "a".repeat(40),
        "library": "p",
        "program_hash": program_hash(recorded_bytes),
        "file_sha256": "",
        "solana_verify_version": "0.5.2",
        "platform_tools": "v1.57",
        "arch": "v3",
        "debug_setting": "1",
        "build_argv": [],
        "text_matches": true,
        "recorded_at_unix": 1,
    });
    std::fs::write(dir.join(".qed/build/p/build.json"), record.to_string()).unwrap();
    spec
}

fn verify(spec: &Path, on_chain: &str, extra: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(["verify", "--spec"])
        .arg(spec)
        .args([
            "--check-deployed",
            "Prog1111111111111111111111111111111111111",
        ])
        .args(extra)
        .env(FAKE_BYTES_ENV, on_chain)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn mismatch_is_reported_as_p2_and_exits_zero() {
    let d = tempfile::tempdir().unwrap();
    let spec = fixture(d.path(), b"verified-bytes");
    let (ok, stderr) = verify(&spec, "other-bytes", &[]);
    assert!(ok, "{stderr}");
    assert!(stderr.contains("P2: deployed program"), "{stderr}");
}

#[test]
fn match_is_reported() {
    let d = tempfile::tempdir().unwrap();
    let spec = fixture(d.path(), b"verified-bytes");
    let (ok, stderr) = verify(&spec, "verified-bytes", &[]);
    assert!(ok, "{stderr}");
    assert!(stderr.contains("matches the verified build"), "{stderr}");
}

#[test]
fn offline_refuses_the_fetch() {
    let d = tempfile::tempdir().unwrap();
    let spec = fixture(d.path(), b"verified-bytes");
    let (ok, stderr) = verify(&spec, "", &["--offline"]);
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("offline mode"), "{stderr}");
}

#[test]
fn runs_alongside_check_upstream() {
    use sha2::{Digest, Sha256};
    let d = tempfile::tempdir().unwrap();
    let spec = fixture(d.path(), b"verified-bytes");
    let pin = format!("sha256:{:x}", Sha256::digest(b"verified-bytes"));
    std::fs::write(
        d.path().join("qed.lock"),
        format!(
            "version = 1\n\n[[dependency]]\nname = \"spl\"\nsource = \"builtin:spl\"\n\
             spec_hash = \"sha256:0\"\nprogram_id = \"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA\"\n\
             upstream_binary_hash = \"{pin}\"\nupstream_version = \"4.0.3\"\n"
        ),
    )
    .unwrap();
    let (ok, stderr) = verify(&spec, "verified-bytes", &["--check-upstream"]);
    assert!(ok, "{stderr}");
    assert!(stderr.contains("matches the verified build"), "{stderr}");
}

/// `--check-deployed` must not swallow other requested work: with
/// `--crucible` the fuzz stage still runs (here it fails on the empty spec).
#[test]
fn crucible_still_runs_after_the_deploy_check() {
    let d = tempfile::tempdir().unwrap();
    let spec = fixture(d.path(), b"verified-bytes");
    let (ok, stderr) = verify(&spec, "verified-bytes", &["--crucible", "1"]);
    assert!(stderr.contains("matches the verified build"), "{stderr}");
    assert!(!ok, "crucible was skipped:\n{stderr}");
    assert!(stderr.contains("parse error"), "{stderr}");
}

/// A record for another library in the same workspace is not this
/// program's build.
#[test]
fn record_for_another_library_is_not_used() {
    let d = tempfile::tempdir().unwrap();
    let spec = fixture(d.path(), b"verified-bytes");
    std::fs::rename(
        d.path().join(".qed/build/p"),
        d.path().join(".qed/build/other"),
    )
    .unwrap();
    let (ok, stderr) = verify(&spec, "verified-bytes", &[]);
    assert!(ok, "{stderr}");
    assert!(stderr.contains("no qedgen build record for p"), "{stderr}");
}
