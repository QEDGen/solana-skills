// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;
use crate::events::*;
use crate::errors::*;

#[derive(Accounts)]
pub struct Execute<'info> {
    pub executor: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> Execute<'info> {
    #[qed(verified, spec = "../multisig.qedspec", handler = "execute", hash = "74f274605ace77f8", spec_hash = "84fd7f21daf375f3")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &ExecuteBumps) -> Result<(), ProgramError> {
        guards::execute(self)?;
        let _ = bumps;
        self.vault.approval_count = (0).into();
        self.vault.rejection_count = (0).into();
        emit!(ProposalExecuted {
            executor: *self.executor.address(),
        });
        Ok(())
    }
}
