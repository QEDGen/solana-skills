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
pub struct Approve<'info> {
    pub approver: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> Approve<'info> {
    #[qed(verified, spec = "../multisig.qedspec", handler = "approve", hash = "0d53ec7ab3074e97", spec_hash = "0adb464f360e890a")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &ApproveBumps) -> Result<(), ProgramError> {
        guards::approve(self, member_index)?;
        self.vault.approval_count = self.vault.approval_count.checked_add(1).ok_or(MultisigError::MathOverflow)?;
        // Spec: emit!(ProposalApproved)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
