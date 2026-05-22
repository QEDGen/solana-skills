use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedProperty, ParsedSpec};
use crate::codegen::map_type;
use crate::rust_codegen_util;

/// Per-section harness counts, accumulated across single-mode or per-ADT
/// emission. The summary footer in `generate()` reads from this to print
/// totals matching the actual emitted harness count.
#[derive(Default)]
struct HarnessCounts {
    guard: usize,
    prop: usize,
    invariant: usize,
    effect: usize,
    overflow: usize,
    abort: usize,
}

/// Emit `let mut s = State { ... };` with every mutable field bound to
/// `kani::any()`. When the per-account lifecycle has ≥2 states, the
/// synthetic `status` field is also `kani::any()` so callers can layer
/// `kani::assume(s.status == Status::<X>)` on top.
fn emit_state_init_symbolic(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
) {
    out.push_str("    let mut s = State {\n");
    for (fname, _) in mutable_fields {
        out.push_str(&format!("        {}: kani::any(),\n", fname));
    }
    if lifecycle_states.len() >= 2 {
        out.push_str("        status: kani::any(),\n");
    }
    out.push_str("    };\n");
}

/// Emit `let mut s = State { ... };` with every mutable field zeroed and the
/// `status` field set to the section's initial lifecycle state. Used by init-
/// handler harnesses (effect/preservation), where the pre-state is the
/// canonical "before initialization" state.
///
/// Defaults are type-aware via `proptest_gen::default_value_for_field`:
/// `Map[N] T` → `[<inner>; N]`, named records → `<Name>::default()`, named
/// sums with a zero-payload variant → that variant; primitives → `0`. A
/// `None` return means "no sensible default" — skip the field and let
/// rustc surface the missing field with E0063, which is a clearer
/// diagnostic than emitting wrong-type code.
///
/// Same shape (and same helper) as the seed-state init fix landed in
/// `fix(proptest_gen): type-aware seed-state init for arrays + lifecycle
/// status` (#45). Without this, every array-typed state field literals
/// to `{}: 0` and the harness fails to compile:
///
///     error[E0308]: mismatched types
///        --> tests/kani.rs:N:M
///         |
///       N |         rfp_milestone_amounts: 0,
///         |                                ^ expected `[u64; 8]`, found integer
fn emit_state_init_zeroed(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
) {
    out.push_str("    let mut s = State {\n");
    for (fname, ftype) in mutable_fields {
        if let Some(default) = crate::proptest_gen::default_value_for_field(ftype, spec) {
            out.push_str(&format!("        {}: {},\n", fname, default));
        }
    }
    if let Some(initial) = lifecycle_states.first() {
        if lifecycle_states.len() >= 2 {
            out.push_str(&format!("        status: Status::{},\n", initial));
        }
    }
    out.push_str("    };\n");
}

/// Append `kani::assume(s.status == Status::<pre>);` when the handler has a
/// pre-status declaration AND this section has a lifecycle. No-op otherwise.
/// Without this, guard-rejection / abort harnesses for lifecycle-gated
/// handlers can pass for the wrong reason — the handler rejects because the
/// symbolic status didn't match the pre-state, not because the requires/
/// guard fired.
fn emit_pre_status_assume(out: &mut String, op: &ParsedHandler, lifecycle_states: &[String]) {
    if lifecycle_states.len() < 2 {
        return;
    }
    if let Some(ref pre) = op.pre_status {
        out.push_str(&format!("    kani::assume(s.status == Status::{});\n", pre));
    }
}

