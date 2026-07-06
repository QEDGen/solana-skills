//! Upstream binary diff — `qedgen verify --check-upstream`.
//!
//! Walks `qed.lock`, fetches the on-chain `.so` for every dep with an
//! `upstream_binary_hash` pin, hashes it, and reports mismatches. The fetch
//! shells out to the user's `solana` CLI (`solana program dump`) rather than
//! pulling in `solana-client` — same RPC config, no new dependency.
//!
//! Per-dep outcome: **Match**, **Mismatch** (redeploy / retagged commit /
//! tampered lock), **Skipped** (no pin or no `program_id`), or **Error**
//! (`solana` CLI failed).
//!
//! Severity routing — a mismatch surfaces as a [`Finding`] whose severity
//! depends on the [`Gate`]:
//! - `verify --check-upstream` → `Crit`, exits non-zero
//! - `check --frozen` → `P2`, exits zero (warning)
//! - `check --frozen --strict` → `Crit`, exits non-zero
//! - `verify --check-upstream --upstream-stale-ok` → `Info` (suppressed),
//!   exits zero; for offline dev
//!
//! Network/CLI errors stay non-blocking (`P2`) under every gate so a missing
//! `solana` CLI never silently passes nor falsely gates CI.
//!
//! Test seam: `QEDGEN_UPSTREAM_FAKE_BYTES` makes `SolanaCliFetcher::fetch`
//! return the var's UTF-8 bytes instead of shelling out, so E2E tests can
//! drive the full dispatch path (including exit codes) without the Solana
//! CLI. The payload goes through `format_hash` like any other bytes.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::qed_lock::{self, LockEntry, LockFile};

/// Env var overriding `solana program dump` in `SolanaCliFetcher`;
/// test-only (see module docs).
pub const FAKE_BYTES_ENV: &str = "QEDGEN_UPSTREAM_FAKE_BYTES";

/// Result of checking one dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// proof_hash drift between the lock and the provider's `.qed/proofs/`
    /// content (`qed_lock::compute_proof_hash`; no network). Same
    /// [`Gate`]-aware severity routing as `Mismatch`. Surfaces when a
    /// provider's proof package changed without re-running `qedgen check`.
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
pub struct DepCheckResult {
    pub name: String,
    pub outcome: DepCheckOutcome,
}

// ----------------------------------------------------------------------------
// Severity routing
// ----------------------------------------------------------------------------

/// Verification gate the upstream check runs under; maps `Mismatch` outcomes
/// onto [`FindingSeverity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// `qedgen verify --check-upstream` — mismatch = Crit, fails.
    Verify,
    /// `qedgen verify --check-upstream --upstream-stale-ok` — mismatch
    /// demoted to Info; exits zero. Offline-dev only. Today `run.rs` skips
    /// the check entirely under the flag; the demotion routing stays tested
    /// for when the report is rendered anyway.
    #[cfg_attr(not(test), allow(dead_code))]
    VerifyStaleOk,
    /// `qedgen check --frozen` — mismatch = P2 (warning), exits zero.
    CheckFrozen,
    /// `qedgen check --frozen --strict` — mismatch = Crit, fails.
    CheckFrozenStrict,
}

/// Severity assigned to a single [`Finding`] after [`Gate`]-aware routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingSeverity {
    /// Verification-gating. Caller exits non-zero.
    Crit,
    /// P2 warning. Surfaces in the report but the caller exits zero.
    P2,
    /// Informational; suppressed by `--upstream-stale-ok` or a clean run.
    Info,
}

/// One finding per `Mismatch` / `Error` dep; matches and skips are
/// summarized separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub name: String,
    pub severity: FindingSeverity,
    pub message: String,
}

/// Result of routing a slate of [`DepCheckResult`]s through a [`Gate`].
#[derive(Debug, Clone)]
pub struct RoutedReport {
    /// One [`Finding`] per Mismatch / Error outcome, with severity routed.
    pub findings: Vec<Finding>,
    /// The original outcomes, preserved so the caller can render skips /
    /// matches alongside the routed findings.
    pub raw: Vec<DepCheckResult>,
}

impl RoutedReport {
    /// True if any finding is severity-CRIT — the caller exits non-zero.
    pub fn any_blocking(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, FindingSeverity::Crit))
    }

    /// True if any finding is P2 — caller renders the warnings tail line
    /// but exits zero.
    #[cfg_attr(not(test), allow(dead_code))] // production renders findings individually; kept as the test-side summary
    pub fn any_warning(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, FindingSeverity::P2))
    }
}

