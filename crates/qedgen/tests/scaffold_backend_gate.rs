//! #364 gate: the `scaffold` verify backend compiles the generated program
//! crate.
//!
//! Before this backend, nothing in the user-facing loop ever built what
//! codegen wrote. `check` is spec-level lint, and `verify` builds the
//! Kani/proptest harnesses in isolated crates on purpose. So "codegen emits
//! Rust that does not compile" reached users as a red `cargo build` with no
//! qedgen diagnostic, often right after `check` printed `0 error(s)`.
//!
//! Most tests here use a tiny dependency-free crate: the backend's job is to
//! run cargo and classify the result, and that is fully exercised without a
//! cold `anchor-lang` build. The one test that generates a real Anchor
//! program is `#[ignore]`, like the other compile-heavy gates.

mod common;

use common::{ensure_qedgen_built, git_init, qedgen_bin, redirect_macros_to_path};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Minimal spec that parses and lowers. The scaffold backend does not read
/// it, but `verify` resolves and parses a spec on every run.
const SPEC: &str = "\
spec VaultChecked

type State
  | Active of {
      balance : U64,
    }

handler deposit (amount : U64) : State.Active -> State.Active {
  effect { balance += amount }
}
";

/// Spec carrying the #363 defect: `requires … else <Variant>` emits
/// `<Prog>Error::<Variant>` into `guards.rs` without declaring it in
/// `errors.rs`, and `check` reports `0 error(s)`.
const SPEC_UNDECLARED_VARIANT: &str = "\
spec ErrVar

type Error
  | InvalidAmount

type State
  | Active of {
      total : U64,
    }

handler settle (amount : U64) : State.Active -> State.Active {
  auth owner
  accounts { owner : signer, writable
             vault : writable }
  requires amount > 0 else UndeclaredRequiresErr
  effect { total := 0 }
}
";

/// A git-initialized project with a spec and a `.qed/` dir — what `verify`
/// expects to find.
fn project(spec: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("v.qedspec"), spec).expect("write spec");
    std::fs::create_dir_all(tmp.path().join(".qed")).expect("mkdir .qed");
    git_init(tmp.path());
    tmp
}

/// Dependency-free crate, so `cargo check` is sub-second and offline.
fn minimal_crate(root: &Path, lib_rs: &str) -> PathBuf {
    let dir = root.join("programs");
    std::fs::create_dir_all(dir.join("src")).expect("mkdir crate");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"mini\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write manifest");
    std::fs::write(dir.join("src/lib.rs"), lib_rs).expect("write lib.rs");
    dir
}

/// `verify` output plus its exit code. `common::run_ok` asserts success, and
/// half of what this gate checks is a deliberate failure.
fn verify_scaffold_with(
    project_root: &Path,
    program: &Path,
    extra_args: &[&str],
) -> (String, bool) {
    ensure_qedgen_built();
    let mut command = Command::new(qedgen_bin());
    command
        .args(["verify", "--spec", "v.qedspec", "--scaffold", "--program"])
        .arg(program)
        .args(extra_args)
        .current_dir(project_root);
    let out = command.output().expect("spawn qedgen verify");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (combined, out.status.success())
}

fn verify_scaffold(project_root: &Path, program: &Path) -> (String, bool) {
    verify_scaffold_with(project_root, program, &[])
}

/// A dependency-free shared repro crate whose one test fires. This exercises
/// the real `--probe-repros` stage without pulling in Mollusk or Solana.
fn passing_probe_repro(project_root: &Path) {
    let repros = project_root.join("target/qedgen-repros");
    std::fs::create_dir_all(repros.join("src")).expect("mkdir repro crate");
    std::fs::write(
        repros.join("Cargo.toml"),
        "[package]\nname = \"probe-repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write repro manifest");
    std::fs::write(
        repros.join("src/lib.rs"),
        "#[test]\nfn finding_fires() { assert_eq!(2 + 2, 4); }\n",
    )
    .expect("write repro test");
}

#[test]
fn type_error_fails_the_scaffold_backend() {
    let tmp = project(SPEC);
    let krate = minimal_crate(tmp.path(), "pub fn f() -> u8 { \"not a u8\" }\n");

    let (out, ok) = verify_scaffold(tmp.path(), &krate);

    assert!(
        !ok,
        "a crate that does not typecheck must fail verify:\n{out}"
    );
    assert!(out.contains("[FAIL] scaffold"), "{out}");
    assert!(
        out.contains("E0308"),
        "the rustc diagnostic is the whole value of the report:\n{out}"
    );
    assert!(
        out.contains("src/lib.rs"),
        "the report must name the offending file:\n{out}"
    );
}

#[test]
fn clean_crate_passes_the_scaffold_backend() {
    let tmp = project(SPEC);
    let krate = minimal_crate(tmp.path(), "pub fn f() -> u8 { 0 }\n");

    let (out, ok) = verify_scaffold(tmp.path(), &krate);

    assert!(ok, "a crate that typechecks must pass:\n{out}");
    assert!(out.contains("[PASS] scaffold"), "{out}");
}

#[test]
fn check_upstream_combination_still_runs_scaffold() {
    let tmp = project(SPEC);
    let krate = minimal_crate(tmp.path(), "pub fn f() -> u8 { 0 }\n");
    std::fs::write(tmp.path().join("qed.lock"), "version = 1\n").expect("write empty lock");

    let (out, ok) = verify_scaffold_with(tmp.path(), &krate, &["--check-upstream", "--offline"]);

    assert!(ok, "both requested stages must pass:\n{out}");
    assert!(
        out.contains("[PASS] scaffold"),
        "--check-upstream must not return before the requested scaffold backend:\n{out}"
    );
}

