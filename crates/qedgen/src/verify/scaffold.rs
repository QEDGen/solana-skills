//! `scaffold` backend (#364): compiles the generated program crate.
//!
//! Every other backend compiles a HARNESS. `check` is spec-level lint only,
//! and `verify` builds the Kani/proptest harnesses in isolated crates on
//! purpose (see the note at the `run_kani` call site: tying them to the
//! program package would let unrelated scaffold errors block Kani). So
//! nothing in the user-facing loop ever compiled the thing codegen actually
//! wrote, and "codegen emits Rust that does not build" stayed a class the
//! user discovered from a red `cargo build`.
//!
//! The maintainer-side gate that catches this (`tests/generated_artifact_
//! gate.rs`, #294) is corpus-bound: it only sees defects some bundled
//! example happens to trigger, and it cannot see a user's spec at all. This
//! backend is the property-wise version — it runs against whatever the user
//! generated.
//!
//! ## Why the outcome has four variants and not two
//!
//! A failed `cargo check` is not evidence of a codegen defect on its own.
//! The generated `Cargo.toml` pins `qedgen-macros` to a git tag at the
//! current crate version, which does not exist upstream until release time
//! (`tests/common/mod.rs::redirect_macros_to_path` documents the same
//! constraint from the gate side). A developer build therefore fails to
//! resolve for a reason that has nothing to do with the spec. Reporting
//! that as `Failed` would fire constantly, and a backend that cries wolf
//! gets ignored — which would leave the class exactly as open as it is now.
//!
//! So dependency and toolchain failures report `Unresolved` and map to
//! `Skipped`, and only rustc rejecting code in THIS crate reports
//! `TypeErrors` and maps to `Failed`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What `cargo check --tests` said about the generated crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldOutcome {
    /// The crate and every test target typecheck.
    Compiled,
    /// rustc rejected code belonging to this crate. The codegen-defect case,
    /// and the only one that fails a verify run.
    TypeErrors {
        summary: String,
        log: Option<PathBuf>,
    },
    /// cargo never got as far as typechecking this crate: dependency fetch,
    /// version resolution, a dependency that itself fails to build, or a
    /// missing toolchain. Says nothing about the generated code.
    Unresolved { reason: String },
    /// Nothing to compile here.
    NotApplicable { reason: String },
}

