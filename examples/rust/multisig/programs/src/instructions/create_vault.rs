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
pub struct CreateVault {
    #[account(mut)]
    pub creator: Signer,
    #[account(mut, init, payer = creator, seeds = [b"vault", creator.key().as_ref()], bump)]
    pub vault: Account<MultisigAccount>,
    pub system_program: Program<System>,
}

impl CreateVault {
    #[qed(verified, spec = "../multisig.qedspec", handler = "create_vault", hash = "5153647095626903", spec_hash = "17cb8535550bbe69")]
    #[inline(always)]
    pub fn handler(&mut self, threshold: u8, member_count: u8, bumps: &CreateVaultBumps) -> Result<(), ProgramError> {
        guards::create_vault(self, threshold, member_count)?;
        self.vault.threshold = threshold;
        self.vault.member_count = member_count;
        self.vault.approval_count = 0;
        self.vault.rejection_count = 0;
        // Spec: emit!(VaultCreated)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
