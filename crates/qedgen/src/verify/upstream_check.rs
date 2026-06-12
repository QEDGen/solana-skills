//! Upstream binary diff — `qedgen verify --check-upstream` (v2.8 G5).
//!
//! Walks `qed.lock`, fetches the on-chain `.so` for every dependency
//! that carries an `upstream_binary_hash` pin, hashes it, and reports
//! mismatches. Per `feedback_dispatch_over_reimplement.md`, the on-chain
//! fetch shells out to the user's `solana` CLI (`solana program dump
//! --url <rpc> <program-id> <tmpfile>`) instead of pulling in
//! `solana-client` — same RPC config the user already has, no new
//! dependency added to qedgen.
//!
//! Per-dependency outcome is one of:
//! - **Match**: on-chain SHA matches the pinned hash.
//! - **Mismatch**: hashes differ — likely a redeploy, a tag pointing
//!   at a different commit, or a tampered lock file.
//! - **Skipped**: dep has no `upstream_binary_hash` (path source, peer
//!   spec, or library entry that hasn't been pinned yet) or is missing
//!   a `program_id` to fetch by.
//! - **Error**: the `solana` CLI failed (network, auth, missing CLI).
//!
//! v2.26 Slice 4c — severity routing. A mismatched pin is no longer a
//! plain stderr warning; it surfaces as a structured [`Finding`] with a
//! severity that depends on the [`Gate`] the call was made from:
//!
//! - `qedgen verify --check-upstream` → mismatch = `Crit`, exits non-zero
//! - `qedgen check --frozen` → mismatch = `P2`, exits zero (warning)
//! - `qedgen check --frozen --strict` → mismatch = `Crit`, exits non-zero
//! - `qedgen verify --check-upstream --upstream-stale-ok` → mismatch
//!   demoted to `Info` (suppressed); exits zero. Intended for offline dev.
//!
//! Network/CLI errors stay non-blocking under every gate — they surface
//! as `P2` so a missing `solana` CLI never silently passes nor falsely
//! gates CI. Only `Mismatch` is severity-routed.
//!
//! ### Test seam — `QEDGEN_UPSTREAM_FAKE_BYTES`
//!
//! v2.26 Track M — `SolanaCliFetcher::fetch` honors the
//! `QEDGEN_UPSTREAM_FAKE_BYTES` env var: when set, the value's UTF-8 bytes
//! are returned in place of the `solana program dump` payload (no shell
//! out). Lets the end-to-end CLI test exercise the full
//! `verify --check-upstream` dispatch path — including the
//! `std::process::exit` codes that the routing layer can't observe — on
//! hosts without the Solana CLI. Production callers never set this env
//! var, so behavior outside tests is unchanged. The value goes through
//! `format_hash` like any other byte payload, so tests compute their
//! pinned hash from the same `format_hash` they're asserting against.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

use crate::qed_lock::{self, LockEntry, LockFile};

/// Env var that overrides `solana program dump` in the production
/// `SolanaCliFetcher`. Used only by `tests/upstream_check_e2e.rs` and the
/// in-process E2E test below. See the module docs for details.
#[allow(dead_code)]
pub const FAKE_BYTES_ENV: &str = "QEDGEN_UPSTREAM_FAKE_BYTES";

/// Result of checking one dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DepCheckOutcome {
    Match {
        program_id: String,
        hash: String,
    },
    Mismatch {
        program_id: String,
        pinned: String,
        on_chain: String,
    },
    /// v2.27 Track D1 — proof_hash drift between the on-disk lock and the
    /// content of the provider's proof package on disk. No network fetch:
    /// the "on-chain" side is `qed_lock::compute_proof_hash` walking the
    /// provider's `.qed/proofs/` directory. Routed through the same
    /// [`Gate`]-aware severity layer as [`DepCheckOutcome::Mismatch`] so
    /// `check --frozen` warns (P2) while `--strict` and `verify` block
    /// (CRIT). Surfaces when a provider's proof package was edited
    /// without re-running `qedgen check` to refresh the lockfile.
    ProofHashMismatch {
        pinned: String,
        computed: String,
    },
    Skipped {
        reason: String,
    },
    Error {
        message: String,
    },
}

/// One row in the report. `name` is the manifest dep key.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DepCheckResult {
    pub name: String,
    pub outcome: DepCheckOutcome,
}

// ----------------------------------------------------------------------------
// v2.26 Slice 4c — severity routing
// ----------------------------------------------------------------------------

/// Verification gate the upstream check is running under. Determines how
/// `Mismatch` outcomes map onto [`FindingSeverity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Gate {
    /// `qedgen verify --check-upstream` — mismatch = Crit, fails.
    Verify,
    /// `qedgen verify --check-upstream --upstream-stale-ok` — mismatch
    /// demoted to Info; exits zero. Offline-dev only.
    VerifyStaleOk,
    /// `qedgen check --frozen` — mismatch = P2 (warning), exits zero.
    CheckFrozen,
    /// `qedgen check --frozen --strict` — mismatch = Crit, fails.
    CheckFrozenStrict,
}

