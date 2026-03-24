use anyhow::Result;
use std::path::Path;

// Embed template files
const LAKEFILE: &str = include_str!("../templates/lakefile.lean");
const LEAN_TOOLCHAIN: &str = include_str!("../templates/lean-toolchain");
const MAIN_LEAN: &str = include_str!("../templates/Main.lean");
const GITIGNORE: &str = include_str!("../templates/.gitignore");
const README: &str = include_str!("../templates/README.lean.md");

// Embed lean_support files
const SUPPORT_LAKEFILE: &str = include_str!("../lean_support/lakefile.lean");
const SUPPORT_TOOLCHAIN: &str = include_str!("../lean_support/lean-toolchain");
const SUPPORT_ROOT: &str = include_str!("../lean_support/Leanstral.lean");
const SUPPORT_ACCOUNT: &str = include_str!("../lean_support/Leanstral/Solana/Account.lean");
const SUPPORT_AUTHORITY: &str = include_str!("../lean_support/Leanstral/Solana/Authority.lean");
const SUPPORT_STATE: &str = include_str!("../lean_support/Leanstral/Solana/State.lean");
const SUPPORT_TOKEN: &str = include_str!("../lean_support/Leanstral/Solana/Token.lean");
const SUPPORT_CPI: &str = include_str!("../lean_support/Leanstral/Solana/Cpi.lean");
const SUPPORT_VALID: &str = include_str!("../lean_support/Leanstral/Solana/Valid.lean");
const SUPPORT_SOLANA: &str = include_str!("../lean_support/Leanstral/Solana.lean");

pub fn setup_lean_project(output_dir: &Path) -> Result<()> {
    // Write template files
    std::fs::write(output_dir.join("lakefile.lean"), LAKEFILE)?;
    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;
    std::fs::write(output_dir.join("Main.lean"), MAIN_LEAN)?;
    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;
    std::fs::write(output_dir.join("README.md"), README)?;

    // Write lean_support directory
    write_lean_support(output_dir)?;

    Ok(())
}

/// Update only the lean_support/ files without touching lakefile.lean or
/// lean-toolchain. This preserves the .lake/ build cache while ensuring
/// axiom definitions are current.
pub fn update_lean_support(output_dir: &Path) -> Result<()> {
    write_lean_support(output_dir)
}

fn write_lean_support(output_dir: &Path) -> Result<()> {
    let support_dir = output_dir.join("lean_support");
    std::fs::create_dir_all(&support_dir)?;
    std::fs::write(support_dir.join("lakefile.lean"), SUPPORT_LAKEFILE)?;
    std::fs::write(support_dir.join("lean-toolchain"), SUPPORT_TOOLCHAIN)?;
    std::fs::write(support_dir.join("Leanstral.lean"), SUPPORT_ROOT)?;

    // Write Leanstral/Solana.lean (namespace file)
    let leanstral_dir = support_dir.join("Leanstral");
    std::fs::create_dir_all(&leanstral_dir)?;
    std::fs::write(leanstral_dir.join("Solana.lean"), SUPPORT_SOLANA)?;

    // Write Leanstral/Solana modules
    let solana_dir = support_dir.join("Leanstral/Solana");
    std::fs::create_dir_all(&solana_dir)?;
    std::fs::write(solana_dir.join("Account.lean"), SUPPORT_ACCOUNT)?;
    std::fs::write(solana_dir.join("Authority.lean"), SUPPORT_AUTHORITY)?;
    std::fs::write(solana_dir.join("State.lean"), SUPPORT_STATE)?;
    std::fs::write(solana_dir.join("Token.lean"), SUPPORT_TOKEN)?;
    std::fs::write(solana_dir.join("Cpi.lean"), SUPPORT_CPI)?;
    std::fs::write(solana_dir.join("Valid.lean"), SUPPORT_VALID)?;

    Ok(())
}
