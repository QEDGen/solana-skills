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
pub struct LiquidateCase1<'info> {
    pub authority: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> LiquidateCase1<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "liquidate", hash = "3326abd4e85addbd", spec_hash = "7bd0413339d25826")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::liquidate_case_1(self, i)?;
        self.vault.accounts[i].active = (0).into();
        Ok(())
    }
}
