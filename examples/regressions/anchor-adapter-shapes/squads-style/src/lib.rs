//! Squads-style brownfield fixture: `<Type>::<method>(ctx, ...)`
//! forwarder shape. Each handler in `#[program]` mod calls a
//! type-associated function on the accounts type.
//!
//! `qedgen adapt --program examples/regressions/anchor-adapter-shapes/squads-style`
//! resolves these via the `TypeAssoc` classifier and `find_impl_method`
//! (M4.2). The output is `before.qedspec` here.

use anchor_lang::prelude::*;

declare_id!("Squads22222222222222222222222222222222222");

#[program]
pub mod multisig {
    use super::*;

    /// Create a new multisig.
    pub fn multisig_create(
        ctx: Context<MultisigCreate>,
        threshold: u16,
    ) -> Result<()> {
        MultisigCreate::multisig_create(ctx, threshold)
    }

    /// Approve a pending transaction by the calling member.
    pub fn proposal_approve(ctx: Context<ProposalApprove>) -> Result<()> {
        ProposalApprove::proposal_approve(ctx)
    }
}

#[derive(Accounts)]
pub struct MultisigCreate<'info> {
    #[account(mut)]
    pub multisig: Account<'info, Multisig>,
    #[account(mut)]
    pub creator: Signer<'info>,
}

#[derive(Accounts)]
pub struct ProposalApprove<'info> {
    #[account(mut)]
    pub multisig: Account<'info, Multisig>,
    pub member: Signer<'info>,
}

impl<'info> MultisigCreate<'info> {
    pub fn multisig_create(_ctx: Context<MultisigCreate>, threshold: u16) -> Result<()> {
        require!(threshold >= 1, MultisigError::InvalidThreshold);
        Ok(())
    }
}

impl<'info> ProposalApprove<'info> {
    pub fn proposal_approve(_ctx: Context<ProposalApprove>) -> Result<()> {
        Ok(())
    }
}

#[account]
pub struct Multisig {
    pub threshold: u16,
    pub member_count: u16,
}

#[error_code]
pub enum MultisigError {
    InvalidThreshold,
    NotAMember,
    AlreadyApproved,
}
