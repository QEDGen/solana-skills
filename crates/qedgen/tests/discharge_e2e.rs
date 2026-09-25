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

/// Run `qedgen discharge --json` and return (exit ok, verdict, reason). `extra` is appended
/// to the command line (the input-layout flags).
fn discharge(
    env: &Env,
    spec: &Path,
    handler: &str,
    so: &str,
    lean: bool,
    extra: &[&str],
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
    cmd.args(extra);
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
    let (ok, verdict, _) = discharge(&env, &valid, "increment", "vault.so", true, &[]);
    assert!(ok && verdict == "verified", "valid discharge: {verdict}");

    // Same run with no Lean check: generated, not verified, still exit 0.
    let (ok, verdict, _) = discharge(&env, &valid, "increment", "vault.so", false, &[]);
    assert!(ok && verdict == "emitted", "no Lean project: {verdict}");

    // The obligation conflicts with the bytes.
    let (ok, verdict, reason) = discharge(&env, &wrong_delta, "increment", "vault.so", true, &[]);
    assert!(!ok && verdict == "rejected", "wrong delta: {verdict}");
    assert_eq!(reason.as_deref(), Some("mutation_mismatch"));

    // A parameter delta with the right schema v3 input layout binds `amount` to its
    // serialized instruction-data address and verifies (#404).
    let layout = ["--account-data-lengths", "41"];
    let (ok, verdict, _) = discharge(&env, &valid, "deposit", "vault_deposit.so", true, &layout);
    assert!(ok && verdict == "verified", "v3 deposit: {verdict}");

    // A wrong account length moves the instruction-data address, so qedlift rejects the
    // binding instead of proving a statement about the wrong bytes.
    let wrong_len = ["--account-data-lengths", "40"];
    let (ok, verdict, reason) = discharge(
        &env,
        &valid,
        "deposit",
        "vault_deposit.so",
        true,
        &wrong_len,
    );
    assert!(
        !ok && verdict == "rejected",
        "wrong length: {verdict} ({reason:?})"
    );
}

/// A wrong account index fails inside qedgen, before qedlift runs: the IDL instruction takes
/// one account, so index 1 cannot name it.
#[test]
#[ignore = "needs a built qedsvm checkout and qedlift (see module docs)"]
fn wrong_account_index_fails_before_qedlift() {
    let env = env();
    let valid = spec(env.work.path(), "valid.qedspec", "total += 1");
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("discharge")
        .arg("--spec")
        .arg(&valid)
        .args(["--handler", "deposit", "--account", "vault"])
        .arg("--so")
        .arg(env.work.path().join("vault_deposit.so"))
        .arg("--idl")
        .arg(env.work.path().join("vault.codama.json"))
        .arg("--qedlift")
        .arg(&env.qedlift)
        .args(["--account-data-lengths", "41", "--account-index", "1"])
        .output()
        .expect("spawn qedgen discharge");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("outside --account-data-lengths"),
        "{stderr}"
    );
}

/// `--transition` on qedsvm's `guarded_counter` (#405): a success path and a rejection path
/// (`amount == 0` returns 1). The spec's `requires amount > 0 else ZeroAmount` expects a
/// `zero_amount` trace, so the fixture's `abort` trace is copied to that label.
///
/// Lean must accept every emitted module. qedlift prints per-path kinds only once
/// QEDGen/qedsvm#70 lands; until then the verdict is capped at `incomplete / no_path_outcomes`.
/// A qedlift with the outcome line verifies both paths.
#[test]
#[ignore = "needs a built qedsvm checkout and qedlift (see module docs)"]
fn transition_paths_are_checked_by_lean_and_reconciled_with_the_spec() {
    let env = env();
    let dir = env.work.path();
    let fixtures = env.qedsvm.join("qedsvm-rs/tests/fixtures");
    for (from, to) in [
        ("guarded_counter.so", "guarded_counter.so"),
        ("guarded_counter_success.pcs", "guarded_counter_success.pcs"),
        (
            "guarded_counter_abort.pcs",
            "guarded_counter_zero_amount.pcs",
        ),
    ] {
        std::fs::copy(fixtures.join(from), dir.join(to)).unwrap();
    }
    let spec = dir.join("guarded.qedspec");
    std::fs::write(
        &spec,
        "spec GuardedCounter\n\nstate {\n  amount  : U64\n  counter : U64\n}\n\n\
         type Error\n  | ZeroAmount\n\nhandler credit (amount : U64) {\n  \
         requires amount > 0 else ZeroAmount\n  effect { counter += amount }\n}\n",
    )
    .unwrap();
    // qedgen emits no inline layout, so the shape comes from a small Codama IDL.
    let idl = dir.join("guarded.codama.json");
    std::fs::write(
        &idl,
        r#"{"kind":"rootNode","program":{"kind":"programNode","name":"guardedCounter","accounts":[{"kind":"accountNode","name":"GuardedCounter","data":{"kind":"structTypeNode","fields":[{"kind":"structFieldTypeNode","name":"amount","type":{"kind":"numberTypeNode","format":"u64","endian":"le"}},{"kind":"structFieldTypeNode","name":"counter","type":{"kind":"numberTypeNode","format":"u64","endian":"le"}}]}}],"instructions":[]}}"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(["discharge", "--transition", "--json"])
        .arg("--spec")
        .arg(&spec)
        .args(["--handler", "credit", "--account", "GuardedCounter"])
        .arg("--so")
        .arg(dir.join("guarded_counter.so"))
        .arg("--idl")
        .arg(&idl)
        .arg("--qedlift")
        .arg(&env.qedlift)
        .arg("--lean-project")
        .arg(&env.qedsvm)
        .output()
        .expect("spawn qedgen discharge --transition");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}):\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(report["lean_check"]["status"], "passed", "{report:#}");
    let paths = report["paths"].as_array().unwrap();
    for label in ["success", "zero_amount"] {
        let row = paths.iter().find(|p| p["label"] == label).unwrap();
        assert_eq!(row["expected"], true);
        assert_eq!(row["traced"], true);
    }
    let verdict = report["verdict"].as_str().unwrap();
    match verdict {
        "incomplete" => assert_eq!(report["reason"], "no_path_outcomes", "{report:#}"),
        "verified" => assert_eq!(report["all_expected_paths_verified"], true, "{report:#}"),
        other => panic!("unexpected verdict {other}: {report:#}"),
    }
}
