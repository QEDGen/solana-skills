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
pub struct Withdraw<'info> {
    #[account(mut)]
    pub authority: &'info mut Signer,
    #[account(mut)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> Withdraw<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "withdraw", hash = "0859e05e99e3954a", spec_hash = "16390bf2da1be4e7")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize, amount: u128) -> Result<(), ProgramError> {
        guards::withdraw(self, i, amount)?;
        self.vault.V = self.vault.V.checked_sub(amount).ok_or(PercolatorError::MathOverflow)?;
        self.vault.accounts[i].capital = self.vault.accounts[i].capital.checked_sub(amount).ok_or(PercolatorError::MathOverflow)?;
        Ok(())
    }
}
