use anchor_lang::prelude::*;

use crate::Settle;

pub fn handler(_ctx: Context<Settle>) -> Result<()> {
    msg!("settled");
    Ok(())
}
