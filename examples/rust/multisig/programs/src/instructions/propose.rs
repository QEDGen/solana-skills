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
    #[qed(verified, spec = "../multisig.qedspec", handler = "propose", hash = "d8710d638614d27b", spec_hash = "7e1a675c5e1599ed")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &ProposeBumps) -> Result<(), ProgramError> {
        guards::propose(self)?;
        let _ = bumps;
        self.vault.approval_count = (0).into();
        self.vault.rejection_count = (0).into();
        emit!(ProposalCreated {
            proposer: *self.proposer.address(),
        });
        Ok(())
    }
}