/// Severity assigned to a single [`Finding`] after [`Gate`]-aware routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FindingSeverity {
    /// Verification-gating. Caller exits non-zero.
    Crit,
    /// P2 warning. Surfaces in the report but the caller exits zero.
    P2,
    /// Informational; suppressed by `--upstream-stale-ok` or a clean run.
    Info,
}

/// Structured finding the verify / check command rolls up. One per
/// dependency that had a `Mismatch` or `Error` outcome; clean matches
/// and unpinned skips are summarized separately.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Finding {
    pub name: String,
    pub severity: FindingSeverity,
    pub message: String,
}

/// Result of routing a slate of [`DepCheckResult`]s through a [`Gate`].
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RoutedReport {
    /// One [`Finding`] per Mismatch / Error outcome, with severity routed.
    pub findings: Vec<Finding>,
    /// The original outcomes, preserved so the caller can render skips /
    /// matches alongside the routed findings.
    pub raw: Vec<DepCheckResult>,
}

impl RoutedReport {
    /// True if any finding is severity-CRIT — the caller exits non-zero.
    #[allow(dead_code)]
    pub fn any_blocking(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, FindingSeverity::Crit))
    }

    /// True if any finding is at least P2 — the caller renders the
    /// "warnings present" tail line but does not exit non-zero.
    #[allow(dead_code)]
    pub fn any_warning(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, FindingSeverity::P2))
    }
}

/// Pure routing step. Takes the per-dep outcomes and the [`Gate`] and
/// produces a [`RoutedReport`]. No I/O — the network call already
/// happened in `check_lock_with_fetcher`. Unit-tested below.
#[allow(dead_code)]
pub fn route_findings(results: Vec<DepCheckResult>, gate: Gate) -> RoutedReport {
    let mut findings = Vec::new();
    for r in &results {
        match &r.outcome {
            DepCheckOutcome::Mismatch {
                program_id,
                pinned,
                on_chain,
            } => {
                let severity = match gate {
                    Gate::Verify | Gate::CheckFrozenStrict => FindingSeverity::Crit,
                    Gate::CheckFrozen => FindingSeverity::P2,
                    Gate::VerifyStaleOk => FindingSeverity::Info,
                };
                findings.push(Finding {
                    name: r.name.clone(),
                    severity,
                    message: format!(
                        "binary_hash pin for {} ({}) is stale — pinned {}, on-chain {}",
                        r.name, program_id, pinned, on_chain
                    ),
                });
            }
            DepCheckOutcome::ProofHashMismatch { pinned, computed } => {
                // Track D1 — same severity routing as binary_hash
                // Mismatch. Drift between the on-disk lock and the
                // provider's proof package content is the legible signal
                // that a Stance-2 callee's proofs changed without the
                // consumer rerunning `qedgen check`.
                let severity = match gate {
                    Gate::Verify | Gate::CheckFrozenStrict => FindingSeverity::Crit,
                    Gate::CheckFrozen => FindingSeverity::P2,
                    Gate::VerifyStaleOk => FindingSeverity::Info,
                };
                findings.push(Finding {
                    name: r.name.clone(),
                    severity,
                    message: format!(
                        "proof_hash for {} is stale — pinned {}, computed {}",
                        r.name, pinned, computed
                    ),
                });
            }
            DepCheckOutcome::Error { message } => {
                // Network / CLI errors are never CRIT; we don't want a
                // missing `solana` CLI to gate CI silently. P2 under
                // every gate; demoted to Info under VerifyStaleOk so
                // offline dev runs stay green.
                let severity = match gate {
                    Gate::VerifyStaleOk => FindingSeverity::Info,
                    _ => FindingSeverity::P2,
                };
                findings.push(Finding {
                    name: r.name.clone(),
                    severity,
                    message: format!("upstream fetch failed: {}", message),
                });
            }
            DepCheckOutcome::Match { .. } | DepCheckOutcome::Skipped { .. } => {
                // No finding — caller renders these in the summary tail.
            }
        }
    }
    RoutedReport {
        findings,
        raw: results,
    }
}

/// True if `lock` has at least one entry with a populated
/// `upstream_binary_hash`. `qedgen verify` uses this to auto-enable
/// `--check-upstream` when any pin is present (v2.26 Slice 4c).
#[allow(dead_code)]
pub fn lock_has_pinned_hash(lock: &LockFile) -> bool {
    lock.dependencies.iter().any(|e| {
        e.upstream_binary_hash
            .as_deref()
            .map(|h| !h.is_empty())
            .unwrap_or(false)
    })
}

