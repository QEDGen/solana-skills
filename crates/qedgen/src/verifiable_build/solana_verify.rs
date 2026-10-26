//! The `solana-verify build` command. It builds in Docker inside the mounted
//! workspace and keeps `target/`, so the unstripped binary survives.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildArgs {
    pub library: String,
    pub arch: String,
    pub tools_version: String,
}

fn cargo_build_sbf_arg(a: &BuildArgs) -> String {
    format!("--cargo-build-sbf-args=--tools-version {}", a.tools_version)
}

/// Arguments to `solana-verify` for the build.
pub fn build_argv(a: &BuildArgs) -> Vec<String> {
    vec![
        "build".into(),
        "--library-name".into(),
        a.library.clone(),
        "--arch".into(),
        a.arch.clone(),
        cargo_build_sbf_arg(a),
    ]
}

/// Arguments to `solana-verify` that rebuild the same commit and compare it
/// with a deployed program. `mount_path` is the workspace's path inside the
/// repo (empty at the repo root). For Anchor 0.32+ the same arguments pass
/// through `anchor verify`.
pub fn verify_from_repo_argv(
    a: &BuildArgs,
    repo_url: &str,
    commit: &str,
    program_id: &str,
    mount_path: &str,
) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "verify-from-repo".into(),
        "--program-id".into(),
        program_id.into(),
        "--commit-hash".into(),
        commit.into(),
        "--library-name".into(),
        a.library.clone(),
        "--arch".into(),
        a.arch.clone(),
        cargo_build_sbf_arg(a),
    ];
    if !mount_path.is_empty() {
        argv.push("--mount-path".into());
        argv.push(mount_path.into());
    }
    argv.push(repo_url.into());
    argv
}

/// A shell command that rebuilds `commit` and compares it with a deployed
/// program. The program id and repo URL are placeholders: the build does not
/// know them. `mount_path` is the workspace's path inside the repo.
pub fn verify_command_hint(a: &BuildArgs, commit: &str, mount_path: &str) -> String {
    let argv = verify_from_repo_argv(a, "<REPO_URL>", commit, "<PROGRAM_ID>", mount_path);
    let quoted: Vec<String> = argv
        .into_iter()
        .map(|arg| {
            if arg.contains(' ') {
                format!("'{arg}'")
            } else {
                arg
            }
        })
        .collect();
    format!("solana-verify {}", quoted.join(" "))
}

/// The `target/` subdirectory that holds the unstripped binary.
pub fn sbpf_target_dir(arch: &str) -> String {
    if arch == "v0" {
        "sbpf-solana-solana".to_string()
    } else {
        format!("sbpf{arch}-solana-solana")
    }
}

/// The verify images are amd64 only.
pub fn needs_amd64_emulation() -> bool {
    std::env::consts::ARCH != "x86_64"
}

/// The library name `cargo build-sbf` uses for the `.so`: `[lib] name`, else
/// the package name with `-` replaced by `_`.
pub fn library_name(program: &Path) -> Result<String> {
    let path = program.join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let manifest: toml::Value = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    let name = |table: &str| {
        manifest
            .get(table)
            .and_then(|t| t.get("name"))
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    match name("lib").or_else(|| name("package").map(|n| n.replace('-', "_"))) {
        Some(name) => Ok(name),
        None => bail!(
            "{} has no [lib] or [package] name; pass --library",
            path.display()
        ),
    }
}

/// Run `solana-verify build` in `workspace`. Its output streams to the
/// terminal and is also returned, so the caller can read the hash and the
/// image version it printed.
pub fn run(workspace: &Path, a: &BuildArgs) -> Result<String> {
    let argv = build_argv(a);
    let mut cmd = Command::new("solana-verify");
    cmd.args(&argv)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if needs_amd64_emulation() {
        cmd.env("DOCKER_DEFAULT_PLATFORM", "linux/amd64");
    }
    let mut child = cmd
        .spawn()
        .context("starting solana-verify; is it installed?")?;
    let mut captured = String::new();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let line = line.context("reading solana-verify output")?;
            println!("{line}");
            captured.push_str(&line);
            captured.push('\n');
        }
    }
    let status = child.wait().context("waiting for solana-verify")?;
    if !status.success() {
        bail!(
            "`solana-verify {}` failed in {} ({status})",
            argv.join(" "),
            workspace.display()
        );
    }
    Ok(captured)
}

