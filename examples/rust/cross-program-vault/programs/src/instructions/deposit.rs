// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use anchor_lang::prelude::*;
use crate::guards;
use qedgen_macros::qed;
use crate::errors::*;
use crate::state::VaultAccountInner;
use crate::Deposit;

impl<'info> Deposit<'info> {
    #[qed(verified, spec = "../vault.qedspec", handler = "deposit", hash = "ba56825b97e00c86", spec_hash = "1fa96b38d214b3c3")]
    #[inline(always)]
    pub fn handler(&mut self, amount: u64) -> Result<()> {
        guards::deposit(self, amount)?;
        match &mut self.vault.inner {
            VaultAccountInner::Active { total_deposits, .. } => {
            *total_deposits = total_deposits.checked_add(amount).ok_or(VaultError::MathOverflow)?;
            }
            _ => return Err(VaultError::WrongState.into()),
        }
        // Spec call: Token.transfer (Anchor CPI emitted by v2.8 G4)
        {
            use anchor_spl::token::{self, Transfer};
            let cpi_accounts = Transfer {
                from:      self.user_ta.to_account_info(),
                to:        self.vault_ta.to_account_info(),
                authority: self.user.to_account_info(),
            };
            let cpi_program = self.token_program.to_account_info();
            token::transfer(CpiContext::new(cpi_program, cpi_accounts), amount)?;
        }
        Ok(())
    }
}
