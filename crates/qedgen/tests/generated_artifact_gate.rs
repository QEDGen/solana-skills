//! Executable generated-artifact gate (#294).
//!
//! Snapshot suites prove generated text is stable; users consume it as
//! compiled and executed software. This gate closes that gap for the
//! Anchor lane. For each bundled example it, from a clean tempdir:
//!
//! 1. runs `qedgen codegen --all`;
//! 2. asserts every expected Rust artifact exists — a silently skipped
//!    artifact fails here, not in a user's project;
//! 3. compiles the scaffold and every test target, and RUNS the generated
//!    unit tests and proptests (`cargo test`);
//! 4. type-checks the generated Kani harness with ordinary rustc
//!    (`cargo rustc --test kani -- --cfg kani` against the
//!    `qedgen-kani-compile-stub` crate) — the harness is `#![cfg(kani)]`,
//!    so step 3 alone would compile it to nothing. Kani proof EXECUTION
//!    stays in its dedicated workflow; this gates compilation only.
//!
//! All examples share one cargo target dir (`target/generated-artifact-
//! gate`) so anchor-lang and friends compile once per run and CI's cargo
//! cache covers them.
//!
//! Tests are `#[ignore]` (compile-heavy); CI runs them with `-- --ignored`
//! in a dedicated job.
//!
//! First full run (2026-07-20) caught four latent defect classes across
//! the bundled examples — see the #294 thread — which is the point: none
//! of them were visible to `cargo check` or the snapshot suites.

mod common;

use common::{redirect_macros_to_path, repo_root, run_capture_ok, run_ok};
use std::path::Path;
use std::process::Command;

/// Every Rust artifact `codegen --all` must produce for an Anchor spec.
/// Missing ⇒ the artifact was silently skipped ⇒ fail.
const REQUIRED_ARTIFACTS: &[&str] = &[
    "Cargo.toml",
    "src/lib.rs",
    "tests/unit.rs",
    "tests/proptest.rs",
    "tests/kani.rs",
];

/// Shared cargo target dir for all gate compiles (dep reuse + CI cache).
fn gate_target_dir() -> std::path::PathBuf {
    repo_root().join("target").join("generated-artifact-gate")
}

/// Add the compile-only `kani` stub to the generated crate's
/// `[dev-dependencies]` so `--cfg kani` compilation can resolve
/// `kani::*` paths without the Kani toolchain.
fn inject_kani_stub(cargo_toml: &Path) {
    let manifest = std::fs::read_to_string(cargo_toml).expect("read Cargo.toml");
    assert!(
        !manifest.contains("qedgen-kani-compile-stub"),
        "kani stub already injected in {}",
        cargo_toml.display()
    );
    let stub_path = repo_root().join("crates/kani-compile-stub");
    let dep =
        format!("kani = {{ package = \"qedgen-kani-compile-stub\", path = {stub_path:?} }}\n");
    let rewritten = if manifest.contains("[dev-dependencies]") {
        manifest.replace(
            "[dev-dependencies]\n",
            &format!("[dev-dependencies]\n{dep}"),
        )
    } else {
        format!("{manifest}\n[dev-dependencies]\n{dep}")
    };
    std::fs::write(cargo_toml, rewritten).expect("rewrite Cargo.toml");
}

/// Full gate for one bundled Anchor example.
fn gate_anchor_example(example: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let example_dir = repo_root().join("examples/rust").join(example);
    let spec_path = temp.path().join(format!("{example}.qedspec"));
    std::fs::copy(example_dir.join(format!("{example}.qedspec")), &spec_path)
        .unwrap_or_else(|e| panic!("copy {example} spec: {e}"));
    std::fs::copy(example_dir.join("qed.toml"), temp.path().join("qed.toml"))
        .unwrap_or_else(|e| panic!("copy {example} manifest: {e}"));
    std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");
    common::git_init(temp.path());

    let output_dir = temp.path().join("programs");
    run_ok(
        Command::new(env!("CARGO_BIN_EXE_qedgen"))
            .arg("codegen")
            .arg("--spec")
            .arg(&spec_path)
            .arg("--target")
            .arg("anchor")
            .arg("--all")
            .arg("--output-dir")
            .arg(&output_dir)
            .current_dir(temp.path()),
    );

    // (2) Silent-skip guard: every expected artifact must exist.
    for rel in REQUIRED_ARTIFACTS {
        assert!(
            output_dir.join(rel).is_file(),
            "{example}: `codegen --all` silently skipped {rel}"
        );
    }

    let cargo_toml = output_dir.join("Cargo.toml");
    redirect_macros_to_path(&cargo_toml);
    inject_kani_stub(&cargo_toml);

    // (3) Compile scaffold + all test targets; run unit tests + proptests.
    // `--no-fail-fast`: report every failing target in one run — cargo's
    // default stops at the first failing test binary, hiding the rest.
    let output = run_capture_ok(
        Command::new("cargo")
            .arg("test")
            .arg("--manifest-path")
            .arg(&cargo_toml)
            .arg("--no-fail-fast")
            .env("CARGO_TARGET_DIR", gate_target_dir()),
    );
    // Execution-level silent-skip guard: cargo must have RUN both
    // generated test targets, not merely compiled them.
    for target in ["unit.rs", "proptest.rs"] {
        assert!(
            output.contains(target),
            "{example}: cargo test did not run the generated {target} target:\n{output}"
        );
    }

    // (4) Kani harness compile gate (ordinary rustc + stub, no toolchain).
    run_ok(
        Command::new("cargo")
            .arg("rustc")
            .arg("--manifest-path")
            .arg(&cargo_toml)
            .arg("--test")
            .arg("kani")
            .env("CARGO_TARGET_DIR", gate_target_dir())
            .arg("--")
            .arg("--cfg")
            .arg("kani"),
    );
}

#[test]
#[ignore = "compile-heavy: codegen --all + cargo test + kani compile gate"]
fn escrow_generated_artifacts_compile_and_run() {
    gate_anchor_example("escrow");
}

#[test]
#[ignore = "compile-heavy: codegen --all + cargo test + kani compile gate"]
fn lending_generated_artifacts_compile_and_run() {
    gate_anchor_example("lending");
}

#[test]
#[ignore = "compile-heavy: codegen --all + cargo test + kani compile gate"]
fn multisig_generated_artifacts_compile_and_run() {
    gate_anchor_example("multisig");
}
