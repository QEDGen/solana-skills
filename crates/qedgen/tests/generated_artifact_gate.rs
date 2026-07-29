//! Executable generated-artifact gate (#294).
//!
//! Snapshot suites prove generated text is stable; users consume it as
//! compiled and executed software. This gate closes that gap for the
//! Anchor and Quasar lanes. For each bundled example at each target it,
//! from a clean tempdir:
//!
//! 1. runs `qedgen codegen` for every artifact this gate compiles
//!    (see `generate_artifacts` for why Quasar does not use `--all`);
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

fn gate_anchor_example(example: &str) {
    gate_example(example, "anchor");
}

/// Quasar counterpart (#372). Kept separate from the Anchor entry point so
/// the artifact set each target must produce stays explicit.
fn gate_quasar_example(example: &str) {
    gate_example(example, "quasar");
}

/// Generate every artifact this gate compiles, for one target.
///
/// Never `--all`, because `--all` emits the Parallax integration scaffold
/// and this gate RUNS what it generates. Those tests load the program under
/// test into LiteSVM, and nothing here produces a `.so` — no `cargo
/// build-sbf` — so every one of them panics in `setup()`:
///
/// ```text
/// thread 'test_cancel' panicked at tests/integration_tests.rs:63:
///   load compiled program into Parallax
/// ```
///
/// Pulling `parallax-svm` into `[dev-dependencies]` is the second reason:
/// it drags a large git-sourced tree into a gate that exists to catch
/// CODEGEN regressions, where an upstream dependency break would be
/// indistinguishable from a codegen break.
///
/// Quasar has always been excluded this way. Anchor joined it when #366
/// gave Anchor an instruction builder, since `--all --target anchor` began
/// emitting the scaffold too — which is exactly how this gate went red on
/// that change.
///
/// The integration lane has its own gate (`parallax_integration_gate`),
/// which type-checks a real generated crate per target without executing
/// it. This one covers the program crate and the three framework-neutral
/// harnesses, and it does execute those.
fn generate_artifacts(spec_path: &Path, output_dir: &Path, cwd: &Path, target: &str) {
    let codegen = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qedgen"));
        cmd.arg("codegen")
            .arg("--spec")
            .arg(spec_path)
            .arg("--target")
            .arg(target)
            .arg("--output-dir")
            .arg(output_dir)
            .args(extra)
            .current_dir(cwd);
        run_ok(&mut cmd);
    };

    // Bare run = the Rust scaffold; the second names the harnesses
    // explicitly. `scaffold_requested = all || !explicit_artifact_requested`
    // (run.rs), so one combined invocation would skip the scaffold.
    codegen(&[]);
    codegen(&["--test", "--proptest", "--kani"]);
}

/// Full gate for one bundled example at one target.
fn gate_example(example: &str, target: &str) {
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
    generate_artifacts(&spec_path, &output_dir, temp.path(), target);

    // (2) Silent-skip guard: every expected artifact must exist.
    for rel in REQUIRED_ARTIFACTS {
        assert!(
            output_dir.join(rel).is_file(),
            "{example} ({target}): codegen silently skipped {rel}"
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
    for artifact in ["unit.rs", "proptest.rs"] {
        assert!(
            output.contains(artifact),
            "{example} ({target}): cargo test did not run the generated \
             {artifact} target:\n{output}"
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

// #372 — the Quasar half. Until these existed, no Quasar artifact in this
// repo had ever been compiled: the snapshot suites compare generated TEXT,
// which was stable and wrong in exactly the same way every run, and this
// gate regenerated the three examples as ANCHOR even though all three ship
// as Quasar crates. `codegen --target quasar` emitted `Pubkey` into every
// one of them, a type quasar-lang does not define, so the bundled examples
// shipped programs that could not build.

#[test]
#[ignore = "compile-heavy: codegen + cargo test + kani compile gate"]
fn escrow_quasar_artifacts_compile_and_run() {
    gate_quasar_example("escrow");
}

#[test]
#[ignore = "compile-heavy: codegen + cargo test + kani compile gate"]
fn lending_quasar_artifacts_compile_and_run() {
    gate_quasar_example("lending");
}

#[test]
#[ignore = "compile-heavy: codegen + cargo test + kani compile gate"]
fn multisig_quasar_artifacts_compile_and_run() {
    gate_quasar_example("multisig");
}

/// #331 — the product-state proptest artifact for a multi-account +
/// ghost spec must COMPILE AND RUN, not just match a snapshot: the
/// product module composes per-account strategies, delegating wrappers
/// with atomic ghost updates, and the init-seeded sequence harness.
#[test]
#[ignore = "compile-heavy: codegen --proptest + cargo test against the proptest crate"]
fn product_state_ghost_proptest_compiles_and_runs() {
    common::ensure_qedgen_built();
    let tmp = common::stage_fixture("crates/qedgen/tests/fixtures/product-state-ghosts");

    run_ok(
        Command::new(common::qedgen_bin())
            .arg("codegen")
            .arg("--spec")
            .arg("ghost_pool.qedspec")
            .arg("--proptest")
            .arg("--proptest-output")
            .arg("harness/tests/proptest.rs")
            .current_dir(tmp.path()),
    );

    // Minimal crate around the generated test target.
    let harness = tmp.path().join("harness");
    std::fs::create_dir_all(harness.join("src")).expect("mkdir src");
    std::fs::write(harness.join("src/lib.rs"), "").expect("write lib.rs");
    std::fs::write(
        harness.join("Cargo.toml"),
        "[package]\n\
         name = \"product-state-ghosts-harness\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         \n\
         [dev-dependencies]\n\
         proptest = \"1\"\n\
         \n\
         [workspace]\n",
    )
    .expect("write Cargo.toml");

    let out = run_capture_ok(
        Command::new("cargo")
            .arg("test")
            .arg("--test")
            .arg("proptest")
            .env("CARGO_TARGET_DIR", gate_target_dir())
            .current_dir(&harness),
    );
    assert!(
        out.contains("product::product_state_machine_sequence"),
        "product sequence harness must run:\n{out}"
    );
    assert!(
        !out.contains("FAILED"),
        "generated product proptests must pass:\n{out}"
    );
}
