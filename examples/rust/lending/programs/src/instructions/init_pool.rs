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
pub struct InitPool {
    #[account(mut)]
    pub authority: Signer,
    #[account(mut, init, payer = authority, seeds = [b"pool", authority.key().as_ref()], bump)]
    pub pool: Account<PoolAccount>,
    pub system_program: Program<System>,
}

impl InitPool {
    #[qed(verified, spec = "../lending.qedspec", handler = "init_pool", hash = "b0db21ece9560e17", spec_hash = "b5d51ab2e00d8e0e")]
    #[inline(always)]
    pub fn handler(&mut self, rate: u64, bumps: &InitPoolBumps) -> Result<(), ProgramError> {
        guards::init_pool(self, rate)?;
        self.pool.interest_rate = rate;
        self.pool.total_deposits = 0;
        self.pool.total_borrows = 0;
        // Spec: emit!(PoolInitialized)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
