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
pub struct Repay {
    #[account(mut)]
    pub borrower: Signer,
    #[account(mut, seeds = [b"loan", pool.key().as_ref(), borrower.key().as_ref()], bump)]
    pub loan: UncheckedAccount,
    #[account(mut, seeds = [b"pool", pool.authority.as_ref()], bump)]
    pub pool: UncheckedAccount,
    #[account(mut, token::authority = pool)]
    pub pool_vault: Account<Token>,
    #[account(mut)]
    pub borrower_ta: Account<Token>,
    pub token_program: Program<System>,
}

impl Repay {
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
