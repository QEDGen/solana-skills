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

/// Resolve the state-field / lifecycle view for a section emitter: single
/// `type Account` block → its fields + lifecycle; none →
/// `resolve_state_fields` + `spec.lifecycle_states`. Multi-account specs
/// reaching this path get the best-effort primary-account view — the
/// multi-mode dispatch passes a single-account scoped spec, so in
/// practice `account_types.len() <= 1` at every call site.
pub(crate) fn resolve_account_view(parsed: &ParsedSpec) -> (&[(String, String)], &[String]) {
    if parsed.account_types.is_empty() {
        (
            crate::rust_codegen_util::resolve_state_fields(parsed),
            parsed.lifecycle_states.as_slice(),
        )
    } else {
        (
            &parsed.account_types[0].fields,
            parsed.account_types[0].lifecycle.as_slice(),
        )
    }
}

/// Options for the shared `#[kani::proof]` harness preamble.
pub(crate) struct PreambleOpts<'a> {
    pub(crate) harness_name: &'a str,
    pub(crate) unwind: usize,
    pub(crate) solver: &'a str,
    /// Zeroed init (init handlers) instead of symbolic init.
    pub(crate) zeroed_init: bool,
    /// Emit the pre-status assume after the symbolic init. Ignored on
    /// the zeroed path and when `op` is `None` (file-level harnesses).
    pub(crate) pre_status_assume: bool,
}

/// Shared harness preamble: `#[kani::proof]` / unwind / solver attrs, the
/// `fn <name>() {` line, and the state init (symbolic + optional
/// pre-status assume, or zeroed for init handlers).
pub(crate) fn emit_proof_preamble(
    out: &mut String,
    parsed: &ParsedSpec,
    op: Option<&crate::check::ParsedHandler>,
    mutable: &[&(String, String)],
    lifecycle: &[String],
    opts: PreambleOpts<'_>,
) {
    use crate::rust_codegen_util as util;

    out.push_str("#[kani::proof]\n");
    out.push_str(&format!("#[kani::unwind({})]\n", opts.unwind));
    out.push_str(&format!("#[kani::solver({})]\n", opts.solver));
    out.push_str(&format!("fn {}() {{\n", opts.harness_name));
    if opts.zeroed_init {
        util::emit_state_init_zeroed(out, mutable, lifecycle, parsed);
    } else {
        util::emit_state_init_symbolic(out, mutable, lifecycle);
        if opts.pre_status_assume {
            if let Some(op) = op {
                util::emit_pre_status_assume(out, op, lifecycle);
            }
        }
    }
}

/// Symbolic `takes_params` + abstract-binder emission shared by the
/// harness bodies. `binder_emits` is 1 normally, 2 for the guard/abort
/// harnesses' deliberate double-emit (bug-for-bug parity with the
/// snapshot-pinned output; cleanup deferred to v3.0).
pub(crate) fn emit_symbolic_params(
    out: &mut String,
    parsed: &ParsedSpec,
    op: &crate::check::ParsedHandler,
    binder_emits: usize,
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    for (pname, ptype) in &op.takes_params {
        out.push_str(&format!(
            "    let {}: {} = kani::any();\n",
            pname,
            map_type(ptype, parsed)?
        ));
    }
    for _ in 0..binder_emits {
        util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
