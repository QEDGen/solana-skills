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
pub struct Propose<'info> {
    pub proposer: &'info Signer,
    #[account(mut)]
    pub vault: &'info mut Account<MultisigAccount>,
}

impl<'info> Propose<'info> {
    #[qed(verified, spec = "../multisig.qedspec", handler = "propose", hash = "67eab555bf36ad95", spec_hash = "b06988c8f1b3f041")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &ProposeBumps) -> Result<(), ProgramError> {
        guards::propose(self)?;
        self.vault.approval_count = 0;
        self.vault.rejection_count = 0;
        // Spec: emit!(ProposalCreated)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
