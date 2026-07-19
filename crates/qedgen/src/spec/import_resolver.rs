//! Import resolver — fetches sources for `import Name from "key"` against a
//! `qed.toml` `Manifest` (GitHub or path deps).
//!
//! GitHub fetches shell out to the system `git` binary (no `git2`): `Tag` /
//! `Branch` use `git clone --depth=1 --branch <ref>`; `Rev` clones the
//! default branch then `git checkout <rev>`. Cache layout:
//! `<cache_root>/github/<org>/<repo>/<kind>/<ref>/`, root `~/.qedgen/cache`,
//! overridable via `QEDGEN_CACHE_DIR` (tests use this).

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
pub struct ResolvedImport {
    /// Source-side interface name — what the imported `.qedspec`
    /// declares as `interface <bound_name> { ... }`. The merge step
    /// looks up by this name.
    pub bound_name: String,
    /// Manifest dep key (matches `qed.toml`'s `[dependencies]`).
    pub dep_key: String,
    /// Local alias from `import X from "y" as <alias>`; `None` keeps
    /// `bound_name` as the local name.
    pub local_alias: Option<String>,
    pub sources: Vec<(PathBuf, String)>,
    pub commit: Option<String>,
    /// True iff the provider ships a Lake-buildable proof package — both
    /// `<source_dir>/.qed/proofs/<bound_name>.lean` and a sibling
    /// `lakefile.lean` present. When true, consumer codegen skips its local
    /// sibling axiom module and emits a lakefile `require` against
    /// `proof_pkg_root` instead.
    pub has_proofs: bool,
    /// Directory containing the provider's proof package; `Some` iff
    /// `has_proofs`. The consumer's lakefile rewrite computes a relative
    /// path to it.
    pub proof_pkg_root: Option<PathBuf>,
}

/// Cache-handling options for github fetches.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheOpts {
    /// `--no-cache`: forcibly clear and refetch every github source.
    /// Path sources are unaffected.
    pub force_refresh: bool,
}

