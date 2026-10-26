//! `qedgen build` (#449): one verifiable `solana-verify build` that keeps the
//! stripped deploy `.so`, the unstripped `.so` (its debug info is the label
//! source for the lift), and a build record tied to a git commit.

pub(crate) mod deploy_check;
pub(crate) mod preflight;
pub(crate) mod selfcheck;
pub(crate) mod solana_verify;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const BUILD_DIR: &str = ".qed/build";
pub const RECORD_FILENAME: &str = "build.json";
pub const DEFAULT_ARCH: &str = "v3";
pub const DEFAULT_TOOLS_VERSION: &str = "v1.57";

/// What one `qedgen build` produced, tied to the commit it built.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildRecord {
    pub schema_version: u32,
    /// Full SHA of the commit that was built.
    pub commit: String,
    pub library: String,
    /// `solana-verify` style hash of the deploy `.so` (see
    /// [`selfcheck::program_hash`]); the number `solana-verify` prints.
    pub program_hash: String,
    /// Plain SHA-256 of the deploy `.so`, for `shasum` users.
    pub file_sha256: String,
    pub solana_verify_version: String,
    /// The Solana version of the image `solana-verify` built in, as it
    /// printed it. `solana-verify` does not print the image name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub platform_tools: String,
    pub arch: String,
    /// The `[profile.release] debug` value the build used.
    pub debug_setting: String,
    /// The `solana-verify` arguments that produced this build.
    pub build_argv: Vec<String>,
    /// The workspace's path inside the git repository (empty at the repo
    /// root). `verify-from-repo --mount-path` needs it to rebuild the same
    /// workspace.
    #[serde(default)]
    pub workspace_path: String,
    /// The self-check result. Always true: a failed check writes no record.
    pub text_matches: bool,
    pub recorded_at_unix: u64,
}

pub struct BuildOptions {
    pub program: PathBuf,
    pub library: Option<String>,
    pub arch: String,
    pub tools_version: String,
}

pub fn record_path(workspace: &Path) -> PathBuf {
    workspace.join(BUILD_DIR).join(RECORD_FILENAME)
}

/// The workspace whose `.qed/build/` holds the record for a verify run:
/// the workspace of `--program` when given, else the workspace of the spec
/// directory. It resolves the same way `qedgen build` does.
pub fn record_workspace(spec: &Path, program: Option<&Path>) -> Result<PathBuf> {
    let start = match program {
        Some(program) => program,
        None => spec
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    };
    preflight::workspace_root(start)
}

pub fn load_record(workspace: &Path) -> Result<Option<BuildRecord>> {
    let path = record_path(workspace);
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let record =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(record))
}

pub fn write_record(workspace: &Path, record: &BuildRecord) -> Result<PathBuf> {
    let path = record_path(workspace);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(record)?),
    )?;
    Ok(path)
}

