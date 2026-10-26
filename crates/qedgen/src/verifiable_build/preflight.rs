//! Checks that run before the Docker build: a committed tree, a `debug`
//! setting in `[profile.release]`, and the tools the build needs.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Oldest `solana-verify` the build was tested with.
pub const MIN_SOLANA_VERIFY: (u64, u64, u64) = (0, 5, 2);

/// The nearest ancestor of `program` (itself included) whose `Cargo.toml`
/// declares `[workspace]`, else `program`. `target/` and `[profile.release]`
/// live there, and `solana-verify` mounts it.
pub fn workspace_root(program: &Path) -> Result<PathBuf> {
    let program = program
        .canonicalize()
        .with_context(|| format!("reading program crate {}", program.display()))?;
    Ok(program
        .ancestors()
        .find(|dir| crate::run_helpers::manifest_declares_workspace(&dir.join("Cargo.toml")))
        .unwrap_or(&program)
        .to_path_buf())
}

/// The `[profile.release] debug` value, or `None` when it is absent or turns
/// debug info off. Only `[profile.release]` counts: `build-override` applies
/// to build scripts, not to the program.
pub fn release_debug_setting(workspace_cargo_toml: &str) -> Option<String> {
    let manifest: toml::Value = workspace_cargo_toml.parse().ok()?;
    let debug = manifest.get("profile")?.get("release")?.get("debug")?;
    let value = match debug {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match value.as_str() {
        "0" | "false" | "none" => None,
        _ => Some(value),
    }
}

/// The Solana version `solana-verify` will pick its build image by, read
/// the same way it reads it: `[workspace.metadata.cli] solana` in the
/// workspace `Cargo.toml`, else the first of `solana-program`,
/// `solana-program-error`, `solana-account-info` in `Cargo.lock`.
pub fn solana_image_version(
    workspace_cargo_toml: &str,
    cargo_lock: Option<&str>,
) -> Option<(u64, u64, u64)> {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
        match (parts.next()??, parts.next()??, parts.next()??, parts.next()) {
            (major, minor, patch, None) => Some((major, minor, patch)),
            _ => None,
        }
    };
    let manifest: Option<toml::Value> = workspace_cargo_toml.parse().ok();
    let pinned = manifest.as_ref().and_then(|m| {
        m.get("workspace")?
            .get("metadata")?
            .get("cli")?
            .get("solana")?
            .as_str()
    });
    if let Some(version) = pinned.and_then(parse) {
        return Some(version);
    }
    let lock: toml::Value = cargo_lock?.parse().ok()?;
    let packages = lock.get("package")?.as_array()?;
    [
        "solana-program",
        "solana-program-error",
        "solana-account-info",
    ]
    .iter()
    .find_map(|name| {
        packages.iter().find_map(|pkg| {
            if pkg.get("name")?.as_str()? != *name {
                return None;
            }
            parse(pkg.get("version")?.as_str()?)
        })
    })
}

/// Refuse when `solana-verify` cannot pick an image, or when the image it
/// picks is a Solana 1.x one, whose `cargo build-sbf` only knows the old
/// `sbfv1`/`sbfv2` arch names and so cannot build `--arch v1..v3`.
pub fn check_image_arch(version: Option<(u64, u64, u64)>, arch: &str) -> Result<()> {
    let fix = "set it in the workspace Cargo.toml and commit:\n\n[workspace.metadata.cli]\n\
               solana = \"3.0.1\"\n\n(3.0.1 is the tested version)";
    match version {
        None => bail!(
            "solana-verify cannot tell which Solana build image to use: no \
             [workspace.metadata.cli] solana version and no solana-program, \
             solana-program-error or solana-account-info in Cargo.lock; {fix}"
        ),
        Some((major, minor, patch)) if major < 2 && arch != "v0" => bail!(
            "solana-verify would build in the Solana {major}.{minor}.{patch} image, whose \
             cargo build-sbf has no --arch {arch}; {fix}"
        ),
        Some(_) => Ok(()),
    }
}

/// A warning for an image that may fail inside Docker after minutes: the
/// Solana 2.x images ship cargo 1.84, which cannot read crates on edition
/// 2024, and a fresh lockfile today pulls some.
pub fn image_warning(version: Option<(u64, u64, u64)>) -> Option<String> {
    match version {
        Some((2, minor, patch)) => Some(format!(
            "solana-verify will build in the Solana 2.{minor}.{patch} image, whose cargo 1.84 \
             cannot read crates on edition 2024; if the build fails on that, set \
             [workspace.metadata.cli] solana = \"3.0.1\" and commit it"
        )),
        _ => None,
    }
}

