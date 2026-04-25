//! Import resolver — fetches sources for `import Name from "key"` (v2.8 G1).
//!
//! Consumes a `Manifest` (parsed from `qed.toml`) and a list of
//! `ParsedImport` statements, and returns the source bytes for each
//! imported spec — fetched from GitHub for `Dependency::Github` or read
//! from disk for `Dependency::Path`.
//!
//! Per `feedback_dispatch_over_reimplement.md`, GitHub fetches shell out
//! to the system `git` binary rather than pulling in `git2`. For `Tag`
//! and `Branch` refs we use `git clone --depth=1 --branch <ref>`; for
//! `Rev` (commit hash) we clone the default branch and `git checkout
//! <rev>` afterwards.
//!
//! Cache layout: `<cache_root>/github/<org>/<repo>/<kind>/<ref>/`. The
//! cache root defaults to `~/.qedgen/cache` and can be overridden via
//! the `QEDGEN_CACHE_DIR` env var (used by tests to avoid polluting the
//! user's real cache).
//!
//! v2.8 scope:
//! - Single-level resolution. Imported specs that themselves contain
//!   `import` statements are not transitively resolved — each consumer
//!   is responsible for declaring its own direct dependencies. This
//!   matches stance 1 from `docs/design/spec-composition.md`.
//! - No lock-file integration; that lands in M1.5 once the resolver is
//!   wired into the parse pipeline.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::check::ParsedImport;
use crate::qed_manifest::{Dependency, GitRef, Manifest};

/// Source bytes for one resolved import. `sources` is a list of
/// `(path, bytes)` pairs — single-element when the dependency points at
/// one `.qedspec` file, multi-element when it points at a directory of
/// fragments. The `commit` field is `Some(hash)` for GitHub sources and
/// `None` for path sources.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ResolvedImport {
    pub bound_name: String,
    pub dep_key: String,
    pub sources: Vec<(PathBuf, String)>,
    pub commit: Option<String>,
}