/// Generate Kani proof harnesses from a spec file (.lean or .qedspec).
///
/// Produces self-contained proofs that model state transitions from the spec
/// and verify properties using Kani bounded model checking — no framework deps.
///
/// v2.21 Pair A — multi-ADT support: when `spec.account_types.len() > 1`,
/// emit one `mod <name> { ... }` per account type wrapping its State struct,
/// transition fns, and proof harnesses. Mirrors proptest_gen's per-account
/// dispatch (see `proptest_gen::emit_account_section`). Single-ADT specs keep
/// the original flat output unchanged.
pub fn generate(spec_path: &Path, output_path: &Path) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No operations found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    rust_codegen_util::check_effect_targets(&spec)?;

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let hash = fp
        .file_hashes
        .get("tests/kani.rs")
        .cloned()
        .unwrap_or_default();

    let is_multi = spec.account_types.len() > 1;

    let mut out = String::new();

    // ── File header ──────────────────────────────────────────────────────
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

    // ── Math helpers (mirrors proptest_gen) ─────────────────────────────
    // The standalone kani harness lives at `programs/<prog>/tests/kani.rs`
    // (no `pub use crate::math::*`) — generated under `qedgen codegen
    // --kani` against an existing program crate that hasn't been emitted
    // via `--all`. In that case `src/math.rs` is never (re)generated, so
    // any `mul_div_floor_u128` / `mul_div_ceil_u128` calls emitted by
    // `chumsky_adapter::expr_to_rust` have no definition in scope:
    //
    //     error[E0425]: cannot find function `mul_div_floor_u128`
    //                   in this scope
    //
    // Same mismatch + same fix shape as
    // `fix(proptest_gen): inline mul_div helpers when standalone proptest
    // needs them` (#45). Inline the canonical bodies here too, gated by
    // the same `guards_use_math_helpers` predicate so we ship the helpers
    // ONLY when the spec actually calls into them — otherwise we'd ship
    // two sources of truth for the helpers (kani.rs + math.rs) with the
    // silent-divergence risk that implies for any future change.
    if crate::codegen::guards_use_math_helpers(&spec) {
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

    // ── State model header ───────────────────────────────────────────────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// State model (derived from qedspec — no framework dependencies)\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    // Constants are file-scoped — referenced from inside per-ADT modules
    // via `use super::*` so duplicating them per module is wasted bytes.
    rust_codegen_util::emit_constants(&mut out, &spec.constants);

    let mut counts = HarnessCounts::default();

    if is_multi {
        // Multi-ADT: one `mod <lowercase_name> { ... }` per account type,
        // each with its own State struct + harnesses. Mirrors
        // `proptest_gen::emit_account_section` at proptest_gen.rs:540.
        for acct in &spec.account_types {
            let acct_fields = rust_codegen_util::mutable_fields(&acct.fields);
            if acct_fields.is_empty() {
                continue;
            }
            let acct_handlers: Vec<&ParsedHandler> = spec
                .handlers
                .iter()
                .filter(|h| h.on_account.as_deref() == Some(acct.name.as_str()))
                .collect();
            if acct_handlers.is_empty() {
                continue;
            }
            // Filter properties to those whose expression references at least
            // one field declared on THIS account type. Same heuristic as
            // proptest_gen.rs:489-491.
            let acct_field_names: Vec<&str> = acct_fields.iter().map(|(n, _)| n.as_str()).collect();
            let acct_props: Vec<&ParsedProperty> = spec
                .properties
                .iter()
                .filter(|p| {
                    if let Some(ref expr) = p.expression {
                        acct_field_names.iter().any(|f| expr.contains(f))
                    } else {
                        false
                    }
                })
                .collect();

            let mod_name = acct.name.to_lowercase();
            out.push_str(&format!("mod {} {{\n", mod_name));
            out.push_str("    use super::*;\n\n");
            emit_kani_account_section(
                &mut out,
                &acct_fields,
                &acct_handlers,
                &acct_props,
                &acct.lifecycle,
                &spec,
                &mut counts,
            )?;
            out.push_str(&format!("}} // mod {}\n\n", mod_name));
        }
    } else {
        // Single-ADT: flat layout, identical to pre-v2.21 output.
        // When the spec declares exactly one account type, use its fields
        // and its lifecycle; otherwise fall back to the flat `state_fields`
        // + spec-level lifecycle list.
        let (state_fields, lifecycle): (&[(String, String)], &[String]) =
            if spec.account_types.len() == 1 {
                (
                    &spec.account_types[0].fields,
                    spec.account_types[0].lifecycle.as_slice(),
                )
            } else {
                (
                    rust_codegen_util::resolve_state_fields(&spec),
                    spec.lifecycle_states.as_slice(),
                )
            };
        let mutable = rust_codegen_util::mutable_fields(state_fields);
        let all_handlers: Vec<&ParsedHandler> = spec.handlers.iter().collect();
        let all_props: Vec<&ParsedProperty> = spec.properties.iter().collect();
        emit_kani_account_section(
            &mut out,
            &mutable,
            &all_handlers,
            &all_props,
            lifecycle,
            &spec,
            &mut counts,
        )?;
    }

    // ── File-level features (single-ADT mode only) ──────────────────────
    // Covers, liveness, and environment harnesses reference the per-ADT
    // State struct + transition fns directly. In multi-ADT mode, those live
    // inside per-account modules, so a top-level harness can't see them
    // without explicit qualification. Per-ADT cover/liveness/env emission
    // is v2.22 scope; for v2.21 we skip these in multi-mode (proptest_gen
    // does the same). Single-mode behavior is unchanged.
    if !is_multi {
        let (mutable_fields_view, file_lifecycle): (Vec<&(String, String)>, &[String]) =
            if spec.account_types.len() == 1 {
                (
                    rust_codegen_util::mutable_fields(&spec.account_types[0].fields),
                    spec.account_types[0].lifecycle.as_slice(),
                )
            } else {
                (
                    rust_codegen_util::mutable_fields(rust_codegen_util::resolve_state_fields(
                        &spec,
                    )),
                    spec.lifecycle_states.as_slice(),
                )
            };
        emit_file_level_features(&mut out, &spec, &mutable_fields_view, file_lifecycle)?;
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    std::fs::write(output_path, &out)?;

    // ── Summary ──────────────────────────────────────────────────────────
    let HarnessCounts {
        guard: guard_count,
        prop: prop_count,
        invariant: invariant_count,
        effect: effect_count,
        overflow: overflow_count,
        abort: abort_count,
    } = counts;
    let total =
        guard_count + prop_count + invariant_count + effect_count + overflow_count + abort_count;

    eprintln!(
        "Generated {} Kani harnesses in {}",
        total,
        output_path.display()
    );
    if guard_count > 0 {
        eprintln!("  {} guard enforcement proof(s)", guard_count);
    }
    if prop_count > 0 {
        eprintln!("  {} property preservation proof(s)", prop_count);
    }
    if invariant_count > 0 {
        eprintln!("  {} invariant preservation proof(s)", invariant_count);
    }
    if effect_count > 0 {
        eprintln!("  {} effect conformance proof(s)", effect_count);
    }
    if overflow_count > 0 {
        eprintln!("  {} overflow detection proof(s)", overflow_count);
    }
    if abort_count > 0 {
        eprintln!("  {} abort condition proof(s)", abort_count);
    }

    Ok(())
}

/// Emit the per-account section: State struct, property/invariant predicates,
/// transition functions, and every proof harness whose body references the
/// per-account `s: &State`. Called once for single-ADT specs (flat) or once
/// per `account_types` entry for multi-ADT specs (wrapped in `mod <name>`).
///
/// `handlers` is the filtered handler list for this section (all handlers in
/// single mode; `op.on_account == Some(acct.name)` in multi mode).
/// `properties` is the filtered property list (all in single mode;
/// expression-references-a-field-of-this-ADT in multi mode).
fn emit_kani_account_section(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    handlers: &[&ParsedHandler],
    properties: &[&ParsedProperty],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
    counts: &mut HarnessCounts,
) -> Result<()> {
    let has_lifecycle = lifecycle_states.len() >= 2;

    // User-defined records/enums referenced by the State struct must be
    // declared first. `#![cfg(kani)]` at the top of this file lets us derive
    // Kani's Arbitrary trait unconditionally — generated Rust only compiles
    // under Kani anyway. Records, unit enums, and the Status enum live
    // *inside* this section so multi-ADT mode wraps each set in its own
    // `mod <name>` namespace (mirrors proptest_gen::emit_account_section).
    // The Status enum is built from the *per-account* `lifecycle_states` —
    // not the spec-level one — so an ADT with its own variant names (e.g.
    // `Loan { Empty, Active, Liquidated }`) gets the right enum even when
    // another ADT in the same spec has different variants.
    rust_codegen_util::emit_record_structs(out, spec, "Clone, Copy, kani::Arbitrary", |t| {
        map_type(t, spec)
    })?;
    rust_codegen_util::emit_unit_enum_sums(
        out,
        spec,
        "Clone, Copy, PartialEq, Eq, kani::Arbitrary",
    )?;
    rust_codegen_util::emit_lifecycle_status_enum_from(
        out,
        lifecycle_states,
        "Clone, Copy, PartialEq, Eq, kani::Arbitrary",
    );

    rust_codegen_util::emit_state_struct_with_lifecycle(
        out,
        mutable_fields,
        "Clone, Copy",
        |t| map_type(t, spec),
        has_lifecycle,
    )?;

    // ── Property predicates ──────────────────────────────────────────────
    if !properties.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Property predicates (from qedspec `property` declarations)\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        // `emit_property_predicates_with` takes &[ParsedProperty] (not &[&_]),
        // so reconstruct an owned Vec view of the filtered slice.
        let owned_props: Vec<crate::check::ParsedProperty> =
            properties.iter().map(|p| (*p).clone()).collect();
        rust_codegen_util::emit_property_predicates_with(out, &owned_props, false, |t| {
            map_type(t, spec)
        });
    }

    // ── Invariant predicates ─────────────────────────────────────────────
    // Filter to invariants linked from at least one handler in THIS section
    // (in multi mode, this restricts to invariants the per-ADT handlers
    // claim to preserve/establish; in single mode it's identical to the
    // pre-v2.21 spec-wide filter).
    let linked_invs: Vec<&crate::check::ParsedInvariant> = spec
        .invariants
        .iter()
        .filter(|i| {
            handlers
                .iter()
                .any(|h| h.invariants.contains(&i.name) || h.establishes.contains(&i.name))
        })
        .collect();
    if !linked_invs.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Invariant predicates (from qedspec `invariant` declarations linked via\n");
        out.push_str(
            "// handler-side `invariant Name` clauses). v2.17.x wires ParsedInvariant.rust_expr\n",
        );
        out.push_str("// through to per-(handler, invariant) BMC preservation harnesses below.\n");
        out.push_str(
            "// ============================================================================\n\n",
        );
        rust_codegen_util::emit_invariant_predicates(out, &linked_invs);
    }

    // ── Transition functions ─────────────────────────────────────────────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Transition functions (from qedspec operations — effects + guards)\n");
    out.push_str("//\n");
    out.push_str("// Each returns true if the guard passes and the transition fires,\n");
    out.push_str("// false if the guard rejects the operation.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in handlers {
        rust_codegen_util::emit_transition_fn(out, op, spec, false, |t| map_type(t, spec))?;
    }

    // v2.25 — ref_impl bodies, emitted as Rust fns so ensures-preservation
    // harnesses can call them at assertion sites. Pure expressions, no
    // state mutation — render directly from `rust_body`.
    if !spec.ref_impls.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Reference implementations (from qedspec ref_impl declarations).\n");
        out.push_str(
            "// ============================================================================\n\n",
        );
        for r in &spec.ref_impls {
            let params = r
                .params
                .iter()
                .map(|(n, t)| format!("{}: {}", n, map_type(t, spec).unwrap_or_else(|_| t.clone())))
                .collect::<Vec<_>>()
                .join(", ");
            let ret = map_type(&r.return_type, spec).unwrap_or_else(|_| r.return_type.clone());
            out.push_str(&format!(
                "fn {}({}) -> {} {{\n    {}\n}}\n\n",
                r.name, params, ret, r.rust_body
            ));
        }
    }

    // ── Guard enforcement proofs ─────────────────────────────────────────
    let guard_ops: Vec<&ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|op| op.has_guard())
        .collect();
    if !guard_ops.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Guard enforcement — transitions reject invalid inputs\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for op in &guard_ops {
            // Roll `guard_str` AND every `requires` clause into a single
            // expression. Previously we took `guard_str.unwrap_or("true")`,
            // which silently emitted `kani::assume(!(true))` — an impossible
            // precondition — whenever a handler had only `requires` clauses
            // and no top-level `guard`. That made the harness pass vacuously
            // and hid real rejection-path bugs.
            let Some(full_guard) = rust_codegen_util::collect_full_guard(op, false) else {
                // No guard, no requires → nothing to reject. Skip instead of
                // emitting a vacuous harness that would always pass.
                continue;
            };

            out.push_str("#[kani::proof]\n");
            out.push_str("#[kani::unwind(2)]\n");
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!("fn verify_{}_rejects_invalid() {{\n", op.name));

            emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
            emit_pre_status_assume(out, op, lifecycle_states);

            // Symbolic params
            for (pname, ptype) in &op.takes_params {
                out.push_str(&format!(
                    "    let {}: {} = kani::any();\n",
                    pname,
                    map_type(ptype, spec)?
                ));
            }

            // Assume at least one guard component is violated. For a
            // conjunction `g1 && g2 && ... && gN` the negation is
            // `!(g1 && ... && gN)`, which is what we want the harness to
            // exhaustively cover.
            out.push_str(&format!("    kani::assume(!({full_guard}));\n"));

            // Assert rejection
            let args: String = op
                .takes_params
                .iter()
                .map(|(n, _)| format!(", {}", n))
                .collect();
            out.push_str(&format!("    assert!(!{}(&mut s{}),\n", op.name, args));
            out.push_str(&format!(
                "        \"{} must reject when guard is violated\");\n",
                op.name
            ));
            out.push_str("}\n\n");
            counts.guard += 1;
        }
    }

    // ── Abort condition proofs ────────────────────────────────────────────
    let abort_ops: Vec<&ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|op| !op.aborts_if.is_empty())
        .collect();
    if !abort_ops.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Abort conditions — operations must reject under specified conditions\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for op in &abort_ops {
            for abort in &op.aborts_if {
                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!(
                    "fn verify_{}_aborts_if_{}() {{\n",
                    op.name, abort.error_name
                ));

                emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
                emit_pre_status_assume(out, op, lifecycle_states);

                // Symbolic params
                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype, spec)?
                    ));
                }

                // Assume abort condition
                out.push_str(&format!("    kani::assume({});\n", abort.rust_expr));

                // Assert rejection
                let args: String = op
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!(", {}", n))
                    .collect();
                out.push_str(&format!("    assert!(!{}(&mut s{}),\n", op.name, args));
                out.push_str(&format!(
                    "        \"{} must abort with {}\");\n",
                    op.name, abort.error_name
                ));
                out.push_str("}\n\n");
                counts.abort += 1;
            }
        }
    }

    // ── Property preservation proofs ─────────────────────────────────────
    if !properties.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Property preservation — invariants hold through all transitions\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for prop in properties {
            if prop.expression.is_none() {
                continue;
            }

            for op_name in &prop.preserved_by {
                // In multi-ADT mode, only emit when the preserving handler
                // belongs to this section. Skipping otherwise keeps the
                // harness call valid (no cross-module fn reference) and
                // avoids duplicate emission across modules.
                let Some(op) = handlers.iter().copied().find(|o| &o.name == op_name) else {
                    continue;
                };

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!(
                    "fn verify_{}_preserves_{}() {{\n",
                    op_name, prop.name
                ));

                // Determine if this is an initializing operation
                let is_init = op.pre_status.as_deref() == Some("Uninitialized");

                // v2.20 §S1.1: for `forall <binder> : <ty>, body preserved_by
                // <op>`, bind <binder> symbolically and drive the check via
                // `<prop>_at(&s, <binder>)`. When the handler already takes a
                // matching `<binder>` as a param, skip the local binding —
                // the symbolic param binding below shadows it and unifies
                // the value pre and post.
                let handler_takes_binder = match &prop.per_slot {
                    Some(slot) => op
                        .takes_params
                        .iter()
                        .any(|(n, t)| n == &slot.binder_name && t == &slot.binder_type),
                    _ => false,
                };
                let needs_local_binder = prop.per_slot.is_some() && !handler_takes_binder;

                // v2.23 Slice 4: produce `pre` (symbolic or zeroed) and
                // `let mut post = pre;`. The handler mutates `post`; the
                // preservation assertion compares to `pre` for Binary
                // properties (those containing `old(...)`) and to `post`
                // alone for Unary properties. Pre-v2.23 the Kani harness
                // used a single `s` that was mutated in place, mirroring
                // the proptest bug — every `old(...)`-bearing property
                // verified vacuously because the temporal marker had
                // been overwritten before the assertion fired. Open
                // question 3 (PRD-v2.23): we assume `State: Copy` on the
                // Kani path; this is true for every shipping spec today
                // and is documented as a Kani-side limitation.
                if is_init {
                    // Init handler — pre-state is the zeroed initial state.
                    // `let pre = State { ... }; let mut post = pre;` so the
                    // shape matches the proptest init path.
                    out.push_str("    let pre = ");
                    // Emit the literal struct body inline so we can name the
                    // binding `pre` instead of `s`. Same fields/values as
                    // `emit_state_init_zeroed`.
                    out.push_str("State {\n");
                    for (fname, ftype) in mutable_fields {
                        if let Some(default) =
                            crate::proptest_gen::default_value_for_field(ftype, spec)
                        {
                            out.push_str(&format!("        {}: {},\n", fname, default));
                        }
                    }
                    if let Some(initial) = lifecycle_states.first() {
                        if lifecycle_states.len() >= 2 {
                            out.push_str(&format!("        status: Status::{},\n", initial));
                        }
                    }
                    out.push_str("    };\n");
                    out.push_str("    let mut post = pre;\n");
                } else {
                    // Non-init handler — pre is symbolic; assumptions apply
                    // to pre; post is a Copy of pre before mutation.
                    out.push_str("    let pre = State {\n");
                    for (fname, _) in mutable_fields {
                        out.push_str(&format!("        {}: kani::any(),\n", fname));
                    }
                    if lifecycle_states.len() >= 2 {
                        out.push_str("        status: kani::any(),\n");
                    }
                    out.push_str("    };\n");
                    if lifecycle_states.len() >= 2 {
                        if let Some(ref pre_status) = op.pre_status {
                            out.push_str(&format!(
                                "    kani::assume(pre.status == Status::{});\n",
                                pre_status
                            ));
                        }
                    }

                    // Bind <binder> symbolically up front so the pre-state
                    // assume and the post-state assert reference the same
                    // value. Same binder pre & post = preservation.
                    if needs_local_binder {
                        if let Some(slot) = &prop.per_slot {
                            let rust_ty = map_type(&slot.binder_type, spec)?;
                            out.push_str(&format!(
                                "    let {}: {} = kani::any();\n",
                                slot.binder_name, rust_ty
                            ));
                        }
                    }

                    // v2.23 Slice 4: assume all declared (unary) properties
                    // hold before the transition. Binary properties have a
                    // `(pre, post)` signature with no single-state form —
                    // asserting them against `(pre, pre)` is trivially true
                    // and offers no information. Skip them in the assume
                    // loop; the post-assert below dispatches by class.
                    for pre_prop in properties.iter().copied() {
                        if pre_prop.expression.is_none() {
                            continue;
                        }
                        if pre_prop.class == crate::check::PropertyClass::Binary {
                            continue;
                        }
                        match &pre_prop.per_slot {
                            Some(slot) if pre_prop.name == prop.name => {
                                out.push_str(&format!(
                                    "    kani::assume({}_at(&pre, {}));\n",
                                    pre_prop.name, slot.binder_name
                                ));
                            }
                            _ => {
                                out.push_str(&format!(
                                    "    kani::assume({}(&pre));\n",
                                    pre_prop.name
                                ));
                            }
                        }
                    }

                    // Assume MAX_MEMBERS bound (derived from create_vault guard)
                    if !spec.constants.is_empty() {
                        for (cname, _cval) in &spec.constants {
                            let upper = cname.to_uppercase();
                            if upper.contains("MAX") || upper.contains("MEMBER") {
                                if mutable_fields.iter().any(|(f, _)| f == "member_count") {
                                    out.push_str(&format!(
                                        "    kani::assume(pre.member_count <= {});\n",
                                        upper
                                    ));
                                }
                                break;
                            }
                        }
                    }

                    // post = Copy of pre; handler mutates post.
                    out.push_str("    let mut post = pre;\n");
                }

                // Symbolic params
                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype, spec)?
                    ));
                }

                // For operations that increment a field (add effect), assume
                // the field is strictly less than its bound to prevent overflow.
                // v2.23 Slice 4: bounds apply to pre-state.
                let owned_props: Vec<crate::check::ParsedProperty> =
                    properties.iter().map(|p| (*p).clone()).collect();
                rust_codegen_util::emit_add_strict_bounds(out, op, &owned_props, "    kani::assume(pre.{field} < pre.{bound}); // strict bound: {field} increments\n");

                // Call transition (mutates `post`) and assert property.
                // v2.23 Slice 4 dispatch on `prop.class`:
                //   - Unary, per_slot: `<prop>_at(&post, binder)`.
                //   - Unary, plain:    `<prop>(&post)`.
                //   - Binary:          `<prop>(&pre, &post)` — the binary
                //     signature emitted by `emit_property_predicates_with`.
                // Per_slot × Binary is deferred per PRD open question 4;
                // such a property falls through to the plain binary form.
                let args: String = op
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!(", {}", n))
                    .collect();
                out.push_str(&format!("    if {}(&mut post{}) {{\n", op_name, args));
                let is_binary_prop = prop.class == crate::check::PropertyClass::Binary;
                if is_binary_prop {
                    out.push_str(&format!("        assert!({}(&pre, &post),\n", prop.name));
                    out.push_str(&format!(
                        "            \"{} must hold after {} (binary: pre/post)\");\n",
                        prop.name, op_name
                    ));
                } else {
                    match &prop.per_slot {
                        Some(slot) => {
                            out.push_str(&format!(
                                "        assert!({}_at(&post, {}),\n",
                                prop.name, slot.binder_name
                            ));
                            out.push_str(&format!(
                                "            \"{} must hold after {} (forall {} : {})\");\n",
                                prop.name, op_name, slot.binder_name, slot.binder_type
                            ));
                        }
                        None => {
                            out.push_str(&format!("        assert!({}(&post),\n", prop.name));
                            out.push_str(&format!(
                                "            \"{} must hold after {}\");\n",
                                prop.name, op_name
                            ));
                        }
                    }
                }
                out.push_str("    }\n");
                out.push_str("}\n\n");
                counts.prop += 1;
            }
        }
    }

    // ── Ensures preservation proofs (v2.25 Phase B) ──────────────────────
    // For each handler that carries `ensures <expr>` clauses, emit a BMC
    // harness that:
    //   1. Initializes symbolic pre-state (matches property-preservation shape)
    //   2. Snapshots pre = s.clone() before running the transition
    //   3. Calls the spec-translated transition (mutates s into post-state)
    //   4. Asserts each ensures clause against (pre, s) using the
    //      `rust_expr_binary` rendering (`old(state.x)` → `pre.x`,
    //      bare `state.x` → `post.x`).
    //
    // This catches spec-internal inconsistency between the effect block
    // and the ensures contract. If `modifies` declares a field as
    // mutable but the effect block doesn't write it (the LP-math
    // pattern), the transition leaves the field equal to pre — Kani
    // surfaces a counterexample where the ensures fails for any input
    // that exercises the missing math. That counterexample IS the
    // signal that the user's Rust impl needs to fill the modifies-fill
    // todo!() site to satisfy the contract.
    //
    // v2.26+ extends this to call the user's REAL Rust handler (bridged
    // through the integration-test account builder). v2.25 ships the
    // spec-model variant first because it requires no framework-specific
    // wiring and surfaces the contract gap immediately.
    let handlers_with_ensures: Vec<&ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|h| !h.ensures.is_empty())
        .collect();
    if !handlers_with_ensures.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Ensures preservation — `ensures <expr>` clauses verified against\n");
        out.push_str("// (pre, post) of the spec-translated transition. Counterexamples here\n");
        out.push_str("// indicate the spec's effect block doesn't satisfy its own ensures —\n");
        out.push_str("// usually because the math lives in the user's Rust impl, behind a\n");
        out.push_str("// `modifies`-driven todo!() fill site. See SKILL.md §ref_impl.\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for op in handlers_with_ensures {
            for (idx, ensures) in op.ensures.iter().enumerate() {
                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!("fn verify_{}_ensures_{}() {{\n", op.name, idx));

                emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
                emit_pre_status_assume(out, op, lifecycle_states);

                // Symbolic params declared first so any subsequent
                // `kani::assume` referencing them resolves.
                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype, spec)?
                    ));
                }

                // Assume the handler's `requires` guards hold pre-state.
                // Otherwise the transition would reject the input and the
                // ensures clause wouldn't be exercised — vacuous pass.
                if let Some(full_guard) = rust_codegen_util::collect_full_guard(op, false) {
                    out.push_str(&format!("    kani::assume({});\n", full_guard));
                }

                // Snapshot pre-state AFTER assumes so the snapshot
                // reflects the constrained pre-state Kani is exploring.
                out.push_str("    let pre = s.clone();\n");

                // Call the spec-translated transition (mutates s).
                let args: String = op
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!(", {}", n))
                    .collect();
                out.push_str(&format!("    if {}(&mut s{}) {{\n", op.name, args));

                // Post-state is the mutated `s`. Bind `post = &s` so the
                // ensures rendering's `post.x` paths resolve.
                out.push_str("        let post = &s;\n");
                out.push_str(&format!("        assert!({},\n", ensures.rust_expr_binary));
                out.push_str(&format!(
                    "            \"ensures clause {} on {} violated by spec-translated transition\");\n",
                    idx, op.name
                ));
                out.push_str("    }\n");
                out.push_str("}\n\n");
                counts.prop += 1;
            }
        }
    }

    // ── Invariant preservation proofs ────────────────────────────────────
    // For each handler that carries `invariant Name` in its clause list,
    // emit a BMC harness that asserts the invariant holds post-transition
    // when it held pre-transition. Same shape as the property-preservation
    // loop above but iterates the join from the handler side (where
    // ParsedHandler.invariants stores the relationship).
    if !linked_invs.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str(
            "// Invariant preservation — `invariant Name` on a handler asserts the named\n",
        );
        out.push_str("// top-level invariant holds before AND after the handler runs. Each pair\n");
        out.push_str("// becomes its own BMC proof.\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for op in handlers.iter().copied() {
            // Walk both `invariant Name` (preserves) and `establishes Name`
            // clauses; the bool is_establish controls whether to assume the
            // invariant pre-state. Establish skips the pre-assume so the
            // harness checks "after this handler runs, X holds" regardless
            // of pre-state. Preserves wraps the pre-state in an assume so
            // BMC starts in a state where X already holds.
            let pairs: Vec<(&String, bool)> = op
                .invariants
                .iter()
                .map(|n| (n, false))
                .chain(op.establishes.iter().map(|n| (n, true)))
                .collect();
            for (inv_name, is_establish) in pairs {
                let Some(inv) = linked_invs.iter().find(|i| &i.name == inv_name) else {
                    continue;
                };
                if inv
                    .rust_expr
                    .as_deref()
                    .map(crate::check::rust_expr_is_unsupported)
                    .unwrap_or(true)
                {
                    continue;
                }
                let is_init = op.pre_status.as_deref() == Some("Uninitialized");

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                let verb = if is_establish {
                    "establishes"
                } else {
                    "preserves"
                };
                out.push_str(&format!(
                    "fn verify_{}_{}_{}() {{\n",
                    op.name, verb, inv.name
                ));

                if is_init {
                    emit_state_init_zeroed(out, mutable_fields, lifecycle_states, spec);
                } else {
                    emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
                    emit_pre_status_assume(out, op, lifecycle_states);
                    if !is_establish {
                        out.push_str(&format!("    kani::assume({}(&s));\n", inv.name));
                    }
                }

                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype, spec)?
                    ));
                }

                let args: String = op
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!(", {}", n))
                    .collect();
                out.push_str(&format!("    if {}(&mut s{}) {{\n", op.name, args));
                out.push_str(&format!("        assert!({}(&s),\n", inv.name));
                out.push_str(&format!(
                    "            \"invariant {} must hold after {}\");\n",
                    inv.name, op.name
                ));
                out.push_str("    }\n");
                out.push_str("}\n\n");
                counts.invariant += 1;
            }
        }
    }

    // ── Effect conformance proofs ─────────────────────────────────────────
    let effect_ops: Vec<&ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|op| op.has_effect())
        .collect();
    if !effect_ops.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Effect conformance — verify transition effects match spec\n");
        out.push_str("//\n");
        out.push_str(
            "// Each proof applies a transition to symbolic state and checks that every\n",
        );
        out.push_str("// field changed/unchanged matches the spec's effect: declarations.\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        // B11 v2.6: split effect conformance into PER-FIELD harnesses — one
        // proof per (handler, field) pair — so a single stuck mul/div field
        // doesn't block verification of its siblings. Solver choice per
        // harness is delegated to `pick_kani_solver`, which tiers:
        //   * cadical     — scalar / linear (default)
        //   * minisat     — narrow-type (u8/u16/u32) mul/div
        //   * bin="z3"    — wide-type (u64/u128/i128) mul/div, e.g. the
        //                   `amount * 125 / 10000 * N / 10000` pattern
        //
        // Pre-v2.6 a single `verify_X_effects` harness combined every field's
        // assertion — `verify_buy_side_a_effects` took 20+ min on a 5×mul/div
        // effect body. Per-field + tiered solver drops wide-arith harnesses
        // from >17 min (minisat-stuck) to seconds, and failures on one field
        // don't hide the rest.
        let field_type_lookup: std::collections::HashMap<&str, &str> = mutable_fields
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        for op in &effect_ops {
            let is_init = op.pre_status.as_deref() == Some("Uninitialized");

            for (field, op_kind, value) in &op.effects {
                // Skip effects targeting fields that aren't in the per-ADT
                // Kani State model. `mutable_fields` only contains this
                // section's fields, so an effect like
                // `initializer_token_account := initializer_ta.pubkey`
                // can't be asserted against — the field doesn't exist on
                // this State, and the RHS references an unbound account
                // binding. In multi-ADT mode, this also skips effects that
                // target fields belonging to a DIFFERENT account type's State.
                let base = rust_codegen_util::effect_target_base(field);
                if !field_type_lookup.contains_key(base) {
                    continue;
                }

                let field_type = field_type_lookup.get(field.as_str()).copied().unwrap_or("");
                let solver = rust_codegen_util::pick_kani_solver_for_effect(field_type, value, op);

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str(&format!("#[kani::solver({})]\n", solver));
                out.push_str(&format!(
                    "fn verify_{}_effect_{}() {{\n",
                    op.name,
                    crate::codegen::sanitize_ident(field)
                ));

                // Symbolic state
                if is_init {
                    emit_state_init_zeroed(out, mutable_fields, lifecycle_states, spec);
                } else {
                    emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
                    emit_pre_status_assume(out, op, lifecycle_states);
                }

                // Symbolic params
                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype, spec)?
                    ));
                }

                // Bounds assumptions for arithmetic safety
                if !is_init {
                    if !spec.constants.is_empty() {
                        for (cname, _) in &spec.constants {
                            let upper = cname.to_uppercase();
                            if upper.contains("MAX") || upper.contains("MEMBER") {
                                if mutable_fields.iter().any(|(f, _)| f == "member_count") {
                                    out.push_str(&format!(
                                        "    kani::assume(s.member_count <= {});\n",
                                        upper
                                    ));
                                }
                                break;
                            }
                        }
                    }
                    let owned_props: Vec<crate::check::ParsedProperty> =
                        properties.iter().map(|p| (*p).clone()).collect();
                    rust_codegen_util::emit_add_strict_bounds(
                        out,
                        op,
                        &owned_props,
                        "    kani::assume(s.{field} < s.{bound}); // strict bound: {field} increments\n",
                    );
                }

                // Snapshot pre-state — every mutable field (one assertion
                // pass: changed field + unchanged sibling fields).
                let needs_pre_for: Vec<&&(String, String)> = mutable_fields
                    .iter()
                    .filter(|(fname, _)| {
                        // "set" effects don't need pre on the target field;
                        // other fields do (to assert unchanged).
                        !(fname.as_str() == field.as_str() && op_kind == "set")
                    })
                    .collect();
                for (fname, _) in &needs_pre_for {
                    out.push_str(&format!("    let pre_{} = s.{};\n", fname, fname));
                }

                // Call transition
                let args: String = op
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!(", {}", n))
                    .collect();
                out.push_str(&format!("    if {}(&mut s{}) {{\n", op.name, args));

                // Assert THIS field's effect only.
                //
                // The effect-conformance harness snapshots every mutable
                // field as `pre_<fname> = s.<fname>` BEFORE calling the
                // transition. The post-condition RHS — `value` here — comes
                // from the spec's effect block (e.g. `:= state.now`), with
                // the `state.` prefix already stripped by the upstream
                // chumsky_adapter so each backend can apply its own binder.
                //
                // Without binder resolution the emitted assertion reads
                // `assert!(s.X == now)` — bare `now` is undefined in scope
                // and the harness fails to compile with
                // `error[E0425]: cannot find value 'now' in this scope`.
                //
                // `resolve_value` is identity for handler params and inlines
                // constants; the `Some("pre_")` binder is applied only when
                // `value` is a state field name. Same shape as the
                // analogous fix in `rust_codegen_util::emit_one_effect` for
                // the transition-fn emission target (`Some("s.")`).
                let resolved = rust_codegen_util::resolve_value(value, op, spec, Some("pre_"));
                match op_kind.as_str() {
                    "set" => {
                        out.push_str(&format!(
                            "        assert!(s.{} == {}, \"{} must equal {}\");\n",
                            field, resolved, field, resolved
                        ));
                    }
                    "add" => {
                        out.push_str(&format!(
                            "        assert!(s.{} == pre_{}.wrapping_add({}), \"{} must increment by {}\");\n",
                            field, field, resolved, field, resolved
                        ));
                    }
                    "sub" => {
                        out.push_str(&format!(
                            "        assert!(s.{} == pre_{}.wrapping_sub({}), \"{} must decrement by {}\");\n",
                            field, field, resolved, field, resolved
                        ));
                    }
                    _ => {}
                }

                // Assert all sibling fields unchanged
                for (fname, _) in mutable_fields {
                    if fname.as_str() != field.as_str() {
                        // Only assert unchanged if this sibling isn't itself
                        // mutated by ANOTHER effect in the same handler —
                        // otherwise the assertion would be wrong.
                        let sibling_mutated = op
                            .effects
                            .iter()
                            .any(|(f, _, _)| f.as_str() == fname.as_str());
                        if !sibling_mutated {
                            out.push_str(&format!(
                                "        assert!(s.{} == pre_{}, \"{} must not change\");\n",
                                fname, fname, fname
                            ));
                        }
                    }
                }

                out.push_str("    }\n");
                out.push_str("}\n\n");
                counts.effect += 1;
            }
        }
    }

    // ── Overflow detection harnesses ─────────────────────────────────────
    let overflow_ops: Vec<&ParsedHandler> = handlers
        .iter()
        .copied()
        .filter(|op| op.effects.iter().any(|(_, kind, _)| kind == "add"))
        .collect();
    if !overflow_ops.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Overflow detection — Kani catches arithmetic overflow on add effects\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for op in &overflow_ops {
            out.push_str("#[kani::proof]\n");
            out.push_str("#[kani::unwind(2)]\n");
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!("fn verify_{}_no_overflow() {{\n", op.name));

            emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
            emit_pre_status_assume(out, op, lifecycle_states);

            // Symbolic params
            for (pname, ptype) in &op.takes_params {
                out.push_str(&format!(
                    "    let {}: {} = kani::any();\n",
                    pname,
                    map_type(ptype, spec)?
                ));
            }

            // Call transition — Kani's built-in overflow detection fires on +=
            let args: String = op
                .takes_params
                .iter()
                .map(|(n, _)| format!(", {}", n))
                .collect();
            out.push_str(&format!(
                "    {}(&mut s{});  // Kani detects overflow on += internally\n",
                op.name, args
            ));
            out.push_str("}\n\n");
            counts.overflow += 1;
        }
    }

    Ok(())
}

