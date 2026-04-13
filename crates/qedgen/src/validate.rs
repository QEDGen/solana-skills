use crate::api::BuildStatus;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

pub struct ValidationResult {
    pub status: BuildStatus,
    pub log_path: Option<PathBuf>,
}

pub async fn validate_completion(
    output_dir: &Path,
    completion_index: usize,
    validation_workspace: Option<&Path>,
    mathlib: bool,
) -> Result<ValidationResult> {
    let log_dir = output_dir.join("validation");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("completion_{}.log", completion_index));

    let workspace = match validation_workspace {
        Some(ws) => ws.to_path_buf(),
        None => validation_workspace_dir()?,
    };
    std::fs::create_dir_all(&workspace)?;
    ensure_workspace_ready(&workspace, mathlib).await?;

    // Copy Best.lean to validation workspace
    std::fs::copy(output_dir.join("Best.lean"), workspace.join("Best.lean"))?;

    // Run lake build
    let build_result = run_command("lake", &["build", "Best"], &workspace, &[]).await;

    match build_result {
        Ok((stdout, stderr, code)) => {
            let combined = format!("{}\n{}", stdout, stderr);
            std::fs::write(&log_path, &combined)?;
            Ok(ValidationResult {
                status: if code == 0 {
                    BuildStatus::Success
                } else {
                    BuildStatus::Failed
                },
                log_path: Some(log_path),
            })
        }
        Err(e) => {
            std::fs::write(&log_path, format!("Error: {}", e))?;
            Ok(ValidationResult {
                status: BuildStatus::Skipped,
                log_path: Some(log_path),
            })
        }
    }
}

/// Set up the global validation workspace. Called by `qedgen setup` and
/// by the install script to pre-fetch the Mathlib cache.
pub async fn setup_workspace(workspace: Option<&Path>, mathlib: bool) -> Result<()> {
    let ws = match workspace {
        Some(ws) => ws.to_path_buf(),
        None => validation_workspace_dir()?,
    };

    std::fs::create_dir_all(&ws)?;
    eprintln!("Setting up validation workspace at {}...", ws.display());

    crate::project::setup_lean_project(&ws, mathlib)?;
    eprintln!("  Project scaffold created.");

    eprintln!("  Running lake update...");
    let _update = run_command("lake", &["update"], &ws, &[]).await;

    if mathlib {
        fetch_or_build_mathlib(&ws).await;
    }

    eprintln!("Workspace setup complete: {}", ws.display());
    Ok(())
}

/// Ensure the validation workspace is ready for `lake build Best`.
///
/// On first call (no lakefile.lean exists): sets up the full project scaffold,
/// runs `lake update` to resolve dependencies, and fetches the Mathlib cache.
///
/// On subsequent calls: only updates the lean_solana/ files (which may change
/// when axioms are updated), preserving .lake/ build cache.
async fn ensure_workspace_ready(workspace: &Path, mathlib: bool) -> Result<()> {
    if !workspace.join("lakefile.lean").exists() {
        crate::project::setup_lean_project(workspace, mathlib)?;

        eprintln!("  Setting up validation workspace (first time)...");
        let _update = run_command("lake", &["update"], workspace, &[]).await;

        if mathlib {
            fetch_or_build_mathlib(workspace).await;
        }
    } else {
        crate::project::update_lean_solana(workspace, mathlib)?;
    }

    Ok(())
}

/// Fetch pre-built Mathlib oleans, falling back to building from source.
async fn fetch_or_build_mathlib(workspace: &Path) {
    eprintln!("  Fetching Mathlib cache (this may take a few minutes)...");
    let cache_result = run_command("lake", &["exe", "cache", "get"], workspace, &[]).await;

    match &cache_result {
        Ok((_, _, code)) if *code == 0 => {
            eprintln!("  Mathlib cache fetched successfully.");
        }
        _ => {
            eprintln!("  Mathlib cache fetch failed. Building from source...");
            let build_result =
                run_command("lake", &["build", "Mathlib.Tactic"], workspace, &[]).await;
            match &build_result {
                Ok((_, _, code)) if *code == 0 => {
                    eprintln!("  Mathlib built from source successfully.");
                }
                _ => {
                    eprintln!(
                        "  Warning: Mathlib build failed. First validation run will be slow."
                    );
                }
            }
        }
    }
}

async fn run_command(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<(String, String, i32)> {
    let mut command = Command::new(cmd);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in env {
        command.env(key, value);
    }

    let output = command.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    Ok((stdout, stderr, code))
}

/// Returns the path to ~/.qedgen/ — the global QEDGen home directory.
/// Override with QEDGEN_HOME env var.
pub fn qedgen_home() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("QEDGEN_HOME") {
        return Ok(PathBuf::from(home));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".qedgen"))
}

fn validation_workspace_dir() -> Result<PathBuf> {
    if let Ok(ws) = std::env::var("QEDGEN_VALIDATION_WORKSPACE") {
        return Ok(PathBuf::from(ws));
    }
    Ok(qedgen_home()?.join("workspace"))
}
