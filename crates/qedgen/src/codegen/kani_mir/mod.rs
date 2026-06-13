//! qedgen Kani codegen — the sole Kani path, consuming `mir::Mir` + the
//! originating `ParsedSpec` (passed through to the shared
//! `rust_codegen_util::emit_*` helpers). Output pinned by `tests/kani_snapshot.rs`.
//!
//! `generate` emits: structural prefix (banner / math helpers / state-model
//! header / file-scoped constants), per-account sections (multi-account wraps
//! each in `mod <lowercase>`; covers/liveness/env emit in single mode only),
//! then the `DO NOT EDIT BELOW` footer. sBPF specs never reach this module —
//! `qedgen codegen --kani` skips assembly targets (Lean + client tests instead).

use anyhow::Result;
use std::path::Path;

use crate::check::ParsedSpec;
use crate::codegen_shared::{write_generated_file, DslTypeExt};
use crate::mir::Mir;

// Per-concern submodules. The directory rename keeps the module path
// `crate::codegen::kani_mir` (and the root re-export `crate::kani_mir`) intact;
// these globs re-export each submodule's items so the existing
// `crate::kani_mir::<name>` call sites — and the cross-submodule references —
// continue to resolve unchanged.
mod account;
mod conformance;
mod driver;
mod guards;
mod prefix;
mod preservation;

pub(crate) use account::*;
pub(crate) use conformance::*;
pub(crate) use driver::*;
pub(crate) use guards::*;
pub(crate) use prefix::*;
pub(crate) use preservation::*;

#[cfg(test)]
mod tests;
