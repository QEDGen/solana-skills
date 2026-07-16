use anchor_lang::prelude::*;

declare_id!("Vault1111111111111111111111111111111111111");

// Fresh clone, never `anchor build`-ed: no target/idl exists, so the
// overlay must report `derivable_idl: "anchor"` instead of staying silent.
#[program]
pub mod vault {
    use super::*;
    pub fn initialize(_ctx: Context<Initialize>, cap: u64) -> Result<()> {
        let _ = cap;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}
