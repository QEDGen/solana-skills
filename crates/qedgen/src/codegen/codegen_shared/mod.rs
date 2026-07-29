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

/// `declare_id!` value emitted when the spec declares no `program_id`
/// (#368). This is the System Program's address, which cannot be a user
/// program's own address, so it is a placeholder in every sense — but it is
/// a VALID base58 pubkey, so nothing downstream rejects it on shape.
///
/// Single-sourced because three places have to agree about it: codegen
/// emits it, the `missing_program_id` lint warns about it, and the probe
/// reproducer lane must refuse to aim an attack transaction at it. When
/// those were independent, the repro lane happily resolved the System
/// Program as the target and could report "no bug" for a reason unrelated
/// to the finding.
pub(crate) const PLACEHOLDER_PROGRAM_ID: &str = "11111111111111111111111111111111";

mod account_attr;
mod cargo_toml;
mod cpi;
mod effect;
mod error_variants;
mod generators;
mod guards;
mod scaffold;
mod types;

pub(crate) use account_attr::*;
pub(crate) use cargo_toml::*;
pub(crate) use cpi::*;
pub(crate) use effect::*;
pub(crate) use error_variants::*;
pub(crate) use generators::*;
pub(crate) use guards::*;
pub(crate) use scaffold::*;
pub(crate) use types::*;

#[cfg(test)]
mod tests;
