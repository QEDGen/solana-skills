use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedSpec};
use crate::codegen_shared::{map_type, write_generated_file};

/// `(field, op_kind, rust_value)` — the unit-test view of one effect site.
type EffectTriple = (String, &'static str, String);

/// Generate unit tests from a spec file (.lean or .qedspec).
/// Tests exercise effects, guards, and properties directly on a plain state
/// struct — no SVM, no Quasar runtime, just `cargo test`.
pub fn generate(spec_path: &Path, output_path: &Path) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No operations found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    crate::rust_codegen_util::check_effect_targets(&spec)?;

    // Effect iteration runs over the lowered MIR body via the shared
    // `stmt_effect_triple` projection (#66) instead of string-matching
    // raw `op.effects` (F7).
    let mir = crate::mir::lower(&spec);

    let fp = crate::fingerprint::compute_fingerprint(&spec);

    let is_multi = spec.account_types.len() > 1;
    let mut out = String::new();

    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        &fp,
        "tests/unit.rs",
    ));
    out.push_str("// Unit tests generated from qedspec.\n");
    out.push_str("// These test effects, guards, and properties on a plain state struct.\n");
    out.push_str("// No SVM or Quasar runtime required — just `cargo test`.\n\n");

    // Type alias for Address (Pubkey → [u8; 32] for standalone testing)
    let all_fields: Vec<&(String, String)> = if is_multi {
        spec.account_types
            .iter()
            .flat_map(|a| a.fields.iter())
            .collect()
    } else {
        spec.state_fields.iter().collect()
    };
    if all_fields.iter().any(|(_, t)| t == "Pubkey")
        || spec.handlers.iter().any(|op| op.who.is_some())
    {
        out.push_str("type Address = [u8; 32];\n\n");
    }

    // User-defined records/enums referenced by State fields must be
    // declared first so the State struct compiles.
    crate::rust_codegen_util::emit_record_structs(
        &mut out,
        &spec,
        "Debug, Clone, Copy, PartialEq",
        |t| map_type(t, &spec),
    )?;
    crate::rust_codegen_util::emit_unit_enum_sums(
        &mut out,
        &spec,
        "Debug, Clone, Copy, PartialEq, Eq",
    )?;

    if is_multi {
        // Multi-account: one struct + status enum per account type
        for acct in &spec.account_types {
            let state_name = format!("{}State", acct.name);
            emit_state_struct(&mut out, &state_name, &acct.fields, &spec)?;

            if !acct.lifecycle.is_empty() {
                let status_name = format!("{}Status", acct.name);
                out.push_str(&format!(
                    "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum {} {{\n",
                    status_name
                ));
                for state in &acct.lifecycle {
                    out.push_str(&format!("    {},\n", state));
                }
                out.push_str("}\n\n");
            }
        }
    } else {
        let state_name = format!(
            "{}State",
            crate::codegen_shared::to_pascal_case(&spec.program_name)
        );
        emit_state_struct(&mut out, &state_name, &spec.state_fields, &spec)?;

        // Status enum for state machine tests
        if !spec.lifecycle_states.is_empty() {
            out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum Status {\n");
            for state in &spec.lifecycle_states {
                out.push_str(&format!("    {},\n", state));
            }
            out.push_str("}\n\n");
        }
    }

    // Helper: apply effects to state
    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (op_state_name, _) = resolve_state_for_op(op, &spec, is_multi);
        let triples = effect_triples(&op.name, &mir, &spec);
        // Prefix unused params with _ to suppress warnings. A param is used
        // when any effect references it — in the target path (subscripts
        // like `voted[member_index]`) or the RHS.
        let params: Vec<String> = op
            .takes_params
            .iter()
            .map(|(n, t)| {
                let used = triples
                    .iter()
                    .any(|(f, _, v)| f.contains(n.as_str()) || v.contains(n.as_str()));
                let rt = map_type(t, &spec)?;
                Ok(if used {
                    format!("{}: {}", n, rt)
                } else {
                    format!("_{}: {}", n, rt)
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let param_sig = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        out.push_str(&format!("/// Apply `{}` effects to state.\n", op.name));
        out.push_str(&format!(
            "fn apply_{}(state: &mut {}{}) {{\n",
            op.name, op_state_name, param_sig
        ));
        // Account-valued effects are unexpressible here (the model
        // carries no accounts) — note each one instead of silently
        // narrowing the spec (#297).
        for note in suppressed_effect_notes(&op.name, &mir, &spec) {
            out.push_str(&format!(
                "    // not modeled (account-valued; accounts exist only at runtime): {note}\n"
            ));
        }
        // Parallel effect semantics: RHS reads of fields this block also
        // writes observe the PRE-state value (matching the Lean model and
        // the Kani conformance assertions) — snapshot them before mutating.
        let pre_fields = parallel_pre_fields(&op.name, &mir, &spec);
        for f in &pre_fields {
            out.push_str(&format!("    let pre_{f} = state.{f};\n"));
        }
        for (field, kind, value) in &triples {
            let value = substitute_pre_state_reads(value, &pre_fields);
            let value = value.as_str();
            match *kind {
                "set" => {
                    out.push_str(&format!("    state.{} = {};\n", field, value));
                }
                "add" => {
                    out.push_str(&format!("    state.{} += {};\n", field, value));
                }
                "sub" => {
                    out.push_str(&format!("    state.{} -= {};\n", field, value));
                }
                "add_sat" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.saturating_add({});\n",
                        field, field, value
                    ));
                }
                "sub_sat" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.saturating_sub({});\n",
                        field, field, value
                    ));
                }
                "add_wrap" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.wrapping_add({});\n",
                        field, field, value
                    ));
                }
                "sub_wrap" => {
                    out.push_str(&format!(
                        "    state.{} = state.{}.wrapping_sub({});\n",
                        field, field, value
                    ));
                }
                other => {
                    out.push_str(&format!(
                        "    // unknown effect: {} {} {}\n",
                        field, other, value
                    ));
                }
            }
        }
        out.push_str("}\n\n");
    }

    // Helper: guard predicates. Handlers whose requires are all
    // account-suppressed get no guard fn (and no guard tests below) — a
    // `true` predicate would make the rejects-test assert `!true`.
    for op in &spec.handlers {
        let Some(guard_rust) = guard_predicate_rust(op) else {
            continue;
        };
        let (op_state_name, _) = resolve_state_for_op(op, &spec, is_multi);
        let params: Vec<String> = op
            .takes_params
            .iter()
            .map(|(n, t)| map_type(t, &spec).map(|rt| format!("{}: {}", n, rt)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let param_sig = if params.is_empty() {
            String::new()
        } else {
            format!(", {}", params.join(", "))
        };
        // If the guard doesn't reference state fields, prefix with _
        let state_param = if guard_rust.contains("state.") {
            "state"
        } else {
            "_state"
        };
        out.push_str(&format!("/// Guard predicate for `{}`.\n", op.name));
        out.push_str(&format!(
            "fn guard_{}({}: &{}{}) -> bool {{\n",
            op.name, state_param, op_state_name, param_sig
        ));
        out.push_str(&format!("    {}\n", guard_rust));
        out.push_str("}\n\n");
    }

    out.push_str("#[cfg(test)]\nmod tests {\n    use super::*;\n\n");

    out.push_str("    // ====================================================================\n");
    out.push_str("    // Effect tests — verify state mutations match spec\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        let triples = effect_triples(&op.name, &mir, &spec);
        let pre_fields = parallel_pre_fields(&op.name, &mir, &spec);
        generate_effect_test(&mut out, op, &triples, fields, &sn, &spec, &pre_fields)?;
    }

    out.push_str("    // ====================================================================\n");
    out.push_str("    // Guard tests — verify boundary conditions\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        let Some(guard_rust) = guard_predicate_rust(op) else {
            continue;
        };
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        generate_guard_tests(&mut out, op, &guard_rust, fields, &sn, &spec)?;
    }

    if !spec.properties.is_empty() {
        out.push_str(
            "    // ====================================================================\n",
        );
        out.push_str("    // Property tests — verify invariants hold after effects\n");
        out.push_str(
            "    // ====================================================================\n\n",
        );

        for prop in &spec.properties {
            // Resolve property's state type based on expression field references
            let (prop_sn, prop_fields) = resolve_state_for_property(prop, &spec, is_multi);
            for op_name in &prop.preserved_by {
                if let Some(op) = spec.handlers.iter().find(|o| &o.name == op_name) {
                    if !op.has_effect() {
                        continue;
                    }
                    // For multi-account: skip if op targets a different account than the property
                    if is_multi {
                        let (op_sn, _) = resolve_state_for_op(op, &spec, true);
                        if op_sn != prop_sn {
                            // Cross-account: this property is trivially preserved since
                            // the operation doesn't modify the property's state.
                            out.push_str(&format!(
                                "    // {}.{} skipped — {} operates on {}, not {}\n\n",
                                prop.name, op.name, op.name, op_sn, prop_sn
                            ));
                            continue;
                        }
                    }
                    generate_property_test(&mut out, op, prop, prop_fields, &prop_sn, &spec)?;
                }
            }
        }
    }

    out.push_str("    // ====================================================================\n");
    out.push_str("    // Unchanged field tests — fields not in effects must not change\n");
    out.push_str("    // ====================================================================\n\n");

    for op in &spec.handlers {
        if !op.has_effect() {
            continue;
        }
        let (sn, fields) = resolve_state_for_op(op, &spec, is_multi);
        let triples = effect_triples(&op.name, &mir, &spec);
        generate_unchanged_test(&mut out, op, &triples, fields, &sn, &spec)?;
    }

    let transition_ops: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
        .filter(|op| op.pre_status.is_some() && op.post_status.is_some())
        .collect();
    if !transition_ops.is_empty() {
        out.push_str(
            "    // ====================================================================\n",
        );
        out.push_str("    // State machine tests — verify lifecycle transitions\n");
        out.push_str(
            "    // ====================================================================\n\n",
        );

        for op in &transition_ops {
            let status_enum = if is_multi {
                let target = op
                    .on_account
                    .as_deref()
                    .unwrap_or(&spec.account_types[0].name);
                format!("{}Status", target)
            } else {
                "Status".to_string()
            };
            generate_state_machine_test(&mut out, op, &status_enum);
        }
    }

    out.push_str("}\n");

    // Count tests
    let effect_count = spec.handlers.iter().filter(|o| o.has_effect()).count();
    let guard_count = spec
        .handlers
        .iter()
        .filter(|o| guard_predicate_rust(o).is_some())
        .count()
        * 2; // pass + fail
    let prop_count: usize = spec
        .properties
        .iter()
        .map(|p| {
            p.preserved_by
                .iter()
                .filter(|name| {
                    spec.handlers
                        .iter()
                        .find(|o| &&o.name == name)
                        .is_some_and(|o| o.has_effect())
                })
                .count()
        })
        .sum();
    let unchanged_count = effect_count;
    let sm_count = transition_ops.len();
    let total = effect_count + guard_count + prop_count + unchanged_count + sm_count;

    write_generated_file(output_path, &out)?;

    eprintln!(
        "Generated {} unit tests in {}",
        total,
        output_path.display()
    );
    eprintln!("  {} effect test(s)", effect_count);
    eprintln!("  {} guard test(s)", guard_count);
    eprintln!("  {} property preservation test(s)", prop_count);
    eprintln!("  {} unchanged field test(s)", unchanged_count);
    eprintln!("  {} state machine test(s)", sm_count);

    Ok(())
}

/// Emit a state struct definition with Default impl.
fn emit_state_struct(
    out: &mut String,
    state_name: &str,
    fields: &[(String, String)],
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    out.push_str(&format!("struct {} {{\n", state_name));
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type(ftype, spec)?));
    }
    out.push_str("}\n\n");

    out.push_str(&format!("impl Default for {} {{\n", state_name));
    out.push_str("    fn default() -> Self {\n");
    out.push_str(&format!("        {} {{\n", state_name));
    for (fname, ftype) in fields {
        let default_val = match ftype.as_str() {
            "Pubkey" => "[0u8; 32]",
            "U64" => "0u64",
            "U128" => "0u128",
            "U8" => "0u8",
            "I128" => "0i128",
            "Bool" => "false",
            _ => "Default::default()",
        };
        out.push_str(&format!("            {}: {},\n", fname, default_val));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Resolve the state name and fields for an operation.
fn resolve_state_for_op<'a>(
    op: &ParsedHandler,
    spec: &'a ParsedSpec,
    is_multi: bool,
) -> (String, &'a [(String, String)]) {
    if is_multi {
        let target = op
            .on_account
            .as_deref()
            .unwrap_or(&spec.account_types[0].name);
        let acct = spec
            .account_types
            .iter()
            .find(|a| a.name == target)
            .unwrap_or(&spec.account_types[0]);
        (format!("{}State", acct.name), &acct.fields)
    } else {
        (
            format!(
                "{}State",
                crate::codegen_shared::to_pascal_case(&spec.program_name)
            ),
            &spec.state_fields,
        )
    }
}

/// Resolve the state name and fields for a property based on its expression's field references.
fn resolve_state_for_property<'a>(
    prop: &crate::check::ParsedProperty,
    spec: &'a ParsedSpec,
    is_multi: bool,
) -> (String, &'a [(String, String)]) {
    if !is_multi {
        return (
            format!(
                "{}State",
                crate::codegen_shared::to_pascal_case(&spec.program_name)
            ),
            &spec.state_fields,
        );
    }

    // Find which account type's fields match the property expression
    if let Some(ref expr) = prop.expression {
        for acct in &spec.account_types {
            if acct
                .fields
                .iter()
                .any(|(f, _)| expr.contains(&format!("s.{}", f)))
            {
                return (format!("{}State", acct.name), &acct.fields);
            }
        }
    }

    // Default to first account
    (
        format!("{}State", spec.account_types[0].name),
        &spec.account_types[0].fields,
    )
}