/// Pure routing step (no I/O — fetching already happened in
/// `check_lock_with_fetcher`).
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
                // Same severity routing as binary_hash Mismatch.
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
                // Network / CLI errors are never CRIT (a missing `solana` CLI
                // must not gate CI): P2 everywhere, Info under VerifyStaleOk.
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

/// True if `lock` has any populated `upstream_binary_hash`; `qedgen verify`
/// uses this to auto-enable `--check-upstream`.
pub fn lock_has_pinned_hash(lock: &LockFile) -> bool {
    lock.dependencies.iter().any(|e| {
        e.upstream_binary_hash
            .as_deref()
            .map(|h| !h.is_empty())
            .unwrap_or(false)
    })
}

/// Read `qed.lock` from `spec_dir` and check every pinned dependency.
/// One result per dep (full report, not fail-fast).
///
/// `rpc_url` flows to `solana program dump --url`; `None` uses the CLI's
/// configured cluster. `offline`: deps needing an RPC fetch return
/// `Error { offline-blocked }` instead of shelling out (for network-free CI).
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
    // A1: stash verified bytes for later discharge. Skip under the test seam —
    // `QEDGEN_UPSTREAM_FAKE_BYTES` returns synthetic payloads that aren't real
    // programs worth caching (and keeps unit tests from touching the home dir).
    let under_fake_seam = std::env::var(FAKE_BYTES_ENV).is_ok_and(|v| !v.is_empty());
    let elf_cache = if under_fake_seam {
        None
    } else {
        elf_cache_root().ok()
    };
    if offline {
        Ok(check_lock_with_fetcher(
            &lock,
            &mut OfflineFetcher,
            elf_cache.as_deref(),
        ))
    } else {
        Ok(check_lock_with_fetcher(
            &lock,
            &mut SolanaCliFetcher { rpc_url },
            elf_cache.as_deref(),
        ))
    }
}

/// `--offline` fetcher: always errors with an "offline mode" message.
/// Skipped entries never reach `fetch`, so offline runs still distinguish
/// "couldn't reach RPC" from "nothing to verify".
struct OfflineFetcher;

impl BinaryFetcher for OfflineFetcher {
    fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>> {
        anyhow::bail!(
            "offline mode: would have fetched on-chain bytes for {} via `solana program dump`",
            program_id
        )
    }
}

/// Test seam separating the side-effecting fetch from the pure hash-compare
/// logic. Production uses `SolanaCliFetcher`; tests inject an in-memory fake.
pub trait BinaryFetcher {
    /// Raw bytes of the deployed program (`.so` payload); error cleanly on
    /// network/CLI failure.
    fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>>;
}

/// Production fetcher: shells out to `solana program dump`.
struct SolanaCliFetcher<'a> {
    rpc_url: Option<&'a str>,
}

