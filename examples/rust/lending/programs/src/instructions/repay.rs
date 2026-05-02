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
    #[qed(verified, spec = "../lending.qedspec", handler = "repay", hash = "6c160536ed8b56b1", spec_hash = "4740e8280cb17bad")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &RepayBumps) -> Result<(), ProgramError> {
        guards::repay(self)?;
        let _ = bumps;
        let amount: u64 = self.loan.amount.into();
        self.token_program
            .transfer(&*self.borrower_ta, &*self.pool_vault, &*self.borrower, amount)
            .invoke()?;
        // pool.total_borrows -= amount — see borrow.rs note; v2.16 codegen
        // will emit this from the spec's effect block.
        let new_total: u64 = u64::from(self.pool.total_borrows)
            .checked_sub(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        self.pool.total_borrows = new_total.into();
        self.loan.amount = (0u64).into();
        self.loan.collateral = (0u64).into();
        emit!(Repaid {
            borrower: *self.borrower.address(),
            amount,
        });
        Ok(())
    }
}
