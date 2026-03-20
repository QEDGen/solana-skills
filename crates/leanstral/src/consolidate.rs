use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const CONSOLIDATED_LAKEFILE: &str = r#"import Lake
open Lake DSL

package escrowProofs

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git" @ "v4.15.0"
require leanstralSupport from
  "./lean_support"

@[default_target]
lean_lib EscrowProofs where
  roots := #[`EscrowProofs]
"#;

const CONSOLIDATED_README: &str = r#"# Solana Program Lean Proofs

This directory contains formal verification proofs for the Solana program, generated using Leanstral.

## Building and Verifying

To build and verify all proofs:

```bash
lake build
```

This will verify all theorems and ensure they compile correctly.

## Structure

All proofs are contained in `EscrowProofs.lean`, organized into namespaces to avoid naming conflicts:
- Each proof has its own namespace
- Shared definitions from the Leanstral Solana library are imported at the top
- The `lean_support` directory contains the Solana modeling framework

## Generated Proofs

See `EscrowProofs.lean` for the complete list of theorems and their proofs.
"#;

const CONSOLIDATED_GITIGNORE: &str = r#"/.lake
/lake-manifest.json
/lakefile.olean
/lakefile.olean.trace
*.olean
*.trace
.lake
"#;

/// Find all Best.lean files in subdirectories of the given path
fn find_proof_files(input_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut proof_files = Vec::new();

    if !input_dir.is_dir() {
        anyhow::bail!("Input path is not a directory: {}", input_dir.display());
    }

    for entry in fs::read_dir(input_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let best_lean = path.join("Best.lean");
            if best_lean.exists() {
                proof_files.push(best_lean);
            }
        }
    }

    if proof_files.is_empty() {
        anyhow::bail!("No Best.lean files found in subdirectories of {}", input_dir.display());
    }

    proof_files.sort();
    Ok(proof_files)
}

/// Extract namespace from directory name (e.g., "cancel_access_control" -> "CancelAccessControl")
fn to_namespace(dir_name: &str) -> String {
    dir_name
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// Read a proof file and wrap it in a namespace
fn process_proof_file(proof_file: &Path) -> Result<(String, String)> {
    let content = fs::read_to_string(proof_file)
        .with_context(|| format!("Failed to read {}", proof_file.display()))?;

    // Get the parent directory name for the namespace
    let parent = proof_file
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Could not determine parent directory"))?;

    let namespace = to_namespace(parent);

    // Strip imports from the content since we'll have them at the top level
    let lines: Vec<&str> = content.lines().collect();
    let mut content_start = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with("import ")
            && !trimmed.starts_with("open ") {
            content_start = i;
            break;
        }
    }

    // Skip "open" statements too
    while content_start < lines.len() {
        let trimmed = lines[content_start].trim();
        if !trimmed.is_empty() && !trimmed.starts_with("open ") {
            break;
        }
        content_start += 1;
    }

    let proof_content = lines[content_start..].join("\n");

    Ok((namespace, proof_content))
}

/// Consolidate multiple Lean proof projects into a single project
pub fn consolidate_proofs(input_dir: &Path, output_dir: &Path) -> Result<()> {
    // Create output directory
    fs::create_dir_all(output_dir)?;

    // Find all proof files
    let proof_files = find_proof_files(input_dir)?;
    println!("Found {} proof files to consolidate", proof_files.len());

    // Build the consolidated proof file
    let mut consolidated = String::new();

    // Add imports at the top
    consolidated.push_str("import Mathlib.Tactic\n");
    consolidated.push_str("import Leanstral.Solana.Account\n");
    consolidated.push_str("import Leanstral.Solana.Authority\n");
    consolidated.push_str("import Leanstral.Solana.State\n");
    consolidated.push_str("import Leanstral.Solana.Token\n\n");
    consolidated.push_str("open Leanstral.Solana\n\n");

    // Process each proof file
    for proof_file in &proof_files {
        let (namespace, content) = process_proof_file(proof_file)?;

        consolidated.push_str(&format!(
            "/- {separator}\n   {title}\n   {separator} -/\n\n",
            separator = "=".repeat(76),
            title = format!("{} Proof", namespace)
        ));

        consolidated.push_str(&format!("namespace {}\n\n", namespace));
        consolidated.push_str(&content);
        consolidated.push_str("\n\nend ");
        consolidated.push_str(&namespace);
        consolidated.push_str("\n\n");
    }

    // Write the consolidated proof file
    fs::write(output_dir.join("EscrowProofs.lean"), consolidated)?;

    // Copy lean_support from one of the proof directories
    let first_proof_dir = proof_files[0].parent().unwrap();
    let source_support = first_proof_dir.join("lean_support");
    let dest_support = output_dir.join("lean_support");

    if source_support.exists() {
        copy_dir_recursive(&source_support, &dest_support)?;
    }

    // Copy lean-toolchain
    let source_toolchain = first_proof_dir.join("lean-toolchain");
    if source_toolchain.exists() {
        fs::copy(&source_toolchain, output_dir.join("lean-toolchain"))?;
    }

    // Write lakefile, README, and .gitignore
    fs::write(output_dir.join("lakefile.lean"), CONSOLIDATED_LAKEFILE)?;
    fs::write(output_dir.join("README.md"), CONSOLIDATED_README)?;
    fs::write(output_dir.join(".gitignore"), CONSOLIDATED_GITIGNORE)?;

    println!("Consolidated proofs written to {}", output_dir.display());
    println!("  - EscrowProofs.lean");
    println!("  - lakefile.lean");
    println!("  - lean-toolchain");
    println!("  - lean_support/");
    println!("  - README.md");
    println!("  - .gitignore");

    Ok(())
}

/// Recursively copy a directory
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)?;
        }
    }

    Ok(())
}
