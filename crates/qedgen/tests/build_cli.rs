//! `qedgen build` refusals (#449). Each case must stop before Docker or
//! `solana-verify` is touched, so these run without either.

use std::path::Path;
use std::process::{Command, Output};

fn qedgen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(args)
        .output()
        .expect("qedgen binary runs")
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn committed_crate(cargo_toml: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Cargo.toml"), cargo_toml).unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), "").unwrap();
    git(d.path(), &["init", "-q"]);
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "init"]);
    d
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn refuses_without_release_debug() {
    let d = committed_crate("[package]\nname = \"x\"\nversion = \"0.1.0\"\n");
    let out = qedgen(&["build", "--program", d.path().to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("[profile.release]"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn refuses_dirty_tree() {
    let d = committed_crate(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[profile.release]\ndebug = 1\n",
    );
    std::fs::write(d.path().join("src/new.rs"), "fn x() {}\n").unwrap();
    let out = qedgen(&["build", "--program", d.path().to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("has uncommitted changes"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn refuses_outside_git() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\n[profile.release]\ndebug = 1\n",
    )
    .unwrap();
    let out = qedgen(&["build", "--program", d.path().to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("is not in a git repository"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn rejects_unknown_arch() {
    let d = committed_crate(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[profile.release]\ndebug = 1\n",
    );
    let out = qedgen(&[
        "build",
        "--program",
        d.path().to_str().unwrap(),
        "--arch",
        "v9",
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("v9"), "{}", stderr(&out));
}

#[test]
fn refuses_without_cargo_lock() {
    let d = committed_crate(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[profile.release]\ndebug = 1\n",
    );
    let out = qedgen(&["build", "--program", d.path().to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("has no Cargo.lock; solana-verify builds with --locked"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn refuses_when_no_build_image_can_be_picked() {
    let d = committed_crate(
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[profile.release]\ndebug = 1\n",
    );
    std::fs::write(d.path().join("Cargo.lock"), "version = 4\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-q", "-m", "lock"]);
    let out = qedgen(&["build", "--program", d.path().to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("solana-verify cannot tell which Solana build image to use"),
        "{}",
        stderr(&out)
    );
}
