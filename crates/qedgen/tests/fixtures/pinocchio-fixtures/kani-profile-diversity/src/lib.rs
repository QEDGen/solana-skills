#![allow(unexpected_cfgs)]

use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};

#[cfg(kani)]
extern crate kani;
#[cfg(kani)]
mod kani_impl;

pub type Pubkey = [u8; 32];

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey = [8u8; 32];
pub const TOKEN_PROGRAM_ID: Pubkey = [9u8; 32];
pub const VAULT_AUTHORITY_SEED: &[u8] = b"vault-authority";
pub const MISSING_PROFILE: u8 = 250;
pub const SPL_TOKEN_ID: Pubkey = [
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79,
    0xac, 0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff,
    0x00, 0xa9,
];

const TOKEN_MINT_OFF: usize = 0;
const TOKEN_OWNER_OFF: usize = 32;
const TOKEN_AMOUNT_OFF: usize = 64;
const MINT_DECIMALS_OFF: usize = 44;
const CONFIG_MAGIC: &[u8; 8] = b"CFGMAGIC";
const CONFIG_ADMIN_OFF: usize = 8;
const CONFIG_OPERATOR_OFF: usize = 40;
const CONFIG_REBALANCER_OFF: usize = 72;
const CONFIG_MAX_FEE_BPS_OFF: usize = 104;
const CONFIG_PAUSED_OFF: usize = 106;
const CONFIG_LANE_COUNT_OFF: usize = 107;
const CONFIG_MINT_COUNT_OFF: usize = 108;
const CONFIG_ALLOWED_MINT_OFF: usize = 109;
const CONFIG_ALLOWED_MINT_ITEM_LEN: usize = 32;

pub fn next_account_info<'a, I>(iter: &mut I) -> Result<&'a AccountInfo, ProgramError>
where
    I: Iterator<Item = &'a AccountInfo>,
{
    iter.next().ok_or(ProgramError::InvalidAccountData)
}

pub fn require_key(account: &AccountInfo, key: &Pubkey) -> ProgramResult {
    if account.key() == key {
        Ok(())
    } else {
        Err(ProgramError::InvalidAccountData)
    }
}

pub fn require_token_account(
    account: &AccountInfo,
    mint: &Pubkey,
    owner: &Pubkey,
) -> ProgramResult {
    if !account.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = unsafe { account.borrow_data_unchecked() };
    if data.len() < TOKEN_AMOUNT_OFF + 8 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &data[TOKEN_MINT_OFF..TOKEN_MINT_OFF + 32] != mint {
        return Err(ProgramError::InvalidAccountData);
    }
    if &data[TOKEN_OWNER_OFF..TOKEN_OWNER_OFF + 32] != owner {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

pub fn require_token_mint(account: &AccountInfo, mint: &Pubkey) -> ProgramResult {
    if !account.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    let data = unsafe { account.borrow_data_unchecked() };
    if data.len() < TOKEN_AMOUNT_OFF + 8 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &data[TOKEN_MINT_OFF..TOKEN_MINT_OFF + 32] != mint {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

pub fn read_mint_decimals(mint: &AccountInfo) -> Result<u8, ProgramError> {
    let data = unsafe { mint.borrow_data_unchecked() };
    data.get(MINT_DECIMALS_OFF)
        .copied()
        .ok_or(ProgramError::InvalidAccountData)
}

#[cfg(not(kani))]
pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[VAULT_AUTHORITY_SEED, &[lane_id]], program_id)
}

#[cfg(kani)]
pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    let mut key = *program_id;
    key[0] = lane_id;
    key[1..1 + VAULT_AUTHORITY_SEED.len()].copy_from_slice(VAULT_AUTHORITY_SEED);
    (key, 255)
}

#[cfg(not(kani))]
pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

#[cfg(kani)]
pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    let mut key = *authority;
    key[0] = mint[0];
    key[1] = TOKEN_PROGRAM_ID[0];
    (key, 255)
}

