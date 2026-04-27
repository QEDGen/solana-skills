// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use quasar_lang::prelude::*;
use quasar_spl::{Token, Mint};
use crate::state::*;
use crate::guards;
use qedgen_macros::qed;
use crate::events::*;
use crate::errors::*;

#[derive(Accounts)]
pub struct Borrow<'info> {
    #[account(mut)]
    pub borrower: &'info mut Signer,
    #[account(mut, init, payer = borrower, seeds = [b"loan", pool, borrower], bump, has_one = borrower)]
    pub loan: &'info mut Account<LoanAccount>,
    #[account(mut)]
    pub pool: &'info mut Account<PoolAccount>,
    #[account(mut)]
    pub pool_vault: &'info mut Account<Token>,
    #[account(mut)]
    pub borrower_ta: &'info mut Account<Token>,
    pub token_program: &'info Program<System>,
    pub system_program: &'info Program<System>,
}

impl<'info> Borrow<'info> {
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
