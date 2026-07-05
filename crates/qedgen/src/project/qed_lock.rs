//! `qed.lock` — resolved dependency snapshot, sibling to `qed.toml`. The
//! manifest declares deps; the lock pins how each resolved on the last
//! successful parse (cargo/yarn split).
//!
//! Each entry pins both `ref` (the tag/branch/rev as the user wrote it) and
//! the immutable `resolved_commit` — a force-pushed tag surfaces as a
//! `resolved_commit` diff even though `ref` is identical.
//!
//! Lock format:
//!
//! ```toml
//! version = 1
//!
//! [[dependency]]
//! name = "spl_token"
//! source = "github:QEDGen/solana-skills"
//! ref = "v2.8.0"
//! resolved_commit = "a1b2c3d4..."
//! path = "interfaces/spl_token"
//! spec_hash = "sha256:7f3a..."
//! upstream_binary_hash = "sha256:9c1e..."
//! upstream_version = "spl-token@4.0.3"
//!
//! [[dependency]]
//! name = "my_amm"
//! source = "path:../my_amm"
//! spec_hash = "sha256:b240..."
//! ```
//!
//! Path-source deps write only `name`, `source`, and `spec_hash`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// File name of the lock, expected next to `qed.toml`.
pub const LOCK_FILENAME: &str = "qed.lock";

/// Lock format version; readers reject unknown versions.
pub const LOCK_VERSION: u32 = 1;

/// Top-level structure of `qed.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockFile {
    pub version: u32,
    /// Sorted by `name` for a deterministic on-disk form.
    #[serde(default, rename = "dependency")]
    pub dependencies: Vec<LockEntry>,
}

/// One resolved dependency snapshot. `ref` is a Rust keyword, so the field
/// is `git_ref`, renamed to `ref` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Manifest dep key (the `from "..."` value in `import` statements).
    pub name: String,
    /// `"github:org/repo"` or `"path:<rel>"` — the prefix distinguishes
    /// source kinds without inspecting the optional fields.
    pub source: String,
    /// `"sha256:..."` of the resolved spec source bytes (or, for
    /// multi-file deps, the sorted concatenation of all fragments).
    pub spec_hash: String,

    // ---- GitHub-only fields. None for path sources. ----
    /// Tag / branch / rev value as the user wrote it in the manifest.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none", default)]
    pub git_ref: Option<String>,
    /// Immutable git commit hash captured at resolution time.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolved_commit: Option<String>,
    /// Sub-path inside the repo (e.g. `"interfaces/spl_token"`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,

    /// Program ID from the imported `interface { program_id "..." }` — the
    /// `solana program dump` target for `--check-upstream`. None for
    /// shape-only imports without a deployment target.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub program_id: Option<String>,

    // ---- Upstream binary pin. None unless the imported interface declares
    //      `upstream { binary_hash "..." }`. ----
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub upstream_binary_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub upstream_version: Option<String>,

    // ---- Verified-callee composition. ----
    /// `true` iff the provider shipped a Lake-buildable proof package
    /// (`import_resolver::ResolvedImport::has_proofs`); the consumer's Lean
    /// codegen then `require`s the provider's proof module instead of
    /// generating a sibling axiom module. Defaults `false` on old lockfiles;
    /// a fresh resolve writes `true` and `--frozen` notices the drift.
    #[serde(default, skip_serializing_if = "is_false")]
    pub verified: bool,
    /// sha256 of the provider's proof package contents (sorted-path
    /// concatenation of every `.lean` file under the proof package
    /// root). `None` when `verified` is false.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub proof_hash: Option<String>,

    // ---- Full-spec import bookkeeping. ----
    /// Comma-separated account-type names from the imported source (e.g.
    /// `"State,UserVault"`); empty for interface-only stubs. `--frozen`
    /// uses this to catch renamed / removed imported types at resolve time,
    /// before the consumer's codegen fails mysteriously.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub imported_account_type_names: String,
}

/// `skip_serializing_if` predicate for default-false bools (serde has no
/// `Option::is_none` analogue for plain `bool`).
fn is_false(b: &bool) -> bool {
    !*b
}

impl LockFile {
    /// Empty lock with the current schema version.
    pub fn new() -> Self {
        Self {
            version: LOCK_VERSION,
            dependencies: Vec::new(),
        }
    }

