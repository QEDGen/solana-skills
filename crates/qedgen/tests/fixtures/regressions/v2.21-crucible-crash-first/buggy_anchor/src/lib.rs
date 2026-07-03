//! Deliberately buggy Anchor program for the v2.21 §S1.2 Crucible
//! lamport-conservation regression fixture. See ../README.md.
//!
//! Three handlers, demonstrating BOTH what Crucible's brownfield crash-
//! first harness catches and what it does *not*:
//!
//!   * `drain`  — empties a **program-owned PDA vault** into the calling
//!                `authority` with no check that the caller is the
//!                legitimate admin. Any signer can drain the vault to
//!                itself. `authority` is a tracked wallet that GAINS the
//!                vault's lamports (which come from outside the tracked
//!                set), tripping the §S1.2 `assert_no_wallet_inflation`
//!                guard. **This is the finding the fixture fires** — a
//!                textbook missing-authority-check withdraw.
//!   * `run`    — divides by a runtime zero. **Does NOT fire**: an
//!                in-program SBF fault surfaces as a transaction *error*,
//!                not a host panic, so Crucible's intrinsic detector never
//!                sees it. Kept as a control.
//!   * `maybe`  — `Option::unwrap` on `None`. Same as `run`: a program-
//!                side abort, not a host crash. Does NOT fire.
//!
//! The §S1.2 guard is a *protocol* invariant (lamport conservation), so it
//! fires with no `.qedspec` — the brownfield value-add.

use anchor_lang::prelude::*;

declare_id!("6bRRkRXokuEQs6sctPhSGjqEnEkPgbda16N1aajwH7bp");

#[program]
pub mod buggy_anchor {
    use super::*;

    /// Drains the program-owned `vault` PDA into `authority` with **no
    /// authority check** — the vulnerability. A program may freely debit
    /// accounts it owns, so this direct lamport move succeeds for any
    /// caller, draining the vault to whoever signs.
    pub fn drain(ctx: Context<DrainAccounts>) -> Result<()> {
        let amount = ctx.accounts.vault.to_account_info().lamports();
        **ctx.accounts.vault.try_borrow_mut_lamports()? = 0;
        let credited = ctx
            .accounts
            .authority
            .to_account_info()
            .lamports()
            .checked_add(amount)
            .unwrap();
        **ctx.accounts.authority.to_account_info().try_borrow_mut_lamports()? = credited;
        Ok(())
    }

    /// Divides by a runtime zero. Does NOT fire under crash-first: the SBF
    /// fault is a transaction error, not a host panic.
    ///
    /// The divisor is runtime-derived (`authority.lamports() -
    /// authority.lamports()`) rather than `let zero = 0`: rustc's
    /// `unconditional_panic` lint const-folds the literal form into a
    /// *compile* error, so the crate would never build.
    pub fn run(ctx: Context<OneSigner>) -> Result<()> {
        let l = ctx.accounts.authority.to_account_info().lamports();
        let zero: u32 = (l - l) as u32;
        let _ = 100u32 / zero;
        Ok(())
    }

    /// Unwraps a `None`. Does NOT fire under crash-first — program-side
    /// abort, not a host panic.
    pub fn maybe(ctx: Context<OneSigner>) -> Result<()> {
        let _ = ctx;
        let value: Option<u32> = None;
        let _ = value.unwrap();
        Ok(())
    }
}

#[derive(Accounts)]
pub struct OneSigner<'info> {
    /// Pays the fee so the handler actually executes (and faults
    /// in-program, demonstrating the crash-first limitation).
    #[account(mut)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct DrainAccounts<'info> {
    /// Program-owned vault (PDA, empty seeds). The program may debit it.
    /// CHECK: address validated by the seeds constraint; raw AccountInfo
    /// for the direct lamport move.
    #[account(mut, seeds = [], bump)]
    pub vault: AccountInfo<'info>,
    /// The caller — receives the drained lamports. **Never checked against
    /// a stored admin/authority** (the bug). A signer, so the harness
    /// tracks it; gaining the vault's lamports trips the §S1.2 guard.
    #[account(mut)]
    pub authority: Signer<'info>,
}
