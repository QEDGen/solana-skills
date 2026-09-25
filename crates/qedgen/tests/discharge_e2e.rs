//! End-to-end gate for `qedgen discharge` verdicts (#406) against a real
//! qedlift and a real Lean check.
//!
//! The unit tests in `descriptor.rs` drive a fake qedlift. This gate runs the
//! real one on qedsvm's own fixture programs, so the verdicts are qedlift's
//! and Lean's, not a script's.
//!
//! `#[ignore]` and opt-in, because it needs a built qedsvm checkout:
//!
//! ```bash
//! # in the qedsvm repo: build the Lean library and qedlift
//! lake build
//! (cd qedsvm-rs && cargo build -p qedlift --bin qedlift)
//!
//! QEDGEN_E2E_QEDSVM=/path/to/qedsvm \
//! QEDGEN_E2E_QEDLIFT=/path/to/qedsvm/qedsvm-rs/target/debug/qedlift \
//!   cargo test -p qedgen-solana-skills --test discharge_e2e -- --ignored
//! ```
//!
//! qedlift writes the `.so` path into a Lean block comment, and Lean block
//! comments nest, so a path containing `/-` breaks the emitted module. The
//! fixtures are copied into a fresh temp dir first; keep the qedsvm checkout
//! itself on a path without `/-` too.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    qedsvm: PathBuf,
    qedlift: PathBuf,
    work: tempfile::TempDir,
}

fn env() -> Env {
    let var = |name: &str| {
        PathBuf::from(std::env::var(name).unwrap_or_else(|_| {
            panic!("set {name} (see the module docs of tests/discharge_e2e.rs)")
        }))
    };
    let qedsvm = var("QEDGEN_E2E_QEDSVM");
    let qedlift = var("QEDGEN_E2E_QEDLIFT");
    let work = tempfile::tempdir().expect("tempdir");
    let fixtures = qedsvm.join("qedsvm-rs/tests/fixtures");
    for name in ["vault.so", "vault_deposit.so", "vault.codama.json"] {
        std::fs::copy(fixtures.join(name), work.path().join(name))
            .unwrap_or_else(|e| panic!("copy {name} from {}: {e}", fixtures.display()));
    }
    Env {
        qedsvm,
        qedlift,
        work,
    }
}

fn spec(dir: &Path, name: &str, effect: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "spec Vault\n\nstate {{\n  owner : Pubkey\n  total : U64\n  bump  : U8\n}}\n\n\
             handler increment {{\n  effect {{ {effect} }}\n}}\n\n\
             handler deposit (amount : U64) {{\n  effect {{ total += amount }}\n}}\n"
        ),
    )
    .unwrap();
    path
}

/// Run `qedgen discharge --json` and return (exit ok, verdict, reason).
fn discharge(
    env: &Env,
    spec: &Path,
    handler: &str,
    so: &str,
    lean: bool,
) -> (bool, String, Option<String>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qedgen"));
    cmd.arg("discharge")
        .arg("--spec")
        .arg(spec)
        .args(["--handler", handler, "--account", "vault", "--json"])
        .arg("--so")
        .arg(env.work.path().join(so))
        .arg("--idl")
        .arg(env.work.path().join("vault.codama.json"))
        .arg("--qedlift")
        .arg(&env.qedlift);
    if lean {
        cmd.arg("--lean-project").arg(&env.qedsvm);
    }
    let out = cmd.output().expect("spawn qedgen discharge");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "discharge --json is not JSON ({e}):\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (
        out.status.success(),
        report["verdict"].as_str().unwrap_or_default().to_string(),
        report["reason"].as_str().map(str::to_string),
    )
}

#[test]
#[ignore = "needs a built qedsvm checkout and qedlift (see module docs)"]
fn discharge_verdicts_match_qedlift_and_lean() {
    let env = env();
    let dir = env.work.path();
    let valid = spec(dir, "valid.qedspec", "total += 1");
    let wrong_delta = spec(dir, "wrong_delta.qedspec", "total += 2");

    // Valid descriptor + bytecode + passing Lean build.
    let (ok, verdict, _) = discharge(&env, &valid, "increment", "vault.so", true);
    assert!(ok && verdict == "verified", "valid discharge: {verdict}");

    // Same run with no Lean check: generated, not verified, still exit 0.
    let (ok, verdict, _) = discharge(&env, &valid, "increment", "vault.so", false);
    assert!(ok && verdict == "emitted", "no Lean project: {verdict}");

    // The obligation conflicts with the bytes.
    let (ok, verdict, reason) = discharge(&env, &wrong_delta, "increment", "vault.so", true);
    assert!(!ok && verdict == "rejected", "wrong delta: {verdict}");
    assert_eq!(reason.as_deref(), Some("mutation_mismatch"));

    // A legacy (schema v2) parameter descriptor is unsupported, never verified.
    let (ok, verdict, reason) = discharge(&env, &valid, "deposit", "vault_deposit.so", true);
    assert!(!ok && verdict == "unsupported", "legacy param: {verdict}");
    assert_eq!(reason.as_deref(), Some("missing_parameter_binding"));
}
