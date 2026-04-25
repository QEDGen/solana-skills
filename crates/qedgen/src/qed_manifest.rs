//! `qed.toml` — manifest for spec dependencies (v2.8 G1).
//!
//! Cargo-style split: `qed.toml` is the source of truth for which interfaces
//! a spec depends on. The spec-side `import Name from "key"` statements
//! reference dep keys declared here; the resolver consumes both to fetch
//! sources and merge interface declarations.
//!
//! Schema:
//!
//! ```toml
//! [dependencies]
//! spl_token = { github = "QEDGen/solana-skills", path = "interfaces/spl_token", tag = "v2.8.0" }
//! system_program = { github = "QEDGen/solana-skills", branch = "main" }
//! my_amm = { path = "../my_amm" }
//! ```
//!
//! Source forms:
//! - **GitHub**: `github = "org/repo"` plus exactly one of `tag` / `branch` /
//!   `rev`, plus optional `path` (sub-path within the repo).
//! - **Path**: `path = "..."` — relative to `qed.toml`'s directory, or
//!   absolute. Mutually exclusive with the GitHub fields.
//!
//! v2.8 does not support: registry shorthand (`spl_token = "1.0"`), workspace
//! inheritance, or transitive lock merging.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// File name of the manifest, expected at the spec root.
pub const MANIFEST_FILENAME: &str = "qed.toml";

/// Validated manifest. Keys are dep names (the `from "..."` strings in
/// import statements); values are validated source descriptors.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct Manifest {
    pub dependencies: BTreeMap<String, Dependency>,
}

/// One validated dependency entry.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Dependency {
    /// `github = "org/repo"` plus a ref + optional sub-path.
    Github {
        /// `"org/repo"` as declared in the manifest.
        repo: String,
        /// Tag / branch / rev — exactly one is set per entry.
        git_ref: GitRef,
        /// Sub-path within the repo (e.g. `"interfaces/spl_token"`).
        /// `None` means the repo root.
        path: Option<String>,
    },
    /// `path = "..."` — local source, no fetch.
    Path { path: String },
}

/// Which kind of git reference the dependency pins.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum GitRef {
    Tag(String),
    Branch(String),
    Rev(String),
}

#[allow(dead_code)]
impl GitRef {
    /// Human-readable form of the ref (the value the user wrote). Used for
    /// `git clone --branch <ref>` (which accepts tags and branches but not
    /// commit hashes — for `Rev` the resolver clones default branch then
    /// `git checkout <rev>`).
    pub fn as_str(&self) -> &str {
        match self {
            GitRef::Tag(s) | GitRef::Branch(s) | GitRef::Rev(s) => s,
        }
    }

    /// Tag for the cache key — distinguishes tag vs branch vs rev so a tag
    /// `v1.0` and a branch named `v1.0` don't collide on disk.
    pub fn cache_kind(&self) -> &'static str {
        match self {
            GitRef::Tag(_) => "tag",
            GitRef::Branch(_) => "branch",
            GitRef::Rev(_) => "rev",
        }
    }
}

// ----------------------------------------------------------------------------
// Raw deserialization shape — accepts the full superset of fields, then
// `validate` boils each entry down to a `Dependency`.
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default)]
    dependencies: BTreeMap<String, RawDependency>,
}

#[derive(Debug, Deserialize)]
struct RawDependency {
    github: Option<String>,
    tag: Option<String>,
    branch: Option<String>,
    rev: Option<String>,
    path: Option<String>,
}

// ----------------------------------------------------------------------------
// Public API
// ----------------------------------------------------------------------------

