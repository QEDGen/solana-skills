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
pub struct TopUpInsurance<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> TopUpInsurance<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "top_up_insurance", hash = "401a67b01994714c", spec_hash = "056c1e0955d861c4")]
    #[inline(always)]
    pub fn handler(&mut self, amount: u128) -> Result<(), ProgramError> {
        guards::top_up_insurance(self, amount)?;
        self.vault.V = self.vault.V.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        self.vault.I = self.vault.I.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        Ok(())
    }
}
