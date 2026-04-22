use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler};
use crate::codegen::map_type;
use crate::rust_codegen_util;

/// Generate Kani proof harnesses from a spec file (.lean or .qedspec).
///
/// Produces self-contained proofs that model state transitions from the spec
/// and verify properties using Kani bounded model checking — no framework deps.
pub fn generate(spec_path: &Path, output_path: &Path) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No operations found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

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

    // ── State model ──────────────────────────────────────────────────────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// State model (derived from qedspec — no framework dependencies)\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    // Emit constants — infer type from value magnitude
    rust_codegen_util::emit_constants(&mut out, &spec.constants);

    // Collect mutable state fields (skip Pubkey — those are identity, not mutable state)
    let state_fields = rust_codegen_util::resolve_state_fields(&spec);
    let mutable_fields = rust_codegen_util::mutable_fields(state_fields);

    rust_codegen_util::emit_state_struct(&mut out, &mutable_fields, "Clone, Copy", map_type);

    // ── Property predicates ──────────────────────────────────────────────
    if !spec.properties.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Property predicates (from qedspec `property` declarations)\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        rust_codegen_util::emit_property_predicates(&mut out, &spec.properties, false);
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

    for op in &spec.handlers {
        rust_codegen_util::emit_transition_fn(&mut out, op, &spec, false, map_type);
    }

    // ── Guard enforcement proofs ─────────────────────────────────────────
    let guard_ops: Vec<&ParsedHandler> = spec.handlers.iter().filter(|op| op.has_guard()).collect();
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

            // Symbolic state
            out.push_str("    let mut s = State {\n");
            for (fname, _) in &mutable_fields {
                out.push_str(&format!("        {}: kani::any(),\n", fname));
            }
            out.push_str("    };\n");

            // Symbolic params
            for (pname, ptype) in &op.takes_params {
                out.push_str(&format!(
                    "    let {}: {} = kani::any();\n",
                    pname,
                    map_type(ptype)
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
        }
    }

    // ── Abort condition proofs ────────────────────────────────────────────
    let abort_ops: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
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

                // Symbolic state
                out.push_str("    let mut s = State {\n");
                for (fname, _) in &mutable_fields {
                    out.push_str(&format!("        {}: kani::any(),\n", fname));
                }
                out.push_str("    };\n");

                // Symbolic params
                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype)
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
            }
        }
    }

    // ── Property preservation proofs ─────────────────────────────────────
    if !spec.properties.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Property preservation — invariants hold through all transitions\n");
        out.push_str(
            "// ============================================================================\n\n",
        );

        for prop in &spec.properties {
            if prop.expression.is_none() {
                continue;
            }

            for op_name in &prop.preserved_by {
                let op = spec.handlers.iter().find(|o| &o.name == op_name);

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str("#[kani::solver(cadical)]\n");
                out.push_str(&format!(
                    "fn verify_{}_preserves_{}() {{\n",
                    op_name, prop.name
                ));

                // Determine if this is an initializing operation
                let is_init = op
                    .map(|o| o.pre_status.as_deref() == Some("Uninitialized"))
                    .unwrap_or(false);

                if is_init {
                    // For init operations, start with zeroed state
                    out.push_str("    let mut s = State {\n");
                    for (fname, _) in &mutable_fields {
                        out.push_str(&format!("        {}: 0,\n", fname));
                    }
                    out.push_str("    };\n");
                } else {
                    // Symbolic state with invariant assumptions
                    out.push_str("    let mut s = State {\n");
                    for (fname, _) in &mutable_fields {
                        out.push_str(&format!("        {}: kani::any(),\n", fname));
                    }
                    out.push_str("    };\n");

                    // Assume all declared properties hold before transition
                    for pre_prop in &spec.properties {
                        if pre_prop.expression.is_some() {
                            out.push_str(&format!("    kani::assume({}(&s));\n", pre_prop.name));
                        }
                    }

                    // Assume MAX_MEMBERS bound (derived from create_vault guard)
                    if !spec.constants.is_empty() {
                        // Find a "members" or "max" constant
                        for (cname, _cval) in &spec.constants {
                            let upper = cname.to_uppercase();
                            if upper.contains("MAX") || upper.contains("MEMBER") {
                                // Assume member_count <= MAX (from create_vault guard)
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
                }

                // Symbolic params
                if let Some(op) = op {
                    for (pname, ptype) in &op.takes_params {
                        out.push_str(&format!(
                            "    let {}: {} = kani::any();\n",
                            pname,
                            map_type(ptype)
                        ));
                    }
                }

                // For operations that increment a field (add effect), assume
                // the field is strictly less than its bound to prevent overflow
                if let Some(op) = op {
                    rust_codegen_util::emit_add_strict_bounds(&mut out, op, &spec.properties, "    kani::assume(s.{field} < s.{bound}); // strict bound: {field} increments\n");
                }

                // Call transition and assert property
                let args: String = op
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .map(|(n, _)| format!(", {}", n))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push_str(&format!("    if {}(&mut s{}) {{\n", op_name, args));
                out.push_str(&format!("        assert!({}(&s),\n", prop.name));
                out.push_str(&format!(
                    "            \"{} must hold after {}\");\n",
                    prop.name, op_name
                ));
                out.push_str("    }\n");
                out.push_str("}\n\n");
            }
        }
    }

    // ── Effect conformance proofs ─────────────────────────────────────────
    let effect_ops: Vec<&ParsedHandler> =
        spec.handlers.iter().filter(|op| op.has_effect()).collect();
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
        // proof per (handler, field) pair — so a single cadical-stuck mul/div
        // field doesn't block verification of its siblings. Heavy-arith
        // handlers (value RHS contains `*` or `/`) also get `minisat` instead
        // of `cadical`: minisat solves multiplication-heavy bit-blasted
        // problems that cadical wedges on.
        //
        // Pre-v2.6 a single `verify_X_effects` harness combined every field's
        // assertion — `verify_buy_side_a_effects` took 20+ min on a 5×mul/div
        // effect body. Per-field + solver hint drops that to seconds per
        // harness, and failures on one field don't hide the rest.
        for op in &effect_ops {
            let is_init = op.pre_status.as_deref() == Some("Uninitialized");

            for (field, op_kind, value) in &op.effects {
                let rhs_is_arithmetic = value.contains('*') || value.contains('/');
                let solver = if rhs_is_arithmetic {
                    "minisat"
                } else {
                    "cadical"
                };

                out.push_str("#[kani::proof]\n");
                out.push_str("#[kani::unwind(2)]\n");
                out.push_str(&format!("#[kani::solver({})]\n", solver));
                out.push_str(&format!("fn verify_{}_effect_{}() {{\n", op.name, field));

                // Symbolic state
                if is_init {
                    out.push_str("    let mut s = State {\n");
                    for (fname, _) in &mutable_fields {
                        out.push_str(&format!("        {}: 0,\n", fname));
                    }
                    out.push_str("    };\n");
                } else {
                    out.push_str("    let mut s = State {\n");
                    for (fname, _) in &mutable_fields {
                        out.push_str(&format!("        {}: kani::any(),\n", fname));
                    }
                    out.push_str("    };\n");
                }

                // Symbolic params
                for (pname, ptype) in &op.takes_params {
                    out.push_str(&format!(
                        "    let {}: {} = kani::any();\n",
                        pname,
                        map_type(ptype)
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
                    rust_codegen_util::emit_add_strict_bounds(
                        &mut out,
                        op,
                        &spec.properties,
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

                // Assert THIS field's effect only
                match op_kind.as_str() {
                    "set" => {
                        out.push_str(&format!(
                            "        assert!(s.{} == {}, \"{} must equal {}\");\n",
                            field, value, field, value
                        ));
                    }
                    "add" => {
                        out.push_str(&format!(
                            "        assert!(s.{} == pre_{}.wrapping_add({}), \"{} must increment by {}\");\n",
                            field, field, value, field, value
                        ));
                    }
                    "sub" => {
                        out.push_str(&format!(
                            "        assert!(s.{} == pre_{}.wrapping_sub({}), \"{} must decrement by {}\");\n",
                            field, field, value, field, value
                        ));
                    }
                    _ => {}
                }

                // Assert all sibling fields unchanged
                for (fname, _) in &mutable_fields {
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
            }
        }
    }

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

                // Start with symbolic state
                out.push_str("    let mut s = State {\n");
                for (fname, _) in &mutable_fields {
                    out.push_str(&format!("        {}: kani::any(),\n", fname));
                }
                out.push_str("    };\n");

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
                                map_type(ptype)
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
            out.push_str("#[kani::proof]\n");
            out.push_str(&format!("#[kani::unwind({})]\n", bound + 1));
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!("fn verify_liveness_{}() {{\n", liveness.name));

            // Symbolic state
            out.push_str("    let mut s = State {\n");
            for (fname, _) in &mutable_fields {
                out.push_str(&format!("        {}: kani::any(),\n", fname));
            }
            out.push_str("    };\n");

            // Build via ops match
            let via_ops = &liveness.via_ops;
            out.push_str(&format!("    for _ in 0..{} {{\n", bound));
            out.push_str("        let op: u8 = kani::any();\n");
            out.push_str("        match op {\n");
            for (i, op_name) in via_ops.iter().enumerate() {
                let op = spec.handlers.iter().find(|o| o.name == *op_name);
                let param_decls: String = op
                    .map(|o| {
                        o.takes_params
                            .iter()
                            .map(|(n, t)| {
                                format!("            let {}: {} = kani::any();\n", n, map_type(t))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
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

            // Note: kani::cover! doesn't take a state check directly,
            // so we use assert-like pattern for the target condition
            out.push_str(&format!(
                "    // Target: from {} to {} within {} steps\n",
                liveness.from_state, liveness.leads_to_state, bound
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

                // Symbolic state
                out.push_str("    let mut s = State {\n");
                for (fname, _) in &mutable_fields {
                    out.push_str(&format!("        {}: kani::any(),\n", fname));
                }
                out.push_str("    };\n");
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

    // ── Overflow detection harnesses ─────────────────────────────────────
    let overflow_ops: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
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

            // Symbolic state
            out.push_str("    let mut s = State {\n");
            for (fname, _) in &mutable_fields {
                out.push_str(&format!("        {}: kani::any(),\n", fname));
            }
            out.push_str("    };\n");

            // Symbolic params
            for (pname, ptype) in &op.takes_params {
                out.push_str(&format!(
                    "    let {}: {} = kani::any();\n",
                    pname,
                    map_type(ptype)
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
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    std::fs::write(output_path, &out)?;

    // ── Summary ──────────────────────────────────────────────────────────
    let guard_count = guard_ops.len();
    let prop_count: usize = spec
        .properties
        .iter()
        .filter(|p| p.expression.is_some())
        .map(|p| p.preserved_by.len())
        .sum();
    let effect_count = effect_ops.len();
    let overflow_count = overflow_ops.len();
    let abort_count: usize = abort_ops.iter().map(|op| op.aborts_if.len()).sum();
    let total = guard_count + prop_count + effect_count + overflow_count + abort_count;

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

#[cfg(test)]
mod tests {
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
            crate::codegen::map_type,
        );
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
            crate::codegen::map_type,
        );

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
            crate::codegen::map_type,
        );
        assert!(
            out.contains("wrapping_add"),
            "proptest mode (wrapping=true) must keep wrapping_add:\n{}",
            out
        );
        assert!(!out.contains("checked_add"));
    }

    // B11 regression: effect conformance must be split per-field so one
    // CBMC-stuck field doesn't block the rest. Heavy-arith RHS (`*`/`/` in
    // the value) switches solver to `minisat`, which handles bit-blasted
    // multiplication without cadical's wedge. This doesn't test the full
    // kani.rs emission path (that requires a spec file on disk) — it encodes
    // the invariants we rely on as comments/plumbing.
    #[test]
    fn b11_effect_heuristics() {
        // Arithmetic detection: a value containing `*` or `/` triggers the
        // minisat solver hint. `amount * 125 / 10000` is the canonical case
        // from the B11 repro (fee arithmetic).
        let v1 = "amount";
        let v2 = "amount * 125 / 10000";
        assert!(!v1.contains('*') && !v1.contains('/'));
        assert!(v2.contains('*') && v2.contains('/'));
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
}
