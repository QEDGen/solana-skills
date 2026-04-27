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
pub struct CancelProposal<'info> {
    pub canceller: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> CancelProposal<'info> {
    #[qed(verified, spec = "../multisig.qedspec", handler = "cancel_proposal", hash = "8acac11c9628cb80", spec_hash = "35605e3ff8d9be8a")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &CancelProposalBumps) -> Result<(), ProgramError> {
        guards::cancel_proposal(self)?;
        self.vault.approval_count = (0).into();
        self.vault.rejection_count = (0).into();
        // Spec: emit!(ProposalCancelled)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