#[test]
fn probe_repros_combination_still_runs_scaffold() {
    let tmp = project(SPEC);
    let krate = minimal_crate(tmp.path(), "pub fn f() -> u8 { 0 }\n");
    passing_probe_repro(tmp.path());

    let (out, ok) = verify_scaffold_with(tmp.path(), &krate, &["--probe-repros"]);

    assert!(ok, "both requested stages must pass:\n{out}");
    assert!(out.contains("[FIRED] shared-crate"), "{out}");
    assert!(
        out.contains("[PASS] scaffold"),
        "--probe-repros must not return before the requested scaffold backend:\n{out}"
    );
}

/// Absence of a crate is not evidence of a defect. Skipping keeps the
/// backend usable in projects that have not run codegen yet.
#[test]
fn missing_crate_skips_rather_than_fails() {
    let tmp = project(SPEC);

    let (out, ok) = verify_scaffold(tmp.path(), &tmp.path().join("nonexistent"));

    assert!(ok, "a missing crate must not fail the run:\n{out}");
    assert!(out.contains("[SKIP] scaffold"), "{out}");
    assert!(out.contains("no Cargo.toml"), "the skip states why:\n{out}");
}

/// The trap this backend has to avoid: it compiles the REAL program crate,
/// so it looks implementation-bound. It is not. A passing `cargo check`
/// proves the code typechecks, never that it conforms to the spec, and must
/// never let `qedgen stamp` write `#[qed(verified)]`.
#[test]
fn scaffold_pass_does_not_authorize_stamp() {
    let tmp = project(SPEC);
    let krate = minimal_crate(tmp.path(), "pub fn f() -> u8 { 0 }\n");

    let (out, ok) = verify_scaffold(tmp.path(), &krate);
    assert!(ok, "{out}");

    let evidence = std::fs::read_to_string(tmp.path().join(".qed/verify-evidence.json"))
        .expect("verify records evidence");
    let v: serde_json::Value = serde_json::from_str(&evidence).expect("evidence is json");

    assert_eq!(
        v["implementation_verified"], false,
        "a scaffold pass must not set implementation_verified:\n{evidence}"
    );
    let scaffold = v["backends"]
        .as_array()
        .expect("backends array")
        .iter()
        .find(|b| b["name"] == "scaffold")
        .expect("scaffold backend recorded");
    assert_eq!(scaffold["status"], "passed", "{evidence}");
    assert_eq!(
        scaffold["implementation_bound"], false,
        "compiling is not verifying:\n{evidence}"
    );
}

/// The post-codegen check must not turn a first generation into a
/// multi-minute cold build. It defers until the dependency tree resolves,
/// and says so rather than staying silent.
#[test]
fn codegen_skips_the_compile_check_without_a_lockfile() {
    ensure_qedgen_built();
    let tmp = project(SPEC);

    let out = Command::new(qedgen_bin())
        .args(["codegen", "--spec", "v.qedspec", "--target", "anchor"])
        .current_dir(tmp.path())
        .output()
        .expect("spawn qedgen codegen");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        out.status.success(),
        "codegen must still succeed:\n{combined}"
    );
    assert!(
        combined.contains("skipping the generated-crate compile check"),
        "the deferred check must be stated, not silent:\n{combined}"
    );
    assert!(
        combined.contains("verify --scaffold"),
        "the note must point at the surface that runs it now:\n{combined}"
    );
}

/// The end-to-end case #364 was filed for: a real Anchor program carrying a
/// real codegen defect (#363's undeclared error variant), caught by qedgen
/// instead of by the user's `cargo build`.
///
/// `#[ignore]`: resolves and compiles the anchor-lang tree on first run.
/// CI runs it with `-- --ignored`, like `generated_artifact_gate`.
#[test]
#[ignore = "compile-heavy: resolves and typechecks the anchor-lang tree"]
fn undeclared_error_variant_fails_the_scaffold_backend() {
    ensure_qedgen_built();
    let tmp = project(SPEC_UNDECLARED_VARIANT);

    common::run_ok(
        Command::new(qedgen_bin())
            .args(["codegen", "--spec", "v.qedspec", "--target", "anchor"])
            .current_dir(tmp.path()),
    );

    let krate = tmp.path().join("programs");

    // The generated manifest pins `qedgen-macros` to a git tag at the
    // current crate version, which does not exist upstream until release
    // time. Point it at the in-repo crate so this gate tests the scaffold
    // and not the release calendar.
    redirect_macros_to_path(&krate.join("Cargo.toml"));

    // Confirm the defect is actually present before asserting it is caught,
    // so a codegen fix turns this into a clear failure here rather than a
    // silently vacuous pass.
    let guards = std::fs::read_to_string(krate.join("src/guards.rs")).expect("read guards.rs");
    let errors = std::fs::read_to_string(krate.join("src/errors.rs")).expect("read errors.rs");
    assert!(
        guards.contains("UndeclaredRequiresErr"),
        "fixture no longer reproduces #363: guards.rs does not name the variant"
    );
    assert!(
        !errors.contains("UndeclaredRequiresErr"),
        "#363 appears fixed — the variant is now declared. Update this gate to \
         assert the new behavior instead of deleting it."
    );

    let (out, ok) = verify_scaffold(tmp.path(), &krate);

    assert!(
        !ok,
        "a non-compiling generated crate must fail verify:\n{out}"
    );
    assert!(out.contains("[FAIL] scaffold"), "{out}");
    assert!(
        out.contains("E0599") && out.contains("UndeclaredRequiresErr"),
        "the report must name the undeclared variant:\n{out}"
    );
    assert!(
        out.contains("src/guards.rs"),
        "the report must point at the generated file:\n{out}"
    );
}
