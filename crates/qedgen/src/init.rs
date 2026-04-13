use anyhow::{ensure, Context, Result};
use std::path::Path;

const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.24.0\n";
const GITIGNORE: &str = ".lake/\nbuild/\nlake-packages/\n";

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

    let proofs_dir = output_dir.join("Proofs");
    std::fs::create_dir_all(&proofs_dir)?;

    // Write lean_solana/ support library (embedded in binary)
    crate::project::update_lean_solana(output_dir)?;

    // Lean toolchain
    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;

    // .gitignore
    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;

    // If --asm, run asm2lean first so we know the module name
    let asm_module = if let Some(asm_path) = asm_source {
        let module_name = format!("{}Prog", capitalize(name));
        let output_file = output_dir.join(format!("{}.lean", module_name));
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

    // Proofs.lean root import
    let proofs_root = generate_proofs_root(name, asm_module.as_deref());
    std::fs::write(output_dir.join("Proofs.lean"), proofs_root)?;

    // Proofs/.gitkeep so the directory is tracked
    std::fs::write(proofs_dir.join(".gitkeep"), "")?;

    eprintln!("Initialized formal_verification project '{}'", name);
    eprintln!("  {}/", output_dir.display());
    eprintln!("  ├── lakefile.lean");
    eprintln!("  ├── lean-toolchain");
    eprintln!("  ├── lean_solana/        (support library)");
    eprintln!("  ├── Spec.lean          ← define your spec here");
    eprintln!("  ├── Proofs.lean");
    if asm_module.is_some() {
        eprintln!("  ├── {}Prog.lean", capitalize(name));
    }
    eprintln!("  ├── Proofs/");
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

    // Spec library
    s.push_str(&format!(
        "lean_lib {}Spec where\n  roots := #[`Spec]\n\n",
        capitalize(name)
    ));

    // Proofs library (default target)
    s.push_str("@[default_target]\n");
    s.push_str(&format!(
        "lean_lib {}Proofs where\n  roots := #[`Proofs]\n",
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

fn generate_proofs_root(_name: &str, asm_module: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str("import QEDGen.Solana\n");
    if let Some(module) = asm_module {
        s.push_str(&format!("import {}\n", module));
    }
    s.push_str("import Spec\n");
    s.push_str("\n-- Import proof files here as you write them:\n");
    s.push_str("-- import Proofs.AccessControl\n");
    s.push_str("-- import Proofs.CpiCorrectness\n");
    s.push_str("-- import Proofs.StateMachine\n");
    s.push_str("-- import Proofs.Conservation\n");
    s
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}
