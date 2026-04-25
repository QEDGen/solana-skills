use anchor_lang::prelude::*;

use crate::{LiquidUnstake, StakeError};

impl<'info> LiquidUnstake<'info> {
    pub fn process(&mut self, shares: u64) -> Result<()> {
        require!(
            self.state.total_shares >= shares,
            StakeError::InsufficientStake
        );
        self.state.total_shares -= shares;
        Ok(())
    }
}
