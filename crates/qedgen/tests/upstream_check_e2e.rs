//! End-to-end test for `qedgen verify --check-upstream` (v2.26 Track M).
//!
//! Drives the actual `qedgen` binary through the full CLI dispatch path
//! (clap parse → `Verify` arm → `route_findings` → `print_routed_report`
//! → `std::process::exit`), with the on-chain fetch shimmed via the
//! `QEDGEN_UPSTREAM_FAKE_BYTES` env var. Asserts:
//!
//! - exit code propagation per gate (CRIT exits non-zero; P2/Info exit
//!   zero),
//! - CLI flag wiring (`--strict`, `--upstream-stale-ok`) takes effect.
//!
//! Complements the in-process tests in `upstream_check::tests::e2e_*`
//! (which assert the routing layer on real `check_lock` output) by
//! covering the parts those tests can't reach: `std::process::exit`
//! codes and CLI flag parsing.
//!
//! Marked `#[ignore]` to match the precedent set by `codegen_smoke.rs` —
//! the test takes a beat (binary spawn) and isn't necessary in the
//! default `cargo test` fast path. Run with:
//!
//! ```bash
//! cargo test --release --test upstream_check_e2e -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// Env var that overrides `solana program dump` inside
/// `SolanaCliFetcher::fetch`. Must match `upstream_check::FAKE_BYTES_ENV`.
/// Kept as a literal here (not imported via `pub use`) so the
/// integration test reads as a black-box CLI invocation rather than
/// peeking at internal symbols.
const FAKE_BYTES_ENV: &str = "QEDGEN_UPSTREAM_FAKE_BYTES";

const FAKE_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Hex sha256 prefix that `upstream_check::format_hash` emits.
/// Computed below so tests don't need to hard-code the digest.
fn format_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}

/// Write a minimal project fixture into `dir`:
/// - `git init` (qedgen requires a git repo),
/// - empty `<dir>/program.qedspec` so `--spec` resolves to a real file,
/// - `<dir>/qed.lock` declaring one pinned dep whose hash matches
///   `pinned_payload`.
///
/// The qedspec body is intentionally empty: `verify --check-upstream`
/// alone reads only `qed.lock` and exits before any spec parsing
/// happens. If that ever changes, this fixture will need a real spec.
fn write_fixture(dir: &Path, pinned_payload: &[u8]) -> PathBuf {
    let spec_path = dir.join("program.qedspec");
    std::fs::write(&spec_path, "").expect("write empty spec stub");

    // `verify` calls `require_git_repo()` before reading anything, so
    // `git init` is mandatory — even though we don't commit.
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(dir)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");

    let pinned_hash = format_hash(pinned_payload);
    let lock_body = format!(
        r#"version = 1

[[dependency]]
name = "spl"
source = "builtin:spl"
spec_hash = "sha256:0"
program_id = "{program_id}"
upstream_binary_hash = "{pinned_hash}"
upstream_version = "4.0.3"
"#,
        program_id = FAKE_PROGRAM_ID,
        pinned_hash = pinned_hash,
    );
    std::fs::write(dir.join("qed.lock"), lock_body).expect("write qed.lock");

    spec_path
}

/// Spawn `qedgen verify --check-upstream …` with the env-var seam set,
/// return the captured `(status, stderr_lossy)`. `extra_args` lets the
/// individual cases tack on `--strict` / `--upstream-stale-ok`.
fn run_verify(
    spec: &Path,
    on_chain_payload: &str,
    extra_args: &[&str],
) -> (std::process::ExitStatus, String) {
    let bin = env!("CARGO_BIN_EXE_qedgen");
    let mut cmd = Command::new(bin);
    cmd.arg("verify")
        .arg("--check-upstream")
        .arg("--spec")
        .arg(spec)
        .env(FAKE_BYTES_ENV, on_chain_payload);
    for a in extra_args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("spawn qedgen verify");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status, stderr)
}