/// Compile the crate at `crate_dir` and classify the result.
///
/// `--tests` covers the generated test targets (`tests/unit.rs`,
/// `tests/proptest.rs`, `tests/integration_tests.rs`) as well as the lib.
/// That is where several past instances of this bug class landed. The
/// trade-off is attribution: a hand-written broken test in the same crate
/// also reports here, named by file in the summary but blamed on the
/// scaffold.
pub fn check_compiles(crate_dir: &Path) -> ScaffoldOutcome {
    let manifest = crate_dir.join("Cargo.toml");
    if !manifest.exists() {
        return ScaffoldOutcome::NotApplicable {
            reason: format!(
                "no Cargo.toml at {} (run `qedgen codegen` first)",
                crate_dir.display()
            ),
        };
    }

    // Absolute, so message `manifest_path` values compare against it.
    let root = crate_dir
        .canonicalize()
        .unwrap_or_else(|_| crate_dir.to_path_buf());

    let output = Command::new("cargo")
        .args(cargo_args(deps_resolved(&root)))
        .current_dir(&root)
        .output();

    let out = match output {
        Ok(out) => out,
        Err(e) => {
            return ScaffoldOutcome::Unresolved {
                reason: format!("failed to spawn cargo: {}", e),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    match classify(out.status.success(), &stdout, &stderr, &root) {
        Classification::Compiled => ScaffoldOutcome::Compiled,
        Classification::Unresolved(reason) => ScaffoldOutcome::Unresolved { reason },
        Classification::TypeErrors(summary) => ScaffoldOutcome::TypeErrors {
            summary,
            log: write_log(&root, &stdout, &stderr),
        },
    }
}

/// Whether this crate already has a resolved dependency tree, which two
/// callers ask for different reasons.
///
/// The post-`codegen` check uses it as a precondition: with a lock,
/// `cargo check` is an incremental pass rather than a multi-minute cold
/// build of `anchor-lang` and friends. That keeps `codegen` fast and offline
/// on a first run, when a user has the least patience for it, and lands the
/// check on every run after — by which point they have built once and the
/// answer arrives in seconds. `verify --scaffold` always runs regardless.
///
/// [`cargo_args`] uses it to decide whether there is user state to protect
/// with `--locked`.
///
/// Checks the crate and its immediate parent, covering both a standalone
/// generated crate and the usual `<project>/programs/` layout with the lock
/// at the project root. Deliberately does not walk to the filesystem root:
/// an unrelated ancestor lockfile is not evidence that THIS crate resolves.
pub fn deps_resolved(crate_dir: &Path) -> bool {
    if crate_dir.join("Cargo.lock").exists() {
        return true;
    }
    crate_dir
        .parent()
        .is_some_and(|p| p.join("Cargo.lock").exists())
}

/// A check must not mutate what it is checking. `cargo check` rewrites
/// `Cargo.lock` whenever resolution moves, so asking "does this compile?"
/// would silently repin the user's dependency set. The codegen snapshot
/// suite caught exactly that: one run rewrote all three committed example
/// locks to a different resolution.
///
/// `--locked` only when a lock already exists. With one, refusing to touch
/// it is the point, and lock drift then surfaces as a cargo-level failure,
/// which classifies as `Unresolved` rather than as a false codegen defect.
/// With no lock there is no user state to protect, and `--locked` would fail
/// every freshly generated crate.
fn cargo_args(has_lock: bool) -> Vec<&'static str> {
    let mut args = vec!["check", "--tests", "--message-format=json"];
    if has_lock {
        args.push("--locked");
    }
    args
}

#[derive(Debug, PartialEq, Eq)]
enum Classification {
    Compiled,
    TypeErrors(String),
    Unresolved(String),
}

/// Pure classifier over one `cargo check --message-format=json` run.
///
/// The split rests on where the failure was reported. rustc diagnostics
/// arrive on stdout as `reason: "compiler-message"` JSON objects carrying
/// the `manifest_path` of the package they belong to. cargo's own failures
/// (fetch, resolution, no toolchain) produce plain stderr text and no such
/// objects at all. Errors from a DEPENDENCY are real compiler errors but
/// carry someone else's `manifest_path`, so they classify as `Unresolved`
/// rather than blaming the generated code.
fn classify(success: bool, stdout: &str, stderr: &str, root: &Path) -> Classification {
    if success {
        return Classification::Compiled;
    }

    let mut own: Vec<String> = Vec::new();
    let mut foreign = 0usize;

    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let Some(body) = msg.get("message") else {
            continue;
        };
        if body.get("level").and_then(|l| l.as_str()) != Some("error") {
            continue;
        }

        // "aborting due to N previous errors" is a tally, not a diagnostic.
        let text = body.get("message").and_then(|m| m.as_str()).unwrap_or("");
        if text.starts_with("aborting due to") {
            continue;
        }

        let owned = msg
            .get("manifest_path")
            .and_then(|p| p.as_str())
            .is_some_and(|p| Path::new(p).starts_with(root));

        if owned {
            let rendered = body
                .get("rendered")
                .and_then(|r| r.as_str())
                .unwrap_or(text)
                .trim_end();
            own.push(rendered.to_string());
        } else {
            foreign += 1;
        }
    }

    if own.is_empty() {
        if foreign > 0 {
            return Classification::Unresolved(format!(
                "{} error(s) came from a dependency, not the generated crate; \
                 the scaffold was never typechecked",
                foreign
            ));
        }
        // No rustc diagnostics at all: cargo failed before compiling.
        return Classification::Unresolved(format!(
            "cargo failed before typechecking the crate — dependencies did \
             not resolve or the toolchain is unavailable:\n{}",
            tail(stderr, 12)
        ));
    }

    Classification::TypeErrors(summarize(&own))
}

/// First few diagnostics in full, with a count when there are more. The
/// rendered form carries the file, line, and caret diagram, which is what
/// makes the failure actionable.
fn summarize(errors: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut out = String::new();
    if errors.len() > SHOWN {
        out.push_str(&format!(
            "{} error(s) in the generated crate; first {}:\n\n",
            errors.len(),
            SHOWN
        ));
    }
    for e in errors.iter().take(SHOWN) {
        out.push_str(e);
        out.push_str("\n\n");
    }
    out.trim_end().to_string()
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Full cargo output next to the project's `.qed/`, best-effort: a log we
/// could not write must not change the verdict.
fn write_log(root: &Path, stdout: &str, stderr: &str) -> Option<PathBuf> {
    let qed = root.parent().map(|p| p.join(".qed"))?;
    if !qed.is_dir() {
        return None;
    }
    let path = qed.join("scaffold-check.log");
    std::fs::write(&path, format!("{}\n{}", stdout, stderr)).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiler_error(manifest: &str, message: &str, rendered: &str) -> String {
        serde_json::json!({
            "reason": "compiler-message",
            "manifest_path": manifest,
            "message": {
                "level": "error",
                "message": message,
                "rendered": rendered,
            }
        })
        .to_string()
    }

    #[test]
    fn success_is_compiled() {
        let c = classify(true, "", "", Path::new("/p/programs"));
        assert_eq!(c, Classification::Compiled);
    }

    #[test]
    fn rustc_error_in_our_crate_is_a_type_error() {
        let stdout = compiler_error(
            "/p/programs/Cargo.toml",
            "cannot find type `Pubkey` in this scope",
            "error[E0412]: cannot find type `Pubkey` in this scope\n --> src/state.rs:5:27",
        );
        let c = classify(false, &stdout, "", Path::new("/p/programs"));
        match c {
            Classification::TypeErrors(s) => {
                assert!(s.contains("E0412"), "summary keeps the rendered form: {s}");
                assert!(
                    s.contains("src/state.rs:5:27"),
                    "summary keeps the span: {s}"
                );
            }
            other => panic!("expected TypeErrors, got {other:?}"),
        }
    }

    /// The `qedgen-macros` git tag does not exist until release time, so this
    /// is the everyday developer case. It must not read as a codegen defect.
    #[test]
    fn cargo_level_failure_is_unresolved() {
        let stderr = "error: failed to get `qedgen-macros` as a dependency\n\n\
                      Caused by:\n  failed to find tag `v2.49.0`";
        let c = classify(false, "", stderr, Path::new("/p/programs"));
        match c {
            Classification::Unresolved(r) => {
                assert!(r.contains("did not resolve"), "reason: {r}");
                assert!(r.contains("failed to find tag"), "reason keeps stderr: {r}");
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    /// A dependency that fails to build produces real rustc errors that are
    /// not ours. Blaming the scaffold for them would be a false positive.
    #[test]
    fn dependency_compile_error_is_unresolved_not_ours() {
        let stdout = compiler_error(
            "/home/u/.cargo/registry/src/index.crates.io-abc/anchor-lang-0.30.0/Cargo.toml",
            "unresolved import",
            "error[E0432]: unresolved import",
        );
        let c = classify(false, &stdout, "", Path::new("/p/programs"));
        match c {
            Classification::Unresolved(r) => {
                assert!(r.contains("came from a dependency"), "reason: {r}");
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn abort_tally_alone_is_not_a_type_error() {
        let stdout = compiler_error(
            "/p/programs/Cargo.toml",
            "aborting due to 2 previous errors",
            "error: aborting due to 2 previous errors",
        );
        let c = classify(false, &stdout, "some cargo noise", Path::new("/p/programs"));
        assert!(
            matches!(c, Classification::Unresolved(_)),
            "a bare abort tally carries no diagnostic: {c:?}"
        );
    }

    #[test]
    fn non_json_stdout_lines_are_ignored() {
        let stdout = format!(
            "not json at all\n{}\n",
            compiler_error(
                "/p/programs/Cargo.toml",
                "mismatched types",
                "error[E0308]: mismatched types",
            )
        );
        let c = classify(false, &stdout, "", Path::new("/p/programs"));
        assert!(matches!(c, Classification::TypeErrors(_)), "got {c:?}");
    }

    #[test]
    fn summary_caps_the_error_count_and_says_so() {
        let errors: Vec<String> = (0..5)
            .map(|i| format!("error[E000{i}]: boom {i}"))
            .collect();
        let s = summarize(&errors);
        assert!(s.contains("5 error(s)"), "{s}");
        assert!(s.contains("first 3"), "{s}");
        assert!(s.contains("boom 2"), "{s}");
        assert!(!s.contains("boom 3"), "capped at 3: {s}");
    }

    /// Regression for the mutation the snapshot suite caught: a run against
    /// a crate with a committed lock must pass `--locked`, so the check
    /// cannot repin the user's dependencies as a side effect of asking
    /// whether they compile.
    #[test]
    fn an_existing_lock_is_never_rewritten() {
        assert!(
            cargo_args(true).contains(&"--locked"),
            "a crate with a lock must be checked without rewriting it"
        );
    }

    /// The other half: `--locked` on a crate with no lock fails outright,
    /// which would make the check useless on freshly generated output.
    #[test]
    fn a_fresh_crate_is_checked_without_locked() {
        assert!(!cargo_args(false).contains(&"--locked"));
        assert!(cargo_args(false).contains(&"--tests"));
    }

    #[test]
    fn missing_manifest_is_not_applicable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        match check_compiles(tmp.path()) {
            ScaffoldOutcome::NotApplicable { reason } => {
                assert!(reason.contains("no Cargo.toml"), "{reason}");
            }
            other => panic!("expected NotApplicable, got {other:?}"),
        }
    }

    #[test]
    fn deps_resolved_finds_the_lock_at_either_level() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let crate_dir = tmp.path().join("programs");
        std::fs::create_dir_all(&crate_dir).expect("mkdir");
        assert!(!deps_resolved(&crate_dir), "no lock anywhere");

        std::fs::write(tmp.path().join("Cargo.lock"), "").expect("write");
        assert!(deps_resolved(&crate_dir), "lock at the project root");

        std::fs::write(crate_dir.join("Cargo.lock"), "").expect("write");
        assert!(deps_resolved(&crate_dir), "lock in the crate itself");
    }

    /// Walking to the filesystem root would let an unrelated ancestor
    /// lockfile turn the post-codegen check on for a crate that has never
    /// resolved, reintroducing the cold-build stall the gate exists to avoid.
    #[test]
    fn deps_resolved_does_not_walk_past_the_parent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("Cargo.lock"), "").expect("write");
        let deep = tmp.path().join("a").join("b").join("programs");
        std::fs::create_dir_all(&deep).expect("mkdir");
        assert!(!deps_resolved(&deep));
    }
}
