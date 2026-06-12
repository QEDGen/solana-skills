//! Dependency checks — called at point of use, not install time; each
//! returns an install-instruction error when missing.

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

/// Check that `crucible` (Crucible coverage-guided fuzzer) is available.
/// Called before any command that needs to run a fuzz harness.
pub fn require_crucible() -> Result<()> {
    if Command::new("crucible").arg("--version").output().is_ok() {
        return Ok(());
    }
    bail!(
        "Crucible fuzzer not found. Required for `qedgen probe --fuzz` and `qedgen verify --crucible`.\n\n\
         Install Crucible:\n\
         \n\
           cargo install --git https://github.com/asymmetric-research/crucible crucible-fuzz-cli\n\
         \n\
         Crucible is alpha software pinned to Solana v3 + Anchor 1.0.1. The first\n\
         build of a fuzz harness can take 2-5 minutes (LibAFL is a heavy dep tree).\n"
    );
}

/// Check that `cargo +nightly miri` is available (`qedgen verify --miri`).
pub fn require_miri() -> Result<()> {
    let out = Command::new("cargo")
        .args(["+nightly", "miri", "--version"])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            return Ok(());
        }
    }
    bail!(
        "Miri not found on a nightly toolchain. Required for `qedgen verify --miri`.\n\n\
         Install:\n\
         \n\
           rustup toolchain install nightly\n\
           rustup component add miri --toolchain nightly\n\
         \n\
         Then re-run `qedgen verify --miri`. Miri is slow (10-100x native);\n\
         the v2.19 design caches results per (repro_hash, miri_toolchain_hash).\n"
    );
}

/// True if the harness uses z3 (`#[kani::solver(bin = "z3")]`) — shared
/// marker definition for the preflight and tests.
pub(crate) fn harness_uses_z3(harness: &std::path::Path) -> bool {
    std::fs::read_to_string(harness)
        .map(|s| s.contains("bin = \"z3\""))
        .unwrap_or(false)
}

/// If the harness uses z3 (chosen by `pick_kani_solver_for_effect` for
/// wide-type mul/div effects that wedge CBMC's SAT backends), require z3 on
/// PATH — otherwise the run fails with an unhelpful spawn error inside cbmc.
pub fn require_z3_if_kani_harness_needs_it(harness: &std::path::Path) -> Result<()> {
    if !harness_uses_z3(harness) {
        return Ok(());
    }
    if Command::new("z3").arg("--version").output().is_ok() {
        return Ok(());
    }
    bail!(
        "z3 SMT solver not found on PATH.\n\n\
         The Kani harness at {} uses `#[kani::solver(bin = \"z3\")]` for one or\n\
         more wide-type mul/div effect-conformance proofs. CBMC's SAT backends\n\
         (cadical, minisat, kissat) wedge for tens of minutes on 64/128-bit\n\
         bit-vector arithmetic, so qedgen routes those harnesses to z3.\n\n\
         Install z3:\n\
         \n\
           # macOS\n\
           brew install z3\n\
         \n\
           # Debian / Ubuntu\n\
           apt-get install z3\n\
         \n\
         Then re-run `qedgen verify --kani`. To skip z3-backed harnesses, run\n\
         a specific non-z3 one via `cargo kani --harness <name>` in the crate.",
        harness.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_without_z3_marker_is_not_flagged() {
        let dir = std::env::temp_dir().join(format!("qedgen-deps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("no_z3.rs");
        std::fs::write(&path, "#[kani::solver(cadical)]\nfn x() {}\n").unwrap();
        assert!(!harness_uses_z3(&path));
        // No-op when the marker isn't present, regardless of z3 install state.
        assert!(require_z3_if_kani_harness_needs_it(&path).is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn harness_with_z3_marker_is_detected() {
        let dir = std::env::temp_dir().join(format!("qedgen-deps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("uses_z3.rs");
        std::fs::write(
            &path,
            "#[kani::solver(bin = \"z3\")]\nfn verify_wide_mul() {}\n",
        )
        .unwrap();
        assert!(harness_uses_z3(&path));
        // require_'s ok/err depends on the runner's z3 install (both CI
        // states valid); this test pins only the deterministic marker step.
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn nonexistent_harness_is_not_flagged() {
        let path = std::path::Path::new("/nonexistent/path/to/kani.rs");
        assert!(!harness_uses_z3(path));
        assert!(require_z3_if_kani_harness_needs_it(path).is_ok());
    }
}
