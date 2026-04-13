use anyhow::Result;
use std::path::Path;

// Embed template files
const LAKEFILE: &str = include_str!("../templates/lakefile.lean");
const LEAN_TOOLCHAIN: &str = include_str!("../templates/lean-toolchain");
const MAIN_LEAN: &str = include_str!("../templates/Main.lean");
const GITIGNORE: &str = include_str!("../templates/.gitignore");
const README: &str = include_str!("../templates/README.lean.md");

// Embed lean_solana files (from repo root lean_solana/)
const SUPPORT_LAKEFILE: &str = include_str!("../../../lean_solana/lakefile.lean");
const SUPPORT_TOOLCHAIN: &str = include_str!("../../../lean_solana/lean-toolchain");
const SUPPORT_ROOT: &str = include_str!("../../../lean_solana/QEDGen.lean");
const SUPPORT_ACCOUNT: &str = include_str!("../../../lean_solana/QEDGen/Solana/Account.lean");
const SUPPORT_STATE: &str = include_str!("../../../lean_solana/QEDGen/Solana/State.lean");
const SUPPORT_CPI: &str = include_str!("../../../lean_solana/QEDGen/Solana/Cpi.lean");
const SUPPORT_VALID: &str = include_str!("../../../lean_solana/QEDGen/Solana/Valid.lean");
const SUPPORT_ARITHMETIC: &str = include_str!("../../../lean_solana/QEDGen/Solana/Arithmetic.lean");
const SUPPORT_SPEC: &str = include_str!("../../../lean_solana/QEDGen/Solana/Spec.lean");
// Trimmed barrel import — only the modules we embed (no SBPF/Bridge/Guards)
const SUPPORT_SOLANA_BASE: &str = "\
import QEDGen.Solana.Account\n\
import QEDGen.Solana.Cpi\n\
import QEDGen.Solana.State\n\
import QEDGen.Solana.Valid\n\
import QEDGen.Solana.Spec\n";

const SUPPORT_SOLANA_MATHLIB: &str = "\
import QEDGen.Solana.Account\n\
import QEDGen.Solana.Arithmetic\n\
import QEDGen.Solana.Cpi\n\
import QEDGen.Solana.State\n\
import QEDGen.Solana.Valid\n\
import QEDGen.Solana.Spec\n";

const MATHLIB_REQUIRE: &str = "\nrequire mathlib from git\n  \
\"https://github.com/leanprover-community/mathlib4.git\"\n";

pub fn setup_lean_project(output_dir: &Path, mathlib: bool) -> Result<()> {
    // Write template files
    std::fs::write(output_dir.join("lakefile.lean"), LAKEFILE)?;
    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;
    std::fs::write(output_dir.join("Main.lean"), MAIN_LEAN)?;
    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;
    std::fs::write(output_dir.join("README.md"), README)?;

    // Write lean_solana directory
    write_lean_solana(output_dir, mathlib)?;

    Ok(())
}

/// Update only the lean_solana/ files without touching lakefile.lean or
/// lean-toolchain. This preserves the .lake/ build cache while ensuring
/// axiom definitions are current.
pub fn update_lean_solana(output_dir: &Path, mathlib: bool) -> Result<()> {
    write_lean_solana(output_dir, mathlib)
}

fn write_lean_solana(output_dir: &Path, mathlib: bool) -> Result<()> {
    let support_dir = output_dir.join("lean_solana");
    std::fs::create_dir_all(&support_dir)?;

    // Inject mathlib require into lean_solana lakefile when opted in
    if mathlib {
        let lakefile = format!("{}{}", SUPPORT_LAKEFILE, MATHLIB_REQUIRE);
        std::fs::write(support_dir.join("lakefile.lean"), lakefile)?;
    } else {
        std::fs::write(support_dir.join("lakefile.lean"), SUPPORT_LAKEFILE)?;
    }
    std::fs::write(support_dir.join("lean-toolchain"), SUPPORT_TOOLCHAIN)?;
    std::fs::write(support_dir.join("QEDGen.lean"), SUPPORT_ROOT)?;

    // Write QEDGen/Solana.lean (namespace file)
    let qedgen_dir = support_dir.join("QEDGen");
    std::fs::create_dir_all(&qedgen_dir)?;
    let solana_barrel = if mathlib {
        SUPPORT_SOLANA_MATHLIB
    } else {
        SUPPORT_SOLANA_BASE
    };
    std::fs::write(qedgen_dir.join("Solana.lean"), solana_barrel)?;

    // Write QEDGen/Solana modules
    let solana_dir = support_dir.join("QEDGen/Solana");
    std::fs::create_dir_all(&solana_dir)?;
    std::fs::write(solana_dir.join("Account.lean"), SUPPORT_ACCOUNT)?;
    std::fs::write(solana_dir.join("State.lean"), SUPPORT_STATE)?;
    std::fs::write(solana_dir.join("Cpi.lean"), SUPPORT_CPI)?;
    std::fs::write(solana_dir.join("Valid.lean"), SUPPORT_VALID)?;
    std::fs::write(solana_dir.join("Spec.lean"), SUPPORT_SPEC)?;

    // Only deploy Arithmetic.lean when Mathlib is opted in
    if mathlib {
        std::fs::write(solana_dir.join("Arithmetic.lean"), SUPPORT_ARITHMETIC)?;
    }

    Ok(())
}
