//! #88 regression gate — the CLI regen path for sBPF `Spec.lean`.
//!
//! Before the fix, `qedgen codegen` on an sBPF spec either:
//!   * errored with "No handlers found in the spec" before reaching the
//!     Lean branch (old-syntax specs with `instruction` blocks — the
//!     scaffold's handlers gate fired first), or
//!   * silently emitted an Anchor-shaped Rust scaffold that is
//!     meaningless for assembly programs (modern-syntax sBPF specs
//!     with `handler` blocks).
//!
//! These tests drive the built binary the same way the snapshot suites
//! do: a fresh tempdir + `git init`, then `qedgen codegen` invocations.
//! They assert that for `pragma sbpf` specs only `--lean` (and `--ci`)
//! emit, the Rust scaffold is suppressed, and the generated header
//! names a command that actually exists.

mod common;

use common::{ensure_qedgen_built, qedgen_bin, repo_root};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Fresh git-initialized tempdir containing the given spec file.
fn sbpf_project(spec_src: &Path, spec_name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    fs::copy(spec_src, tmp.path().join(spec_name)).expect("copy spec fixture");
    common::git_init(tmp.path());
    tmp
}

fn run_codegen(dir: &Path, args: &[&str]) -> std::process::Output {
    ensure_qedgen_built();
    Command::new(qedgen_bin())
        .arg("codegen")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn qedgen codegen")
}

/// Old-syntax sBPF spec (`instruction` blocks, no handlers) — the
/// issue's headline symptom: the handlers gate fired before the Lean
/// branch was reached.
#[test]
fn sbpf_old_syntax_lean_regen_path_works() {
    let spec_src = repo_root().join("crates/qedgen/tests/fixtures/vault_lock_sbpf.qedspec");
    let tmp = sbpf_project(&spec_src, "vault_lock.qedspec");

    let out = run_codegen(
        tmp.path(),
        &[
            "--lean",
            "--spec",
            "vault_lock.qedspec",
            "--lean-output",
            "formal_verification/Spec.lean",
        ],
    );
    assert!(
        out.status.success(),
        "codegen --lean failed on old-syntax sBPF spec:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let spec_lean = tmp.path().join("formal_verification/Spec.lean");
    let rendered = fs::read_to_string(&spec_lean).expect("Spec.lean written");
    // The header must name the real regen command (the pre-fix header
    // pointed at `qedgen lean-gen`, deleted in v2.32).
    assert!(
        rendered.contains("qedgen codegen --lean"),
        "Spec.lean header should name the real regen command:\n{}",
        rendered.lines().take(4).collect::<Vec<_>>().join("\n"),
    );
    assert!(
        !rendered.contains("lean-gen"),
        "stale `lean-gen` verb in header"
    );

    // No Rust scaffold for assembly targets.
    assert!(
        !tmp.path().join("programs").exists(),
        "Rust scaffold must be suppressed for sBPF specs"
    );
}

/// `--all` on an sBPF spec emits exactly Lean + CI; every Rust-shaped
/// artifact is suppressed rather than erroring.
#[test]
fn sbpf_codegen_all_emits_only_lean_and_ci() {
    let spec_src = repo_root().join("crates/qedgen/tests/fixtures/vault_lock_sbpf.qedspec");
    let tmp = sbpf_project(&spec_src, "vault_lock.qedspec");

    let out = run_codegen(tmp.path(), &["--all", "--spec", "vault_lock.qedspec"]);
    assert!(
        out.status.success(),
        "codegen --all failed on sBPF spec:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    assert!(
        tmp.path().join("formal_verification/Spec.lean").exists(),
        "--all should emit Lean for sBPF specs"
    );
    assert!(
        tmp.path().join(".github/workflows/verify.yml").exists(),
        "--all should emit CI for sBPF specs"
    );
    for suppressed in ["programs", "fuzz", "tests/integration_tests.rs"] {
        assert!(
            !tmp.path().join(suppressed).exists(),
            "`{suppressed}` must not be emitted for sBPF specs"
        );
    }
}

/// Modern-syntax sBPF spec (`handler` blocks + `pragma sbpf`, like the
/// bundled `examples/sbpf/*`) — the pre-fix "bonus wart": the run
/// succeeded but emitted a meaningless Anchor-shaped Rust scaffold.
#[test]
fn sbpf_modern_syntax_suppresses_rust_scaffold() {
    let spec_src = repo_root().join("examples/sbpf/counter/counter.qedspec");
    let tmp = sbpf_project(&spec_src, "counter.qedspec");

    let out = run_codegen(tmp.path(), &["--spec", "counter.qedspec"]);
    assert!(
        out.status.success(),
        "codegen failed on modern-syntax sBPF spec:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !tmp.path().join("programs").exists(),
        "Rust scaffold must be suppressed for sBPF specs"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("skipping Rust scaffold"),
        "expected a skip note on stderr, got:\n{stderr}"
    );
}
