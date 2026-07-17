use anchor_lang::prelude::*;

declare_id!("Vault1111111111111111111111111111111111111");

#[program]
pub mod vault {
    use super::*;
    // Declared in the IDL — signer `admin` → authority_gated narrowing.
    // The bound check is a held invariant the arithmetic_bound
    // hypothesizer lifts into a confirmable clause.
    pub fn initialize(_ctx: Context<Initialize>, cap: u64) -> Result<()> {
        require!(cap <= 1_000_000, VaultError::CapTooHigh);
        let _ = cap;
        Ok(())
    }
    // Declared in the IDL, no signer → permissionless narrowing.
    pub fn crank(_ctx: Context<Crank>) -> Result<()> {
        Ok(())
    }
    // NOT in the IDL — source_only drift candidate.
    pub fn emergency_withdraw(_ctx: Context<Crank>, amount: u64) -> Result<()> {
        let _ = amount;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    // One-shot by construction: `init` fails on an existing account —
    // evidence anchor for the lifecycle_init_once hypothesis.
    #[account(init, payer = admin, space = 128)]
    pub vault: Account<'info, Vault>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct Crank {}

#[account]
pub struct Vault {
    pub cap: u64,
}

#[error_code]
pub enum VaultError {
    #[msg("cap exceeds the maximum")]
    CapTooHigh,
}