/// The program hash `solana-verify build` prints on its own line at the end.
pub fn printed_program_hash(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

/// The Solana version of the image `solana-verify` built in. It does not
/// print the image name, so this is the best record of the image.
pub fn image_solana_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Docker image Solana version:")
            .map(|v| v.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_argv_is_exact() {
        let a = BuildArgs {
            library: "dbgprog".into(),
            arch: "v3".into(),
            tools_version: "v1.57".into(),
        };
        assert_eq!(
            build_argv(&a),
            vec![
                "build",
                "--library-name",
                "dbgprog",
                "--arch",
                "v3",
                "--cargo-build-sbf-args=--tools-version v1.57"
            ]
        );
    }

    #[test]
    fn target_dir_by_arch() {
        assert_eq!(sbpf_target_dir("v0"), "sbpf-solana-solana");
        assert_eq!(sbpf_target_dir("v3"), "sbpfv3-solana-solana");
    }

    #[test]
    fn library_name_prefers_lib_table() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"pinocchio-token-program\"\n[lib]\nname = \"pinocchio_token_program\"\n",
        )
        .unwrap();
        assert_eq!(library_name(d.path()).unwrap(), "pinocchio_token_program");

        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"my-prog\"\n",
        )
        .unwrap();
        assert_eq!(library_name(d.path()).unwrap(), "my_prog");

        std::fs::write(d.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert!(library_name(d.path()).is_err());
    }

    #[test]
    fn verify_from_repo_argv_carries_build_settings() {
        let a = BuildArgs {
            library: "p".into(),
            arch: "v3".into(),
            tools_version: "v1.57".into(),
        };
        let v = verify_from_repo_argv(&a, "https://github.com/o/r", "abc123", "Prog111", "");
        assert!(!v.contains(&"--mount-path".to_string()));
        assert_eq!(v[0], "verify-from-repo");
        assert!(v.windows(2).any(|w| w == ["--program-id", "Prog111"]));
        assert!(v.windows(2).any(|w| w == ["--commit-hash", "abc123"]));
        assert!(v.windows(2).any(|w| w == ["--library-name", "p"]));
        assert!(v.windows(2).any(|w| w == ["--arch", "v3"]));
        assert!(v.contains(&"--cargo-build-sbf-args=--tools-version v1.57".to_string()));
        assert_eq!(v.last().unwrap(), "https://github.com/o/r");
    }

    #[test]
    fn verify_hint_is_a_runnable_command() {
        let a = BuildArgs {
            library: "p".into(),
            arch: "v3".into(),
            tools_version: "v1.57".into(),
        };
        assert_eq!(
            verify_command_hint(&a, "abc123", ""),
            "solana-verify verify-from-repo --program-id <PROGRAM_ID> --commit-hash abc123 \
             --library-name p --arch v3 '--cargo-build-sbf-args=--tools-version v1.57' <REPO_URL>"
        );
    }

    /// A workspace below the repo root must be rebuilt from that path, or
    /// `verify-from-repo` builds the repo root instead.
    #[test]
    fn subdirectory_workspace_adds_mount_path() {
        let a = BuildArgs {
            library: "p".into(),
            arch: "v3".into(),
            tools_version: "v1.57".into(),
        };
        let v = verify_from_repo_argv(
            &a,
            "https://github.com/o/r",
            "abc123",
            "Prog111",
            "programs/p",
        );
        assert!(v.windows(2).any(|w| w == ["--mount-path", "programs/p"]));
        assert!(verify_command_hint(&a, "abc123", "programs/p").contains("--mount-path programs/p"));
    }

    const SAMPLE_OUTPUT: &str = "Workdir: /build\n\
        Finished building program\n\
        Program Solana version: v3.0.1\n\
        Docker image Solana version: v3.0.1\n\
        bd3bd62b3dd2488007db69ce130cff85a286dd5868330b362925d064789673ea\n";

    #[test]
    fn parses_printed_hash_and_image() {
        assert_eq!(
            printed_program_hash(SAMPLE_OUTPUT).as_deref(),
            Some("bd3bd62b3dd2488007db69ce130cff85a286dd5868330b362925d064789673ea")
        );
        assert_eq!(
            image_solana_version(SAMPLE_OUTPUT).as_deref(),
            Some("v3.0.1")
        );
        assert_eq!(printed_program_hash("no hash here\n"), None);
        assert_eq!(image_solana_version("no image\n"), None);
    }
}
