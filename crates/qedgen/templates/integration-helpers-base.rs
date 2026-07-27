#[allow(dead_code)]
fn signer_account(address: Pubkey) -> Account {
    Account::new(address, system_program::ID, DEFAULT_WALLET_LAMPORTS, vec![])
}

#[allow(dead_code)]
fn empty_account(address: Pubkey) -> Account {
    Account::new(address, system_program::ID, 0, vec![])
}
