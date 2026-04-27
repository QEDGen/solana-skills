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
pub struct Borrow {
    #[account(mut)]
    pub borrower: Signer,
    #[account(mut, init, payer = borrower, seeds = [b"loan", pool.key().as_ref(), borrower.key().as_ref()], bump)]
    pub loan: UncheckedAccount,
    #[account(mut, init, payer = borrower, seeds = [b"pool", pool.authority.as_ref()], bump)]
    pub pool: UncheckedAccount,
    #[account(mut, token::authority = pool)]
    pub pool_vault: Account<Token>,
    #[account(mut)]
    pub borrower_ta: Account<Token>,
    pub token_program: Program<System>,
    pub system_program: Program<System>,
}

impl Borrow {
    #[qed(verified, spec = "../lending.qedspec", handler = "borrow", hash = "f94a35e667f6acc5", spec_hash = "6a1c2376f61d1679")]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64, collateral: u64, bumps: &BorrowBumps) -> Result<(), ProgramError> {
        guards::borrow(self, amount, collateral)?;
        // Spec effect (needs fill): amount set amount
        // Spec effect (needs fill): collateral set collateral
        // Spec: emit!(Borrowed)
        // Spec transfer: pool_vault -> borrower_ta amount=amount
        todo!("fill non-mechanical effects, events, transfers, calls")
    }
}
