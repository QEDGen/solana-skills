#![allow(dead_code, unused_imports)]

include!("../templates/kani-impl-pinocchio-scaffold.rs");

#[test]
fn stack_account_supports_pinocchios_checked_borrow_contract() {
    let key = [1u8; 32];
    let owner = [2u8; 32];
    let mut stack = build_data_account(key, owner, true, true, [3u8, 4, 5]);
    stack.hdr.lamports = 99;

    let account = unsafe { account_info_from_stack(&mut stack) };

    assert_eq!(account.key(), &key);
    assert_eq!(unsafe { account.owner() }, &owner);
    assert!(account.is_signer());
    assert!(account.is_writable());
    assert_eq!(account.lamports(), 99);
    assert_eq!(account.data_len(), 3);

    {
        let lamports = account
            .try_borrow_lamports()
            .expect("a fresh scaffold account must allow a checked lamports borrow");
        assert_eq!(*lamports, 99);
        assert!(account.try_borrow_mut_lamports().is_err());
    }

    {
        let mut lamports = account
            .try_borrow_mut_lamports()
            .expect("dropping the shared borrow must restore lamports availability");
        *lamports = 101;
    }
    assert_eq!(account.lamports(), 101);

    {
        let data = account
            .try_borrow_data()
            .expect("a fresh scaffold account must allow a checked data borrow");
        assert_eq!(&*data, &[3, 4, 5]);
        assert!(account.try_borrow_mut_data().is_err());
    }

    {
        let mut data = account
            .try_borrow_mut_data()
            .expect("dropping the shared borrow must restore borrow availability");
        data[2] = 8;
    }
    assert_eq!(stack.data, [3, 4, 8]);
}

#[test]
fn token_and_mint_builders_are_accepted_by_pinocchio_token() {
    use pinocchio_token::state::{Mint, TokenAccount};

    let mint_key = [4u8; 32];
    let authority_key = [5u8; 32];
    let mut token_stack = build_token_account([6u8; 32], true, false, mint_key, authority_key, 42);
    let mut mint_stack = build_mint_account(mint_key, false, false, 9);

    let token_info = unsafe { account_info_from_stack(&mut token_stack) };
    let mint_info = unsafe { account_info_from_stack(&mut mint_stack) };

    let token = TokenAccount::from_account_info(&token_info)
        .expect("generated token layout must pass checked parsing");
    assert_eq!(token.mint(), &mint_key);
    assert_eq!(token.owner(), &authority_key);
    assert_eq!(token.amount(), 42);

    let mint = Mint::from_account_info(&mint_info)
        .expect("generated mint layout must pass checked parsing");
    assert_eq!(mint.decimals(), 9);
    assert!(mint.is_initialized());
}

#[test]
fn zero_length_account_exposes_an_empty_data_slice() {
    let mut stack = build_minimal_account([7u8; 32], false, false);
    let account = unsafe { account_info_from_stack(&mut stack) };

    let data = account
        .try_borrow_data()
        .expect("zero-length account data is still borrowable");
    assert!(data.is_empty());
}

#[test]
fn stack_account_supports_the_exact_permitted_realloc_boundary() {
    const ORIGINAL_LEN: usize = 3;
    const MAX_GROWTH: usize = 10_240;

    assert!(
        core::mem::size_of::<StackAccount<ORIGINAL_LEN>>()
            >= core::mem::size_of::<AccountLayout>() + ORIGINAL_LEN + MAX_GROWTH,
        "the backing allocation must reserve Pinocchio's permitted growth region"
    );

    let mut stack = build_data_account([1u8; 32], [2u8; 32], false, true, [7u8, 8, 9]);
    let account = unsafe { account_info_from_stack(&mut stack) };

    account
        .realloc(ORIGINAL_LEN + MAX_GROWTH, true)
        .expect("the exact maximum permitted growth must remain in bounds");
    assert_eq!(account.data_len(), ORIGINAL_LEN + MAX_GROWTH);
    {
        let data = account.try_borrow_data().expect("grown data is borrowable");
        assert_eq!(&data[..ORIGINAL_LEN], &[7, 8, 9]);
        assert_eq!(data[ORIGINAL_LEN + MAX_GROWTH - 1], 0);
    }

    assert!(account
        .realloc(ORIGINAL_LEN + MAX_GROWTH + 1, false)
        .is_err());
}
