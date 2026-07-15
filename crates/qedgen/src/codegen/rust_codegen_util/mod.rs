/// Shared helpers for generating Rust code from qedspec IR.
///
/// Used by both `proptest_gen` and `kani` to avoid duplicating
/// the qedspec-to-Rust translation logic.
use crate::check::{ParsedHandler, ParsedProperty, ParsedSpec};
use crate::codegen_shared::DslTypeExt;

// Per-concern submodules. The directory rename keeps the module path
// `crate::codegen::rust_codegen_util` (and the root re-export
// `crate::rust_codegen_util`) intact; these globs re-export each submodule's
// items so the existing `crate::rust_codegen_util::<name>` call sites — and the
// cross-submodule references — continue to resolve unchanged.
mod effect;
mod emit;
mod expr;
mod guards;
mod property;
mod pubkey;
// #151 Slice 1: no glob re-export (an unused glob trips `-D warnings`
// until the Kani/proptest emission port lands); path in via
// `rust_codegen_util::tree_render::{render_rust, RustCx, ...}`.
pub(crate) mod tree_render;

pub(crate) use effect::*;
pub(crate) use emit::*;
pub(crate) use expr::*;
pub(crate) use guards::*;
pub(crate) use property::*;
pub(crate) use pubkey::*;

#[cfg(test)]
mod tests;
