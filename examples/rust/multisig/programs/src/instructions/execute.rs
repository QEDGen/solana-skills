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
pub struct Execute {
    pub executor: Signer,
    #[account(mut, seeds = MultisigAccount::seeds(vault), bump)]
    pub vault: Account<MultisigAccount>,
}

impl Execute {
    #[qed(verified, spec = "../multisig.qedspec", handler = "execute", hash = "449f179b64c96261", spec_hash = "f1085840ed69a1d7")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &ExecuteBumps) -> Result<(), ProgramError> {
        guards::execute(self)?;
        self.vault.approval_count = 0;
        self.vault.rejection_count = 0;
        // Spec: emit!(ProposalExecuted)
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
