use anchor_lang::prelude::*;

use crate::{Counter, Initialize};

pub fn handler(ctx: Context<Initialize>, start: u64) -> Result<()> {
    let counter = &mut ctx.accounts.counter;
    counter.value = start;
    counter.authority = ctx.accounts.authority.key();
    Ok(())
}