/// Mismatch under plain `--check-upstream` → CRIT → exit 1.
#[test]
#[ignore = "runs the qedgen binary; requires CARGO_BIN_EXE_qedgen"]
fn verify_check_upstream_mismatch_exits_non_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec = write_fixture(tmp.path(), b"pinned-bytes-v1");
    let (status, stderr) = run_verify(&spec, "on-chain-bytes-v2", &[]);
    assert!(
        !status.success(),
        "CRIT mismatch must exit non-zero (status: {:?}, stderr:\n{})",
        status,
        stderr,
    );
    assert!(
        stderr.contains("[CRIT]"),
        "expected CRIT finding in stderr, got:\n{}",
        stderr,
    );
}

/// `--upstream-stale-ok` suppresses the gate — even on mismatch, exit 0.
#[test]
#[ignore = "runs the qedgen binary; requires CARGO_BIN_EXE_qedgen"]
fn verify_check_upstream_with_stale_ok_exits_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec = write_fixture(tmp.path(), b"pinned-bytes-v1");
    let (status, stderr) = run_verify(&spec, "on-chain-bytes-v2", &["--upstream-stale-ok"]);
    assert!(
        status.success(),
        "--upstream-stale-ok must exit zero even on mismatch (status: {:?}, stderr:\n{})",
        status,
        stderr,
    );
    // The dispatch emits a breadcrumb that the suppression won.
    assert!(
        stderr.contains("--upstream-stale-ok suppressed"),
        "expected suppression breadcrumb in stderr, got:\n{}",
        stderr,
    );
}

/// Match → no finding → exit 0 cleanly.
#[test]
#[ignore = "runs the qedgen binary; requires CARGO_BIN_EXE_qedgen"]
fn verify_check_upstream_match_exits_zero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bytes = "matching-bytes";
    let spec = write_fixture(tmp.path(), bytes.as_bytes());
    let (status, stderr) = run_verify(&spec, bytes, &[]);
    assert!(
        status.success(),
        "matching pin must exit zero (status: {:?}, stderr:\n{})",
        status,
        stderr,
    );
    assert!(
        !stderr.contains("[CRIT]"),
        "matching pin must not emit CRIT, stderr:\n{}",
        stderr,
    );
}

/// `qedgen check --frozen` with the same mismatch is non-blocking by
/// default (P2 warning, exit zero) and CRIT under `--strict` (exit 1).
/// Covers the `check` arm of the differentiated-gate model.
#[test]
#[ignore = "runs the qedgen binary; requires CARGO_BIN_EXE_qedgen"]
fn check_frozen_mismatch_is_warning_strict_escalates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spec = write_fixture(tmp.path(), b"pinned-bytes-v1");

    // Non-strict: P2, exit zero. NB — `check --frozen` does more than
    // upstream-diffing (spec resolution, etc.), and an empty spec stub
    // will fail those steps. We treat the upstream surface as a
    // necessary-but-not-sufficient check: stderr must contain `[P2]`
    // and must NOT contain `[CRIT]`. Exit code is whatever `check`
    // produces for the stub spec.
    let bin = env!("CARGO_BIN_EXE_qedgen");
    let out = Command::new(bin)
        .arg("check")
        .arg("--frozen")
        .arg("--spec")
        .arg(&spec)
        .env(FAKE_BYTES_ENV, "on-chain-bytes-v2")
        .output()
        .expect("spawn qedgen check --frozen");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // `print_routed_report` pads severity tags to 4 chars for column
    // alignment, so the rendered form is `[P2  ]` / `[CRIT]` / `[INFO]`.
    assert!(
        stderr.contains("[P2"),
        "check --frozen mismatch should emit a P2 finding, got:\n{}",
        stderr,
    );
    assert!(
        !stderr.contains("[CRIT]"),
        "check --frozen (non-strict) must not emit [CRIT], got:\n{}",
        stderr,
    );

    // Strict: CRIT, exit non-zero. Same caveat about non-upstream
    // failures — we only assert the upstream tag escalated to CRIT.
    let out = Command::new(bin)
        .arg("check")
        .arg("--frozen")
        .arg("--strict")
        .arg("--spec")
        .arg(&spec)
        .env(FAKE_BYTES_ENV, "on-chain-bytes-v2")
        .output()
        .expect("spawn qedgen check --frozen --strict");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[CRIT]"),
        "check --frozen --strict mismatch must escalate to [CRIT], got:\n{}",
        stderr,
    );
}
