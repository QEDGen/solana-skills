// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use crate::errors::*;
use crate::guards;
use crate::{Deposit, DepositBumps};
use anchor_lang::prelude::*;
use qedgen_macros::qed;

impl<'info> Deposit<'info> {
    #[qed(
        verified,
        spec = "../vault.qedspec",
        handler = "deposit",
        hash = "70cbf4f30e46d55e",
        spec_hash = "5cd4505be62a4b0a"
    )]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64, bumps: &DepositBumps) -> Result<()> {
        guards::deposit(self, amount)?;
        let _ = bumps;
        self.vault.total = self
            .vault
            .total
            .checked_add(amount)
            .ok_or(RuntimeVaultError::MathOverflow)?;
        // Depositor-authorized transfer: the owner signs directly, so
        // this is the non-PDA direction (contrast `withdraw`).
        anchor_spl::token::transfer(
            CpiContext::new(
                self.token_program.to_account_info(),
                anchor_spl::token::Transfer {
                    from: self.owner_ta.to_account_info(),
                    to: self.vault_ta.to_account_info(),
                    authority: self.owner.to_account_info(),
                },
            ),
            amount,
        )?;
        Ok(())
    }
}
