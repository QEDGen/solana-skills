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
    /// Source-side interface name — what the imported `.qedspec`
    /// declares as `interface <bound_name> { ... }`. The merge step
    /// looks up by this name.
    pub bound_name: String,
    /// Manifest dep key (matches `qed.toml`'s `[dependencies]`).
    pub dep_key: String,
    /// Local alias from `import X from "y" as <alias>` (v2.8 F5).
    /// `None` when no alias was declared — the merge keeps `bound_name`
    /// as the local name.
    pub local_alias: Option<String>,
    pub sources: Vec<(PathBuf, String)>,
    pub commit: Option<String>,
}

/// Cache-handling options for github fetches. v2.8 fold-in F7.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct CacheOpts {
    /// `--no-cache`: forcibly clear and refetch every github source.
    /// Path sources are unaffected.
    pub force_refresh: bool,
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
    resolve_imports_with_opts(imports, manifest, manifest_dir, CacheOpts::default())
}

/// `resolve_imports`, with explicit cache-policy controls. Performs
/// **transitive** resolution as of v2.9 G5: imported specs that
/// themselves contain `import` statements are walked depth-first;
/// the resolved set includes both direct and transitive deps.
///
/// Transitive deps are resolved against the **imported spec's own**
/// `qed.toml` (sibling to its source file), not the consumer's
/// manifest. This matches cargo / npm semantics — a peer spec
/// declares its own deps in its own manifest; the consumer doesn't
/// need to repeat them.
///
/// Conflict policy (v2.9 B1, no semver):
/// - Same dep key (`from "..."`) resolving to the same canonical
///   source path → dedupe (silent).
/// - Same dep key resolving to *different* canonical paths →
///   hard-error with the conflicting paths and the import chain
///   that brought each.
/// - Cycle (a transitive walk re-encounters a path already on the
///   current chain) → hard-error with the cycle path.
///
/// Full version-conflict resolution (semver-aware) is v2.10+ scope
/// (B2 from `SCOPING-v2.9.md`).
#[allow(dead_code)]
pub fn resolve_imports_with_opts(
    imports: &[ParsedImport],
    manifest: &Manifest,
    manifest_dir: &Path,
    cache_opts: CacheOpts,
) -> Result<Vec<ResolvedImport>> {
    let mut state = ResolverState::default();
    let mut chain: Vec<String> = Vec::new();
    resolve_recursive(
        imports,
        manifest,
        manifest_dir,
        cache_opts,
        &mut state,
        &mut chain,
    )?;
    Ok(state.resolved)
}

/// Per-invocation resolver state. Tracks the deduplicated set of
/// resolved imports plus the bookkeeping needed for cycle and
/// conflict detection.
#[derive(Default)]
struct ResolverState {
    /// All resolved imports, in DFS-pre-order.
    resolved: Vec<ResolvedImport>,
    /// `dep_key` → canonical source path of the first source. Used
    /// for conflict detection across the dep graph.
    seen: std::collections::HashMap<String, String>,
}

