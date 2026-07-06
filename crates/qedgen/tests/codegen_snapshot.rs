//! Phase 4i codegen-MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 4i".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo, cross-program-vault}`),
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

mod common;

use common::SnapshotHarness;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Copy a fixture into a tempdir, run `qedgen codegen --spec <spec>`,
/// then dump every file under `programs/` into a single concatenated
/// text blob with `--- <relpath> ---` headers. Files are visited in
/// sorted-relative-path order for determinism.
fn render_mir_codegen(fixture_dir: &str, spec_arg: &str) -> String {
    common::ensure_qedgen_built();
    let tmp = common::stage_fixture(fixture_dir);

    let status = Command::new(common::qedgen_bin())
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

const HARNESS: SnapshotHarness = SnapshotHarness {
    suffix: ".codegen.txt",
    kind: "MIR codegen",
    render: render_mir_codegen,
};

fn assert_or_update_snapshot(fixture: &str, fixture_dir: &str, spec_arg: &str) {
    HARNESS.assert_or_update(fixture, fixture_dir, spec_arg);
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
