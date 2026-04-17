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
pub struct Exchange {
    #[account(mut)]
    pub taker: Signer,
    #[account(mut, seeds = EscrowAccount::seeds(escrow), bump)]
    pub escrow: Account<()>,
    #[account(mut)]
    pub initializer_ta: Account<Token>,
    #[account(mut)]
    pub taker_ta: Account<Token>,
    #[account(mut, token::authority = escrow)]
    pub escrow_ta: Account<Token>,
    pub token_program: Program<()>,
}

impl Exchange {
    #[qed(verified, spec = "../escrow.qedspec", handler = "exchange", spec_hash = "b6db8e2676934188")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &ExchangeBumps) -> Result<(), ProgramError> {
        guards::exchange(self)?;
        // Spec: emit!(EscrowExchanged)
        // Spec transfer: taker_ta -> initializer_ta amount=taker_amount
        // Spec transfer: escrow_ta -> taker_ta amount=initializer_amount
        todo!("fill non-mechanical effects, events, transfers")
    }
}
