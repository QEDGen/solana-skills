// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;

#[derive(Accounts)]
pub struct Execute<'info> {
    pub executor: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> Execute<'info> {
    #[qed(verified, spec = "multisig.qedspec", handler = "execute", hash = "9134d22fd08b4dce", spec_hash = "1db32bbd43fb72f5")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &ExecuteBumps) -> Result<(), ProgramError> {
        guards::execute(self, member_index)?;
        let _ = bumps;
        self.vault.approval_count = (0).into();
        self.vault.rejection_count = (0).into();
        // Spec: emit!(ProposalExecuted)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
