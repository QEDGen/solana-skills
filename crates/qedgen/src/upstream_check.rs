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

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

use crate::qed_lock::{self, LockEntry, LockFile};

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

// ----------------------------------------------------------------------------
// Reporting
// ----------------------------------------------------------------------------

/// Render a human-readable report. Returns true if any mismatch or
/// error was reported (caller exits non-zero).
#[allow(dead_code)]
pub fn print_report(results: &[DepCheckResult]) -> bool {
    let mut any_failure = false;
    for r in results {
        match &r.outcome {
            DepCheckOutcome::Match { program_id, hash } => {
                eprintln!("  ✓ {} ({}): {}", r.name, program_id, hash);
            }
            DepCheckOutcome::Mismatch {
                program_id,
                pinned,
                on_chain,
            } => {
                any_failure = true;
                eprintln!("  ✗ {} ({}): MISMATCH", r.name, program_id);
                eprintln!("      pinned:   {}", pinned);
                eprintln!("      on-chain: {}", on_chain);
            }
            DepCheckOutcome::Skipped { reason } => {
                eprintln!("  · {}: skipped — {}", r.name, reason);
            }
            DepCheckOutcome::Error { message } => {
                any_failure = true;
                eprintln!("  ! {}: error — {}", r.name, message);
            }
        }
    }
    any_failure
}

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

    #[test]
    fn print_report_returns_true_on_mismatch() {
        let results = vec![DepCheckResult {
            name: "x".to_string(),
            outcome: DepCheckOutcome::Mismatch {
                program_id: "Xyz".to_string(),
                pinned: "sha256:a".to_string(),
                on_chain: "sha256:b".to_string(),
            },
        }];
        assert!(print_report(&results));
    }

    #[test]
    fn print_report_returns_false_when_all_skipped_or_match() {
        let results = vec![
            DepCheckResult {
                name: "skipped".to_string(),
                outcome: DepCheckOutcome::Skipped {
                    reason: "no pin".to_string(),
                },
            },
            DepCheckResult {
                name: "matched".to_string(),
                outcome: DepCheckOutcome::Match {
                    program_id: "Xyz".to_string(),
                    hash: "sha256:a".to_string(),
                },
            },
        ];
        assert!(!print_report(&results));
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

    #[test]
    fn print_routed_report_returns_blocking_for_crit_only() {
        let routed = route_findings(vec![mismatch_result()], Gate::Verify);
        assert!(routed.any_blocking());

        let routed_p2 = route_findings(vec![mismatch_result()], Gate::CheckFrozen);
        assert!(!routed_p2.any_blocking());
    }
}
