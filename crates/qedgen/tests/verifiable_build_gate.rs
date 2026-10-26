//! `qedgen build` against real Docker builds (#449). Ignored by default:
//! it needs Docker, `solana-verify` 0.5.2+, network access and about 15 GB
//! of disk, and takes several minutes per program under amd64 emulation.
//!
//! ```bash
//! cargo test -p qedgen-solana-skills --test verifiable_build_gate -- --ignored --nocapture
//! ```
//!
//! Each case writes a minimal program into a temp git repo (with `debug = 1`
//! in `[profile.release]`), generates its `Cargo.lock` (`solana-verify`
//! needs one), commits, and runs `qedgen build`. The programs live here,
//! not under `tests/fixtures/`, because the existing program fixtures are
//! deliberately broken or depend on crates outside the mounted workspace.

use std::path::Path;
use std::process::Command;

const ANCHOR_MANIFEST: &str = r#"[package]
name = "gate_anchor"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "lib"]

[features]
default = []
cpi = ["no-entrypoint"]
no-entrypoint = []
no-idl = []
no-log-ix-name = []
idl-build = ["anchor-lang/idl-build"]
custom-heap = []
custom-panic = []
anchor-debug = []

[dependencies]
anchor-lang = "=0.32.1"

[workspace]

# A fresh lockfile pulls crates on edition 2024, which the cargo in the
# Solana 2.x image cannot parse, so pin the newer image.
[workspace.metadata.cli]
solana = "3.0.1"

[profile.release]
debug = 1
"#;

const ANCHOR_LIB: &str = r#"use anchor_lang::prelude::*;

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

#[program]
pub mod gate_anchor {
    use super::*;

    pub fn ping(ctx: Context<Ping>, value: u64) -> Result<()> {
        require!(value > 0, GateError::Zero);
        msg!("ping from {}", ctx.accounts.signer.key());
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Ping<'info> {
    pub signer: Signer<'info>,
}

#[error_code]
pub enum GateError {
    #[msg("value must be positive")]
    Zero,
}
"#;

/// Pinocchio pulls in no Solana crate that `solana-verify` reads a version
/// from, so the image is pinned with `[workspace.metadata.cli]`.
const PINOCCHIO_MANIFEST: &str = r#"[package]
name = "gate_pinocchio"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "lib"]

[dependencies]
pinocchio = "=0.9.3"

[workspace]

[workspace.metadata.cli]
solana = "3.0.1"

[profile.release]
debug = 1
"#;

const PINOCCHIO_LIB: &str = r#"use pinocchio::{
    account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
    ProgramResult,
};

entrypoint!(process);

pub fn process(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [signer, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !signer.is_signer() || data.is_empty() {
        return Err(ProgramError::InvalidInstructionData);
    }
    Ok(())
}
"#;

fn run(dir: &Path, program: &str, args: &[&str]) {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{program} {args:?}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn prepared_repo(manifest: &str, lib: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("Cargo.toml"), manifest).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), lib).unwrap();
    std::fs::write(d.path().join(".gitignore"), "target/\n").unwrap();
    run(d.path(), "cargo", &["generate-lockfile"]);
    run(d.path(), "git", &["init", "-q"]);
    run(d.path(), "git", &["add", "-A"]);
    run(
        d.path(),
        "git",
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    d
}

fn assert_build(manifest: &str, lib: &str, library: &str) {
    let repo = prepared_repo(manifest, lib);
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(["build", "--program"])
        .arg(repo.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "qedgen build failed:\n{stderr}");

    let dir = repo.path().join(".qed/build").join(library);
    assert!(dir.join(format!("{library}.so")).exists());
    assert!(dir.join(format!("{library}.debug.so")).exists());
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("build.json")).unwrap()).unwrap();
    assert_eq!(record["text_matches"], true);
    assert_eq!(record["arch"], "v3");
    assert_eq!(record["platform_tools"], "v1.57");

    // The hash solana-verify printed (its own line of 64 hex digits) is the
    // recorded program hash.
    let hash = record["program_hash"].as_str().unwrap();
    assert!(
        stdout.lines().any(|line| line.trim() == hash),
        "solana-verify did not print {hash}:\n{stdout}"
    );
    assert!(
        stderr.contains("solana-verify verify-from-repo"),
        "no verify hint:\n{stderr}"
    );
}

#[test]
#[ignore = "needs Docker, solana-verify and network; minutes per build"]
fn anchor_build_keeps_labels() {
    assert_build(ANCHOR_MANIFEST, ANCHOR_LIB, "gate_anchor");
}

#[test]
#[ignore = "needs Docker, solana-verify and network; minutes per build"]
fn pinocchio_build_keeps_labels() {
    assert_build(PINOCCHIO_MANIFEST, PINOCCHIO_LIB, "gate_pinocchio");
}
