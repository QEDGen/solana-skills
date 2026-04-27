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
pub struct Reject {
    pub rejecter: Signer,
    #[account(mut, seeds = [b"vault", vault.creator.as_ref()], bump)]
    pub vault: Account<MultisigAccount>,
}

impl Reject {
    #[qed(verified, spec = "../multisig.qedspec", handler = "reject", hash = "e7c7e12019595139", spec_hash = "ea0dd20a238e67a6")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &RejectBumps) -> Result<(), ProgramError> {
        guards::reject(self, member_index)?;
        self.vault.rejection_count = self.vault.rejection_count.checked_add(1).ok_or(MultisigError::MathOverflow)?;
        // Spec: emit!(ProposalRejected)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
