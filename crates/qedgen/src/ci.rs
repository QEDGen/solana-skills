use anyhow::{Context, Result};
use std::path::Path;

/// Generate a GitHub Actions workflow for formal verification CI.
pub fn generate_ci(output_path: &Path, asm_source: Option<&str>) -> Result<()> {
    let verify_step = if let Some(asm) = asm_source {
        format!(
            r#"
      - name: Verify sBPF binary
        run: qedgen verify --asm {} --proofs formal_verification/"#,
            asm
        )
    } else {
        String::new()
    };

    let workflow = format!(
        r#"name: Formal Verification

on:
  push:
    branches: [main]
    paths:
      - 'src/**'
      - 'formal_verification/**'
  pull_request:
    paths:
      - 'src/**'
      - 'formal_verification/**'

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Lean
        uses: leanprover/lean4-action@v1

      - name: Cache lake packages
        uses: actions/cache@v4
        with:
          path: formal_verification/.lake
          key: lake-${{{{ hashFiles('formal_verification/lean-toolchain', 'formal_verification/lakefile.lean') }}}}

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Install qedgen
        run: cargo install --path crates/qedgen
{}
      - name: Build proofs
        run: cd formal_verification && lake build

      - name: Check spec coverage
        run: qedgen check --spec program.qedspec --proofs formal_verification/
"#,
        verify_step
    );

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(output_path, workflow)
        .with_context(|| format!("writing {}", output_path.display()))?;

    eprintln!("Generated CI workflow: {}", output_path.display());
    Ok(())
}
