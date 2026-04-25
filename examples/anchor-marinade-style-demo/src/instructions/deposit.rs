use anchor_lang::prelude::*;

use crate::{Deposit, StakeError};

impl<'info> Deposit<'info> {
    pub fn process(&mut self, lamports: u64) -> Result<()> {
        require!(lamports >= 1_000, StakeError::BelowMinimum);
        self.state.total_lamports = self
            .state
            .total_lamports
            .checked_add(lamports)
            .ok_or(StakeError::BelowMinimum)?;
        Ok(())
    }
}
