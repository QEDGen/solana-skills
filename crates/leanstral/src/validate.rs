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
) -> Result<ValidationResult> {
    let log_dir = output_dir.join("validation");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("completion_{}.log", completion_index));

    let workspace = if let Some(ws) = validation_workspace {
        std::fs::create_dir_all(ws)?;
        ensure_workspace_ready(ws).await?;
        ws.to_path_buf()
    } else {
        let ws = validation_workspace_dir()?;
        std::fs::create_dir_all(&ws)?;
        ensure_workspace_ready(&ws).await?;
        ws
    };

    // Copy Best.lean to validation workspace
    std::fs::copy(output_dir.join("Best.lean"), workspace.join("Best.lean"))?;

    // Run lake build
    let build_result = run_command(
        "lake",
        &["build", "Best"],
        &workspace,
        &[],
    )
    .await;

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

/// Set up the global validation workspace. Called by `leanstral setup` and
/// by the install script to pre-fetch the Mathlib cache.
pub async fn setup_workspace(workspace: Option<&Path>) -> Result<()> {
    let ws = if let Some(ws) = workspace {
        ws.to_path_buf()
    } else {
        validation_workspace_dir()?
    };

    std::fs::create_dir_all(&ws)?;
    eprintln!("Setting up validation workspace at {}...", ws.display());

    // Write full scaffold
    crate::project::setup_lean_project(&ws)?;
    eprintln!("  Project scaffold created.");

    // Resolve dependencies
    eprintln!("  Running lake update...");
    let _update = run_command("lake", &["update"], &ws, &[]).await;

    // Fetch Mathlib cache
    eprintln!("  Fetching Mathlib cache (this may take a few minutes)...");
    let cache_result = run_command("lake", &["exe", "cache", "get"], &ws, &[]).await;

    match &cache_result {
        Ok((_, _, code)) if *code == 0 => {
            eprintln!("  Mathlib cache fetched successfully.");
        }
        _ => {
            eprintln!("  Mathlib cache fetch failed. Building from source...");
            // Build Mathlib from source as fallback
            let build_result = run_command("lake", &["build", "Mathlib.Tactic"], &ws, &[]).await;
            match &build_result {
                Ok((_, _, code)) if *code == 0 => {
                    eprintln!("  Mathlib built from source successfully.");
                }
                _ => {
                    eprintln!("  Warning: Mathlib build failed. First validation run will be slow.");
                }
            }
        }
    }

    eprintln!("Workspace setup complete: {}", ws.display());
    Ok(())
}

/// Ensure the validation workspace is ready for `lake build Best`.
///
/// On first call (no lakefile.lean exists): sets up the full project scaffold,
/// runs `lake update` to resolve dependencies, and fetches the Mathlib cache
/// with `lake exe cache get` to avoid a 25+ minute source compilation.
///
/// On subsequent calls: only updates the lean_support/ files (which may change
/// when axioms are updated). The lakefile.lean, lean-toolchain, and .lake/ cache
/// are preserved to avoid invalidating the build cache.
async fn ensure_workspace_ready(workspace: &Path) -> Result<()> {
    let lakefile_path = workspace.join("lakefile.lean");
    let is_fresh = !lakefile_path.exists();

    if is_fresh {
        // First-time setup: write all scaffold files
        crate::project::setup_lean_project(workspace)?;

        // Resolve dependencies (downloads mathlib, etc.)
        eprintln!("  Setting up validation workspace (first time)...");
        let _update = run_command(
            "lake",
            &["update"],
            workspace,
            &[],
        )
        .await;

        // Fetch pre-built Mathlib oleans from cache (~1-2 min vs 25+ min from source)
        eprintln!("  Fetching Mathlib cache...");
        let cache_result = run_command(
            "lake",
            &["exe", "cache", "get"],
            workspace,
            &[],
        )
        .await;

        match &cache_result {
            Ok((_, _, code)) if *code == 0 => {
                eprintln!("  Mathlib cache fetched successfully.");
            }
            _ => {
                eprintln!("  Warning: Mathlib cache fetch failed, will build from source (slow).");
            }
        }
    } else {
        // Workspace already exists: only update lean_support files in case
        // axioms changed, but don't touch lakefile.lean or lean-toolchain
        // to preserve the .lake/ build cache.
        crate::project::update_lean_support(workspace)?;
    }

    Ok(())
}

async fn run_command(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<(String, String, i32)> {
    let mut command = Command::new(cmd);
    command.args(args).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());

    for (key, value) in env {
        command.env(key, value);
    }

    let output = command.output().await?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    Ok((stdout, stderr, code))
}

fn validation_workspace_dir() -> Result<PathBuf> {
    if let Ok(ws) = std::env::var("LEANSTRAL_VALIDATION_WORKSPACE") {
        return Ok(PathBuf::from(ws));
    }

    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(xdg)
            .join("leanstral-solana-skill")
            .join("validation-workspace"));
    }

    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Caches")
            .join("leanstral-solana-skill")
            .join("validation-workspace"));
    }

    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home)
        .join(".cache")
        .join("leanstral-solana-skill")
        .join("validation-workspace"))
}