/// Read `qed.lock` from `spec_dir` and check every dependency that
/// carries an `upstream_binary_hash`. Returns one result per dep so the
/// caller can render a complete report (rather than failing on the first
/// mismatch).
///
/// `rpc_url` (if set) is passed through to `solana program dump --url`.
/// `None` lets the Solana CLI use its own configured cluster. `offline`
/// (v2.8 fold-in F6): when true, any dep that would require an RPC fetch
/// returns `Error { offline-blocked }` instead of shelling out — useful
/// for CI gates that should never reach external network.
#[allow(dead_code)]
pub fn check_lock(
    spec_dir: &Path,
    rpc_url: Option<&str>,
    offline: bool,
) -> Result<Vec<DepCheckResult>> {
    let lock = match qed_lock::read(spec_dir)? {
        Some(l) => l,
        None => anyhow::bail!(
            "no qed.lock at {} — run `qedgen check --spec {}` first",
            spec_dir.join(qed_lock::LOCK_FILENAME).display(),
            spec_dir.display(),
        ),
    };
    if offline {
        Ok(check_lock_with_fetcher(&lock, &mut OfflineFetcher))
    } else {
        Ok(check_lock_with_fetcher(
            &lock,
            &mut SolanaCliFetcher { rpc_url },
        ))
    }
}

/// `--offline` fetcher: unconditionally errors with a clear "offline mode"
/// message. Skipped entries (no hash / no program_id) bypass `fetch`
/// entirely and remain skipped, so an offline run still distinguishes
/// "couldn't reach RPC" from "nothing to verify."
struct OfflineFetcher;

impl BinaryFetcher for OfflineFetcher {
    fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>> {
        anyhow::bail!(
            "offline mode: would have fetched on-chain bytes for {} via `solana program dump`",
            program_id
        )
    }
}

/// Test-friendly seam: the `BinaryFetcher` trait separates the side-effecting
/// "go fetch the on-chain `.so`" step from the pure "compare hashes and
/// build a report" logic. Production uses `SolanaCliFetcher`; tests inject
/// an in-memory fake.
#[allow(dead_code)]
pub trait BinaryFetcher {
    /// Return the raw bytes of the deployed program (the `.so` payload).
    /// Implementations should error cleanly when the network or CLI fails.
    fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>>;
}

/// Production fetcher: shells out to `solana program dump`.
struct SolanaCliFetcher<'a> {
    rpc_url: Option<&'a str>,
}

impl<'a> BinaryFetcher for SolanaCliFetcher<'a> {
    fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>> {
        // v2.26 Track M — the env-var seam lets the E2E test inject a
        // canned payload without shelling out to `solana`. Honored before
        // the temp-file / Command setup so the test path doesn't depend
        // on the Solana CLI being on $PATH. Empty string is treated as
        // "unset" (defensive: `std::env::set_var("X", "")` should not
        // silently mute the production fetcher in test runners).
        if let Ok(payload) = std::env::var(FAKE_BYTES_ENV) {
            if !payload.is_empty() {
                let _ = program_id; // intentionally unused under the seam
                return Ok(payload.into_bytes());
            }
        }
        let tmp = tempfile::Builder::new()
            .prefix("qedgen-program-")
            .suffix(".so")
            .tempfile()
            .context("creating temp file for `solana program dump` output")?;
        let mut cmd = Command::new("solana");
        cmd.arg("program").arg("dump");
        if let Some(url) = self.rpc_url {
            cmd.arg("--url").arg(url);
        }
        cmd.arg(program_id).arg(tmp.path());
        let output = cmd.output().with_context(|| {
            "running `solana program dump` (is the Solana CLI in PATH? install via \
             `sh -c \"$(curl -sSfL https://release.anza.xyz/stable/install)\"`)"
                .to_string()
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "`solana program dump {}` failed: {}",
                program_id,
                stderr.trim()
            );
        }
        let bytes = std::fs::read(tmp.path())
            .with_context(|| format!("reading dumped binary at {}", tmp.path().display()))?;
        Ok(bytes)
    }
}

#[allow(dead_code)]
pub fn check_lock_with_fetcher(
    lock: &LockFile,
    fetcher: &mut dyn BinaryFetcher,
) -> Vec<DepCheckResult> {
    let mut results = Vec::with_capacity(lock.dependencies.len());
    for entry in &lock.dependencies {
        results.push(DepCheckResult {
            name: entry.name.clone(),
            outcome: check_one(entry, fetcher),
        });
    }
    results
}

fn check_one(entry: &LockEntry, fetcher: &mut dyn BinaryFetcher) -> DepCheckOutcome {
    let pinned = match entry.upstream_binary_hash.as_deref() {
        Some(h) if !h.is_empty() => h,
        _ => {
            return DepCheckOutcome::Skipped {
                reason: "no upstream_binary_hash pinned".to_string(),
            }
        }
    };

    // v2.27 Track C3 — `sha256:00…00` is the recognized sentinel for
    // "no payload to pin against". Native programs like the System
    // Program live inside the validator binary itself and aren't
    // returned by `solana program dump` — there's nothing to fetch and
    // nothing to hash. Treating the sentinel as a real pin would
    // produce a confusing Error ("11111111111111111111111111111111 is
    // not an SBF program") instead of a clean skip. Bundled-stdlib
    // interfaces that ship with the sentinel are intentionally Stance-1
    // tautology axioms with no on-chain counterpart; the trust anchor
    // is the runtime, not a content hash.
    if is_sentinel_hash(pinned) {
        return DepCheckOutcome::Skipped {
            reason: "binary_hash sentinel (sha256:00…00) — native program or unverified pin"
                .to_string(),
        };
    }

    // program_id flows from the imported interface's
    // `program_id "..."` declaration into qed.lock at resolution time
    // (v2.8 fold-in F1). Only `None` when the imported interface itself
    // omits the field — purely shape-only Tier 0 imports with no
    // deployed counterpart to verify against.
    let program_id = match resolve_program_id(entry) {
        Some(pid) => pid,
        None => {
            return DepCheckOutcome::Skipped {
                reason: "program_id not pinned (imported interface omits `program_id \"...\"`)"
                    .to_string(),
            }
        }
    };

    let bytes = match fetcher.fetch(&program_id) {
        Ok(b) => b,
        Err(e) => {
            return DepCheckOutcome::Error {
                message: e.to_string(),
            }
        }
    };
    let on_chain = format_hash(&bytes);
    if on_chain == pinned {
        DepCheckOutcome::Match {
            program_id,
            hash: on_chain,
        }
    } else {
        DepCheckOutcome::Mismatch {
            program_id,
            pinned: pinned.to_string(),
            on_chain,
        }
    }
}

