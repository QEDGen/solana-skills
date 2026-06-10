//! Slice 8 M2 — conservation harness for `process_transfer`.
//!
//! Reference shape for the codegen emitter
//! (`crates/qedgen/src/kani_impl.rs`).
//!
//! ## Why we bypass `pinocchio::entrypoint::deserialize`
//!
//! First attempt used the wire-format approach from
//! `crates/qedgen/tests/fixtures/pinocchio-fixtures/_harness/account.rs:78-127` —
//! `Box::leak`ing a ~42KB buffer and calling `deserialize`. Kani made
//! it through but the SAT instance ballooned to 18.5M variables /
//! 270M clauses (218s on SSA conversion, didn't finish solving
//! within 4 min). The cost driver: pointer-indirection reads through
//! the leaked buffer + memcmp unrolling in pinocchio's owner-check
//! loops.
//!
//! Workaround: build `Account`-layout structs **directly on the
//! stack** (using a local `#[repr(C)]` type with the same field order
//! as `pinocchio::account_info::Account`), then transmute a `*mut
//! AccountLayout` to `AccountInfo`. This is sound because
//! `AccountInfo` is `#[repr(C)] struct AccountInfo { raw: *mut
//! Account }` — a single-field pointer wrapper — so the transmute
//! reinterprets one raw pointer as another, provided the pointee
//! layout matches. Layout is asserted at compile time via
//! `const _: () = assert!(core::mem::size_of::<...>() == 88);`.
//!
//! This drops the symbolic surface dramatically: no leaked buffer, no
//! deserialize loop, just per-account stack storage with concrete
//! flags and (mostly) concrete data. The symbolic surface is just
//! the per-account `amount` field.

#[cfg(kani)]
extern crate alloc;

use core::mem::ManuallyDrop;
use pinocchio::account_info::AccountInfo;

use crate::transfer::process_transfer;

/// Layout-mirror of `pinocchio::account_info::Account` (pinocchio 0.8.4
/// `src/account_info.rs:39-85`). Fields in the same order with the
/// same types and `#[repr(C)]`. Drift between this and pinocchio's
/// `Account` causes immediate UB on the first field access; the
/// compile-time size assertion below catches the most common form
/// (field added/removed).
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

/// One stack-allocated account: 88-byte header followed by its data
/// region. `#[repr(C)]` keeps the layout contiguous so
/// `borrow_*_data_unchecked` (which reads past the header at runtime)
/// sees the data we wrote.
#[repr(C, align(8))]
struct StackAccount<const DATA_LEN: usize> {
    hdr: AccountLayout,
    data: [u8; DATA_LEN],
}

/// `TokenAccount` layout offsets inside the account's data region
/// (per pinocchio-token 0.3.0 `src/state/token.rs:11-49`).
const TOKEN_OWNER_OFF: usize = 32;
const TOKEN_AMOUNT_OFF: usize = 64;
const TOKEN_STATE_OFF: usize = 108;
const TOKEN_DATA_LEN: usize = 165;

/// `pinocchio_token::ID` — the SPL Token program ID. Token accounts'
/// account-header owner must match this for
/// `TokenAccount::from_account_info_unchecked` to accept the layout.
const SPL_TOKEN_PROGRAM_ID: [u8; 32] = [
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79,
    0xac, 0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff,
    0x00, 0xa9,
];

/// `AccountState::Initialized = 1`.
const STATE_INITIALIZED: u8 = 1;

/// Pinocchio tracks borrow availability with set bits. At instruction entry,
/// all lamport/data mutable and immutable borrow slots are available.
const BORROW_STATE_CLEAR: u8 = 0xff;

/// Build a stack-resident token account. `key` and `owner_in_data`
/// are concrete; `amount` is the per-test parameter (symbolic via
/// `kani::any()` in the harness body).
fn build_token_account(
    key: [u8; 32],
    is_writable: bool,
    is_signer: bool,
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
            owner: SPL_TOKEN_PROGRAM_ID, // header-owner = SPL Token program
            lamports: 0,
            data_len: TOKEN_DATA_LEN as u64,
        },
        data: [0u8; TOKEN_DATA_LEN],
    };
    acct.data[TOKEN_OWNER_OFF..TOKEN_OWNER_OFF + 32].copy_from_slice(&owner_in_data);
    acct.data[TOKEN_AMOUNT_OFF..TOKEN_AMOUNT_OFF + 8].copy_from_slice(&amount.to_le_bytes());
    acct.data[TOKEN_STATE_OFF] = STATE_INITIALIZED;
    acct
}

