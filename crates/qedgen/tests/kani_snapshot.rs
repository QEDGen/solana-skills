//! Phase 3f Kani-MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 3f".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo}`),
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

mod common;

use common::SnapshotHarness;
use std::fs;
use std::process::Command;

/// Copy a fixture into a tempdir, run `qedgen codegen --spec <spec>
/// --kani`, and return the rendered `tests/kani.rs` string.
///
/// The fixture is copied (rather than codegen-into-place) because
/// `qedgen codegen --kani` rewrites `programs/` too; the copy
/// isolates the workspace from those rewrites.
///
/// MIR (`kani_mir`) is the sole Kani-codegen path — the legacy
/// `kani` renderer was deleted in v2.32.
fn render_mir_kani(fixture_dir: &str, spec_arg: &str) -> String {
    common::ensure_qedgen_built();
    let tmp = common::stage_fixture(fixture_dir);

    let status = Command::new(common::qedgen_bin())
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

const HARNESS: SnapshotHarness = SnapshotHarness {
    suffix: ".kani.rs",
    kind: "MIR Kani",
    render: render_mir_kani,
};

fn assert_or_update_snapshot(fixture: &str, fixture_dir: &str, spec_arg: &str) {
    HARNESS.assert_or_update(fixture, fixture_dir, spec_arg);
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
fn snapshot_let_bindings_fee_split() {
    assert_or_update_snapshot(
        "let-bindings-fee-split",
        "crates/qedgen/tests/fixtures/let-bindings",
        "fee_split.qedspec",
    );
}

#[test]
fn snapshot_escrow_split() {
    assert_or_update_snapshot("escrow-split", "examples/rust/escrow-split", ".");
}

#[test]
fn snapshot_kani_cpi_account_bindings() {
    assert_or_update_snapshot(
        "kani-cpi-account-bindings",
        "crates/qedgen/tests/fixtures/kani-cpi-account-bindings",
        "config.qedspec",
    );
}
