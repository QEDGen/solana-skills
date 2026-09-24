// sBPF version of a built program, read from its ELF header (#426).
//
// SIMD-0500 (planned for Agave 4.4) rejects deploys and upgrades of programs
// older than sBPF v3. The version is the ELF `e_flags` field. The reader
// checks only the fixed ELF64 header and the bounds of what it points to (the
// program-header table, each segment, the section-header table), so no ELF
// parsing dependency is needed. A bare or truncated header is not a pass.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// First sBPF version SIMD-0500 still accepts for deploys and upgrades.
pub const SBPF_V3: u32 = 3;

const ELF64_HEADER_LEN: usize = 64;
const ELF64_PHDR_LEN: u64 = 56;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_BPF: u16 = 247;
const EM_SBPF: u16 = 263;

/// Read the sBPF version (`e_flags`) of the program at `path`. A missing
/// file, a non-ELF file, a truncated file, or an ELF for another machine is
/// an error, never a version.
pub fn read_sbpf_version(path: &Path) -> Result<u32> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    sbpf_version(&bytes).with_context(|| format!("reading {}", path.display()))
}

fn sbpf_version(elf: &[u8]) -> Result<u32> {
    if elf.len() < ELF64_HEADER_LEN {
        bail!("too short to be an ELF program ({ELF64_HEADER_LEN}-byte header)");
    }
    if &elf[..4] != b"\x7fELF" {
        bail!("not an ELF file (bad magic)");
    }
    if elf[4] != ELFCLASS64 || elf[5] != ELFDATA2LSB {
        bail!("not a 64-bit little-endian ELF, so not an sBPF program");
    }
    let machine = u16::from_le_bytes([elf[18], elf[19]]);
    if machine != EM_BPF && machine != EM_SBPF {
        bail!("ELF machine {machine} is not sBPF (expected {EM_BPF} or {EM_SBPF})");
    }

    let len = elf.len() as u64;
    let in_file = |what: &str, offset: u64, size: u64| -> Result<()> {
        match offset.checked_add(size) {
            Some(end) if end <= len => Ok(()),
            _ => bail!("ELF is truncated: its {what} ends past the file end ({len} bytes)"),
        }
    };

    let (phoff, phentsize, phnum) = (u64_at(elf, 32), u16_at(elf, 54), u16_at(elf, 56));
    if phnum == 0 {
        bail!("ELF has no program headers, so it is not a loadable program");
    }
    if phentsize < ELF64_PHDR_LEN {
        bail!("ELF program header entries are {phentsize} bytes, expected {ELF64_PHDR_LEN}");
    }
    in_file("program header table", phoff, phentsize * phnum)?;
    for i in 0..phnum {
        // In bounds: the whole table was checked above.
        let at = (phoff + i * phentsize) as usize;
        in_file("segment", u64_at(elf, at + 8), u64_at(elf, at + 32))?;
    }
    let (shoff, shentsize, shnum) = (u64_at(elf, 40), u16_at(elf, 58), u16_at(elf, 60));
    in_file("section header table", shoff, shentsize * shnum)?;

    Ok(u32::from_le_bytes([elf[48], elf[49], elf[50], elf[51]]))
}

fn u16_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8-byte field"))
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
        let err = sbpf_version(&header).unwrap_err();
        assert!(format!("{err:#}").contains("not sBPF"), "{err:#}");
    }

    /// A valid header alone, or a file cut after the header, must not pass.
    #[test]
    fn truncated_program_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        for fixture_name in ["counter-v0.so", "counter-v3.so"] {
            let full = std::fs::read(fixture(fixture_name)).unwrap();
            for len in [ELF64_HEADER_LEN, full.len() - 1] {
                let cut = tmp.path().join(format!("{fixture_name}-{len}"));
                std::fs::write(&cut, &full[..len]).unwrap();
                let err = read_sbpf_version(&cut).unwrap_err();
                assert!(
                    format!("{err:#}").contains("truncated"),
                    "{fixture_name} cut to {len}: {err:#}"
                );
            }
        }
    }
}
