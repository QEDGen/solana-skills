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
//! So this gate does what the text tests cannot: it runs codegen against a
//! frozen fixture spec and type-checks the result against the real pinned
//! Parallax and the real published `quasar-lang`.
//!
//! ## Both halves are real (#383)
//!
//! This gate used to compile the scaffold against a hand-written stub crate
//! that mirrored what Quasar codegen was believed to produce. The stub was
//! a fiction in two places, and each fiction hid a defect that shipped:
//!
//! - its state struct was a plain `wincode`-derived struct, so the gate
//!   never saw that Quasar's `#[account]` leaves behind a view type over
//!   `AccountView` with no constructible fields. The scaffold's struct
//!   literal could not compile, and did not, for every real Quasar program.
//! - its error enum hand-wrote `impl From<VaultError> for u32`, which
//!   Quasar's `#[error_code]` does not emit. `Outcome::error(Err::X)`
//!   therefore resolved in the gate and nowhere else.
//!
//! Both halves are now generated. The `program` surface the scaffold
//! imports — `client` instruction builders, `state`, `errors`, `ID` — comes
//! from `codegen --target quasar` on the fixture spec, so a Quasar codegen
//! change that breaks the client boundary fails here.
//!
//! ## What is covered
//!
//! Everything the scaffold touches on the Parallax side: `Ctx::builder` /
//! `crate_name` / `build`, `execute_with` argument shapes, `Outcome::check`
//! chaining and its receiver, `Outcome::success` / `Outcome::error`
//! (including the `Into<u32>` blanket impl the negative tests rely on),
//! `Account` construction both ways, and every prelude symbol the emitters
//! name (`system_program`, `DEFAULT_WALLET_LAMPORTS`,
//! `SPL_TOKEN_PROGRAM_ID`, `Instruction`, `Pubkey`, `Cu`) — plus, now, the
//! generated Quasar program those symbols are wired to.
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

use common::{
    ensure_qedgen_built, qedgen_bin, redirect_macros_to_path, run_capture_ok, run_ok, stage_fixture,
};
use std::process::Command;

/// Shared target dir so the Agave dependency tree compiles once per machine
/// and CI's cargo cache covers it.
fn gate_target_dir(target: &str) -> std::path::PathBuf {
    common::repo_root()
        .join("target")
        .join(format!("parallax-integration-gate-{target}"))
}

#[test]
#[ignore = "compile-heavy: fetches and builds LiteSVM + the Agave runtime"]
fn quasar_integration_scaffold_compiles() {
    gate_target("quasar");
}

/// #366 — the Anchor half. The scaffold has no generated client module to
/// import here, so it builds instructions from the Anchor ABI directly:
/// discriminator, Borsh arguments, declared account metas. That path had
/// never been compiled, and the two defects it turned out to carry (the
/// `solana_pubkey` / `solana_address` split, and untyped argument literals)
/// are exactly the kind a text assertion cannot see.
#[test]
#[ignore = "compile-heavy: fetches and builds LiteSVM + the Agave runtime"]
fn anchor_integration_scaffold_compiles() {
    gate_target("anchor");
}

fn gate_target(target: &str) {
    ensure_qedgen_built();
    let tmp = stage_fixture("crates/qedgen/tests/fixtures/parallax-api-gate");
    std::fs::create_dir_all(tmp.path().join(".qed")).expect("create .qed");
    common::git_init(tmp.path());

    let output_dir = tmp.path().join("programs");
    let manifest = output_dir.join("Cargo.toml");
    let generated = output_dir.join("tests/integration_tests.rs");

    // The program scaffold FIRST. Bare `codegen` writes `Cargo.toml` from
    // scratch, so running it after the integration pass would drop the
    // Parallax dev-dependencies that pass upserts.
    run_ok(
        Command::new(qedgen_bin())
            .arg("codegen")
            .arg("--spec")
            .arg("vault.qedspec")
            .arg("--target")
            .arg(target)
            .arg("--output-dir")
            .arg(&output_dir)
            .current_dir(tmp.path()),
    );

    run_ok(
        Command::new(qedgen_bin())
            .arg("codegen")
            .arg("--spec")
            .arg("vault.qedspec")
            .arg("--target")
            .arg(target)
            .arg("--integration")
            .arg("--output-dir")
            .arg(&output_dir)
            .current_dir(tmp.path()),
    );

    assert!(
        generated.is_file(),
        "codegen --integration silently skipped the scaffold"
    );

    // The dev-dependency upsert is part of the contract: without it the
    // scaffold cannot resolve `parallax_svm` and the compile below is
    // meaningless. Nothing pre-seeds these — the manifest is generated by
    // the pass above, so every one of them got there by upsert.
    let after = std::fs::read_to_string(&manifest).expect("read generated manifest");
    for dependency in ["parallax-svm", "spl-token", "solana-sdk-ids", "wincode"] {
        assert!(
            after.contains(&format!("{dependency} =")),
            "upsert did not add {dependency} to [dev-dependencies]:\n{after}"
        );
    }

    // The generated manifest pins `qedgen-macros` to a git tag at the
    // current crate version, which does not exist upstream until release.
    redirect_macros_to_path(&manifest);

    // The actual gate: type-check the generated program AND its generated
    // test target against the pinned Parallax and the published
    // `quasar-lang`. `--tests` so the test target is compiled, not just the
    // lib.
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
            .env("CARGO_TARGET_DIR", gate_target_dir(target))
            .env("RUSTFLAGS", "-D warnings"),
    );
}