    /// Sort by name (idempotent); call before serializing or comparing for
    /// determinism.
    pub fn sort_dependencies(&mut self) {
        self.dependencies.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

impl Default for LockFile {
    fn default() -> Self {
        Self::new()
    }
}

// ----------------------------------------------------------------------------
// Lock-handling mode
// ----------------------------------------------------------------------------

/// What to do when the on-disk lock differs from the computed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LockMode {
    /// Write the new lock if it's missing or stale. Default.
    #[default]
    Auto,
    /// Error if the lock is missing or stale. Used in CI by
    /// `qedgen check --frozen` to detect un-bumped deps.
    Frozen,
    /// Don't read or write the lock at all. No production caller today —
    /// parse paths pass Auto/Frozen; kept as the documented opt-out mode.
    #[cfg_attr(not(test), allow(dead_code))]
    Skip,
}

// ----------------------------------------------------------------------------
// Computing entries from resolved state
// ----------------------------------------------------------------------------

/// sha256 of the concatenated spec source bytes. Multi-file deps are
/// hashed in sorted-path order with a separator between fragments so a
/// dep that splits one file into two with no content change still
/// changes the hash.
pub fn compute_spec_hash(sources: &[(std::path::PathBuf, String)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (_, bytes) in sources {
        hasher.update(bytes.as_bytes());
        hasher.update(b"\n--QEDGEN-FRAGMENT-BOUNDARY--\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// sha256 of the provider's proof package: every `.lean` under
/// `proof_pkg_root`, sorted-path order, same fragment boundary as
/// `compute_spec_hash`. Lands in `LockEntry.proof_hash` so `--frozen`
/// notices proof changes. `None` when the root is missing or has no `.lean`
/// files (resolver would have set `has_proofs = false`; defensive).
pub fn compute_proof_hash(proof_pkg_root: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    if !proof_pkg_root.is_dir() {
        return None;
    }
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(proof_pkg_root) {
        Ok(rd) => rd
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "lean"))
            .collect(),
        Err(_) => return None,
    };
    if entries.is_empty() {
        return None;
    }
    entries.sort();
    let mut hasher = Sha256::new();
    for path in &entries {
        if let Ok(bytes) = std::fs::read(path) {
            hasher.update(&bytes);
            hasher.update(b"\n--QEDGEN-FRAGMENT-BOUNDARY--\n");
        }
    }
    Some(format!("sha256:{:x}", hasher.finalize()))
}

/// Build a single lock entry from a resolved import + its manifest dep
/// descriptor + the imported interface (which carries its `program_id`
/// and optional `upstream` block).
pub fn entry_for_resolved(
    resolved: &crate::import_resolver::ResolvedImport,
    dep: &crate::qed_manifest::Dependency,
    iface: &crate::check::ParsedInterface,
    imported_account_type_names: &str,
) -> LockEntry {
    use crate::qed_manifest::Dependency;
    let spec_hash = compute_spec_hash(&resolved.sources);
    let (source, git_ref, resolved_commit, path) = match dep {
        Dependency::Github {
            repo,
            git_ref,
            path,
        } => (
            format!("github:{}", repo),
            Some(git_ref.as_str().to_string()),
            resolved.commit.clone(),
            path.clone(),
        ),
        Dependency::Path { path } => (format!("path:{}", path), None, None, None),
    };
    let (upstream_binary_hash, upstream_version) = match &iface.upstream {
        Some(u) => (u.binary_hash.clone(), u.version.clone()),
        None => (None, None),
    };
    // proof_hash only when the resolver detected proofs; otherwise omitted.
    let proof_hash = resolved
        .proof_pkg_root
        .as_deref()
        .filter(|_| resolved.has_proofs)
        .and_then(compute_proof_hash);
    LockEntry {
        name: resolved.dep_key.clone(),
        source,
        spec_hash,
        git_ref,
        resolved_commit,
        path,
        program_id: iface.program_id.clone(),
        upstream_binary_hash,
        upstream_version,
        verified: resolved.has_proofs,
        proof_hash,
        imported_account_type_names: imported_account_type_names.to_string(),
    }
}

/// Lock entry for a bundled-stdlib builtin import: no manifest dep exists,
/// so source is `builtin:<key>` and version fields come from the imported
/// interface's `upstream` block (when present).
pub fn entry_for_builtin(
    resolved: &crate::import_resolver::ResolvedImport,
    iface: &crate::check::ParsedInterface,
    imported_account_type_names: &str,
) -> LockEntry {
    let spec_hash = compute_spec_hash(&resolved.sources);
    let (upstream_binary_hash, upstream_version) = match &iface.upstream {
        Some(u) => (u.binary_hash.clone(), u.version.clone()),
        None => (None, None),
    };
    let proof_hash = resolved
        .proof_pkg_root
        .as_deref()
        .filter(|_| resolved.has_proofs)
        .and_then(compute_proof_hash);
    LockEntry {
        name: resolved.dep_key.clone(),
        source: format!("builtin:{}", resolved.dep_key),
        spec_hash,
        git_ref: None,
        resolved_commit: None,
        path: None,
        program_id: iface.program_id.clone(),
        upstream_binary_hash,
        upstream_version,
        verified: resolved.has_proofs,
        proof_hash,
        imported_account_type_names: imported_account_type_names.to_string(),
    }
}

// ----------------------------------------------------------------------------
// Reconciling the computed lock with disk
// ----------------------------------------------------------------------------

/// Compare `computed` against `<spec_dir>/qed.lock` per `mode`. Returns
/// proof_hash drift findings (empty in Auto/Skip; non-empty in Frozen only
/// when proof_hash is the sole difference — routed P2/CRIT at the call site
/// via [`crate::upstream_check::route_findings`]). Frozen bails on
/// structural drift (any other field, or added/removed entries); Auto
/// writes the new lock either way.
pub fn handle_lock(
    spec_dir: &Path,
    computed: &LockFile,
    mode: LockMode,
) -> Result<Vec<crate::upstream_check::DepCheckResult>> {
    if matches!(mode, LockMode::Skip) {
        return Ok(Vec::new());
    }

    let mut computed_sorted = computed.clone();
    computed_sorted.sort_dependencies();

    let on_disk = read(spec_dir)?;
    let needs_update = match &on_disk {
        Some(existing) => {
            let mut existing_sorted = existing.clone();
            existing_sorted.sort_dependencies();
            existing_sorted != computed_sorted
        }
        None => true,
    };

    if !needs_update {
        return Ok(Vec::new());
    }

    match mode {
        LockMode::Auto => {
            write(spec_dir, &computed_sorted)?;
            Ok(Vec::new())
        }
        LockMode::Frozen => {
            // proof_hash-only delta → soft findings; anything else bails —
            // rebuilding from the wrong git ref or spec source isn't a
            // soft signal.
            if let Some(existing) = on_disk.as_ref() {
                if structurally_equal(existing, &computed_sorted) {
                    return Ok(detect_proof_hash_drift_from_locks(
                        existing,
                        &computed_sorted,
                    ));
                }
            }
            let diff = describe_lock_diff(on_disk.as_ref(), &computed_sorted);
            anyhow::bail!(
                "qed.lock at {} is stale (--frozen):\n{}",
                spec_dir.join(LOCK_FILENAME).display(),
                diff
            )
        }
        LockMode::Skip => unreachable!(),
    }
}

/// True if `a` and `b` agree on every per-entry field except `proof_hash`
/// and have the same entry set.
fn structurally_equal(a: &LockFile, b: &LockFile) -> bool {
    let mut aa = a.clone();
    let mut bb = b.clone();
    aa.sort_dependencies();
    bb.sort_dependencies();
    for e in &mut aa.dependencies {
        e.proof_hash = None;
    }
    for e in &mut bb.dependencies {
        e.proof_hash = None;
    }
    aa == bb
}

/// Proof_hash drift between an on-disk and freshly-computed lock: yields a
/// [`crate::upstream_check::DepCheckOutcome::ProofHashMismatch`] per
/// `verified` entry (matched by name) whose proof_hash differs. Adds /
/// removes are structural drift handled elsewhere — excluded here so the
/// routing layer doesn't false-positive on them.
pub fn detect_proof_hash_drift_from_locks(
    on_disk: &LockFile,
    computed: &LockFile,
) -> Vec<crate::upstream_check::DepCheckResult> {
    use crate::upstream_check::{DepCheckOutcome, DepCheckResult};
    let on_disk_map: std::collections::BTreeMap<&str, &LockEntry> = on_disk
        .dependencies
        .iter()
        .map(|d| (d.name.as_str(), d))
        .collect();
    let mut results = Vec::new();
    for entry in &computed.dependencies {
        if !entry.verified {
            continue;
        }
        if let Some(existing) = on_disk_map.get(entry.name.as_str()) {
            if existing.proof_hash != entry.proof_hash {
                results.push(DepCheckResult {
                    name: entry.name.clone(),
                    outcome: DepCheckOutcome::ProofHashMismatch {
                        pinned: existing.proof_hash.clone().unwrap_or_default(),
                        computed: entry.proof_hash.clone().unwrap_or_default(),
                    },
                });
            }
        }
    }
    results
}

/// Render a short human-readable diff between an existing lock (or
/// missing) and the computed one. Used in --frozen error messages so CI
/// failures point straight at the offending dep.
fn describe_lock_diff(existing: Option<&LockFile>, computed: &LockFile) -> String {
    let mut out = String::new();
    let existing_map: std::collections::BTreeMap<&str, &LockEntry> = existing
        .map(|e| {
            e.dependencies
                .iter()
                .map(|d| (d.name.as_str(), d))
                .collect()
        })
        .unwrap_or_default();
    let computed_map: std::collections::BTreeMap<&str, &LockEntry> = computed
        .dependencies
        .iter()
        .map(|d| (d.name.as_str(), d))
        .collect();

    if existing.is_none() {
        out.push_str("  qed.lock is missing on disk; expected entries:\n");
        for entry in &computed.dependencies {
            out.push_str(&format!("    + {} ({})\n", entry.name, entry.source));
        }
        return out;
    }

    for (name, computed_entry) in &computed_map {
        match existing_map.get(name) {
            Some(existing_entry) if existing_entry == computed_entry => {}
            Some(existing_entry) => {
                out.push_str(&format!("  ~ {} (changed):\n", name));
                if existing_entry.source != computed_entry.source {
                    out.push_str(&format!(
                        "      source: {} → {}\n",
                        existing_entry.source, computed_entry.source
                    ));
                }
                if existing_entry.git_ref != computed_entry.git_ref {
                    out.push_str(&format!(
                        "      ref: {:?} → {:?}\n",
                        existing_entry.git_ref, computed_entry.git_ref
                    ));
                }
                if existing_entry.resolved_commit != computed_entry.resolved_commit {
                    out.push_str(&format!(
                        "      resolved_commit: {:?} → {:?}\n",
                        existing_entry.resolved_commit, computed_entry.resolved_commit
                    ));
                }
                if existing_entry.spec_hash != computed_entry.spec_hash {
                    out.push_str(&format!(
                        "      spec_hash: {} → {}\n",
                        existing_entry.spec_hash, computed_entry.spec_hash
                    ));
                }
                if existing_entry.upstream_binary_hash != computed_entry.upstream_binary_hash {
                    out.push_str(&format!(
                        "      upstream_binary_hash: {:?} → {:?}\n",
                        existing_entry.upstream_binary_hash, computed_entry.upstream_binary_hash,
                    ));
                }
                if existing_entry.proof_hash != computed_entry.proof_hash {
                    out.push_str(&format!(
                        "      proof_hash: {:?} → {:?}\n",
                        existing_entry.proof_hash, computed_entry.proof_hash,
                    ));
                }
                if existing_entry.verified != computed_entry.verified {
                    out.push_str(&format!(
                        "      verified: {} → {}\n",
                        existing_entry.verified, computed_entry.verified,
                    ));
                }
                // Imported type-set drift: name the added/removed/renamed type.
                if existing_entry.imported_account_type_names
                    != computed_entry.imported_account_type_names
                {
                    out.push_str(&format!(
                        "      imported_account_type_names: {:?} → {:?}\n",
                        existing_entry.imported_account_type_names,
                        computed_entry.imported_account_type_names,
                    ));
                }
            }
            None => {
                out.push_str(&format!(
                    "  + {} (new dep, source: {})\n",
                    name, computed_entry.source
                ));
            }
        }
    }
    for (name, existing_entry) in &existing_map {
        if !computed_map.contains_key(name) {
            out.push_str(&format!(
                "  - {} (no longer in qed.toml, was: {})\n",
                name, existing_entry.source
            ));
        }
    }
    out
}

// ----------------------------------------------------------------------------
// Disk I/O
// ----------------------------------------------------------------------------

/// Read `<spec_dir>/qed.lock` if present. Returns `Ok(None)` when the
/// file doesn't exist (the caller decides whether that's an error).
pub fn read(spec_dir: &Path) -> Result<Option<LockFile>> {
    let path = spec_dir.join(LOCK_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let lock: LockFile =
        toml::from_str(&bytes).with_context(|| format!("parsing {} as TOML", path.display()))?;
    if lock.version != LOCK_VERSION {
        anyhow::bail!(
            "qed.lock at {} declares unsupported version {} (expected {})",
            path.display(),
            lock.version,
            LOCK_VERSION
        );
    }
    Ok(Some(lock))
}

/// Write `qed.lock` to `<spec_dir>/qed.lock`, sorting dependencies by
/// name first so the on-disk form is deterministic.
pub fn write(spec_dir: &Path, lock: &LockFile) -> Result<()> {
    let mut to_write = lock.clone();
    to_write.sort_dependencies();
    let body = toml::to_string_pretty(&to_write).context("serializing qed.lock to TOML")?;
    let header = "# Generated by qedgen — do not edit by hand.\n\
                  # Tracks the resolved snapshot of every dep declared in qed.toml.\n\n";
    let full = format!("{}{}", header, body);
    let path = spec_dir.join(LOCK_FILENAME);
    std::fs::write(&path, full).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, source: &str, spec_hash: &str) -> LockEntry {
        LockEntry {
            name: name.to_string(),
            source: source.to_string(),
            spec_hash: spec_hash.to_string(),
            git_ref: None,
            resolved_commit: None,
            path: None,
            program_id: None,
            upstream_binary_hash: None,
            upstream_version: None,
            verified: false,
            proof_hash: None,
            imported_account_type_names: String::new(),
        }
    }

    #[test]
    fn round_trips_minimal_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("spl_token", "path:../t", "sha256:abc")],
        };
        write(tmp.path(), &lock).unwrap();
        let read_back = read(tmp.path()).unwrap().unwrap();
        assert_eq!(read_back, lock);
    }

