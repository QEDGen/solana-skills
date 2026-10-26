// sBPF version of a built program, read from its ELF header (#426).
//
// SIMD-0500 (planned for Agave 4.4) rejects deploys and upgrades of programs
// older than sBPF v3. The version is the ELF `e_flags` field. The reader
// reads only the fixed ELF64 header and the program-header table, and checks
// that the tables and every segment fit in the file. No ELF parsing
// dependency is needed, and a bare or truncated header is not a pass.

use anyhow::{bail, Context, Result};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// First sBPF version SIMD-0500 still accepts for deploys and upgrades.
pub const SBPF_V3: u32 = 3;

const ELF64_HEADER_LEN: usize = 64;
const ELF64_PHDR_LEN: u64 = 56;
const ELF64_SHDR_LEN: u64 = 64;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EM_BPF: u16 = 247;
const EM_SBPF: u16 = 263;

/// Read the sBPF version (`e_flags`) of the program at `path`. A missing
/// file, a non-ELF file, a truncated file, or an ELF for another machine is
/// an error, never a version. Reads only the ELF header and the
/// program-header table, never the segment contents.
pub fn read_sbpf_version(path: &Path) -> Result<u32> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    sbpf_version(&mut file).with_context(|| format!("reading {}", path.display()))
}

fn sbpf_version(file: &mut std::fs::File) -> Result<u32> {
    let len = file.metadata()?.len();
    let mut elf = [0u8; ELF64_HEADER_LEN];
    if len < ELF64_HEADER_LEN as u64 {
        bail!("too short to be an ELF program ({ELF64_HEADER_LEN}-byte header)");
    }
    file.read_exact(&mut elf)?;
    let version = check_header(&elf)?;

    let in_file = |what: &str, offset: u64, size: u64| -> Result<()> {
        match offset.checked_add(size) {
            Some(end) if end <= len => Ok(()),
            _ => bail!("ELF is truncated: its {what} ends past the file end ({len} bytes)"),
        }
    };

    let (phoff, phentsize, phnum) = (u64_at(&elf, 32), u16_at(&elf, 54), u16_at(&elf, 56));
    if phnum == 0 {
        bail!("ELF has no program headers, so it is not a loadable program");
    }
    if phentsize < ELF64_PHDR_LEN {
        bail!("ELF program header entries are {phentsize} bytes, expected {ELF64_PHDR_LEN}");
    }
    // At most 65535 * 65535 bytes by the u16 fields, and in the file.
    in_file("program header table", phoff, phentsize * phnum)?;
    let mut table = vec![0u8; (phentsize * phnum) as usize];
    file.seek(SeekFrom::Start(phoff))?;
    file.read_exact(&mut table)?;
    for i in 0..phnum {
        let at = (i * phentsize) as usize;
        in_file("segment", u64_at(&table, at + 8), u64_at(&table, at + 32))?;
    }
    let (shoff, shentsize, shnum) = (u64_at(&elf, 40), u16_at(&elf, 58), u16_at(&elf, 60));
    in_file("section header table", shoff, shentsize * shnum)?;

    Ok(version)
}

/// The same check as [`read_sbpf_version`], on a program already in memory.
pub fn sbpf_version_of(elf: &[u8]) -> Result<u32> {
    let header = header_of(elf)?;
    let version = check_header(header)?;
    let (phoff, phentsize, phnum) = (u64_at(elf, 32), u16_at(elf, 54), u16_at(elf, 56));
    if phnum == 0 {
        bail!("ELF has no program headers, so it is not a loadable program");
    }
    if phentsize < ELF64_PHDR_LEN {
        bail!("ELF program header entries are {phentsize} bytes, expected {ELF64_PHDR_LEN}");
    }
    let table = slice_in(elf, "program header table", phoff, phentsize * phnum)?;
    for i in 0..phnum {
        let at = (i * phentsize) as usize;
        slice_in(
            elf,
            "segment",
            u64_at(table, at + 8),
            u64_at(table, at + 32),
        )?;
    }
    Ok(version)
}

/// The bytes of the section called `name`, or `None` when the ELF has no
/// such section. A malformed or truncated section table is an error. The
/// `qedgen build` self-check uses this to compare `.text` and to find the
/// debug sections.
pub fn section_bytes<'a>(elf: &'a [u8], name: &str) -> Result<Option<&'a [u8]>> {
    let header = header_of(elf)?;
    check_header(header)?;
    let (shoff, shentsize, shnum) = (u64_at(elf, 40), u16_at(elf, 58), u16_at(elf, 60));
    if shnum == 0 {
        return Ok(None);
    }
    if shentsize < ELF64_SHDR_LEN {
        bail!("ELF section header entries are {shentsize} bytes, expected {ELF64_SHDR_LEN}");
    }
    let table = slice_in(elf, "section header table", shoff, shentsize * shnum)?;
    let entry = |i: u64| &table[(i * shentsize) as usize..];
    let strndx = u16_at(elf, 62);
    if strndx >= shnum {
        bail!("ELF section name table index {strndx} is out of range ({shnum} sections)");
    }
    let names = entry(strndx);
    let names = slice_in(
        elf,
        "section name table",
        u64_at(names, 24),
        u64_at(names, 32),
    )?;
    for i in 0..shnum {
        let sh = entry(i);
        let name_at = u32::from_le_bytes(sh[..4].try_into().expect("4-byte field")) as usize;
        let Some(rest) = names.get(name_at..) else {
            bail!("ELF section name offset {name_at} is past the name table");
        };
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        if &rest[..end] == name.as_bytes() {
            return slice_in(elf, "section", u64_at(sh, 24), u64_at(sh, 32)).map(Some);
        }
    }
    Ok(None)
}

