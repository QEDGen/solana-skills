use super::*;

/// Framework descriptor for the struct-based (Anchor / Quasar) impl
/// harness emitters. The two scaffolds are byte-identical in shape — the
/// Quasar scaffold emits the user's handler as `impl <Pascal> { pub fn
/// handler(&mut self, …) }` exactly like Anchor's, and its `#[program]`
/// `Ctx<X>` dispatcher just forwards `ctx.accounts.handler(…)` — so one
/// emitter serves both, with the fingerprint key + header prose as data.
pub(crate) struct StructImplFramework {
    /// `compute_fingerprint` file key. Anchor emits under `tests/`;
    /// Quasar under `src/` (per design doc §11a: `cargo kani` only
    /// discovers `#[kani::proof]` in the lib for that target, and the
    /// harness's `crate::<Pascal>` references only resolve from inside
    /// the lib crate — the CLI's `redirect_kani_impl_to_src` rewrites
    /// the output path accordingly).
    pub(crate) fingerprint_key: &'static str,
    /// Framework label passed to `emit_symbolic_accounts_module`.
    pub(crate) label: &'static str,
    /// Header prose between the banner's `//` separator and the shared
    /// "Pairs with `tests/kani.rs`" block.
    pub(crate) intro: &'static str,
    /// Extra placement note emitted after the "Pairs with" block
    /// (includes its own leading `//` separator); empty for Anchor.
    pub(crate) placement: &'static str,
}

pub(crate) const ANCHOR_IMPL_FRAMEWORK: StructImplFramework = StructImplFramework {
    fingerprint_key: "tests/kani_impl.rs",
    label: "Anchor",
    intro: "\
// Impl-targeted Kani harnesses — call the user's real Anchor handler\n\
// against a symbolic `Accounts` context and assert the spec's\n\
// `ensures` clauses against pre/post account-field snapshots.\n",
    placement: "",
};

pub(crate) const QUASAR_IMPL_FRAMEWORK: StructImplFramework = StructImplFramework {
    fingerprint_key: "src/kani_impl.rs",
    label: "Quasar",
    intro: "\
// Impl-targeted Kani harnesses for a Quasar (`#[program]`) program.\n\
// Calls the user's real handler — the `impl <Pascal> { fn handler\n\
// (&mut self, …) }` method that the `Ctx<X>` dispatcher forwards to —\n\
// against a symbolic accounts struct, asserting the spec's `ensures`\n\
// clauses against pre/post account-field snapshots.\n",
    placement: "\
//\n\
// PLACEMENT (per the M1 smoke-test findings, design doc §11a):\n\
//   1. This file lives in the program crate's `src/` (NOT `tests/`):\n\
//      `cargo kani` only discovers `#[kani::proof]` in the lib, and\n\
//      the `crate::<Pascal>` references below only resolve there.\n\
//   2. Add this line to the crate root (`src/lib.rs`):\n\
//        #[cfg(kani)] mod kani_impl;\n\
//      A Quasar crate is `std` on the host target Kani builds for, so\n\
//      — unlike Pinocchio — no `extern crate kani` is needed.\n",
};

/// Emit the struct-based impl-targeted harness for `fw`: symbolic
/// accounts struct + `.handler(<params>)` call against the user's real
/// handler.
///
/// One `build_<handler>()` per emit target (see
/// `emit_symbolic_accounts_module`): a fully-symbolic accounts context
/// where PDA-derived addresses bind to the spec's declared `pda <name>
/// [seeds]` and account-data fields are `kani::any()`. The shape mirrors
/// what `tests/integration_tests.rs` builds at runtime, but with
/// `kani::any()` substituted for concrete keypair init; the handler is
/// called via `accounts.handler(<params>)` — the same method signature
/// the integration test invokes.
pub(crate) fn emit_kani_impl_struct_framework(
    spec: &ParsedSpec,
    output_path: &Path,
    emit_targets: &[&ParsedHandler],
    auto_handlers: &[&str],
    explicit_flag: bool,
    fw: &StructImplFramework,
) -> Result<()> {
    let fp = crate::fingerprint::compute_fingerprint(spec);

    let mut out = String::new();

    // ── File header ──────────────────────────────────────────────────────
    out.push_str(&crate::codegen_shared::marker_unlabeled(
        &fp,
        fw.fingerprint_key,
    ));
    out.push_str("//\n");
    out.push_str(fw.intro);
    out.push_str("//\n");
    out.push_str("// Pairs with `tests/kani.rs` (spec-model harness) — that file checks\n");
    out.push_str("// the spec's effect block satisfies its own ensures; this file checks\n");
    out.push_str("// the user's Rust impl does. A counterexample here blames the impl,\n");
    out.push_str("// not the spec.\n");
    out.push_str(fw.placement);
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered: the following handlers declare `modifies` fields\n");
        out.push_str("// that are NOT written in their `effect` block (the v2.25 LP-shape\n");
        out.push_str("// signal). The agent-fill `todo!()` site is expected to compute\n");
        out.push_str("// those fields against the spec's ensures; this harness verifies\n");
        out.push_str("// the result.\n");
        for name in auto_handlers {
            out.push_str(&format!("//   - {}\n", name));
        }
        out.push_str("//\n");
        out.push_str("// Pass `--kani-impl` to `qedgen codegen` to force emission for\n");
        out.push_str("// every handler with `ensures`, regardless of the modifies-diff.\n");
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n\n");

    // ── Symbolic-accounts builder module ────────────────────────────────
    emit_symbolic_accounts_module(&mut out, spec, emit_targets, fw.label)?;

    // ── Per-handler proof harnesses ─────────────────────────────────────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Impl-targeted ensures-preservation proofs\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let mut emitted_count = 0;
    for handler in emit_targets {
        for (idx, ensures) in handler.ensures.iter().enumerate() {
            emit_handler_harness(&mut out, handler, idx, ensures, spec)?;
            emitted_count += 1;
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    crate::codegen_shared::write_generated_file(output_path, &out)?;

    eprintln!(
        "Generated {} impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );

    Ok(())
}
