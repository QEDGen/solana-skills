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
pub struct Liquidate<'info> {
    #[account(mut)]
    pub liquidator: &'info mut Signer,
    #[account(mut)]
    pub loan: &'info mut Account<LoanAccount>,
    #[account(mut)]
    pub pool: &'info mut Account<PoolAccount>,
    #[account(mut)]
    pub pool_vault: &'info mut Account<Token>,
    #[account(mut)]
    pub liquidator_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
}

impl<'info> Liquidate<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "liquidate", hash = "040997ce5a073924", spec_hash = "cc08c2279c20f07d")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &LiquidateBumps) -> Result<(), ProgramError> {
        guards::liquidate(self)?;
        let _ = bumps;
        let amount: u64 = self.loan.amount.into();
        let pool_authority = self.pool.authority;
        let pool_bump = [self.pool.bump];
        let pool_seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool_authority.as_ref()),
            Seed::from(&pool_bump as &[u8]),
        ];
        self.token_program
            .transfer(&*self.pool_vault, &*self.liquidator_ta, &*self.pool, amount)
            .invoke_signed(&pool_seeds)?;
        // pool.total_borrows -= amount — see borrow.rs note; v2.16 codegen
        // will emit this from the spec's effect block.
        let new_total: u64 = u64::from(self.pool.total_borrows)
            .checked_sub(amount)
            .ok_or(ProgramError::ArithmeticOverflow)?;
        self.pool.total_borrows = new_total.into();
        self.loan.amount = (0u64).into();
        emit!(LoanLiquidated {
            borrower: self.loan.borrower,
            amount,
        });
        Ok(())
    }
}
