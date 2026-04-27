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
pub struct Repay<'info> {
    #[account(mut)]
    pub borrower: &'info mut Signer,
    #[account(mut, seeds = [b"loan", pool, borrower], bump)]
    pub loan: &'info mut Account<LoanAccount>,
    #[account(mut)]
    pub pool: &'info mut Account<PoolAccount>,
    #[account(mut)]
    pub pool_vault: &'info mut Account<Token>,
    #[account(mut)]
    pub borrower_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<System>,
}

impl<'info> Repay<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "repay", hash = "5ffb3b8d1270c3f4", spec_hash = "38b9e7598cf201bb")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &RepayBumps) -> Result<(), ProgramError> {
        guards::repay(self)?;
        // Spec effect (needs fill): amount set 0
        // Spec effect (needs fill): collateral set 0
        // Spec: emit!(Repaid)
        // Spec transfer: borrower_ta -> pool_vault amount=amount
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
