use anchor_lang::prelude::*;

declare_id!("8QZ9bJtGZqfNfZ5Bw4tEp7Np5nVbA2qDMoZ3XaTw5H4R");

#[program]
pub mod stored_counter {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        ctx.accounts.counter.count = 0;
        Ok(())
    }

    pub fn increment(ctx: Context<Increment>, amount: u64) -> Result<()> {
        let counter = &mut ctx.accounts.counter;
        #[cfg(feature = "corrected")]
        { counter.count = counter.count.saturating_add(amount).min(10); }
        #[cfg(not(feature = "corrected"))]
        { counter.count = counter.count.saturating_add(amount); }
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = authority, space = 8 + 8, seeds = [b"counter"], bump)]
    pub counter: Account<'info, Counter>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Increment<'info> {
    #[account(mut, seeds = [b"counter"], bump)]
    pub counter: Account<'info, Counter>,
}

#[account]
pub struct Counter {
    pub count: u64,
}