impl<'a> BinaryFetcher for SolanaCliFetcher<'a> {
    fn fetch(&mut self, program_id: &str) -> Result<Vec<u8>> {
        // Test seam: checked before any temp-file / Command setup so the test
        // path never needs the Solana CLI. Empty string = "unset" (defensive
        // against set_var("X", "") muting the production fetcher).
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

pub fn check_lock_with_fetcher(
    lock: &LockFile,
    fetcher: &mut dyn BinaryFetcher,
    elf_cache: Option<&Path>,
) -> Vec<DepCheckResult> {
    let mut results = Vec::with_capacity(lock.dependencies.len());
    for entry in &lock.dependencies {
        results.push(DepCheckResult {
            name: entry.name.clone(),
            outcome: check_one(entry, fetcher, elf_cache),
        });
    }
    results
}

fn check_one(
    entry: &LockEntry,
    fetcher: &mut dyn BinaryFetcher,
    elf_cache: Option<&Path>,
) -> DepCheckOutcome {
    let pinned = match entry.upstream_binary_hash.as_deref() {
        Some(h) if !h.is_empty() => h,
        _ => {
            return DepCheckOutcome::Skipped {
                reason: "no upstream_binary_hash pinned".to_string(),
            }
        }
    };

    // `sha256:00…00` = "no payload to pin against" sentinel. Native programs
    // (e.g. System) live in the validator binary and aren't dumpable;
    // treating the sentinel as a real pin would yield a confusing "not an SBF
    // program" Error instead of a clean skip. Sentinel-shipping bundled
    // interfaces trust the runtime, not a content hash.
    if is_sentinel_hash(pinned) {
        return DepCheckOutcome::Skipped {
            reason: "binary_hash sentinel (sha256:00…00) — native program or unverified pin"
                .to_string(),
        };
    }

    // program_id flows from the imported interface's `program_id "..."` into
    // qed.lock at resolution time; `None` = shape-only Tier 0 import with no
    // deployed counterpart.
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
        // A1: cache the just-verified bytes content-addressed by their hash
        // (== the pin on this branch) so a later `qedsvm_discharge` reads them
        // without re-dumping. Best-effort — a cache-write failure must never
        // turn a clean Match into a gating error.
        if let Some(root) = elf_cache {
            if let Err(e) = stash_elf_in(root, &on_chain, &bytes) {
                eprintln!(
                    "  · note: could not cache verified ELF for {} ({}): {}",
                    program_id, on_chain, e
                );
            }
        }
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

/// program_id from a lock entry (copied from the imported interface at
/// resolution time); `None` = shape-only Tier 0 import.
fn resolve_program_id(entry: &LockEntry) -> Option<String> {
    entry.program_id.clone()
}

fn format_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// Recognize the "no payload to pin" sentinel. Accepts `sha256:`/`SHA256:`
/// prefix and any all-zero body (64 zeros standard, fewer as hand-written
/// shorthand) — stricter matching would send native-marked entries to the
/// real fetcher and surface "not an SBF program" errors.
///
/// Also used by `check::collect_require_verified_findings` to exempt
/// sentinel-pinned native programs from `--require-verified` (their `ensures`
/// are validated by the runtime, not a proof package); `pub(crate)` so the
/// lint reuses the same definition.
pub(crate) fn is_sentinel_hash(pinned: &str) -> bool {
    let body = pinned
        .strip_prefix("sha256:")
        .or_else(|| pinned.strip_prefix("SHA256:"))
        .unwrap_or(pinned);
    !body.is_empty() && body.chars().all(|c| c == '0')
}

// ----------------------------------------------------------------------------
// ELF cache (discharge Slice A / A1)
// ----------------------------------------------------------------------------
//
// `--check-upstream` already dumps + hashes the deployed `.so`; today it drops
// the bytes after the compare. The discharge pipeline (docs/design/
// qedsvm-discharge.md §6.1, §14-A1) needs those exact bytes to decode the ELF
// at proof time. Stash them content-addressed by the verified `binary_hash` —
// the same pin already in `qed.lock` / the `.qedspec` `upstream {}` block — so a
// later `qedsvm_discharge` reads what `--check-upstream` proved, with no second
// `solana program dump`. Caching is a pure optimization: every failure is
// non-fatal and degrades to a re-fetch.

/// Root of the content-addressed ELF cache: an `elf-cache/` sibling of the
/// validation workspace under the qedgen home (`~/.qedgen/elf-cache`, override
/// via `QEDGEN_HOME`).
pub(crate) fn elf_cache_root() -> Result<PathBuf> {
    Ok(crate::validate::qedgen_home()?.join("elf-cache"))
}

/// Map a `sha256:<hex>` pin to its file path under `root`. The body must be
/// pure hex (what [`format_hash`] emits); anything else is rejected so a
/// malformed pin can never escape the cache directory via `..`/separators.
pub(crate) fn cached_elf_path_in(root: &Path, hash: &str) -> Result<PathBuf> {
    let body = hash
        .strip_prefix("sha256:")
        .or_else(|| hash.strip_prefix("SHA256:"))
        .unwrap_or(hash);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("refusing to cache ELF under non-hex binary_hash {:?}", hash);
    }
    Ok(root.join(format!("{}.so", body.to_ascii_lowercase())))
}

/// Stash `bytes` content-addressed by `hash` under `root`, written atomically
/// (temp file + rename) so a concurrent reader never sees a torn file.
/// Idempotent: the path is content-addressed, so an existing entry is the same
/// bytes by construction and the write is skipped. Returns the cached path.
pub(crate) fn stash_elf_in(root: &Path, hash: &str, bytes: &[u8]) -> Result<PathBuf> {
    use std::io::Write;
    let path = cached_elf_path_in(root, hash)?;
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(root)
        .with_context(|| format!("creating ELF cache dir {}", root.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".qedgen-elf-")
        .tempfile_in(root)
        .with_context(|| format!("creating temp file in ELF cache {}", root.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("writing ELF bytes to {}", tmp.path().display()))?;
    tmp.flush().ok();
    tmp.persist(&path)
        .map_err(|e| anyhow::anyhow!("persisting cached ELF to {}: {}", path.display(), e))?;
    Ok(path)
}

/// Read the cached ELF for `hash` under `root`, or `None` if absent/unreadable.
/// The read side the discharge step consumes. Explicitly re-deferred in
/// v2.40 (#124): `qedgen discharge` takes `--so` directly today; wiring the
/// cache read means discharging against the *pinned upstream* binary by
/// `binary_hash` — do it when the aggregate trust report folds in per-handler
/// discharge verdicts (the same consumer).
pub(crate) fn read_cached_elf_in(root: &Path, hash: &str) -> Option<Vec<u8>> {
    std::fs::read(cached_elf_path_in(root, hash).ok()?).ok()
}

/// [`read_cached_elf_in`] against the default [`elf_cache_root`].
#[allow(dead_code)] // staged: re-deferred in v2.40 (#124) — see doc comment on read_cached_elf_in
pub(crate) fn read_cached_elf(hash: &str) -> Option<Vec<u8>> {
    read_cached_elf_in(&elf_cache_root().ok()?, hash)
}

// ----------------------------------------------------------------------------
// Reporting
// ----------------------------------------------------------------------------

/// Render a [`RoutedReport`]: per-dep outcomes first (skip / match context),
/// then the severity-tagged findings tail. Returns true if the caller should
/// exit non-zero (any CRIT).
pub fn print_routed_report(report: &RoutedReport) -> bool {
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
        let results = check_lock_with_fetcher(&lock, &mut fetcher, None);
        assert_eq!(results.len(), 1);
        match &results[0].outcome {
            DepCheckOutcome::Skipped { reason } => {
                assert!(reason.contains("no upstream_binary_hash"));
            }
            other => panic!("expected Skipped, got {:?}", other),
        }
    }

    #[test]
    fn skips_entries_with_zero_sentinel_hash() {
        // Sentinel-pinned native programs are Skipped, never fetched.
        let zero = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let mut e = entry_with_hash("system", Some(zero));
        e.program_id = Some("11111111111111111111111111111111".to_string());
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e],
        };
        // Empty fetcher: any fetch attempt would surface as Error.
        let mut fetcher = FakeFetcher::new();
        let results = check_lock_with_fetcher(&lock, &mut fetcher, None);
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
        assert!(is_sentinel_hash(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        ));
        assert!(is_sentinel_hash("SHA256:0000"));
        assert!(is_sentinel_hash("0000000000000000"));
        assert!(!is_sentinel_hash(
            "sha256:8190d3f7ceb6cb7a7a8d8924bff89f9f611e15ce1f806f2b6237f3311a98f697"
        ));
        // Empty body = "no hash pinned" branch, not sentinel.
        assert!(!is_sentinel_hash("sha256:"));
        assert!(!is_sentinel_hash(""));
    }

    #[test]
    fn skips_when_imported_interface_omits_program_id() {
        // Hash pin but no `program_id` (shape-only Tier 0 import) → Skipped.
        let hash = format_hash(b"some bytes");
        let mut e = entry_with_hash("pinned", Some(&hash));
        e.program_id = None; // imported interface had no program_id
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![e],
        };
        let mut fetcher = FakeFetcher::new();
        let results = check_lock_with_fetcher(&lock, &mut fetcher, None);
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
        let results = check_lock_with_fetcher(&lock, &mut fetcher, None);
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
        let results = check_lock_with_fetcher(&lock, &mut fetcher, None);
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
        let results = check_lock_with_fetcher(&lock, &mut fetcher, None);
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

        // Empty-string hash = not pinned (defensive against serde "" default).
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
        // A missing solana CLI never gates CI — P2 under every blocking gate.
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

    // proof_hash drift routes identically to binary_hash Mismatch.

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

    // E2E through `check_lock` + the env-var seam: full lock-read → fetch →
    // hash → route pipeline without `solana program dump`. Complements the
    // pure routing tests above and `tests/upstream_check_e2e.rs`.

    /// Serialize tests touching `QEDGEN_UPSTREAM_FAKE_BYTES` — env-var
    /// mutation is process-global and cargo test runs in parallel.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// RAII guard: sets `QEDGEN_UPSTREAM_FAKE_BYTES`, unsets on drop;
    /// holds `env_lock`.
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

    /// Minimal `qed.lock` with one dep pinned to `format_hash(pinned_payload)`;
    /// returns the program_id. The env var controls whether the fetch matches.
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

    /// Mismatch under `Gate::Verify` → CRIT finding that gates exit.
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

    /// Same mismatch under `Gate::CheckFrozen` → P2, no exit gating.
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

    /// `--upstream-stale-ok` demotes the mismatch to Info — recorded, but
    /// neither blocks nor warns.
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

    /// Matching bytes → no finding under any gate (the env var doesn't
    /// manufacture findings).
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

    // ------------------------------------------------------------------------
    // ELF cache (A1)
    // ------------------------------------------------------------------------

    #[test]
    fn cached_elf_path_strips_prefix_lowercases_and_appends_so() {
        let root = Path::new("/cache");
        let p = cached_elf_path_in(root, "sha256:ABCdef01").expect("hex path");
        assert_eq!(p, Path::new("/cache/abcdef01.so"));
        // Bare hex (no prefix) is accepted too.
        let p2 = cached_elf_path_in(root, "00ff").expect("bare hex path");
        assert_eq!(p2, Path::new("/cache/00ff.so"));
    }

    #[test]
    fn cached_elf_path_rejects_non_hex_to_prevent_path_escape() {
        let root = Path::new("/cache");
        for bad in [
            "sha256:../escape",
            "sha256:not-hex!!",
            "sha256:",
            "sha256:dead/beef",
        ] {
            assert!(
                cached_elf_path_in(root, bad).is_err(),
                "non-hex pin {bad:?} must be rejected",
            );
        }
    }

    #[test]
    fn stash_then_read_roundtrips_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = b"\x7fELF-fake-program-bytes".to_vec();
        let hash = format_hash(&bytes);

        let path = stash_elf_in(dir.path(), &hash, &bytes).expect("stash");
        assert!(path.exists(), "cache file written");
        assert_eq!(
            read_cached_elf_in(dir.path(), &hash).as_deref(),
            Some(bytes.as_slice()),
            "read returns the stashed bytes",
        );

        // Content-addressed: a second stash is a no-op returning the same path.
        let path2 = stash_elf_in(dir.path(), &hash, &bytes).expect("re-stash");
        assert_eq!(path, path2);
    }

