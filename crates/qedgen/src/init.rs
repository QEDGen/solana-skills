use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.24.0\n";
const GITIGNORE: &str = ".lake/\nbuild/\nlake-packages/\nlean_solana/.lake/\nlean_solana/build/\n";
const QED_DIR: &str = ".qed";

/// Persistent project metadata stored in `.qed/config.json`.
#[derive(Serialize, Deserialize)]
pub struct QedConfig {
    pub name: String,
    pub spec: Option<String>,
    pub created_at: String,
}

/// Check whether `.qed/` exists in the same directory as `spec_path`.
/// `.qed/` is program-level, not repo-level — it lives next to the `.qedspec` file.
pub fn find_qed_dir(spec_path: &Path) -> Option<std::path::PathBuf> {
    let dir = if spec_path.is_file() {
        spec_path.parent()?
    } else {
        spec_path
    };
    let candidate = dir.join(QED_DIR);
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

/// Initialize `.qed/` in the given directory. Returns error if already initialized.
/// `dir` should be the program root — the directory where the `.qedspec` lives.
pub fn init_qed_dir(dir: &Path, name: &str) -> Result<()> {
    let qed_path = dir.join(QED_DIR);
    if qed_path.exists() {
        anyhow::bail!(
            "Already initialized — .qed/ exists in {}\n\
             To reinitialize, remove it first: rm -rf {}",
            dir.display(),
            qed_path.display()
        );
    }
    std::fs::create_dir_all(&qed_path)?;

    let config = QedConfig {
        name: name.to_string(),
        spec: None,
        created_at: chrono_now(),
    };
    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(qed_path.join("config.json"), json)?;

    // Add .qed/ gitignore for internal state (config.json is committed)
    std::fs::write(
        qed_path.join(".gitignore"),
        "# .qed/ is project metadata — commit config.json\n",
    )?;

    Ok(())
}

/// Simple ISO-8601 timestamp without pulling in chrono.
fn chrono_now() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", d.as_secs())
}

/// Scaffold a formal_verification/ project directory.
pub fn init(
    name: &str,
    output_dir: &Path,
    asm_source: Option<&Path>,
    mathlib: bool,
    quasar: bool,
) -> Result<()> {
    ensure!(!name.is_empty(), "project name must not be empty");
    ensure!(
        name.chars().all(|c| c.is_alphanumeric() || c == '_'),
        "project name must be alphanumeric (underscores allowed)"
    );

    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    // Write lean_solana/ support library (embedded in binary)
    crate::project::update_lean_solana(output_dir, mathlib)?;

    // Lean toolchain
    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;

    // .gitignore
    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;

    // If --asm, run asm2lean first
    let asm_module = if let Some(asm_path) = asm_source {
        let module_name = "Program".to_string();
        let output_file = output_dir.join("Program.lean");
        crate::asm2lean::asm2lean(asm_path, &output_file, Some(&module_name))?;
        eprintln!("Generated {}", output_file.display());
        Some(module_name)
    } else {
        None
    };

    // lakefile.lean
    let lakefile = generate_lakefile(name, asm_module.as_deref(), mathlib);
    std::fs::write(output_dir.join("lakefile.lean"), lakefile)?;

    // Spec.lean skeleton
    let spec = if quasar {
        generate_quasar_spec_skeleton(name)
    } else {
        generate_spec_skeleton(name)
    };
    std::fs::write(output_dir.join("Spec.lean"), spec)?;

    eprintln!("Initialized formal_verification project '{}'", name);
    eprintln!("  {}/", output_dir.display());
    eprintln!("  ├── lakefile.lean");
    eprintln!("  ├── lean-toolchain");
    eprintln!("  ├── lean_solana/        (support library)");
    if asm_module.is_some() {
        eprintln!("  ├── Program.lean");
    }
    eprintln!("  ├── Spec.lean          ← definitions + proofs");
    eprintln!("  └── .gitignore");
    eprintln!();
    eprintln!(
        "Next: edit Spec.lean, then run `lake build` in {}/",
        output_dir.display()
    );

    Ok(())
}

fn generate_lakefile(name: &str, asm_module: Option<&str>, mathlib: bool) -> String {
    let pkg_name = format!("{}Proofs", name);
    let mut s = String::new();

    s.push_str("import Lake\nopen Lake DSL\n\n");
    s.push_str(&format!("package {}\n\n", pkg_name));
    s.push_str("require qedgenSupport from\n  \"./lean_solana\"\n\n");

    if mathlib {
        s.push_str("require \"leanprover-community\" / \"mathlib\" @ git \"v4.24.0\"\n\n");
    }

    // asm2lean-generated program module
    if let Some(module) = asm_module {
        s.push_str(&format!(
            "lean_lib {} where\n  roots := #[`{}]\n\n",
            module, module
        ));
    }

    // Spec library (definitions + proofs in one file)
    s.push_str("@[default_target]\n");
    s.push_str(&format!(
        "lean_lib {}Spec where\n  roots := #[`Spec]\n",
        capitalize(name)
    ));

    s
}

fn generate_spec_skeleton(name: &str) -> String {
    let cap = capitalize(name);
    format!(
        r#"import QEDGen.Solana.Spec

open QEDGen.Solana.SpecDSL

/-!
# {} Verification Spec

Define the program's state, operations, invariants, and trust boundary here.
This file is the source of truth — proofs must satisfy the properties declared below.
-/

-- Uncomment and fill in your spec:
-- qedspec {} where
--   state
--     owner : Pubkey
--     amount : U64
--
--   operation initialize
--     who: owner
--     when: Uninitialized
--     then: Active
--
--   operation transfer
--     who: owner
--     when: Active
--     then: Active
--     calls: TOKEN_PROGRAM_ID DISC_TRANSFER(source writable, destination writable, authority signer)
--
--   invariant conservation "total tokens preserved"
"#,
        cap, cap
    )
}

fn generate_quasar_spec_skeleton(name: &str) -> String {
    let cap = capitalize(name);
    format!(
        r#"import QEDGen.Solana.Spec
open QEDGen.Solana.SpecDSL

/-!
# {cap} Verification Spec

This spec drives Quasar codegen, Lean proofs, and Kani harnesses.
Edit operations, context blocks, and properties to match your program.
-/

qedspec {cap} where
  program_id: "11111111111111111111111111111111"

  state
    authority : Pubkey
    value : U64

  event InitEvent {{ authority : Pubkey }}

  errors: Unauthorized

  operation initialize
    doc: "Initialize the program state"
    who: authority
    when: Uninitialized
    then: Active
    emits: InitEvent
    context: {{
      authority : Signer, mut
      system_program : Program, System
    }}

  operation update
    doc: "Update the value"
    who: authority
    when: Active
    then: Active
    takes: new_value U64
    guard: "new_value > 0"
    effect: value set new_value
    context: {{
      authority : Signer
    }}

  property bounded "s.value ≤ U64_MAX"
    preserved_by: update
"#,
    )
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}
