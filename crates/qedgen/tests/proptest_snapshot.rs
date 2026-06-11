//! Phase 5 proptest-MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 5".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo, percolator}`),
//! regenerates the MIR-rendered `tests/proptest.rs` and compares
//! against a checked-in snapshot at
//! `crates/qedgen/tests/snapshots/<fixture>.proptest.rs`.
//!
//! The Phase 5 MIR scaffold delegates the full emit to legacy
//! `proptest_gen::generate`; the snapshot therefore locks the
//! legacy output as the MIR-default reference. Any unintended
//! drift between routes (e.g. future legacy edits that don't
//! flow through the MIR scaffold) fails the gate immediately.
//! `QEDGEN_LEGACY_PROPTEST` is explicitly cleared so a parent
//! shell can't accidentally force the snapshot tests onto the
//! legacy path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qedgen crate at <repo>/crates/qedgen")
        .to_path_buf()
}

fn qedgen_bin() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    repo_root().join("target").join(profile).join("qedgen")
}

fn ensure_qedgen_built() {
    if !qedgen_bin().exists() {
        let status = Command::new("cargo")
            .args(["build", "--bin", "qedgen"])
            .current_dir(repo_root())
            .status()
            .expect("spawn cargo build");
        assert!(status.success(), "cargo build qedgen failed");
    }
}

fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
}

/// Copy a fixture into a tempdir, run `qedgen codegen --spec <spec>
/// --proptest`, and return the rendered `tests/proptest.rs` string.
fn render_mir_proptest(fixture_dir: &str, spec_arg: &str) -> String {
    ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = repo_root().join(fixture_dir);

    let rsync = Command::new("rsync")
        .args([
            "-aq",
            "--exclude=.anchor",
            "--exclude=target",
            "--exclude=.lake",
            "--exclude=node_modules",
        ])
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", tmp.path().display()))
        .status()
        .expect("spawn rsync");
    assert!(rsync.success(), "rsync failed for fixture {}", fixture_dir);

    let git_init = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(tmp.path())
        .status()
        .expect("spawn git init");
    assert!(
        git_init.success(),
        "git init failed in {}",
        tmp.path().display()
    );

    let status = Command::new(qedgen_bin())
        .arg("codegen")
        .arg("--spec")
        .arg(spec_arg)
        .arg("--proptest")
        .env_remove("QEDGEN_LEGACY_PROPTEST")
        .current_dir(tmp.path())
        .status()
        .expect("spawn qedgen codegen");
    assert!(
        status.success(),
        "qedgen codegen failed for {}",
        fixture_dir
    );

    let out = tmp
        .path()
        .join("programs")
        .join("tests")
        .join("proptest.rs");
    fs::read_to_string(&out).unwrap_or_else(|e| panic!("read {}: {e}", out.display()))
}

fn assert_or_update_snapshot(fixture: &str, fixture_dir: &str, spec_arg: &str) {
    let rendered = render_mir_proptest(fixture_dir, spec_arg);
    let snapshot_path = snapshots_dir().join(format!("{}.proptest.rs", fixture));
    let update = std::env::var("UPDATE_SNAPSHOTS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if update {
        fs::create_dir_all(snapshots_dir()).expect("create snapshots dir");
        fs::write(&snapshot_path, &rendered)
            .unwrap_or_else(|e| panic!("write {}: {e}", snapshot_path.display()));
        eprintln!("UPDATE_SNAPSHOTS=1: wrote {}", snapshot_path.display());
        return;
    }

    let expected = fs::read_to_string(&snapshot_path).unwrap_or_else(|e| {
        panic!(
            "missing snapshot {}: {e}\n\
             Run with UPDATE_SNAPSHOTS=1 to seed it.",
            snapshot_path.display()
        )
    });

    if expected != rendered {
        let diff = diff_unified(&expected, &rendered);
        panic!(
            "{fixture}: MIR proptest snapshot drift detected.\n\
             Snapshot: {}\n\
             Re-run with UPDATE_SNAPSHOTS=1 to refresh (then inspect the diff before \
             committing).\n\
             {diff}",
            snapshot_path.display()
        );
    }
}

fn diff_unified(expected: &str, actual: &str) -> String {
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    out.push_str("--- snapshot\n+++ rendered\n");
    let max = exp_lines.len().max(act_lines.len());
    let mut printed = 0usize;
    let max_lines = 80usize;
    for i in 0..max {
        let e = exp_lines.get(i).copied().unwrap_or("");
        let a = act_lines.get(i).copied().unwrap_or("");
        if e != a {
            if printed >= max_lines {
                out.push_str("... (diff truncated)\n");
                break;
            }
            out.push_str(&format!("@@ line {} @@\n", i + 1));
            if !e.is_empty() || i < exp_lines.len() {
                out.push_str(&format!("-{}\n", e));
            }
            if !a.is_empty() || i < act_lines.len() {
                out.push_str(&format!("+{}\n", a));
            }
            printed += 1;
        }
    }
    out
}

// ---- Per-fixture snapshot tests ----

#[test]
fn snapshot_escrow() {
    assert_or_update_snapshot("escrow", "examples/rust/escrow", "escrow.qedspec");
}

#[test]
fn snapshot_lending() {
    assert_or_update_snapshot("lending", "examples/rust/lending", "lending.qedspec");
}

#[test]
fn snapshot_multisig() {
    assert_or_update_snapshot("multisig", "examples/rust/multisig", "multisig.qedspec");
}

#[test]
fn snapshot_bundled_stdlib_demo() {
    assert_or_update_snapshot(
        "bundled-stdlib-demo",
        "examples/rust/bundled-stdlib-demo",
        "pool.qedspec",
    );
}

#[test]
fn snapshot_escrow_split() {
    assert_or_update_snapshot("escrow-split", "examples/rust/escrow-split", ".");
}

#[test]
fn snapshot_percolator() {
    assert_or_update_snapshot(
        "percolator",
        "examples/rust/percolator",
        "percolator.qedspec",
    );
}
