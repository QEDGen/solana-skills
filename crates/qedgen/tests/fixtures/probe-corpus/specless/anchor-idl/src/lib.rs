use anchor_lang::prelude::*;

declare_id!("Vault1111111111111111111111111111111111111");

#[program]
pub mod vault {
    use super::*;
    // Declared in the IDL — signer `admin` → authority_gated narrowing.
    pub fn initialize(_ctx: Context<Initialize>, cap: u64) -> Result<()> {
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
pub struct Initialize {}
#[derive(Accounts)]
pub struct Crank {}
