// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;
use crate::events::*;
use crate::errors::*;

#[derive(Accounts)]
pub struct Liquidate {
    #[account(mut)]
    pub liquidator: Signer,
    #[account(mut, seeds = [b"loan", pool.key().as_ref(), loan.borrower.as_ref()], bump)]
    pub loan: UncheckedAccount,
    #[account(mut, seeds = [b"pool", pool.authority.as_ref()], bump)]
    pub pool: UncheckedAccount,
    #[account(mut, token::authority = pool)]
    pub pool_vault: Account<Token>,
    #[account(mut)]
    pub liquidator_ta: Account<Token>,
    pub token_program: Program<System>,
}

impl Liquidate {
    #[qed(verified, spec = "../lending.qedspec", handler = "liquidate", hash = "fdcb8dadabdb3a67", spec_hash = "b3815843622e25b9")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &LiquidateBumps) -> Result<(), ProgramError> {
        guards::liquidate(self)?;
        // Spec effect (needs fill): amount set 0
        // Spec: emit!(LoanLiquidated)
        // Spec transfer: pool_vault -> liquidator_ta amount=amount
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
