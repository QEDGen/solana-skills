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
    #[qed(verified, spec = "../multisig.qedspec", handler = "approve", hash = "659801016cb87703", spec_hash = "a2474552f74b6625")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &ApproveBumps) -> Result<(), ProgramError> {
        guards::approve(self, member_index)?;
        let _ = bumps;
        self.vault.approval_count = self.vault.approval_count.checked_add(1).ok_or(MultisigError::MathOverflow)?;
        self.vault.voted[(member_index) as usize] = (1).into();
        emit!(ProposalApproved {
            approver: *self.approver.address(),
            approval_count: self.vault.approval_count,
        });
        Ok(())
    }
}
