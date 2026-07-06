//! Shared recursive `.rs` file walker (T7d / F15).
//!
//! Replaces the sixteen hand-rolled per-module walkers that differed only
//! in skip-dir lists (and had silently diverged). Every caller routes
//! through [`collect_rs_files`]; the skip list is an explicit parameter so
//! a caller that must scan a dir the default excludes can say so at the
//! call site instead of forking the walker.

use std::path::{Path, PathBuf};

/// Canonical skip list: the **union** of the per-walker lists that existed
/// before unification. Unifying on the union is a deliberate, documented
/// behavior change (e.g. `crucible_brownfield` previously skipped only
/// `target`).
///
/// - `target` — cargo build output (may contain copied sources)
/// - `.git` — VCS internals
/// - `node_modules` — JS deps (Anchor workspaces)
/// - `tests` — integration-test code, not program logic
/// - `fuzz` — fuzz harnesses (including QEDGen-generated Crucible ones)
/// - `migrations` — Anchor deploy scripts
/// - `formal_verification` — QEDGen Lean workspace
/// - `.qed` — QEDGen artifacts (audit sets, generated harnesses)
pub(crate) const DEFAULT_SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "tests",
    "fuzz",
    "migrations",
    "formal_verification",
    ".qed",
];

/// Recursively collect `.rs` files under `root`, skipping any directory
/// whose name appears in `skip` (almost always [`DEFAULT_SKIP_DIRS`]).
///
/// - `root` may itself be an `.rs` file → returned as the single entry.
/// - Missing or unreadable directories are silently skipped.
/// - Result is sorted for deterministic downstream behavior.
pub(crate) fn collect_rs_files(root: &Path, skip: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.is_file() {
        if root.extension().is_some_and(|e| e == "rs") {
            out.push(root.to_path_buf());
        }
        return out;
    }
    walk(root, skip, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, skip: &[&str], out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if skip.contains(&name) {
                continue;
            }
            walk(&path, skip, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "// test").unwrap();
    }

    #[test]
    fn skips_default_dirs_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/lib.rs");
        touch(dir.path(), "src/b.rs");
        touch(dir.path(), "src/a.rs");
        touch(dir.path(), "target/debug/build.rs");
        touch(dir.path(), "tests/it.rs");
        touch(dir.path(), "fuzz/src/main.rs");
        touch(dir.path(), ".qed/fuzz/gen.rs");
        touch(dir.path(), "src/notes.txt");
        let files = collect_rs_files(dir.path(), DEFAULT_SKIP_DIRS);
        let rels: Vec<_> = files
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_str().unwrap())
            .collect();
        assert_eq!(rels, vec!["src/a.rs", "src/b.rs", "src/lib.rs"]);
    }

    #[test]
    fn empty_skip_list_includes_everything() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/lib.rs");
        touch(dir.path(), "tests/it.rs");
        let files = collect_rs_files(dir.path(), &[]);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn rs_file_input_returns_itself() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "one.rs");
        let f = dir.path().join("one.rs");
        assert_eq!(collect_rs_files(&f, DEFAULT_SKIP_DIRS), vec![f.clone()]);
    }

    #[test]
    fn missing_root_yields_empty() {
        let dir = tempfile::tempdir().unwrap();
        let files = collect_rs_files(&dir.path().join("nope"), DEFAULT_SKIP_DIRS);
        assert!(files.is_empty());
    }

    #[test]
    fn skip_matches_dirs_only_not_files() {
        // A FILE named like a skip entry (e.g. `tests.rs`) must survive.
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "src/tests.rs");
        let files = collect_rs_files(dir.path(), DEFAULT_SKIP_DIRS);
        assert_eq!(files.len(), 1);
    }
}
