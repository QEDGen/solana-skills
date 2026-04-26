//! Marinade-style brownfield fixture: `ctx.accounts.<method>(...)`
//! forwarder shape. Each handler in `#[program]` mod hands off to a
//! method on the `Context<X>`'s accounts struct, defined in a sibling
//! module.
//!
//! `qedgen adapt --program examples/regressions/anchor-adapter-shapes/marinade-style`
//! resolves these handlers via the `AccountsMethod` classifier and
//! `find_impl_method` (M4.2). The output is `before.qedspec` here.

use anchor_lang::prelude::*;

pub mod instructions;

declare_id!("Marinade1111111111111111111111111111111111");

#[program]
pub mod stake {
    use super::*;

    /// Stake some lamports.
    pub fn deposit(ctx: Context<Deposit>, lamports: u64) -> Result<()> {
        ctx.accounts.process(lamports)
    }

    /// Unstake some shares.
    pub fn liquid_unstake(ctx: Context<LiquidUnstake>, shares: u64) -> Result<()> {
        ctx.accounts.process(shares)
    }
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub state: Account<'info, StakeState>,
    #[account(mut)]
    pub depositor: Signer<'info>,
}

#[derive(Accounts)]
pub struct LiquidUnstake<'info> {
    #[account(mut)]
    pub state: Account<'info, StakeState>,
    #[account(mut)]
    pub holder: Signer<'info>,
}

#[account]
pub struct StakeState {
    pub total_lamports: u64,
    pub total_shares: u64,
}

#[error_code]
pub enum StakeError {
    InsufficientStake,
    BelowMinimum,
}