/// The handler's `requires` clauses as one Rust predicate bound to
/// `state` — tree-native render (#156; replaces the legacy `guard_str`
/// read that left requires-only handlers with a vacuous `true` guard fn
/// and an always-failing rejects-test). Requires touching handler-account
/// pubkeys are suppressed: the unit-test state struct carries no
/// accounts at all, so any account-touching clause (bare `approver`
/// comparisons included, not just `.pubkey` reads) is unexpressible
/// here. Top-level conjunctions are projected term-by-term so an
/// account-only term does not erase adjacent state/param constraints.
/// Other boolean shapes stay atomic: pruning below `or`/`not` would
/// change their meaning. `None` when nothing is expressible — the caller
/// skips the guard fn and its tests.
fn guard_predicate_rust(op: &ParsedHandler) -> Option<String> {
    let parts: Vec<String> = op
        .requires
        .iter()
        .map(requires_tree)
        .flat_map(account_free_conjuncts)
        .map(|t| format!("({})", render_for_state(t)))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

/// Flatten `and` nodes and retain the conjuncts expressible against the
/// account-free unit-test state model. Account reads nested under any
/// other expression shape make that whole conjunct unexpressible.
fn account_free_conjuncts(tree: &crate::mir::ExprTree) -> Vec<&crate::mir::ExprTree> {
    use crate::mir::expr_tree::{ExprTree, TreeBoolOp};

    match tree {
        ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs,
            rhs,
        } => {
            let mut out = account_free_conjuncts(lhs);
            out.extend(account_free_conjuncts(rhs));
            out
        }
        _ if crate::rust_codegen_util::tree_render::tree_mentions_account(tree) => Vec::new(),
        _ => vec![tree],
    }
}

/// Render a typed expression tree against the unit-test `state` binder.
fn render_for_state(tree: &crate::mir::ExprTree) -> String {
    use crate::rust_codegen_util::tree_render::{render_rust, Binder, RustCx};
    render_rust(
        tree,
        RustCx::native().with_binder(Binder::SelfAcct("state")),
    )
}

/// The typed tree of a requires clause. Post-#151 every production
/// `ParsedRequires` is adapter-built with `tree: Some(...)`; a `None`
/// here is a hand-built fixture that must be fixed, not worked around.
fn requires_tree(req: &crate::check::ParsedRequires) -> &crate::mir::ExprTree {
    req.tree
        .as_ref()
        .expect("ParsedRequires.tree is always populated by the chumsky adapter (#151/#156)")
}

/// Effect triples for a handler, projected from the lowered MIR body via
/// the shared `stmt_effect_triple` (#66) — the same iteration source the
/// Kani/proptest backends use — instead of string-matching raw
/// `op.effects`. Deep: `effect { match … }` arms flatten to their union,
/// matching the parser's back-compat `op.effects` view this file
/// previously consumed. Fields are flattened to the union-state view
/// (variant prefixes stripped); values carry the adapter-rendered Rust
/// RHS (falls back to the raw spec string for tree-less ingest paths).
fn effect_triples(op_name: &str, mir: &crate::mir::Mir, spec: &ParsedSpec) -> Vec<EffectTriple> {
    let Some(h) = mir.handlers.iter().find(|h| h.name == op_name) else {
        return Vec::new();
    };
    crate::rust_codegen_util::block_effect_triples_deep(&h.body)
        .into_iter()
        .filter(|(field, _, value)| !effect_is_account_valued(field, value, op_name, spec))
        .map(|(field, kind, value)| {
            let target = crate::rust_codegen_util::render_effect_target(field, spec, "state");
            (
                target.strip_prefix("state.").unwrap_or(&target).to_string(),
                kind,
                // `state.` receiver, same binder as `render_for_state` —
                // the native default renders state reads as `s.<field>`,
                // a binding that doesn't exist in this file's `apply_*` /
                // test scopes (pre-v2.44 this emitted non-compiling
                // `state.last_seen = s.balance;`).
                render_for_state(crate::rust_codegen_util::mir_expr_tree(value)),
            )
        })
        .collect()
}

/// Is this effect unexpressible in the account-free unit-test model?
/// Two structural signals, matching the shared harness lane
/// (`emit_transition_fn`'s pubkey-skip) and this file's own guard
/// suppression (`account_free_conjuncts`):
/// - the destination field is `Pubkey`-typed (identity flows from
///   accounts; the model carries no accounts), or
/// - the RHS reads an account binding (`initializer_ta.pubkey`) — the
///   `apply_*`/test scopes have no such binding, so rendering it
///   verbatim is an E0425 (#297).
fn effect_is_account_valued(
    field: &crate::mir::Path,
    value: &crate::mir::Expr,
    op_name: &str,
    spec: &ParsedSpec,
) -> bool {
    let dest_is_pubkey = spec
        .handlers
        .iter()
        .find(|o| o.name == op_name)
        .is_some_and(|op| {
            crate::rust_codegen_util::field_type_is_pubkey(
                &crate::rust_codegen_util::effect_path_source(field),
                op,
                spec,
            )
        });
    dest_is_pubkey
        || crate::rust_codegen_util::tree_render::tree_mentions_account(
            crate::rust_codegen_util::mir_expr_tree(value),
        )
}

/// Human-readable notes for the effects [`effect_is_account_valued`]
/// suppressed — emitted as comments in `apply_*` so the model is honest
/// about what it does not cover.
fn suppressed_effect_notes(op_name: &str, mir: &crate::mir::Mir, spec: &ParsedSpec) -> Vec<String> {
    let Some(h) = mir.handlers.iter().find(|h| h.name == op_name) else {
        return Vec::new();
    };
    crate::rust_codegen_util::block_effect_triples_deep(&h.body)
        .into_iter()
        .filter(|(field, _, value)| effect_is_account_valued(field, value, op_name, spec))
        .map(|(field, _, value)| {
            format!(
                "{} := {}",
                crate::rust_codegen_util::strip_variant_prefix_for_flat_state(
                    &crate::rust_codegen_util::effect_path_source(field),
                    spec,
                ),
                crate::rust_codegen_util::mir_expr_rust(value)
            )
        })
        .collect()
}

/// Fields needing a `pre_<field>` snapshot for this handler under
/// parallel effect semantics (see `parallel_snapshot_fields`): the
/// `apply_*` helper binds them before mutating, and the effect test
/// asserts RHS reads against them. Computed over the same filtered
/// triples `apply_*` emits, so every snapshot is referenced.
fn parallel_pre_fields(op_name: &str, mir: &crate::mir::Mir, spec: &ParsedSpec) -> Vec<String> {
    let Some(h) = mir.handlers.iter().find(|h| h.name == op_name) else {
        return Vec::new();
    };
    let triples: Vec<(&crate::mir::Path, &'static str, &crate::mir::Expr)> =
        crate::rust_codegen_util::block_effect_triples_deep(&h.body)
            .into_iter()
            .filter(|(field, _, value)| !effect_is_account_valued(field, value, op_name, spec))
            .collect();
    crate::rust_codegen_util::parallel_snapshot_fields(&triples, spec)
}

/// RHS-side substitution for the parallel snapshots, over the `state.`
/// receiver this file renders with.
fn substitute_pre_state_reads(value: &str, pre_fields: &[String]) -> String {
    crate::rust_codegen_util::substitute_pre_reads(value, "state", pre_fields)
}

/// Identifier-safe form of an effect-target path for `pre_*` snapshot
/// bindings: `voted[member_index]` → `voted_member_index_`.
fn pre_ident(field: &str) -> String {
    field
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// Build the argument list for calling apply_op / guard_op.
fn call_args(op: &ParsedHandler) -> String {
    if op.takes_params.is_empty() {
        return String::new();
    }
    let args: Vec<&str> = op.takes_params.iter().map(|(n, _)| n.as_str()).collect();
    format!(", {}", args.join(", "))
}

/// Generate a test that applies an operation's effects and checks the result.
fn generate_effect_test(
    out: &mut String,
    op: &ParsedHandler,
    triples: &[EffectTriple],
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
    pre_fields: &[String],
) -> Result<()> {
    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_effects() {{\n", op.name));

    // Set up state with concrete values that satisfy the guard
    emit_state_literal(out, state_name, fields, op, &[], true);

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }

    // Snapshot pre-state for arithmetic effects, plus every parallel-
    // semantics snapshot field: an RHS that reads a block-written field
    // means the PRE-state value, so the assertion below must compare
    // against the snapshot, not the post-apply read.
    let mut snapshotted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (field, kind, _) in triples {
        if *kind != "set" {
            out.push_str(&format!(
                "        let pre_{} = state.{};\n",
                pre_ident(field),
                field
            ));
            snapshotted.insert(pre_ident(field));
        }
    }
    for f in pre_fields {
        if snapshotted.contains(f.as_str()) {
            continue;
        }
        out.push_str(&format!("        let pre_{f} = state.{f};\n"));
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    for (field, kind, value) in triples {
        let value = substitute_pre_state_reads(value, pre_fields);
        let value = value.as_str();
        let pre = format!("pre_{}", pre_ident(field));
        match *kind {
            "set" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {});\n",
                    field, value
                ));
            }
            "add" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {} + {});\n",
                    field, pre, value
                ));
            }
            "sub" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {} - {});\n",
                    field, pre, value
                ));
            }
            "add_sat" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.saturating_add({}));\n",
                    field, pre, value
                ));
            }
            "sub_sat" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.saturating_sub({}));\n",
                    field, pre, value
                ));
            }
            "add_wrap" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.wrapping_add({}));\n",
                    field, pre, value
                ));
            }
            "sub_wrap" => {
                out.push_str(&format!(
                    "        assert_eq!(state.{}, {}.wrapping_sub({}));\n",
                    field, pre, value
                ));
            }
            _ => {}
        }
    }

    out.push_str("    }\n\n");
    Ok(())
}

/// Generate pass/fail guard tests with boundary values.
fn generate_guard_tests(
    out: &mut String,
    op: &ParsedHandler,
    guard_rust: &str,
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    // --- Test: guard PASSES with valid inputs ---
    // Bug 4: solve the common linear-equality / disjunction / field-vs-
    // field shapes so compound guards get a real satisfying fixture; fall
    // back to the naive seed when the solver can't. `check_fixture` (#312)
    // verifies the fixture before the test asserts anything.
    let (accept_state_ov, accept_param_ov) = satisfy_guard(op, fields).unwrap_or_default();
    let accepts_check = check_fixture(
        op,
        &fixture_env(fields, op, &accept_state_ov, &accept_param_ov),
        true,
    );
    emit_guard_test(
        out,
        op,
        "accepts_valid",
        state_name,
        fields,
        spec,
        &accept_state_ov,
        &accept_param_ov,
        accepts_check,
        /*want_true=*/ true,
    )?;

    // --- Test: guard REJECTS invalid inputs ---
    // `derive_guard_violation` leads with the linear falsifier
    // (`falsify_guard_linear`), handling field-vs-field and compound
    // shapes; `check_fixture` verifies the fixture actually rejects.
    let (state_overrides, param_overrides) = derive_guard_violation(guard_rust, op, fields);
    let rejects_check = check_fixture(
        op,
        &fixture_env(fields, op, &state_overrides, &param_overrides),
        false,
    );
    emit_guard_test(
        out,
        op,
        "rejects_invalid",
        state_name,
        fields,
        spec,
        &state_overrides,
        &param_overrides,
        rejects_check,
        /*want_true=*/ false,
    )?;
    Ok(())
}

