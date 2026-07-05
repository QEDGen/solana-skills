//! Phase 4i codegen-MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 4i".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo, percolator, cross-program-vault}`),
//! regenerates the MIR-rendered `programs/` tree, concatenates every
//! file into a single text dump with file-path markers, and compares
//! against a checked-in snapshot at
//! `crates/qedgen/tests/snapshots/<fixture>.codegen.txt`.
//!
//! Distinct from the Lean and Kani snapshots in two ways:
//!   1. **Multi-file output**: codegen ships `lib.rs`, `state.rs`,
//!      `errors.rs`, `events.rs`, `instructions/<handler>.rs`,
//!      `guards.rs`, `math.rs`, `Cargo.toml`, `imported/<ns>.rs`,
//!      etc. The snapshot is a concatenated dump of every file in
//!      the `programs/` tree, sorted by relative path, with
//!      `--- <relpath> ---` headers between files.
//!   2. **Idempotent files skipped**: `lib.rs` and
//!      `instructions/<name>.rs` are user-owned (skipped if
//!      existing). Snapshot regenerates from a clean tempdir so
//!      every file emits fresh.
//!
//! When the snapshot diverges, the test prints the unified diff and
//! fails. Refresh via `UPDATE_SNAPSHOTS=1 cargo test --test
//! codegen_snapshot`.

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

/// Copy a fixture into a tempdir, run `qedgen codegen --spec <spec>`,
/// then dump every file under `programs/` into a single concatenated
/// text blob with `--- <relpath> ---` headers. Files are visited in
/// sorted-relative-path order for determinism.
fn render_mir_codegen(fixture_dir: &str, spec_arg: &str) -> String {
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
        .current_dir(tmp.path())
        .status()
        .expect("spawn qedgen codegen");
    assert!(
        status.success(),
        "qedgen codegen failed for {}",
        fixture_dir
    );

    dump_programs_tree(&tmp.path().join("programs"))
}

/// Walk the `programs/` tree, collect every file path + content,
/// sort by relative path, and return a concatenated blob with
/// `--- <relpath> ---` headers between files. Binary files are
/// represented by a `[binary file, N bytes]` line so the snapshot
/// stays text-diff-able.
fn dump_programs_tree(root: &Path) -> String {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut entries);
    entries.sort();

    let mut out = String::new();
    for path in entries {
        let rel = path
            .strip_prefix(root)
            .expect("file under root")
            .to_string_lossy()
            .replace('\\', "/");
        out.push_str(&format!("--- {} ---\n", rel));
        match fs::read_to_string(&path) {
            Ok(text) => {
                out.push_str(&text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(_) => {
                let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                out.push_str(&format!("[binary file, {} bytes]\n", bytes));
            }
        }
        out.push('\n');
    }
    out
}

fn collect_files(dir: &Path, acc: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_files(&p, acc);
        } else {
            acc.push(p);
        }
    }
}

fn assert_or_update_snapshot(fixture: &str, fixture_dir: &str, spec_arg: &str) {
    let rendered = render_mir_codegen(fixture_dir, spec_arg);
    let snapshot_path = snapshots_dir().join(format!("{}.codegen.txt", fixture));
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
            "{fixture}: MIR codegen snapshot drift detected.\n\
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
    let max_lines = 120usize;
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

// `cross-program-vault` is intentionally omitted from this set. It
// has a sibling `[dependencies.admin_config] path =
// "../../imports/cross-program-vault-admin"` that doesn't survive
// the tempdir rsync (the resolver looks for the import relative to
// the spec file's parent, which the tempdir doesn't have).
// `mir_snapshot` (Lean) and `kani_snapshot` (Kani) handle their
// equivalents through different mechanisms; the manual Phase 4
// byte-equivalence sweep covers this fixture's `programs/` tree
// end-to-end. Wiring it into this snapshot harness needs a
// special-case import-path setup — a future cleanup, not blocking
// the Phase 4i dispatch flip.
