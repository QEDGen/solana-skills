//! qedgen Lean codegen — the sole Lean path. Consumes `mir::Mir` and writes
//! `Spec.lean` (+ interface sidecars via [`crate::lean_sidecars`]). Output
//! pinned by `tests/mir_snapshot.rs`.
//!
//! `render` dispatches on spec shape: sBPF (`mir.is_assembly`, dispatched in
//! `generate`) → `render_sbpf`; indexed (records / `Map[N] T`) →
//! `render_indexed_state`; multi-account → `render_multi_account`;
//! multi-variant ADT → `render_single_account_adt`; else
//! `render_single_account`, whose fixed section order is: imports →
//! namespace → helpers/ref-impls → constants → Status → State → transitions
//! → CPI theorems → invariants → Operation/applyOp → properties → aborts →
//! ensures → frame → covers/liveness/env/overflow → end.

use crate::mir::Mir;
use crate::obligations::{ObligationBackend, ObligationEntry, ObligationRecorder};
use anyhow::Result;
use std::path::Path;

mod cpi;
mod indexed;
mod liveness;
mod multi_account;
mod overflow;
mod properties;
mod sbpf;
mod state;
// #151 Slice 2: no glob re-export until the lean_gen_mir emission port lands.
#[cfg(test)]
mod tests;
mod transitions;
pub(crate) mod tree_render;
mod util;

#[allow(unused_imports)]
use cpi::*;
#[allow(unused_imports)]
use indexed::*;
#[allow(unused_imports)]
use liveness::*;
#[allow(unused_imports)]
use multi_account::*;
#[allow(unused_imports)]
use overflow::*;
#[allow(unused_imports)]
use properties::*;
#[allow(unused_imports)]
use sbpf::*;
#[allow(unused_imports)]
use state::*;
#[allow(unused_imports)]
use transitions::*;
#[allow(unused_imports)]
use util::*;

/// Top-level entry: render the `Spec.lean` body from MIR, then delegate
/// sidecar work to `lean_sidecars::write_spec_with_sidecars`.
pub fn generate(mir: &Mir, parsed: &crate::check::ParsedSpec, output_path: &Path) -> Result<()> {
    generate_with_obligations(mir, parsed, output_path).map(|_| ())
}

/// `generate` + the backend-obligation record (#332): what this run
/// emitted, and what it could not express, per obligation.
pub fn generate_with_obligations(
    mir: &Mir,
    parsed: &crate::check::ParsedSpec,
    output_path: &Path,
) -> Result<Vec<ObligationEntry>> {
    // sBPF assembly specs render a wholly different shape (guard/property
    // theorem stubs over `executeFn`/`wp_exec`) with no state-machine
    // `Stmt` representation; the renderer reads `ParsedSpec` directly —
    // MIR carries only the `is_assembly` dispatch signal. sBPF is out of
    // scope for the obligation manifest v1: no entries.
    if mir.is_assembly {
        let content = render_sbpf(parsed);
        crate::lean_sidecars::write_spec_with_sidecars(content, parsed, output_path)?;
        return Ok(Vec::new());
    }
    let mut rec = ObligationRecorder::new(ObligationBackend::Lean);
    let content = render_with_obligations(mir, &mut rec);
    crate::lean_sidecars::write_spec_with_sidecars(content, parsed, output_path)?;
    Ok(rec.into_entries())
}

/// Obligation collection without artifact generation: run the pure render
/// with a recorder and discard the output. sBPF specs report no entries
/// (out of scope for the manifest v1).
pub fn collect_obligations(mir: &Mir) -> Vec<ObligationEntry> {
    if mir.is_assembly {
        return Vec::new();
    }
    let mut rec = ObligationRecorder::new(ObligationBackend::Lean);
    let _ = render_with_obligations(mir, &mut rec);
    rec.into_entries()
}

/// Pure render. Dispatches on MIR shape and emits the full Spec.lean.
/// Creates and discards a recorder — existing callers/tests that only
/// need the rendered string stay signature-stable.
#[cfg_attr(not(test), allow(dead_code))]
pub fn render(mir: &Mir) -> String {
    let mut rec = ObligationRecorder::new(ObligationBackend::Lean);
    render_with_obligations(mir, &mut rec)
}

/// `render` with obligation recording (#332). Recording MUST NOT change
/// rendered output by a single byte — `tests/mir_snapshot.rs` pins this.
pub(crate) fn render_with_obligations(mir: &Mir, rec: &mut ObligationRecorder) -> String {
    // sBPF is dispatched earlier in `generate`; only state-machine
    // shapes reach here.
    if is_indexed(mir) {
        return render_indexed_state(mir, rec);
    }
    if is_multi_account(mir) {
        return render_multi_account(mir, rec);
    }
    if is_multi_variant_adt(mir) {
        return render_single_account_adt(mir, rec);
    }
    render_single_account(mir, rec)
}

