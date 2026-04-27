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
pub struct Propose {
    pub proposer: Signer,
    #[account(mut, seeds = [b"vault", vault.creator.as_ref()], bump)]
    pub vault: Account<MultisigAccount>,
}

impl Propose {
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