/// Pull the program_id from a lock entry. v2.8 fold-in F1: the lock
/// schema now carries `program_id` directly, copied from the imported
/// interface's `program_id "..."` declaration at resolution time. None
/// only when the imported interface itself omits `program_id` (purely
/// shape-only Tier 0 imports without a deployed counterpart).
fn resolve_program_id(entry: &LockEntry) -> Option<String> {
    entry.program_id.clone()
}

fn format_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// v2.27 Track C3 — recognize the "no payload to pin" sentinel.
/// Accepts `sha256:` prefix, any case, and either 64 zero characters
/// (the standard form qedgen emits) or fewer (a degenerate shorthand a
/// user might hand-write). Defensive: stricter matching would let the
/// production fetcher attempt `solana program dump` on entries the
/// author marked as native, producing confusing "not an SBF program"
/// errors instead of a clean skip.
///
/// v2.27 Track D2 fold-in: also called by
/// `check::collect_require_verified_findings` to exempt sentinel-pinned
/// native programs (System) from `--require-verified` — their `ensures`
/// clauses are validated by the validator runtime itself, not by a
/// Lake-buildable proof package. Made `pub(crate)` so the lint reuses the
/// same definition the runtime fetcher trusts.
#[allow(dead_code)]
pub(crate) fn is_sentinel_hash(pinned: &str) -> bool {
    let body = pinned
        .strip_prefix("sha256:")
        .or_else(|| pinned.strip_prefix("SHA256:"))
        .unwrap_or(pinned);
    !body.is_empty() && body.chars().all(|c| c == '0')
}

// ----------------------------------------------------------------------------
// Reporting
// ----------------------------------------------------------------------------

