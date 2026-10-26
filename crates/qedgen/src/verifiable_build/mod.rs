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

/// Where one program's binaries and record live: `.qed/build/<library>/`.
/// Programs that share a workspace keep separate builds.
pub fn build_dir(workspace: &Path, library: &str) -> PathBuf {
    workspace.join(BUILD_DIR).join(library)
}

pub fn record_path(workspace: &Path, library: &str) -> PathBuf {
    build_dir(workspace, library).join(RECORD_FILENAME)
}

/// The build record a verify run should use: the workspace `qedgen build`
/// wrote it in, and the program's library name. The program is `--program`
/// when given, else the crate the spec sits in. `None` when the spec is not
/// in a program crate (for example at a workspace root), because then no
/// single program's build can be named.
pub fn record_target(spec: &Path, program: Option<&Path>) -> Result<Option<(PathBuf, String)>> {
    let anchor = match program {
        Some(program) => program,
        None => spec
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    };
    let workspace = preflight::workspace_root(anchor)?;
    if program.is_some() {
        return Ok(Some((workspace, solana_verify::library_name(anchor)?)));
    }
    let names_a_program = std::fs::read_to_string(anchor.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .is_some_and(|manifest| manifest.get("package").is_some() || manifest.get("lib").is_some());
    if !names_a_program {
        return Ok(None);
    }
    Ok(Some((workspace, solana_verify::library_name(anchor)?)))
}

pub fn load_record(workspace: &Path, library: &str) -> Result<Option<BuildRecord>> {
    let path = record_path(workspace, library);
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
    let path = record_path(workspace, &record.library);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(record)?),
    )?;
    Ok(path)
}

/// `--library` becomes a file name under `.qed/build/`, so it must be a
/// plain crate-style identifier, never a path.
pub fn validate_library(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("--library {name:?} must be a library name (letters, digits, _), not a path");
    }
    Ok(())
}

/// Compare the hash `solana-verify` printed with qedgen's own. A mismatch
/// is an error. When nothing was printed (for example after an output
/// format change), return a warning instead of skipping the check silently.
pub fn cross_check_hash(printed: Option<&str>, ours: &str) -> Result<Option<String>> {
    match printed {
        Some(printed) if printed == ours => Ok(None),
        Some(printed) => {
            bail!("solana-verify printed program hash {printed}, but qedgen computed {ours}")
        }
        None => Ok(Some(format!(
            "solana-verify did not print a program hash, so qedgen could not cross-check {ours}"
        ))),
    }
}

/// Write the two binaries to `out_dir` (this program's build directory) and
/// the `.gitignore` to its parent, first removing any `.so` an earlier build
/// of this program left there. Other programs' directories are not touched.
pub fn write_outputs(out_dir: &Path, library: &str, deploy: &[u8], debug: &[u8]) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    if let Some(build_root) = out_dir.parent() {
        std::fs::write(build_root.join(".gitignore"), "*.so\n")?;
    }
    for entry in std::fs::read_dir(out_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "so") {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing old build output {}", path.display()))?;
        }
    }
    std::fs::write(out_dir.join(format!("{library}.so")), deploy)?;
    std::fs::write(out_dir.join(format!("{library}.debug.so")), debug)?;
    Ok(())
}

