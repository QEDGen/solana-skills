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

/// Read `qed.lock` from `spec_dir` and check every dependency that
/// carries an `upstream_binary_hash`. Returns one result per dep so the
/// caller can render a complete report (rather than failing on the first
/// mismatch).
///
/// `rpc_url` (if set) is passed through to `solana program dump --url`.
/// `None` lets the Solana CLI use its own configured cluster.
#[allow(dead_code)]
pub fn check_lock(spec_dir: &Path, rpc_url: Option<&str>) -> Result<Vec<DepCheckResult>> {
    let lock = match qed_lock::read(spec_dir)? {
        Some(l) => l,
        None => anyhow::bail!(
            "no qed.lock at {} — run `qedgen check --spec {}` first",
            spec_dir.join(qed_lock::LOCK_FILENAME).display(),
            spec_dir.display(),
        ),
    };
    Ok(check_lock_with_fetcher(
        &lock,
        &mut SolanaCliFetcher { rpc_url },
    ))
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
}