/// Resolve every `import` statement against the manifest, fetch sources,
/// and return them. Errors carry enough context to point the user at the
/// offending import or manifest entry.
#[cfg_attr(not(test), allow(dead_code))] // production passes explicit CacheOpts via resolve_imports_with_opts; kept as the test entry
pub fn resolve_imports(
    imports: &[ParsedImport],
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Result<Vec<ResolvedImport>> {
    resolve_imports_with_opts(imports, manifest, manifest_dir, CacheOpts::default())
}

/// `resolve_imports` with explicit cache-policy controls. Resolution is
/// **transitive** (depth-first); transitive deps resolve against the
/// **imported spec's own** `qed.toml` (sibling to its source), not the
/// consumer's manifest — cargo / npm semantics.
///
/// Conflict policy (no semver):
/// - Same dep key → same canonical source path: silent dedupe.
/// - Same dep key → *different* canonical paths: hard-error with both
///   paths and the import chain.
/// - Cycle (path re-encountered on the current chain): hard-error.
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

/// Per-invocation resolver state: deduped results + cycle/conflict bookkeeping.
#[derive(Default)]
struct ResolverState {
    /// All resolved imports, in DFS-pre-order.
    resolved: Vec<ResolvedImport>,
    /// `dep_key` → canonical first-source path, for conflict detection.
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
        // Builtin stdlib short-circuit: bundled fixtures are always
        // available without a manifest entry — materialize to the cache
        // and resolve as a path dep.
        let res = if let Some(builtin) = builtin_source(&imp.from) {
            resolve_builtin_dep(&imp.from, builtin)?
        } else {
            let dep = manifest.dependencies.get(&imp.from).ok_or_else(|| {
                // Name the manifest actually searched, absolutely — a
                // relative manifest_dir can be empty (spec at cwd), which
                // rendered as "(looking in )" (#254).
                let searched = manifest_dir
                    .canonicalize()
                    .ok()
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .map(|cwd| cwd.join(manifest_dir))
                    })
                    .unwrap_or_else(|| manifest_dir.to_path_buf())
                    .join("qed.toml");
                anyhow!(
                    "import `{}` references manifest dep `{}`, but no such entry in qed.toml under [dependencies] (looking in {})",
                    imp.name,
                    imp.from,
                    searched.display(),
                )
            })?;
            match dep {
                Dependency::Path { path } => resolve_path_dep(&imp.name, path, manifest_dir)?,
                Dependency::Github {
                    repo,
                    git_ref,
                    path,
                } => resolve_github_dep(&imp.name, repo, git_ref, path.as_deref(), cache_opts)?,
            }
        };

        // Canonical source path = canonicalized first source file — stable
        // across multi-file deps (sources are sorted); falls back to the
        // raw path if canonicalize fails.
        let first_source = &res.sources[0].0;
        let canonical = first_source
            .canonicalize()
            .unwrap_or_else(|_| first_source.clone())
            .to_string_lossy()
            .into_owned();

        if chain.contains(&canonical) {
            anyhow::bail!(
                "import cycle detected:\n  {} -> {}\n  Each consumer must declare its own deps; cyclic peer references are not supported.",
                chain.join("\n    -> "),
                canonical,
            );
        }

        // Conflict: same dep_key, different canonical sources — diagnose
        // with the chain that brought the second path.
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

        // Detect a Lake-buildable proof package alongside the qedspec:
        // `<source_dir>/.qed/proofs/<bound_name>.lean` plus a sibling
        // `lakefile.lean` (see `lean_gen::proof_pkg_name`). Both required —
        // the consumer's `require` needs a Lake package to point at.
        let source_dir = first_source.parent().unwrap_or(Path::new("."));
        let proof_module = source_dir
            .join(".qed")
            .join("proofs")
            .join(format!("{}.lean", imp.name));
        let proof_lakefile = source_dir.join(".qed").join("proofs").join("lakefile.lean");
        let (has_proofs, proof_pkg_root) = if proof_module.is_file() && proof_lakefile.is_file() {
            (true, Some(source_dir.join(".qed").join("proofs")))
        } else {
            (false, None)
        };

        state.seen.insert(imp.from.clone(), canonical.clone());
        state.resolved.push(ResolvedImport {
            bound_name: imp.name.clone(),
            dep_key: imp.from.clone(),
            local_alias: imp.as_name.clone(),
            sources: res.sources.clone(),
            commit: res.commit.clone(),
            has_proofs,
            proof_pkg_root,
        });

        // Transitive deps resolve against the imported spec's own qed.toml
        // (sibling to its source); absence just means no transitive deps.
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

/// Extract an imported spec's own `import` statements. Tolerates parse
/// failures — a malformed import doesn't block the resolver; the parse
/// error surfaces when the consumer-side pipeline parses it for real.
fn parse_imports_from_sources(sources: &[(PathBuf, String)]) -> Result<Vec<ParsedImport>> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    if sources.len() == 1 {
        let parsed = match crate::chumsky_adapter::parse_str(&sources[0].1) {
            Ok(p) => p,
            Err(_) => return Ok(Vec::new()),
        };
        return Ok(parsed.imports);
    }
    // Multi-file: same merge logic as check.rs (parse each fragment,
    // merge AST top items, adapt).
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
// Builtin stdlib
// ----------------------------------------------------------------------------

/// Bundled SPL Token interface fixture. Tier 1 — `ensures` clauses backed
/// by an `upstream { binary_hash = ... }` pin so callers can discharge
/// post-conditions via the bundled axiom module instead of `sorry`.
const BUILTIN_SPL_TOKEN: &str = include_str!("../../data/interfaces/spl_token.qedspec");

/// Bundled System Program interface fixture (Tier 1).
const BUILTIN_SYSTEM: &str = include_str!("../../data/interfaces/system.qedspec");

/// Bundled Metaplex Token Metadata interface fixture (Tier 1).
const BUILTIN_METAPLEX: &str = include_str!("../../data/interfaces/metaplex.qedspec");

// Bundled proof packages: written alongside the materialized qedspec, so
// the standard proof-detection logic in `resolve_recursive` picks them up.
//
// System Program intentionally has no bundled proof package:
// `create_account` / `assign` have `Pubkey` params that would force the
// bundled module to `import QEDGen.Solana.Account` and thus a `require
// qedgenSupport` in the bundled lakefile — defeating self-contained
// distribution. `import System from "system"` stays axiom-from-local-
// sibling-module.
const BUILTIN_SPL_PROOF_LAKEFILE: &str = include_str!("../../data/proofs/spl/lakefile.lean");
const BUILTIN_SPL_PROOF_MODULE: &str = include_str!("../../data/proofs/spl/Token.lean");
const BUILTIN_METAPLEX_PROOF_LAKEFILE: &str =
    include_str!("../../data/proofs/metaplex/lakefile.lean");
