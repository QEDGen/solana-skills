//! Phase 1d MIR snapshot equivalence — `docs/design/qedgen-mir-sketch.md`
//! §"Phase 1d — snapshot equivalence".
//!
//! For every pilot fixture (`examples/rust/{escrow, escrow-split,
//! lending, multisig, bundled-stdlib-demo, cross-program-vault}`),
//! regenerates the MIR-rendered `Spec.lean` and compares against a
//! checked-in snapshot at `tests/snapshots/<fixture>.Spec.lean`.
//!
//! When the snapshot diverges, the test prints the unified diff and
//! fails — silent drift between MIR + legacy renderers is detectable
//! immediately. Refreshing a snapshot is intentional and requires
//! re-running the fixture binary + committing the updated file
//! (`UPDATE_SNAPSHOTS=1 cargo test --test mir_snapshot` writes them
//! in place).
//!
//! Path coverage across fixtures (post-v2.33, when `pragma state_repr =
//! adt` became the explicit opt-in for the inductive multi-variant
//! State, replacing the incidental `WrongState`-error footgun):
//!   * Flat path (escrow, escrow-split, bundled-stdlib-demo, lending):
//!     the default `structure State` + `status` discriminant. These
//!     were legacy ADT byte-identity fixtures before the representation
//!     default flipped to flat.
//!   * ADT path (cross-program-vault): declares `pragma state_repr =
//!     adt` — its hand-written instruction logic destructures the
//!     inner-enum, so it is the bundled `inductive State` /
//!     `render_single_account_adt` showcase. The dispatch itself (same
//!     shape ⇒ flat vs ADT by pragma) is additionally unit-tested by
//!     `lean_gen_mir::tests::state_repr_pragma_dispatches_inductive_vs_flat`.
//!   * Indexed path (multisig): byte-identical post Phase 1e
//!     indexed-state lowering (Mathlib + IndexedState imports,
//!     `Map[N] T` capacity, `Function.update` collapse).
//!   * Multi-account path (lending): byte-identical post Phase 2
//!     multi-account renderer (per-account `<Name>State` structures,
//!     per-group `apply<Name>Op` dispatchers, per-property
//!     environment scoping, per-via-op liveness scoping).

mod common;

use common::SnapshotHarness;
use std::fs;
use std::process::Command;

/// Run `qedgen codegen --spec <spec> --lean` in an isolated tempdir
/// and return the rendered `Spec.lean` string.
///
/// MIR (`lean_gen_mir`) is the sole Lean-codegen path — the legacy
/// `lean_gen` renderer was deleted in v2.32.
fn render_mir_spec(_fixture_dir: &str, spec_arg: &str) -> String {
    common::ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("create tempdir");
    let root = tmp.path().canonicalize().expect("canonicalize tempdir");
    common::git_init(&root);
    // #279: relative outputs resolve against the SPEC's project root, so
    // stage the spec surface into the tempdir instead of pointing --spec
    // at the real repo example (which would regenerate into the repo).
    let staged = common::stage_spec_surface(&common::repo_root().join(spec_arg), &root);

    let status = Command::new(common::qedgen_bin())
        .arg("codegen")
        .arg("--spec")
        .arg(&staged)
        .arg("--lean")
        .current_dir(&root)
        .status()
        .expect("spawn qedgen codegen");
    assert!(status.success(), "qedgen codegen failed for {}", spec_arg);

    let out = root.join("formal_verification").join("Spec.lean");
    fs::read_to_string(&out).unwrap_or_else(|e| panic!("read {}: {e}", out.display()))
}

const HARNESS: SnapshotHarness = SnapshotHarness {
    suffix: ".Spec.lean",
    kind: "MIR",
    render: render_mir_spec,
};

fn assert_or_update_snapshot(fixture: &str, spec_arg: &str) {
    HARNESS.assert_or_update(fixture, "", spec_arg);
}

// ---- Per-fixture snapshot tests ----
//
// Each test is small + boilerplate-light so failures point at one
// fixture. Adding a new pilot fixture: drop a new test with the same
// shape + run `UPDATE_SNAPSHOTS=1 cargo test --test mir_snapshot
// <new_fixture_name>` once to seed.

#[test]
fn snapshot_escrow() {
    assert_or_update_snapshot("escrow", "examples/rust/escrow/escrow.qedspec");
}

#[test]
fn snapshot_lending() {
    assert_or_update_snapshot("lending", "examples/rust/lending/lending.qedspec");
}

#[test]
fn snapshot_multisig() {
    assert_or_update_snapshot("multisig", "examples/rust/multisig/multisig.qedspec");
}

#[test]
fn snapshot_bundled_stdlib_demo() {
    assert_or_update_snapshot(
        "bundled-stdlib-demo",
        "examples/rust/bundled-stdlib-demo/pool.qedspec",
    );
}

#[test]
fn snapshot_cross_program_vault() {
    assert_or_update_snapshot(
        "cross-program-vault",
        "examples/rust/cross-program-vault/vault.qedspec",
    );
}

#[test]
fn snapshot_let_bindings_fee_split() {
    assert_or_update_snapshot(
        "let-bindings-fee-split",
        "crates/qedgen/tests/fixtures/let-bindings/fee_split.qedspec",
    );
}

#[test]
fn snapshot_escrow_split() {
    assert_or_update_snapshot("escrow-split", "examples/rust/escrow-split");
}
