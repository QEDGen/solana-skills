use anchor_lang::prelude::*;

/// Lite version: just a signer.
#[derive(Accounts)]
pub struct Shared<'info> {
    pub user: Signer<'info>,
}
