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
pub struct TriggerAdl<'info> {
    pub authority: &'info Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> TriggerAdl<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "trigger_adl", hash = "4f75dd7330073924", spec_hash = "16b18e714298add7")]
    #[inline(always)]
    pub fn handler(&mut self) -> Result<(), ProgramError> {
        guards::trigger_adl(self)?;
        Ok(())
    }
}
