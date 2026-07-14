extern crate alloc;

use core::mem::ManuallyDrop;
use pinocchio::account_info::AccountInfo;

/// Layout-mirror of `pinocchio::account_info::Account` (pinocchio 0.8.x).
/// Drift causes immediate UB on first field access; the size assertion
/// catches the common add/remove-field form.
#[repr(C)]
struct AccountLayout {
    borrow_state: u8,
    is_signer: u8,
    is_writable: u8,
    executable: u8,
    original_data_len: u32,
    key: [u8; 32],
    owner: [u8; 32],
    lamports: u64,
    data_len: u64,
}
const _: () = assert!(core::mem::size_of::<AccountLayout>() == 88);

/// 88-byte header followed contiguously by the account's data region.
#[repr(C, align(8))]
struct StackAccount<const DATA_LEN: usize> {
    hdr: AccountLayout,
    data: [u8; DATA_LEN],
}

// SPL Token `TokenAccount` data-region offsets (pinocchio-token 0.3.0).
const TOKEN_MINT_OFF: usize = 0;
const TOKEN_OWNER_OFF: usize = 32;
const TOKEN_AMOUNT_OFF: usize = 64;
const TOKEN_STATE_OFF: usize = 108;
const TOKEN_DATA_LEN: usize = 165;
const MINT_DECIMALS_OFF: usize = 44;
const MINT_STATE_OFF: usize = 45;
const MINT_DATA_LEN: usize = 82;

/// SPL Token program ID — the `from_account_info` owner check target.
const SPL_TOKEN_PROGRAM_ID: [u8; 32] = [
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
    0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
];
const STATE_INITIALIZED: u8 = 1;
/// Pinocchio tracks borrow availability with set bits. At instruction entry,
/// all lamport/data mutable and immutable borrow slots are available.
const BORROW_STATE_CLEAR: u8 = 0xff;

/// Build a stack-resident SPL Token account. `amount` is the field a
/// harness wires up as `kani::any()`.
fn build_token_account(
    key: [u8; 32],
    is_writable: bool,
    is_signer: bool,
    mint_in_data: [u8; 32],
    owner_in_data: [u8; 32],
    amount: u64,
) -> StackAccount<TOKEN_DATA_LEN> {
    let mut acct = StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner: SPL_TOKEN_PROGRAM_ID,
            lamports: 0,
            data_len: TOKEN_DATA_LEN as u64,
        },
        data: [0u8; TOKEN_DATA_LEN],
    };
    write_fixed_32(&mut acct.data, TOKEN_MINT_OFF, mint_in_data);
    write_fixed_32(&mut acct.data, TOKEN_OWNER_OFF, owner_in_data);
    write_fixed_u64(&mut acct.data, TOKEN_AMOUNT_OFF, amount);
    acct.data[TOKEN_STATE_OFF] = STATE_INITIALIZED;
    acct
}

/// Build a stack-resident SPL Token mint. This is enough for
/// `Mint::from_account_info(..)?.decimals()` and initialized-state checks.
fn build_mint_account(
    key: [u8; 32],
    is_signer: bool,
    is_writable: bool,
    decimals: u8,
) -> StackAccount<MINT_DATA_LEN> {
    let mut acct = StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner: SPL_TOKEN_PROGRAM_ID,
            lamports: 0,
            data_len: MINT_DATA_LEN as u64,
        },
        data: [0u8; MINT_DATA_LEN],
    };
    acct.data[MINT_DECIMALS_OFF] = decimals;
    acct.data[MINT_STATE_OFF] = STATE_INITIALIZED;
    acct
}

/// Build a non-token account (mint / authority / signer slot). No data
/// region — the handler only reads `is_signer` / `key`.
fn build_minimal_account(key: [u8; 32], is_signer: bool, is_writable: bool) -> StackAccount<0> {
    StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner: [0u8; 32],
            lamports: 0,
            data_len: 0,
        },
        data: [],
    }
}