const BUILTIN_METAPLEX_PROOF_MODULE: &str =
    include_str!("../../data/proofs/metaplex/Metadata.lean");

/// `(module_filename, module_source, lakefile_source)` for a builtin's
/// bundled proof package; `None` for builtins with none (System — see above).
fn builtin_proof_pkg(key: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match key {
        "spl" => Some((
            "Token.lean",
            BUILTIN_SPL_PROOF_MODULE,
            BUILTIN_SPL_PROOF_LAKEFILE,
        )),
        "metaplex" => Some((
            "Metadata.lean",
            BUILTIN_METAPLEX_PROOF_MODULE,
            BUILTIN_METAPLEX_PROOF_LAKEFILE,
        )),
        _ => None,
    }
}

/// Returns the bundled `.qedspec` source for a builtin import key, or
/// `None` if `key` isn't a recognized builtin.
///
/// Recognized keys (case-sensitive):
/// - `"spl"` → SPL Token program
/// - `"system"` → System Program
/// - `"metaplex"` → Metaplex Token Metadata program
pub fn builtin_source(key: &str) -> Option<&'static str> {
    match key {
        "spl" => Some(BUILTIN_SPL_TOKEN),
        "system" => Some(BUILTIN_SYSTEM),
        "metaplex" => Some(BUILTIN_METAPLEX),
        _ => None,
    }
}

/// True iff every import key is a recognized builtin. Used by
/// `check.rs::resolve_and_merge_imports` to skip the `qed.toml`
/// requirement when consumers only use bundled stdlib interfaces.
pub fn all_imports_are_builtins(imports: &[ParsedImport]) -> bool {
    !imports.is_empty() && imports.iter().all(|i| builtin_source(&i.from).is_some())
}

