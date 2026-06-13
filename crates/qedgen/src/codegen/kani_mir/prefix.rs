//! Structural prefix emission: file header banner, inlined math helpers
//! (`mul_div_floor_u128` / `mul_div_ceil_u128` / `mul_bps_floor_u128`), the
//! state-model header banner, and file-scoped constants.

use super::*;

/// File header: banner with the `tests/kani.rs` fingerprint hash.
pub(crate) fn emit_header(out: &mut String, parsed: &ParsedSpec) {
    let fp = crate::fingerprint::compute_fingerprint(parsed);
    let hash = fp
        .file_hashes
        .get("tests/kani.rs")
        .cloned()
        .unwrap_or_default();

    out.push_str(&crate::banner::banner(None, &hash));
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

/// Math helpers (`mul_div_floor_u128` / `mul_div_ceil_u128`), inlined only
/// when the spec's guards reference them, so the standalone harness
/// compiles without depending on `src/math.rs`.
pub(crate) fn emit_math_helpers(out: &mut String, parsed: &ParsedSpec) {
    // The backslash-continuation strings deliberately drop per-line body
    // indentation — that un-indented shape is what every committed kani.rs
    // fixture/snapshot was generated against. Don't re-indent.
    if crate::codegen_shared::guards_use_math_helpers(parsed) {
        out.push_str(
            "#[allow(dead_code)]\n\
#[inline]\n\
fn mul_div_floor_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 { return 0; }\n\
    a.saturating_mul(b) / d\n\
}\n\n\
#[allow(dead_code)]\n\
#[inline]\n\
fn mul_div_ceil_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 { return 0; }\n\
    let prod = a.saturating_mul(b);\n\
    if prod % d == 0 { prod / d } else { (prod / d).saturating_add(1) }\n\
}\n\n",
        );
    }

    if crate::rust_codegen_util::spec_uses_kani_bps_mul_div_helper(parsed) {
        out.push_str(
            "#[allow(dead_code)]\n\
#[inline]\n\
fn mul_bps_floor_u128(a: u128, bps: u128) -> u128 {\n\
    if bps > 10000 { return u128::MAX; }\n\
    let b = (bps as u16) as u128;\n\
    let q = a / 10000;\n\
    let r = a % 10000;\n\
    q.wrapping_mul(b).wrapping_add(r.wrapping_mul(b) / 10000)\n\
}\n\n",
        );
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
