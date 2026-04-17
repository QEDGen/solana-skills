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
    #[account(mut, seeds = MultisigAccount::seeds(vault), bump)]
    pub vault: Account<()>,
}

impl Reject {
    #[qed(verified, spec = "../multisig.qedspec", handler = "reject", spec_hash = "a1b6f8e6f9e21a39")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &RejectBumps) -> Result<(), ProgramError> {
        guards::reject(self, member_index)?;
        // Spec effect: rejection_count add 1
        // Spec: emit!(ProposalRejected)
        todo!("user business logic")
    }
}
