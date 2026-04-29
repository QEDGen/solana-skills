// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use quasar_spl::{Token, TokenCpi};
use crate::state::*;
use crate::guards;
use crate::events::*;
use qedgen_macros::qed;
use crate::errors::*;

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub depositor: &'info mut Signer,
    #[account(mut)]
    pub pool: &'info mut Account<PoolAccount>,
    #[account(mut)]
    pub pool_vault: &'info mut Account<Token>,
    #[account(mut)]
    pub depositor_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
}

impl<'info> Deposit<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "deposit", hash = "b49d048d05464eac", spec_hash = "85eff0394c972250")]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64, bumps: &DepositBumps) -> Result<(), ProgramError> {
        guards::deposit(self, amount)?;
        let _ = bumps;
        self.pool.total_deposits = self.pool.total_deposits.checked_add(amount).ok_or(LendingError::MathOverflow)?;
        self.token_program
            .transfer(&*self.depositor_ta, &*self.pool_vault, &*self.depositor, amount)
            .invoke()?;
        emit!(Deposited {
            depositor: *self.depositor.address(),
            amount,
        });
        Ok(())
    }
}
