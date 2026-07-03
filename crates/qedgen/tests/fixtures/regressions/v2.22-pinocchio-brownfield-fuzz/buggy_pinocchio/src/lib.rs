//! Deliberately buggy Pinocchio program used by the v2.22 Slice 3
//! brownfield Crucible fuzz regression fixture. See ../README.md.
//!
//! Three handlers, each demonstrating a different intrinsic crash
//! Crucible's brownfield harness surfaces without a `.qedspec`:
//!
//!   * `process_run`    — divide-by-zero panic.
//!   * `process_maybe`  — `Option::unwrap` on `None`.
//!   * `process_drain`  — sweeps every lamport from `source` into
//!                        `target` with no authority check; triggers
//!                        the v2.21 §S1.2 lamport-inflation guard when
//!                        the fuzzer picks `target` as a tracked
//!                        signer.
//!
//! The maintainer-authored Codama IDL at `../idl.json` is what qedgen
//! actually consumes — v2.22 gates Pinocchio brownfield fuzz on a
//! Codama / Anchor 0.30 IDL being present (handler / account / arg
//! inference from source is intentionally out of scope; see
//! crucible_brownfield.rs).

#![no_std]

use pinocchio::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
};

pinocchio::entrypoint!(dispatch);

/// 1-byte dispatcher matching the Codama IDL's `discriminator[0]`
/// assignment: 0 → run, 1 → maybe, 2 → drain.
pub fn dispatch(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (disc, rest) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    match disc {
        0 => process_run(accounts, rest),
        1 => process_maybe(accounts, rest),
        2 => process_drain(accounts, rest),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

/// Divides a constant by zero — fires `runtime_panic` on iteration 1.
pub fn process_run(_accounts: &[AccountInfo], _instruction_data: &[u8]) -> ProgramResult {
    let zero: u32 = 0;
    let _ = 100u32 / zero;
    Ok(())
}

/// Unwraps a `None` — fires `runtime_panic` on iteration 1.
pub fn process_maybe(_accounts: &[AccountInfo], _instruction_data: &[u8]) -> ProgramResult {
    let value: Option<u32> = None;
    let _ = value.unwrap();
    Ok(())
}

/// Sweeps every lamport from `source` into `target` with no authority
/// check. Crucible surfaces this as `assert_no_wallet_inflation`: `target`
/// is a tracked wallet regardless of whether it signs, so the credit fires
/// the guard even though the drain names no signer.
pub fn process_drain(accounts: &[AccountInfo], _instruction_data: &[u8]) -> ProgramResult {
    let [source, target, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let credit = unsafe { *source.borrow_lamports_unchecked() };
    unsafe {
        *source.borrow_mut_lamports_unchecked() = 0;
        *target.borrow_mut_lamports_unchecked() += credit;
    }
    Ok(())
}
