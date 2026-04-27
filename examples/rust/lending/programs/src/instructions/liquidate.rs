// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use quasar_spl::{Token, Mint};
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;
use crate::events::*;
use crate::errors::*;

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
    pub token_program: &'info Program<System>,
}

impl<'info> Liquidate<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "liquidate", hash = "9a62f58d23c76d3a", spec_hash = "51eeccc10cee5384")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &LiquidateBumps) -> Result<(), ProgramError> {
        guards::liquidate(self)?;
        self.loan.amount = (0).into();
        // Spec: emit!(LoanLiquidated)
        // Spec transfer: pool_vault -> liquidator_ta amount=amount
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
