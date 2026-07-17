// User-owned. Regenerating the spec does NOT overwrite this file.
// Guard checks live in the sibling `crate::guards` module and ARE
// regenerated on every `qedgen codegen`. Drift between the spec
// handler block and the `spec_hash` below fires a compile_error!
// via the `#[qed(verified, ...)]` macro.

use anchor_lang::prelude::*;
use crate::guards;
use qedgen_macros::qed;
use crate::state::VaultAccountInner;
use crate::{Initialize, InitializeBumps};

impl<'info> Initialize<'info> {
    #[qed(verified, spec = "../vault.qedspec", handler = "initialize", hash = "116b9e8c13801ca8", spec_hash = "08b5ccd111b6d5eb")]
    #[inline(always)]
    pub fn handler(&mut self, bumps: &InitializeBumps) -> Result<()> {
        guards::initialize(self)?;
        let _ = bumps;
        self.vault.inner = VaultAccountInner::Active {
            total_deposits: 0,
        };
        Ok(())
    }
}