    #[test]
    fn round_trips_github_dep_with_full_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                name: "spl_token".to_string(),
                source: "github:QEDGen/solana-skills".to_string(),
                spec_hash: "sha256:7f3a".to_string(),
                git_ref: Some("v2.8.0".to_string()),
                resolved_commit: Some("a1b2c3d4".to_string()),
                path: Some("interfaces/spl_token".to_string()),
                program_id: Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".to_string()),
                upstream_binary_hash: Some("sha256:9c1e".to_string()),
                upstream_version: Some("spl-token@4.0.3".to_string()),
                verified: true,
                proof_hash: Some("sha256:abcd".to_string()),
                imported_account_type_names: String::new(),
            }],
        };
        write(tmp.path(), &lock).unwrap();
        let read_back = read(tmp.path()).unwrap().unwrap();
        assert_eq!(read_back, lock);
    }

    #[test]
    fn read_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = read(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn write_sorts_dependencies_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![
                entry("z_dep", "path:./z", "sha256:z"),
                entry("a_dep", "path:./a", "sha256:a"),
                entry("m_dep", "path:./m", "sha256:m"),
            ],
        };
        write(tmp.path(), &lock).unwrap();
        let on_disk = std::fs::read_to_string(tmp.path().join(LOCK_FILENAME)).unwrap();
        let a_pos = on_disk.find("a_dep").unwrap();
        let m_pos = on_disk.find("m_dep").unwrap();
        let z_pos = on_disk.find("z_dep").unwrap();
        assert!(
            a_pos < m_pos && m_pos < z_pos,
            "deps should appear in sorted order on disk"
        );
    }

    #[test]
    fn rejects_unknown_version() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(LOCK_FILENAME),
            r#"version = 999
