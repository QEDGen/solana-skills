//! Deliberately buggy Anchor program used by the v2.21 Slice §S1.2
//! Crucible lamport-conservation regression fixture. See ../README.md.
//!
//! Three handlers, chosen to demonstrate BOTH what Crucible's brownfield
//! crash-first harness catches and what it does *not*:
//!
//!   * `drain`  — transfers `source`'s lamports to an arbitrary `target`
//!                with no authorization policy. Both are signers, so both
//!                are in the harness's tracked set; `target` GAINS lamports,
//!                tripping the v2.21 §S1.2 `assert_no_signer_inflation`
//!                guard. **This is the finding the fixture fires.**
//!   * `run`    — divides by a runtime zero. **Does NOT fire**: an
//!                in-program SBF fault surfaces as a transaction *error*,
//!                not a host panic, so Crucible's intrinsic detector never
//!                sees it. Kept as a control for the README's discussion.
//!   * `maybe`  — `Option::unwrap` on `None`. Same story as `run`: a
//!                program-side abort, not a host crash. Does NOT fire.
//!
//! The §S1.2 guard is a *protocol* invariant (lamport conservation), so it
//! fires with no `.qedspec` — the brownfield value-add. The `run`/`maybe`
//! controls document why "crash-first" alone can't catch in-program panics.

use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("6bRRkRXokuEQs6sctPhSGjqEnEkPgbda16N1aajwH7bp");

#[program]
pub mod buggy_anchor {
    use super::*;

    /// Divides by a runtime zero. Does NOT fire under crash-first (see
    /// the module doc): the SBF fault is a transaction error, not a host
    /// panic.
    ///
    /// The divisor is runtime-derived (`stub.lamports() - stub.lamports()`)
    /// rather than `let zero = 0`: rustc's `unconditional_panic` lint
    /// const-folds the literal form into a *compile* error, so the program
    /// would never build and `cargo build-sbf` couldn't emit the `.so`.
    pub fn run(ctx: Context<Empty>) -> Result<()> {
        let l = ctx.accounts.stub.lamports();
        let zero: u32 = (l - l) as u32;
        let _ = 100u32 / zero;
        Ok(())
    }

    /// Unwraps a `None`. Does NOT fire under crash-first — program-side
    /// abort, not a host panic.
    pub fn maybe(ctx: Context<Empty>) -> Result<()> {
        let _ = ctx;
        let value: Option<u32> = None;
        let _ = value.unwrap();
        Ok(())
    }

    /// Sweeps half of `source`'s lamports into `target` with no check that
    /// `target` is an authorized recipient. Implemented as a System CPI
    /// transfer (so it works against the harness's system-owned, funded
    /// accounts). Because `target` is a tracked signer that GAINS lamports,
    /// the v2.21 §S1.2 lamport-inflation guard fires — surfacing the drain
    /// shape with no spec annotation.
    pub fn drain(ctx: Context<DrainAccounts>) -> Result<()> {
        let amount = ctx.accounts.source.to_account_info().lamports() / 2;
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.source.to_account_info(),
                    to: ctx.accounts.target.to_account_info(),
                },
            ),
            amount,
        )
    }
}

#[derive(Accounts)]
pub struct Empty<'info> {
    /// Stand-in unchecked account; the bug fires before any account
    /// access matters so the contents don't matter.
    /// CHECK: not validated; brownfield demo only.
    pub stub: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct DrainAccounts<'info> {
    /// Funds the transfer; signs the CPI.
    #[account(mut)]
    pub source: Signer<'info>,
    /// Arbitrary recipient — no authorization policy (the bug). A signer
    /// so the harness tracks it; gaining lamports trips the §S1.2 guard.
    #[account(mut)]
    pub target: Signer<'info>,
    pub system_program: Program<'info, System>,
}
