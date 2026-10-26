//! `qedgen verify` attaches a fresh `qedgen build` record to its evidence
//! (#449), with or without `--program`, and warns about a stale one.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_record(dir: &Path, commit: &str) {
    std::fs::create_dir_all(dir.join(".qed/build")).unwrap();
    let record = serde_json::json!({
        "schema_version": 1,
        "commit": commit,
        "library": "x",
        "program_hash": "b".repeat(64),
        "file_sha256": "",
        "solana_verify_version": "0.5.2",
        "platform_tools": "v1.57",
        "arch": "v3",
        "debug_setting": "1",
        "build_argv": [],
        "text_matches": true,
        "recorded_at_unix": 1,
    });
    std::fs::write(dir.join(".qed/build/build.json"), record.to_string()).unwrap();
}

/// A root `[workspace]` with the spec in `programs/x/`, committed.
fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"programs/*\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(d.path().join("programs/x")).unwrap();
    std::fs::write(d.path().join("programs/x/x.qedspec"), "").unwrap();
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn verify(dir: &Path) -> (String, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(["verify", "--spec", "programs/x/x.qedspec", "--probe-repros"])
        .current_dir(dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let evidence = std::fs::read_to_string(dir.join("programs/x/.qed/verify-evidence.json"))
        .unwrap_or_else(|_| panic!("no evidence written:\n{stderr}"));
    (stderr, serde_json::from_str(&evidence).unwrap())
}

#[test]
fn fresh_record_is_attached_without_program() {
    let d = project();
    let head = git(d.path(), &["rev-parse", "HEAD"]);
    write_record(d.path(), &head);
    let (stderr, evidence) = verify(d.path());
    assert_eq!(evidence["binary_build"]["commit"], head, "{stderr}");
    assert_eq!(evidence["binary_build"]["program_hash"], "b".repeat(64));

    // A second verify, with the first run's evidence now on disk, keeps it.
    let (stderr, evidence) = verify(d.path());
    assert_eq!(evidence["binary_build"]["commit"], head, "{stderr}");
}

#[test]
fn stale_record_is_not_attached_and_warns() {
    let d = project();
    write_record(d.path(), &"a".repeat(40));
    let (stderr, evidence) = verify(d.path());
    assert!(evidence.get("binary_build").is_none(), "{evidence}");
    assert!(stderr.contains("run qedgen build again"), "{stderr}");
}