/// v2.26 Slice 4c — render a [`RoutedReport`] with severity-tagged
/// findings. Matches stay informational, mismatches / errors carry the
/// gate-derived severity. Returns true if the caller should exit non-zero
/// (any CRIT finding); otherwise the caller surfaces warnings without
/// gating exit.
#[allow(dead_code)]
pub fn print_routed_report(report: &RoutedReport) -> bool {
    // First render the original per-dep outcomes so the operator sees the
    // skip / match context, then the severity-tagged findings tail.
    for r in &report.raw {
        match &r.outcome {
            DepCheckOutcome::Match { program_id, hash } => {
                eprintln!("  ✓ {} ({}): {}", r.name, program_id, hash);
            }
            DepCheckOutcome::Mismatch { program_id, .. } => {
                eprintln!("  ✗ {} ({}): MISMATCH", r.name, program_id);
            }
            DepCheckOutcome::ProofHashMismatch { .. } => {
                eprintln!("  ✗ {}: PROOF_HASH MISMATCH", r.name);
            }
            DepCheckOutcome::Skipped { reason } => {
                eprintln!("  · {}: skipped — {}", r.name, reason);
            }
            DepCheckOutcome::Error { message } => {
                eprintln!("  ! {}: error — {}", r.name, message);
            }
        }
    }
    for f in &report.findings {
        let tag = match f.severity {
            FindingSeverity::Crit => "CRIT",
            FindingSeverity::P2 => "P2  ",
            FindingSeverity::Info => "INFO",
        };
        eprintln!("  [{tag}] {}: {}", f.name, f.message);
    }
    report.any_blocking()
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qed_lock::{LockEntry, LockFile, LOCK_VERSION};

    /// In-memory fetcher: returns canned bytes per program_id.
    struct FakeFetcher {
        responses: std::collections::HashMap<String, Result<Vec<u8>, String>>,
    }

    impl FakeFetcher {
        fn new() -> Self {
            Self {
                responses: std::collections::HashMap::new(),
            }
        }
        fn ok(mut self, program_id: &str, bytes: Vec<u8>) -> Self {
            self.responses.insert(program_id.to_string(), Ok(bytes));
            self
        }
    }

    impl BinaryFetcher for FakeFetcher {
        fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>> {
            match self.responses.get(program_id) {
                Some(Ok(b)) => Ok(b.clone()),
                Some(Err(e)) => anyhow::bail!("{}", e),
                None => anyhow::bail!("no canned response for {}", program_id),
            }
        }
    }

    fn entry_with_hash(name: &str, hash: Option<&str>) -> LockEntry {
        LockEntry {
            name: name.to_string(),
            source: format!("github:fake/{}", name),
            spec_hash: "sha256:0".to_string(),
            git_ref: Some("v1".to_string()),
            resolved_commit: Some("abc".to_string()),
            path: None,
            program_id: None,
            upstream_binary_hash: hash.map(str::to_string),
            upstream_version: None,
            verified: false,
            proof_hash: None,
            imported_account_type_names: String::new(),
        }
    }

    fn mismatch_result() -> DepCheckResult {
        DepCheckResult {
            name: "spl_token".to_string(),
            outcome: DepCheckOutcome::Mismatch {
                program_id: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".to_string(),
                pinned: "sha256:aaaa".to_string(),
                on_chain: "sha256:bbbb".to_string(),
            },
        }
    }

    fn proof_hash_mismatch_result() -> DepCheckResult {
        DepCheckResult {
            name: "tokenlib".to_string(),
            outcome: DepCheckOutcome::ProofHashMismatch {
                pinned: "sha256:proof_old".to_string(),
                computed: "sha256:proof_new".to_string(),
            },
        }
    }

    fn error_result() -> DepCheckResult {
        DepCheckResult {
            name: "missing".to_string(),
            outcome: DepCheckOutcome::Error {
                message: "solana CLI not in PATH".to_string(),
            },
        }
    }

    fn match_result() -> DepCheckResult {
        DepCheckResult {
            name: "fine".to_string(),
            outcome: DepCheckOutcome::Match {
                program_id: "Tokenkeg".to_string(),
                hash: "sha256:aaaa".to_string(),
            },
        }
    }

    fn skipped_result() -> DepCheckResult {
        DepCheckResult {
            name: "no_pin".to_string(),
            outcome: DepCheckOutcome::Skipped {
                reason: "no upstream_binary_hash pinned".to_string(),
            },
        }
    }

    #[test]
    fn skips_entries_without_pinned_hash() {
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry_with_hash("no_pin", None)],
        };
        let mut fetcher = FakeFetcher::new();
        let results = check_lock_with_fetcher(&lock, &mut fetcher);
        assert_eq!(results.len(), 1);
        match &results[0].outcome {
            DepCheckOutcome::Skipped { reason } => {
                assert!(reason.contains("no upstream_binary_hash"));
            }
            other => panic!("expected Skipped, got {:?}", other),
        }
    }

    // ----- v2.27 Track C3: sentinel-hash skip -----

    #[test]
    fn skips_entries_with_zero_sentinel_hash() {
        // Native programs (e.g. the System Program) have no on-chain
        // SBF payload to fetch. The sentinel `sha256:00…00` marks that
        // case; check_one returns Skipped with a sentinel-explaining
        // reason instead of trying to fetch and erroring.
        let zero = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let mut e = entry_with_hash("system", Some(zero));
        e.program_id = Some("11111111111111111111111111111111".to_string());
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e],
        };
        // Fetcher is never called for sentinel entries; assert that by
        // leaving it empty — a fetch attempt would error and propagate
        // through as DepCheckOutcome::Error.
        let mut fetcher = FakeFetcher::new();
        let results = check_lock_with_fetcher(&lock, &mut fetcher);
        match &results[0].outcome {
            DepCheckOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("sentinel"),
                    "should explain the sentinel skip; got: {reason}"
                );
            }
            other => panic!("expected Skipped (sentinel hash), got {:?}", other),
        }
    }

    #[test]
    fn sentinel_detection_accepts_prefix_and_short_form() {
        // Defensive matching: both `sha256:` and `SHA256:` prefixes,
        // and shorthand of fewer than 64 zeros, still classify as
        // sentinel. Stricter matching risks running the real fetcher
        // on intentionally-unpinned entries.
        assert!(is_sentinel_hash(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        ));
        assert!(is_sentinel_hash("SHA256:0000"));
        assert!(is_sentinel_hash("0000000000000000"));
        assert!(!is_sentinel_hash(
            "sha256:8190d3f7ceb6cb7a7a8d8924bff89f9f611e15ce1f806f2b6237f3311a98f697"
        ));
        // Empty body shouldn't be classified as sentinel — that case
        // is the "no hash pinned" branch.
        assert!(!is_sentinel_hash("sha256:"));
        assert!(!is_sentinel_hash(""));
    }

    // ----- end Track C3 -----

    #[test]
    fn skips_when_imported_interface_omits_program_id() {
        // Lock entry has a hash pin but the imported interface didn't
        // declare `program_id "..."` — pure shape-only Tier 0 import
        // with no deployed counterpart. Skipped honestly.
        let hash = format_hash(b"some bytes");
        let mut e = entry_with_hash("pinned", Some(&hash));
        e.program_id = None; // imported interface had no program_id
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e],
        };
        let mut fetcher = FakeFetcher::new();
        let results = check_lock_with_fetcher(&lock, &mut fetcher);
        match &results[0].outcome {
            DepCheckOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("program_id not pinned"),
                    "should explain that the imported interface lacks program_id; got: {reason}"
                );
            }
            other => panic!("expected Skipped (no program_id), got {:?}", other),
        }
    }

    #[test]
    fn matches_when_program_id_present_and_hash_matches() {
        let bytes = b"qedgen-test-binary".to_vec();
        let hash = format_hash(&bytes);
        let mut e = entry_with_hash("pinned", Some(&hash));
        e.program_id = Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".to_string());
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e],
        };
        let mut fetcher =
            FakeFetcher::new().ok("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", bytes.clone());
        let results = check_lock_with_fetcher(&lock, &mut fetcher);
        match &results[0].outcome {
            DepCheckOutcome::Match {
                program_id,
                hash: h,
            } => {
                assert_eq!(program_id, "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
                assert_eq!(h, &hash);
            }
            other => panic!("expected Match, got {:?}", other),
        }
    }

    #[test]
    fn offline_fetcher_errors_for_pinned_entries_but_skips_cleanly() {
        // One entry has both hash + program_id (would fetch); offline mode
        // converts it to Error. A second entry has no pin → still Skipped.
        let bytes = b"would-have-fetched".to_vec();
        let _ = bytes; // unused — offline never reads
        let mut e_pinned = entry_with_hash("pinned", Some("sha256:abc"));
        e_pinned.program_id = Some("Px11111111111111111111111111111111".to_string());
        let e_unpinned = entry_with_hash("unpinned", None);

        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e_pinned, e_unpinned],
        };
        let mut fetcher = OfflineFetcher;
        let results = check_lock_with_fetcher(&lock, &mut fetcher);
        assert!(matches!(results[0].outcome, DepCheckOutcome::Error { .. }));
        match &results[0].outcome {
            DepCheckOutcome::Error { message } => {
                assert!(
                    message.contains("offline mode"),
                    "should explain why fetch was blocked; got: {message}"
                );
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            results[1].outcome,
            DepCheckOutcome::Skipped { .. }
        ));
    }

    #[test]
    fn mismatches_when_on_chain_differs_from_pinned_hash() {
        let pinned_bytes = b"original-binary".to_vec();
        let on_chain_bytes = b"redeployed-binary".to_vec();
        let mut e = entry_with_hash("pinned", Some(&format_hash(&pinned_bytes)));
        e.program_id = Some("FakeProgramId11111111111111111111111111111111".to_string());
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e],
        };
        let mut fetcher = FakeFetcher::new().ok(
            "FakeProgramId11111111111111111111111111111111",
            on_chain_bytes.clone(),
        );
        let results = check_lock_with_fetcher(&lock, &mut fetcher);
        match &results[0].outcome {
            DepCheckOutcome::Mismatch {
                pinned, on_chain, ..
            } => {
                assert_eq!(pinned, &format_hash(&pinned_bytes));
                assert_eq!(on_chain, &format_hash(&on_chain_bytes));
            }
            other => panic!("expected Mismatch, got {:?}", other),
        }
    }

    #[test]
    fn format_hash_matches_pinned_on_identical_bytes() {
        let bytes = b"qedgen-test-binary-payload".to_vec();
        let hash = format_hash(&bytes);
        assert_eq!(hash, format_hash(&bytes), "deterministic");
        assert!(hash.starts_with("sha256:"));
    }

    // ----------------------------------------------------------------------
    // v2.26 Slice 4c — severity-routing unit tests
    // ----------------------------------------------------------------------

    #[test]
    fn lock_has_pinned_hash_detects_populated_entries() {
        let empty = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry_with_hash("plain", None)],
        };
        assert!(!lock_has_pinned_hash(&empty));

        let with_pin = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![
                entry_with_hash("plain", None),
                entry_with_hash("pinned", Some("sha256:abc")),
            ],
        };
        assert!(lock_has_pinned_hash(&with_pin));

        // Empty-string hash counts as not-pinned (defensive against
        // serde defaulting to "").
        let with_empty = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry_with_hash("empty", Some(""))],
        };
        assert!(!lock_has_pinned_hash(&with_empty));
    }

    #[test]
    fn verify_gate_routes_mismatch_to_crit_and_blocks() {
        let routed = route_findings(vec![mismatch_result()], Gate::Verify);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Crit);
        assert!(routed.any_blocking(), "verify mismatch must gate exit");
        assert!(routed.findings[0].message.contains("stale"));
    }

    #[test]
    fn check_frozen_gate_routes_mismatch_to_p2_and_does_not_block() {
        let routed = route_findings(vec![mismatch_result()], Gate::CheckFrozen);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::P2);
        assert!(
            !routed.any_blocking(),
            "check --frozen mismatch must not exit non-zero"
        );
        assert!(
            routed.any_warning(),
            "check --frozen mismatch surfaces as warning"
        );
    }

    #[test]
    fn check_frozen_strict_routes_mismatch_to_crit_and_blocks() {
        let routed = route_findings(vec![mismatch_result()], Gate::CheckFrozenStrict);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Crit);
        assert!(routed.any_blocking(), "--strict must escalate to CRIT");
    }

    #[test]
    fn verify_stale_ok_demotes_mismatch_to_info_and_does_not_block() {
        let routed = route_findings(vec![mismatch_result()], Gate::VerifyStaleOk);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Info);
        assert!(!routed.any_blocking());
        assert!(
            !routed.any_warning(),
            "--upstream-stale-ok suppresses warnings too — exit fully clean"
        );
    }

    #[test]
    fn fetch_errors_stay_p2_under_verify_and_check() {
        // A missing solana CLI never gates CI silently — it surfaces as
        // P2 under both verify (where it would otherwise be tempting to
        // CRIT it) and check --frozen.
        for gate in [Gate::Verify, Gate::CheckFrozen, Gate::CheckFrozenStrict] {
            let routed = route_findings(vec![error_result()], gate);
            assert_eq!(routed.findings.len(), 1);
            assert_eq!(
                routed.findings[0].severity,
                FindingSeverity::P2,
                "fetch errors must be P2 under {gate:?}"
            );
            assert!(
                !routed.any_blocking(),
                "fetch errors must not gate exit under {gate:?}"
            );
        }
    }

    #[test]
    fn fetch_errors_demoted_to_info_under_stale_ok() {
        let routed = route_findings(vec![error_result()], Gate::VerifyStaleOk);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Info);
    }

    #[test]
    fn matches_and_skips_produce_no_findings() {
        let routed = route_findings(vec![match_result(), skipped_result()], Gate::Verify);
        assert!(routed.findings.is_empty());
        assert!(!routed.any_blocking());
    }

    // ----------------------------------------------------------------------
    // v2.27 Track D1 — proof_hash drift severity routing. Same shape as
    // the binary_hash Mismatch routing: P2 under `check --frozen`, CRIT
    // under `verify` and `--frozen --strict`, Info under
    // `--upstream-stale-ok`.
    // ----------------------------------------------------------------------

    #[test]
    fn proof_hash_drift_routes_crit_under_verify() {
        let routed = route_findings(vec![proof_hash_mismatch_result()], Gate::Verify);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Crit);
        assert!(
            routed.findings[0].message.contains("proof_hash"),
            "message must name the drifted field; got: {}",
            routed.findings[0].message,
        );
        assert!(routed.any_blocking());
    }

    #[test]
    fn proof_hash_drift_routes_p2_under_check_frozen() {
        let routed = route_findings(vec![proof_hash_mismatch_result()], Gate::CheckFrozen);
        assert_eq!(routed.findings[0].severity, FindingSeverity::P2);
        assert!(!routed.any_blocking(), "default --frozen must not block");
        assert!(routed.any_warning());
    }

    #[test]
    fn proof_hash_drift_routes_crit_under_strict_frozen() {
        let routed = route_findings(vec![proof_hash_mismatch_result()], Gate::CheckFrozenStrict);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Crit);
        assert!(routed.any_blocking(), "--strict must escalate to CRIT");
    }

    #[test]
    fn proof_hash_drift_demotes_to_info_under_stale_ok() {
        let routed = route_findings(vec![proof_hash_mismatch_result()], Gate::VerifyStaleOk);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Info);
        assert!(!routed.any_blocking());
        assert!(!routed.any_warning());
    }

    #[test]
    fn print_routed_report_returns_blocking_for_crit_only() {
        let routed = route_findings(vec![mismatch_result()], Gate::Verify);
        assert!(routed.any_blocking());

        let routed_p2 = route_findings(vec![mismatch_result()], Gate::CheckFrozen);
        assert!(!routed_p2.any_blocking());
    }

    // ----------------------------------------------------------------------
    // v2.26 Track M — end-to-end test through `check_lock` + the env-var
    // seam. Drives the full lock-read → fetch → hash → route → render
    // pipeline that the CLI dispatch path uses, without shelling out to
    // `solana program dump`. Complements the pure routing unit tests
    // above (which feed synthetic `DepCheckResult`s) and the CLI-level
    // exit-code test in `tests/upstream_check_e2e.rs`.
    // ----------------------------------------------------------------------

    /// Serialize tests that touch `QEDGEN_UPSTREAM_FAKE_BYTES`. `cargo
    /// test` runs cases in parallel by default; env-var mutation is
    /// process-global, so without this mutex one test could observe
    /// another's fake payload (or set/unset race).
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// RAII guard: install `QEDGEN_UPSTREAM_FAKE_BYTES=payload` for the
    /// duration of the test, unset on drop. Acquires `env_lock` so
    /// parallel test cases don't trample each other.
    struct FakeBytesGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl FakeBytesGuard {
        fn set(payload: &str) -> Self {
            let lock = env_lock().lock().unwrap_or_else(|e| e.into_inner());
            std::env::set_var(FAKE_BYTES_ENV, payload);
            Self { _lock: lock }
        }
    }
    impl Drop for FakeBytesGuard {
        fn drop(&mut self) {
            std::env::remove_var(FAKE_BYTES_ENV);
        }
    }

    /// Write a minimal `qed.lock` with one pinned dep into `dir` and
    /// return the program_id used. The pinned hash is `format_hash` of
    /// the supplied `pinned_payload`; the caller controls whether the
    /// fetcher's response (via the env var) matches or differs.
    fn write_lock_with_pin(dir: &Path, pinned_payload: &[u8]) -> String {
        let program_id = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".to_string();
        let pinned_hash = format_hash(pinned_payload);
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                name: "spl".to_string(),
                source: "builtin:spl".to_string(),
                spec_hash: "sha256:0".to_string(),
                git_ref: None,
                resolved_commit: None,
                path: None,
                program_id: Some(program_id.clone()),
                upstream_binary_hash: Some(pinned_hash),
                upstream_version: Some("4.0.3".to_string()),
                verified: false,
                proof_hash: None,
                imported_account_type_names: String::new(),
            }],
        };
        crate::qed_lock::write(dir, &lock).expect("write qed.lock");
        program_id
    }

    /// Mismatch under `Gate::Verify` produces a CRIT finding that gates
    /// exit (`any_blocking()` is true). The pinned payload differs from
    /// the env-var-injected on-chain payload, so the fetcher reports a
    /// hash that doesn't match the lock.
    #[test]
    fn e2e_verify_mismatch_is_crit_and_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _pid = write_lock_with_pin(dir.path(), b"pinned-bytes-v1");
        let _g = FakeBytesGuard::set("on-chain-bytes-v2");

        let results = check_lock(dir.path(), None, false).expect("check_lock");
        let routed = route_findings(results, Gate::Verify);
        assert_eq!(routed.findings.len(), 1, "one finding for one dep");
        assert_eq!(
            routed.findings[0].severity,
            FindingSeverity::Crit,
            "verify mismatch must be CRIT"
        );
        assert!(routed.any_blocking(), "CRIT must gate exit");
    }

    /// Same mismatch under `Gate::CheckFrozen` lowers severity to P2 —
    /// surfaces in the report but does not gate exit. This is the
    /// non-strict `check --frozen` behavior.
    #[test]
    fn e2e_check_frozen_mismatch_is_p2_and_does_not_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _pid = write_lock_with_pin(dir.path(), b"pinned-bytes-v1");
        let _g = FakeBytesGuard::set("on-chain-bytes-v2");

        let results = check_lock(dir.path(), None, false).expect("check_lock");
        let routed = route_findings(results, Gate::CheckFrozen);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::P2);
        assert!(!routed.any_blocking(), "check --frozen must not block");
        assert!(routed.any_warning(), "P2 must surface as warning");
    }

    /// `check --frozen --strict` escalates the same mismatch back to CRIT.
    #[test]
    fn e2e_check_frozen_strict_escalates_to_crit_and_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _pid = write_lock_with_pin(dir.path(), b"pinned-bytes-v1");
        let _g = FakeBytesGuard::set("on-chain-bytes-v2");

        let results = check_lock(dir.path(), None, false).expect("check_lock");
        let routed = route_findings(results, Gate::CheckFrozenStrict);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Crit);
        assert!(routed.any_blocking(), "--strict must gate exit");
    }

    /// `--upstream-stale-ok` demotes the mismatch finding to Info; the
    /// report still records it but neither blocks nor counts as a
    /// warning. Offline-dev escape hatch.
    #[test]
    fn e2e_verify_stale_ok_demotes_to_info() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _pid = write_lock_with_pin(dir.path(), b"pinned-bytes-v1");
        let _g = FakeBytesGuard::set("on-chain-bytes-v2");

        let results = check_lock(dir.path(), None, false).expect("check_lock");
        let routed = route_findings(results, Gate::VerifyStaleOk);
        assert_eq!(routed.findings.len(), 1);
        assert_eq!(routed.findings[0].severity, FindingSeverity::Info);
        assert!(!routed.any_blocking());
        assert!(!routed.any_warning(), "stale-ok suppresses warnings too");
    }

    /// When the on-chain bytes hash to the pinned value, no finding is
    /// produced under any gate and the run exits clean. Sanity-check
    /// that the env var doesn't manufacture findings out of thin air.
    #[test]
    fn e2e_match_under_any_gate_produces_no_finding() {
        let bytes = b"matching-bytes";
        let dir = tempfile::tempdir().expect("tempdir");
        let _pid = write_lock_with_pin(dir.path(), bytes);
        let _g = FakeBytesGuard::set(std::str::from_utf8(bytes).unwrap());

        let results = check_lock(dir.path(), None, false).expect("check_lock");
        assert!(
            matches!(results[0].outcome, DepCheckOutcome::Match { .. }),
            "fetcher payload hashes to pinned value → Match",
        );
        for gate in [
            Gate::Verify,
            Gate::CheckFrozen,
            Gate::CheckFrozenStrict,
            Gate::VerifyStaleOk,
        ] {
            let routed = route_findings(results.clone(), gate);
            assert!(
                routed.findings.is_empty(),
                "matching pin must produce no finding under {gate:?}",
            );
            assert!(!routed.any_blocking());
        }
    }
}
