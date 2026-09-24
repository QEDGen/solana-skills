//! Build Solana programs as sBPF v3 in the ignored runtime gates (#423).
//!
//! sBPF v3 is built with `cargo build-sbf --arch v3` and needs
//! cargo-build-sbf 4.2.0+ and platform-tools v1.56+. Anza warns that
//! `--arch v3` on platform-tools older than v1.53 produces incompatible
//! bytecode, so the flag is only passed after the version check passes.
//!
//! This file has no dependencies beyond `std`. The qedgen gates load it
//! through `common`; the sandbox journey loads it with `#[path]`.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

/// Minimum cargo-build-sbf for sBPF v3.
pub const MIN_CARGO_BUILD_SBF: (u32, u32, u32) = (4, 2, 0);
/// Minimum platform-tools for sBPF v3.
pub const MIN_PLATFORM_TOOLS: (u32, u32) = (1, 56);

/// Versions reported by `cargo build-sbf --version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbfToolchain {
    pub cargo_build_sbf: (u32, u32, u32),
    pub platform_tools: (u32, u32),
}

/// Parse `cargo build-sbf --version` output, for example:
///
/// ```text
/// cargo-build-sbf 4.3.0
/// platform-tools v1.57
/// rustc 1.89.0
/// ```
pub fn parse_version_output(text: &str) -> Option<SbfToolchain> {
    let mut cargo = None;
    let mut tools = None;
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match (words.next(), words.next()) {
            (Some(name), Some(v)) if name.ends_with("cargo-build-sbf") => {
                let mut n = v.split('.').map(|p| p.parse::<u32>().ok());
                cargo = Some((n.next()??, n.next()??, n.next().flatten().unwrap_or(0)));
            }
            (Some("platform-tools"), Some(v)) => {
                let mut n = v
                    .trim_start_matches('v')
                    .split('.')
                    .map(|p| p.parse::<u32>().ok());
                tools = Some((n.next()??, n.next()??));
            }
            _ => {}
        }
    }
    Some(SbfToolchain {
        cargo_build_sbf: cargo?,
        platform_tools: tools?,
    })
}

/// `Ok` when the toolchain can build sBPF v3, else a message naming the
/// minimum versions.
pub fn check_v3_support(t: SbfToolchain) -> Result<(), String> {
    if t.cargo_build_sbf >= MIN_CARGO_BUILD_SBF && t.platform_tools >= MIN_PLATFORM_TOOLS {
        return Ok(());
    }
    let (c0, c1, c2) = t.cargo_build_sbf;
    let (p0, p1) = t.platform_tools;
    let (m0, m1, m2) = MIN_CARGO_BUILD_SBF;
    let (n0, n1) = MIN_PLATFORM_TOOLS;
    Err(format!(
        "sBPF v3 needs cargo-build-sbf {m0}.{m1}.{m2}+ and platform-tools v{n0}.{n1}+; \
         found cargo-build-sbf {c0}.{c1}.{c2} and platform-tools v{p0}.{p1}. \
         Upgrade the Solana toolchain (do not pass --arch v3 to platform-tools older than v1.53)."
    ))
}

/// A `cargo build-sbf --arch v3` command for `dir`. Panics with the minimum
/// versions when the installed toolchain cannot build v3.
pub fn build_sbf_v3(dir: &Path) -> Command {
    let out = Command::new("cargo")
        .args(["build-sbf", "--version"])
        .output()
        .expect("run `cargo build-sbf --version` (is the Solana toolchain installed?)");
    let text = String::from_utf8_lossy(&out.stdout);
    let toolchain = parse_version_output(&text)
        .unwrap_or_else(|| panic!("could not parse `cargo build-sbf --version` output:\n{text}"));
    if let Err(msg) = check_v3_support(toolchain) {
        panic!("{msg}");
    }
    let mut c = Command::new("cargo");
    c.args(["build-sbf", "--arch", "v3"]).current_dir(dir);
    c
}

/// Panic unless the ELF at `so` is sBPF v3 (`e_flags == 3`, a u32 at
/// offset 48).
pub fn assert_sbpf_v3(so: &Path) {
    let bytes = std::fs::read(so).unwrap_or_else(|e| panic!("read {}: {e}", so.display()));
    assert!(
        bytes.len() >= 52 && bytes.starts_with(b"\x7fELF"),
        "{} is not an ELF file",
        so.display()
    );
    let flags = u32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]]);
    assert_eq!(
        flags,
        3,
        "{} is sBPF e_flags={flags}, expected v3",
        so.display()
    );
}

/// Panic unless `deploy_dir` holds at least one `.so` and every one is v3.
pub fn assert_deploy_dir_v3(deploy_dir: &Path) {
    let sos: Vec<_> = std::fs::read_dir(deploy_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", deploy_dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "so"))
        .collect();
    assert!(!sos.is_empty(), "no .so in {}", deploy_dir.display());
    for so in sos {
        assert_sbpf_v3(&so);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_build_sbf_version_output() {
        let t = parse_version_output("cargo-build-sbf 4.3.0\nplatform-tools v1.57\nrustc 1.89.0\n")
            .unwrap();
        assert_eq!(t.cargo_build_sbf, (4, 3, 0));
        assert_eq!(t.platform_tools, (1, 57));
        assert!(check_v3_support(t).is_ok());

        // The older binary name printed a `solana-` prefix.
        let old = parse_version_output(
            "solana-cargo-build-sbf 3.1.11\nplatform-tools v1.52\nrustc 1.89.0\n",
        )
        .unwrap();
        assert_eq!(old.cargo_build_sbf, (3, 1, 11));
        let msg = check_v3_support(old).unwrap_err();
        assert!(msg.contains("4.2.0") && msg.contains("v1.56"), "{msg}");
    }

    #[test]
    fn rejects_new_cargo_with_old_platform_tools() {
        let t = SbfToolchain {
            cargo_build_sbf: (4, 3, 0),
            platform_tools: (1, 52),
        };
        assert!(check_v3_support(t).is_err());
    }

    #[test]
    fn unparseable_output_is_none() {
        assert_eq!(parse_version_output("nonsense"), None);
    }
}
