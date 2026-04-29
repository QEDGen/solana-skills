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
pub struct Borrow<'info> {
    #[account(mut)]
    pub borrower: &'info mut Signer,
    #[account(mut, init, payer = borrower, seeds = [b"loan", pool, borrower], bump, has_one = borrower)]
    pub loan: &'info mut Account<LoanAccount>,
    #[account(mut)]
    pub pool: &'info mut Account<PoolAccount>,
    #[account(mut)]
    pub pool_vault: &'info mut Account<Token>,
    #[account(mut)]
    pub borrower_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
    pub system_program: &'info Program<System>,
}

impl<'info> Borrow<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "borrow", hash = "f6629bf8fef984fe", spec_hash = "6a1c2376f61d1679")]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64, collateral: u64, bumps: &BorrowBumps) -> Result<(), ProgramError> {
        guards::borrow(self, amount, collateral)?;
        let _ = bumps;
        self.loan.amount = (amount).into();
        self.loan.collateral = (collateral).into();
        let pool_authority = self.pool.authority;
        let pool_bump = [self.pool.bump];
        let pool_seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool_authority.as_ref()),
            Seed::from(&pool_bump as &[u8]),
        ];
        self.token_program
            .transfer(&*self.pool_vault, &*self.borrower_ta, &*self.pool, amount)
            .invoke_signed(&pool_seeds)?;
        emit!(Borrowed {
            borrower: *self.borrower.address(),
            amount,
        });
        Ok(())
    }
}
