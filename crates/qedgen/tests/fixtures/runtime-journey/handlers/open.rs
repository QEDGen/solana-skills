// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use crate::events::*;
use crate::guards;
use crate::{Open, OpenBumps};
use anchor_lang::prelude::*;
use qedgen_macros::qed;

impl<'info> Open<'info> {
    #[qed(
        verified,
        spec = "../vault.qedspec",
        handler = "open",
        hash = "85c40c48cacbff21",
        spec_hash = "5471ccd93495ed62"
    )]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &OpenBumps) -> Result<()> {
        guards::open(self)?;
        self.vault.owner = self.owner.key();
        self.vault.total = 0;
        // Persist the canonical bump Anchor derived for the `seeds`
        // constraint. `withdraw` signs the vault's token CPI with it, so
        // a wrong bump surfaces as a failed CPI instead of silently
        // diverging from the constraint.
        self.vault.bump = bumps.vault;
        emit!(VaultOpened {
            owner: self.owner.key()
        });
        Ok(())
    }
}
