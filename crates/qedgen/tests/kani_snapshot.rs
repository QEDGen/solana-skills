//! Phase 3f Kani-MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 3f".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo, percolator}`),
//! regenerates the MIR-rendered `tests/kani.rs` and compares against
//! a checked-in snapshot at
//! `crates/qedgen/tests/snapshots/<fixture>.kani.rs`.
//!
//! When the snapshot diverges, the test prints the unified diff and
//! fails — silent drift between MIR + legacy Kani renderers is
//! detectable immediately. Refreshing a snapshot is intentional and
//! requires re-running the fixture binary + committing the updated
//! file (`UPDATE_SNAPSHOTS=1 cargo test --test kani_snapshot` writes
//! them in place).
//!
//! These snapshots were byte-equivalent to the legacy `kani::generate`
//! output (verified before that renderer was deleted in v2.32). The
//! snapshot lock-in is against the MIR output, so a failing snapshot
//! signals "MIR Kani codegen changed".
//!
//! Parallel structure with `tests/mir_snapshot.rs` (Lean side): same
//! per-fixture harness shape, same `UPDATE_SNAPSHOTS=1` workflow.

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
/// --kani`, and return the rendered `tests/kani.rs` string.
///
/// The fixture is copied (rather than codegen-into-place) because
/// `qedgen codegen --kani` rewrites `programs/` too; the copy
/// isolates the workspace from those rewrites.
///
/// MIR (`kani_mir`) is the sole Kani-codegen path — the legacy
/// `kani` renderer and its `QEDGEN_LEGACY_KANI` escape hatch were
/// deleted in v2.32.
fn render_mir_kani(fixture_dir: &str, spec_arg: &str) -> String {
    ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = repo_root().join(fixture_dir);

    // rsync mirrors the fixture into the tempdir minus build/anchor
    // junk. fall back to shelling out — the std::fs `copy_dir_all`
    // pattern would also work but rsync is what the manual sweep
    // uses, so the snapshot path matches verbatim.
    let rsync = Command::new("rsync")
        .args([
            "-aq",
            "--exclude=.anchor",
            "--exclude=target",
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
        .arg("--kani")
        .current_dir(tmp.path())
        .status()
        .expect("spawn qedgen codegen");
    assert!(
        status.success(),
        "qedgen codegen failed for {}",
        fixture_dir
    );

    let out = tmp.path().join("programs").join("tests").join("kani.rs");
    fs::read_to_string(&out).unwrap_or_else(|e| panic!("read {}: {e}", out.display()))
}

fn assert_or_update_snapshot(fixture: &str, fixture_dir: &str, spec_arg: &str) {
    let rendered = render_mir_kani(fixture_dir, spec_arg);
    let snapshot_path = snapshots_dir().join(format!("{}.kani.rs", fixture));
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
            "{fixture}: MIR Kani snapshot drift detected.\n\
             Snapshot: {}\n\
             Re-run with UPDATE_SNAPSHOTS=1 to refresh (then inspect the diff before \
             committing).\n\
             {diff}",
            snapshot_path.display()
        );
    }
}

/// Produce a unified-diff string between two multiline texts.
/// Same shape as `tests/mir_snapshot.rs::diff_unified`.
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
//
// Each test is small + boilerplate-light so failures point at one
// fixture. Adding a new pilot fixture: drop a new test with the same
// shape + run `UPDATE_SNAPSHOTS=1 cargo test --test kani_snapshot
// <new_fixture_name>` once to seed.
//
// `cross-program-vault` is omitted from this set (the spec exists
// but has no kani.rs reference output today; the mir_snapshot test
// covers it for Lean).

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

#[test]
fn snapshot_kani_cpi_account_bindings() {
    assert_or_update_snapshot(
        "kani-cpi-account-bindings",
        "crates/qedgen/tests/fixtures/kani-cpi-account-bindings",
        "config.qedspec",
    );
}
