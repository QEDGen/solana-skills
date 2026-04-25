//! Framework auto-detection (v2.9 G1).
//!
//! qedgen's codegen + adapter paths assume the host project is an Anchor
//! program — they emit `use anchor_lang::prelude::*`, `anchor_spl::token::*`,
//! and Anchor-style `#[derive(Accounts)]` shapes. v2.8 made the assumption
//! implicit; v2.9 makes it explicit so:
//!   - the brownfield adapter (G2) and `--anchor-project` mode (G4) can
//!     refuse to run on non-Anchor projects with a clear diagnostic;
//!   - generic CPI codegen (G3) can branch on framework rather than always
//!     emitting `anchor_spl::*`;
//!   - non-Anchor support (E1, raw `invoke_signed`) can land additively
//!     in v2.10+ without rewriting the detection layer.
//!
//! Detection cascade (first hit wins):
//!   1. `Anchor.toml` at any ancestor directory of `spec_dir`.
//!      Definitive — only Anchor projects use it.
//!   2. `Cargo.toml` containing `anchor-lang` or `anchor-spl` in any
//!      `[dependencies]`/`[dev-dependencies]`/`[workspace.dependencies]`
//!      table. Definitive even if `Anchor.toml` is absent (some teams
//!      flatten to plain cargo workspaces).
//!   3. `Xargo.toml`. Heuristic — empirically true for the bulk of
//!      deployed Solana programs, including older Anchor projects that
//!      predate the `anchor build` toolchain. Can fire on raw
//!      `cargo-build-bpf` programs too, so it's the lowest-confidence
//!      signal.
//!
//! If none of the three match, callers see a hard error with an install
//! pointer; CLI users can override with `--framework anchor` to bypass.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// The framework the host project uses. v2.9 ships only Anchor; v2.10+
/// can extend the enum (raw, native-solana-program, etc.) without
/// breaking call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Framework {
    Anchor,
}

impl Framework {
    /// Parse a CLI override value (`--framework <name>`).
    #[allow(dead_code)]
    pub fn from_cli(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "anchor" => Ok(Framework::Anchor),
            other => Err(anyhow!("unknown framework `{}` — supported: anchor", other)),
        }
    }
}

/// Walk up from `spec_dir` to its filesystem root, returning the first
/// `Framework` whose marker is found. Returns an error with an install
/// pointer when none match. The `--framework <name>` CLI flag bypasses
/// detection entirely (callers translate the flag to `Framework::from_cli`
/// before this function is reached).
#[allow(dead_code)]
pub fn detect_framework(spec_dir: &Path) -> Result<Framework> {
    // Canonicalize so symlinks don't fool the ancestor walk. Fall back
    // to the input path if canonicalization fails (relative paths, paths
    // that don't yet exist).
    let start = spec_dir
        .canonicalize()
        .unwrap_or_else(|_| spec_dir.to_path_buf());

    // Tier 1 + Tier 2 + Tier 3 walks share the same ancestor traversal.
    // Run them in priority order so the strongest signal wins.
    if walk_up_for_marker(&start, "Anchor.toml").is_some() {
        return Ok(Framework::Anchor);
    }

    if walk_up_for_anchor_dep(&start)? {
        return Ok(Framework::Anchor);
    }

    if walk_up_for_marker(&start, "Xargo.toml").is_some() {
        return Ok(Framework::Anchor);
    }

    Err(anyhow!(
        "could not detect framework from {} — qedgen v2.9 supports Anchor only.\n\
         Expected one of:\n\
           - `Anchor.toml` at any ancestor directory (definitive Anchor signal)\n\
           - `Cargo.toml` listing `anchor-lang` or `anchor-spl` as a dependency\n\
           - `Xargo.toml` (heuristic — most Anchor projects ship one)\n\
         If this IS an Anchor project, override with `--framework anchor`.\n\
         If this is a non-Anchor Solana program, raw / native-program support\n\
         lands in v2.10+; track progress at\n\
         https://github.com/QEDGen/solana-skills/issues",
        start.display()
    ))
}

/// Walk from `start` up to `/` checking each ancestor for `marker`.
/// Returns the first directory that contains the marker, or `None`.
fn walk_up_for_marker(start: &Path, marker: &str) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(marker).is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// Walk from `start` up to `/` checking each ancestor's `Cargo.toml` for
/// `anchor-lang` or `anchor-spl` in any dependency table. Returns true on
/// the first hit.
///
/// Uses substring search rather than full TOML parsing — `anchor-lang`
/// and `anchor-spl` are unambiguous as crate names, and pulling in a
/// full TOML deserialize for a binary substring would add noise without
/// improving accuracy. False positives (e.g. a comment that happens to
/// say "anchor-lang") are theoretically possible but harmless: they
/// resolve to "this looks like an Anchor project," which is the right
/// outcome when the dep is mentioned in any context.
fn walk_up_for_anchor_dep(start: &Path) -> Result<bool> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.is_file() {
            let body = std::fs::read_to_string(&cargo_toml).map_err(|e| {
                anyhow!(
                    "failed to read {} during framework detection: {}",
                    cargo_toml.display(),
                    e
                )
            })?;
            if has_anchor_dep(&body) {
                return Ok(true);
            }
        }
        current = dir.parent();
    }
    Ok(false)
}