/// Build a non-token account (mint slot, authority slot). No data
/// region needed (handler only reads `is_signer` / `key`).
fn build_minimal_account(key: [u8; 32], is_signer: bool) -> StackAccount<0> {
    StackAccount {
        hdr: AccountLayout {
            borrow_state: BORROW_STATE_CLEAR,
            is_signer: is_signer as u8,
            is_writable: 0,
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

/// Transmute a `*mut StackAccount<N>` to an `AccountInfo`.
///
/// SAFETY: `AccountInfo` is `#[repr(C)] struct AccountInfo { raw:
/// *mut Account }` — a single-field pointer wrapper around
/// `pinocchio::account_info::Account` (88 bytes, layout asserted
/// above). `StackAccount<N>::hdr` has identical layout. The transmute
/// reinterprets one raw pointer as another; provided the caller
/// keeps `stack` alive for the lifetime of the returned
/// `AccountInfo`, no use-after-free occurs.
unsafe fn account_info_from_stack<const N: usize>(stack: &mut StackAccount<N>) -> AccountInfo {
    let hdr_ptr: *mut AccountLayout = &mut stack.hdr;
    core::mem::transmute::<*mut AccountLayout, AccountInfo>(hdr_ptr)
}

/// Read the `amount` from a stack token account's data region.
fn read_amount<const N: usize>(stack: &StackAccount<N>) -> u64 {
    let off = TOKEN_AMOUNT_OFF;
    u64::from_le_bytes(stack.data[off..off + 8].try_into().unwrap())
}

/// Conservation: `pre.src.amount + pre.dst.amount == post.src.amount +
/// post.dst.amount`. Expected to fail with a Kani counterexample
/// because `transfer.rs` does unchecked `+` on the destination token
/// amount — Kani finds the input pair where the sum wraps past u64::MAX.
///
/// `#[kani::unwind(34)]` bounds the memcmp loops pinocchio's owner
/// check unrolls (32 bytes for Pubkey comparison, +2 slack).
#[cfg(kani)]
#[kani::proof]
#[kani::unwind(34)]
fn verify_transfer_preserves_token_conservation() {
    let authority_key: [u8; 32] = [7u8; 32];

    // Symbolic amounts. The conservation check is over the pair.
    let src_amount: u64 = kani::any();
    let dst_amount: u64 = kani::any();
    let amount: u64 = kani::any();
    // Avoid the early-return paths: amount > 0 and amount <= src_amount
    // (otherwise the source subtraction would panic and we'd be testing
    // a different bug class).
    kani::assume(amount > 0);
    kani::assume(amount <= src_amount);

    let mut src = build_token_account([1u8; 32], true, false, authority_key, src_amount);
    let mut mint = build_minimal_account([2u8; 32], false);
    let mut dst = build_token_account([3u8; 32], true, false, [9u8; 32], dst_amount);
    let mut auth = build_minimal_account(authority_key, true);

    // Build the AccountInfo array via stack-pointer transmute. The
    // ManuallyDrop wrap stops Rust from dropping the `AccountInfo`
    // copies (they alias the stack accounts).
    let accounts: [ManuallyDrop<AccountInfo>; 4] = unsafe {
        [
            ManuallyDrop::new(account_info_from_stack(&mut src)),
            ManuallyDrop::new(account_info_from_stack(&mut mint)),
            ManuallyDrop::new(account_info_from_stack(&mut dst)),
            ManuallyDrop::new(account_info_from_stack(&mut auth)),
        ]
    };
    // Erase the ManuallyDrop wrapper for the handler call. Safe
    // because the inner AccountInfo is a #[repr(transparent)]-style
    // pointer wrapper that has no Drop impl that would matter.
    let accounts_for_call: &[AccountInfo] = unsafe {
        core::slice::from_raw_parts(
            &accounts as *const _ as *const AccountInfo,
            4,
        )
    };

    let mut instruction_data = [0u8; 8];
    instruction_data.copy_from_slice(&amount.to_le_bytes());

    let pre_src = read_amount(&src);
    let pre_dst = read_amount(&dst);

    let result = process_transfer(accounts_for_call, &instruction_data);

    if result.is_ok() {
        let post_src = read_amount(&src);
        let post_dst = read_amount(&dst);

        let pre_total = (pre_src as u128) + (pre_dst as u128);
        let post_total = (post_src as u128) + (post_dst as u128);
        assert!(pre_total == post_total, "conservation violated");
    }
}
