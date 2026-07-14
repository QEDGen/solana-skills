//! Structural prefix emission: file header banner, inlined math helpers
//! (`mul_div_floor_u128` / `mul_div_ceil_u128` / `mul_bps_floor_u128`), the
//! state-model header banner, and file-scoped constants.

use super::*;

/// File header: banner with the `tests/kani.rs` fingerprint hash.
pub(crate) fn emit_header(out: &mut String, parsed: &ParsedSpec) {
    let fp = crate::fingerprint::compute_fingerprint(parsed);
    out.push_str(&crate::codegen_shared::marker_unlabeled(
        &fp,
        "tests/kani.rs",
    ));
    out.push_str("//\n");
    out.push_str("// Self-contained Kani proof harnesses for the spec.\n");
    out.push_str("//\n");
    out.push_str("// These proofs verify the spec's transition design using Kani bounded model\n");
    out.push_str("// checking. They operate on a pure model of the state machine (derived from\n");
    out.push_str("// the qedspec), independent of framework (Quasar/Anchor) types.\n");
    out.push_str("//\n");
    out.push_str("//   Lean proves:  transition functions preserve invariants (∀ states)\n");
    out.push_str(
        "//   Kani checks:  same properties via bounded model checking + overflow detection\n",
    );
    out.push_str("//   Together:     high assurance that the spec design is correct\n");
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n\n");
}

/// Math helpers (`mul_div_floor_u128` / `mul_div_ceil_u128` /
/// `mul_bps_floor_u128`) — imported from the soundness-proven
/// `qedgen_kani_prelude` crate rather than re-inlined (#182), and only when the
/// spec's guards reference them. Both the crate and this harness are
/// `#![cfg(kani)]`, so the `use` compiles only under `cargo kani`; `run.rs`
/// delivers the crate + path-dep when the emitted harness references it.
pub(crate) fn emit_math_helpers(out: &mut String, parsed: &ParsedSpec) {
    // `allow(unused_imports)`: the floor/ceil pair is imported together but a
    // spec may reference only one (the old inline bodies carried `dead_code`).
    if crate::codegen_shared::guards_use_math_helpers(parsed) {
        out.push_str(
            "#[allow(unused_imports)]\n\
use qedgen_kani_prelude::{mul_div_ceil_u128, mul_div_floor_u128, mul_div_round_half_up_u128};\n\n",
        );
    }

    if crate::rust_codegen_util::spec_uses_kani_bps_mul_div_helper(parsed) {
        out.push_str("use qedgen_kani_prelude::mul_bps_floor_u128;\n\n");
    }
}

/// State model header banner — always emitted, even with no declared state.
pub(crate) fn emit_state_model_header(out: &mut String) {
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// State model (derived from qedspec — no framework dependencies)\n");
    out.push_str(
        "// ============================================================================\n\n",
    );
}

/// File-scoped constants, one per `Mir.constants` entry. Per-ADT modules
/// reference them via `use super::*`, so they live at file scope rather
/// than being duplicated.
pub(crate) fn emit_constants(out: &mut String, mir: &Mir) {
    if mir.constants.is_empty() {
        return;
    }
    crate::rust_codegen_util::emit_constants(out, &mir.constants);
}