pub fn summarize_build_log(build_log: &str) -> String {
    let lines: Vec<&str> = build_log.lines().collect();
    let error_re = regex::Regex::new(r"\berror:|^error\b|unknown identifier|unexpected token").unwrap();

    let mut error_line_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| error_re.is_match(line))
        .map(|(idx, _)| idx)
        .collect();

    if !error_line_indices.is_empty() {
        // Take last 8 error lines
        error_line_indices = error_line_indices.into_iter().rev().take(8).rev().collect();

        let mut selected_indices = std::collections::HashSet::new();
        for idx in error_line_indices {
            for i in idx.saturating_sub(3)..=std::cmp::min(lines.len() - 1, idx + 6) {
                selected_indices.insert(i);
            }
        }

        let mut sorted_indices: Vec<usize> = selected_indices.into_iter().collect();
        sorted_indices.sort_unstable();

        let result: String = sorted_indices
            .iter()
            .map(|&i| lines[i])
            .collect::<Vec<_>>()
            .join("\n");

        return result.chars().take(24000).collect();
    }

    // Fallback: last 250 lines
    lines
        .iter()
        .rev()
        .take(250)
        .rev()
        .map(|s| *s)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(24000)
        .collect()
}

fn diagnose_common_errors(build_log: &str, lean_code: &str) -> String {
    let mut guidance = String::new();

    // Check for rewrite pattern errors
    if build_log.contains("tactic 'rewrite' failed, did not find instance of the pattern") {
        let has_option_inj = lean_code.contains("Option.some.inj");
        let has_apply_inj = lean_code.contains("apply") && lean_code.contains("inj") && lean_code.contains("at");

        if has_option_inj || has_apply_inj {
            guidance.push_str(
                r#"
### DETECTED: Rewrite Direction Error After Option.some.inj

Your code uses `apply Option.some.inj at h` but the rewrite is failing.

**CRITICAL FIX**: After `apply Option.some.inj at h`, the hypothesis becomes: `inner_expr = result`

If your proof pattern looks like this:
```lean
rw [someFunction] at h
apply Option.some.inj at h
rw [h]  -- ❌ WRONG - This is likely the error!
```

You almost always need the LEFTWARD arrow:
```lean
rw [someFunction] at h
apply Option.some.inj at h
rw [← h]  -- ✅ CORRECT - Use leftward arrow
```

**Why**: After `Option.some.inj`, if `h : (map_expr) = p_result` and your goal contains `p_result`, you need `rw [← h]` to replace `p_result` with `map_expr` in the goal.

**Action**: Search for `rw [h]` or `rw [h_eq` patterns after `Option.some.inj` and add the `←` arrow.
"#
            );
        } else {
            guidance.push_str(
                r#"
### DETECTED: Rewrite Pattern Not Found

The `rw` tactic cannot find the pattern you're trying to rewrite. Common causes:
1. The expression in the hypothesis doesn't match the goal exactly
2. You're rewriting in the wrong direction (try adding `←` arrow: `rw [← h]`)
3. The pattern is under a binder or in a different form

**Action**: Check if you need to reverse the rewrite direction with `rw [← h]`.
"#
            );
        }
    }

    // Check for unused variable in if-expression
    if build_log.contains("unused variable") && lean_code.contains("if h :") {
        guidance.push_str(
            r#"
### DETECTED: Unused Variable in If-Expression

You have `if h : condition then ...` but never use the proof `h`.

**Fix**: Remove the proof binding:
```lean
-- Change this:
if h : x = y then some () else none

-- To this:
if x = y then some () else none
```

Only use `if h : condition` when you actually need the proof `h` in the then/else branches.
"#
        );
    }

    guidance
}

pub fn build_repair_prompt(
    original_prompt: &str,
    current_lean: &str,
    build_log: &str,
    round: usize,
) -> String {
    let summarized = summarize_build_log(build_log);
    let error_guidance = diagnose_common_errors(build_log, current_lean);

    format!(
        r#"You previously generated a Lean 4 proof module for this Solana verification task, but it did not compile.

Repair the Lean file using the compiler feedback below.

Hard requirements:
1. Return exactly one Lean 4 module.
2. Keep the same property target unless the build log proves it is impossible as stated.
3. Fix compiler errors concretely; do not leave declarations duplicated or theorem bodies empty.
4. Do not invent APIs or namespaces.
5. Prefer the smallest self-contained model that compiles under Lean 4.15 + Mathlib 4.15.
6. If a proof is incomplete, use `sorry` inside the proof body rather than leaving broken syntax.

This is repair round {}.
{}
## Original Verification Task
{}

## Previous Lean Module
```lean
{}
```

## Lean Build Output
```
{}
```

## Repair Goal
Produce a revised Lean module that addresses the reported compiler errors and is more likely to pass `lake build Best`. Return Lean code only.
"#,
        round, error_guidance, original_prompt, current_lean, summarized
    )
}
