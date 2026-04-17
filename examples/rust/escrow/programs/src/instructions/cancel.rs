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
pub struct Cancel {
    #[account(mut)]
    pub initializer: Signer,
    #[account(mut, seeds = EscrowAccount::seeds(escrow), bump)]
    pub escrow: Account<()>,
    #[account(mut, token::authority = escrow)]
    pub escrow_ta: Account<Token>,
    #[account(mut)]
    pub initializer_ta: Account<Token>,
    pub token_program: Program<()>,
}

impl Cancel {
    #[qed(verified, spec = "../escrow.qedspec", handler = "cancel", spec_hash = "2944975cff8d97d5")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &CancelBumps) -> Result<(), ProgramError> {
        guards::cancel(self)?;
        // Spec: emit!(EscrowCancelled)
        // Spec transfer: escrow_ta -> initializer_ta amount=initializer_amount
        todo!("fill non-mechanical effects, events, transfers")
    }
}