pub fn process_instruction(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (tag, data) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;
    match *tag {
        1 => process_move_tokens(program_id, accounts, data),
        2 => process_move_batch(program_id, accounts, data),
        3 => process_touch_config(program_id, accounts, data),
        4 => process_route_ata(program_id, accounts, data),
        5 => process_partial_fallback(program_id, accounts, data),
        6 => process_set_fee(program_id, accounts, data),
        7 => process_set_paused(program_id, accounts, data),
        8 => process_router_swap(program_id, accounts, data),
        9 => process_router_withdraw(program_id, accounts, data),
        10 => process_router_rebalance(program_id, accounts, data),
        11 => process_router_rebalance_pair(program_id, accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

fn process_move_tokens(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [source, destination, authority, mint, token_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    require_token_account(source, mint.key(), authority.key())?;
    require_token_account(destination, mint.key(), authority.key())?;
    require_key(token_program, &SPL_TOKEN_ID)?;
    let _decimals = read_mint_decimals(mint)?;
    debit_credit_token_amounts(source, destination, amount)
}

fn debit_credit_token_amounts(
    source: &AccountInfo,
    destination: &AccountInfo,
    amount: u64,
) -> ProgramResult {
    let source_data = unsafe { source.borrow_mut_data_unchecked() };
    let source_amount = u64::from_le_bytes(
        source_data[TOKEN_AMOUNT_OFF..TOKEN_AMOUNT_OFF + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?,
    );
    let source_post = source_amount
        .checked_sub(amount)
        .ok_or(ProgramError::InvalidAccountData)?;
    source_data[TOKEN_AMOUNT_OFF..TOKEN_AMOUNT_OFF + 8].copy_from_slice(&source_post.to_le_bytes());

    let destination_data = unsafe { destination.borrow_mut_data_unchecked() };
    let destination_amount = u64::from_le_bytes(
        destination_data[TOKEN_AMOUNT_OFF..TOKEN_AMOUNT_OFF + 8]
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?,
    );
    let destination_post = destination_amount
        .checked_add(amount)
        .ok_or(ProgramError::InvalidAccountData)?;
    destination_data[TOKEN_AMOUNT_OFF..TOKEN_AMOUNT_OFF + 8]
        .copy_from_slice(&destination_post.to_le_bytes());
    Ok(())
}

fn process_move_batch(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [batch_state, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let _transfer_count = instruction_data.get(0).copied().unwrap_or(0);
    if !batch_state.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn process_touch_config(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let config = next_account_info(account_info_iter)?;
    let _max_fee_bps = u16::from_le_bytes(
        instruction_data
            .get(0..2)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let data = unsafe { config.borrow_data_unchecked() };
    if !config.is_writable() || data.get(0..8) != Some(b"CFGMAGIC") {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

fn process_route_ata(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [authority, mint, vault, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let lane_id = u8::from_le_bytes(
        instruction_data
            .get(0..1)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let authority_key = derive_vault_authority(program_id, lane_id).0;
    require_key(authority, &authority_key)?;
    require_key(vault, &derive_token_vault(&authority_key, mint.key()).0)?;
    Ok(())
}

fn config_data(config: &AccountInfo) -> Result<&[u8], ProgramError> {
    let data = unsafe { config.borrow_data_unchecked() };
    if data.len() < CONFIG_ALLOWED_MINT_OFF + CONFIG_ALLOWED_MINT_ITEM_LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    if data.get(0..8) != Some(CONFIG_MAGIC) {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(data)
}

fn require_config_key(data: &[u8], offset: usize, account: &AccountInfo) -> ProgramResult {
    if data.get(offset..offset + 32) == Some(account.key().as_ref()) {
        Ok(())
    } else {
        Err(ProgramError::InvalidAccountData)
    }
}

fn require_allowed_mint(data: &[u8], mint: &Pubkey) -> ProgramResult {
    let count = data
        .get(CONFIG_MINT_COUNT_OFF)
        .copied()
        .ok_or(ProgramError::InvalidAccountData)? as usize;
    if count == 0 || count > 4 {
        return Err(ProgramError::InvalidAccountData);
    }
    for index in 0..count {
        let offset = CONFIG_ALLOWED_MINT_OFF + (index * CONFIG_ALLOWED_MINT_ITEM_LEN);
        if data.get(offset..offset + 32) == Some(mint.as_ref()) {
            return Ok(());
        }
    }
    Err(ProgramError::InvalidAccountData)
}

fn config_max_fee_bps(data: &[u8]) -> Result<u16, ProgramError> {
    Ok(u16::from_le_bytes(
        data.get(CONFIG_MAX_FEE_BPS_OFF..CONFIG_MAX_FEE_BPS_OFF + 2)
            .ok_or(ProgramError::InvalidAccountData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?,
    ))
}

fn config_lane_count(data: &[u8]) -> Result<u8, ProgramError> {
    let lane_count = data
        .get(CONFIG_LANE_COUNT_OFF)
        .copied()
        .ok_or(ProgramError::InvalidAccountData)?;
    if lane_count == 0 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(lane_count)
}

fn require_fee_cap(
    amount_in: u64,
    amount_out: u64,
    input_decimals: u8,
    output_decimals: u8,
    max_fee_bps: u16,
) -> ProgramResult {
    if input_decimals == 6 && output_decimals == 6 {
        let _ = max_fee_bps;
        if amount_out < amount_in {
            return Err(ProgramError::InvalidInstructionData);
        }
        return Ok(());
    }

    let input_normalized = normalize_amount(amount_in, input_decimals)?;
    let output_normalized = normalize_amount(amount_out, output_decimals)?;
    let retained_bps = 10000u128
        .checked_sub(max_fee_bps as u128)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let minimum_output = input_normalized
        .checked_mul(retained_bps)
        .ok_or(ProgramError::InvalidInstructionData)?
        / 10000u128;
    if output_normalized < minimum_output {
        return Err(ProgramError::InvalidInstructionData);
    }
    Ok(())
}

fn normalize_amount(amount: u64, decimals: u8) -> Result<u128, ProgramError> {
    if decimals > 18 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let scale = 10u128
        .checked_pow((18u8 - decimals) as u32)
        .ok_or(ProgramError::InvalidInstructionData)?;
    (amount as u128)
        .checked_mul(scale)
        .ok_or(ProgramError::InvalidInstructionData)
}

fn write_config_u16(config: &AccountInfo, offset: usize, value: u16) -> ProgramResult {
    let data = unsafe { config.borrow_mut_data_unchecked() };
    data.get_mut(offset..offset + 2)
        .ok_or(ProgramError::InvalidAccountData)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_config_bool(config: &AccountInfo, offset: usize, value: bool) -> ProgramResult {
    let data = unsafe { config.borrow_mut_data_unchecked() };
    *data.get_mut(offset).ok_or(ProgramError::InvalidAccountData)? = u8::from(value);
    Ok(())
}

fn process_set_fee(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [config, admin, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let new_max_fee_bps = u16::from_le_bytes(
        instruction_data
            .get(0..2)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    if new_max_fee_bps > 10000 {
        return Err(ProgramError::InvalidInstructionData);
    }
    {
        let data = config_data(config)?;
        require_config_key(data, CONFIG_ADMIN_OFF, admin)?;
    }
    write_config_u16(config, CONFIG_MAX_FEE_BPS_OFF, new_max_fee_bps)
}

fn process_set_paused(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [config, admin, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let paused = instruction_data
        .first()
        .copied()
        .ok_or(ProgramError::InvalidInstructionData)?
        != 0;
    {
        let data = config_data(config)?;
        require_config_key(data, CONFIG_ADMIN_OFF, admin)?;
    }
    write_config_bool(config, CONFIG_PAUSED_OFF, paused)
}

fn process_router_swap(
    program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [
        config,
        user_vault,
        user_input,
        user_output,
        vault_input,
        vault_output,
        input_mint,
        output_mint,
        vault_authority,
        operator,
        token_program,
        ..
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let amount_in = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let amount_out = u64::from_le_bytes(
        instruction_data
            .get(8..16)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let min_out = u64::from_le_bytes(
        instruction_data
            .get(16..24)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let max_fee_bps = u16::from_le_bytes(
        instruction_data
            .get(24..26)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let lane_id = *instruction_data
        .get(26)
        .ok_or(ProgramError::InvalidInstructionData)?;
    if amount_in == 0 || amount_out == 0 || amount_out < min_out {
        return Err(ProgramError::InvalidInstructionData);
    }
    {
        let data = config_data(config)?;
        if data[CONFIG_PAUSED_OFF] != 0 || max_fee_bps > config_max_fee_bps(data)? {
            return Err(ProgramError::InvalidAccountData);
        }
        if lane_id >= config_lane_count(data)? {
            return Err(ProgramError::InvalidAccountData);
        }
        require_config_key(data, CONFIG_OPERATOR_OFF, operator)?;
        require_allowed_mint(data, input_mint.key())?;
        require_allowed_mint(data, output_mint.key())?;
    }
    require_key(token_program, &SPL_TOKEN_ID)?;
    let authority_key = derive_vault_authority(program_id, lane_id).0;
    require_key(vault_authority, &authority_key)?;
    let input_decimals = read_mint_decimals(input_mint)?;
    let output_decimals = read_mint_decimals(output_mint)?;
    if input_decimals != 6 || output_decimals != 6 {
        return Err(ProgramError::InvalidInstructionData);
    }
    require_fee_cap(
        amount_in,
        amount_out,
        input_decimals,
        output_decimals,
        max_fee_bps,
    )?;
    require_token_account(user_input, input_mint.key(), user_vault.key())?;
    require_token_account(user_output, output_mint.key(), user_vault.key())?;
    require_token_account(vault_input, input_mint.key(), &authority_key)?;
    require_token_account(vault_output, output_mint.key(), &authority_key)?;
    debit_credit_token_amounts(user_input, vault_input, amount_in)?;
    debit_credit_token_amounts(vault_output, user_output, amount_out)
}

fn process_router_withdraw(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [config, admin, vault_source, destination, mint, vault_authority, token_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    if amount == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    {
        let data = config_data(config)?;
        require_config_key(data, CONFIG_ADMIN_OFF, admin)?;
        require_allowed_mint(data, mint.key())?;
    }
    require_key(token_program, &SPL_TOKEN_ID)?;
    require_token_account(vault_source, mint.key(), vault_authority.key())?;
    require_token_mint(destination, mint.key())?;
    debit_credit_token_amounts(vault_source, destination, amount)
}

fn process_router_rebalance(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [
        config,
        rebalancer,
        token_program,
        mint,
        source_authority,
        source_inventory,
        destination_inventory,
        ..
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let from_lane_id = *instruction_data
        .get(8)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let to_lane_id = *instruction_data
        .get(9)
        .ok_or(ProgramError::InvalidInstructionData)?;
    if amount == 0 || from_lane_id == to_lane_id {
        return Err(ProgramError::InvalidInstructionData);
    }
    {
        let data = config_data(config)?;
        require_config_key(data, CONFIG_REBALANCER_OFF, rebalancer)?;
        require_allowed_mint(data, mint.key())?;
    }
    require_key(token_program, &SPL_TOKEN_ID)?;
    require_token_account(source_inventory, mint.key(), source_authority.key())?;
    require_token_mint(destination_inventory, mint.key())?;
    debit_credit_token_amounts(source_inventory, destination_inventory, amount)
}

fn process_router_rebalance_pair(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let [
        config,
        rebalancer,
        token_program,
        mint,
        source_authority_0,
        source_inventory_0,
        destination_inventory_0,
        source_authority_1,
        source_inventory_1,
        destination_inventory_1,
        ..
    ] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let amount_0 = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let from_lane_id_0 = *instruction_data
        .get(8)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let to_lane_id_0 = *instruction_data
        .get(9)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let amount_1 = u64::from_le_bytes(
        instruction_data
            .get(10..18)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let from_lane_id_1 = *instruction_data
        .get(18)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let to_lane_id_1 = *instruction_data
        .get(19)
        .ok_or(ProgramError::InvalidInstructionData)?;
    if amount_0 == 0
        || amount_1 == 0
        || from_lane_id_0 == to_lane_id_0
        || from_lane_id_1 == to_lane_id_1
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    {
        let data = config_data(config)?;
        require_config_key(data, CONFIG_REBALANCER_OFF, rebalancer)?;
        require_allowed_mint(data, mint.key())?;
    }
    require_key(token_program, &SPL_TOKEN_ID)?;
    require_token_account(source_inventory_0, mint.key(), source_authority_0.key())?;
    require_token_mint(destination_inventory_0, mint.key())?;
    debit_credit_token_amounts(source_inventory_0, destination_inventory_0, amount_0)?;
    require_token_account(source_inventory_1, mint.key(), source_authority_1.key())?;
    require_token_mint(destination_inventory_1, mint.key())?;
    debit_credit_token_amounts(source_inventory_1, destination_inventory_1, amount_1)
}

fn process_partial_fallback(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> ProgramResult {
    let [scratch, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if scratch.is_writable() {
        Ok(())
    } else {
        Err(ProgramError::InvalidAccountData)
    }
}
