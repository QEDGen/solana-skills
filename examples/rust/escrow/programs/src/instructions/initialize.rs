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
pub struct Initialize {
    #[account(mut)]
    pub initializer: Signer,
    #[account(mut, init, payer = initializer, seeds = EscrowAccount::seeds(escrow), bump)]
    pub escrow: Account<()>,
    pub mint: Account<()>,
    #[account(mut)]
    pub initializer_ta: Account<Token>,
    #[account(mut, token::authority = escrow)]
    pub escrow_ta: Account<Token>,
    pub token_program: Program<()>,
    pub system_program: Program<()>,
}

impl Initialize {
    #[qed(verified, spec = "../escrow.qedspec", handler = "initialize", spec_hash = "579d73a84cc6b6f0")]
    #[inline(always)]
    pub fn handler(&mut self, deposit_amount: u64, receive_amount: u64, bumps: &InitializeBumps) -> Result<(), ProgramError> {
        guards::initialize(self, deposit_amount, receive_amount)?;
        self.escrow.initializer_amount = deposit_amount;
        self.escrow.taker_amount = receive_amount;
        // Spec: emit!(EscrowInitialized)
        // Spec transfer: initializer_ta -> escrow_ta amount=deposit_amount
        todo!("fill non-mechanical effects, events, transfers")
    }
}
