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
pub struct AddUser<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> AddUser<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "add_user", hash = "04e9a2bb3fefe484", spec_hash = "bd20261ef292bca6")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::add_user(self, i)?;
        self.vault.accounts[i].active = (1).into();
        Ok(())
    }
}