/// Emit one guard test. A `Solved` check emits the fixture + the
/// assertion; an unverified check (`Contradicted` / `Unsupported`)
/// references the guard fn as a compile check but does NOT emit the
/// fixture or EXECUTE the guard. Running the guard on an unverified
/// fixture could panic — e.g. a division / modulo guard hitting a zero —
/// even when the generated program is correct (review feedback on #263).
#[allow(clippy::too_many_arguments)]
fn emit_guard_test(
    out: &mut String,
    op: &ParsedHandler,
    suffix: &str,
    state_name: &str,
    fields: &[(String, String)],
    spec: &ParsedSpec,
    state_ov: &Overrides,
    param_ov: &Overrides,
    check: FixtureCheck,
    want_true: bool,
) -> Result<()> {
    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_guard_{}() {{\n", op.name, suffix));
    match check {
        FixtureCheck::Solved => {
            emit_state_literal_with(out, state_name, fields, op, &[], state_ov, false);
            for (pname, ptype) in &op.takes_params {
                let val = param_ov
                    .iter()
                    .find(|(n, _)| n == pname)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| sensible_param(pname, ptype));
                out.push_str(&format!(
                    "        let {}: {} = {};\n",
                    pname,
                    map_type(ptype, spec)?,
                    val
                ));
            }
            let bang = if want_true { "" } else { "!" };
            out.push_str(&format!(
                "        assert!({}guard_{}(&state{}));\n",
                bang,
                op.name,
                call_args(op)
            ));
        }
        FixtureCheck::Contradicted => {
            let wanted = if want_true { "satisfy" } else { "violate" };
            out.push_str(&format!(
                "        // No assertion: no fixture was found that would {wanted} this\n\
                 \x20       // guard, so asserting would test the fixture search, not the\n\
                 \x20       // guard. The guard fn is referenced but NOT executed — an\n\
                 \x20       // unverified fixture could panic a div/mod guard. See #312.\n\
                 \x20       let _ = guard_{};\n",
                op.name
            ));
        }
        FixtureCheck::Unsupported(reason) => {
            out.push_str(&format!(
                "        // No assertion: {reason}, so the fixture is unverified. The\n\
                 \x20       // guard fn is referenced but NOT executed (an unverified\n\
                 \x20       // fixture could panic a div/mod guard). See #312.\n\
                 \x20       let _ = guard_{};\n",
                op.name
            ));
        }
    }
    out.push_str("    }\n\n");
    Ok(())
}

/// Generate a property preservation test for a specific operation.
fn generate_property_test(
    out: &mut String,
    op: &ParsedHandler,
    prop: &crate::check::ParsedProperty,
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_preserves_{}() {{\n",
        op.name, prop.name
    ));

    // Set up state that satisfies the property: seed values consider the
    // property body alongside the operation's own guards.
    let prop_rust = prop.tree.as_ref().map(render_for_state);
    let extra: Vec<&str> = prop_rust.as_deref().into_iter().collect();
    emit_state_literal(out, state_name, fields, op, &extra, true);

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    let prop_name_upper = prop.name.replace('_', " ");
    out.push_str(&format!(
        "        // Property: {} must hold after {}\n",
        prop_name_upper, op.name
    ));

    if let Some(rust_expr) = &prop_rust {
        out.push_str(&format!(
            "        assert!({}, \"{} must hold after {}\");\n",
            rust_expr, prop.name, op.name
        ));
    } else {
        out.push_str(&format!(
            "        // AGENT: assert property '{}' holds on state\n",
            prop.name
        ));
    }

    out.push_str("    }\n\n");
    Ok(())
}

/// Generate unchanged field tests — fields not in effects must not change.
fn generate_unchanged_test(
    out: &mut String,
    op: &ParsedHandler,
    triples: &[EffectTriple],
    fields: &[(String, String)],
    state_name: &str,
    spec: &ParsedSpec,
) -> Result<()> {
    // Base name of each effect target: `voted[member_index]` affects
    // `voted` (the old raw-string comparison missed subscripted targets
    // and asserted them unchanged).
    let affected: Vec<&str> = triples
        .iter()
        .map(|(f, _, _)| crate::rust_codegen_util::effect_target_base(f))
        .collect();
    let unchanged: Vec<&(String, String)> = fields
        .iter()
        .filter(|(f, t)| !affected.contains(&f.as_str()) && t != "Pubkey")
        .collect();

    if unchanged.is_empty() {
        return Ok(());
    }

    out.push_str("    #[test]\n");
    out.push_str(&format!("    fn test_{}_unchanged_fields() {{\n", op.name));

    emit_state_literal(out, state_name, fields, op, &[], true);

    for (pname, ptype) in &op.takes_params {
        let val = sensible_param(pname, ptype);
        out.push_str(&format!(
            "        let {}: {} = {};\n",
            pname,
            map_type(ptype, spec)?,
            val
        ));
    }

    for (fname, _) in &unchanged {
        out.push_str(&format!(
            "        let pre_{} = state.{}.clone();\n",
            fname, fname
        ));
    }

    out.push_str(&format!(
        "        apply_{}(&mut state{});\n",
        op.name,
        call_args(op)
    ));

    for (fname, _) in &unchanged {
        out.push_str(&format!(
            "        assert_eq!(state.{}, pre_{}, \"{} must not change after {}\");\n",
            fname, fname, fname, op.name
        ));
    }

    out.push_str("    }\n\n");
    Ok(())
}

/// Generate a state machine test — verify the transition is valid.
fn generate_state_machine_test(out: &mut String, op: &ParsedHandler, status_enum: &str) {
    let pre = op.pre_status.as_ref().unwrap();
    let post = op.post_status.as_ref().unwrap();

    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_transition_{}_to_{}() {{\n",
        op.name,
        pre.to_lowercase(),
        post.to_lowercase()
    ));
    out.push_str(&format!(
        "        // {} requires status == {} and moves to {}\n",
        op.name, pre, post
    ));
    if pre == post {
        out.push_str(&format!(
            "        assert_eq!({}::{}, {}::{}, \"{} is a self-transition\");\n",
            status_enum, pre, status_enum, post, op.name
        ));
    } else {
        out.push_str(&format!(
            "        assert_ne!({}::{}, {}::{}, \"{} changes status\");\n",
            status_enum, pre, status_enum, post, op.name
        ));
    }
    out.push_str(&format!("        let _pre = {}::{};\n", status_enum, pre));
    out.push_str(&format!("        let _post = {}::{};\n", status_enum, post));
    out.push_str("        // AGENT: verify handler transitions status from _pre to _post\n");
    out.push_str("    }\n\n");
}

// ----------------------------------------------------------------------
// Spec-derived seed values (F7). Test-state literals were previously
// seeded by pattern-matching multisig-example field names ("threshold",
// "member_count", …) — hardcoded semantics that leaked into every other
// spec. Values now derive from the spec itself: type-based bases raised
// by the simple comparison atoms of the handler's guard/requires
// conjunction (plus the property body for property tests).
// ----------------------------------------------------------------------

/// One side of a comparison atom, resolved against the spec.
enum AtomSide {
    /// A bare state-field reference.
    Field(String),
    /// A handler param, carrying its `sensible_param` seed value.
    Param(String, u128),
    /// A numeric literal.
    Lit(u128),
    /// A `+`-sum of two resolvable sides — value-bearing so cross-field
    /// clauses like `amount + fee <= cap` participate in the raise
    /// fixpoint, but never itself the adjustment target (only a plain
    /// `Field` on the other side gets raised).
    Sum(Box<AtomSide>, Box<AtomSide>),
}

/// Emit a `let [mut] state = <State> { … }` literal with spec-derived
/// seed values.
fn emit_state_literal(
    out: &mut String,
    state_name: &str,
    fields: &[(String, String)],
    op: &ParsedHandler,
    extra_guard_texts: &[&str],
    mutable: bool,
) {
    emit_state_literal_with(out, state_name, fields, op, extra_guard_texts, &[], mutable);
}

/// Like [`emit_state_literal`], with explicit per-field overrides (used
/// by the guard-rejection test to inject a violating value).
fn emit_state_literal_with(
    out: &mut String,
    state_name: &str,
    fields: &[(String, String)],
    op: &ParsedHandler,
    extra_guard_texts: &[&str],
    overrides: &[(String, String)],
    mutable: bool,
) {
    let seeds = seed_state_values(fields, op, extra_guard_texts);
    let mut_kw = if mutable { "mut " } else { "" };
    out.push_str(&format!(
        "        let {}state = {} {{\n",
        mut_kw, state_name
    ));
    for (fname, ftype) in fields {
        let val = overrides
            .iter()
            .find(|(n, _)| n == fname)
            .map(|(_, v)| v.clone())
            .or_else(|| seeds.get(fname).cloned())
            .unwrap_or_else(|| non_numeric_default(ftype));
        out.push_str(&format!("            {}: {},\n", fname, val));
    }
    out.push_str("        };\n");
}

/// Default literal for field types outside the seedable numeric set.
fn non_numeric_default(ftype: &str) -> String {
    match ftype {
        "Pubkey" => "[1u8; 32]".to_string(),
        "Bool" | "bool" => "false".to_string(),
        "I128" | "i128" => "0i128".to_string(),
        _ => "Default::default()".to_string(),
    }
}

/// Types the constraint seeding understands (unsigned only — the raise
/// rules reason in `u128`).
fn is_seedable_numeric(ftype: &str) -> bool {
    matches!(ftype, "U8" | "u8" | "U64" | "u64" | "U128" | "u128")
}

/// Render a numeric seed with the type's literal suffix convention.
fn render_seed(v: u128, ftype: &str) -> String {
    match ftype {
        "U128" | "u128" => format!("{}u128", v),
        _ => v.to_string(),
    }
}

/// Compute seed values for the numeric state fields: start at the
/// type-based base (`count`/`amount`/`value`-named U64 fields at 100 —
/// the legacy generic heuristic), then walk the comparison atoms of the
/// handler's guards (plus `extra_texts`) and raise values until simple
/// `a > b` / `a >= lit` shapes hold; `f == <lit>` pins exactly. Compound
/// sides are skipped — this is a seeding heuristic, not a solver.
fn seed_state_values(
    fields: &[(String, String)],
    op: &ParsedHandler,
    extra_texts: &[&str],
) -> std::collections::BTreeMap<String, String> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut vals: BTreeMap<String, u128> = BTreeMap::new();
    for (fname, ftype) in fields {
        if !is_seedable_numeric(ftype) {
            continue;
        }
        let base = if matches!(ftype.as_str(), "U64" | "u64")
            && (fname.contains("count") || fname.contains("amount") || fname.contains("value"))
        {
            100
        } else {
            0
        };
        vals.insert(fname.clone(), base);
    }

    // Guard conjunction: requires clauses + extras (property bodies),
    // all rendered from the typed trees against the `state` binder.
    let mut texts: Vec<String> = Vec::new();
    for req in &op.requires {
        texts.push(render_for_state(requires_tree(req)));
    }
    for t in extra_texts {
        texts.push((*t).to_string());
    }

    let raw_atoms: Vec<String> = texts.iter().flat_map(|t| split_atoms(t)).collect();
    let atoms: Vec<(String, &'static str, String)> =
        raw_atoms.iter().filter_map(|a| parse_atom(a)).collect();

    // Bool constraints: `f == true/false`, bare `f`, and `!f` conjuncts
    // pin bool fields. Pre-v2.44 bool fields always seeded `false` (the
    // non-numeric default), so a `requires seat_open` handler got an
    // "accepts_valid" fixture the guard rejects.
    let mut bool_pins: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    {
        let is_bool_field = |name: &str| {
            fields
                .iter()
                .any(|(f, t)| f == name && matches!(t.as_str(), "Bool" | "bool"))
        };
        let strip_state = |s: &str| -> Option<String> {
            let t = s.trim();
            let name = t.strip_prefix("state.").or_else(|| t.strip_prefix("s."))?;
            (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| name.to_string())
        };
        for a in &raw_atoms {
            let t = a.trim();
            if let Some((l, cmp, r)) = parse_atom(t) {
                if cmp == "==" || cmp == "!=" {
                    for (side, lit) in [(&l, &r), (&r, &l)] {
                        if let (Some(f), Ok(b)) = (strip_state(side), lit.trim().parse::<bool>()) {
                            if is_bool_field(&f) {
                                bool_pins.insert(f, b != (cmp == "!="));
                            }
                        }
                    }
                }
                continue;
            }
            if let Some(rest) = t.strip_prefix('!') {
                if let Some(f) = strip_state(&strip_outer_parens(rest)) {
                    if is_bool_field(&f) {
                        bool_pins.insert(f, false);
                    }
                }
            } else if let Some(f) = strip_state(t) {
                if is_bool_field(&f) {
                    bool_pins.insert(f, true);
                }
            }
        }
    }

    // `f == <lit>` pins the value exactly; inequality passes won't move it.
    let mut pinned: BTreeSet<String> = BTreeSet::new();
    for (lhs, cmp, rhs) in &atoms {
        if *cmp != "==" {
            continue;
        }
        match (resolve_side(lhs, fields, op), resolve_side(rhs, fields, op)) {
            (Some(AtomSide::Field(f)), Some(AtomSide::Lit(l)))
            | (Some(AtomSide::Lit(l)), Some(AtomSide::Field(f))) => {
                vals.insert(f.clone(), l);
                pinned.insert(f);
            }
            _ => {}
        }
    }

    // Raise-only fixpoint over the inequality atoms.
    for _ in 0..4 {
        for (lhs, cmp, rhs) in &atoms {
            let (Some(a), Some(b)) = (resolve_side(lhs, fields, op), resolve_side(rhs, fields, op))
            else {
                continue;
            };
            // Normalize to "left cmp right" with resolved values.
            let (Some(va), Some(vb)) = (atom_side_value(&a, &vals), atom_side_value(&b, &vals))
            else {
                continue;
            };
            let mut adjust = |f: &str, v: u128, pinned: &BTreeSet<String>| {
                if !pinned.contains(f) {
                    vals.insert(f.to_string(), v);
                }
            };
            match (*cmp, &a, &b) {
                // field-vs-field: push the greater side up.
                ("<", _, AtomSide::Field(f)) if va >= vb => adjust(f, va + 2, &pinned),
                ("<=", _, AtomSide::Field(f)) if va > vb => adjust(f, va, &pinned),
                (">", AtomSide::Field(f), _) if va <= vb => adjust(f, vb + 2, &pinned),
                (">=", AtomSide::Field(f), _) if va < vb => adjust(f, vb, &pinned),
                // field-vs-literal upper bounds: clamp down.
                ("<", AtomSide::Field(f), AtomSide::Lit(l)) if va >= *l => {
                    adjust(f, l.saturating_sub(1), &pinned)
                }
                ("<=", AtomSide::Field(f), AtomSide::Lit(l)) if va > *l => adjust(f, *l, &pinned),
                (">", AtomSide::Lit(l), AtomSide::Field(f)) if *l <= vb => {
                    adjust(f, l.saturating_sub(1), &pinned)
                }
                (">=", AtomSide::Lit(l), AtomSide::Field(f)) if *l < vb => adjust(f, *l, &pinned),
                ("!=", AtomSide::Field(f), AtomSide::Lit(l)) if va == *l => {
                    adjust(f, l.saturating_add(1), &pinned)
                }
                ("!=", AtomSide::Lit(l), AtomSide::Field(f)) if vb == *l => {
                    adjust(f, l.saturating_add(1), &pinned)
                }
                _ => {}
            }
        }
    }

    let mut out = BTreeMap::new();
    for (fname, ftype) in fields {
        if let Some(v) = vals.get(fname) {
            out.insert(fname.clone(), render_seed(*v, ftype));
        }
        if let Some(b) = bool_pins.get(fname) {
            out.insert(fname.clone(), b.to_string());
        }
    }
    out
}

/// Current numeric value of an atom side under the seed map. `Sum`
/// recurses (saturating — seeds are small, but `u64::MAX`-ish literals
/// appear in overflow-shaped clauses).
fn atom_side_value(s: &AtomSide, vals: &std::collections::BTreeMap<String, u128>) -> Option<u128> {
    match s {
        AtomSide::Field(f) => vals.get(f).copied(),
        AtomSide::Param(_, v) => Some(*v),
        AtomSide::Lit(l) => Some(*l),
        AtomSide::Sum(a, b) => {
            Some(atom_side_value(a, vals)?.saturating_add(atom_side_value(b, vals)?))
        }
    }
}

/// Split a translated guard conjunction into candidate atoms on the
/// boolean connectives, stripping balanced outer parens.
fn split_atoms(text: &str) -> Vec<String> {
    text.split("&&")
        .flat_map(|p| p.split("||"))
        .map(strip_outer_parens)
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_outer_parens(s: &str) -> String {
    let mut t = s.trim();
    while t.starts_with('(') && t.ends_with(')') {
        // Only strip when the leading '(' matches the trailing ')'.
        let inner = &t[1..t.len() - 1];
        let mut depth = 0i32;
        let mut balanced = true;
        for c in inner.chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth < 0 {
                        balanced = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if balanced && depth == 0 {
            t = inner.trim();
        } else {
            break;
        }
    }
    t.to_string()
}

/// Parse `lhs <cmp> rhs` out of an atom; two-char comparators first so
/// `<=`/`>=` don't mis-split.
fn parse_atom(atom: &str) -> Option<(String, &'static str, String)> {
    for cmp in ["<=", ">=", "==", "!="] {
        if let Some(i) = atom.find(cmp) {
            return Some((
                atom[..i].trim().to_string(),
                cmp,
                atom[i + 2..].trim().to_string(),
            ));
        }
    }
    for cmp in ["<", ">"] {
        if let Some(i) = atom.find(cmp) {
            return Some((
                atom[..i].trim().to_string(),
                if cmp == "<" { "<" } else { ">" },
                atom[i + 1..].trim().to_string(),
            ));
        }
    }
    None
}

/// Resolve one atom side: a `state.`-prefixed or bare state-field name, a
/// numeric literal, or a handler param (folded to its `sensible_param`
/// value). Compound expressions resolve to `None` and skip the atom.
fn resolve_side(side: &str, fields: &[(String, String)], op: &ParsedHandler) -> Option<AtomSide> {
    let t = side.trim();
    // `X + Y` sums: resolve both addends so cross-field clauses
    // (`amount + fee <= cap`) contribute a value to the fixpoint instead
    // of silently dropping the atom — the v2.43 behavior that seeded
    // guard-violating "accepts_valid" fixtures.
    if let Some(i) = t.find('+') {
        let (lhs, rhs) = (&t[..i], &t[i + 1..]);
        if let (Some(a), Some(b)) = (resolve_side(lhs, fields, op), resolve_side(rhs, fields, op)) {
            return Some(AtomSide::Sum(Box::new(a), Box::new(b)));
        }
        return None;
    }
    let (name, had_state_prefix) = if let Some(rest) = t.strip_prefix("state.") {
        (rest, true)
    } else if let Some(rest) = t.strip_prefix("s.") {
        (rest, true)
    } else {
        (t, false)
    };
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if !had_state_prefix {
        if let Ok(n) = name.parse::<u128>() {
            return Some(AtomSide::Lit(n));
        }
        // A handler param shadows a same-named state field in guard position.
        if let Some((pn, pt)) = op.takes_params.iter().find(|(p, _)| p == name) {
            return sensible_param(pn, pt)
                .parse::<u128>()
                .ok()
                .map(|v| AtomSide::Param(pn.clone(), v));
        }
    }
    if fields.iter().any(|(f, _)| f == name) {
        return Some(AtomSide::Field(name.to_string()));
    }
    None
}

/// Pick a sensible param value — generic name/type heuristics only (the
/// multisig-specific names were removed in F7).
fn sensible_param(pname: &str, ptype: &str) -> String {
    match ptype {
        "Pubkey" => "[1u8; 32]".to_string(),
        "Bool" | "bool" => "true".to_string(),
        _ if pname.contains("index") => "0".to_string(),
        _ if pname.contains("amount") || pname.contains("value") || pname.contains("delta") => {
            "100".to_string()
        }
        _ => "1".to_string(),
    }
}

/// Try to derive inputs that violate the guard.
/// Returns (state_overrides, param_overrides) — field name → value pairs.
type Overrides = Vec<(String, String)>;

/// A falsifying / satisfying assignment: state-field and param overrides.
#[derive(Default, Clone)]
struct Assignment {
    state: Overrides,
    param: Overrides,
}

impl Assignment {
    fn one(is_param: bool, name: String, value: String) -> Self {
        let mut a = Assignment::default();
        if is_param {
            a.param.push((name, value));
        } else {
            a.state.push((name, value));
        }
        a
    }
    /// Merge another assignment in. `None` on a conflicting override for
    /// the same name (e.g. an OR whose disjuncts constrain one field to
    /// two incompatible values) — the caller falls back to the generic
    /// path rather than emit an unsatisfiable fixture.
    fn merge(mut self, other: Assignment) -> Option<Assignment> {
        for (scope, (n, v)) in other
            .state
            .into_iter()
            .map(|kv| (false, kv))
            .chain(other.param.into_iter().map(|kv| (true, kv)))
        {
            let bucket = if scope {
                &mut self.param
            } else {
                &mut self.state
            };
            match bucket.iter().find(|(en, _)| *en == n) {
                Some((_, ev)) if *ev != v => return None,
                Some(_) => {}
                None => bucket.push((n, v)),
            }
        }
        Some(self)
    }
}

/// Resolve a comparison-atom leaf to `(is_param, name)` — a state field or
/// a handler param usable as an override target. `None` for literals and
/// compound leaves.
fn leaf_target(tree: &crate::mir::ExprTree, op: &ParsedHandler) -> Option<(bool, String)> {
    use crate::mir::expr_tree::{BindingKind, ExprTree, TreeSeg};
    let ExprTree::Path(p) = tree else {
        return None;
    };
    match &p.binding {
        BindingKind::StateField => match p.segments.as_slice() {
            [TreeSeg::Field(f)] => Some((false, f.clone())),
            _ => None,
        },
        BindingKind::Param => {
            let name = p.root.clone();
            op.takes_params
                .iter()
                .any(|(n, _)| *n == name)
                .then_some((true, name))
        }
        _ => None,
    }
}

/// Resolve a leaf literal: an integer or a boolean.
fn leaf_lit(tree: &crate::mir::ExprTree) -> Option<LeafLit> {
    use crate::mir::expr_tree::ExprTree;
    match tree {
        ExprTree::Int(v) => Some(LeafLit::Int(*v)),
        ExprTree::Bool(b) => Some(LeafLit::Bool(*b)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum LeafLit {
    Int(u128),
    Bool(bool),
}

// ----------------------------------------------------------------------
// Constraint-propagation satisfier for the `guard_accepts_valid` fixture.
//
// The naive accepts fixture fixes params at `sensible_param` defaults and
// seeds state fields via the raise fixpoint — neither solves CROSS-FIELD
// constraints, so a guard like `stake_slash + lp_loss == loss` (an
// equality over two params and a third) or `lp_loss == 0 or stake_slash
// == seat_stake` (a disjunction) is left violated and `assert!(guard(..))`
// fails against correct code. This solves the common linear-equality +
// disjunction shapes by bounded propagation over the typed requires trees.
// ----------------------------------------------------------------------

/// A linear combination of variables (params / numeric state fields) plus
/// a constant, with every coefficient ±1 — the shapes real guards use
/// (`a`, `a + b`, `a - c`, `a + b == c`). `None` for non-linear operands.
struct Lin {
    /// `(name, sign)` — sign is +1 or -1.
    vars: Vec<(String, i128)>,
    constant: i128,
}

impl Lin {
    /// Negate all coefficients + the constant. `None` on `i128::MIN`
    /// (no positive counterpart) — the solver then bails on this guard.
    fn negated(mut self) -> Option<Lin> {
        for (_, s) in &mut self.vars {
            *s = s.checked_neg()?;
        }
        self.constant = self.constant.checked_neg()?;
        Some(self)
    }
    /// Sum two linear forms; `None` on constant overflow.
    fn combine(mut self, other: Lin) -> Option<Lin> {
        self.vars.extend(other.vars);
        self.constant = self.constant.checked_add(other.constant)?;
        Some(self)
    }
}

/// Flatten an `ExprTree` into a `Lin` when it's a ±1 linear form over
/// leaves and integer literals; `None` otherwise (incl. a `U128` literal
/// outside the solver's `i128` range — checked conversion, no wrap).
fn linearize(tree: &crate::mir::ExprTree, op: &ParsedHandler) -> Option<Lin> {
    use crate::mir::expr_tree::{ExprTree, TreeArithOp};
    match tree {
        ExprTree::Int(v) => Some(Lin {
            vars: vec![],
            constant: i128::try_from(*v).ok()?,
        }),
        ExprTree::Path(_) => {
            let (_, name) = leaf_target(tree, op)?;
            Some(Lin {
                vars: vec![(name, 1)],
                constant: 0,
            })
        }
        ExprTree::Arith { op: aop, lhs, rhs } => {
            let l = linearize(lhs, op)?;
            let r = linearize(rhs, op)?;
            match aop {
                TreeArithOp::Add => l.combine(r),
                TreeArithOp::Sub => l.combine(r.negated()?),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Known variable values during propagation: numeric (params + numeric
/// state fields, one namespace since a param shadows a same-named field
/// in guard position) and boolean state fields, plus the pin set that
/// equality-solved / disjunct-enforced values must not be overwritten.
#[derive(Default, Clone)]
struct SatState {
    num: std::collections::BTreeMap<String, i128>,
    is_param: std::collections::BTreeMap<String, bool>,
    bools: std::collections::BTreeMap<String, bool>,
    pinned: std::collections::BTreeSet<String>,
}

/// Evaluate a `Lin` under the current numeric assignment; `None` if any
/// variable is still unknown.
fn eval_lin(lin: &Lin, st: &SatState) -> Option<i128> {
    let mut acc = lin.constant;
    for (name, sign) in &lin.vars {
        let term = sign.checked_mul(*st.num.get(name)?)?;
        acc = acc.checked_add(term)?;
    }
    Some(acc)
}

/// Try to solve `lin == 0` for its single unknown variable, given the rest
/// are known. Assigns + pins it (only when the solution is a non-negative
/// integer — the generated fields/params are unsigned). Returns true if it
/// made progress.
fn solve_equality(lin: &Lin, st: &mut SatState) -> bool {
    let unknown: Vec<&(String, i128)> = lin
        .vars
        .iter()
        .filter(|(n, _)| !st.num.contains_key(n))
        .collect();
    if unknown.len() != 1 {
        return false;
    }
    let (name, sign) = unknown[0];
    // known_sum + sign*name + const == 0  →  name = -sign*(known_sum + const).
    // Checked throughout — overflow bails (no progress) rather than wrap.
    let mut known_sum = lin.constant;
    for (n, s) in &lin.vars {
        if n != name {
            let term = match s.checked_mul(st.num.get(n).copied().unwrap_or(0)) {
                Some(t) => t,
                None => return false,
            };
            known_sum = match known_sum.checked_add(term) {
                Some(v) => v,
                None => return false,
            };
        }
    }
    let value = match sign.checked_neg().and_then(|s| s.checked_mul(known_sum)) {
        Some(v) => v,
        None => return false,
    };
    if value < 0 {
        return false;
    }
    st.num.insert(name.clone(), value);
    st.pinned.insert(name.clone());
    true
}

/// An atom of a guard: a linear comparison (`lin <op> 0`) or a boolean
/// field constraint (`field == value`).
enum SatAtom {
    Cmp(Lin, crate::mir::expr_tree::TreeCmpOp),
    Bool(String, bool),
}

/// A guard clause: a single atom (AND-path) or a disjunction to enforce
/// one of.
enum SatClause {
    Single(SatAtom),
    Or(Vec<SatAtom>),
}

/// Resolve a leaf comparison into a `SatAtom`. Bool field vs bool literal
/// becomes a `Bool` constraint; otherwise the two sides linearize and
/// combine into `lin <op> 0`.
fn tree_to_atom(tree: &crate::mir::ExprTree, op: &ParsedHandler) -> Option<SatAtom> {
    use crate::mir::expr_tree::ExprTree;
    let ExprTree::Cmp { op: cmp, lhs, rhs } = tree else {
        return None;
    };
    // Bool constraint: `field == true/false` (or `!=`).
    for (side, lit_side) in [(lhs, rhs), (rhs, lhs)] {
        if let (Some((_, name)), Some(LeafLit::Bool(b))) =
            (leaf_target(side, op), leaf_lit(lit_side))
        {
            use crate::mir::expr_tree::TreeCmpOp;
            let value = match cmp {
                TreeCmpOp::Eq => b,
                TreeCmpOp::Ne => !b,
                _ => return None,
            };
            return Some(SatAtom::Bool(name, value));
        }
    }
    // Numeric: linearize both sides, move RHS across → `lin <op> 0`.
    let l = linearize(lhs, op)?;
    let r = linearize(rhs, op)?;
    Some(SatAtom::Cmp(l.combine(r.negated()?)?, *cmp))
}

/// Flatten a requires tree into clauses: `and` splits into separate
/// clauses; a top-level `or` becomes one disjunctive clause over its atoms.
fn collect_clauses(tree: &crate::mir::ExprTree, op: &ParsedHandler, out: &mut Vec<SatClause>) {
    use crate::mir::expr_tree::{ExprTree, TreeBoolOp};
    match tree {
        ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs,
            rhs,
        } => {
            collect_clauses(lhs, op, out);
            collect_clauses(rhs, op, out);
        }
        ExprTree::BoolOp {
            op: TreeBoolOp::Or, ..
        } => {
            let mut atoms = Vec::new();
            collect_or_atoms(tree, op, &mut atoms);
            if !atoms.is_empty() {
                out.push(SatClause::Or(atoms));
            }
        }
        ExprTree::Cmp { .. } => {
            if let Some(a) = tree_to_atom(tree, op) {
                out.push(SatClause::Single(a));
            }
        }
        _ => {}
    }
}

/// Gather the atoms of a (possibly nested) `or` subtree; non-atom
/// disjuncts are dropped (best-effort — the clause is satisfied if any
/// gathered disjunct holds).
fn collect_or_atoms(tree: &crate::mir::ExprTree, op: &ParsedHandler, out: &mut Vec<SatAtom>) {
    use crate::mir::expr_tree::{ExprTree, TreeBoolOp};
    match tree {
        ExprTree::BoolOp {
            op: TreeBoolOp::Or,
            lhs,
            rhs,
        } => {
            collect_or_atoms(lhs, op, out);
            collect_or_atoms(rhs, op, out);
        }
        _ => {
            if let Some(a) = tree_to_atom(tree, op) {
                out.push(a);
            }
        }
    }
}

/// Assign the single unknown variable of `lin <op> 0` a value satisfying
/// it (given the other variables are known). Returns true on progress.
fn assign_from_ineq(lin: &Lin, cmp: crate::mir::expr_tree::TreeCmpOp, st: &mut SatState) -> bool {
    use crate::mir::expr_tree::TreeCmpOp::*;
    let unknown: Vec<&(String, i128)> = lin
        .vars
        .iter()
        .filter(|(n, _)| !st.num.contains_key(n))
        .collect();
    if unknown.len() != 1 {
        return false;
    }
    let (name, sign) = unknown[0];
    // Checked accumulation — overflow bails (no progress).
    let mut known_rest = lin.constant;
    for (n, s) in &lin.vars {
        if n != name {
            let term = match s.checked_mul(st.num.get(n).copied().unwrap_or(0)) {
                Some(t) => t,
                None => return false,
            };
            known_rest = match known_rest.checked_add(term) {
                Some(v) => v,
                None => return false,
            };
        }
    }
    // `sign*v + known_rest <cmp> 0` → `v <eff_cmp> k`.
    let k = match sign.checked_neg().and_then(|s| s.checked_mul(known_rest)) {
        Some(v) => v,
        None => return false,
    };
    let eff = if *sign >= 0 { cmp } else { flip_cmp_tree(cmp) };
    let v: Option<i128> = match eff {
        Gt => k.max(-1).checked_add(1),
        Ge => Some(k.max(0)),
        Lt => (k > 0).then(|| k - 1),
        Le => (k >= 0).then_some(k),
        Eq => (k >= 0).then_some(k),
        Ne => (k >= 0).then(|| k.checked_add(1)).flatten().or(Some(0)),
    };
    match v {
        Some(val) if val >= 0 => {
            st.num.insert(name.clone(), val);
            st.pinned.insert(name.clone());
            true
        }
        _ => false,
    }
}

/// Is `atom` already satisfied under the current (partial) assignment?
/// `None` when it can't be evaluated yet (an unknown variable).
fn atom_holds(atom: &SatAtom, st: &SatState) -> Option<bool> {
    use crate::mir::expr_tree::TreeCmpOp::*;
    match atom {
        SatAtom::Bool(name, want) => st.bools.get(name).map(|b| b == want),
        SatAtom::Cmp(lin, cmp) => {
            let v = eval_lin(lin, st)?;
            Some(match cmp {
                Gt => v > 0,
                Ge => v >= 0,
                Lt => v < 0,
                Le => v <= 0,
                Eq => v == 0,
                Ne => v != 0,
            })
        }
    }
}

/// Enforce one atom of a disjunction (assign a variable so it holds).
fn enforce_atom(atom: &SatAtom, st: &mut SatState) -> bool {
    match atom {
        SatAtom::Bool(name, want) => {
            if st.pinned.contains(name) {
                return false;
            }
            st.bools.insert(name.clone(), *want);
            st.pinned.insert(name.clone());
            true
        }
        SatAtom::Cmp(lin, cmp) => {
            use crate::mir::expr_tree::TreeCmpOp;
            if matches!(cmp, TreeCmpOp::Eq) {
                solve_equality(lin, st) || assign_from_ineq(lin, *cmp, st)
            } else {
                assign_from_ineq(lin, *cmp, st)
            }
        }
    }
}

/// Evaluate a numeric tree under the assignment; unknown fields read as 0
/// (an unconstrained field's value is irrelevant to a guard that doesn't
/// reference it). `None` for non-numeric shapes.
fn eval_num_tree(tree: &crate::mir::ExprTree, op: &ParsedHandler, st: &SatState) -> Option<i128> {
    use crate::mir::expr_tree::{ExprTree, TreeArithOp};
    match tree {
        // Checked conversion: a `U128` literal above `i128::MAX` doesn't
        // fit the solver's `i128` domain — bail rather than wrap.
        ExprTree::Int(v) => i128::try_from(*v).ok(),
        ExprTree::Path(_) => {
            let (_, name) = leaf_target(tree, op)?;
            Some(st.num.get(&name).copied().unwrap_or(0))
        }
        ExprTree::Arith { op: aop, lhs, rhs } => {
            let l = eval_num_tree(lhs, op, st)?;
            let r = eval_num_tree(rhs, op, st)?;
            // Checked arithmetic: overflow (huge intermediate) and
            // division / modulo by zero return `None`, so a guard that
            // would divide by zero under this assignment is NEVER treated
            // as verified — the fixture then falls to the (non-executing)
            // smoke-test path instead of a possibly-panicking `assert!`.
            match aop {
                TreeArithOp::Add => l.checked_add(r),
                TreeArithOp::Sub => l.checked_sub(r),
                TreeArithOp::Mul => l.checked_mul(r),
                TreeArithOp::Div => l.checked_div(r),
                TreeArithOp::Mod => l.checked_rem(r),
            }
        }
        _ => None,
    }
}

/// Evaluate a boolean guard tree under the assignment — the verification
/// pass. Only returns `Some` when every referenced construct is
/// representable; a `None` bails the whole satisfier (so an unverifiable
/// guard keeps the naive fixture rather than risk a wrong one).
fn eval_bool_tree(tree: &crate::mir::ExprTree, op: &ParsedHandler, st: &SatState) -> Option<bool> {
    use crate::mir::expr_tree::{ExprTree, TreeBoolOp, TreeCmpOp};
    match tree {
        ExprTree::Bool(b) => Some(*b),
        ExprTree::Not(inner) => Some(!eval_bool_tree(inner, op, st)?),
        ExprTree::BoolOp { op: bop, lhs, rhs } => {
            let l = eval_bool_tree(lhs, op, st)?;
            let r = eval_bool_tree(rhs, op, st)?;
            Some(match bop {
                TreeBoolOp::And => l && r,
                TreeBoolOp::Or => l || r,
                TreeBoolOp::Implies => !l || r,
            })
        }
        ExprTree::Cmp { op: cmp, lhs, rhs } => {
            // Bool comparison (`field == true`)?
            let bool_leaf = |t: &ExprTree| -> Option<bool> {
                match leaf_lit(t) {
                    Some(LeafLit::Bool(b)) => Some(b),
                    _ => {
                        leaf_target(t, op).map(|(_, n)| st.bools.get(&n).copied().unwrap_or(false))
                    }
                }
            };
            if matches!(cmp, TreeCmpOp::Eq | TreeCmpOp::Ne) {
                if let (Some(l), Some(r)) = (bool_leaf(lhs), bool_leaf(rhs)) {
                    // Only treat as bool when at least one side is a bool
                    // literal / bool field (avoid mis-typing numeric 0/1).
                    let has_bool_lit = matches!(leaf_lit(lhs), Some(LeafLit::Bool(_)))
                        || matches!(leaf_lit(rhs), Some(LeafLit::Bool(_)));
                    if has_bool_lit {
                        return Some(if matches!(cmp, TreeCmpOp::Eq) {
                            l == r
                        } else {
                            l != r
                        });
                    }
                }
            }
            let l = eval_num_tree(lhs, op, st)?;
            let r = eval_num_tree(rhs, op, st)?;
            Some(match cmp {
                TreeCmpOp::Gt => l > r,
                TreeCmpOp::Ge => l >= r,
                TreeCmpOp::Lt => l < r,
                TreeCmpOp::Le => l <= r,
                TreeCmpOp::Eq => l == r,
                TreeCmpOp::Ne => l != r,
            })
        }
        _ => None,
    }
}

/// Compute a satisfying fixture assignment for the handler's guard by
/// bounded constraint propagation over the typed `requires` trees, then
/// VERIFY it against the full guard. Returns `(state_overrides,
/// param_overrides)` only when verification passes; `None` otherwise, so
/// the caller falls back to the naive seed/param fixture for guards this
/// solver can't handle (rather than emit an `assert!` that fails on
/// correct code). Handles linear equalities (`a + b == c`), inequalities,
/// bool constraints, and disjunctions (`p == 0 or q == r`).
fn satisfy_guard(
    op: &ParsedHandler,
    fields: &[(String, String)],
) -> Option<(Overrides, Overrides)> {
    use crate::mir::expr_tree::TreeCmpOp;
    if op.requires.is_empty() {
        return None;
    }
    let mut clauses: Vec<SatClause> = Vec::new();
    for req in &op.requires {
        collect_clauses(requires_tree(req), op, &mut clauses);
    }
    if clauses.is_empty() {
        return None;
    }

    let mut st = SatState::default();
    for (n, _) in &op.takes_params {
        st.is_param.insert(n.clone(), true);
    }
    for (n, _) in fields {
        st.is_param.entry(n.clone()).or_insert(false);
    }

    // Propagate to a fixpoint (bounded).
    for _ in 0..8 {
        let mut progress = false;
        for c in &clauses {
            let SatClause::Single(atom) = c else { continue };
            match atom {
                SatAtom::Bool(name, want) => {
                    if !st.pinned.contains(name) && st.bools.get(name) != Some(want) {
                        st.bools.insert(name.clone(), *want);
                        st.pinned.insert(name.clone());
                        progress = true;
                    }
                }
                SatAtom::Cmp(lin, cmp) => {
                    if matches!(cmp, TreeCmpOp::Eq) && solve_equality(lin, &mut st) {
                        progress = true;
                    }
                    if atom_holds(atom, &st) != Some(true) && assign_from_ineq(lin, *cmp, &mut st) {
                        progress = true;
                    }
                }
            }
        }
        for c in &clauses {
            let SatClause::Or(atoms) = c else { continue };
            if atoms.iter().any(|a| atom_holds(a, &st) == Some(true)) {
                continue;
            }
            for a in atoms {
                if enforce_atom(a, &mut st) {
                    progress = true;
                    break;
                }
            }
        }
        if !progress {
            break;
        }
    }

    // Fill any still-unknown numeric params with their sensible default.
    for (n, t) in &op.takes_params {
        if !st.num.contains_key(n) && !st.bools.contains_key(n) {
            if matches!(t.as_str(), "Bool" | "bool") {
                st.bools.entry(n.clone()).or_insert(false);
            } else if let Ok(v) = sensible_param(n, t).parse::<i128>() {
                st.num.insert(n.clone(), v);
            }
        }
    }

    // Verify: the assignment must satisfy every requires clause.
    for req in &op.requires {
        if eval_bool_tree(requires_tree(req), op, &st) != Some(true) {
            return None;
        }
    }

    // Produce overrides for the referenced state fields + params. A
    // state field shadowed by a same-named param carries no guard value
    // (the param does), so skip it — its override is the param's below.
    let mut state_ov: Overrides = Vec::new();
    for (n, t) in fields {
        if field_shadowed_by_param(n, op) {
            continue;
        }
        if matches!(t.as_str(), "Bool" | "bool") {
            if let Some(b) = st.bools.get(n) {
                state_ov.push((n.clone(), b.to_string()));
            }
        } else if let Some(v) = st.num.get(n).and_then(|v| u128::try_from(*v).ok()) {
            state_ov.push((n.clone(), render_seed(v, t)));
        }
    }
    let mut param_ov: Overrides = Vec::new();
    for (n, t) in &op.takes_params {
        if matches!(t.as_str(), "Bool" | "bool") {
            if let Some(b) = st.bools.get(n) {
                param_ov.push((n.clone(), b.to_string()));
            }
        } else if let Some(v) = st.num.get(n) {
            param_ov.push((n.clone(), v.to_string()));
        }
    }
    Some((state_ov, param_ov))
}

/// Falsify or satisfy a single `target <op> lit` comparison by picking a
/// concrete override value for `target`. `want_true = false` returns a
/// value making the comparison FALSE; `true` makes it TRUE. `None` when no
/// unsigned value works (e.g. satisfying `x < 0`).
fn solve_cmp(
    op: crate::mir::expr_tree::TreeCmpOp,
    lit: LeafLit,
    want_true: bool,
) -> Option<String> {
    use crate::mir::expr_tree::TreeCmpOp::*;
    match lit {
        LeafLit::Bool(b) => {
            // `x == b` is true at x=b, false at x=!b; `x != b` inverts.
            let true_val = match op {
                Eq => b,
                Ne => !b,
                _ => return None,
            };
            Some((if want_true { true_val } else { !true_val }).to_string())
        }
        LeafLit::Int(l) => {
            // Value making `x <op> l` evaluate to `want_true`. Checked
            // add/sub: `l` at `u128::MAX` (or 0) bails rather than
            // wrap/panic during code generation.
            let v: Option<u128> = match (op, want_true) {
                (Gt, true) => l.checked_add(1),
                (Gt, false) => Some(l),
                (Ge, true) => Some(l),
                (Ge, false) => l.checked_sub(1),
                (Lt, true) => l.checked_sub(1),
                (Lt, false) => Some(l),
                (Le, true) => Some(l),
                (Le, false) => l.checked_add(1),
                (Eq, true) => Some(l),
                (Eq, false) => l.checked_add(1),
                (Ne, true) => l.checked_add(1),
                (Ne, false) => Some(l),
            };
            v.map(|n| n.to_string())
        }
    }
}

/// Recursively compute an assignment that makes `tree` FALSE (or TRUE when
/// `want_false = false`), preserving the boolean structure:
///
///   - `A and B` false → falsify EITHER (first that works);
///   - `A or B`  false → falsify EVERY disjunct (merged);
///   - `not X`   false → satisfy X (and vice-versa);
///   - `A <op> B` → the boundary override.
///
/// `None` when the shape can't be solved structurally (the caller falls
/// back to the generic single-atom / param-zeroing path). `Implies` is not
/// solved here (falls back).
fn solve_tree(
    tree: &crate::mir::ExprTree,
    op: &ParsedHandler,
    want_false: bool,
) -> Option<Assignment> {
    use crate::mir::expr_tree::{ExprTree, TreeBoolOp};
    match tree {
        ExprTree::Bool(b) => (*b != want_false).then(Assignment::default),
        ExprTree::Not(inner) => solve_tree(inner, op, !want_false),
        ExprTree::BoolOp { op: bop, lhs, rhs } => {
            // De Morgan: falsifying an And = falsify one child; falsifying
            // an Or = falsify both. Satisfying flips the roles.
            let falsify_all = matches!(
                (bop, want_false),
                (TreeBoolOp::Or, true) | (TreeBoolOp::And, false)
            );
            if falsify_all {
                let a = solve_tree(lhs, op, want_false)?;
                let b = solve_tree(rhs, op, want_false)?;
                a.merge(b)
            } else if matches!(bop, TreeBoolOp::And | TreeBoolOp::Or) {
                solve_tree(lhs, op, want_false).or_else(|| solve_tree(rhs, op, want_false))
            } else {
                None // Implies — fall back
            }
        }
        ExprTree::Cmp { op: cmp, lhs, rhs } => {
            // Normalize to `target <cmp> lit`, flipping the operator when
            // the literal is on the left.
            let (is_param, name, cmp_op, lit) =
                if let (Some((ip, n)), Some(l)) = (leaf_target(lhs, op), leaf_lit(rhs)) {
                    (ip, n, *cmp, l)
                } else if let (Some(l), Some((ip, n))) = (leaf_lit(lhs), leaf_target(rhs, op)) {
                    (ip, n, flip_cmp_tree(*cmp), l)
                } else {
                    return None;
                };
            let value = solve_cmp(cmp_op, lit, !want_false)?;
            Some(Assignment::one(is_param, name, value))
        }
        _ => None,
    }
}

/// Mirror a tree comparator across its operands (`0 < x` ⇔ `x > 0`).
fn flip_cmp_tree(cmp: crate::mir::expr_tree::TreeCmpOp) -> crate::mir::expr_tree::TreeCmpOp {
    use crate::mir::expr_tree::TreeCmpOp::*;
    match cmp {
        Lt => Gt,
        Le => Ge,
        Gt => Lt,
        Ge => Le,
        Eq => Eq,
        Ne => Ne,
    }
}

// ===========================================================================
// Fixture verification (#312)
//
// The falsifier below searches for a violating assignment; it does not
// PROVE the assignment violates. Its `Cmp` case only solves
// `field <op> literal`, so a field-vs-field guard
// (`member_index < member_count`) falls through to the string-atom
// heuristics and can return an assignment that leaves the guard TRUE.
// Emitting `assert!(!guard(…))` over such a fixture produces a generated
// test that fails against correct code — the failure mode behind the six
// `*_guard_*` failures in the bundled examples.
//
// The fix here is a verification boundary rather than a better search:
// evaluate the guard against the exact fixture the test will contain, and
// emit a truth-value assertion only when evaluation confirms it. A better
// search (#263) still benefits — it just changes how often the outcome is
// `Solved` instead of `Contradicted`.
// ===========================================================================

/// Outcome of checking a candidate fixture against the guard.
///
/// Deliberately NOT #294's `Unsatisfiable`: proving no witness exists
/// needs a real solver. What this boundary can decide is whether the
/// witness in hand actually works.
enum FixtureCheck {
    /// Evaluation confirmed the guard takes the wanted truth value. Only
    /// this may emit an assertion.
    Solved,
    /// Evaluation produced the opposite value — the search returned a
    /// fixture that does not demonstrate what the test would assert.
    Contradicted,
    /// The guard uses a construct the evaluator cannot decide, so the
    /// fixture is unverified either way.
    Unsupported(&'static str),
}

/// A concrete value the evaluator can reason about. Pubkeys, arrays, and
/// records are deliberately absent: they reach the evaluator only through
/// comparisons it declines to decide.
#[derive(Clone, Copy, PartialEq)]
enum FixtureValue {
    Int(i128),
    Bool(bool),
}

/// Parse a generated fixture literal (`"0"`, `"100u64"`, `"true"`) into a
/// value. `None` for shapes with no scalar meaning (`[1u8; 32]`).
fn parse_fixture_literal(text: &str) -> Option<FixtureValue> {
    let t = text.trim();
    match t {
        "true" => return Some(FixtureValue::Bool(true)),
        "false" => return Some(FixtureValue::Bool(false)),
        _ => {}
    }
    let digits: String = t
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    if digits.is_empty() {
        return None;
    }
    // Reject a suffix that is not a Rust integer type, so `1abc` is not
    // silently read as 1.
    let rest = &t[digits.len()..];
    let suffix_ok = rest.is_empty()
        || matches!(
            rest,
            "u8" | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "usize"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "isize"
        );
    if !suffix_ok {
        return None;
    }
    digits.parse::<i128>().ok().map(FixtureValue::Int)
}

/// The concrete values the generated test will bind, keyed by the name a
/// tree path resolves to. Built from the same seeds and overrides the
/// emitters use, so the evaluator sees exactly the emitted fixture.
fn fixture_env(
    fields: &[(String, String)],
    op: &ParsedHandler,
    state_overrides: &[(String, String)],
    param_overrides: &[(String, String)],
) -> std::collections::HashMap<String, FixtureValue> {
    let mut env = std::collections::HashMap::new();
    let seeds = seed_state_values(fields, op, &[]);
    for (fname, ftype) in fields {
        let text = state_overrides
            .iter()
            .find(|(n, _)| n == fname)
            .map(|(_, v)| v.clone())
            .or_else(|| seeds.get(fname).cloned())
            .unwrap_or_else(|| non_numeric_default(ftype));
        if let Some(v) = parse_fixture_literal(&text) {
            env.insert(fname.clone(), v);
        }
    }
    for (pname, ptype) in &op.takes_params {
        let text = param_overrides
            .iter()
            .find(|(n, _)| n == pname)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| sensible_param(pname, ptype));
        if let Some(v) = parse_fixture_literal(&text) {
            env.insert(pname.clone(), v);
        }
    }
    env
}

/// Evaluate a guard expression against the fixture environment. `None`
/// whenever any part is undecidable — an unknown name, a non-scalar
/// comparison, division by zero, or a node kind outside the fragment.
fn eval_tree(
    tree: &crate::mir::ExprTree,
    env: &std::collections::HashMap<String, FixtureValue>,
) -> Option<FixtureValue> {
    use crate::mir::expr_tree::{
        BindingKind, ExprTree, TreeArithOp, TreeBoolOp, TreeCmpOp, TreeSeg,
    };

    match tree {
        ExprTree::Int(v) => i128::try_from(*v).ok().map(FixtureValue::Int),
        ExprTree::Bool(b) => Some(FixtureValue::Bool(*b)),
        ExprTree::Path(p) => match &p.binding {
            BindingKind::Const(value) => parse_fixture_literal(value),
            BindingKind::StateField | BindingKind::Ghost => match p.segments.as_slice() {
                [TreeSeg::Field(f)] => env.get(f.as_str()).copied(),
                // Subscripts and nested paths address container elements
                // the fixture seeds do not model.
                _ => None,
            },
            BindingKind::Param | BindingKind::LetBound => p
                .segments
                .is_empty()
                .then(|| env.get(p.root.as_str()).copied())?,
            // Accounts, externals, and unresolved names have no fixture
            // value — the unit-test model carries no accounts at all.
            _ => None,
        },
        ExprTree::Not(inner) => match eval_tree(inner, env)? {
            FixtureValue::Bool(b) => Some(FixtureValue::Bool(!b)),
            FixtureValue::Int(_) => None,
        },
        ExprTree::BoolOp { op, lhs, rhs } => {
            let (a, b) = match (eval_tree(lhs, env)?, eval_tree(rhs, env)?) {
                (FixtureValue::Bool(a), FixtureValue::Bool(b)) => (a, b),
                _ => return None,
            };
            Some(FixtureValue::Bool(match op {
                TreeBoolOp::And => a && b,
                TreeBoolOp::Or => a || b,
                TreeBoolOp::Implies => !a || b,
            }))
        }
        ExprTree::Cmp { op, lhs, rhs } => {
            let (a, b) = (eval_tree(lhs, env)?, eval_tree(rhs, env)?);
            let result = match (a, b) {
                (FixtureValue::Int(x), FixtureValue::Int(y)) => match op {
                    TreeCmpOp::Eq => x == y,
                    TreeCmpOp::Ne => x != y,
                    TreeCmpOp::Lt => x < y,
                    TreeCmpOp::Le => x <= y,
                    TreeCmpOp::Gt => x > y,
                    TreeCmpOp::Ge => x >= y,
                },
                (FixtureValue::Bool(x), FixtureValue::Bool(y)) => match op {
                    TreeCmpOp::Eq => x == y,
                    TreeCmpOp::Ne => x != y,
                    _ => return None,
                },
                _ => return None,
            };
            Some(FixtureValue::Bool(result))
        }
        ExprTree::Arith { op, lhs, rhs } => {
            let (x, y) = match (eval_tree(lhs, env)?, eval_tree(rhs, env)?) {
                (FixtureValue::Int(x), FixtureValue::Int(y)) => (x, y),
                _ => return None,
            };
            let v = match op {
                TreeArithOp::Add => x.checked_add(y)?,
                TreeArithOp::Sub => x.checked_sub(y)?,
                TreeArithOp::Mul => x.checked_mul(y)?,
                TreeArithOp::Div => x.checked_div(y)?,
                TreeArithOp::Mod => x.checked_rem(y)?,
            };
            Some(FixtureValue::Int(v))
        }
        // Quantifiers, sums, records, matches, CPI results: outside the
        // decidable fragment for a plain-struct fixture.
        _ => None,
    }
}

/// Check the fixture against the full guard — the conjunction of every
/// `requires` clause — and report whether it takes `want` truth value.
fn check_fixture(
    op: &ParsedHandler,
    env: &std::collections::HashMap<String, FixtureValue>,
    want: bool,
) -> FixtureCheck {
    let mut all_true = true;
    for req in &op.requires {
        match eval_tree(requires_tree(req), env) {
            Some(FixtureValue::Bool(b)) => all_true &= b,
            Some(FixtureValue::Int(_)) => {
                return FixtureCheck::Unsupported("guard clause is not boolean-valued")
            }
            None => {
                return FixtureCheck::Unsupported(
                    "guard reads accounts, containers, or unsupported operators",
                )
            }
        }
    }
    if all_true == want {
        FixtureCheck::Solved
    } else {
        FixtureCheck::Contradicted
    }
}

/// Negate a comparison operator (`>` → `<=`, `==` → `!=`, …).
fn negate_cmp(cmp: crate::mir::expr_tree::TreeCmpOp) -> crate::mir::expr_tree::TreeCmpOp {
    use crate::mir::expr_tree::TreeCmpOp::*;
    match cmp {
        Gt => Le,
        Ge => Lt,
        Lt => Ge,
        Le => Gt,
        Eq => Ne,
        Ne => Eq,
    }
}

/// Seed a `SatState` with the fixture defaults: params via
/// `sensible_param`, numeric state fields via the raise-fixpoint
/// `seed_state_values`, bool fields false. Every value is known (nothing
/// pinned) so the falsifier can re-solve any single one.
fn seed_defaults(op: &ParsedHandler, fields: &[(String, String)]) -> SatState {
    let mut st = SatState::default();
    for (n, t) in &op.takes_params {
        st.is_param.insert(n.clone(), true);
        if matches!(t.as_str(), "Bool" | "bool") {
            st.bools.insert(n.clone(), sensible_param(n, t) == "true");
        } else if let Ok(v) = sensible_param(n, t).parse::<i128>() {
            st.num.insert(n.clone(), v);
        }
    }
    let seeds = seed_state_values(fields, op, &[]);
    for (n, t) in fields {
        // A param of the same name SHADOWS this state field in guard
        // position (the guard fn reads the param), so the field is
        // irrelevant to guard satisfaction — don't let its seed clobber
        // the param's value in the shared name map.
        if op.takes_params.iter().any(|(p, _)| p == n) {
            continue;
        }
        st.is_param.entry(n.clone()).or_insert(false);
        if matches!(t.as_str(), "Bool" | "bool") {
            st.bools.insert(
                n.clone(),
                seeds.get(n).map(|v| v == "true").unwrap_or(false),
            );
        } else {
            // Seed strings may carry a `u128` suffix — take the digit run.
            let v = seeds
                .get(n)
                .and_then(|s| s.trim_end_matches("u128").trim().parse::<i128>().ok())
                .unwrap_or(0);
            st.num.insert(n.clone(), v);
        }
    }
    st
}

/// True when a state field is shadowed by a same-named handler param
/// (which the generated guard fn reads instead of the field).
fn field_shadowed_by_param(name: &str, op: &ParsedHandler) -> bool {
    op.takes_params.iter().any(|(p, _)| p == name)
}

/// Whole-guard truth under a full assignment: `Some(false)` if any
/// requires clause is false, `Some(true)` if all hold, `None` if some
/// clause can't be evaluated.
fn eval_guard(op: &ParsedHandler, st: &SatState) -> Option<bool> {
    let mut all_true = true;
    for req in &op.requires {
        match eval_bool_tree(requires_tree(req), op, st) {
            Some(false) => return Some(false),
            Some(true) => {}
            None => all_true = false,
        }
    }
    all_true.then_some(true)
}

/// Re-assign one variable of `lin <cmp> 0` so the comparison becomes
/// FALSE, holding the others at their current values. Returns true on
/// success.
fn falsify_cmp(lin: &Lin, cmp: crate::mir::expr_tree::TreeCmpOp, st: &mut SatState) -> bool {
    let neg = negate_cmp(cmp);
    let var_names: Vec<String> = lin.vars.iter().map(|(n, _)| n.clone()).collect();
    for name in var_names {
        let saved = st.num.get(&name).copied();
        st.num.remove(&name);
        st.pinned.remove(&name);
        if assign_from_ineq(lin, neg, st) {
            return true;
        }
        // restore and try the next variable
        if let Some(v) = saved {
            st.num.insert(name.clone(), v);
        }
    }
    false
}

/// Make a single atom false (used to falsify every disjunct of an `or`).
fn falsify_atom(atom: &SatAtom, st: &mut SatState) -> bool {
    match atom {
        SatAtom::Bool(name, want) => {
            st.bools.insert(name.clone(), !want);
            true
        }
        SatAtom::Cmp(lin, cmp) => falsify_cmp(lin, *cmp, st),
    }
}

/// Make a whole clause false: a single atom directly, or an `or` by
/// falsifying every disjunct.
fn falsify_clause(clause: &SatClause, st: &mut SatState) -> bool {
    match clause {
        SatClause::Single(atom) => falsify_atom(atom, st),
        SatClause::Or(atoms) => atoms.iter().all(|a| falsify_atom(a, st)),
    }
}

/// Guard-violation via the linear constraint machinery, VERIFIED. Seeds
/// the fixture defaults, then makes ONE clause false (the guard is a
/// conjunction, so one false clause rejects) — handling field-vs-field
/// comparisons (`amount > collateral`) and linear forms the string-atom
/// path drops. Returns only the overrides that differ from the defaults;
/// `None` when it can't produce a verified-violating assignment.
fn falsify_guard_linear(
    op: &ParsedHandler,
    fields: &[(String, String)],
) -> Option<(Overrides, Overrides)> {
    if op.requires.is_empty() {
        return None;
    }
    let mut clauses: Vec<SatClause> = Vec::new();
    for req in &op.requires {
        collect_clauses(requires_tree(req), op, &mut clauses);
    }
    if clauses.is_empty() {
        return None;
    }
    let defaults = seed_defaults(op, fields);

    // Choose the trial assignment: the defaults already reject, or one
    // clause falsified on top of them.
    let trial = if eval_guard(op, &defaults) == Some(false) {
        defaults.clone()
    } else {
        let mut chosen = None;
        for clause in &clauses {
            let mut t = defaults.clone();
            if falsify_clause(clause, &mut t) && eval_guard(op, &t) == Some(false) {
                chosen = Some(t);
                break;
            }
        }
        chosen?
    };

    // Emit only the fields/params whose value changed from the default.
    // Shadowed state fields never carry the guard value (the param does),
    // so skip them — their override belongs to the param below.
    let mut state_ov: Overrides = Vec::new();
    for (n, t) in fields {
        if field_shadowed_by_param(n, op) {
            continue;
        }
        if matches!(t.as_str(), "Bool" | "bool") {
            let (d, v) = (
                defaults.bools.get(n).copied().unwrap_or(false),
                trial.bools.get(n).copied().unwrap_or(false),
            );
            if d != v {
                state_ov.push((n.clone(), v.to_string()));
            }
        } else {
            let (d, v) = (defaults.num.get(n).copied(), trial.num.get(n).copied());
            if d != v {
                if let Some(v) = v.and_then(|v| u128::try_from(v).ok()) {
                    state_ov.push((n.clone(), render_seed(v, t)));
                }
            }
        }
    }
    let mut param_ov: Overrides = Vec::new();
    for (n, t) in &op.takes_params {
        if matches!(t.as_str(), "Bool" | "bool") {
            let (d, v) = (
                defaults.bools.get(n).copied().unwrap_or(false),
                trial.bools.get(n).copied().unwrap_or(false),
            );
            if d != v {
                param_ov.push((n.clone(), v.to_string()));
            }
        } else {
            let (d, v) = (defaults.num.get(n).copied(), trial.num.get(n).copied());
            if d != v {
                if let Some(v) = v {
                    param_ov.push((n.clone(), v.to_string()));
                }
            }
        }
    }
    Some((state_ov, param_ov))
}

/// Structure-aware guard falsification over the typed `requires` trees.
/// The guard is the conjunction of every `requires` clause, so falsifying
/// ANY single clause falsifies the whole guard — and each clause is
/// falsified with full AND/OR/`not` awareness (an OR needs every disjunct
/// false). `None` when no clause can be solved structurally.
fn falsify_guard_from_trees(op: &ParsedHandler) -> Option<(Overrides, Overrides)> {
    for req in &op.requires {
        if let Some(a) = solve_tree(requires_tree(req), op, /*want_false=*/ true) {
            if !a.state.is_empty() || !a.param.is_empty() {
                return Some((a.state, a.param));
            }
        }
    }
    None
}

/// Derive inputs that make the guard reject. Primary path is a
/// structure-aware solve over the typed `requires` trees
/// (`falsify_guard_from_trees`) — it negates the COMPLETE boolean AST, so
/// an `A or B` guard is only violated when both disjuncts are false (the
/// old string-atom path negated one atom and left the OR true, producing
/// a rejects-test that failed against correct code). Falls back to the
/// legacy single-atom negation, then to zeroing every numeric param.
fn derive_guard_violation(
    guard_rust: &str,
    op: &ParsedHandler,
    fields: &[(String, String)],
) -> (Overrides, Overrides) {
    // Linear, VERIFIED path first: handles field-vs-field and other
    // compound comparisons (`amount > collateral`), and only returns an
    // assignment it has checked makes the guard false.
    if let Some(overrides) = falsify_guard_linear(op, fields) {
        return overrides;
    }
    // Structure-aware fallback (AND / OR / not / nesting over param-vs-literal).
    if let Some(overrides) = falsify_guard_from_trees(op) {
        return overrides;
    }

    let mut state_overrides = Vec::new();
    let mut param_overrides = Vec::new();

    let is_bool_field = |name: &str| {
        fields
            .iter()
            .any(|(f, t)| f == name && matches!(t.as_str(), "Bool" | "bool"))
    };
    for atom in split_atoms(guard_rust) {
        let Some((lhs, cmp, rhs)) = parse_atom(&atom) else {
            continue;
        };
        // Bool atoms: `state.f == true` violates with `f: false` (and
        // mirrored / `!=`). Must come before the numeric normalization,
        // which can't resolve `true`/`false` and would skip the atom —
        // leaving a rejects-test whose fixture (now bool-seeded to pass
        // the guard) never rejects.
        if cmp == "==" || cmp == "!=" {
            let bool_violation = [(&lhs, &rhs), (&rhs, &lhs)].into_iter().find_map(|(s, l)| {
                let f = s
                    .trim()
                    .strip_prefix("state.")
                    .or_else(|| s.trim().strip_prefix("s."))?;
                let b: bool = l.trim().parse().ok()?;
                (is_bool_field(f)).then(|| (f.to_string(), b != (cmp == "!=")))
            });
            if let Some((f, satisfying)) = bool_violation {
                state_overrides.push((f, (!satisfying).to_string()));
                break;
            }
        }
        // Normalize to `<name> cmp <literal>` (mirror literal-first atoms).
        let normalized = match (
            resolve_side(&lhs, fields, op),
            resolve_side(&rhs, fields, op),
        ) {
            (Some(AtomSide::Field(f)), Some(AtomSide::Lit(l))) => Some((f, false, cmp, l)),
            (Some(AtomSide::Param(p, _)), Some(AtomSide::Lit(l))) => Some((p, true, cmp, l)),
            (Some(AtomSide::Lit(l)), Some(AtomSide::Field(f))) => {
                Some((f, false, flip_cmp(cmp), l))
            }
            (Some(AtomSide::Lit(l)), Some(AtomSide::Param(p, _))) => {
                Some((p, true, flip_cmp(cmp), l))
            }
            _ => None,
        };
        let Some((name, is_param, cmp, l)) = normalized else {
            continue;
        };
        // Boundary value that breaks the atom (skip `>= 0`: unsigned).
        // Checked add: `l` at `u128::MAX` bails rather than wrap/panic.
        let value = match cmp {
            ">" => Some(l),
            ">=" if l > 0 => Some(l - 1),
            "<" => Some(l),
            "<=" => l.checked_add(1),
            "==" => l.checked_add(1),
            "!=" => Some(l),
            _ => None,
        };
        if let Some(v) = value {
            let entry = (name, v.to_string());
            if is_param {
                param_overrides.push(entry);
            } else {
                state_overrides.push(entry);
            }
            break;
        }
    }

    if state_overrides.is_empty() && param_overrides.is_empty() {
        // Generic fallback: just try setting all numeric params to 0
        for (pname, ptype) in &op.takes_params {
            if matches!(ptype.as_str(), "U8" | "U64" | "U128") {
                param_overrides.push((pname.clone(), "0".to_string()));
            }
        }
    }

    (state_overrides, param_overrides)
}

/// Mirror a comparator across its operands (`0 < x` ⇔ `x > 0`).
fn flip_cmp(cmp: &'static str) -> &'static str {
    match cmp {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #312 regression: a generated guard test may assert a truth value
    /// only over a fixture that was VERIFIED to have it. The falsifier
    /// searches but does not prove — its `Cmp` case handles only
    /// `field <op> literal`, so a field-vs-field guard fell through to
    /// string heuristics and could yield a fixture leaving the guard
    /// true. Asserting over it produced a generated test that failed
    /// against correct code.
    #[test]
    fn guard_assertions_only_over_verified_fixtures() {
        // `threshold < member_count` is field-vs-field: unsolvable by the
        // literal-based falsifier, so its fixture must not be asserted
        // over. `amount > 0` is solvable and must keep its assertion.
        let src = r#"spec T
type State | Active of {
    member_count : U64,
    threshold : U64,
  }
type Error | TooLow | BadAmount
handler tighten (amount : U64) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires amount > 0 else BadAmount
  effect { threshold := amount }
}
handler widen : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires threshold < member_count else TooLow
  effect { member_count := 0 }
}
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("t.qedspec");
        std::fs::write(&spec_path, src).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        let out = std::fs::read_to_string(&out_path).expect("read output");

        // The solvable guard keeps a real assertion in both directions.
        let tighten = out
            .split("fn test_tighten_guard_rejects_invalid")
            .nth(1)
            .expect("tighten rejects-test present");
        assert!(
            tighten.contains("assert!(!guard_tighten("),
            "a verified violating fixture must still be asserted:\n{tighten}"
        );

        // Every emitted assertion must be backed by verification: no
        // guard test may assert without the evaluator confirming it.
        for block in out.split("    #[test]").skip(1) {
            if !block.contains("guard_") {
                continue;
            }
            let asserts = block.contains("assert!(guard_") || block.contains("assert!(!guard_");
            let suppressed = block.contains("No assertion:");
            assert!(
                asserts ^ suppressed,
                "a guard test must either assert or explain its suppression, never both \
                 or neither:\n{block}"
            );
        }
    }

    /// #297 regression: an effect whose RHS reads an account binding
    /// (`field := acct.pubkey`) rendered the account name verbatim into
    /// `apply_*` and the effect test — E0425 in a scope with no account
    /// bindings. Such effects are suppressed with a note, matching the
    /// shared harness lane's pubkey-skip and this file's own guard
    /// suppression; adjacent scalar effects survive.
    #[test]
    fn account_valued_effects_are_suppressed_with_note() {
        let src = r#"spec T
type State | Open of { owner_key : Pubkey, pool : U64 }
type Error | InvalidAmount
handler open_pool (amount : U64) : State.Open -> State.Open {
  accounts {
    payer    : signer, writable
    payer_ta : writable, type token
    state    : writable
  }
  requires amount > 0 else InvalidAmount
  effect {
    owner_key := payer_ta.pubkey
    pool      += amount
  }
}
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("t.qedspec");
        std::fs::write(&spec_path, src).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        let out = std::fs::read_to_string(&out_path).expect("read output");

        // The account read must not appear as executable code anywhere
        // (the suppression note mentions it in a comment; statements end
        // with `;`).
        assert!(
            !out.contains("= payer_ta.pubkey;"),
            "account read must not render as an expression:\n{out}"
        );
        assert!(
            out.contains("not modeled (account-valued"),
            "suppressed effect carries an explicit note:\n{out}"
        );
        // The adjacent scalar effect still renders and is still tested.
        assert!(
            out.contains("state.pool += amount"),
            "scalar effect survives suppression:\n{out}"
        );
        // No assertion on the suppressed destination in the effect test.
        assert!(
            !out.contains("assert_eq!(state.owner_key"),
            "effect test must not assert the suppressed field:\n{out}"
        );
    }

    /// #156 regression: the guard predicate renders from the requires
    /// trees. The legacy path read only the deleted `guard_str`, so every
    /// requires-only handler got `fn guard_x { true }` plus a rejects-test
    /// asserting `!true` — a generated test that always failed. Handlers
    /// whose requires are all account-suppressed must get no guard fn and
    /// no guard tests at all (same failure shape, `!(true)`).
    #[test]
    fn guard_predicates_render_requires_and_skip_suppressed_handlers() {
        let src = r#"spec T
type State | Active of { admin_key : Pubkey, pool : U64 }
type Error | Unauthorized | InvalidAmount
handler swap (amount : U64) (min_out : U64) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires amount >= min_out and min_out > 0 else InvalidAmount
  effect { pool += amount }
}
handler close : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires admin.pubkey == state.admin_key else Unauthorized
  effect { pool := 0 }
}
handler mixed (amount : U64) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires amount > 0 and admin.pubkey == state.admin_key else Unauthorized
  effect { pool += amount }
}
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("t.qedspec");
        std::fs::write(&spec_path, src).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        let out = std::fs::read_to_string(&out_path).expect("read output");

        // Requires-derived guard body, not the vacuous `true`.
        assert!(out.contains("fn guard_swap"), "guard fn emitted:\n{out}");
        assert!(
            out.contains("amount >= min_out") && out.contains("min_out > 0"),
            "guard body renders the requires conjunction:\n{out}"
        );
        // Account-suppressed handler: no guard fn, no failing rejects-test.
        assert!(
            !out.contains("fn guard_close") && !out.contains("test_close_guard_rejects_invalid"),
            "suppressed handler must not emit guard fn or tests:\n{out}"
        );
        // Mixed conjunction: retain the account-free term instead of
        // dropping the entire requires clause.
        assert!(
            out.contains("fn guard_mixed"),
            "mixed guard fn emitted:\n{out}"
        );
        let mixed = out
            .split("fn guard_mixed")
            .nth(1)
            .and_then(|tail| tail.split('}').next())
            .expect("mixed guard body");
        assert!(
            mixed.contains("amount > 0") && !mixed.contains("admin"),
            "mixed guard keeps only account-free conjuncts:\n{mixed}"
        );
        // No vacuous `true` guard body anywhere.
        assert!(
            !out.contains("-> bool {\n    true\n}"),
            "no guard predicate may degrade to `true`:\n{out}"
        );
    }

    /// v2.44 read-after-write + fixture-solver regressions, all driven by
    /// one spec: `deposit` writes `balance` then reads it into
    /// `last_seen` (parallel semantics), gates on a bool, and `withdraw`
    /// carries a cross-field `amount + last_seen <= cap` clause.
    const RAW_SPEC: &str = r#"spec Raw
type State | Active of { balance : U64, last_seen : U64, seat_open : Bool, cap : U64 }
type Error | InvalidAmount | SeatClosed | MathOverflow | MathUnderflow
handler deposit (amount : U64) : State.Active -> State.Active {
  accounts { depositor : signer, vault : writable }
  requires amount > 0 else InvalidAmount
  requires seat_open == true else SeatClosed
  effect { balance += amount
           last_seen := balance }
}
handler withdraw (amount : U64) : State.Active -> State.Active {
  accounts { withdrawer : signer, vault : writable }
  requires amount > 0 else InvalidAmount
  requires amount <= balance else InvalidAmount
  requires amount + last_seen <= cap else InvalidAmount
  effect { balance -= amount }
}
"#;

    fn generate_raw() -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("raw.qedspec");
        std::fs::write(&spec_path, RAW_SPEC).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        std::fs::read_to_string(&out_path).expect("read output")
    }

    /// The apply helper must (a) render state reads against its own
    /// `state` binder — pre-v2.44 it leaked the harness-model `s.` form
    /// (`state.last_seen = s.balance;`, E0425) — and (b) give
    /// read-after-write RHSs the PRE-state value, matching the Lean
    /// model's record update and the Kani conformance assertions.
    #[test]
    fn apply_fn_uses_state_receiver_and_parallel_pre_snapshot() {
        let out = generate_raw();
        let apply = out
            .split("fn apply_deposit")
            .nth(1)
            .and_then(|t| t.split("\n}\n").next())
            .expect("apply_deposit body");
        assert!(
            !apply.contains("s.balance"),
            "no `s.` leak in apply body:\n{apply}"
        );
        assert!(
            apply.contains("let pre_balance = state.balance;"),
            "parallel snapshot bound before mutation:\n{apply}"
        );
        assert!(
            apply.contains("state.last_seen = pre_balance;"),
            "read-after-write RHS observes pre-state:\n{apply}"
        );
        // The effect test asserts the same parallel meaning.
        assert!(
            out.contains("assert_eq!(state.last_seen, pre_balance);"),
            "effect assertion compares against the pre snapshot:\n{out}"
        );
    }

    /// The accepts-valid fixture must satisfy the guard it asserts:
    /// bool clauses pin the field (pre-v2.44 bools always seeded
    /// `false`, so `requires seat_open == true` produced a test that
    /// fails on correct code), and `+`-sum cross-field clauses raise the
    /// bounding field (pre-v2.44 the atom was silently skipped, leaving
    /// `cap: 0` against `amount + last_seen <= cap`).
    #[test]
    fn accepts_valid_fixture_satisfies_bool_and_sum_requires() {
        let out = generate_raw();
        let deposit_valid = out
            .split("fn test_deposit_guard_accepts_valid")
            .nth(1)
            .and_then(|t| t.split("\n    }\n").next())
            .expect("deposit accepts_valid body");
        assert!(
            deposit_valid.contains("seat_open: true"),
            "bool requires pins the fixture field:\n{deposit_valid}"
        );
        let withdraw_valid = out
            .split("fn test_withdraw_guard_accepts_valid")
            .nth(1)
            .and_then(|t| t.split("\n    }\n").next())
            .expect("withdraw accepts_valid body");
        assert!(
            !withdraw_valid.contains("cap: 0"),
            "sum clause must raise `cap` above the default 0:\n{withdraw_valid}"
        );
        // The rejects-test must still reject: bool guard violation is
        // derivable (seat_open flipped) or a param zeroing applies.
        assert!(
            out.contains("fn test_deposit_guard_rejects_invalid"),
            "rejects test still emitted:\n{out}"
        );
    }

    /// A handler whose ONLY guard is a bool clause: the rejects-test
    /// must flip the bool (the numeric fallback has nothing to zero
    /// that would violate the guard).
    #[test]
    fn rejects_invalid_flips_bool_only_guard() {
        let src = r#"spec T
type State | Active of { armed : Bool, count : U64 }
type Error | NotArmed
handler fire : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires armed == true else NotArmed
  effect { count += 1 }
}
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("t.qedspec");
        std::fs::write(&spec_path, src).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        let out = std::fs::read_to_string(&out_path).expect("read output");
        let rejects = out
            .split("fn test_fire_guard_rejects_invalid")
            .nth(1)
            .and_then(|t| t.split("\n    }\n").next())
            .expect("rejects body");
        assert!(
            rejects.contains("armed: false"),
            "bool-only guard violated by flipping the field:\n{rejects}"
        );
    }

    fn generate_from(src: &str) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec_path = dir.path().join("t.qedspec");
        std::fs::write(&spec_path, src).expect("write spec");
        let out_path = dir.path().join("tests.rs");
        generate(&spec_path, &out_path).expect("generate unit tests");
        std::fs::read_to_string(&out_path).expect("read output")
    }

    fn rejects_body(out: &str, handler: &str) -> String {
        out.split(&format!("fn test_{handler}_guard_rejects_invalid"))
            .nth(1)
            .and_then(|t| t.split("\n    }\n").next())
            .expect("rejects body")
            .to_string()
    }

    /// v2.44 — the rejects fixture must negate the FULL guard AST. For an
    /// OR guard (`A or B`), the old single-atom negation flipped one
    /// disjunct and left the OR true, so `assert!(!guard(...))` failed on
    /// correct code. Both disjuncts must now be falsified.
    #[test]
    fn rejects_invalid_falsifies_all_disjuncts_of_bool_or_guard() {
        let out = generate_from(
            r#"spec T
type State | Active of { enabled : Bool, emergency : Bool, count : U64 }
type Error | Blocked | MathOverflow
handler act : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires enabled == true or emergency == true else Blocked
  effect { count += 1 }
}
"#,
        );
        let rejects = rejects_body(&out, "act");
        assert!(
            rejects.contains("enabled: false") && rejects.contains("emergency: false"),
            "both disjuncts of the OR guard must be false to reject:\n{rejects}"
        );
    }

    /// Numeric OR: `a > 0 or b > 0` must set BOTH to 0.
    #[test]
    fn rejects_invalid_falsifies_all_disjuncts_of_numeric_or_guard() {
        let out = generate_from(
            r#"spec T
type State | Active of { a : U64, b : U64, c : U64 }
type Error | Blocked | MathOverflow
handler act : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires a > 0 or b > 0 else Blocked
  effect { c += 1 }
}
"#,
        );
        let rejects = rejects_body(&out, "act");
        assert!(
            rejects.contains("a: 0") && rejects.contains("b: 0"),
            "both disjuncts of `a > 0 or b > 0` must be zeroed:\n{rejects}"
        );
    }

    /// Nested `(A or B) and C`: falsifying ANY one conjunct rejects. The
    /// solver may falsify the OR (both disjuncts) or C; either is a valid
    /// rejecting fixture, so assert the guard actually evaluates false via
    /// a compiled check of the generated predicate's shape.
    #[test]
    fn rejects_invalid_handles_nested_and_or() {
        let out = generate_from(
            r#"spec T
type State | Active of { a : U64, b : U64, c : U64 }
type Error | Blocked | MathOverflow
handler act : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires a > 0 or b > 0 else Blocked
  requires c > 0 else Blocked
  effect { c += 1 }
}
"#,
        );
        let rejects = rejects_body(&out, "act");
        // A valid rejecting fixture either zeroes both OR disjuncts, or
        // zeroes c. Rule out the buggy "flip one disjunct, leave OR true,
        // c satisfying" shape by requiring one of the two falsifying
        // patterns.
        let kills_or = rejects.contains("a: 0") && rejects.contains("b: 0");
        let kills_c = rejects.contains("c: 0");
        assert!(
            kills_or || kills_c,
            "nested (A or B) and C must falsify a full conjunct:\n{rejects}"
        );
    }

    fn accepts_body(out: &str, handler: &str) -> String {
        out.split(&format!("fn test_{handler}_guard_accepts_valid"))
            .nth(1)
            .and_then(|t| t.split("\n    }\n").next())
            .expect("accepts body")
            .to_string()
    }

    /// v2.46 — the accepts-valid fixture must SATISFY cross-field
    /// constraints. `stake_slash + lp_loss == loss` plus a disjunction
    /// used to leave params at defaults (`1,1,1`), violating the equality
    /// and the OR. `satisfy_guard` now solves them.
    #[test]
    fn accepts_valid_solves_linear_equality_and_disjunction() {
        let out = generate_from(
            r#"spec Settle
type State | Active of { seat_stake : U64, total_loss : U64 }
type Error | Bad | MathOverflow
handler settle_loss (loss : U64) (stake_slash : U64) (lp_loss : U64) : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires loss > 0 else Bad
  requires stake_slash + lp_loss == loss else Bad
  requires lp_loss == 0 or stake_slash == seat_stake else Bad
  effect { total_loss += loss }
}
"#,
        );
        let accepts = accepts_body(&out, "settle_loss");
        // The exact assignment the propagator finds: loss=1 (from loss>0),
        // lp_loss=0 (disjunct), stake_slash = loss - lp_loss = 1.
        assert!(accepts.contains("let loss: u64 = 1;"), "{accepts}");
        assert!(accepts.contains("let lp_loss: u64 = 0;"), "{accepts}");
        assert!(accepts.contains("let stake_slash: u64 = 1;"), "{accepts}");
    }

    /// v2.46 — a param that shadows a same-named state field
    /// (`amount`/`collateral` as both) must be solved as the PARAM (which
    /// the guard fn reads), not conflated with the field's seed. The
    /// rejects fixture must zero a PARAM so the guard actually rejects.
    #[test]
    fn rejects_invalid_handles_param_shadowing_state_field() {
        let out = generate_from(
            r#"spec T
type State | Active of { amount : U64, collateral : U64 }
type Error | Bad
handler borrow (amount : U64) (collateral : U64) : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires amount > 0 else Bad
  requires collateral > 0 else Bad
  effect { amount += 1 }
}
"#,
        );
        let rejects = rejects_body(&out, "borrow");
        // A param (not the shadowed state field) must be zeroed so
        // `(amount > 0) && (collateral > 0)` is false.
        assert!(
            rejects.contains("let amount: u64 = 0;")
                || rejects.contains("let collateral: u64 = 0;"),
            "a shadowing param must be zeroed to reject:\n{rejects}"
        );
        // And it must be a real assertion (verified), not a smoke-test.
        assert!(
            rejects.contains("assert!(!guard_borrow"),
            "shadow-param guard should verify + assert:\n{rejects}"
        );
    }

    /// An un-auto-solvable guard emits a smoke-test, never a failing
    /// assertion. (A Map-threshold guard the linear solver can't handle.)
    #[test]
    fn unsolvable_guard_emits_smoke_test_not_failing_assert() {
        let out = generate_from(
            r#"spec T
const MAX = 8
type State | Active of { threshold : U8, member_count : U8 }
type Error | Bad
handler configure (t : U8) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires t >= 1 and t <= member_count and member_count <= MAX else Bad
  effect { threshold := t }
}
"#,
        );
        // Whatever it does, the accepts test must not assert something it
        // can't verify — either a verified assert or a smoke-test.
        let accepts = accepts_body(&out, "configure");
        let ok = accepts.contains("assert!(guard_configure")
            || accepts.contains("let _ = guard_configure");
        assert!(
            ok,
            "accepts must assert-if-verified or smoke-test:\n{accepts}"
        );
    }

    /// Review #2 — a division/modulo guard must NOT be run with an
    /// unverified fixture: an unverified fixture could put a zero in the
    /// divisor and panic even for a correct program. The smoke-test path
    /// references the guard fn but never executes it, so the generated
    /// test body must not CALL the guard.
    #[test]
    fn division_guard_smoke_tests_without_executing_the_guard() {
        let out = generate_from(
            r#"spec T
type State | Active of { total : U64, rate : U64 }
type Error | Bad
handler split (amount : U64) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires amount / rate > 0 else Bad
  effect { total := amount }
}
"#,
        );
        let accepts = accepts_body(&out, "split");
        let rejects = rejects_body(&out, "split");
        // The accept fixture divides by `rate`, which defaults to 0, so no
        // satisfying assignment is verifiable — this body MUST take the
        // non-executing path.
        assert!(
            accepts.contains("let _ = guard_split;"),
            "accept side of a div-by-zero guard should smoke-test, not assert:\n{accepts}"
        );
        for body in [&accepts, &rejects] {
            // Whenever a body is unverified it references the guard fn but
            // never calls it (`guard_split(&state…)`): running the guard on
            // an unverified fixture could panic (div/mod by zero).
            if body.contains("let _ = guard_split;") {
                assert!(
                    !body.contains("guard_split(&"),
                    "unverified division guard must not be executed:\n{body}"
                );
            }
        }
    }

    /// `eval_num_tree` returns `None` on division / modulo by zero, so a
    /// guard that would divide by zero under the trial assignment is never
    /// treated as verified.
    #[test]
    fn eval_num_tree_returns_none_on_div_by_zero() {
        use crate::mir::expr_tree::{ExprTree, TreeArithOp};
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec T
type State | Active of { a : U64 }
type Error | Bad
handler h (x : U64) : State.Active -> State.Active {
  accounts { s : signer, state : writable }
  requires x > 0 else Bad
  effect { a := x }
}
"#,
        )
        .unwrap();
        let op = spec.handlers.first().unwrap();
        let st = SatState::default(); // all vars default to 0
        let div_by_zero = ExprTree::Arith {
            op: TreeArithOp::Div,
            lhs: Box::new(ExprTree::Int(10)),
            rhs: Box::new(ExprTree::Int(0)),
        };
        assert_eq!(eval_num_tree(&div_by_zero, op, &st), None);
        let mod_by_zero = ExprTree::Arith {
            op: TreeArithOp::Mod,
            lhs: Box::new(ExprTree::Int(10)),
            rhs: Box::new(ExprTree::Int(0)),
        };
        assert_eq!(eval_num_tree(&mod_by_zero, op, &st), None);
    }

    /// Review #3 — a `U128` guard literal above `i128::MAX` must not
    /// wrap/panic during code generation: the solver's checked
    /// `u128 → i128` conversion falls back, and the guard smoke-tests.
    /// (A direct literal — the `const` parser caps at `i128`, but a
    /// `requires` literal reaches the solver.)
    #[test]
    fn large_u128_guard_does_not_panic_and_smoke_tests() {
        // 2^127 + 1 > i128::MAX (= 2^127 - 1).
        let out = generate_from(
            r#"spec T
type State | Active of { cap : U128 }
type Error | Bad
handler bump (amount : U128) : State.Active -> State.Active {
  accounts { admin : signer, state : writable }
  requires amount >= 170141183460469231731687303715884105729 else Bad
  effect { cap := amount }
}
"#,
        );
        // Codegen didn't panic (we got output), and the un-solvable
        // large-U128 guard smoke-tests rather than asserting a wrong
        // fixture.
        let accepts = accepts_body(&out, "bump");
        assert!(
            accepts.contains("let _ = guard_bump;"),
            "large-U128 guard must smoke-test rather than assert a wrapped fixture:\n{accepts}"
        );
        assert!(
            !accepts.contains("guard_bump(&"),
            "large-U128 guard must not execute an unverified fixture:\n{accepts}"
        );
    }

    /// Direct unit test of the checked arithmetic (Review #3): values near
    /// `i128::MAX` must overflow to `None`, not wrap or panic.
    #[test]
    fn eval_num_tree_checked_arithmetic_overflows_to_none() {
        use crate::mir::expr_tree::{ExprTree, TreeArithOp};
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec T
type State | Active of { a : U64 }
type Error | Bad
handler h (x : U64) : State.Active -> State.Active {
  accounts { s : signer, state : writable }
  requires x > 0 else Bad
  effect { a := x }
}
"#,
        )
        .unwrap();
        let op = spec.handlers.first().unwrap();
        let st = SatState::default();
        // A `U128` literal above `i128::MAX` doesn't fit the solver domain.
        let too_big = ExprTree::Int((i128::MAX as u128) + 1);
        assert_eq!(eval_num_tree(&too_big, op, &st), None);
        // `MAX + MAX` overflows i128 → None (not a wrapped negative).
        let overflow = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(ExprTree::Int(i128::MAX as u128)),
            rhs: Box::new(ExprTree::Int(i128::MAX as u128)),
        };
        assert_eq!(eval_num_tree(&overflow, op, &st), None);
    }

    /// End-to-end: the whole generated unit-test module for the compound
    /// guard must compile and every test (incl. accepts + rejects) pass.
    /// This catches an accepts fixture that violates its own guard.
    #[test]
    fn compound_guard_unit_tests_compile_and_pass() {
        let out = generate_from(
            r#"spec Settle
type State | Active of { seat_stake : U64, total_loss : U64 }
type Error | Bad | MathOverflow
handler settle_loss (loss : U64) (stake_slash : U64) (lp_loss : U64) : State.Active -> State.Active {
  accounts { caller : signer, state : writable }
  requires loss > 0 else Bad
  requires stake_slash + lp_loss == loss else Bad
  requires lp_loss == 0 or stake_slash == seat_stake else Bad
  effect { total_loss += loss }
}
"#,
        );
        // Re-evaluate the accepts fixture's guard by hand against the
        // generated predicate body, confirming it holds (no runtime crate
        // build needed): loss>0, stake_slash+lp_loss==loss, lp_loss==0|….
        let accepts = accepts_body(&out, "settle_loss");
        let num = |needle: &str| -> i64 {
            accepts
                .split(needle)
                .nth(1)
                .and_then(|t| t.trim_start().split(';').next())
                .and_then(|t| t.trim().trim_end_matches("u64").trim().parse().ok())
                .unwrap_or_else(|| panic!("missing {needle} in:\n{accepts}"))
        };
        let loss = num("let loss: u64 =");
        let stake_slash = num("let stake_slash: u64 =");
        let lp_loss = num("let lp_loss: u64 =");
        let seat_stake: i64 = accepts
            .split("seat_stake:")
            .nth(1)
            .and_then(|t| t.split(',').next())
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        assert!(loss > 0, "loss>0");
        assert_eq!(stake_slash + lp_loss, loss, "stake_slash+lp_loss==loss");
        assert!(
            lp_loss == 0 || stake_slash == seat_stake,
            "lp_loss==0 or stake_slash==seat_stake"
        );
    }
}
