//! A small vault for the label-driven lift (#450). Three instructions, one
//! authority check, one account state struct, and error paths. Accounts are
//! read with the lazy entrypoint, so no loop runs on a success path. Every function is
//! `#[inline(always)]` so the handler logic sits in the entrypoint: the success-path
//! analysis only sees guards in the entry function, and treats calls as opaque.
#![no_std]

use pinocchio::{
    account_info::AccountInfo,
    entrypoint::{lazy::MaybeAccount, InstructionContext},
    lazy_program_entrypoint, no_allocator, nostd_panic_handler,
    program_error::ProgramError,
    pubkey::pubkey_eq,
    ProgramResult,
};

lazy_program_entrypoint!(process);
no_allocator!();
nostd_panic_handler!();

#[repr(C)]
pub struct Vault {
    pub initialized: u8,
    pub _pad: [u8; 7],
    pub authority: [u8; 32],
    pub count: u64,
}

impl Vault {
    pub const LEN: usize = core::mem::size_of::<Vault>();
}

pub const WRONG_AUTHORITY: u32 = 1;
pub const OVERFLOW: u32 = 2;

#[inline(always)]
fn account(maybe: MaybeAccount) -> Result<AccountInfo, ProgramError> {
    match maybe {
        MaybeAccount::Account(account) => Ok(account),
        MaybeAccount::Duplicated(_) => Err(ProgramError::InvalidArgument),
    }
}

#[inline(always)]
fn vault_state(vault: &AccountInfo) -> Result<&mut Vault, ProgramError> {
    if vault.data_len() != Vault::LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    // SAFETY: the length matches and the runtime aligns account data to 8.
    Ok(unsafe { &mut *(vault.borrow_mut_data_unchecked().as_mut_ptr() as *mut Vault) })
}

#[inline(always)]
fn authorized<'a>(vault: &'a AccountInfo, authority: &AccountInfo) -> Result<&'a mut Vault, ProgramError> {
    let state = vault_state(vault)?;
    if state.initialized != 1 {
        return Err(ProgramError::UninitializedAccount);
    }
    if !authority.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // `pubkey_eq` compares four u64 words inline, with no `memcmp` call.
    if !pubkey_eq(authority.key(), &state.authority) {
        return Err(ProgramError::Custom(WRONG_AUTHORITY));
    }
    Ok(state)
}

#[inline(always)]
pub fn process(mut context: InstructionContext) -> ProgramResult {
    let vault = account(context.next_account()?)?;
    let authority = account(context.next_account()?)?;
    let data = context.instruction_data()?;
    let (&discriminator, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    match discriminator {
        0 => initialize(&vault, &authority),
        1 => increment(&vault, &authority, rest),
        2 => reset(&vault, &authority),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[inline(always)]
fn initialize(vault: &AccountInfo, authority: &AccountInfo) -> ProgramResult {
    let state = vault_state(vault)?;
    if state.initialized != 0 {
        return Err(ProgramError::AccountAlreadyInitialized);
    }
    if !authority.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    state.initialized = 1;
    state.authority = *authority.key();
    state.count = 0;
    Ok(())
}

#[inline(always)]
fn increment(vault: &AccountInfo, authority: &AccountInfo, rest: &[u8]) -> ProgramResult {
    let state = authorized(vault, authority)?;
    // Indexing (not `get`) keeps a real bounds-check panic path in the binary.
    let amount = u64::from_le_bytes(rest[..8].try_into().unwrap());
    state.count = state
        .count
        .checked_add(amount)
        .ok_or(ProgramError::Custom(OVERFLOW))?;
    Ok(())
}

#[inline(always)]
fn reset(vault: &AccountInfo, authority: &AccountInfo) -> ProgramResult {
    let state = authorized(vault, authority)?;
    state.count = 0;
    Ok(())
}
