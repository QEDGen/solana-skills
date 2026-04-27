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
pub struct LiquidateCase0<'info> {
    pub authority: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> LiquidateCase0<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "liquidate", hash = "cf44a92c87457290", spec_hash = "7bd0413339d25826")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::liquidate_case_0(self, i)?;
        Ok(())
    }
}