    #[test]
    fn read_cached_elf_returns_none_when_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_cached_elf_in(dir.path(), &format_hash(b"never-stored")).is_none());
    }

    #[test]
    fn match_stashes_verified_bytes_under_their_hash() {
        let bytes = b"deployed-program-bytes".to_vec();
        let hash = format_hash(&bytes);
        let pid = "Tokenkeg11111111111111111111111111111111111".to_string();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                program_id: Some(pid.clone()),
                upstream_binary_hash: Some(hash.clone()),
                ..entry_with_hash("spl", Some(&hash))
            }],
        };
        let mut fetcher = FakeFetcher::new().ok(&pid, bytes.clone());
        let dir = tempfile::tempdir().expect("tempdir");

        let results = check_lock_with_fetcher(&lock, &mut fetcher, Some(dir.path()));
        assert!(matches!(results[0].outcome, DepCheckOutcome::Match { .. }));
        assert_eq!(
            read_cached_elf_in(dir.path(), &hash).as_deref(),
            Some(bytes.as_slice()),
            "a verified Match stashes the bytes content-addressed",
        );
    }

    #[test]
    fn mismatch_does_not_stash() {
        let pinned = format_hash(b"pinned-v1");
        let pid = "Tokenkeg11111111111111111111111111111111111".to_string();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                program_id: Some(pid.clone()),
                upstream_binary_hash: Some(pinned.clone()),
                ..entry_with_hash("spl", Some(&pinned))
            }],
        };
        // Fetcher returns different bytes → on-chain hash ≠ pin → Mismatch.
        let on_chain = b"redeployed-v2".to_vec();
        let mut fetcher = FakeFetcher::new().ok(&pid, on_chain.clone());
        let dir = tempfile::tempdir().expect("tempdir");

        let results = check_lock_with_fetcher(&lock, &mut fetcher, Some(dir.path()));
        assert!(matches!(
            results[0].outcome,
            DepCheckOutcome::Mismatch { .. }
        ));
        assert!(
            read_cached_elf_in(dir.path(), &pinned).is_none(),
            "pinned hash not cached on mismatch",
        );
        assert!(
            read_cached_elf_in(dir.path(), &format_hash(&on_chain)).is_none(),
            "untrusted on-chain bytes are not cached",
        );
    }
}