/// Build a non-token account with an ABI-profiled data region. The data is
/// symbolic in generated harnesses; ABI layout facts only fix the byte length.
fn build_data_account<const DATA_LEN: usize>(
    key: [u8; 32],
    owner: [u8; 32],
    is_signer: bool,
    is_writable: bool,
    data: [u8; DATA_LEN],
) -> StackAccount<DATA_LEN> {
    StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: is_writable as u8,
            executable: 0,
            original_data_len: 0,
            key,
            owner,
            lamports: 0,
            data_len: DATA_LEN as u64,
        },
        data,
    }
}

/// Transmute a `*mut StackAccount<N>::hdr` to `AccountInfo`.
///
/// SAFETY: `AccountInfo` is `#[repr(C)] struct { raw: *mut Account }` —
/// a single-field pointer wrapper. `StackAccount<N>::hdr` mirrors
/// `Account`'s layout (asserted above). The caller must keep `stack`
/// alive for the lifetime of the returned `AccountInfo`.
unsafe fn account_info_from_stack<const N: usize>(stack: &mut StackAccount<N>) -> AccountInfo {
    let hdr_ptr: *mut AccountLayout = &mut stack.hdr;
    core::mem::transmute::<*mut AccountLayout, AccountInfo>(hdr_ptr)
}

/// Read the `amount` from a stack token account's data region.
fn read_token_amount<const N: usize>(stack: &StackAccount<N>) -> u64 {
    u64::from_le_bytes([
        stack.data[TOKEN_AMOUNT_OFF],
        stack.data[TOKEN_AMOUNT_OFF + 1],
        stack.data[TOKEN_AMOUNT_OFF + 2],
        stack.data[TOKEN_AMOUNT_OFF + 3],
        stack.data[TOKEN_AMOUNT_OFF + 4],
        stack.data[TOKEN_AMOUNT_OFF + 5],
        stack.data[TOKEN_AMOUNT_OFF + 6],
        stack.data[TOKEN_AMOUNT_OFF + 7],
    ])
}

fn read_state_pubkey<const N: usize>(stack: &StackAccount<N>, offset: usize) -> [u8; 32] {
    [
        stack.data[offset],
        stack.data[offset + 1],
        stack.data[offset + 2],
        stack.data[offset + 3],
        stack.data[offset + 4],
        stack.data[offset + 5],
        stack.data[offset + 6],
        stack.data[offset + 7],
        stack.data[offset + 8],
        stack.data[offset + 9],
        stack.data[offset + 10],
        stack.data[offset + 11],
        stack.data[offset + 12],
        stack.data[offset + 13],
        stack.data[offset + 14],
        stack.data[offset + 15],
        stack.data[offset + 16],
        stack.data[offset + 17],
        stack.data[offset + 18],
        stack.data[offset + 19],
        stack.data[offset + 20],
        stack.data[offset + 21],
        stack.data[offset + 22],
        stack.data[offset + 23],
        stack.data[offset + 24],
        stack.data[offset + 25],
        stack.data[offset + 26],
        stack.data[offset + 27],
        stack.data[offset + 28],
        stack.data[offset + 29],
        stack.data[offset + 30],
        stack.data[offset + 31],
    ]
}

fn write_state_pubkey<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: [u8; 32]) {
    write_fixed_32(&mut stack.data, offset, value);
}

fn read_state_bool<const N: usize>(stack: &StackAccount<N>, offset: usize) -> bool {
    stack.data[offset] != 0
}

fn write_state_bool<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: bool) {
    stack.data[offset] = u8::from(value);
}

fn read_state_u8<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u8 {
    stack.data[offset]
}

fn write_state_u8<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u8) {
    stack.data[offset] = value;
}

fn read_state_u16<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u16 {
    u16::from_le_bytes([stack.data[offset], stack.data[offset + 1]])
}

fn write_state_u16<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u16) {
    let bytes = value.to_le_bytes();
    stack.data[offset] = bytes[0];
    stack.data[offset + 1] = bytes[1];
}

fn read_state_u64<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u64 {
    u64::from_le_bytes([
        stack.data[offset],
        stack.data[offset + 1],
        stack.data[offset + 2],
        stack.data[offset + 3],
        stack.data[offset + 4],
        stack.data[offset + 5],
        stack.data[offset + 6],
        stack.data[offset + 7],
    ])
}

fn write_state_u64<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u64) {
    write_fixed_u64(&mut stack.data, offset, value);
}

