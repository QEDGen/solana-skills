//! Dependency checks — called at the point of use, not at install time.
//!
//! Each function checks for a specific external dependency and returns
//! a clear error message with install instructions if it's missing.

use anyhow::{bail, Result};
use std::process::Command;

/// Check that `lake` (Lean build tool) is available.
/// Called before any command that needs to build Lean files.
pub fn require_lean() -> Result<()> {
    if Command::new("lake").arg("--version").output().is_ok() {
        return Ok(());
    }
    if Command::new("lean").arg("--version").output().is_ok() {
        bail!(
            "Lean is installed but `lake` was not found.\n\
             Try reinstalling via elan: https://github.com/leanprover/elan#installation"
        );
    }
    bail!(
        "Lean toolchain not found. It is required for building proofs.\n\n\
         Install elan (Lean version manager):\n\
         \n\
           curl -sSf https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh | sh\n\
         \n\
         Then run:\n\
         \n\
           qedgen setup            # set up validation workspace\n\
           qedgen setup --mathlib  # include Mathlib (adds 15-45 min)\n"
    );
}

/// Check that `cargo-kani` is available.
/// Called before any command that needs to run Kani harnesses.
pub fn require_kani() -> Result<()> {
    if Command::new("cargo-kani").arg("--version").output().is_ok() {
        return Ok(());
    }
    bail!(
        "Kani verifier not found. It is required for Kani proof harnesses.\n\n\
         Install Kani:\n\
         \n\
           cargo install --locked kani-verifier\n\
           cargo kani setup\n"
    );
}
