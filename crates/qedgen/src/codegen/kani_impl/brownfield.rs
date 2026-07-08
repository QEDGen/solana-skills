use super::*;

/// Emit the brownfield-Anchor impl-targeted harness (#162): a **state-struct**
/// harness per `(handler, ensures)` instead of the greenfield `Accounts`
/// context + `accounts.handler(param)` shape. The Context shape only matches
/// qedgen-generated code; a real Anchor program shares one Accounts struct
/// across handlers, takes `Context<T>` + an `Args` struct, and exposes
/// handlers as associated fns — none of which the Context harness resolves
/// against. The state-struct harness (symbolic state → apply the real effect
/// / call the real `invariant()`/helper → assert `ensures`) is the shape that
/// actually works, validated by the two bundled brownfield harnesses.
///
/// Construction is generated from the spec's State when it fully mirrors the
/// real `#[account]` struct (every field a scalar / `Pubkey` / `Option` / `Vec`
/// / nested record / enum sum-type — see `state_ctor`); the harness then calls
/// the emitted `symbolic_<struct>()` and only the effect + validity gate stays
/// agent-fill. When the State can't be fully constructed (an imported/unresolved
/// type or a `Map` field), construction falls back to an agent-fill `todo!()`.
/// The scaffolding around both — snapshot set (incl. the read-only-field fix),
/// requires-assume, ensures-assert, unwind hint — is always generated.
pub(crate) fn emit_kani_impl_anchor_brownfield(
    spec: &ParsedSpec,
    output_path: &Path,
    emit_targets: &[&ParsedHandler],
    explicit_flag: bool,
) -> Result<()> {
    let fp = crate::fingerprint::compute_fingerprint(spec);

    let mut out = String::new();

    out.push_str(&crate::codegen_shared::marker_unlabeled(
        &fp,
        "tests/kani_impl.rs",
    ));
    out.push_str("//\n");
    out.push_str("// Impl-targeted Kani harnesses — BROWNFIELD Anchor (#162). Verifies the\n");
    out.push_str("// user's REAL state logic against a symbolic state-account struct, rather\n");
    out.push_str("// than a synthetic `Accounts` context. Use this shape when the program\n");
    out.push_str("// pre-exists qedgen (shared Accounts structs, `Context<T>` + `Args`,\n");
    out.push_str("// associated-fn handlers) — the greenfield `accounts.handler(...)` shape\n");
    out.push_str("// does not resolve against it.\n");
    out.push_str("//\n");
    out.push_str("// Construction is generated from the spec's State when it fully mirrors\n");
    out.push_str("// the real `#[account]` struct (a `symbolic_<struct>()` ctor below);\n");
    out.push_str("// otherwise it falls back to an agent-fill `todo!()`. Applying the real\n");
    out.push_str("// effect + validity gate is always agent-fill. The snapshot / assume /\n");
    out.push_str("// assert scaffolding around them is generated and correct.\n");
    out.push_str("//\n");
    out.push_str("// PLACEMENT: this file must live INSIDE the program crate (e.g.\n");
    out.push_str("// `src/kani_impl.rs` + `#[cfg(kani)] mod kani_impl;` in lib.rs) — a\n");
    out.push_str("// standalone harness crate hits cargo dependency-hell (spl-token-2022 vs\n");
    out.push_str("// solana-program skew). See docs/toolchain-backlog.md G3.\n");
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered (a handler declares `modifies` fields absent from its\n");
        out.push_str("// `effect` block). Pass `--kani-impl` to force emission for every\n");
        out.push_str("// handler with `ensures`.\n");
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani -Z stubbing --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n\n");

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Impl-targeted ensures-preservation proofs (brownfield state-struct)\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    // Symbolic-state constructor — emitted ONCE per file when the spec's State
    // is fully constructible (every field a scalar / Pubkey / Option / Vec /
    // nested record). Each harness then calls it instead of a construction
    // `todo!()`. When the State isn't fully constructible (a bare enum / Map
    // field, or an ambiguous multi-account spec), `state_struct` stays `None`
    // and the harnesses fall back to the agent-fill `todo!()`.
    let ctor_ctx = super::state_ctor::CtorCtx::from_spec(spec);
    let state_struct: Option<String> = match super::state_ctor::resolve_state_struct(spec) {
        Some((name, fields)) => match super::state_ctor::emit_state_ctor(name, fields, &ctor_ctx) {
            Some(ctor) => {
                out.push_str(&ctor);
                out.push('\n');
                Some(name.to_string())
            }
            None => None,
        },
        None => None,
    };

    let mut emitted_count = 0;
    for handler in emit_targets {
        for (idx, ensures) in handler.ensures.iter().enumerate() {
            emit_brownfield_handler_harness(
                &mut out,
                handler,
                idx,
                ensures,
                spec,
                state_struct.as_deref(),
            )?;
            emitted_count += 1;
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    crate::codegen_shared::write_generated_file(output_path, &out)?;

    eprintln!(
        "Generated {} brownfield impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );

    Ok(())
}
