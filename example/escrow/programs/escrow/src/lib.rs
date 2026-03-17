use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

declare_id!("FyeRokiKoSz9VxRdgDEuKVKwWuGsZLEbMkywgJQDXeFK");

#[program]
pub mod escrow {
    use super::*;

    /// Initialize an escrow account
    ///
    /// This creates an escrow state that defines the terms of the exchange:
    /// - The initializer deposits token A
    /// - The taker must provide token B to complete the exchange
    pub fn initialize(
        ctx: Context<Initialize>,
        amount: u64,
        taker_amount: u64,
    ) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;

        escrow.initializer = ctx.accounts.initializer.key();
        escrow.initializer_token_account = ctx.accounts.initializer_deposit_token_account.key();
        escrow.initializer_amount = amount;
        escrow.taker_amount = taker_amount;
        escrow.escrow_token_account = ctx.accounts.escrow_token_account.key();
        escrow.bump = ctx.bumps.escrow;

        // Transfer tokens from initializer to escrow
        let cpi_accounts = Transfer {
            from: ctx.accounts.initializer_deposit_token_account.to_account_info(),
            to: ctx.accounts.escrow_token_account.to_account_info(),
            authority: ctx.accounts.initializer.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        msg!("Escrow initialized: {} tokens deposited", amount);
        Ok(())
    }

    /// Execute the escrow exchange
    ///
    /// The taker deposits their tokens and receives the initializer's tokens.
    /// The initializer receives the taker's tokens.
    pub fn exchange(ctx: Context<Exchange>) -> Result<()> {
        let escrow = &ctx.accounts.escrow;

        // Transfer taker's tokens to initializer
        let cpi_accounts = Transfer {
            from: ctx.accounts.taker_deposit_token_account.to_account_info(),
            to: ctx.accounts.initializer_receive_token_account.to_account_info(),
            authority: ctx.accounts.taker.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, escrow.taker_amount)?;

        // Transfer initializer's tokens from escrow to taker
        let seeds = &[
            b"escrow".as_ref(),
            escrow.initializer.as_ref(),
            &[escrow.bump],
        ];
        let signer = &[&seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.escrow_token_account.to_account_info(),
            to: ctx.accounts.taker_receive_token_account.to_account_info(),
            authority: ctx.accounts.escrow.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        token::transfer(cpi_ctx, escrow.initializer_amount)?;

        msg!("Escrow exchange completed successfully");
        Ok(())
    }

    /// Cancel the escrow and return tokens to initializer
    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
        let escrow = &ctx.accounts.escrow;

        // Transfer tokens back from escrow to initializer
        let seeds = &[
            b"escrow".as_ref(),
            escrow.initializer.as_ref(),
            &[escrow.bump],
        ];
        let signer = &[&seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.escrow_token_account.to_account_info(),
            to: ctx.accounts.initializer_deposit_token_account.to_account_info(),
            authority: ctx.accounts.escrow.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        token::transfer(cpi_ctx, escrow.initializer_amount)?;

        msg!("Escrow cancelled, tokens returned to initializer");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub initializer: Signer<'info>,

    #[account(
        init,
        payer = initializer,
        space = 8 + EscrowState::INIT_SPACE,
        seeds = [b"escrow", initializer.key().as_ref()],
        bump
    )]
    pub escrow: Account<'info, EscrowState>,

    #[account(mut)]
    pub initializer_deposit_token_account: Account<'info, TokenAccount>,

    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = initializer,
        token::mint = mint,
        token::authority = escrow,
        seeds = [b"escrow_token", initializer.key().as_ref()],
        bump
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Exchange<'info> {
    #[account(mut)]
    pub taker: Signer<'info>,

    #[account(
        mut,
        seeds = [b"escrow", escrow.initializer.as_ref()],
        bump = escrow.bump,
        close = initializer
    )]
    pub escrow: Account<'info, EscrowState>,

    #[account(mut)]
    pub taker_deposit_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub taker_receive_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub initializer_receive_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [b"escrow_token", escrow.initializer.as_ref()],
        bump
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// CHECK: This is the initializer account that will receive rent
    #[account(mut)]
    pub initializer: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Cancel<'info> {
    #[account(mut)]
    pub initializer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"escrow", initializer.key().as_ref()],
        bump = escrow.bump,
        has_one = initializer,
        close = initializer
    )]
    pub escrow: Account<'info, EscrowState>,

    #[account(mut)]
    pub initializer_deposit_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        seeds = [b"escrow_token", initializer.key().as_ref()],
        bump
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[account]
#[derive(InitSpace)]
pub struct EscrowState {
    pub initializer: Pubkey,
    pub initializer_token_account: Pubkey,
    pub initializer_amount: u64,
    pub taker_amount: u64,
    pub escrow_token_account: Pubkey,
    pub bump: u8,
}

#[error_code]
pub enum EscrowError {
    #[msg("The provided amount must be greater than zero")]
    InvalidAmount,
}
