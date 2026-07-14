//! The protocol permits any request amount, but the ratified product domain
//! caps an accepted request at 10 units. `accept` omits that domain guard.

use anchor_lang::prelude::*;

declare_id!("6bRRkRXokuEQs6sctPhSGjqEnEkPgbda16N1aajwH7bp");

#[program]
pub mod domain_boundary {
    use super::*;

    pub fn accept(_ctx: Context<Accept>, _amount: u64) -> Result<()> {
        // BUG: the product contract requires amount <= 10.
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Accept<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
}
