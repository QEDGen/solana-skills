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
pub struct Exchange<'info> {
    #[account(mut)]
    pub taker: &'info mut Signer,
    #[account(mut, has_one = taker)]
    pub escrow: &'info mut Account<EscrowAccount>,
    #[account(mut)]
    pub initializer_ta: &'info mut Account<Token>,
    #[account(mut)]
    pub taker_ta: &'info mut Account<Token>,
    #[account(mut)]
    pub escrow_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<Token>,
}

impl<'info> Exchange<'info> {
    #[qed(verified, spec = "../escrow.qedspec", handler = "exchange", hash = "7b560f1c9c9b8b97", spec_hash = "67871b9bea7db0e6")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &ExchangeBumps) -> Result<(), ProgramError> {
        guards::exchange(self)?;
        let _ = bumps;
        let taker_amount: u64 = self.escrow.taker_amount.into();
        let initializer_amount: u64 = self.escrow.initializer_amount.into();
        // Taker pays the initializer.
        self.token_program
            .transfer(&*self.taker_ta, &*self.initializer_ta, &*self.taker, taker_amount)
            .invoke()?;
        // Escrow PDA releases the initializer's deposit to the taker.
        let escrow_initializer = self.escrow.initializer;
        let escrow_bump = [self.escrow.bump];
        let escrow_seeds = [
            Seed::from(b"escrow" as &[u8]),
            Seed::from(escrow_initializer.as_ref()),
            Seed::from(&escrow_bump as &[u8]),
        ];
        self.token_program
            .transfer(&*self.escrow_ta, &*self.taker_ta, &*self.escrow, initializer_amount)
            .invoke_signed(&escrow_seeds)?;
        emit!(EscrowExchanged {
            taker: *self.taker.address(),
            amount: taker_amount,
        });
        Ok(())
    }
}
