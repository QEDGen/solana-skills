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
pub struct LiquidateOtherwise<'info> {
    pub authority: &'info Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> LiquidateOtherwise<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "liquidate", hash = "0acf8b80d22d297b", spec_hash = "7bd0413339d25826")]
    #[inline(always)]
    pub fn handler(&mut self, i: usize) -> Result<(), ProgramError> {
        guards::liquidate_otherwise(self, i)?;
        Ok(())
    }
}
