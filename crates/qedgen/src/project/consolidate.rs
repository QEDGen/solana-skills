use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Lakefile for the consolidated project. `name` follows the same
/// `package <name>Proofs` / `lean_lib <Name>Proofs` convention as
/// `init.rs::generate_lakefile` (this was previously hardcoded to
/// `escrowProofs` for every project).
fn consolidated_lakefile(name: &str) -> String {
    let pascal = to_namespace(name);
    format!(
        r#"import Lake
open Lake DSL

package {name}Proofs

require qedgenSupport from
  "./lean_solana"

@[default_target]
lean_lib {pascal}Proofs where
  roots := #[`{pascal}Proofs]
"#
    )
}

/// Project name for the consolidated lakefile: the output directory's
/// basename, sanitized to a Lean-identifier-safe form (separators become
/// `_`, anything else non-alphanumeric is dropped).
fn consolidated_project_name(output_dir: &Path) -> String {
    let raw = output_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("consolidated");
    let cleaned: String = raw
        .chars()
        .map(|c| if c == '-' || c == ' ' { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if cleaned.trim_matches('_').is_empty() {
        "consolidated".to_string()
    } else {
        cleaned
    }
}

const CONSOLIDATED_README: &str = r#"# Solana Program Lean Proofs

This directory contains formal verification proofs for the Solana program, generated using QEDGen.

## Building and Verifying

To build and verify all proofs:

```bash
lake build
```

This will verify all theorems and ensure they compile correctly.

## Structure

All proofs are contained in `Proofs.lean`, organized into namespaces to avoid naming conflicts:
- Each proof has its own namespace
- Shared definitions from the QEDGen Solana library are imported at the top
- The `lean_solana` directory contains the Solana modeling framework

## Generated Proofs

See `Proofs.lean` for the complete list of theorems and their proofs.
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
        anyhow::bail!(
            "No Best.lean files found in subdirectories of {}",
            input_dir.display()
        );
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

/// Read a proof file, extract its imports, and wrap the body in a namespace
fn process_proof_file(proof_file: &Path) -> Result<(String, Vec<String>, String)> {
    let content = fs::read_to_string(proof_file)
        .with_context(|| format!("Failed to read {}", proof_file.display()))?;

    let parent = proof_file
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Could not determine parent directory"))?;

    let namespace = to_namespace(parent);

    // Collect imports and find where the body starts
    let lines: Vec<&str> = content.lines().collect();
    let mut imports = Vec::new();
    let mut content_start = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") {
            imports.push(trimmed.to_string());
        } else if !trimmed.is_empty() && !trimmed.starts_with("open ") && !trimmed.starts_with("--")
        {
            content_start = i;
            break;
        }
    }

    // Skip any remaining open/empty lines
    while content_start < lines.len() {
        let trimmed = lines[content_start].trim();
        if !trimmed.is_empty() && !trimmed.starts_with("open ") {
            break;
        }
        content_start += 1;
    }

    let proof_content = lines[content_start..].join("\n");

    Ok((namespace, imports, proof_content))
}

/// Consolidate multiple Lean proof projects into a single project
pub fn consolidate_proofs(input_dir: &Path, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)?;

    let proof_files = find_proof_files(input_dir)?;
    println!("Found {} proof files to consolidate", proof_files.len());

    let mut all_imports = std::collections::BTreeSet::new();
    let mut proofs = Vec::new();

    for proof_file in &proof_files {
        let (namespace, imports, content) = process_proof_file(proof_file)?;
        all_imports.extend(imports);
        proofs.push((namespace, content));
    }

    let mut consolidated = String::new();

    for import in &all_imports {
        consolidated.push_str(import);
        consolidated.push('\n');
    }
    consolidated.push_str("\nopen QEDGen.Solana\n\n");

    for (namespace, content) in &proofs {
        consolidated.push_str(&format!(
            "/- {separator}\n   {namespace} Proof\n   {separator} -/\n\n",
            separator = "=".repeat(76),
        ));

        consolidated.push_str(&format!("namespace {}\n\n", namespace));
        consolidated.push_str(content);
        consolidated.push_str("\n\nend ");
        consolidated.push_str(namespace);
        consolidated.push_str("\n\n");
    }

    fs::write(output_dir.join("Proofs.lean"), consolidated)?;

    crate::project::update_lean_solana(output_dir, false)?;

    let toolchain = include_str!("../../../../lean_solana/lean-toolchain");
    fs::write(output_dir.join("lean-toolchain"), toolchain)?;

    let project_name = consolidated_project_name(output_dir);
    fs::write(
        output_dir.join("lakefile.lean"),
        consolidated_lakefile(&project_name),
    )?;
    fs::write(output_dir.join("README.md"), CONSOLIDATED_README)?;
    fs::write(output_dir.join(".gitignore"), CONSOLIDATED_GITIGNORE)?;

    println!("Consolidated proofs written to {}", output_dir.display());
    println!("  - Proofs.lean");
    println!("  - lakefile.lean");
    println!("  - lean-toolchain");
    println!("  - lean_solana/");
    println!("  - README.md");
    println!("  - .gitignore");

    Ok(())
}