/// Resolve every `import` statement against the manifest, fetch sources,
/// and return them. Errors carry enough context to point the user at the
/// offending import or manifest entry.
#[allow(dead_code)]
pub fn resolve_imports(
    imports: &[ParsedImport],
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Result<Vec<ResolvedImport>> {
    let mut resolved = Vec::with_capacity(imports.len());
    for imp in imports {
        let dep = manifest.dependencies.get(&imp.from).ok_or_else(|| {
            anyhow!(
                "import `{}` references manifest dep `{}`, but no such entry in qed.toml under [dependencies]",
                imp.name,
                imp.from
            )
        })?;

        let res = match dep {
            Dependency::Path { path } => resolve_path_dep(&imp.name, path, manifest_dir)?,
            Dependency::Github {
                repo,
                git_ref,
                path,
            } => resolve_github_dep(&imp.name, repo, git_ref, path.as_deref())?,
        };

        resolved.push(ResolvedImport {
            bound_name: imp.name.clone(),
            dep_key: imp.from.clone(),
            sources: res.sources,
            commit: res.commit,
        });
    }
    Ok(resolved)
}

// ----------------------------------------------------------------------------
// Path source
// ----------------------------------------------------------------------------

struct ResolvedSource {
    sources: Vec<(PathBuf, String)>,
    commit: Option<String>,
}

fn resolve_path_dep(
    bound_name: &str,
    rel_path: &str,
    manifest_dir: &Path,
) -> Result<ResolvedSource> {
    let target = if Path::new(rel_path).is_absolute() {
        PathBuf::from(rel_path)
    } else {
        manifest_dir.join(rel_path)
    };

    // No `canonicalize` here: the auto-extension fallback inside
    // `read_spec_sources` needs to handle the case where `target` doesn't
    // exist on disk yet (because the user wrote `path = "token"` and the
    // real file is `token.qedspec`).
    let sources = read_spec_sources(&target)
        .with_context(|| format!("resolving path dep for `{}`", bound_name))?;

    Ok(ResolvedSource {
        sources,
        commit: None,
    })
}

// ----------------------------------------------------------------------------
// GitHub source
// ----------------------------------------------------------------------------

fn resolve_github_dep(
    bound_name: &str,
    repo: &str,
    git_ref: &GitRef,
    sub_path: Option<&str>,
) -> Result<ResolvedSource> {
    let cache = ensure_github_cache(repo, git_ref)
        .with_context(|| format!("fetching `{}` ({}@{})", bound_name, repo, git_ref.as_str()))?;

    let target = match sub_path {
        Some(p) => cache.dir.join(p),
        None => cache.dir.clone(),
    };

    let sources = read_spec_sources(&target).with_context(|| {
        format!(
            "loading spec source for `{}` from {} (sub-path {:?})",
            bound_name,
            cache.dir.display(),
            sub_path,
        )
    })?;

    Ok(ResolvedSource {
        sources,
        commit: Some(cache.commit),
    })
}

struct GithubCache {
    dir: PathBuf,
    commit: String,
}

fn ensure_github_cache(repo: &str, git_ref: &GitRef) -> Result<GithubCache> {
    let cache_root = cache_root();
    let (org, name) = split_repo(repo)?;
    let kind = git_ref.cache_kind();
    let ref_safe = sanitize_for_path(git_ref.as_str());
    let dir = cache_root
        .join("github")
        .join(org)
        .join(name)
        .join(kind)
        .join(&ref_safe);

    let commit_marker = dir.join(".qedgen-commit");

    // Cache hit: directory exists and we have a recorded commit. Skip the
    // clone entirely — `git rev-parse HEAD` would be cheap, but the marker
    // file lets us skip even spawning git when the cache is warm.
    if dir.exists() && commit_marker.exists() {
        let commit = std::fs::read_to_string(&commit_marker)
            .with_context(|| format!("reading cache marker {}", commit_marker.display()))?
            .trim()
            .to_string();
        if !commit.is_empty() {
            return Ok(GithubCache { dir, commit });
        }
    }

    // Cache miss (or partial). Wipe any partial state and clone fresh.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("clearing partial cache at {}", dir.display()))?;
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating cache parent {}", parent.display()))?;
    }

    let url = format!("https://github.com/{}.git", repo);
    match git_ref {
        GitRef::Tag(r) | GitRef::Branch(r) => {
            run_git(&[
                "clone",
                "--depth=1",
                "--branch",
                r,
                "--single-branch",
                &url,
                dir.to_string_lossy().as_ref(),
            ])
            .with_context(|| format!("git clone --branch {} {}", r, url))?;
        }
        GitRef::Rev(rev) => {
            // Commit hash needs the full default branch, then checkout.
            run_git(&["clone", &url, dir.to_string_lossy().as_ref()])
                .with_context(|| format!("git clone {}", url))?;
            run_git_in(&dir, &["checkout", rev])
                .with_context(|| format!("git checkout {}", rev))?;
        }
    }

    let commit = run_git_in(&dir, &["rev-parse", "HEAD"])
        .context("capturing resolved commit hash")?
        .trim()
        .to_string();

    std::fs::write(&commit_marker, &commit)
        .with_context(|| format!("writing cache marker {}", commit_marker.display()))?;

    Ok(GithubCache { dir, commit })
}

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    let mut parts = repo.splitn(2, '/');
    let org = parts.next().filter(|s| !s.is_empty());
    let name = parts.next().filter(|s| !s.is_empty());
    match (org, name) {
        (Some(o), Some(n)) if !n.contains('/') => Ok((o, n)),
        _ => bail!("malformed github source `{}`; expected `org/repo`", repo),
    }
}

