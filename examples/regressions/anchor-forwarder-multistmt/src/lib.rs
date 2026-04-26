//! Regression: multi-statement forwarder body misclassified as Inline.
//!
//! Pre-fix, `qedgen adapt` saw the two-statement
//! `instructions::buy::handler(ctx, amount)?; Ok(())` shape and
//! returned `ForwarderKind::Inline`, sealing the wrapper bytes in
//! `lib.rs` instead of the real handler in `instructions/buy.rs`.
//! That meant `qedgen check --anchor-project` falsely flagged the
//! handler's effects as missing — the bytes it hashed were just the
//! forwarder, not the real handler body.
//!
//! Post-fix, the classifier accepts `<call>?; Ok(())` (no user logic
//! between the call and return) as a pure forwarder. See
//! `crates/qedgen/src/anchor_resolver.rs::extract_forwarder_tail`.

use anchor_lang::prelude::*;

pub mod instructions;

declare_id!("Multistmt1111111111111111111111111111111111");

#[program]
pub mod multistmt {
    use super::*;

    /// Two-statement forwarder: propagate via `?`, then return Ok(()).
    /// Reviewer's repro shape — ubiquitous in Anchor scaffolds.
    pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
        instructions::buy::handler(ctx, amount)?;
        Ok(())
    }

    /// Single-statement `?`-tail forwarder: same intent without the
    /// `Ok(())` second statement.
    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        instructions::settle::handler(ctx)?
    }

    /// Genuinely-inline body — multi-stmt with user logic. Stays
    /// classified Inline post-fix; the `msg!` between the call and
    /// the return is real work that has to flow into the body hash.
    pub fn fence(ctx: Context<Fence>) -> Result<()> {
        instructions::fence::handler(&ctx)?;
        msg!("fence done");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Buy<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct Fence<'info> {
    pub authority: Signer<'info>,
}
