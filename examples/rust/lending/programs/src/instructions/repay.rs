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

#[derive(Accounts)]
pub struct Repay<'info> {
    #[account(mut)]
    pub borrower: &'info mut Signer,
    #[account(mut, seeds = [b"loan", pool, borrower], bump, has_one = borrower)]
    pub loan: &'info mut Account<LoanAccount>,
    #[account(mut)]
    pub pool: &'info mut Account<PoolAccount>,
    #[account(mut)]
    pub pool_vault: &'info mut Account<Token>,
    #[account(mut)]
    pub borrower_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
}

impl<'info> Repay<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "repay", hash = "ab60d83abdd0c21d", spec_hash = "38b9e7598cf201bb")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &RepayBumps) -> Result<(), ProgramError> {
        guards::repay(self)?;
        let _ = bumps;
        let amount: u64 = self.loan.amount.into();
        self.token_program
            .transfer(&*self.borrower_ta, &*self.pool_vault, &*self.borrower, amount)
            .invoke()?;
        self.loan.amount = (0u64).into();
        self.loan.collateral = (0u64).into();
        emit!(Repaid {
            borrower: *self.borrower.address(),
            amount,
        });
        Ok(())
    }
}
