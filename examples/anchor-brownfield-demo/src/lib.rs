//! Demo Anchor program for `qedgen adapt` — a minimal counter.
//!
//! Two instructions, both using the Anchor scaffold's free-fn
//! forwarder pattern (`<module>::handler(ctx, args)`). Run
//! `qedgen adapt --program examples/anchor-brownfield-demo` to see
//! the generated `.qedspec` skeleton.

use anchor_lang::prelude::*;

pub mod instructions;

declare_id!("Counter11111111111111111111111111111111111");

#[program]
pub mod counter {
    use super::*;

    /// Initialize a fresh counter to a starting value.
    pub fn initialize(ctx: Context<Initialize>, start: u64) -> Result<()> {
        instructions::initialize::handler(ctx, start)
    }

    /// Increment the counter by `delta`. Errors if the increment
    /// would overflow `u64::MAX`.
    pub fn increment(ctx: Context<Increment>, delta: u64) -> Result<()> {
        instructions::increment::handler(ctx, delta)
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = authority, space = 8 + 8 + 32)]
    pub counter: Account<'info, Counter>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Increment<'info> {
    #[account(mut, has_one = authority)]
    pub counter: Account<'info, Counter>,
    pub authority: Signer<'info>,
}

#[account]
pub struct Counter {
    pub value: u64,
    pub authority: Pubkey,
}
