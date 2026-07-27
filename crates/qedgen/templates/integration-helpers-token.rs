#[allow(dead_code)]
fn mint_account(address: Pubkey, authority: Pubkey) -> Account {
    let mint = SplMint {
        mint_authority: COption::Some(authority),
        supply: 1_000_000_000,
        decimals: 9,
        is_initialized: true,
        freeze_authority: COption::None,
    };
    let mut data = vec![0; SplMint::LEN];
    SplMint::pack(mint, &mut data).expect("encode SPL mint fixture");
    Account::new(address, SPL_TOKEN_PROGRAM_ID, 2_000_000, data)
}

#[allow(dead_code)]
fn token_account(address: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let token = SplTokenAccount {
        mint,
        owner,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut data = vec![0; SplTokenAccount::LEN];
    SplTokenAccount::pack(token, &mut data).expect("encode SPL token fixture");
    Account::new(address, SPL_TOKEN_PROGRAM_ID, 2_000_000, data)
}
