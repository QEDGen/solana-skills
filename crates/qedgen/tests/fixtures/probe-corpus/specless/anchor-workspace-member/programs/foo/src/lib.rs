use anchor_lang::prelude::*;

declare_id!("Foo1111111111111111111111111111111111111111");

// Workspace-member shape (#241): this program lives at `programs/foo/`
// while its committed IDL sits at the repo root under `idl/foo.json`.
// Probing `--root programs/foo` must still find and apply the overlay.
#[program]
pub mod foo {
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
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
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
