// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use quasar_spl::Token;
use crate::state::*;
use crate::guards;
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
    #[qed(verified, spec = "lending.qedspec", handler = "deposit", hash = "1ccb43c96197d294", spec_hash = "935397caad48aa67")]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64, bumps: &DepositBumps) -> Result<(), ProgramError> {
        guards::deposit(self, amount)?;
        let _ = bumps;
        self.pool.total_deposits = self.pool.total_deposits.checked_add(amount).ok_or(LendingError::MathOverflow)?;
        // Spec: emit!(Deposited)
        // Spec transfer: depositor_ta -> pool_vault amount=amount
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