/// Build the program in Docker, check the result, and keep both binaries
/// and the record under `<workspace>/.qed/build/`. Every refusal happens
/// before Docker starts.
pub fn run_build(opts: &BuildOptions) -> Result<BuildRecord> {
    let expected_sbpf = selfcheck::arch_to_sbpf_version(&opts.arch)?;
    let workspace = preflight::workspace_root(&opts.program)?;
    let commit = preflight::git_head(&workspace)?;
    let workspace_path = preflight::repo_relative(&workspace)?;
    let manifest = workspace.join("Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let Some(debug_setting) = preflight::release_debug_setting(&manifest_text) else {
        bail!(
            "set debug = 1 (or \"line-tables-only\") in [profile.release] of {} and commit it; \
             a verifier rebuild must produce the same labels",
            manifest.display()
        );
    };
    preflight::assert_clean_tree(&workspace)?;
    let lock = std::fs::read_to_string(workspace.join("Cargo.lock")).ok();
    if lock.is_none() {
        bail!(
            "{} has no Cargo.lock; solana-verify builds with --locked, so generate and \
             commit one (cargo generate-lockfile)",
            workspace.display()
        );
    }
    preflight::check_image_arch(
        preflight::solana_image_version(&manifest_text, lock.as_deref()),
        &opts.arch,
    )?;
    let library = match &opts.library {
        Some(name) => name.clone(),
        None => solana_verify::library_name(&opts.program)?,
    };
    let solana_verify_version = preflight::check_tools()?;

    let args = solana_verify::BuildArgs {
        library: library.clone(),
        arch: opts.arch.clone(),
        tools_version: opts.tools_version.clone(),
    };
    let output = solana_verify::run(&workspace, &args)?;

    let target = workspace.join("target");
    let deploy_path = target.join("deploy").join(format!("{library}.so"));
    let debug_path = target
        .join(solana_verify::sbpf_target_dir(&opts.arch))
        .join("release")
        .join(format!("{library}.so"));
    let deploy = std::fs::read(&deploy_path)
        .with_context(|| format!("reading {}", deploy_path.display()))?;
    let debug =
        std::fs::read(&debug_path).with_context(|| format!("reading {}", debug_path.display()))?;
    selfcheck::self_check(&deploy, &debug, expected_sbpf)?;

    let program_hash = selfcheck::program_hash(&deploy);
    if let Some(printed) = solana_verify::printed_program_hash(&output) {
        if printed != program_hash {
            bail!(
                "solana-verify printed program hash {printed}, but {} hashes to {program_hash}",
                deploy_path.display()
            );
        }
    }

    let out_dir = workspace.join(BUILD_DIR);
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(out_dir.join(".gitignore"), "*.so\n")?;
    std::fs::write(out_dir.join(format!("{library}.so")), &deploy)?;
    std::fs::write(out_dir.join(format!("{library}.debug.so")), &debug)?;

    let record = BuildRecord {
        schema_version: 1,
        commit,
        library,
        program_hash,
        file_sha256: selfcheck::file_sha256(&deploy),
        solana_verify_version,
        image: solana_verify::image_solana_version(&output).map(|v| format!("solana {v}")),
        platform_tools: opts.tools_version.clone(),
        arch: opts.arch.clone(),
        debug_setting,
        build_argv: solana_verify::build_argv(&args),
        workspace_path,
        text_matches: true,
        recorded_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let path = write_record(&workspace, &record)?;
    eprintln!(
        "Built {} at commit {}: program hash {}. Deploy .so, debug .so and record in {}",
        record.library,
        record.commit,
        record.program_hash,
        path.parent().unwrap_or(&workspace).display()
    );
    eprintln!(
        "To check a deploy later: {}",
        solana_verify::verify_command_hint(&args, &record.commit, &record.workspace_path)
    );
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BuildRecord {
        BuildRecord {
            schema_version: 1,
            commit: "a".repeat(40),
            library: "dbgprog".into(),
            program_hash: "b".repeat(64),
            file_sha256: "c".repeat(64),
            solana_verify_version: "0.5.2".into(),
            image: Some("solana v3.0.1".into()),
            platform_tools: "v1.57".into(),
            arch: "v3".into(),
            debug_setting: "1".into(),
            build_argv: vec!["build".into()],
            workspace_path: "programs/p".into(),
            text_matches: true,
            recorded_at_unix: 1,
        }
    }

    #[test]
    fn record_without_workspace_path_loads_as_repo_root() {
        let d = tempfile::tempdir().unwrap();
        let mut json = serde_json::to_value(sample()).unwrap();
        json.as_object_mut().unwrap().remove("workspace_path");
        std::fs::create_dir_all(d.path().join(BUILD_DIR)).unwrap();
        std::fs::write(record_path(d.path()), json.to_string()).unwrap();
        assert_eq!(load_record(d.path()).unwrap().unwrap().workspace_path, "");
    }

    /// Standard layout: `programs/x/x.qedspec` under a root `[workspace]`.
    /// `qedgen build --program programs/x` writes the record at the root, so
    /// verify must find it there with or without `--program`.
    #[test]
    fn record_workspace_resolves_to_the_build_workspace() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"programs/*\"]\n",
        )
        .unwrap();
        let program = d.path().join("programs/x");
        std::fs::create_dir_all(&program).unwrap();
        std::fs::write(program.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let spec = program.join("x.qedspec");
        std::fs::write(&spec, "").unwrap();
        let root = d.path().canonicalize().unwrap();
        assert_eq!(record_workspace(&spec, None).unwrap(), root);
        assert_eq!(record_workspace(&spec, Some(&program)).unwrap(), root);
        assert_eq!(
            record_workspace(std::path::Path::new("x.qedspec"), None).unwrap(),
            preflight::workspace_root(std::path::Path::new(".")).unwrap()
        );
    }

    #[test]
    fn record_round_trips() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load_record(d.path()).unwrap(), None);
        write_record(d.path(), &sample()).unwrap();
        assert_eq!(load_record(d.path()).unwrap(), Some(sample()));
        assert!(record_path(d.path()).ends_with(".qed/build/build.json"));
    }
}
