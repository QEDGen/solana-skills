// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use crate::errors::*;
use crate::events::*;
use crate::guards;
use crate::{Withdraw, WithdrawBumps};
use anchor_lang::prelude::*;
use qedgen_macros::qed;

impl<'info> Withdraw<'info> {
    #[qed(
        verified,
        spec = "../vault.qedspec",
        handler = "withdraw",
        hash = "e60f68b3b9fa381d",
        spec_hash = "5bbaee2050327cd2"
    )]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64, bumps: &WithdrawBumps) -> Result<()> {
        guards::withdraw(self, amount)?;
        let _ = bumps;
        self.vault.total = self
            .vault
            .total
            .checked_sub(amount)
            .ok_or(RuntimeVaultError::MathUnderflow)?;
        // The token authority is the vault PDA itself, so the CPI must
        // carry its signer seeds. These MUST match the `seeds` + `bump`
        // on the generated `#[account(...)]` constraint; a mismatch is
        // invisible to `cargo check` and only fails here, at runtime.
        let owner_key = self.owner.key();
        let signer_seeds: &[&[u8]] = &[b"vault", owner_key.as_ref(), &[bumps.vault]];
        anchor_spl::token::transfer(
            CpiContext::new_with_signer(
                self.token_program.to_account_info(),
                anchor_spl::token::Transfer {
                    from: self.vault_ta.to_account_info(),
                    to: self.owner_ta.to_account_info(),
                    authority: self.vault.to_account_info(),
                },
                &[signer_seeds],
            ),
            amount,
        )?;
        emit!(Withdrawn {
            owner: owner_key,
            amount
        });
        Ok(())
    }
}
