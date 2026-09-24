// sBPF version of a built program, read from its ELF header (#426).
//
// SIMD-0500 (planned for Agave 4.4) rejects deploys and upgrades of programs
// older than sBPF v3. The version is the ELF `e_flags` field. Only the fixed
// 64-byte ELF64 header is read, so no ELF parsing dependency is needed.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;

/// First sBPF version SIMD-0500 still accepts for deploys and upgrades.
pub const SBPF_V3: u32 = 3;

const ELF64_HEADER_LEN: usize = 64;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_BPF: u16 = 247;
const EM_SBPF: u16 = 263;

/// Read the sBPF version (`e_flags`) of the program at `path`. A missing
/// file, a non-ELF file, or an ELF for another machine is an error, never a
/// version.
pub fn read_sbpf_version(path: &Path) -> Result<u32> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut header = [0u8; ELF64_HEADER_LEN];
    file.read_exact(&mut header).with_context(|| {
        format!(
            "{} is too short to be an ELF program ({ELF64_HEADER_LEN}-byte header)",
            path.display()
        )
    })?;
    sbpf_version_from_header(&header).with_context(|| format!("reading {}", path.display()))
}

fn sbpf_version_from_header(header: &[u8; ELF64_HEADER_LEN]) -> Result<u32> {
    if &header[..4] != b"\x7fELF" {
        bail!("not an ELF file (bad magic)");
    }
    if header[4] != ELFCLASS64 || header[5] != ELFDATA2LSB {
        bail!("not a 64-bit little-endian ELF, so not an sBPF program");
    }
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if machine != EM_BPF && machine != EM_SBPF {
        bail!("ELF machine {machine} is not sBPF (expected {EM_BPF} or {EM_SBPF})");
    }
    Ok(u32::from_le_bytes([
        header[48], header[49], header[50], header[51],
    ]))
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
    fn reads_v0_and_v3_fixtures() {
        assert_eq!(read_sbpf_version(&fixture("counter-v0.so")).unwrap(), 0);
        assert_eq!(read_sbpf_version(&fixture("counter-v3.so")).unwrap(), 3);
    }

    #[test]
    fn missing_file_is_an_error() {
        let err = read_sbpf_version(Path::new("/does/not/exist.so")).unwrap_err();
        assert!(format!("{err:#}").contains("reading"), "{err:#}");
    }

    #[test]
    fn non_elf_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let short = tmp.path().join("short.so");
        std::fs::write(&short, b"not an elf").unwrap();
        let err = read_sbpf_version(&short).unwrap_err();
        assert!(format!("{err:#}").contains("too short"), "{err:#}");

        let text = tmp.path().join("text.so");
        std::fs::write(&text, [b'x'; 64]).unwrap();
        let err = read_sbpf_version(&text).unwrap_err();
        assert!(format!("{err:#}").contains("bad magic"), "{err:#}");
    }

    #[test]
    fn non_sbpf_elf_is_an_error() {
        let mut header = [0u8; ELF64_HEADER_LEN];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = ELFCLASS64;
        header[5] = ELFDATA2LSB;
        header[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
        let err = sbpf_version_from_header(&header).unwrap_err();
        assert!(format!("{err:#}").contains("not sBPF"), "{err:#}");
    }
}