/// Materialize a builtin fixture to `<cache_root>/builtin/<key>/<key>.qedspec`
/// and return its source. The path is stable across runs — the resolver's
/// cycle/conflict dedup needs stable canonical paths — but contents are
/// refreshed whenever the bundled `include_str!` source differs from the
/// on-disk copy, so a qedgen upgrade is visible without manually clearing
/// `~/.qedgen/cache/builtin`.
fn resolve_builtin_dep(key: &str, source: &'static str) -> Result<ResolvedSource> {
    let cache_root = cache_root();
    let builtin_dir = cache_root.join("builtin").join(key);
    std::fs::create_dir_all(&builtin_dir)
        .with_context(|| format!("creating builtin cache dir {}", builtin_dir.display()))?;
    let file_path = builtin_dir.join(format!("{}.qedspec", key));
    let needs_write = match std::fs::read_to_string(&file_path) {
        Ok(existing) => existing != source,
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&file_path, source)
            .with_context(|| format!("materializing builtin fixture {}", file_path.display()))?;
    }

    // Also materialize the bundled proof package (if any) at the
    // conventional `<source_dir>/.qed/proofs/` location so the standard
    // proof-detection logic populates `verified_callees` with no
    // builtin-aware code path. Same content-refresh rule as the qedspec.
    if let Some((module_filename, module_src, lakefile_src)) = builtin_proof_pkg(key) {
        let proofs_dir = builtin_dir.join(".qed").join("proofs");
        std::fs::create_dir_all(&proofs_dir)
            .with_context(|| format!("creating bundled proof-pkg dir {}", proofs_dir.display()))?;
        let lakefile_path = proofs_dir.join("lakefile.lean");
        let module_path = proofs_dir.join(module_filename);
        for (path, content) in [(&lakefile_path, lakefile_src), (&module_path, module_src)] {
            let needs = match std::fs::read_to_string(path) {
                Ok(existing) => existing != content,
                Err(_) => true,
            };
            if needs {
                std::fs::write(path, content).with_context(|| {
                    format!("materializing bundled proof-pkg file {}", path.display())
                })?;
            }
        }
    }

    Ok(ResolvedSource {
        sources: vec![(file_path, source.to_string())],
        commit: None,
    })
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
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set<V: AsRef<std::ffi::OsStr>>(key: &'static str, value: V) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

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

    // ----- verified-callee detection -----

    #[test]
    fn detects_proof_package_alongside_qedspec() {
        // Layout:
        //   <root>/
        //     qed.toml
        //     token.qedspec
        //     .qed/proofs/
        //       Token.lean
        //       lakefile.lean
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        std::fs::write(
            manifest_dir.join("token.qedspec"),
            "spec Token\ninterface Token { program_id \"x\" }\n",
        )
        .unwrap();
        let proofs_dir = manifest_dir.join(".qed").join("proofs");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        std::fs::write(proofs_dir.join("Token.lean"), "-- proof module\n").unwrap();
        std::fs::write(proofs_dir.join("lakefile.lean"), "-- lakefile\n").unwrap();

        let manifest = manifest_with(vec![(
            "spl_token",
            Dependency::Path {
                path: "token.qedspec".to_string(),
            },
        )]);
        let imports = vec![imp("Token", "spl_token")];

        let resolved = resolve_imports(&imports, &manifest, manifest_dir).unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(
            resolved[0].has_proofs,
            "should detect proofs at .qed/proofs/Token.lean"
        );
        assert_eq!(
            resolved[0].proof_pkg_root.as_ref(),
            Some(&proofs_dir),
            "proof_pkg_root should point at the .qed/proofs/ dir"
        );
    }

    #[test]
    fn no_proofs_when_module_file_is_missing() {
        // lakefile present but no <bound_name>.lean → not verified.
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        std::fs::write(
            manifest_dir.join("token.qedspec"),
            "spec Token\ninterface Token { program_id \"x\" }\n",
        )
        .unwrap();
        let proofs_dir = manifest_dir.join(".qed").join("proofs");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        std::fs::write(proofs_dir.join("lakefile.lean"), "-- lakefile\n").unwrap();

        let manifest = manifest_with(vec![(
            "spl_token",
            Dependency::Path {
                path: "token.qedspec".to_string(),
            },
        )]);
        let imports = vec![imp("Token", "spl_token")];

        let resolved = resolve_imports(&imports, &manifest, manifest_dir).unwrap();
        assert!(!resolved[0].has_proofs);
        assert!(resolved[0].proof_pkg_root.is_none());
    }

    #[test]
    fn no_proofs_when_lakefile_is_missing() {
        // Module file alone isn't enough — need lakefile for the `require`
        // directive to compile on the consumer side.
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        std::fs::write(
            manifest_dir.join("token.qedspec"),
            "spec Token\ninterface Token { program_id \"x\" }\n",
        )
        .unwrap();
        let proofs_dir = manifest_dir.join(".qed").join("proofs");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        std::fs::write(proofs_dir.join("Token.lean"), "-- proof module\n").unwrap();

        let manifest = manifest_with(vec![(
            "spl_token",
            Dependency::Path {
                path: "token.qedspec".to_string(),
            },
        )]);
        let imports = vec![imp("Token", "spl_token")];

        let resolved = resolve_imports(&imports, &manifest, manifest_dir).unwrap();
        assert!(!resolved[0].has_proofs);
        assert!(resolved[0].proof_pkg_root.is_none());
    }

    #[test]
    fn has_proofs_defaults_to_false_for_bare_path_dep() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_dir = tmp.path();
        std::fs::write(
            manifest_dir.join("token.qedspec"),
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
        assert!(!resolved[0].has_proofs);
        assert!(resolved[0].proof_pkg_root.is_none());
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
        // #254: the searched path renders absolutely — never the empty
        // "(looking in )" that a bare relative manifest dir produced.
        assert!(
            !err.contains("(looking in )") && err.contains("qed.toml)"),
            "search path must name the manifest absolutely: {err}"
        );
    }

    // ----- bundled-stdlib builtins -----

    #[test]
    fn builtin_source_returns_spl_system_and_metaplex() {
        let spl = builtin_source("spl").expect("spl builtin must exist");
        assert!(
            spl.contains("interface Token"),
            "spl fixture has Token block"
        );
        assert!(
            spl.contains("binary_hash"),
            "spl fixture has binary_hash pin"
        );

        let system = builtin_source("system").expect("system builtin must exist");
        assert!(
            system.contains("interface System"),
            "system fixture has System block"
        );

        let metaplex = builtin_source("metaplex").expect("metaplex builtin must exist");
        assert!(
            metaplex.contains("interface Metadata"),
            "metaplex fixture has Metadata block"
        );
        assert!(
            metaplex.contains("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s"),
            "metaplex fixture pins the canonical Token Metadata program ID"
        );

        assert!(
            builtin_source("not_a_builtin").is_none(),
            "unknown key returns None"
        );
    }

    /// The Metaplex fixture must parse cleanly through the full pipeline
    /// so consumers can `import Metadata from "metaplex"` without a
    /// `qed.toml` entry and get the standard Tier-1 axiom-discharge
    /// behavior on caller-side Lean / Kani harnesses.
    #[test]
    fn metaplex_builtin_parses_and_carries_interface_handlers() {
        let src = builtin_source("metaplex").expect("metaplex builtin must exist");
        let spec = crate::chumsky_adapter::parse_str(src).expect("metaplex must parse");
        let iface = spec
            .interfaces
            .iter()
            .find(|i| i.name == "Metadata")
            .expect("interface Metadata must be present");
        assert!(
            iface.handlers.len() >= 4,
            "Metadata interface must declare ≥4 handlers; got {}",
            iface.handlers.len()
        );
        assert!(
            iface
                .handlers
                .iter()
                .any(|h| h.name == "create_metadata_account_v3"),
            "create_metadata_account_v3 handler must be present"
        );
    }

    #[test]
    fn all_imports_are_builtins_classifies_mixed_sets() {
        let only_spl = vec![imp("Token", "spl")];
        assert!(all_imports_are_builtins(&only_spl));

        let mixed = vec![imp("Token", "spl"), imp("MyAmm", "my_amm")];
        assert!(!all_imports_are_builtins(&mixed));

        let only_path = vec![imp("MyAmm", "my_amm")];
        assert!(!all_imports_are_builtins(&only_path));

        let empty: Vec<ParsedImport> = vec![];
        assert!(!all_imports_are_builtins(&empty));
    }

    #[test]
    fn resolves_spl_builtin_without_manifest_entry() {
        let _env_lock = lock_env();
        let tmp = tempfile::tempdir().unwrap();
        let _cache_dir = EnvVarGuard::set("QEDGEN_CACHE_DIR", tmp.path());

        // Empty manifest — the builtin short-circuit kicks in before
        // the manifest lookup.
        let manifest = manifest_with(vec![]);
        let imports = vec![imp("Token", "spl")];
        let resolved = resolve_imports(&imports, &manifest, Path::new(".")).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].dep_key, "spl");
        assert!(resolved[0].sources[0]
            .1
            .contains("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"));
    }

    // ----- transitive resolution -----

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
        let _env_lock = lock_env();
        let _ttl = EnvVarGuard::set("QEDGEN_CACHE_TTL", "0");
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join(".qedgen-commit");
        std::fs::write(&marker, "abc123").unwrap();
        assert!(!marker_is_stale(&marker));
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
        let _env_lock = lock_env();
        let _ttl = EnvVarGuard::remove("QEDGEN_CACHE_TTL");
        assert_eq!(cache_ttl_secs(), 7 * 24 * 60 * 60);
    }

    #[test]
    fn cache_ttl_secs_honors_env_override() {
        let _env_lock = lock_env();
        let _ttl = EnvVarGuard::set("QEDGEN_CACHE_TTL", "3600");
        assert_eq!(cache_ttl_secs(), 3600);
    }

    #[test]
    fn cache_root_honors_env_override() {
        let _env_lock = lock_env();
        let _cache_dir = EnvVarGuard::set("QEDGEN_CACHE_DIR", "/tmp/qedgen-test-cache");
        let root = cache_root();
        assert_eq!(root, PathBuf::from("/tmp/qedgen-test-cache"));
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
        let _env_lock = lock_env();
        let tmp = tempfile::tempdir().unwrap();
        let _cache_dir = EnvVarGuard::set("QEDGEN_CACHE_DIR", tmp.path());

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
