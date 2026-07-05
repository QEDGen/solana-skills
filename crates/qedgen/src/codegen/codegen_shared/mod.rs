//! Shared Rust-codegen helper library for `codegen_mir`: the per-target
//! `FrameworkSurface`, `generate_guards`, the Pinocchio scaffold emitters,
//! the SPL CPI dispatch (`try_emit_cpi` / `emit_spl_*`), and helpers
//! (`map_type`, `to_pascal_case`, `mechanize_effect`, …). These read
//! `ParsedSpec` directly — account-constraint / guard-predicate /
//! framework-scaffold surface, not effect-body `Stmt` IR.
//!
//! This is a facade: each concern lives in a sibling module, re-exported
//! here so existing `crate::codegen_shared::<symbol>` paths stay valid.

// Common imports shared by every submodule (reached via `use super::*;`).
pub(crate) use crate::check::{ParsedHandler, ParsedSpec};
pub(crate) use crate::fingerprint::SpecFingerprint;
pub(crate) use crate::spec_hash;
pub(crate) use crate::Target;
pub(crate) use anyhow::Result;
pub(crate) use std::path::Path;

/// Placeholder spliced into the `hash = "..."` field of `#[qed(verified)]`
/// during scaffold rendering; the fixup pass at the end of
/// `render_handler_scaffold` replaces it with the real body hash. Obviously
/// not SHA-hex, so a missed fixup trips the macro's "expected hash format"
/// error instead of shipping silently.
pub(crate) const BODY_HASH_PLACEHOLDER: &str = "QEDGEN_FIXUP_BODY_HASH";

mod account_attr;
mod cargo_toml;
mod cpi;
mod effect;
mod generators;
mod guards;
mod scaffold;
mod types;

pub(crate) use account_attr::*;
pub(crate) use cargo_toml::*;
pub(crate) use cpi::*;
pub(crate) use effect::*;
pub(crate) use generators::*;
pub(crate) use guards::*;
pub(crate) use scaffold::*;
pub(crate) use types::*;

#[cfg(test)]
mod tests;
