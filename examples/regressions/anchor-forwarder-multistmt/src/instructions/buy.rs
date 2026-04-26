use anchor_lang::prelude::*;

use crate::Buy;

pub fn handler(_ctx: Context<Buy>, amount: u64) -> Result<()> {
    msg!("buy {} units", amount);
    Ok(())
}
