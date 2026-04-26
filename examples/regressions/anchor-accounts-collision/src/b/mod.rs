use anchor_lang::prelude::*;

/// Heavy version: vault + authority.
#[derive(Accounts)]
pub struct Shared<'info> {
    #[account(mut)]
    pub vault: Account<'info, Vault>,
    pub authority: Signer<'info>,
}

#[account]
pub struct Vault {
    pub balance: u64,
}