/// Replace path-unsafe characters in a ref so it can be a directory name.
/// Tags like `v2.8.0` and branches like `main` pass through; refs with
/// slashes (e.g. `release/2.8`) get flattened.
fn sanitize_for_path(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

fn cache_root() -> PathBuf {
    if let Ok(env) = std::env::var("QEDGEN_CACHE_DIR") {
        return PathBuf::from(env);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".qedgen").join("cache")
}

fn run_git(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .context("invoking `git` (is it in PATH?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn run_git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("invoking `git` (is it in PATH?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git -C {} {} failed: {}",
            dir.display(),
            args.join(" "),
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ----------------------------------------------------------------------------
// Spec source loading (path or cache-rooted)
// ----------------------------------------------------------------------------

/// Resolve a path that may be a `.qedspec` file, a directory of fragments,
/// or an extension-less file alias (e.g. `interfaces/spl_token` → load
/// `interfaces/spl_token.qedspec`).
fn read_spec_sources(target: &Path) -> Result<Vec<(PathBuf, String)>> {
    if target.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(target)
            .with_context(|| format!("reading directory {}", target.display()))?
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "qedspec"))
            .collect();
        entries.sort(); // deterministic merge order matches `parse_spec_dir`.
        if entries.is_empty() {
            bail!("no `.qedspec` files found under {}", target.display());
        }
        let mut sources = Vec::with_capacity(entries.len());
        for path in entries {
            let bytes = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            sources.push((path, bytes));
        }
        Ok(sources)
    } else if target.is_file() {
        let bytes = std::fs::read_to_string(target)
            .with_context(|| format!("reading {}", target.display()))?;
        Ok(vec![(target.to_path_buf(), bytes)])
    } else {
        // Try auto-extension: `interfaces/spl_token` → `interfaces/spl_token.qedspec`.
        let with_ext = {
            let mut p = target.to_path_buf();
            let new_name = match target.file_name() {
                Some(n) => format!("{}.qedspec", n.to_string_lossy()),
                None => bail!(
                    "spec source path {} has no file name component",
                    target.display()
                ),
            };
            p.set_file_name(new_name);
            p
        };
        if with_ext.is_file() {
            let bytes = std::fs::read_to_string(&with_ext)
                .with_context(|| format!("reading {}", with_ext.display()))?;
            Ok(vec![(with_ext, bytes)])
        } else {
            bail!(
                "no spec source at {} (also tried {})",
                target.display(),
                with_ext.display()
            );
        }
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qed_manifest::Manifest;
    use std::collections::BTreeMap;

    fn manifest_with(deps: Vec<(&str, Dependency)>) -> Manifest {
        let mut m = BTreeMap::new();
        for (k, v) in deps {
            m.insert(k.to_string(), v);
        }
        Manifest { dependencies: m }
    }

    fn imp(name: &str, from: &str) -> ParsedImport {
        ParsedImport {
            name: name.to_string(),
            from: from.to_string(),
        }
    }

    #[test]
    fn resolves_path_source_pointing_at_single_file() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        let spec_path = manifest_dir.join("token.qedspec");
        std::fs::write(
            &spec_path,
            "spec Token\ninterface Token { program_id \"x\" }\n",
        )
        .unwrap();

        let manifest = manifest_with(vec![(
            "spl_token",
            Dependency::Path {
                path: "token.qedspec".to_string(),
            },
        )]);
        let imports = vec![imp("Token", "spl_token")];

        let resolved = resolve_imports(&imports, &manifest, manifest_dir).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].bound_name, "Token");
        assert_eq!(resolved[0].dep_key, "spl_token");
        assert_eq!(resolved[0].sources.len(), 1);
        assert!(resolved[0].sources[0].1.contains("interface Token"));
        assert!(resolved[0].commit.is_none());
    }

    #[test]
    fn resolves_path_source_pointing_at_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        let dep_dir = manifest_dir.join("local-amm");
        std::fs::create_dir(&dep_dir).unwrap();
        std::fs::write(dep_dir.join("a.qedspec"), "spec MyAmm\n").unwrap();
        std::fs::write(dep_dir.join("b.qedspec"), "spec MyAmm\n").unwrap();
        // A non-qedspec file should be ignored.
        std::fs::write(dep_dir.join("README.md"), "ignore me").unwrap();

        let manifest = manifest_with(vec![(
            "amm",
            Dependency::Path {
                path: "local-amm".to_string(),
            },
        )]);
        let imports = vec![imp("MyAmm", "amm")];

        let resolved = resolve_imports(&imports, &manifest, manifest_dir).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].sources.len(),
            2,
            "should load both .qedspec files"
        );
        // Sorted by path → a.qedspec first.
        assert!(resolved[0].sources[0].0.ends_with("a.qedspec"));
        assert!(resolved[0].sources[1].0.ends_with("b.qedspec"));
    }

    #[test]
    fn resolves_path_source_with_auto_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        std::fs::write(manifest_dir.join("token.qedspec"), "spec Token\n").unwrap();

        // path = "token" (no extension) should resolve to token.qedspec.
        let manifest = manifest_with(vec![(
            "tok",
            Dependency::Path {
                path: "token".to_string(),
            },
        )]);
        let imports = vec![imp("Token", "tok")];

        let resolved = resolve_imports(&imports, &manifest, manifest_dir).unwrap();
        assert_eq!(resolved[0].sources.len(), 1);
        assert!(resolved[0].sources[0].0.ends_with("token.qedspec"));
    }

    #[test]
    fn errors_when_import_references_unknown_dep_key() {
        let manifest = manifest_with(vec![]);
        let imports = vec![imp("Token", "spl_token")];

        let err = resolve_imports(&imports, &manifest, Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no such entry in qed.toml"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn errors_on_missing_path_source() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = manifest_with(vec![(
            "missing",
            Dependency::Path {
                path: "does_not_exist".to_string(),
            },
        )]);
        let imports = vec![imp("X", "missing")];

        let err = resolve_imports(&imports, &manifest, tmp.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("resolving path dep") || err.contains("does_not_exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn errors_on_directory_with_no_qedspec_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dep_dir = tmp.path().join("empty-deps");
        std::fs::create_dir(&dep_dir).unwrap();
        std::fs::write(dep_dir.join("README.md"), "no qedspec here").unwrap();

        let manifest = manifest_with(vec![(
            "empty",
            Dependency::Path {
                path: "empty-deps".to_string(),
            },
        )]);
        let imports = vec![imp("X", "empty")];

        // Use `{:#}` to format the full error chain — the "no `.qedspec`
        // files" message is the root cause, which `.to_string()` buries.
        let err = format!(
            "{:#}",
            resolve_imports(&imports, &manifest, tmp.path()).unwrap_err()
        );
        assert!(
            err.contains("no `.qedspec` files"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn split_repo_accepts_org_repo() {
        let (org, name) = split_repo("QEDGen/solana-skills").unwrap();
        assert_eq!(org, "QEDGen");
        assert_eq!(name, "solana-skills");
    }

    #[test]
    fn split_repo_rejects_no_slash() {
        assert!(split_repo("noslash").is_err());
    }

    #[test]
    fn split_repo_rejects_extra_slash() {
        assert!(split_repo("a/b/c").is_err());
    }

    #[test]
    fn sanitize_for_path_passes_through_simple_refs() {
        assert_eq!(sanitize_for_path("v2.8.0"), "v2.8.0");
        assert_eq!(sanitize_for_path("main"), "main");
    }

    #[test]
    fn sanitize_for_path_replaces_slashes() {
        assert_eq!(sanitize_for_path("release/2.8"), "release_2.8");
    }

    #[test]
    fn cache_root_honors_env_override() {
        // Capture and restore so other tests aren't disturbed.
        let prev = std::env::var("QEDGEN_CACHE_DIR").ok();
        std::env::set_var("QEDGEN_CACHE_DIR", "/tmp/qedgen-test-cache");
        let root = cache_root();
        assert_eq!(root, PathBuf::from("/tmp/qedgen-test-cache"));
        if let Some(p) = prev {
            std::env::set_var("QEDGEN_CACHE_DIR", p);
        } else {
            std::env::remove_var("QEDGEN_CACHE_DIR");
        }
    }

    /// GitHub fetch via shell-out — only runs when explicitly opted in.
    /// CI sets `QEDGEN_TEST_NETWORK=1` for the network smoke test;
    /// developers running `cargo test` locally don't get charged the
    /// clone time.
    #[test]
    fn github_fetch_smoke() {
        if std::env::var("QEDGEN_TEST_NETWORK").is_err() {
            return; // skipped silently
        }
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("QEDGEN_CACHE_DIR", tmp.path());

        let manifest = manifest_with(vec![(
            "skills",
            Dependency::Github {
                repo: "QEDGen/solana-skills".to_string(),
                git_ref: GitRef::Tag("v2.7.2".to_string()),
                path: Some("README".to_string()),
            },
        )]);
        let imports = vec![imp("Skills", "skills")];

        // The actual repo doesn't have a `.qedspec` at `README` — we expect
        // a "no spec source" error, not a network error. That's enough to
        // verify clone+checkout+rev-parse all worked end-to-end.
        let err = resolve_imports(&imports, &manifest, Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no spec source") || err.contains("no `.qedspec`"),
            "expected post-clone resolution error, got: {err}"
        );
    }
}