/// Emit covers, liveness, and environment harnesses at file scope. These
/// reference handlers by name and the per-spec State directly, so they only
/// fire in single-ADT mode where there's a unique top-level `fn deposit(...)`
/// etc. In multi-ADT mode these are skipped (per-ADT lifting is v2.22 scope).
fn emit_file_level_features(
    out: &mut String,
    spec: &ParsedSpec,
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
) -> Result<()> {
    let has_lifecycle = lifecycle_states.len() >= 2;
    // ── Cover properties (reachability) ───────────────────────────────────
    if !spec.covers.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Cover properties — reachability via kani::cover!\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for cover in &spec.covers {
            for (i, trace) in cover.traces.iter().enumerate() {
                let suffix = if cover.traces.len() > 1 {
                    format!("_{}", i)
                } else {
                    String::new()
                };
                out.push_str("#[kani::proof]\n");
                let unwind = trace.len() + 1;
                out.push_str(&format!("#[kani::unwind({})]\n", unwind));
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!("fn cover_{}{}() {{\n", cover.name, suffix));

                emit_state_init_symbolic(out, mutable_fields, lifecycle_states);

                // Chain operations with nested ifs
                let mut indent = "    ".to_string();
                for (j, op_name) in trace.iter().enumerate() {
                    let op = spec.handlers.iter().find(|o| o.name == *op_name);
                    // Generate symbolic params
                    if let Some(op) = op {
                        for (pname, ptype) in &op.takes_params {
                            out.push_str(&format!(
                                "{}let {}_{}: {} = kani::any();\n",
                                indent,
                                pname,
                                j,
                                map_type(ptype, spec)?
                            ));
                        }
                    }
                    let args: String = op
                        .map(|o| {
                            o.takes_params
                                .iter()
                                .map(|(n, _)| format!(", {}_{}", n, j))
                                .collect()
                        })
                        .unwrap_or_default();

                    if j < trace.len() - 1 {
                        out.push_str(&format!("{}if {}(&mut s{}) {{\n", indent, op_name, args));
                        indent.push_str("    ");
                    } else {
                        out.push_str(&format!(
                            "{}kani::cover!({}(&mut s{}), \"{} trace is reachable\");\n",
                            indent, op_name, args, cover.name
                        ));
                    }
                }
                // Close braces
                for _ in 0..trace.len().saturating_sub(1) {
                    indent = indent[..indent.len() - 4].to_string();
                    out.push_str(&format!("{}}}\n", indent));
                }
                out.push_str("}\n\n");
            }
        }
    }

    // ── Liveness properties (bounded reachability) ──────────────────────
    if !spec.liveness_props.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Liveness properties — bounded reachability via non-deterministic ops\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for liveness in &spec.liveness_props {
            let bound = liveness.within_steps.unwrap_or(10) as usize;

            // Without a lifecycle in the State model, the target predicate
            // (`s.status == Status::<leads_to_state>`) has nothing to bind
            // to. Skip emission rather than ship a harness that runs random
            // ops and ends with no assertion — silent vacuous "verification"
            // is worse than no verification.
            if !has_lifecycle {
                out.push_str(&format!(
                    "// liveness {}: skipped — spec has no lifecycle, no target predicate to cover\n\n",
                    liveness.name
                ));
                continue;
            }

            out.push_str("#[kani::proof]\n");
            out.push_str(&format!("#[kani::unwind({})]\n", bound + 1));
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!("fn verify_liveness_{}() {{\n", liveness.name));

            emit_state_init_symbolic(out, mutable_fields, lifecycle_states);

            // Pre-state: assume the from-state. Without this, the harness
            // would explore symbolic-status executions where the via-ops
            // never fire (status mismatch on every step), and the cover
            // would only succeed by accident — a vacuous pass mode.
            out.push_str(&format!(
                "    kani::assume(s.status == Status::{});\n",
                liveness.from_state
            ));

            // Build via ops match
            let via_ops = &liveness.via_ops;
            out.push_str(&format!("    for _ in 0..{} {{\n", bound));
            out.push_str("        let op: u8 = kani::any();\n");
            out.push_str("        match op {\n");
            for (i, op_name) in via_ops.iter().enumerate() {
                let op = spec.handlers.iter().find(|o| o.name == *op_name);
                let param_decls: String = match op {
                    Some(o) => o
                        .takes_params
                        .iter()
                        .map(|(n, t)| {
                            map_type(t, spec)
                                .map(|rt| format!("            let {}: {} = kani::any();\n", n, rt))
                        })
                        .collect::<anyhow::Result<String>>()?,
                    None => String::new(),
                };
                let args: String = op
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .map(|(n, _)| format!(", {}", n))
                            .collect()
                    })
                    .unwrap_or_default();

                out.push_str(&format!("            {} => {{\n", i));
                out.push_str(&param_decls);
                out.push_str(&format!("                {}(&mut s{});\n", op_name, args));
                out.push_str("            }\n");
            }
            out.push_str("            _ => {}\n");
            out.push_str("        }\n");
            out.push_str("    }\n");

            // Post-state: cover the leads-to state. `kani::cover!` succeeds
            // when at least one execution path satisfies the predicate —
            // exactly the semantics of bounded reachability.
            out.push_str(&format!(
                "    kani::cover!(s.status == Status::{}, \"{} reaches {} within {} steps\");\n",
                liveness.leads_to_state, liveness.name, liveness.leads_to_state, bound
            ));
            out.push_str("}\n\n");
        }
    }

    // ── Environment property harnesses ────────────────────────────────────
    if !spec.environments.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Environment — properties hold under external state changes\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for env in &spec.environments {
            for prop in &spec.properties {
                if prop.expression.is_none() {
                    continue;
                }

                let rust_constraints: &[String] = &env.constraints_rust;

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!(
                    "fn verify_{}_under_{}() {{\n",
                    prop.name, env.name
                ));

                emit_state_init_symbolic(out, mutable_fields, lifecycle_states);
                out.push_str(&format!("    kani::assume({}(&s));\n", prop.name));

                // Apply environment mutation
                for (field, ftype) in &env.mutates {
                    out.push_str(&format!("    s.{} = kani::any();\n", field));
                    let _ = ftype; // type already handled by State struct
                }

                // Assume constraints
                for constraint in rust_constraints {
                    out.push_str(&format!("    kani::assume({});\n", constraint));
                }

                // Assert property still holds
                out.push_str(&format!("    assert!({}(&s),\n", prop.name));
                out.push_str(&format!(
                    "        \"{} must hold after {}\");\n",
                    prop.name, env.name
                ));
                out.push_str("}\n\n");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{emit_kani_account_section, HarnessCounts};
    use crate::check::{ParsedHandler, ParsedProperty};
    use crate::chumsky_adapter::parse_str;

    // B4 regression: a handler whose precondition is expressed purely through
    // `requires` clauses (no top-level `guard` DSL) used to emit
    // `kani::assume(!(true))`, making the rejection harness unreachable and
    // silently vacuous. The harness must now reflect the conjunction of every
    // `requires`.
    #[test]
    fn rejects_invalid_harness_folds_requires_clauses() {
        // `state` sugar + `requires` — no `guard` keyword. Pre-fix this path
        // fell through to `unwrap_or("true")`.
        let src = r#"spec T
state { balance : U64, status : U8 }
handler deposit (amount : U64) {
  requires amount > 0 else BelowMinimumAmount
  requires amount < 1_000_000_000 else MathOverflow
  requires state.status == 0 else WrongStatus
  effect {
    balance += amount
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        assert_eq!(op.requires.len(), 3);

        // Compose what `collect_full_guard` would produce; assert it's all three.
        let full = crate::rust_codegen_util::collect_full_guard(op, false)
            .expect("three requires clauses → Some");
        assert!(full.contains("amount > 0"));
        assert!(full.contains("1000000000"));
        assert!(full.contains("s.status == 0"));

        // Simulate the kani.rs emission: the assume line must negate the full
        // conjunction, NOT collapse to `!(true)`.
        let emitted_assume = format!("    kani::assume(!({}));", full);
        assert!(
            !emitted_assume.contains("!(true)"),
            "assume must not be vacuous: {}",
            emitted_assume
        );
        assert!(
            emitted_assume.contains("amount > 0"),
            "assume must reference a real guard: {}",
            emitted_assume
        );
    }

    // B3 regression: `let` bindings declared in the handler body MUST flow
    // into the generated Rust transition function so that the effect RHS
    // sees the binder in scope. Previously dropped entirely — the Rust
    // `net`/`total_fee` references crashed the compiler.
    #[test]
    fn let_bindings_flow_into_rust_transition() {
        let src = r#"spec T
state { pool : U64, fees : U64 }
handler compute (amount : U64) {
  requires amount > 0 else InvalidAmount
  let total_fee = amount * 125 / 10000
  let net = amount - total_fee
  effect {
    pool += net
    fees += total_fee
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        assert_eq!(op.let_bindings.len(), 2);
        let names: Vec<&str> = op.let_bindings.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(names, vec!["total_fee", "net"]);

        // Drive the transition emitter and assert both names appear as `let` in Rust.
        let mut out = String::new();
        crate::rust_codegen_util::emit_transition_fn(
            &mut out,
            op,
            &spec,
            /*wrapping=*/ false,
            |t| crate::codegen::map_type(t, &spec),
        )
        .expect("emit_transition_fn");
        assert!(
            out.contains("let total_fee ="),
            "missing total_fee let in transition:\n{}",
            out
        );
        assert!(
            out.contains("let net ="),
            "missing net let in transition:\n{}",
            out
        );
        // And the effects that reference these binders must come after.
        let total_fee_pos = out.find("let total_fee").unwrap();
        let pool_effect_pos = out.find("s.pool").unwrap();
        assert!(
            total_fee_pos < pool_effect_pos,
            "let bindings must precede effects:\n{}",
            out
        );
    }

    // B10 regression: transition functions must model `+=` as checked in the
    // Kani model (`wrapping=false`). Pre-fix the model emitted bare `s.x += v`,
    // which CBMC flagged as overflow on every unbounded pre-state — a
    // spec-model artifact that didn't match deployed Anchor programs using
    // `checked_add`.
    #[test]
    fn add_effect_uses_checked_semantics_in_kani_model() {
        let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) {
  requires amount > 0 else BelowMinimumAmount
  effect { pool += amount }
}"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];

        let mut out = String::new();
        crate::rust_codegen_util::emit_transition_fn(
            &mut out,
            op,
            &spec,
            /*wrapping=*/ false,
            |t| crate::codegen::map_type(t, &spec),
        )
        .expect("emit_transition_fn");

        // Must NOT emit the bare `+=` pattern — that's the pre-v2.6 model.
        assert!(
            !out.contains("s.pool += amount;"),
            "kani model (wrapping=false) must not use bare `+=`:\n{}",
            out
        );
        // Must emit the checked pattern; overflow → return false, matching
        // the Anchor program's `checked_add(..).ok_or(MathOverflow)?`.
        assert!(
            out.contains("checked_add"),
            "expected checked_add in non-wrapping model:\n{}",
            out
        );
        assert!(
            out.contains("return false"),
            "overflow must short-circuit the transition:\n{}",
            out
        );
    }

    #[test]
    fn add_effect_keeps_wrapping_for_proptest_mode() {
        let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool += amount } }"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        let mut out = String::new();
        crate::rust_codegen_util::emit_transition_fn(
            &mut out,
            op,
            &spec,
            /*wrapping=*/ true,
            |t| crate::codegen::map_type(t, &spec),
        )
        .expect("emit_transition_fn");
        assert!(
            out.contains("wrapping_add"),
            "proptest mode (wrapping=true) must keep wrapping_add:\n{}",
            out
        );
        assert!(!out.contains("checked_add"));
    }

    // B11 regression: effect conformance must be split per-field so one
    // CBMC-stuck field doesn't block the rest, and the solver is chosen per
    // (field-width × RHS-shape) by `pick_kani_solver`:
    //   * scalar/linear  → cadical
    //   * narrow mul/div → minisat
    //   * wide mul/div   → z3 (via `bin = "z3"`)
    #[test]
    fn b11_effect_solver_tiers() {
        use crate::rust_codegen_util::pick_kani_solver_for_effect;
        // Empty handler — no let bindings to chase through, so the RHS is
        // inspected directly. Exercises the width tiering in isolation.
        let src = r#"spec T
state { x : U64 }
handler noop { }
"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];

        // Scalar: no arithmetic → cadical regardless of width.
        assert_eq!(pick_kani_solver_for_effect("U64", "amount", op), "cadical");
        assert_eq!(pick_kani_solver_for_effect("U8", "1", op), "cadical");
        // Narrow-type mul/div → minisat.
        assert_eq!(pick_kani_solver_for_effect("U8", "x * 3", op), "minisat");
        assert_eq!(
            pick_kani_solver_for_effect("U32", "amount / 100", op),
            "minisat"
        );
        // Wide-type mul/div → z3 (the `amount * 125 / 10000` canonical case).
        assert_eq!(
            pick_kani_solver_for_effect("U64", "amount * 125 / 10000", op),
            "bin = \"z3\""
        );
        assert_eq!(
            pick_kani_solver_for_effect("U128", "a * b", op),
            "bin = \"z3\""
        );
        assert_eq!(
            pick_kani_solver_for_effect("I128", "a / b", op),
            "bin = \"z3\""
        );
        // Unknown type → falls back to minisat for arithmetic (safe default,
        // avoids cadical wedge until we learn the width).
        assert_eq!(pick_kani_solver_for_effect("", "a * b", op), "minisat");
    }

    // B11 let-binding chase: the canonical roaster_v2 pattern hides arithmetic
    // behind a let binding. The effect RHS is a bare ident; the solver
    // selector must chase through the binding to find the mul/div.
    #[test]
    fn b11_effect_solver_resolves_through_let_bindings() {
        use crate::rust_codegen_util::pick_kani_solver_for_effect;
        let src = r#"spec T
state { pool : U64, fees : U64 }
handler compute (amount : U64) {
  requires amount > 0 else InvalidAmount
  let total_fee = amount * 125 / 10000
  let net = amount - total_fee
  effect {
    pool += net
    fees += total_fee
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        // `fees += total_fee` — RHS is bare ident, let-binding has mul/div,
        // U64 field → z3.
        assert_eq!(
            pick_kani_solver_for_effect("U64", "total_fee", op),
            "bin = \"z3\"",
            "wide mul/div hidden in `let total_fee` must route to z3"
        );
        // `pool += net` — let-binding is `amount - total_fee`, no mul/div at
        // this level, but chases to `total_fee` which has mul/div → z3.
        assert_eq!(
            pick_kani_solver_for_effect("U64", "net", op),
            "bin = \"z3\"",
            "transitive let-chase must reach mul/div through `net → total_fee`"
        );
        // Narrow-field variant of the same pattern → minisat.
        assert_eq!(
            pick_kani_solver_for_effect("U8", "total_fee", op),
            "minisat"
        );
    }

    // B4 corollary: a handler with NO guards AND NO requires must not get a
    // rejection harness at all (kani.rs previously emitted one; now it skips).
    #[test]
    fn no_guards_no_requires_means_no_rejects_harness() {
        let src = r#"spec T
state { x : U8 }
handler noop {
  effect { x := 1 }
}"#;
        let spec = parse_str(src).expect("parse");
        let op = &spec.handlers[0];
        assert!(op.requires.is_empty());
        assert!(op.guard_str.is_none());
        assert!(
            crate::rust_codegen_util::collect_full_guard(op, false).is_none(),
            "handler with no preconditions must yield None — the kani.rs loop \
             should then `continue` and skip the harness entirely"
        );
    }

    // v2.21 Pair A — S2.2: multi-ADT specs MUST emit per-account `mod` blocks
    // so each ADT's State struct + transition fns are visible. Pre-fix
    // lending's two ADTs (Pool + Loan) collapsed to one flat State containing
    // Pool's fields only — Loan was silently dropped. The regression target
    // is two same-named fields across two ADTs; both should appear in their
    // respective module's State.
    #[test]
    fn multi_adt_emits_per_account_modules() {
        let src = r#"spec MultiADT

type Distribution
  | Empty
  | Active of {
      authority : Pubkey,
      balance   : U64,
    }

type Claim
  | Empty
  | Active of {
      claimant : Pubkey,
      balance  : U64,
    }

handler init_distribution (cap : U64) : Distribution.Empty -> Distribution.Active {
  effect { balance := cap }
}

handler init_claim (amount : U64) : Claim.Empty -> Claim.Active {
  effect { balance := amount }
}
"#;
        let spec = parse_str(src).expect("parse multi-ADT");
        assert_eq!(spec.account_types.len(), 2);
        // The kani.rs generator writes to a file, so use the in-memory
        // shape directly: emit_kani_account_section should be callable per
        // account. Here we just confirm the spec parses with two ADTs and
        // each has a non-empty field list — the file-level emission is
        // covered by the regen-drift sweep on bundled lending.
        let names: Vec<&str> = spec.account_types.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"Distribution"));
        assert!(names.contains(&"Claim"));
        for a in &spec.account_types {
            assert!(
                a.fields.iter().any(|(n, _)| n == "balance"),
                "ADT {} must carry `balance`",
                a.name
            );
        }
    }

    // ========================================================================
    // v2.23 Slice 4 — Kani preservation harness pre/post bifurcation
    // ========================================================================

    /// Parse a single-ADT spec and emit its Kani section to a string. Used
    /// by Slice 4 tests to assert on the harness shape.
    fn emit_kani_section(src: &str) -> String {
        let spec = parse_str(src).expect("parse");
        let mutable_fields: Vec<&(String, String)> = spec.state_fields.iter().collect();
        let handlers: Vec<&ParsedHandler> = spec.handlers.iter().collect();
        let properties: Vec<&ParsedProperty> = spec.properties.iter().collect();
        let mut out = String::new();
        let mut counts = HarnessCounts::default();
        emit_kani_account_section(
            &mut out,
            &mutable_fields,
            &handlers,
            &properties,
            &spec.lifecycle_states,
            &spec,
            &mut counts,
        )
        .expect("emit");
        out
    }

    const KANI_BINARY_SPEC: &str = r#"
spec KaniBinaryTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, settled : U64 }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}

