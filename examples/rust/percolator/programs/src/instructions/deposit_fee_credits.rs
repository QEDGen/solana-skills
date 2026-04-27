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
pub struct DepositFeeCredits<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> DepositFeeCredits<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "deposit_fee_credits", hash = "b72580bb27230339", spec_hash = "2ec166fc7b6d1c47")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize, amount: u128) -> Result<(), ProgramError> {
        guards::deposit_fee_credits(self, i, amount)?;
        self.vault.V = self.vault.V.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        self.vault.F = self.vault.F.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        self.vault.accounts[i].fee_credits = self.vault.accounts[i].fee_credits.checked_add(amount).ok_or(PercolatorError::MathOverflow)?;
        Ok(())
    }
}
