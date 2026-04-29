// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use crate::state::*;
use crate::guards;
use crate::events::*;
use qedgen_macros::qed;

#[derive(Accounts)]
pub struct InitPool<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut, init, payer = authority, seeds = [b"pool", authority], bump, has_one = authority)]
    pub pool: &'info mut Account<PoolAccount>,
    pub system_program: &'info Program<System>,
}

impl<'info> InitPool<'info> {
    #[qed(verified, spec = "../lending.qedspec", handler = "init_pool", hash = "8e67e51c9c857651", spec_hash = "b5d51ab2e00d8e0e")]
    #[inline(always)]
    pub fn handler(&mut self, rate: u64, bumps: &InitPoolBumps) -> Result<(), ProgramError> {
        guards::init_pool(self, rate)?;
        let _ = bumps;
        self.pool.interest_rate = (rate).into();
        self.pool.total_deposits = (0).into();
        self.pool.total_borrows = (0).into();
        emit!(PoolInitialized {
            authority: *self.authority.address(),
            rate,
        });
        Ok(())
    }
}
