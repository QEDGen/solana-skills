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
    // sBPF assembly specs render a wholly different shape (guard/property
    // theorem stubs over `executeFn`/`wp_exec`) with no state-machine
    // `Stmt` representation; the renderer reads `ParsedSpec` directly —
    // MIR carries only the `is_assembly` dispatch signal.
    let content = if mir.is_assembly {
        render_sbpf(parsed)
    } else {
        render(mir)
    };
    crate::lean_sidecars::write_spec_with_sidecars(content, parsed, output_path)
}

/// Pure render. Dispatches on MIR shape and emits the full Spec.lean.
pub fn render(mir: &Mir) -> String {
    // sBPF is dispatched earlier in `generate`; only state-machine
    // shapes reach here.
    if is_indexed(mir) {
        return render_indexed_state(mir);
    }
    if is_multi_account(mir) {
        return render_multi_account(mir);
    }
    if is_multi_variant_adt(mir) {
        return render_single_account_adt(mir);
    }
    render_single_account(mir)
}

// ----------------------------------------------------------------------
// Shape detection
// ----------------------------------------------------------------------

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

fn render_single_account(mir: &Mir) -> String {
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
    let _pinned = emit_cpi_theorems(&mut out, mir);
    emit_invariants(&mut out, mir);
    emit_operation_inductive(&mut out, mir);
    emit_properties(&mut out, mir);
    emit_aborts_if(&mut out, mir);
    emit_ensures(&mut out, mir);
    emit_frame_conditions(&mut out, mir);
    emit_covers(&mut out, mir);
    emit_liveness(&mut out, mir);
    emit_environments(&mut out, mir);
    emit_overflow(&mut out, mir);
    emit_namespace_close(&mut out, mir);
    out
}

/// Multi-variant ADT path: state lowers as a real `inductive State where
/// | V1 | V2 …` block (payload per variant); transitions pattern-match on
/// the pre-variant; covers build per-variant witnesses; properties /
/// aborts / overflow take the ADT-flavored emitter pair.
fn render_single_account_adt(mir: &Mir) -> String {
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

    emit_transitions_adt(&mut out, mir);
    // ADT-flavored emitters (aborts / frame / overflow) emit `:= by sorry`
    // and the True-placeholder frame. Other sections (ensures, properties,
    // covers, liveness, environments) share the flat-shape emitters —
    // their statements are independent of the State carrier.
    let _pinned = emit_cpi_theorems(&mut out, mir);
    emit_invariants(&mut out, mir);
    emit_operation_inductive(&mut out, mir);
    emit_properties(&mut out, mir);
    emit_aborts_if_adt(&mut out, mir);
    emit_ensures(&mut out, mir);
    emit_frame_conditions_adt(&mut out, mir);
    emit_covers_adt(&mut out, mir);
    emit_liveness_adt(&mut out, mir);
    emit_environments(&mut out, mir);
    emit_overflow_adt(&mut out, mir);
    emit_namespace_close(&mut out, mir);
    out
}
