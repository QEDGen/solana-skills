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
pub struct AddMember<'info> {
    pub creator: &'info Signer,
    #[account(mut, seeds = [b"vault", creator], bump, has_one = creator)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> AddMember<'info> {
    #[qed(verified, spec = "../multisig.qedspec", handler = "add_member", hash = "9ec9c37e98a46e02", spec_hash = "4210397198e8c8a2")]
    #[inline(always)]
    pub fn handler(&mut self, member_index: u8, member_pubkey: Address, bumps: &AddMemberBumps) -> Result<(), ProgramError> {
        guards::add_member(self, member_index, member_pubkey)?;
        let _ = bumps;
        self.vault.members[(member_index) as usize] = (member_pubkey).into();
        Ok(())
    }
}
