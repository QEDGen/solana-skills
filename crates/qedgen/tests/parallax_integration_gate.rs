//! Compile gate for the generated Parallax integration scaffold.
//!
//! The scaffold is emitted against `parallax-svm` pinned to a git revision
//! on a repository that is young and moving fast. The module's own unit
//! tests only assert on the generated TEXT (`out.contains(..)`), which
//! cannot notice that `Outcome::check` changed its receiver, that a symbol
//! left the prelude, or that `execute_with` took a different argument
//! shape. That class of break reaches a user as a red `cargo test` in their
//! own crate.
//!
//! So this gate does what the text tests cannot: it runs `codegen
//! --integration` against a frozen fixture spec, drops the result into a
//! crate that provides the `program` surface the scaffold imports, and
//! type-checks the whole thing against the real pinned Parallax.
//!
//! ## What is and is not covered
//!
//! COVERED — everything the scaffold touches on the Parallax side:
//! `Ctx::builder`/`crate_name`/`build`, `execute_with` argument shapes,
//! `Outcome::check` chaining and its receiver, `Outcome::success` /
//! `Outcome::error` (including the `Into<u32>` blanket impl the negative
//! tests rely on), `Account` construction both ways, and every prelude
//! symbol the emitters name (`system_program`, `DEFAULT_WALLET_LAMPORTS`,
//! `SPL_TOKEN_PROGRAM_ID`, `Instruction`, `Pubkey`, `Cu`).
//!
//! NOT COVERED — the Quasar client boundary. The stub below mirrors the
//! shapes Quasar codegen produces, so if Quasar's own output moves this gate
//! will not catch it.
//!
//! The blocker that forced the stub is gone: #372 gave Quasar its own
//! `Address` mapping, and `generated_artifact_gate` now compiles all three
//! bundled examples as Quasar. Replacing this stub with a real generated
//! Quasar program is the remaining step, and it is blocked on the dependency
//! problem below rather than on codegen.
//!
//! This gate is also the only thing that catches the wincode 0.5/0.6 split:
//! `litesvm` requires `wincode ^0.5.5`, and solana crates cross to 0.6 in
//! MINOR bumps that a caret requirement takes silently, landing two
//! incompatible majors in one graph. It went red exactly that way with no
//! qedgen change. `parallax_dev_dependencies` documents the pin list and how
//! to extend it; the pin-liveness script (#371) does NOT catch this, because
//! it checks that the revision exists, not that it builds.
//!
//! This gate is also the only thing that catches the wincode 0.5/0.6 split:
//! `litesvm` requires `wincode ^0.5.5`, and solana crates cross to 0.6 in
//! MINOR bumps that a caret requirement takes silently, landing two
//! incompatible majors in one graph. It went red exactly that way with no
//! qedgen change. `parallax_dev_dependencies` documents the pin list and how
//! to extend it; the pin-liveness script (#371) does NOT catch this, because
//! it checks that the revision exists, not that it builds.
//!
//! `#[ignore]`: pulls LiteSVM + the Agave runtime on first run (network,
//! multi-minute cold compile). CI runs it with `-- --ignored`, like
//! `generated_artifact_gate`.

mod common;

use common::{ensure_qedgen_built, qedgen_bin, run_capture_ok, run_ok, stage_fixture};
use std::process::Command;

/// Shared target dir so the Agave dependency tree compiles once per machine
/// and CI's cargo cache covers it.
fn gate_target_dir() -> std::path::PathBuf {
    common::repo_root()
        .join("target")
        .join("parallax-integration-gate")
}

#[test]
#[ignore = "compile-heavy: fetches and builds LiteSVM + the Agave runtime"]
fn parallax_integration_scaffold_compiles() {
    ensure_qedgen_built();
    let tmp = stage_fixture("crates/qedgen/tests/fixtures/parallax-api-gate");
    common::git_init(tmp.path());

    let stub = tmp.path().join("stub");
    let manifest = stub.join("Cargo.toml");
    let generated = stub.join("tests/integration_tests.rs");

    // Only the DEV section must start empty. The stub lib itself depends on
    // `parallax-svm` (its state struct is typed with Parallax's `Pubkey`),
    // so a whole-file search would trip on `[dependencies]`.
    let before = std::fs::read_to_string(&manifest).expect("read stub manifest");
    let dev_section = before
        .split_once("[dev-dependencies]")
        .map(|(_, rest)| rest.trim())
        .expect("fixture manifest declares [dev-dependencies]");
    assert!(
        dev_section.is_empty(),
        "the fixture's [dev-dependencies] must start empty so the upsert is what fills it, got:\n{dev_section}"
    );

    run_ok(
        Command::new(qedgen_bin())
            .arg("codegen")
            .arg("--spec")
            .arg("vault.qedspec")
            .arg("--target")
            .arg("quasar")
            .arg("--integration")
            .arg("--integration-output")
            .arg(&generated)
            .current_dir(tmp.path()),
    );

    assert!(
        generated.is_file(),
        "codegen --integration silently skipped the scaffold"
    );

    // The dev-dependency upsert is part of the contract: without it the
    // scaffold cannot resolve `parallax_svm` and the compile below is
    // meaningless.
    let after = std::fs::read_to_string(&manifest).expect("read stub manifest");
    for dependency in ["parallax-svm", "spl-token", "solana-sdk-ids", "wincode"] {
        assert!(
            after.contains(&format!("{dependency} =")),
            "upsert did not add {dependency} to [dev-dependencies]:\n{after}"
        );
    }

    // The actual gate: type-check the generated test target against the
    // pinned Parallax. `--tests` so the test target is compiled, not just
    // the stub lib.
    //
    // `-D warnings` is the warning-clean check: a generated DO-NOT-EDIT file
    // must not land dead code or an unused import, and denying promotes
    // those to errors, which `run_capture_ok` turns into a panic with the
    // full compiler output. Do NOT additionally grep the output for
    // "warning:" — cargo emits unrelated notices there (`warning: spurious
    // network error` on a slow fetch), and a grep would fail the gate for a
    // flaky download rather than a real defect.
    run_capture_ok(
        Command::new("cargo")
            .arg("check")
            .arg("--tests")
            .arg("--manifest-path")
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", gate_target_dir())
            .env("RUSTFLAGS", "-D warnings"),
    );
}
