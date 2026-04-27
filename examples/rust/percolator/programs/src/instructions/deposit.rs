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
pub struct Deposit<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> Deposit<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "deposit", hash = "ba5ea0e0f0aa2732", spec_hash = "c1246b21825b0ed2")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize, amount: u128) -> Result<(), ProgramError> {
        guards::deposit(self, i, amount)?;
        self.vault.V = self.vault.V.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        self.vault.accounts[i].capital = self.vault.accounts[i].capital.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        Ok(())
    }
}
