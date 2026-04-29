// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use quasar_spl::{Token, TokenCpi};
use crate::state::*;
use crate::guards;
use crate::events::*;
use qedgen_macros::qed;

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub initializer: &'info mut Signer,
    #[account(mut, seeds = [b"escrow", initializer], bump, has_one = initializer)]
    pub escrow: &'info mut Account<EscrowAccount>,
    pub mint: &'info UncheckedAccount,
    #[account(mut)]
    pub initializer_ta: &'info mut Account<Token>,
    #[account(mut)]
    pub escrow_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
    pub system_program: &'info Program<System>,
}

impl<'info> Initialize<'info> {
    #[qed(verified, spec = "../escrow.qedspec", handler = "initialize", hash = "1de528c0f3938362", spec_hash = "804b5ee68ad1d84b")]
    #[inline(always)]
    pub fn handler(&mut self, deposit_amount: u64, receive_amount: u64, bumps: &InitializeBumps) -> Result<(), ProgramError> {
        guards::initialize(self, deposit_amount, receive_amount)?;
        let _ = bumps;
        self.escrow.initializer_amount = (deposit_amount).into();
        self.escrow.taker_amount = (receive_amount).into();
        self.escrow.initializer_token_account = *self.initializer_ta.address();
        self.token_program
            .transfer(&*self.initializer_ta, &*self.escrow_ta, &*self.initializer, deposit_amount)
            .invoke()?;
        emit!(EscrowInitialized {
            initializer: *self.initializer.address(),
            amount: deposit_amount,
        });
        Ok(())
    }
}
