//! Regression: duplicate `pub struct <name>` in different modules
//! caused the adapter to seal the wrong type.
//!
//! Pre-fix, `extract_accounts_type` discarded everything but the last
//! ident of `Context<X>`, so a handler whose signature explicitly
//! wrote `Context<crate::b::Shared>` could not communicate that
//! intent. `accounts_struct_for_handler` then walked every `*.rs`
//! under `src/` and returned the first `pub struct Shared` it found
//! — typically the wrong one. Post-fix the qualifying prefix is
//! preserved and used to prioritise the file walk.
//!
//! The handler bodies here are inline (they don't need to forward —
//! the regression is in the *signature*, not the body).
//!
//! See `crates/qedgen/src/anchor_adapt.rs::accounts_struct_for_handler`
//! and the regression test
//! `compute_attributes_respects_qualified_accounts_path`.

use anchor_lang::prelude::*;

pub mod a;
pub mod b;

declare_id!("Collision111111111111111111111111111111111");

#[program]
pub mod collision {
    use super::*;

    /// `Context<crate::a::Shared>` — MUST seal against `src/a/mod.rs`.
    pub fn lite(_ctx: Context<crate::a::Shared>, _amount: u64) -> Result<()> {
        Ok(())
    }

    /// `Context<crate::b::Shared>` — MUST seal against `src/b/mod.rs`,
    /// not the alphabetically-earlier `a/mod.rs`.
    pub fn heavy(_ctx: Context<crate::b::Shared>, _amount: u64) -> Result<()> {
        Ok(())
    }
}