// ----------------------------------------------------------------------
// Shape detection
// ----------------------------------------------------------------------

/// Whether this spec routes to the indexed-state Lean renderer
/// (`Map[N]` fields) — the shape whose theorems live in the user-owned
/// `Proofs.lean` skeleton, not `Spec.lean`. The obligation reconcile
/// (#332) keys the `lean_indexed_shape_proofs_external` status on this.
pub(crate) fn uses_indexed_shape(mir: &Mir) -> bool {
    is_indexed(mir)
}

fn is_indexed(mir: &Mir) -> bool {
    mir.state.variants.iter().any(|v| {
        v.fields
            .iter()
            .any(|f| matches!(&f.ty, crate::mir::Ty::Map { .. }))
    })
}

fn is_multi_account(mir: &Mir) -> bool {
    mir.account_states.len() > 1
}

/// True iff the single-account spec opts into the multi-variant ADT shape:
/// declares `pragma state_repr = adt` (lifted to `Mir::adt_state`), has ≥ 2
/// state variants, and is not indexed (Map / record fields route elsewhere).
fn is_multi_variant_adt(mir: &Mir) -> bool {
    mir.adt_state && mir.state.variants.len() > 1 && !is_indexed(mir)
}

// ----------------------------------------------------------------------
// Shape-specific renderers
// ----------------------------------------------------------------------

fn render_single_account(mir: &Mir, rec: &mut ObligationRecorder) -> String {
    let mut out = String::new();
    emit_header(&mut out, mir);
    emit_namespace_open(&mut out, mir);
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);
    emit_constants(&mut out, mir);
    emit_lifecycle_marker(&mut out, mir);
    emit_state_struct(&mut out, mir);
    emit_transitions(&mut out, mir);
    // In-`Spec.lean` CPI theorems only; sibling axiom modules + lakefile
    // wiring are written by `lean_sidecars::write_spec_with_sidecars`,
    // which recomputes the pinned set — the returned value is unused.
    let _pinned = emit_cpi_theorems(&mut out, mir, rec);
    emit_invariants(&mut out, mir);
    emit_operation_inductive(&mut out, mir);
    emit_properties(&mut out, mir, rec);
    emit_aborts_if(&mut out, mir, rec);
    emit_ensures(&mut out, mir, rec);
    emit_frame_conditions(&mut out, mir, rec);
    emit_covers(&mut out, mir, rec);
    emit_liveness(&mut out, mir, rec);
    emit_environments(&mut out, mir, rec);
    emit_overflow(&mut out, mir, rec);
    emit_namespace_close(&mut out, mir);
    out
}

/// Multi-variant ADT path: state lowers as a real `inductive State where
/// | V1 | V2 …` block (payload per variant); transitions pattern-match on
/// the pre-variant; covers build per-variant witnesses; properties /
/// aborts / overflow take the ADT-flavored emitter pair.
fn render_single_account_adt(mir: &Mir, rec: &mut ObligationRecorder) -> String {
    let mut out = String::new();
    emit_header(&mut out, mir);
    emit_namespace_open(&mut out, mir);
    emit_uninterpreted_helpers(&mut out, mir);
    emit_ref_impls(&mut out, mir);
    emit_constants(&mut out, mir);

    emit_status_inductive_adt(&mut out, mir);
    emit_inductive_state_adt(&mut out, mir);
    emit_state_status_accessor_adt(&mut out, mir);
    emit_state_field_accessors_adt(&mut out, mir);

    emit_transitions_adt(&mut out, mir, rec);
    // ADT-flavored emitters (aborts / frame / overflow) emit `:= by sorry`
    // and the True-placeholder frame. Other sections (ensures, properties,
    // covers, liveness, environments) share the flat-shape emitters —
    // their statements are independent of the State carrier.
    let _pinned = emit_cpi_theorems(&mut out, mir, rec);
    emit_invariants(&mut out, mir);
    emit_operation_inductive(&mut out, mir);
    emit_properties(&mut out, mir, rec);
    emit_aborts_if_adt(&mut out, mir, rec);
    emit_ensures(&mut out, mir, rec);
    emit_frame_conditions_adt(&mut out, mir, rec);
    emit_covers_adt(&mut out, mir, rec);
    emit_liveness_adt(&mut out, mir, rec);
    emit_environments(&mut out, mir, rec);
    emit_overflow_adt(&mut out, mir, rec);
    emit_namespace_close(&mut out, mir);
    out
}