property balance_nonneg :
  state.balance >= 0
  preserved_by all

property settled_monotonic :
  state.settled >= old(state.settled)
  preserved_by all
"#;

    #[test]
    fn kani_binary_property_predicate_has_pre_post_signature() {
        // Slice 4: `fn settled_monotonic(pre: &State, post: &State) -> bool`
        // — pre-v2.23 it emitted the single-state signature, producing
        // `s.settled >= s.settled` (tautology) in the body.
        let out = emit_kani_section(KANI_BINARY_SPEC);
        assert!(
            out.contains("fn settled_monotonic(pre: &State, post: &State) -> bool"),
            "binary predicate must have (pre, post) signature; got:\n{}",
            out
        );
        assert!(
            !out.contains("fn settled_monotonic(s: &State)"),
            "binary predicate must not have unary signature; got:\n{}",
            out
        );
    }

    #[test]
    fn kani_unary_property_predicate_keeps_single_state_signature() {
        // Slice 4 is additive on the binary path — unary properties see no
        // diff in their signature or body.
        let out = emit_kani_section(KANI_BINARY_SPEC);
        assert!(
            out.contains("fn balance_nonneg(s: &State) -> bool"),
            "unary predicate must keep single-state signature; got:\n{}",
            out
        );
    }

    #[test]
    fn kani_preservation_harness_captures_pre_state() {
        // Slice 4: each `verify_<op>_preserves_<prop>` harness must emit
        // `let pre = State { ... };` (symbolic) and `let mut post = pre;`
        // — the pre-v2.23 single `let mut s` shape destroyed the pre-state
        // before the assertion could see it.
        let out = emit_kani_section(KANI_BINARY_SPEC);
        let start = out
            .find("fn verify_bump_preserves_settled_monotonic()")
            .unwrap_or_else(|| {
                panic!(
                    "missing verify_bump_preserves_settled_monotonic; got:\n{}",
                    out
                )
            });
        let end = out[start..]
            .find("\n}\n\n")
            .map(|i| start + i)
            .unwrap_or(out.len());
        let body = &out[start..end];
        assert!(
            body.contains("let pre = State {"),
            "harness must declare symbolic `pre`; got:\n{}",
            body
        );
        assert!(
            body.contains("let mut post = pre;"),
            "harness must clone `post` from `pre` before mutation; got:\n{}",
            body
        );
        assert!(
            body.contains("bump(&mut post"),
            "handler must mutate `post`, not `s`; got:\n{}",
            body
        );
        assert!(
            body.contains("settled_monotonic(&pre, &post)"),
            "binary post-assert must use (&pre, &post); got:\n{}",
            body
        );
    }

    #[test]
    fn kani_preservation_harness_assumes_use_pre() {
        // Slice 4: `kani::assume(<unary>(&pre))` — pre-conditions apply to
        // the pre-state. Binary properties are skipped in the assume loop
        // (their (pre, pre) form is trivially true and offers no info).
        let out = emit_kani_section(KANI_BINARY_SPEC);
        let start = out
            .find("fn verify_bump_preserves_balance_nonneg()")
            .unwrap_or_else(|| {
                panic!(
                    "missing verify_bump_preserves_balance_nonneg; got:\n{}",
                    out
                )
            });
        let end = out[start..]
            .find("\n}\n\n")
            .map(|i| start + i)
            .unwrap_or(out.len());
        let body = &out[start..end];
        assert!(
            body.contains("kani::assume(balance_nonneg(&pre))"),
            "unary kani::assume must use `&pre`; got:\n{}",
            body
        );
        assert!(
            !body.contains("kani::assume(settled_monotonic("),
            "binary property must not appear in kani::assume; got:\n{}",
            body
        );
        assert!(
            body.contains("balance_nonneg(&post)"),
            "unary post-assert must use `&post`; got:\n{}",
            body
        );
        assert!(
            !body.contains("balance_nonneg(&s)"),
            "harness must not still reference legacy `&s`; got:\n{}",
            body
        );
    }
}
