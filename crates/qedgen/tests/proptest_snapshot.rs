//! Phase 5 proptest-MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 5".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo}`),
//! regenerates the MIR-rendered `tests/proptest.rs` and compares
//! against a checked-in snapshot at
//! `crates/qedgen/tests/snapshots/<fixture>.proptest.rs`.
//!
//! MIR (`proptest_gen_mir`) is the sole proptest-codegen path — the
//! legacy `proptest_gen` renderer was deleted in v2.32. When the
//! snapshot diverges, the test prints the unified diff and fails;
//! refresh via `UPDATE_SNAPSHOTS=1 cargo test --test proptest_snapshot`.

mod common;

use common::SnapshotHarness;
use std::fs;
use std::process::Command;

/// Copy a fixture into a tempdir, run `qedgen codegen --spec <spec>
/// --proptest`, and return the rendered `tests/proptest.rs` string.
fn render_mir_proptest(fixture_dir: &str, spec_arg: &str) -> String {
    common::ensure_qedgen_built();
    let tmp = common::stage_fixture(fixture_dir);

    let status = Command::new(common::qedgen_bin())
        .arg("codegen")
        .arg("--spec")
        .arg(spec_arg)
        .arg("--proptest")
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

const HARNESS: SnapshotHarness = SnapshotHarness {
    suffix: ".proptest.rs",
    kind: "MIR proptest",
    render: render_mir_proptest,
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
