use anchor_lang::prelude::*;

pub mod errors;
use crate::errors::VaultError;

declare_id!("Unwired11111111111111111111111111111111111");

#[program]
pub mod unwired {
    use super::*;

    // `Unauthorized` is wired via require!, `AmountTooLarge` via a
    // return Err — both must stay silent. `HookAuthorityCannotBePartOfHookAccounts`
    // is defined in errors.rs but referenced nowhere → the sole candidate.
    pub fn withdraw(ctx: Context<Withdraw>, amount: u64, is_authority: bool) -> Result<()> {
        require!(is_authority, VaultError::Unauthorized);
        if amount > 1_000 {
            return Err(VaultError::AmountTooLarge.into());
        }
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Withdraw {}