fn header_of(elf: &[u8]) -> Result<&[u8; ELF64_HEADER_LEN]> {
    elf.get(..ELF64_HEADER_LEN)
        .and_then(|h| h.try_into().ok())
        .ok_or_else(|| {
            anyhow::anyhow!("too short to be an ELF program ({ELF64_HEADER_LEN}-byte header)")
        })
}

fn slice_in<'a>(elf: &'a [u8], what: &str, offset: u64, size: u64) -> Result<&'a [u8]> {
    match offset.checked_add(size) {
        Some(end) if end <= elf.len() as u64 => Ok(&elf[offset as usize..end as usize]),
        _ => bail!(
            "ELF is truncated: its {what} ends past the file end ({} bytes)",
            elf.len()
        ),
    }
}

/// Check the fixed ELF64 header and return its `e_flags`.
fn check_header(elf: &[u8; ELF64_HEADER_LEN]) -> Result<u32> {
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
    Ok(u32::from_le_bytes([elf[48], elf[49], elf[50], elf[51]]))
}

/// Warn, once per path, when an execution lane is about to run a program
/// built older than sBPF v3. `cargo build-sbf` still defaults to V0, so a
/// plain build silently produces a program that SIMD-0500 blocks from
/// deploys and upgrades, and any result is about that build. Silent when the
/// file is missing or unreadable: the lane reports that itself.
pub fn warn_if_pre_v3(so: &Path, lane: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<HashSet<std::path::PathBuf>>> = OnceLock::new();

    let Ok(version) = read_sbpf_version(so) else {
        return;
    };
    if version >= SBPF_V3 {
        return;
    }
    let first = WARNED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|mut seen| seen.insert(so.to_path_buf()))
        .unwrap_or(true);
    if first {
        eprintln!("warning: {}", pre_v3_message(so, version, lane));
    }
}

fn pre_v3_message(so: &Path, version: u32, lane: &str) -> String {
    format!(
        "{lane} is running {} built as sBPF v{version}, not v3. SIMD-0500 blocks deploying \
         or upgrading it, so these results describe a build that cannot ship. Rebuild with \
         `cargo build-sbf --arch v3` (or `anchor build -- --arch v3`); `cargo build-sbf` \
         defaults to V0.",
        so.display()
    )
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
        let err = check_header(&header).unwrap_err();
        assert!(format!("{err:#}").contains("not sBPF"), "{err:#}");
    }

    #[test]
    fn pre_v3_message_names_the_fix() {
        let msg = pre_v3_message(&fixture("counter-v0.so"), 0, "probe --fuzz");
        assert!(
            msg.contains("sBPF v0") && msg.contains("--arch v3"),
            "{msg}"
        );
        // A v3 build and a missing file stay silent (no panic, no message).
        warn_if_pre_v3(&fixture("counter-v3.so"), "test");
        warn_if_pre_v3(Path::new("/does/not/exist.so"), "test");
    }

    #[test]
    fn text_section_matches_between_stripped_and_debug_fixture() {
        let a = std::fs::read(fixture("anchor-v3.so")).unwrap();
        let b = std::fs::read(fixture("anchor-v3-debug.so")).unwrap();
        let ta = section_bytes(&a, ".text").unwrap().expect("has .text");
        let tb = section_bytes(&b, ".text").unwrap().expect("has .text");
        assert!(!ta.is_empty());
        assert_eq!(ta, tb);
    }

    #[test]
    fn debug_sections_present_only_in_debug_fixture() {
        let a = std::fs::read(fixture("anchor-v3.so")).unwrap();
        let b = std::fs::read(fixture("anchor-v3-debug.so")).unwrap();
        assert!(section_bytes(&a, ".debug_info").unwrap().is_none());
        assert!(section_bytes(&b, ".debug_info").unwrap().is_some());
        assert!(section_bytes(&b, ".debug_line").unwrap().is_some());
    }

    #[test]
    fn v0_and_v3_text_differ() {
        let a = std::fs::read(fixture("counter-v0.so")).unwrap();
        let b = std::fs::read(fixture("anchor-v3.so")).unwrap();
        assert_ne!(
            section_bytes(&a, ".text").unwrap(),
            section_bytes(&b, ".text").unwrap()
        );
    }

    #[test]
    fn section_bytes_rejects_truncated_elf() {
        let a = std::fs::read(fixture("anchor-v3.so")).unwrap();
        assert!(section_bytes(&a[..80], ".text").is_err());
        assert!(section_bytes(&a[..a.len() - 1], ".text").is_err());
    }

    #[test]
    fn sbpf_version_of_bytes_matches_file_reader() {
        let a = std::fs::read(fixture("anchor-v3.so")).unwrap();
        assert_eq!(sbpf_version_of(&a).unwrap(), 3);
        let b = std::fs::read(fixture("counter-v0.so")).unwrap();
        assert_eq!(sbpf_version_of(&b).unwrap(), 0);
        assert!(sbpf_version_of(&a[..ELF64_HEADER_LEN]).is_err());
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
