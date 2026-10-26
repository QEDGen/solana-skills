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

fn write_record(dir: &Path, library: &str, commit: &str) {
    std::fs::create_dir_all(dir.join(".qed/build").join(library)).unwrap();
    let record = serde_json::json!({
        "schema_version": 1,
        "commit": commit,
        "library": library,
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
    std::fs::write(
        dir.join(".qed/build").join(library).join("build.json"),
        record.to_string(),
    )
    .unwrap();
}

/// A root `[workspace]` with programs `x` and `y`, each with its spec in
/// its crate, committed.
fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"programs/*\"]\n",
    )
    .unwrap();
    for name in ["x", "y"] {
        let crate_dir = d.path().join("programs").join(name);
        std::fs::create_dir_all(&crate_dir).unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .unwrap();
        std::fs::write(crate_dir.join(format!("{name}.qedspec")), "").unwrap();
    }
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn verify(dir: &Path, program: &str) -> (String, serde_json::Value) {
    let spec = format!("programs/{program}/{program}.qedspec");
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(["verify", "--spec", &spec, "--probe-repros"])
        .current_dir(dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let evidence = std::fs::read_to_string(
        dir.join("programs")
            .join(program)
            .join(".qed/verify-evidence.json"),
    )
    .unwrap_or_else(|_| panic!("no evidence written:\n{stderr}"));
    (stderr, serde_json::from_str(&evidence).unwrap())
}

#[test]
fn fresh_record_is_attached_without_program() {
    let d = project();
    let head = git(d.path(), &["rev-parse", "HEAD"]);
    write_record(d.path(), "x", &head);
    let (stderr, evidence) = verify(d.path(), "x");
    assert_eq!(evidence["binary_build"]["commit"], head, "{stderr}");
    assert_eq!(evidence["binary_build"]["program_hash"], "b".repeat(64));

    // A second verify, with the first run's evidence now on disk, keeps it.
    let (stderr, evidence) = verify(d.path(), "x");
    assert_eq!(evidence["binary_build"]["commit"], head, "{stderr}");
}

/// Building `x` and then verifying `y` in the same workspace must not put
/// `x`'s binary into `y`'s evidence.
#[test]
fn another_programs_build_is_not_attached() {
    let d = project();
    let head = git(d.path(), &["rev-parse", "HEAD"]);
    write_record(d.path(), "x", &head);
    let (stderr, evidence) = verify(d.path(), "y");
    assert!(
        evidence.get("binary_build").is_none(),
        "{evidence}\n{stderr}"
    );
}

#[test]
fn stale_record_is_not_attached_and_warns() {
    let d = project();
    write_record(d.path(), "x", &"a".repeat(40));
    let (stderr, evidence) = verify(d.path(), "x");
    assert!(evidence.get("binary_build").is_none(), "{evidence}");
    assert!(stderr.contains("run qedgen build again"), "{stderr}");
}
