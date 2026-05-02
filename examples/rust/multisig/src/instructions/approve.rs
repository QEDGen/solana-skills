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
pub struct Approve<'info> {
    pub approver: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> Approve<'info> {
    #[qed(verified, spec = "multisig.qedspec", handler = "approve", hash = "8a4696899e69668e", spec_hash = "96727ecc91c0452e")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &ApproveBumps) -> Result<(), ProgramError> {
        guards::approve(self, member_index)?;
        let _ = bumps;
        self.vault.approval_count = self.vault.approval_count.checked_add(1).ok_or(MultisigError::MathOverflow)?;
        self.vault.voted[(member_index) as usize] = (1).into();
        // Spec: emit!(ProposalApproved)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
