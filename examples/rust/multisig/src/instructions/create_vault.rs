// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;

#[derive(Accounts)]
pub struct CreateVault<'info> {
    #[account(mut)]
    pub creator: &'info mut Signer,
    #[account(mut, seeds = [b"vault", creator], bump, has_one = creator)]
    pub vault: &'info mut Account<MultisigAccount>,
    pub system_program: &'info Program<System>,
}

impl<'info> CreateVault<'info> {
    #[qed(verified, spec = "multisig.qedspec", handler = "create_vault", hash = "e9d49ea51bfc5616", spec_hash = "ca38005f77bb0ff7")]
    #[inline(always)]
    pub fn handler(&mut self, threshold: u8, member_count: u8, bumps: &CreateVaultBumps) -> Result<(), ProgramError> {
        guards::create_vault(self, threshold, member_count)?;
        let _ = bumps;
        self.vault.threshold = (threshold).into();
        self.vault.member_count = (member_count).into();
        self.vault.approval_count = (0).into();
        self.vault.rejection_count = (0).into();
        // Spec: emit!(VaultCreated)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
