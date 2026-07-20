//! Shared harness for the qedgen integration-test binaries.
//!
//! Each `tests/*.rs` binary includes this via `mod common;`. It owns the
//! repo/binary path helpers, fixture-tempdir staging, the unified-diff
//! renderer, and the [`SnapshotHarness`] assert-or-update loop shared by
//! the four snapshot suites (`mir`, `kani`, `codegen`, `proptest`).
//!
//! `ensure_qedgen_built` fixes the historical stale-binary footgun: it
//! unconditionally runs `cargo build --bin qedgen` (once per test binary,
//! via `Once`; cargo's own freshness check makes the no-op case cheap),
//! so a stale `target/<profile>/qedgen` can never silently serve old
//! behavior to the snapshot suites.
#![allow(dead_code)] // each test binary uses a subset of these helpers

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

/// Repository root, derived from this crate's manifest dir
/// (`<repo>/crates/qedgen`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qedgen crate at <repo>/crates/qedgen")
        .to_path_buf()
}

/// Path of the built `qedgen` binary for the active profile.
pub fn qedgen_bin() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    repo_root().join("target").join(profile).join("qedgen")
}

/// Build `qedgen` before driving it. Always rebuilds (cargo's freshness
/// check keeps the no-op case fast) so a stale binary can never serve old
/// behavior; the `Once` keeps parallel tests in the same binary from
/// racing multiple `cargo build` invocations.
pub fn ensure_qedgen_built() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let mut args = vec!["build", "--bin", "qedgen"];
        if !cfg!(debug_assertions) {
            args.push("--release");
        }
        let status = Command::new("cargo")
            .args(&args)
            .current_dir(repo_root())
            .status()
            .expect("spawn cargo build");
        assert!(status.success(), "cargo build qedgen failed");
    });
}

/// Run a command; panic with full stdout/stderr on nonzero exit.
pub fn run_ok(command: &mut Command) {
    run_capture_ok(command);
}

/// Run a command; panic with full output on nonzero exit; return
/// stdout + stderr on success.
pub fn run_capture_ok(command: &mut Command) -> String {
    let output = command.output().expect("failed to spawn command");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        panic!(
            "command {:?} failed with status {}\noutput:\n{combined}",
            command.get_program(),
            output.status,
        );
    }
    combined
}

