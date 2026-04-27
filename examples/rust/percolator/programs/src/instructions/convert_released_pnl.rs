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
pub struct ConvertReleasedPnl<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> ConvertReleasedPnl<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "convert_released_pnl", hash = "b616936ec856cc3a", spec_hash = "cad0809661eff4aa")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize, x: u128) -> Result<(), ProgramError> {
        guards::convert_released_pnl(self, i, x)?;
        self.vault.V = self.vault.V.checked_sub(x).ok_or(PercolatorError::MathOverflow)?;
        self.vault.accounts[i].reserved_pnl = self.vault.accounts[i].reserved_pnl.checked_sub(x).ok_or(PercolatorError::MathOverflow)?;
        Ok(())
    }
}
