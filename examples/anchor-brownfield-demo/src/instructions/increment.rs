use anchor_lang::prelude::*;

use crate::Increment;

#[error_code]
pub enum CounterError {
    #[msg("increment would overflow")]
    Overflow,
}

pub fn handler(ctx: Context<Increment>, delta: u64) -> Result<()> {
    let counter = &mut ctx.accounts.counter;
    counter.value = counter
        .value
        .checked_add(delta)
        .ok_or(CounterError::Overflow)?;
    Ok(())
}