fn read_state_u128<const N: usize>(stack: &StackAccount<N>, offset: usize) -> u128 {
    u128::from_le_bytes([
        stack.data[offset],
        stack.data[offset + 1],
        stack.data[offset + 2],
        stack.data[offset + 3],
        stack.data[offset + 4],
        stack.data[offset + 5],
        stack.data[offset + 6],
        stack.data[offset + 7],
        stack.data[offset + 8],
        stack.data[offset + 9],
        stack.data[offset + 10],
        stack.data[offset + 11],
        stack.data[offset + 12],
        stack.data[offset + 13],
        stack.data[offset + 14],
        stack.data[offset + 15],
    ])
}

fn write_state_u128<const N: usize>(stack: &mut StackAccount<N>, offset: usize, value: u128) {
    let bytes = value.to_le_bytes();
    stack.data[offset] = bytes[0];
    stack.data[offset + 1] = bytes[1];
    stack.data[offset + 2] = bytes[2];
    stack.data[offset + 3] = bytes[3];
    stack.data[offset + 4] = bytes[4];
    stack.data[offset + 5] = bytes[5];
    stack.data[offset + 6] = bytes[6];
    stack.data[offset + 7] = bytes[7];
    stack.data[offset + 8] = bytes[8];
    stack.data[offset + 9] = bytes[9];
    stack.data[offset + 10] = bytes[10];
    stack.data[offset + 11] = bytes[11];
    stack.data[offset + 12] = bytes[12];
    stack.data[offset + 13] = bytes[13];
    stack.data[offset + 14] = bytes[14];
    stack.data[offset + 15] = bytes[15];
}

fn write_fixed_32<const N: usize>(data: &mut [u8; N], offset: usize, value: [u8; 32]) {
    data[offset] = value[0];
    data[offset + 1] = value[1];
    data[offset + 2] = value[2];
    data[offset + 3] = value[3];
    data[offset + 4] = value[4];
    data[offset + 5] = value[5];
    data[offset + 6] = value[6];
    data[offset + 7] = value[7];
    data[offset + 8] = value[8];
    data[offset + 9] = value[9];
    data[offset + 10] = value[10];
    data[offset + 11] = value[11];
    data[offset + 12] = value[12];
    data[offset + 13] = value[13];
    data[offset + 14] = value[14];
    data[offset + 15] = value[15];
    data[offset + 16] = value[16];
    data[offset + 17] = value[17];
    data[offset + 18] = value[18];
    data[offset + 19] = value[19];
    data[offset + 20] = value[20];
    data[offset + 21] = value[21];
    data[offset + 22] = value[22];
    data[offset + 23] = value[23];
    data[offset + 24] = value[24];
    data[offset + 25] = value[25];
    data[offset + 26] = value[26];
    data[offset + 27] = value[27];
    data[offset + 28] = value[28];
    data[offset + 29] = value[29];
    data[offset + 30] = value[30];
    data[offset + 31] = value[31];
}

fn write_fixed_u64<const N: usize>(data: &mut [u8; N], offset: usize, value: u64) {
    let bytes = value.to_le_bytes();
    data[offset] = bytes[0];
    data[offset + 1] = bytes[1];
    data[offset + 2] = bytes[2];
    data[offset + 3] = bytes[3];
    data[offset + 4] = bytes[4];
    data[offset + 5] = bytes[5];
    data[offset + 6] = bytes[6];
    data[offset + 7] = bytes[7];
}

fn normalized_fee_decimal_scale(decimals: u64) -> u128 {
    match decimals {
        0 => 1_000_000_000_000_000_000,
        1 => 100_000_000_000_000_000,
        2 => 10_000_000_000_000_000,
        3 => 1_000_000_000_000_000,
        4 => 100_000_000_000_000,
        5 => 10_000_000_000_000,
        6 => 1_000_000_000_000,
        7 => 100_000_000_000,
        8 => 10_000_000_000,
        9 => 1_000_000_000,
        10 => 100_000_000,
        11 => 10_000_000,
        12 => 1_000_000,
        13 => 100_000,
        14 => 10_000,
        15 => 1_000,
        16 => 100,
        17 => 10,
        18 => 1,
        _ => 0,
    }
}
