//! sBPF v3 runtime gate for the auditor catalog (#427).
//!
//! Builds the fixtures in `crates/qedgen/tests/fixtures/sbpf-v3-runtime/` as
//! V0 and as v3 from the same source, runs them in Mollusk, and pins what each
//! runtime does. The catalog entries `sbpf_v3_low_address_read`,
//! `sbpf_v3_stack_overrun_no_fault`, and `sbpf_v3_unresolved_syscall` cite this
//! gate as their evidence, so a runtime change that makes them wrong fails
//! here.
//!
//! `#[ignore]`: needs the Solana toolchain. CI runs it with `-- --ignored` in
//! the runtime-journey job.

#[path = "../../qedgen/tests/common/sbf.rs"]
mod sbf;

use mollusk_svm::program::ProgramCache;
use mollusk_svm::result::types::ProgramResult;
use mollusk_svm::Mollusk;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Copy, Debug)]
enum Arch {
    V0,
    V3,
}

/// Copy one fixture program into a fresh tempdir so the build never writes
/// into the repository.
fn stage(program: &str) -> tempfile::TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../qedgen/tests/fixtures/sbpf-v3-runtime")
        .join(program);
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("src")).unwrap();
    for rel in ["Cargo.toml", "src/lib.rs"] {
        std::fs::copy(src.join(rel), tmp.path().join(rel))
            .unwrap_or_else(|e| panic!("copy {program}/{rel}: {e}"));
    }
    tmp
}

fn build(dir: &Path, arch: Arch, out: &Path) -> Output {
    let mut cmd = match arch {
        Arch::V3 => sbf::build_sbf_v3(dir),
        Arch::V0 => {
            let mut c = Command::new("cargo");
            c.args(["build-sbf", "--arch", "v0"]).current_dir(dir);
            c
        }
    };
    cmd.arg("--sbf-out-dir").arg(out);
    cmd.output().expect("spawn cargo build-sbf")
}

/// Build `program` for `arch` and return the deploy directory.
fn build_ok(program: &str, arch: Arch, tmp: &Path) -> PathBuf {
    let out = tmp.join(format!("deploy-{arch:?}"));
    let result = build(tmp, arch, &out);
    assert!(
        result.status.success(),
        "{program} ({arch:?}) failed to build:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let so = out.join(format!("{program}.so"));
    let bytes = std::fs::read(&so).unwrap_or_else(|e| panic!("read {}: {e}", so.display()));
    let flags = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
    let expected = match arch {
        Arch::V0 => 0,
        Arch::V3 => 3,
    };
    assert_eq!(flags, expected, "{program} ({arch:?}) has e_flags {flags}");
    out
}

/// Run `program` once. `simd_0460 = false` turns the feature off, which puts
/// the V0 stack frame gaps back.
fn run(deploy: &Path, program: &str, data: &[u8], simd_0460: bool) -> ProgramResult {
    std::env::set_var("SBF_OUT_DIR", deploy);
    let id = Pubkey::new_unique();
    let mut mollusk = Mollusk::default();
    if !simd_0460 {
        mollusk
            .feature_set
            .deactivate(&agave_feature_set::virtual_address_space_adjustments::id());
        mollusk.program_cache =
            ProgramCache::new(&mollusk.feature_set, &mollusk.compute_budget, false);
    }
    mollusk.add_program(&id, program);
    let ix = Instruction::new_with_bytes(id, data, vec![]);
    mollusk.process_instruction(&ix, &[]).program_result
}

fn is_fault(result: &ProgramResult) -> bool {
    matches!(result, ProgramResult::UnknownError(_))
}

/// `Failure(Custom(code))`, matched through `Debug` so the gate needs no
/// direct `solana-program-error` dependency.
fn is_custom(result: &ProgramResult, code: u32) -> bool {
    format!("{result:?}") == format!("Failure(Custom({code}))")
}

#[test]
#[ignore = "needs the Solana toolchain: builds V0 and v3 programs, runs them in Mollusk"]
fn low_address_read_faults_on_v0_and_succeeds_on_v3() {
    let tmp = stage("low_address");
    let v0 = build_ok("low_address", Arch::V0, tmp.path());
    let v3 = build_ok("low_address", Arch::V3, tmp.path());
    for simd_0460 in [false, true] {
        let r0 = run(&v0, "low_address", &[], simd_0460);
        assert!(
            is_fault(&r0),
            "V0 null read must fault (SIMD-0460 {simd_0460}): {r0:?}"
        );
        let r3 = run(&v3, "low_address", &[], simd_0460);
        assert!(
            matches!(r3, ProgramResult::Success),
            "v3 maps .rodata at address 0, so the null read succeeds (SIMD-0460 {simd_0460}): {r3:?}"
        );
    }
}

#[test]
#[ignore = "needs the Solana toolchain: builds V0 and v3 programs, runs them in Mollusk"]
fn stack_overrun_faults_only_with_v0_frame_gaps() {
    let tmp = stage("stack_overrun");
    let v0 = build_ok("stack_overrun", Arch::V0, tmp.path());
    let v3 = build_ok("stack_overrun", Arch::V3, tmp.path());
    let within = 64u64.to_le_bytes();
    let across = 4160u64.to_le_bytes();

    for (deploy, arch) in [(&v0, "V0"), (&v3, "v3")] {
        for simd_0460 in [false, true] {
            let r = run(deploy, "stack_overrun", &within, simd_0460);
            assert!(
                matches!(r, ProgramResult::Success),
                "{arch} in-bounds write: {r:?}"
            );
        }
    }

    // V0 with frame gaps: the write past the frame hits a gap and faults.
    let r = run(&v0, "stack_overrun", &across, false);
    assert!(is_fault(&r), "V0 with gaps must fault: {r:?}");
    // No gaps: the same write corrupts the callee's live locals (exit 1).
    let r = run(&v0, "stack_overrun", &across, true);
    assert!(
        is_custom(&r, 1),
        "V0 under SIMD-0460 must corrupt silently: {r:?}"
    );
    for simd_0460 in [false, true] {
        let r = run(&v3, "stack_overrun", &across, simd_0460);
        assert!(
            is_custom(&r, 1),
            "v3 has no frame gaps, so the write must corrupt silently (SIMD-0460 {simd_0460}): {r:?}"
        );
    }
}

#[test]
#[ignore = "needs the Solana toolchain: builds V0 and v3 programs"]
fn extern_syscall_fails_to_link_on_v3() {
    let tmp = stage("extern_syscall");
    build_ok("extern_syscall", Arch::V0, tmp.path());
    let out = build(tmp.path(), Arch::V3, &tmp.path().join("deploy-v3"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success()
            && (stderr.contains("undefined symbol") || stdout.contains("undefined symbol")),
        "a hand-declared syscall must fail the v3 link, not build and emit `call -1`:\n{stderr}"
    );
}
