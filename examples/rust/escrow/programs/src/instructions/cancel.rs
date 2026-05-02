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
pub struct Cancel<'info> {
    #[account(mut)]
    pub initializer: &'info mut Signer,
    #[account(mut, seeds = [b"escrow", initializer], bump, has_one = initializer)]
    pub escrow: &'info mut Account<EscrowAccount>,
    #[account(mut)]
    pub escrow_ta: &'info mut Account<Token>,
    #[account(mut)]
    pub initializer_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
}

impl<'info> Cancel<'info> {
    #[qed(verified, spec = "../escrow.qedspec", handler = "cancel", hash = "ddff71e2acaaf308", spec_hash = "c47340875b51de3b")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &CancelBumps) -> Result<(), ProgramError> {
        guards::cancel(self)?;
        let _ = bumps;
        let initializer_amount: u64 = self.escrow.initializer_amount.into();
        let escrow_initializer = self.escrow.initializer;
        let escrow_bump = [self.escrow.bump];
        let escrow_seeds = [
            Seed::from(b"escrow" as &[u8]),
            Seed::from(escrow_initializer.as_ref()),
            Seed::from(&escrow_bump as &[u8]),
        ];
        self.token_program
            .transfer(&*self.escrow_ta, &*self.initializer_ta, &*self.escrow, initializer_amount)
            .invoke_signed(&escrow_seeds)?;
        emit!(EscrowCancelled {
            initializer: *self.initializer.address(),
        });
        Ok(())
    }
}