fn resolve_recursive(
    imports: &[ParsedImport],
    manifest: &Manifest,
    manifest_dir: &Path,
    cache_opts: CacheOpts,
    state: &mut ResolverState,
    chain: &mut Vec<String>,
) -> Result<()> {
    for imp in imports {
        let dep = manifest.dependencies.get(&imp.from).ok_or_else(|| {
            anyhow!(
                "import `{}` references manifest dep `{}`, but no such entry in qed.toml under [dependencies] (looking in {})",
                imp.name,
                imp.from,
                manifest_dir.display(),
            )
        })?;

        let res = match dep {
            Dependency::Path { path } => resolve_path_dep(&imp.name, path, manifest_dir)?,
            Dependency::Github {
                repo,
                git_ref,
                path,
            } => resolve_github_dep(&imp.name, repo, git_ref, path.as_deref(), cache_opts)?,
        };

        // Canonical source path = canonicalized first source file.
        // Stable across multi-file deps (sources are returned in
        // sorted order); falls back to the raw path if canonicalize
        // fails (e.g. the file lives under a path that no longer
        // exists when this is called repeatedly).
        let first_source = &res.sources[0].0;
        let canonical = first_source
            .canonicalize()
            .unwrap_or_else(|_| first_source.clone())
            .to_string_lossy()
            .into_owned();

        // Cycle detection: if this path is already on the current
        // resolution chain, we have a cycle.
        if chain.contains(&canonical) {
            anyhow::bail!(
                "import cycle detected:\n  {} -> {}\n  Each consumer must declare its own deps; cyclic peer references are not supported.",
                chain.join("\n    -> "),
                canonical,
            );
        }

        // Conflict detection: same dep_key, different canonical
        // sources. Diagnose with the chain that brought the second
        // path so the user can see the conflict point.
        if let Some(prev_canonical) = state.seen.get(&imp.from) {
            if prev_canonical != &canonical {
                anyhow::bail!(
                    "dep `{}` resolved to two different sources:\n  first:  {}\n  later:  {}\n  reached via: {}\n  v2.9 has no version-conflict resolution; both paths must agree, or one must rename via `import X from \"y\" as Z`.",
                    imp.from,
                    prev_canonical,
                    canonical,
                    if chain.is_empty() {
                        "<direct>".to_string()
                    } else {
                        chain.join(" -> ")
                    },
                );
            }
            // Already resolved with the same source — silently dedupe.
            continue;
        }

        state.seen.insert(imp.from.clone(), canonical.clone());
        state.resolved.push(ResolvedImport {
            bound_name: imp.name.clone(),
            dep_key: imp.from.clone(),
            local_alias: imp.as_name.clone(),
            sources: res.sources.clone(),
            commit: res.commit.clone(),
        });

        // Walk into transitive deps. Imported specs that contain
        // `import` statements need their own qed.toml (sibling to
        // the source file) to resolve; absence is fine and just
        // means no transitive deps.
        let transitive = parse_imports_from_sources(&res.sources).with_context(|| {
            format!(
                "scanning imported spec `{}` for transitive imports",
                imp.name
            )
        })?;
        if !transitive.is_empty() {
            let imported_manifest_dir = first_source.parent().unwrap_or(manifest_dir).to_path_buf();
            let imported_manifest = crate::qed_manifest::load_from_dir(&imported_manifest_dir)?
                .ok_or_else(|| {
                    anyhow!(
                        "imported spec `{}` has {} `import` statement(s) but no `qed.toml` next to it (expected at {})",
                        imp.name,
                        transitive.len(),
                        imported_manifest_dir
                            .join(crate::qed_manifest::MANIFEST_FILENAME)
                            .display(),
                    )
                })?;

            chain.push(canonical);
            resolve_recursive(
                &transitive,
                &imported_manifest,
                &imported_manifest_dir,
                cache_opts,
                state,
                chain,
            )?;
            chain.pop();
        }
    }
    Ok(())
}