/// Rewrite the `qedgen-macros` line in a generated Cargo.toml from a git
/// dep tagged at the current crate version (which doesn't exist on GitHub
/// until release time) to a `path` dep pointing at the in-repo crate.
pub fn redirect_macros_to_path(cargo_toml: &Path) {
    let manifest = fs::read_to_string(cargo_toml).expect("read Cargo.toml");
    let macros_path = repo_root().join("crates/qedgen-macros");
    let replacement = format!("qedgen-macros = {{ path = {:?} }}", macros_path);
    let mut found = false;
    let rewritten: String = manifest
        .lines()
        .map(|line| {
            if line.starts_with("qedgen-macros = {")
                && line.contains("git = \"https://github.com/qedgen/solana-skills\"")
            {
                found = true;
                replacement.clone()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        found,
        "expected qedgen-macros git line in {}",
        cargo_toml.display()
    );
    fs::write(cargo_toml, format!("{rewritten}\n")).expect("rewrite Cargo.toml");
}

/// `crates/qedgen/tests/snapshots/` — where the checked-in references live.
pub fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
}

/// `git init --quiet` in `dir` — qedgen is git-native by design
/// ([[project-git-native]]); codegen tempdirs need their own repo so the
/// run doesn't collide with the workspace's git state.
pub fn git_init(dir: &Path) {
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(dir)
        .status()
        .expect("spawn git init");
    assert!(status.success(), "git init failed in {}", dir.display());
}

/// Copy the repo-relative `fixture_dir` into a fresh git-initialized
/// tempdir (rsync minus build/anchor junk) so codegen rewrites are
/// isolated from the workspace.
pub fn stage_fixture(fixture_dir: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = repo_root().join(fixture_dir);

    let rsync = Command::new("rsync")
        .args([
            "-aq",
            "--exclude=.anchor",
            "--exclude=target",
            "--exclude=.lake",
            "--exclude=node_modules",
        ])
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", tmp.path().display()))
        .status()
        .expect("spawn rsync");
    assert!(rsync.success(), "rsync failed for fixture {}", fixture_dir);

    git_init(tmp.path());
    tmp
}

/// Like [`stage_fixture`], but copies into a *named* subdirectory of
/// the tempdir — for tests whose assertions depend on the program
/// root's directory name (#289; a bare tempdir's name is random).
/// Returns the tempdir guard plus the staged program root.
pub fn stage_fixture_named(fixture_dir: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let dest = tmp.path().join(name);
    fs::create_dir_all(&dest).expect("create named fixture dir");
    let src = repo_root().join(fixture_dir);

    let rsync = Command::new("rsync")
        .args([
            "-aq",
            "--exclude=.anchor",
            "--exclude=target",
            "--exclude=.lake",
            "--exclude=node_modules",
        ])
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .status()
        .expect("spawn rsync");
    assert!(rsync.success(), "rsync failed for fixture {}", fixture_dir);

    git_init(&dest);
    (tmp, dest)
}

/// Produce a unified-diff string between two multiline texts. Avoids
/// pulling in an extra crate; the output isn't IDE-grade but suffices
/// for test failure messages.
pub fn diff_unified(expected: &str, actual: &str) -> String {
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    out.push_str("--- snapshot\n+++ rendered\n");
    let max = exp_lines.len().max(act_lines.len());
    let mut printed = 0usize;
    let max_lines = 120usize;
    for i in 0..max {
        let e = exp_lines.get(i).copied().unwrap_or("");
        let a = act_lines.get(i).copied().unwrap_or("");
        if e != a {
            if printed >= max_lines {
                out.push_str("... (diff truncated)\n");
                break;
            }
            out.push_str(&format!("@@ line {} @@\n", i + 1));
            if !e.is_empty() || i < exp_lines.len() {
                out.push_str(&format!("-{}\n", e));
            }
            if !a.is_empty() || i < act_lines.len() {
                out.push_str(&format!("+{}\n", a));
            }
            printed += 1;
        }
    }
    out
}

/// Compare `rendered` against `tests/snapshots/<snapshot_name>`, or write
/// it when `UPDATE_SNAPSHOTS=1`. On drift, panics with a unified diff.
pub fn assert_or_update_snapshot_content(snapshot_name: &str, kind: &str, rendered: &str) {
    let snapshot_path = snapshots_dir().join(snapshot_name);
    let update = std::env::var("UPDATE_SNAPSHOTS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if update {
        fs::create_dir_all(snapshots_dir()).expect("create snapshots dir");
        fs::write(&snapshot_path, rendered)
            .unwrap_or_else(|e| panic!("write {}: {e}", snapshot_path.display()));
        eprintln!("UPDATE_SNAPSHOTS=1: wrote {}", snapshot_path.display());
        return;
    }

    let expected = fs::read_to_string(&snapshot_path).unwrap_or_else(|e| {
        panic!(
            "missing snapshot {}: {e}\n\
             Run with UPDATE_SNAPSHOTS=1 to seed it.",
            snapshot_path.display()
        )
    });

    if expected != rendered {
        let diff = diff_unified(&expected, rendered);
        panic!(
            "{snapshot_name}: {kind} snapshot drift detected.\n\
             Snapshot: {}\n\
             Re-run with UPDATE_SNAPSHOTS=1 to refresh (then inspect the diff before \
             committing).\n\
             {diff}",
            snapshot_path.display()
        );
    }
}

/// One snapshot suite: a per-fixture renderer plus the snapshot filename
/// suffix it locks against. Collapses the four suite harnesses
/// (`tests/{mir,kani,codegen,proptest}_snapshot.rs`) to their `#[test]`
/// lists + a render fn.
pub struct SnapshotHarness {
    /// Appended to the fixture name for the snapshot file, e.g.
    /// `".Spec.lean"` → `tests/snapshots/escrow.Spec.lean`.
    pub suffix: &'static str,
    /// Drift-panic label, e.g. `"MIR Kani"`.
    pub kind: &'static str,
    /// Regenerate the artifact for `(fixture_dir, spec_arg)`. Suites
    /// that take the spec path directly ignore `fixture_dir`.
    pub render: fn(&str, &str) -> String,
}

impl SnapshotHarness {
    /// Render the fixture and compare against (or refresh) its snapshot.
    pub fn assert_or_update(&self, fixture: &str, fixture_dir: &str, spec_arg: &str) {
        let rendered = (self.render)(fixture_dir, spec_arg);
        assert_or_update_snapshot_content(
            &format!("{}{}", fixture, self.suffix),
            self.kind,
            &rendered,
        );
    }
}

/// Stage the spec surface (every `*.qedspec` fragment — subdirectories
/// included — plus `qed.toml`, and an empty `.qed/`) of `spec`'s project
/// into `out_dir`; returns the staged `--spec` argument RELATIVE to
/// `out_dir` (run the binary with `current_dir(out_dir)` so embedded
/// `#[qed(spec = …)]` stamps stay run-independent).
///
/// #279 made relative codegen outputs resolve against the spec's project
/// root, so a test must never point `--spec` at a real repo example or
/// fixture — the run would regenerate artifacts into the repo tree.
pub fn stage_spec_surface(spec: &Path, out_dir: &Path) -> PathBuf {
    let (src_dir, spec_file) = if spec.is_dir() {
        (spec.to_path_buf(), None)
    } else {
        (
            spec.parent().expect("spec has a parent").to_path_buf(),
            Some(spec.file_name().expect("spec file name").to_owned()),
        )
    };
    fn copy_surface(src: &Path, src_root: &Path, out_root: &Path) {
        for entry in fs::read_dir(src).expect("read spec dir").flatten() {
            let p = entry.path();
            let name = entry.file_name();
            if p.is_dir() {
                // Build outputs / VCS state aren't spec surface.
                if name != ".git" && name != "target" && name != ".lake" {
                    copy_surface(&p, src_root, out_root);
                }
            } else {
                let is_spec = name.to_string_lossy().ends_with(".qedspec");
                if is_spec || name == "qed.toml" {
                    let rel = p.strip_prefix(src_root).expect("under src root");
                    let dst = out_root.join(rel);
                    fs::create_dir_all(dst.parent().expect("dst parent")).expect("mkdir");
                    fs::copy(&p, &dst).expect("stage spec file");
                }
            }
        }
    }
    copy_surface(&src_dir, &src_dir, out_dir);
    // Codegen's preflight requires a .qed/ next to the spec.
    fs::create_dir_all(out_dir.join(".qed")).expect("create .qed");
    match spec_file {
        Some(name) => PathBuf::from(name),
        None => PathBuf::from("."),
    }
}
