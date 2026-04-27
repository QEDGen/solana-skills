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
pub struct CancelProposal {
    pub canceller: Signer,
    #[account(mut, seeds = MultisigAccount::seeds(vault), bump)]
    pub vault: Account<MultisigAccount>,
}

impl CancelProposal {
    #[qed(verified, spec = "../multisig.qedspec", handler = "cancel_proposal", hash = "2625d4f5e45311fa", spec_hash = "816a905224142489")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &CancelProposalBumps) -> Result<(), ProgramError> {
        guards::cancel_proposal(self)?;
        self.vault.approval_count = 0;
        self.vault.rejection_count = 0;
        // Spec: emit!(ProposalCancelled)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