/// Build the program in Docker, check the result, and keep both binaries
/// and the record under `<workspace>/.qed/build/`. Every refusal happens
/// before Docker starts.
pub fn run_build(opts: &BuildOptions) -> Result<BuildRecord> {
    let expected_sbpf = selfcheck::arch_to_sbpf_version(&opts.arch)?;
    let library = match &opts.library {
        Some(name) => name.clone(),
        None => solana_verify::library_name(&opts.program)?,
    };
    validate_library(&library)?;
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
    let image_version = preflight::solana_image_version(&manifest_text, lock.as_deref());
    preflight::check_image_arch(image_version, &opts.arch)?;
    if let Some(warning) = preflight::image_warning(image_version) {
        eprintln!("warning: {warning}");
    }

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
    if let Some(warning) = cross_check_hash(
        solana_verify::printed_program_hash(&output).as_deref(),
        &program_hash,
    )? {
        eprintln!("warning: {warning}");
    }

    preflight::assert_unchanged_since(&workspace, &commit)?;
    write_outputs(&build_dir(&workspace, &library), &library, &deploy, &debug)?;

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
        std::fs::create_dir_all(build_dir(d.path(), "dbgprog")).unwrap();
        std::fs::write(record_path(d.path(), "dbgprog"), json.to_string()).unwrap();
        let loaded = load_record(d.path(), "dbgprog").unwrap().unwrap();
        assert_eq!(loaded.workspace_path, "");
    }

    fn two_program_workspace() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"programs/*\"]\n",
        )
        .unwrap();
        for name in ["x", "y"] {
            let program = d.path().join("programs").join(name);
            std::fs::create_dir_all(&program).unwrap();
            std::fs::write(
                program.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            )
            .unwrap();
            std::fs::write(program.join(format!("{name}.qedspec")), "").unwrap();
        }
        d
    }

    /// The record belongs to one program: the workspace `qedgen build`
    /// wrote it in, and the program's library name. Without `--program`
    /// the spec's own crate names the program.
    #[test]
    fn record_target_names_the_workspace_and_the_library() {
        let d = two_program_workspace();
        let root = d.path().canonicalize().unwrap();
        let x = d.path().join("programs/x");
        let y = d.path().join("programs/y");
        assert_eq!(
            record_target(&x.join("x.qedspec"), None).unwrap(),
            Some((root.clone(), "x".to_string()))
        );
        assert_eq!(
            record_target(&x.join("x.qedspec"), Some(&y)).unwrap(),
            Some((root.clone(), "y".to_string()))
        );
        // A spec at the workspace root names no program.
        std::fs::write(d.path().join("all.qedspec"), "").unwrap();
        assert_eq!(
            record_target(&d.path().join("all.qedspec"), None).unwrap(),
            None
        );
    }

    /// Two programs in one workspace keep separate records and binaries:
    /// building one never removes or replaces the other.
    #[test]
    fn records_and_outputs_are_per_library() {
        let d = two_program_workspace();
        let mut x = sample();
        x.library = "x".into();
        let mut y = sample();
        y.library = "y".into();
        y.program_hash = "d".repeat(64);
        write_outputs(&build_dir(d.path(), "x"), "x", b"x-deploy", b"x-debug").unwrap();
        write_record(d.path(), &x).unwrap();
        write_outputs(&build_dir(d.path(), "y"), "y", b"y-deploy", b"y-debug").unwrap();
        write_record(d.path(), &y).unwrap();
        assert_eq!(load_record(d.path(), "x").unwrap(), Some(x));
        assert_eq!(load_record(d.path(), "y").unwrap(), Some(y));
        assert_eq!(
            std::fs::read(build_dir(d.path(), "x").join("x.so")).unwrap(),
            b"x-deploy"
        );
        assert!(record_path(d.path(), "x").ends_with(".qed/build/x/build.json"));
    }

    #[test]
    fn library_must_be_an_identifier() {
        validate_library("gate_anchor").unwrap();
        validate_library("Prog2").unwrap();
        for bad in ["../x", "a/b", "", "x.so", "a b"] {
            assert!(validate_library(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn hash_cross_check() {
        assert_eq!(cross_check_hash(Some("aa"), "aa").unwrap(), None);
        assert!(cross_check_hash(Some("bb"), "aa").is_err());
        let warning = cross_check_hash(None, "aa").unwrap().expect("warns");
        assert!(
            warning.contains("did not print a program hash"),
            "{warning}"
        );
    }

    #[test]
    fn outputs_replace_binaries_from_an_earlier_build() {
        let d = tempfile::tempdir().unwrap();
        let out = build_dir(d.path(), "new");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("old.so"), b"old").unwrap();
        std::fs::write(out.join("old.debug.so"), b"old").unwrap();
        std::fs::write(out.join("build.json"), b"{}").unwrap();
        write_outputs(&out, "new", b"deploy", b"debug").unwrap();
        assert!(!out.join("old.so").exists());
        assert!(!out.join("old.debug.so").exists());
        assert!(
            out.join("build.json").exists(),
            "the record is not a binary"
        );
        assert_eq!(std::fs::read(out.join("new.so")).unwrap(), b"deploy");
        assert_eq!(std::fs::read(out.join("new.debug.so")).unwrap(), b"debug");
        assert_eq!(
            std::fs::read_to_string(d.path().join(BUILD_DIR).join(".gitignore")).unwrap(),
            "*.so\n"
        );
    }

    #[test]
    fn record_round_trips() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load_record(d.path(), "dbgprog").unwrap(), None);
        write_record(d.path(), &sample()).unwrap();
        assert_eq!(load_record(d.path(), "dbgprog").unwrap(), Some(sample()));
    }
}