/// Parse an imported spec's source bytes just far enough to extract
/// its own `import` statements. Equivalent to a full
/// `chumsky_adapter::parse_str` followed by reading
/// `parsed.imports`, but tolerates minor parse failures: a malformed
/// imported spec doesn't block the resolver from reporting the
/// actual problem (the parse error surfaces when the consumer-side
/// pipeline parses it for real).
fn parse_imports_from_sources(sources: &[(PathBuf, String)]) -> Result<Vec<ParsedImport>> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    // Fast path: single file → parse directly.
    if sources.len() == 1 {
        let parsed = match crate::chumsky_adapter::parse_str(&sources[0].1) {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };
        return Ok(parsed.imports);
    }
    // Multi-file: concatenate via the same logic check.rs uses
    // (parse each fragment, merge AST top items, adapt).
    let mut merged_items = Vec::new();
    let mut merged_name: Option<String> = None;
    for (_, src) in sources {
        let typed = match crate::chumsky_parser::parse(src) {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()),
        };
        if merged_name.is_none() {
            merged_name = Some(typed.name.clone());
        }
        merged_items.extend(typed.items);
    }
    let merged = crate::ast::Spec {
        name: merged_name.unwrap_or_else(|| "Merged".to_string()),
        items: merged_items,
    };
    let parsed = crate::chumsky_adapter::adapt(&merged);
    Ok(parsed.imports)
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
    cache_opts: CacheOpts,
) -> Result<ResolvedSource> {
    let cache = ensure_github_cache(repo, git_ref, cache_opts)
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

fn ensure_github_cache(repo: &str, git_ref: &GitRef, cache_opts: CacheOpts) -> Result<GithubCache> {
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

    // Cache hit: directory exists, we have a recorded commit, --no-cache
    // wasn't requested, and the marker is fresh enough per QEDGEN_CACHE_TTL
    // (default 7 days). Skip the clone entirely — `git rev-parse HEAD`
    // would be cheap, but the marker file lets us skip even spawning git
    // when the cache is warm.
    if !cache_opts.force_refresh
        && dir.exists()
        && commit_marker.exists()
        && !marker_is_stale(&commit_marker)
    {
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

/// Default cache TTL: 7 days. Override via QEDGEN_CACHE_TTL=<seconds>.
/// Branches change content under a stable ref name, so re-fetching every
/// week catches typical cadence drift; tags and revs are immutable
/// content-wise, so the TTL only forces a no-op `git rev-parse HEAD`
/// re-check on those.
const DEFAULT_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

fn cache_ttl_secs() -> u64 {
    std::env::var("QEDGEN_CACHE_TTL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CACHE_TTL_SECS)
}

/// True iff the cache marker is older than the configured TTL. Errors
/// reading mtime fall back to "not stale" — better to use a possibly-
/// stale cache than to refetch on every run because of a flaky stat.
fn marker_is_stale(marker: &Path) -> bool {
    let ttl = cache_ttl_secs();
    if ttl == 0 {
        // TTL=0 disables time-based invalidation.
        return false;
    }
    let Ok(meta) = std::fs::metadata(marker) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
        return false;
    };
    age.as_secs() > ttl
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
            as_name: None,
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

    // ----- v2.9 G5: transitive resolution -----

    #[test]
    fn resolves_three_level_chain_via_transitive_walk() {
        // Layout:
        //   /tmp/<root>/
        //     qed.toml           (consumer's manifest, declares spl_token)
        //     spl_token/
        //       qed.toml         (spl_token's manifest, declares system)
        //       spec.qedspec     (declares interface SplToken)
        //                        + `import System from "system"`
        //     system/
        //       spec.qedspec     (declares interface System)
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let token_dir = root.join("spl_token");
        std::fs::create_dir(&token_dir).unwrap();
        let system_dir = root.join("system");
        std::fs::create_dir(&system_dir).unwrap();

        std::fs::write(
            system_dir.join("spec.qedspec"),
            r#"spec SystemIface
interface System {
  program_id "11111111111111111111111111111111"
}
"#,
        )
        .unwrap();

        std::fs::write(
            token_dir.join("qed.toml"),
            r#"
[dependencies]
system = { path = "../system/spec.qedspec" }
"#,
        )
        .unwrap();
        std::fs::write(
            token_dir.join("spec.qedspec"),
            r#"spec SplTokenIface
import System from "system"
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
}
"#,
        )
        .unwrap();

        // Consumer's manifest only declares the direct dep; transitive
        // walk discovers `system` through spl_token's own qed.toml.
        std::fs::write(
            root.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "spl_token/spec.qedspec" }
"#,
        )
        .unwrap();

        let manifest = crate::qed_manifest::load_from_dir(root).unwrap().unwrap();
        let imports = vec![imp("Token", "spl_token")];
        let resolved = resolve_imports(&imports, &manifest, root).unwrap();

        // Both spl_token (direct) and system (transitive) should land.
        assert_eq!(
            resolved.len(),
            2,
            "expected 2 resolved deps; got {:?}",
            resolved.iter().map(|r| &r.dep_key).collect::<Vec<_>>()
        );
        let keys: Vec<&str> = resolved.iter().map(|r| r.dep_key.as_str()).collect();
        assert!(keys.contains(&"spl_token"));
        assert!(keys.contains(&"system"));
    }

    #[test]
    fn dedupes_when_same_dep_reached_via_two_paths() {
        // Diamond: consumer imports A and B; both A and B import C.
        // C should appear once in the resolved set.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let a = root.join("a");
        let b = root.join("b");
        let c = root.join("c");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::create_dir(&c).unwrap();

        std::fs::write(
            c.join("spec.qedspec"),
            "spec C\ninterface C { program_id \"22222222222222222222222222222222\" }\n",
        )
        .unwrap();
        for (dir, name) in [(&a, "A"), (&b, "B")] {
            std::fs::write(
                dir.join("qed.toml"),
                "[dependencies]\nc = { path = \"../c/spec.qedspec\" }\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("spec.qedspec"),
                format!(
                    "spec {0}\nimport C from \"c\"\ninterface {0} {{ program_id \"3333333333333333333333333333333{0}\" }}\n",
                    name
                ),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("qed.toml"),
            r#"
[dependencies]
a = { path = "a/spec.qedspec" }
b = { path = "b/spec.qedspec" }
"#,
        )
        .unwrap();

        let manifest = crate::qed_manifest::load_from_dir(root).unwrap().unwrap();
        let imports = vec![imp("A", "a"), imp("B", "b")];
        let resolved = resolve_imports(&imports, &manifest, root).unwrap();

        // a, b, and c — c only once despite the diamond.
        assert_eq!(resolved.len(), 3);
        let c_count = resolved.iter().filter(|r| r.dep_key == "c").count();
        assert_eq!(
            c_count, 1,
            "diamond should dedupe; c appeared {c_count} times"
        );
    }

    #[test]
    fn errors_on_cycle_in_transitive_chain() {
        // a/spec.qedspec imports b; b imports a. Resolver should
        // detect the cycle.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = root.join("a");
        let b = root.join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();

        std::fs::write(
            a.join("qed.toml"),
            "[dependencies]\nb = { path = \"../b/spec.qedspec\" }\n",
        )
        .unwrap();
        std::fs::write(
            a.join("spec.qedspec"),
            "spec A\nimport B from \"b\"\ninterface A { program_id \"a\" }\n",
        )
        .unwrap();
        std::fs::write(
            b.join("qed.toml"),
            "[dependencies]\na = { path = \"../a/spec.qedspec\" }\n",
        )
        .unwrap();
        std::fs::write(
            b.join("spec.qedspec"),
            "spec B\nimport A from \"a\"\ninterface B { program_id \"b\" }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("qed.toml"),
            "[dependencies]\na = { path = \"a/spec.qedspec\" }\n",
        )
        .unwrap();

        let manifest = crate::qed_manifest::load_from_dir(root).unwrap().unwrap();
        let imports = vec![imp("A", "a")];
        let err = resolve_imports(&imports, &manifest, root)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("import cycle detected"),
            "expected cycle error; got: {err}"
        );
    }

    #[test]
    fn errors_on_conflicting_versions_for_same_dep_key() {
        // Consumer imports A and B; A imports C from path X; B imports
        // C from path Y. Same dep key, different sources → conflict.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = root.join("a");
        let b = root.join("b");
        let c1 = root.join("c1");
        let c2 = root.join("c2");
        for d in [&a, &b, &c1, &c2] {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(
            c1.join("spec.qedspec"),
            "spec C1\ninterface C { program_id \"c1\" }\n",
        )
        .unwrap();
        std::fs::write(
            c2.join("spec.qedspec"),
            "spec C2\ninterface C { program_id \"c2\" }\n",
        )
        .unwrap();
        // A imports c from c1
        std::fs::write(
            a.join("qed.toml"),
            "[dependencies]\nc = { path = \"../c1/spec.qedspec\" }\n",
        )
        .unwrap();
        std::fs::write(
            a.join("spec.qedspec"),
            "spec A\nimport C from \"c\"\ninterface A { program_id \"a\" }\n",
        )
        .unwrap();
        // B imports c from c2
        std::fs::write(
            b.join("qed.toml"),
            "[dependencies]\nc = { path = \"../c2/spec.qedspec\" }\n",
        )
        .unwrap();
        std::fs::write(
            b.join("spec.qedspec"),
            "spec B\nimport C from \"c\"\ninterface B { program_id \"b\" }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("qed.toml"),
            "[dependencies]\na = { path = \"a/spec.qedspec\" }\nb = { path = \"b/spec.qedspec\" }\n",
        )
        .unwrap();

        let manifest = crate::qed_manifest::load_from_dir(root).unwrap().unwrap();
        let imports = vec![imp("A", "a"), imp("B", "b")];
        let err = resolve_imports(&imports, &manifest, root)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("resolved to two different sources"),
            "expected conflict error; got: {err}"
        );
        assert!(err.contains("c1"), "should mention first source: {err}");
        assert!(
            err.contains("c2"),
            "should mention conflicting source: {err}"
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
    fn marker_is_stale_disabled_when_ttl_is_zero() {
        // QEDGEN_CACHE_TTL=0 disables the time-based staleness check —
        // useful when users want to defer cache invalidation entirely
        // to --no-cache.
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join(".qedgen-commit");
        std::fs::write(&marker, "abc123").unwrap();
        std::env::set_var("QEDGEN_CACHE_TTL", "0");
        assert!(!marker_is_stale(&marker));
        std::env::remove_var("QEDGEN_CACHE_TTL");
    }

    // Note: the "TTL exceeded → stale" path is genuinely time-and-env-
    // dependent, and rust's parallel-by-default test runner makes
    // QEDGEN_CACHE_TTL races flaky. The unit coverage above (TTL=0
    // short-circuits, missing marker is non-stale, cache_ttl_secs reads
    // env) covers the moving parts; the cumulative path is exercised in
    // ad-hoc integration runs (not part of cargo test).

    #[test]
    fn marker_is_stale_returns_false_for_missing_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("does-not-exist");
        // Stat fails → fall back to "not stale" (don't gratuitously refetch).
        assert!(!marker_is_stale(&marker));
    }

    #[test]
    fn cache_ttl_secs_defaults_to_one_week() {
        std::env::remove_var("QEDGEN_CACHE_TTL");
        assert_eq!(cache_ttl_secs(), 7 * 24 * 60 * 60);
    }

    #[test]
    fn cache_ttl_secs_honors_env_override() {
        std::env::set_var("QEDGEN_CACHE_TTL", "3600");
        assert_eq!(cache_ttl_secs(), 3600);
        std::env::remove_var("QEDGEN_CACHE_TTL");
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
