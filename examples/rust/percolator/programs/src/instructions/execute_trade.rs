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
pub struct ExecuteTrade<'info> {
    pub authority: &'info Signer,
    #[account(mut, has_one = authority)]
    pub vault: &'info mut Account<PercolatorAccount>,
}

impl<'info> ExecuteTrade<'info> {
    #[qed(verified, spec = "../percolator.qedspec", handler = "execute_trade", hash = "85e82ad5ed0accc4", spec_hash = "3e2b05f1e61e35b8")]
    #[inline(always)]
    pub fn handler(&mut self, a: usize, b: usize, size_q: i128, exec_price: u64) -> Result<(), ProgramError> {
        guards::execute_trade(self, a, b, size_q, exec_price)?;
        Ok(())
    }
}