[[dependency]]
name = "x"
source = "path:./x"
spec_hash = "sha256:abc"
"#,
        )
        .unwrap();
        let err = read(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("unsupported version"), "got: {err}");
    }

    #[test]
    fn elides_none_fields_in_serialized_form() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("local", "path:../l", "sha256:0")],
        };
        write(tmp.path(), &lock).unwrap();
        let on_disk = std::fs::read_to_string(tmp.path().join(LOCK_FILENAME)).unwrap();
        // None fields should not appear at all (skip_serializing_if).
        assert!(!on_disk.contains("ref ="), "ref should be elided");
        assert!(
            !on_disk.contains("resolved_commit"),
            "resolved_commit should be elided"
        );
        assert!(
            !on_disk.contains("upstream_binary_hash"),
            "upstream_binary_hash should be elided"
        );
    }

    #[test]
    fn header_comment_is_preserved_on_write() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile::new();
        write(tmp.path(), &lock).unwrap();
        let on_disk = std::fs::read_to_string(tmp.path().join(LOCK_FILENAME)).unwrap();
        assert!(on_disk.contains("Generated by qedgen"));
    }

    // ----- compute / handle_lock -----

    #[test]
    fn compute_spec_hash_is_deterministic_and_distinguishes_content() {
        use std::path::PathBuf;
        let s1 = vec![(PathBuf::from("a"), "spec X\n".to_string())];
        let s2 = vec![(PathBuf::from("a"), "spec X\n".to_string())];
        let s3 = vec![(PathBuf::from("a"), "spec Y\n".to_string())];

        assert_eq!(compute_spec_hash(&s1), compute_spec_hash(&s2));
        assert_ne!(compute_spec_hash(&s1), compute_spec_hash(&s3));
        assert!(
            compute_spec_hash(&s1).starts_with("sha256:"),
            "hash should be prefixed with sha256:"
        );
    }

    #[test]
    fn handle_lock_skip_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:0")],
        };
        handle_lock(tmp.path(), &computed, LockMode::Skip).unwrap();
        // Nothing written to disk.
        assert!(!tmp.path().join(LOCK_FILENAME).exists());
    }

    #[test]
    fn handle_lock_auto_writes_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:0")],
        };
        handle_lock(tmp.path(), &computed, LockMode::Auto).unwrap();
        let on_disk = read(tmp.path()).unwrap().expect("should be written");
        assert_eq!(on_disk, computed);
    }

    #[test]
    fn handle_lock_auto_overwrites_when_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let old = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:OLD")],
        };
        write(tmp.path(), &old).unwrap();

        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:NEW")],
        };
        handle_lock(tmp.path(), &computed, LockMode::Auto).unwrap();
        let on_disk = read(tmp.path()).unwrap().unwrap();
        assert_eq!(on_disk.dependencies[0].spec_hash, "sha256:NEW");
    }

    #[test]
    fn handle_lock_auto_is_idempotent_when_current() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:0")],
        };
        write(tmp.path(), &lock).unwrap();
        let mtime_before = std::fs::metadata(tmp.path().join(LOCK_FILENAME))
            .unwrap()
            .modified()
            .unwrap();

        // Tiny sleep to ensure mtime would visibly change if we re-wrote.
        std::thread::sleep(std::time::Duration::from_millis(20));

        handle_lock(tmp.path(), &lock, LockMode::Auto).unwrap();
        let mtime_after = std::fs::metadata(tmp.path().join(LOCK_FILENAME))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "lock should not be rewritten when content is unchanged"
        );
    }

    #[test]
    fn handle_lock_frozen_errors_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:0")],
        };
        let err = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .unwrap_err()
            .to_string();
        assert!(err.contains("stale (--frozen)"), "got: {err}");
        assert!(err.contains("missing on disk"), "got: {err}");
    }

    #[test]
    fn handle_lock_frozen_errors_when_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let old = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:OLD")],
        };
        write(tmp.path(), &old).unwrap();

        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:NEW")],
        };
        let err = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .unwrap_err()
            .to_string();
        assert!(err.contains("spec_hash"), "got: {err}");
        assert!(err.contains("OLD"), "got: {err}");
        assert!(err.contains("NEW"), "got: {err}");
    }

    #[test]
    fn handle_lock_frozen_succeeds_when_current() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("x", "path:./x", "sha256:0")],
        };
        write(tmp.path(), &lock).unwrap();
        handle_lock(tmp.path(), &lock, LockMode::Frozen).unwrap();
    }

    // ----- verified + proof_hash schema -----

    #[test]
    fn old_lockfile_without_verified_field_parses_with_default_false() {
        // Old lockfile without `verified` / `proof_hash` must parse with defaults.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(LOCK_FILENAME),
            r#"version = 1

[[dependency]]
name = "x"
source = "path:./x"
spec_hash = "sha256:abc"
"#,
        )
        .unwrap();
        let lock = read(tmp.path()).unwrap().expect("must parse");
        assert_eq!(lock.dependencies.len(), 1);
        assert!(!lock.dependencies[0].verified);
        assert!(lock.dependencies[0].proof_hash.is_none());
    }

    #[test]
    fn verified_false_is_elided_from_serialized_form() {
        // Defaults must not serialize — otherwise old lockfiles churn on regen.
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("local", "path:../l", "sha256:0")],
        };
        write(tmp.path(), &lock).unwrap();
        let on_disk = std::fs::read_to_string(tmp.path().join(LOCK_FILENAME)).unwrap();
        assert!(
            !on_disk.contains("verified"),
            "verified=false should not appear on disk; got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("proof_hash"),
            "proof_hash=None should not appear on disk; got:\n{on_disk}"
        );
    }

    #[test]
    fn verified_true_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                name: "amm".to_string(),
                source: "path:./amm".to_string(),
                spec_hash: "sha256:s".to_string(),
                git_ref: None,
                resolved_commit: None,
                path: None,
                program_id: None,
                upstream_binary_hash: None,
                upstream_version: None,
                verified: true,
                proof_hash: Some("sha256:p".to_string()),
                imported_account_type_names: String::new(),
            }],
        };
        write(tmp.path(), &lock).unwrap();
        let read_back = read(tmp.path()).unwrap().unwrap();
        assert_eq!(read_back, lock);
        let on_disk = std::fs::read_to_string(tmp.path().join(LOCK_FILENAME)).unwrap();
        assert!(on_disk.contains("verified = true"), "got:\n{on_disk}");
        assert!(on_disk.contains("proof_hash"), "got:\n{on_disk}");
    }

    #[test]
    fn compute_proof_hash_is_deterministic_across_runs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Token.lean"), "-- module\n").unwrap();
        std::fs::write(tmp.path().join("lakefile.lean"), "-- lakefile\n").unwrap();
        let a = compute_proof_hash(tmp.path()).expect("present");
        let b = compute_proof_hash(tmp.path()).expect("present");
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));
    }

    #[test]
    fn compute_proof_hash_changes_with_module_content() {
        let tmp1 = tempfile::tempdir().unwrap();
        std::fs::write(tmp1.path().join("Token.lean"), "-- v1\n").unwrap();
        std::fs::write(tmp1.path().join("lakefile.lean"), "-- lake\n").unwrap();

        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp2.path().join("Token.lean"), "-- v2 different\n").unwrap();
        std::fs::write(tmp2.path().join("lakefile.lean"), "-- lake\n").unwrap();

        let h1 = compute_proof_hash(tmp1.path()).unwrap();
        let h2 = compute_proof_hash(tmp2.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn compute_proof_hash_returns_none_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nope = tmp.path().join("does-not-exist");
        assert!(compute_proof_hash(&nope).is_none());
    }

    #[test]
    fn compute_proof_hash_returns_none_when_no_lean_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), "not lean").unwrap();
        assert!(compute_proof_hash(tmp.path()).is_none());
    }

    #[test]
    fn frozen_proof_hash_only_drift_returns_soft_findings_not_bail() {
        // proof_hash-only drift under --frozen returns ProofHashMismatch
        // findings instead of bailing; the caller routes them P2/CRIT.
        let tmp = tempfile::tempdir().unwrap();
        let old = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                name: "amm".to_string(),
                source: "path:./amm".to_string(),
                spec_hash: "sha256:same".to_string(),
                git_ref: None,
                resolved_commit: None,
                path: None,
                program_id: None,
                upstream_binary_hash: None,
                upstream_version: None,
                verified: true,
                proof_hash: Some("sha256:OLD".to_string()),
                imported_account_type_names: String::new(),
            }],
        };
        write(tmp.path(), &old).unwrap();

        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                name: "amm".to_string(),
                source: "path:./amm".to_string(),
                spec_hash: "sha256:same".to_string(),
                git_ref: None,
                resolved_commit: None,
                path: None,
                program_id: None,
                upstream_binary_hash: None,
                upstream_version: None,
                verified: true,
                proof_hash: Some("sha256:NEW".to_string()),
                imported_account_type_names: String::new(),
            }],
        };
        let findings = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .expect("proof_hash-only drift must return Ok, not bail");
        assert_eq!(findings.len(), 1, "one finding for one drifted dep");
        match &findings[0].outcome {
            crate::upstream_check::DepCheckOutcome::ProofHashMismatch { pinned, computed } => {
                assert_eq!(pinned, "sha256:OLD");
                assert_eq!(computed, "sha256:NEW");
            }
            other => panic!("expected ProofHashMismatch, got {:?}", other),
        }
        assert_eq!(findings[0].name, "amm");
    }

    #[test]
    fn frozen_structural_drift_still_bails_even_with_proof_hash_change() {
        // Soft-routing applies only when proof_hash is the ONLY drifted
        // field; structural drift alongside it still bails.
        let tmp = tempfile::tempdir().unwrap();
        let base = LockEntry {
            name: "amm".to_string(),
            source: "path:./amm".to_string(),
            spec_hash: "sha256:OLD".to_string(),
            git_ref: None,
            resolved_commit: None,
            path: None,
            program_id: None,
            upstream_binary_hash: None,
            upstream_version: None,
            verified: true,
            proof_hash: Some("sha256:proof_OLD".to_string()),
            imported_account_type_names: String::new(),
        };
        write(
            tmp.path(),
            &LockFile {
                version: LOCK_VERSION,
                dependencies: vec![base.clone()],
            },
        )
        .unwrap();

        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                spec_hash: "sha256:NEW".to_string(),
                proof_hash: Some("sha256:proof_NEW".to_string()),
                ..base
            }],
        };
        let err = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .expect_err("structural drift must still bail")
            .to_string();
        assert!(err.contains("stale (--frozen)"), "got: {err}");
    }

    // ----- detect_proof_hash_drift_from_locks -----

    fn verified_entry(name: &str, proof_hash: Option<&str>) -> LockEntry {
        let mut e = entry(name, "path:./x", "sha256:same");
        e.verified = true;
        e.proof_hash = proof_hash.map(str::to_string);
        e
    }

    #[test]
    fn detect_proof_hash_drift_yields_mismatch_when_verified_proof_changes() {
        let on_disk = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![verified_entry("amm", Some("sha256:OLD"))],
        };
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![verified_entry("amm", Some("sha256:NEW"))],
        };
        let drifts = detect_proof_hash_drift_from_locks(&on_disk, &computed);
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].name, "amm");
        match &drifts[0].outcome {
            crate::upstream_check::DepCheckOutcome::ProofHashMismatch { pinned, computed } => {
                assert_eq!(pinned, "sha256:OLD");
                assert_eq!(computed, "sha256:NEW");
            }
            other => panic!("expected ProofHashMismatch, got {:?}", other),
        }
    }

    #[test]
    fn detect_proof_hash_drift_silent_when_proof_hash_matches() {
        let on_disk = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![verified_entry("amm", Some("sha256:SAME"))],
        };
        let computed = on_disk.clone();
        assert!(detect_proof_hash_drift_from_locks(&on_disk, &computed).is_empty());
    }

    #[test]
    fn detect_proof_hash_drift_skips_unverified_entries() {
        // verified=false: proof_hash drift is a no-op or structural drift —
        // never a soft finding.
        let on_disk = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("noproofs", "path:./n", "sha256:0")],
        };
        let mut changed = entry("noproofs", "path:./n", "sha256:0");
        changed.proof_hash = Some("sha256:NEW".to_string());
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![changed],
        };
        assert!(
            detect_proof_hash_drift_from_locks(&on_disk, &computed).is_empty(),
            "verified=false drift must not produce ProofHashMismatch findings",
        );
    }

    #[test]
    fn detect_proof_hash_drift_ignores_entries_added_or_removed() {
        // Added/removed entries are structural — no per-entry drift, so the
        // routing layer doesn't double-report.
        let on_disk = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![verified_entry("removed", Some("sha256:gone"))],
        };
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![verified_entry("added", Some("sha256:new"))],
        };
        assert!(
            detect_proof_hash_drift_from_locks(&on_disk, &computed).is_empty(),
            "added/removed entries must not yield ProofHashMismatch",
        );
    }

    #[test]
    fn handle_lock_frozen_diff_names_upstream_binary_hash() {
        // A bumped `binary_hash` must be named in the frozen diff (spec_hash
        // also changes since the source bytes drift).
        let tmp = tempfile::tempdir().unwrap();
        let mut old = LockEntry {
            name: "spl".to_string(),
            source: "builtin:spl".to_string(),
            spec_hash: "sha256:same".to_string(),
            git_ref: None,
            resolved_commit: None,
            path: None,
            program_id: Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA".to_string()),
            upstream_binary_hash: Some("sha256:OLD".to_string()),
            upstream_version: Some("4.0.3".to_string()),
            verified: false,
            proof_hash: None,
            imported_account_type_names: String::new(),
        };
        let old_lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![old.clone()],
        };
        write(tmp.path(), &old_lock).unwrap();

        old.upstream_binary_hash = Some("sha256:NEW".to_string());
        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![old],
        };
        let err = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("upstream_binary_hash"),
            "diff must call out upstream_binary_hash; got: {err}"
        );
        assert!(err.contains("OLD"), "diff should include old value: {err}");
        assert!(err.contains("NEW"), "diff should include new value: {err}");
    }

    #[test]
    fn handle_lock_frozen_diff_names_proof_hash_and_verified() {
        // A verified flip and proof_hash drift must both surface in the diff.
        let tmp = tempfile::tempdir().unwrap();
        let old = LockEntry {
            name: "amm".to_string(),
            source: "path:./amm".to_string(),
            spec_hash: "sha256:same".to_string(),
            git_ref: None,
            resolved_commit: None,
            path: None,
            program_id: None,
            upstream_binary_hash: None,
            upstream_version: None,
            verified: false,
            proof_hash: None,
            imported_account_type_names: String::new(),
        };
        let old_lock = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![old.clone()],
        };
        write(tmp.path(), &old_lock).unwrap();

        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![LockEntry {
                verified: true,
                proof_hash: Some("sha256:NEW".to_string()),
                ..old
            }],
        };
        let err = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("verified"),
            "diff must call out verified flip; got: {err}"
        );
        assert!(
            err.contains("proof_hash"),
            "diff must call out proof_hash; got: {err}"
        );
    }

    #[test]
    fn handle_lock_frozen_describes_added_and_removed_deps() {
        let tmp = tempfile::tempdir().unwrap();
        let old = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("removed", "path:./r", "sha256:0")],
        };
        write(tmp.path(), &old).unwrap();

        let computed = LockFile {
            version: LOCK_VERSION,
            dependencies: vec![entry("added", "path:./a", "sha256:1")],
        };
        let err = handle_lock(tmp.path(), &computed, LockMode::Frozen)
            .unwrap_err()
            .to_string();
        assert!(err.contains("+ added"), "should report added dep: {err}");
        assert!(
            err.contains("- removed"),
            "should report removed dep: {err}"
        );
    }
}