/// Load and validate `qed.toml` from `spec_dir`. Returns `Ok(None)` if no
/// manifest is present (the caller can decide whether that's an error
/// based on whether the spec has any `import` statements). Returns
/// `Ok(Some(_))` on success and `Err(_)` on read / parse / validation
/// failure.
#[allow(dead_code)]
pub fn load_from_dir(spec_dir: &Path) -> Result<Option<Manifest>> {
    let manifest_path = spec_dir.join(MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let raw: RawManifest = toml::from_str(&bytes)
        .with_context(|| format!("parsing {} as TOML", manifest_path.display()))?;
    let manifest =
        validate(raw).with_context(|| format!("validating {}", manifest_path.display()))?;
    Ok(Some(manifest))
}

/// Parse a manifest from a TOML string. Useful for tests and for callers
/// that already have the bytes in memory.
#[allow(dead_code)]
pub fn parse_str(src: &str) -> Result<Manifest> {
    let raw: RawManifest = toml::from_str(src).context("parsing qed.toml")?;
    validate(raw)
}

// ----------------------------------------------------------------------------
// Validation
// ----------------------------------------------------------------------------

fn validate(raw: RawManifest) -> Result<Manifest> {
    let mut deps = BTreeMap::new();
    for (name, raw_dep) in raw.dependencies {
        let dep = validate_dep(&name, raw_dep)?;
        deps.insert(name, dep);
    }
    Ok(Manifest { dependencies: deps })
}

fn validate_dep(name: &str, raw: RawDependency) -> Result<Dependency> {
    let RawDependency {
        github,
        tag,
        branch,
        rev,
        path,
    } = raw;

    match github {
        Some(repo) => {
            // GitHub source. `path` (if present) is the sub-path within the
            // repo, not a local filesystem path.
            let slash_count = repo.matches('/').count();
            if slash_count != 1 {
                bail!(
                    "github source must be in `org/repo` form, got `{}` (expected exactly one `/`)",
                    repo
                );
            }
            if repo.starts_with('/') || repo.ends_with('/') {
                bail!("github source `{}` has empty org or repo segment", repo);
            }

            let git_ref = match (tag, branch, rev) {
                (Some(t), None, None) => GitRef::Tag(t),
                (None, Some(b), None) => GitRef::Branch(b),
                (None, None, Some(r)) => GitRef::Rev(r),
                (None, None, None) => bail!(
                    "github source for `{}` must specify exactly one of `tag`, `branch`, or `rev`",
                    name
                ),
                _ => bail!(
                    "github source for `{}` must specify exactly one of `tag`, `branch`, or `rev` — multiple given",
                    name
                ),
            };

            Ok(Dependency::Github {
                repo,
                git_ref,
                path,
            })
        }
        None => {
            // Must be a path source.
            let p = match path {
                Some(p) => p,
                None => bail!(
                    "dependency `{}` must specify either `github` (with `tag` / `branch` / `rev`) or `path`",
                    name
                ),
            };
            if tag.is_some() || branch.is_some() || rev.is_some() {
                bail!(
                    "path source for `{}` must not have `tag` / `branch` / `rev`",
                    name
                );
            }
            Ok(Dependency::Path { path: p })
        }
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_dep_with_tag() {
        let src = r#"
[dependencies]
spl_token = { github = "QEDGen/solana-skills", path = "interfaces/spl_token", tag = "v2.8.0" }
"#;
        let m = parse_str(src).unwrap();
        let dep = m.dependencies.get("spl_token").unwrap();
        match dep {
            Dependency::Github {
                repo,
                git_ref,
                path,
            } => {
                assert_eq!(repo, "QEDGen/solana-skills");
                assert!(matches!(git_ref, GitRef::Tag(t) if t == "v2.8.0"));
                assert_eq!(path.as_deref(), Some("interfaces/spl_token"));
            }
            _ => panic!("expected Github dep"),
        }
    }

    #[test]
    fn parses_path_dep() {
        let src = r#"
[dependencies]
my_amm = { path = "../my_amm" }
"#;
        let m = parse_str(src).unwrap();
        let dep = m.dependencies.get("my_amm").unwrap();
        match dep {
            Dependency::Path { path } => assert_eq!(path, "../my_amm"),
            _ => panic!("expected Path dep"),
        }
    }

    #[test]
    fn rejects_github_without_ref() {
        let src = r#"
[dependencies]
foo = { github = "a/b" }
"#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(
            err.contains("must specify exactly one of `tag`, `branch`, or `rev`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_github_with_multiple_refs() {
        let src = r#"
[dependencies]
foo = { github = "a/b", tag = "v1", branch = "main" }
"#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(err.contains("multiple given"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_malformed_org_repo() {
        let src = r#"
[dependencies]
foo = { github = "no-slash", tag = "v1" }
"#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(err.contains("org/repo"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_path_with_git_ref_fields() {
        let src = r#"
[dependencies]
foo = { path = "../foo", tag = "v1" }
"#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(
            err.contains("must not have `tag`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_empty_dependency() {
        let src = r#"
[dependencies]
foo = { }
"#;
        let err = parse_str(src).unwrap_err().to_string();
        assert!(
            err.contains("must specify either `github`") || err.contains("path"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_manifest_parses_to_empty_deps() {
        let m = parse_str("").unwrap();
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn load_from_dir_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = load_from_dir(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_from_dir_reads_qed_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(MANIFEST_FILENAME),
            r#"
[dependencies]
spl_token = { github = "QEDGen/solana-skills", path = "interfaces/spl_token", tag = "v2.8.0" }
"#,
        )
        .unwrap();
        let m = load_from_dir(tmp.path()).unwrap().unwrap();
        assert!(m.dependencies.contains_key("spl_token"));
    }
}
