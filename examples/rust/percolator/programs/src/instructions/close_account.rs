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
pub struct CloseAccount<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> CloseAccount<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "close_account", hash = "7b2a0c9556522bf0", spec_hash = "6537f7c1d89dcb54")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::close_account(self, i)?;
        let capital: u128 = self.vault.accounts[(i) as usize].capital.into();
        self.vault.V = self.vault.V.checked_sub(capital).ok_or(PercolatorError::MathOverflow)?;
        self.vault.accounts[(i) as usize].capital = (0).into();
        self.vault.accounts[(i) as usize].active = (0).into();
        Ok(())
    }
}
