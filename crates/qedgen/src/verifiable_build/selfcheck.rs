//! Program hash and the debug-build self-check.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

use crate::sbpf_elf;

/// The program hash as `solana-verify` computes it (`get_binary_hash`):
/// SHA-256 of the bytes with trailing zero bytes stripped, as lowercase hex.
/// On-chain program data can carry zero padding after the ELF, so a plain
/// file hash would not match what `solana-verify` prints.
pub fn program_hash(bytes: &[u8]) -> String {
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    file_sha256(&bytes[..end])
}

/// Plain SHA-256 of the bytes, as lowercase hex (what `shasum -a 256` prints).
pub fn file_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Check that the unstripped binary describes the deploy binary: the same
/// `.text`, debug info present, and the requested sBPF version. Labels taken
/// from a binary that fails this would not describe the deployed bytes.
pub fn self_check(deploy: &[u8], debug: &[u8], expected_sbpf: u32) -> Result<()> {
    let deploy_text = sbpf_elf::section_bytes(deploy, ".text")?;
    let debug_text = sbpf_elf::section_bytes(debug, ".text")?;
    if deploy_text.is_none() || deploy_text != debug_text {
        bail!(
            "the unstripped binary's .text differs from the deploy .so; \
             the labels would not describe the deployed bytes"
        );
    }
    for section in [".debug_info", ".debug_line"] {
        if sbpf_elf::section_bytes(debug, section)?.is_none() {
            bail!(
                "the unstripped binary has no debug info (.debug_info/.debug_line); \
                 set debug in [profile.release]"
            );
        }
    }
    let got = sbpf_elf::sbpf_version_of(deploy)?;
    if got != expected_sbpf {
        bail!("built sBPF v{got}, expected v{expected_sbpf}");
    }
    Ok(())
}

/// The sBPF version that `cargo build-sbf --arch <arch>` produces.
pub fn arch_to_sbpf_version(arch: &str) -> Result<u32> {
    match arch {
        "v0" => Ok(0),
        "v1" => Ok(1),
        "v2" => Ok(2),
        "v3" => Ok(3),
        other => bail!("unknown sBPF arch {other:?}; expected v0, v1, v2 or v3"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sbpf-elf")
            .join(name)
    }

    #[test]
    fn program_hash_strips_trailing_zeros() {
        assert_eq!(program_hash(b"abc\0\0\0"), program_hash(b"abc"));
        assert_eq!(
            program_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(program_hash(b"abc\0"), file_sha256(b"abc\0"));
    }

    #[test]
    fn program_hash_of_all_zeros_is_empty_hash() {
        assert_eq!(
            program_hash(&[0u8; 16]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn self_check_passes_on_matching_pair() {
        let d = std::fs::read(fixture("anchor-v3.so")).unwrap();
        let g = std::fs::read(fixture("anchor-v3-debug.so")).unwrap();
        self_check(&d, &g, 3).unwrap();
    }

    #[test]
    fn self_check_rejects_text_mismatch() {
        let d = std::fs::read(fixture("counter-v0.so")).unwrap();
        let g = std::fs::read(fixture("anchor-v3-debug.so")).unwrap();
        let e = self_check(&d, &g, 0).unwrap_err().to_string();
        assert!(e.contains(".text"), "{e}");
    }

    #[test]
    fn self_check_rejects_missing_debug_info() {
        let d = std::fs::read(fixture("anchor-v3.so")).unwrap();
        let e = self_check(&d, &d, 3).unwrap_err().to_string();
        assert!(e.contains("debug"), "{e}");
    }

    #[test]
    fn self_check_rejects_wrong_sbpf_version() {
        let d = std::fs::read(fixture("anchor-v3.so")).unwrap();
        let g = std::fs::read(fixture("anchor-v3-debug.so")).unwrap();
        let e = self_check(&d, &g, 0).unwrap_err().to_string();
        assert!(e.contains("expected v0"), "{e}");
    }

    #[test]
    fn arch_maps_to_sbpf_version() {
        assert_eq!(arch_to_sbpf_version("v0").unwrap(), 0);
        assert_eq!(arch_to_sbpf_version("v3").unwrap(), 3);
        assert!(arch_to_sbpf_version("v9").is_err());
    }
}
