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
pub struct AddLp<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> AddLp<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "add_lp", hash = "bd03bd97810286c7", spec_hash = "bd20261ef292bca6")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::add_lp(self, i)?;
        self.vault.accounts[i].active = (1).into();
        Ok(())
    }
}
