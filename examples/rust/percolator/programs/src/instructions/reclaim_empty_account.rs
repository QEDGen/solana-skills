// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;
use crate::errors::*;

#[derive(Accounts)]
pub struct ReclaimEmptyAccount<'info> {
    pub authority: &'info Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> ReclaimEmptyAccount<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "reclaim_empty_account", hash = "bba97c39895a4d1f", spec_hash = "131b0df2f49fa778")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::reclaim_empty_account(self, i)?;
        self.vault.accounts[i].active = (0).into();
        Ok(())
    }
}