/// The full commit SHA of `HEAD` in the repository that holds `dir`.
pub fn git_head(dir: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .context("running git rev-parse HEAD")?;
    if !out.status.success() {
        bail!(
            "{} is not in a git repository with a commit; qedgen build ties the build \
             to a commit, so run `git init` and commit first",
            dir.display()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `dir`'s path inside its git repository, with `/` separators, or an empty
/// string at the repo root. `solana-verify verify-from-repo --mount-path`
/// takes this form.
pub fn repo_relative(dir: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-prefix"])
        .current_dir(dir)
        .output()
        .context("running git rev-parse --show-prefix")?;
    if !out.status.success() {
        bail!("{} is not in a git repository", dir.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_end_matches('/')
        .to_string())
}

/// Refuse when `dir` has staged, unstaged or untracked changes. The build
/// is tied to a commit, so it must build exactly that commit. qedgen's own
/// files under any `.qed/` directory (build output, verify evidence, the
/// obligation manifest) do not count: they cannot change the built bytes.
pub fn assert_clean_tree(dir: &Path) -> Result<()> {
    let out = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all", "--", "."])
        .arg(":(exclude,glob)**/.qed/**")
        .current_dir(dir)
        .output()
        .context("running git status")?;
    if !out.status.success() {
        bail!(
            "git status failed under {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let changes = String::from_utf8_lossy(&out.stdout);
    if !changes.trim().is_empty() {
        bail!(
            "{} has uncommitted changes; qedgen build ties the build to a commit, \
             so commit or stash them first:\n{}",
            dir.display(),
            changes.trim_end()
        );
    }
    Ok(())
}

/// Parse `solana-verify --version` output (`solana-verify 0.5.2`).
pub fn parse_solana_verify_version(stdout: &str) -> Result<(u64, u64, u64)> {
    let version = stdout
        .split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .with_context(|| format!("no version in solana-verify output {stdout:?}"))?;
    let mut parts = version.split('.').map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
    });
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) => Ok((major, minor, patch)),
        _ => bail!("cannot read solana-verify version {version:?}"),
    }
}

pub fn version_at_least(v: (u64, u64, u64), min: (u64, u64, u64)) -> bool {
    v >= min
}

/// Check that `solana-verify` (at least [`MIN_SOLANA_VERIFY`]) and a running
/// Docker are available. There is no fallback: a local build does not
/// reproduce the Docker build. Returns the `solana-verify` version string.
pub fn check_tools() -> Result<String> {
    let (major, minor, patch) = MIN_SOLANA_VERIFY;
    let install = format!(
        "install it with `cargo install solana-verify --locked` (at least {major}.{minor}.{patch})"
    );
    let out = match Command::new("solana-verify").arg("--version").output() {
        Ok(out) if out.status.success() => out,
        _ => bail!("solana-verify was not found; {install}"),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let version = parse_solana_verify_version(&stdout)?;
    if !version_at_least(version, MIN_SOLANA_VERIFY) {
        bail!(
            "solana-verify {}.{}.{} is too old; {install}",
            version.0,
            version.1,
            version.2
        );
    }
    let docker = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output();
    if !docker.is_ok_and(|out| out.status.success()) {
        bail!("Docker is not running; qedgen build needs Docker for a verifiable build");
    }
    Ok(format!("{}.{}.{}", version.0, version.1, version.2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    const LOCK_WITH: &str = "version = 4\n\n[[package]]\nname = \"solana-account-info\"\nversion = \"2.3.0\"\n\n[[package]]\nname = \"solana-program\"\nversion = \"1.18.26\"\n";

    #[test]
    fn image_version_prefers_workspace_metadata() {
        let toml = "[workspace]\n[workspace.metadata.cli]\nsolana = \"3.0.1\"\n";
        assert_eq!(solana_image_version(toml, Some(LOCK_WITH)), Some((3, 0, 1)));
    }

    #[test]
    fn image_version_falls_back_to_lock_in_solana_verify_order() {
        // solana-program wins over solana-account-info, as in solana-verify.
        assert_eq!(
            solana_image_version("[workspace]\n", Some(LOCK_WITH)),
            Some((1, 18, 26))
        );
        let only_info =
            "version = 4\n\n[[package]]\nname = \"solana-account-info\"\nversion = \"2.3.0\"\n";
        assert_eq!(
            solana_image_version("[workspace]\n", Some(only_info)),
            Some((2, 3, 0))
        );
        assert_eq!(solana_image_version("[workspace]\n", None), None);
        assert_eq!(
            solana_image_version("[workspace]\n", Some("version = 4\n")),
            None
        );
    }

    #[test]
    fn image_must_know_the_arch() {
        assert!(check_image_arch(Some((3, 0, 1)), "v3").is_ok());
        let e = check_image_arch(Some((1, 18, 26)), "v3")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("1.18.26") && e.contains("[workspace.metadata.cli]"),
            "{e}"
        );
        let e = check_image_arch(None, "v3").unwrap_err().to_string();
        assert!(e.contains("[workspace.metadata.cli]"), "{e}");
    }

    #[test]
    fn solana_2_image_gets_a_cargo_warning() {
        let w = image_warning(Some((2, 3, 0))).expect("warns");
        assert!(w.contains("edition 2024") && w.contains("3.0.1"), "{w}");
        assert_eq!(image_warning(Some((3, 0, 1))), None);
        assert_eq!(image_warning(None), None);
    }

    #[test]
    fn reads_release_debug() {
        assert_eq!(
            release_debug_setting("[profile.release]\ndebug = 1\n").as_deref(),
            Some("1")
        );
        assert_eq!(
            release_debug_setting("[profile.release]\ndebug = \"line-tables-only\"\n").as_deref(),
            Some("line-tables-only")
        );
        assert_eq!(
            release_debug_setting("[profile.release]\ndebug = 0\n"),
            None
        );
        assert_eq!(
            release_debug_setting("[profile.release]\ndebug = false\n"),
            None
        );
        assert_eq!(
            release_debug_setting("[profile.release]\ndebug = \"none\"\n"),
            None
        );
        assert_eq!(
            release_debug_setting("[profile.release]\nlto = \"fat\"\n"),
            None
        );
        assert_eq!(release_debug_setting("not toml ["), None);
    }

    #[test]
    fn build_override_debug_does_not_count() {
        let t = "[profile.release]\nlto = \"fat\"\n[profile.release.build-override]\ndebug = 1\n";
        assert_eq!(release_debug_setting(t), None);
    }

    #[test]
    fn workspace_root_found_from_member_crate() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[workspace]\nmembers=[\"programs/*\"]\n",
        )
        .unwrap();
        let p = d.path().join("programs/x");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(
            workspace_root(&p).unwrap().canonicalize().unwrap(),
            d.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn standalone_crate_is_its_own_workspace_root() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(
            workspace_root(d.path()).unwrap().canonicalize().unwrap(),
            d.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn version_compare_is_numeric() {
        assert_eq!(
            parse_solana_verify_version("solana-verify 0.5.10\n").unwrap(),
            (0, 5, 10)
        );
        assert_eq!(
            parse_solana_verify_version("solana-verify 0.5.2").unwrap(),
            (0, 5, 2)
        );
        assert!(parse_solana_verify_version("something else").is_err());
        assert!(version_at_least((0, 5, 10), (0, 5, 2)));
        assert!(version_at_least((0, 5, 2), (0, 5, 2)));
        assert!(!version_at_least((0, 4, 9), (0, 5, 2)));
    }

    #[test]
    fn dirty_tree_is_refused_and_build_dir_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        git(d.path(), &["add", "."]);
        git(d.path(), &["commit", "-q", "-m", "init"]);
        assert_eq!(git_head(d.path()).unwrap().len(), 40);

        std::fs::create_dir_all(d.path().join(".qed/build")).unwrap();
        std::fs::write(d.path().join(".qed/build/x.so"), b"elf").unwrap();
        assert_clean_tree(d.path()).unwrap();

        std::fs::write(d.path().join("new.rs"), "fn x() {}\n").unwrap();
        let e = assert_clean_tree(d.path()).unwrap_err().to_string();
        assert!(e.contains("commit"), "{e}");
    }

    /// `verify` and `codegen` write untracked metadata under `.qed/` (at the
    /// spec dir, which may be below the workspace root). It cannot change
    /// the built bytes, so it must not block a build or drop the evidence.
    #[test]
    fn qedgen_metadata_anywhere_does_not_dirty_the_tree() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        std::fs::write(d.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        git(d.path(), &["add", "."]);
        git(d.path(), &["commit", "-q", "-m", "init"]);
        std::fs::create_dir_all(d.path().join(".qed")).unwrap();
        std::fs::write(d.path().join(".qed/verify-evidence.json"), "{}").unwrap();
        std::fs::create_dir_all(d.path().join("programs/x/.qed")).unwrap();
        std::fs::write(d.path().join("programs/x/.qed/obligations.json"), "{}").unwrap();
        assert_clean_tree(d.path()).unwrap();

        std::fs::write(d.path().join("programs/x/lib.rs"), "fn x() {}\n").unwrap();
        assert!(assert_clean_tree(d.path()).is_err());
    }

    #[test]
    fn repo_relative_path_of_workspace() {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        let sub = d.path().join("programs/p");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(repo_relative(d.path()).unwrap(), "");
        assert_eq!(repo_relative(&sub).unwrap(), "programs/p");
    }

    #[test]
    fn git_head_outside_repo_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let e = git_head(d.path()).unwrap_err().to_string();
        assert!(e.contains("git"), "{e}");
    }
}
