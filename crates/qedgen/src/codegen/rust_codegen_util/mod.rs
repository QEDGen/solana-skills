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

pub(crate) use effect::*;
pub(crate) use emit::*;
pub(crate) use expr::*;
pub(crate) use guards::*;
pub(crate) use property::*;
pub(crate) use pubkey::*;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardTermSource {
    Guard,
    Requires { error_name: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardTerm {
    pub source: GuardTermSource,
    pub rust_expr: String,
}