/// True if the Cargo.toml body mentions either `anchor-lang` or
/// `anchor-spl` as a dependency. Tolerant of formatting variations:
///   - `anchor-lang = "0.30"` (inline)
///   - `anchor-lang = { version = "0.30", features = [...] }` (table)
///   - `anchor-lang.workspace = true` (workspace inheritance)
///   - `[dependencies.anchor-lang]` (sectioned)
fn has_anchor_dep(cargo_toml_body: &str) -> bool {
    cargo_toml_body.contains("anchor-lang") || cargo_toml_body.contains("anchor-spl")
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), "").unwrap();
    }

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn detects_anchor_via_anchor_toml_at_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "Anchor.toml");
        let inner = tmp.path().join("programs/escrow/src");
        fs::create_dir_all(&inner).unwrap();
        let detected = detect_framework(&inner).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn detects_anchor_via_cargo_toml_dep() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[package]
name = "my-program"
version = "0.1.0"

[dependencies]
anchor-lang = "0.30"
"#,
        );
        let detected = detect_framework(tmp.path()).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn detects_anchor_via_anchor_spl_dep() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[package]
name = "token-program"

[dependencies]
anchor-spl = { version = "0.30", features = ["token"] }
"#,
        );
        let detected = detect_framework(tmp.path()).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn detects_anchor_via_workspace_inheritance() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[dependencies]
anchor-lang.workspace = true
"#,
        );
        let detected = detect_framework(tmp.path()).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn detects_anchor_via_xargo_toml_heuristic() {
        let tmp = tempfile::tempdir().unwrap();
        // No Anchor.toml, no Cargo.toml — only Xargo.toml.
        touch(tmp.path(), "Xargo.toml");
        let detected = detect_framework(tmp.path()).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn detection_walks_up_from_nested_spec_dir() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "Anchor.toml");
        // Spec lives 3 levels deep — detection still finds the marker.
        let nested = tmp.path().join("programs/foo/specs");
        fs::create_dir_all(&nested).unwrap();
        let detected = detect_framework(&nested).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn errors_with_install_pointer_when_no_marker_found() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty directory — detection should fail with the install pointer.
        let err = detect_framework(tmp.path()).unwrap_err().to_string();
        assert!(
            err.contains("qedgen v2.9 supports Anchor only"),
            "expected v2.9 framework hint; got: {err}"
        );
        assert!(
            err.contains("--framework anchor"),
            "should point users at the override flag; got: {err}"
        );
    }

    #[test]
    fn anchor_toml_wins_over_anchor_dep() {
        // Both signals present — the Tier-1 marker takes precedence (the
        // test only checks they both resolve to Anchor; ordering matters
        // when more frameworks land in v2.10).
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "Anchor.toml");
        write(
            tmp.path(),
            "Cargo.toml",
            "[dependencies]\nanchor-lang = \"0.30\"\n",
        );
        let detected = detect_framework(tmp.path()).unwrap();
        assert_eq!(detected, Framework::Anchor);
    }

    #[test]
    fn cargo_toml_without_anchor_dep_is_not_a_match() {
        // Plain Rust crate that happens to live on the path. We don't
        // claim it's Anchor — fall through to Xargo.toml or error.
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[package]
name = "plain-rust"

[dependencies]
serde = "1"
"#,
        );
        let err = detect_framework(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("supports Anchor only"),
            "non-Anchor Cargo.toml should fall through to error; got: {msg}"
        );
    }

    #[test]
    fn from_cli_recognizes_anchor() {
        assert_eq!(Framework::from_cli("anchor").unwrap(), Framework::Anchor);
        assert_eq!(Framework::from_cli("Anchor").unwrap(), Framework::Anchor);
        assert_eq!(Framework::from_cli("ANCHOR").unwrap(), Framework::Anchor);
    }

    #[test]
    fn from_cli_rejects_unknown_framework() {
        let err = Framework::from_cli("solana").unwrap_err().to_string();
        assert!(err.contains("unknown framework"));
        assert!(err.contains("supported: anchor"));
    }

    #[test]
    fn has_anchor_dep_handles_all_dep_table_styles() {
        assert!(has_anchor_dep("anchor-lang = \"0.30\""));
        assert!(has_anchor_dep("anchor-spl = { version = \"0.30\" }"));
        assert!(has_anchor_dep("anchor-lang.workspace = true"));
        assert!(has_anchor_dep(
            "[dependencies.anchor-lang]\nversion = \"0.30\""
        ));
        assert!(!has_anchor_dep("[dependencies]\nserde = \"1\""));
    }
}
