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
    #[qed(verified, spec = "lending.qedspec", handler = "repay", hash = "89e86c91065c832f", spec_hash = "4740e8280cb17bad")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &RepayBumps) -> Result<(), ProgramError> {
        guards::repay(self)?;
        let _ = bumps;
        self.loan.pool.total_borrows = self.loan.pool.total_borrows.checked_sub(amount).ok_or(LendingError::MathOverflow)?;
        self.loan.amount = (0).into();
        self.loan.collateral = (0).into();
        // Spec: emit!(Repaid)
        // Spec transfer: borrower_ta -> pool_vault amount=amount
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
