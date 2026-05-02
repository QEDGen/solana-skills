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
pub struct Reject<'info> {
    pub rejecter: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> Reject<'info> {
    #[qed(verified, spec = "../multisig.qedspec", handler = "reject", hash = "84367b63404eb816", spec_hash = "5c1482681fbda8fd")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, bumps: &RejectBumps) -> Result<(), ProgramError> {
        guards::reject(self, member_index)?;
        let _ = bumps;
        self.vault.rejection_count = self.vault.rejection_count.checked_add(1).ok_or(MultisigError::MathOverflow)?;
        self.vault.voted[(member_index) as usize] = (1).into();
        emit!(ProposalRejected {
            rejecter: *self.rejecter.address(),
            rejection_count: self.vault.rejection_count,
        });
        Ok(())
    }
}
