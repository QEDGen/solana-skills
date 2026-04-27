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
pub struct RemoveMember {
    pub creator: Signer,
    #[account(mut, seeds = MultisigAccount::seeds(vault), bump)]
    pub vault: Account<MultisigAccount>,
}

impl RemoveMember {
    #[qed(verified, spec = "../multisig.qedspec", handler = "remove_member", hash = "eea3ddaf89ccbad2", spec_hash = "30f1786f10a626f5")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &RemoveMemberBumps) -> Result<(), ProgramError> {
        guards::remove_member(self)?;
        self.vault.member_count = self.vault.member_count.checked_sub(1).ok_or(MultisigError::MathOverflow)?;
        Ok(())
    }
}
