//! `qed.lock` — resolved dependency snapshot (v2.8 G2).
//!
//! Sibling to `qed.toml`. The manifest is the source-of-truth for which
//! deps a spec has; the lock is a snapshot of how each dep resolved on
//! the last successful parse. Standard cargo / yarn split.
//!
//! Each lock entry pins both the human-readable `ref` (the tag, branch,
//! or rev value the user wrote in the manifest) and the `resolved_commit`
//! — the immutable git commit hash. If a tag is force-pushed, the next
//! resolution discovers a different commit; the lock catches the change
//! by diffing `resolved_commit`, even though `ref` is identical.
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
//! For path-source deps, `ref` / `resolved_commit` / `path` /
//! `upstream_*` are all `None` — only `name`, `source`, and `spec_hash`
//! are written.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// File name of the lock, expected next to `qed.toml`.
pub const LOCK_FILENAME: &str = "qed.lock";

/// Current lock format version. Bumped when the schema changes
/// incompatibly; readers reject locks with an unknown version.
pub const LOCK_VERSION: u32 = 1;

/// Top-level structure of `qed.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LockFile {
    pub version: u32,
    /// Sorted by `name` so the on-disk form is deterministic regardless
    /// of resolution order.
    #[serde(default, rename = "dependency")]
    pub dependencies: Vec<LockEntry>,
}

/// One resolved dependency snapshot.
///
/// Field naming note: `ref` is a Rust keyword, so the Rust field is
/// `git_ref` and we rename to `ref` on the wire via serde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LockEntry {
    /// Manifest dep key (the `from "..."` value in `import` statements).
    pub name: String,
    /// `"github:org/repo"` for github sources, `"path:<rel>"` for path
    /// sources. The qualifier prefix lets readers distinguish without
    /// looking at the optional fields.
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

    // ---- Upstream binary pin. None unless the imported interface declares
    //      `upstream { binary_hash "..." }`. ----
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub upstream_binary_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub upstream_version: Option<String>,
}

impl LockFile {
    /// Empty lock with the current schema version.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            version: LOCK_VERSION,
            dependencies: Vec::new(),
        }
    }

    /// Sort dependencies by name. Idempotent. Call before serializing
    /// or comparing so the on-disk form and equality checks are
    /// deterministic.
    #[allow(dead_code)]
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
#[allow(dead_code)]
pub enum LockMode {
    /// Write the new lock if it's missing or stale. Default.
    #[default]
    Auto,
    /// Error if the lock is missing or stale. Used in CI by
    /// `qedgen check --frozen` to detect un-bumped deps.
    Frozen,
    /// Don't read or write the lock at all.
    Skip,
}

// ----------------------------------------------------------------------------
// Computing entries from resolved state
// ----------------------------------------------------------------------------

/// sha256 of the concatenated spec source bytes. Multi-file deps are
/// hashed in sorted-path order with a separator between fragments so a
/// dep that splits one file into two with no content change still
/// changes the hash.
#[allow(dead_code)]
pub fn compute_spec_hash(sources: &[(std::path::PathBuf, String)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (_, bytes) in sources {
        hasher.update(bytes.as_bytes());
        hasher.update(b"\n--QEDGEN-FRAGMENT-BOUNDARY--\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Build a single lock entry from a resolved import + its manifest dep
/// descriptor + the imported interface's optional `upstream` block.
#[allow(dead_code)]
pub fn entry_for_resolved(
    resolved: &crate::import_resolver::ResolvedImport,
    dep: &crate::qed_manifest::Dependency,
    iface_upstream: Option<&crate::check::ParsedUpstream>,
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
    let (upstream_binary_hash, upstream_version) = match iface_upstream {
        Some(u) => (u.binary_hash.clone(), u.version.clone()),
        None => (None, None),
    };
    LockEntry {
        name: resolved.dep_key.clone(),
        source,
        spec_hash,
        git_ref,
        resolved_commit,
        path,
        upstream_binary_hash,
        upstream_version,
    }
}

// ----------------------------------------------------------------------------
// Reconciling the computed lock with disk
// ----------------------------------------------------------------------------

/// Compare `computed` against the lock on disk at `spec_dir/qed.lock`
/// and act per `mode`. Returns Ok(()) if the lock is current (or was
/// updated successfully); Err(...) if Frozen mode finds drift.
#[allow(dead_code)]
pub fn handle_lock(spec_dir: &Path, computed: &LockFile, mode: LockMode) -> Result<()> {
    if matches!(mode, LockMode::Skip) {
        return Ok(());
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
        return Ok(());
    }

    match mode {
        LockMode::Auto => {
            write(spec_dir, &computed_sorted)?;
            Ok(())
        }
        LockMode::Frozen => {
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
            upstream_binary_hash: None,
            upstream_version: None,
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
                upstream_binary_hash: Some("sha256:9c1e".to_string()),
                upstream_version: Some("spl-token@4.0.3".to_string()),
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
