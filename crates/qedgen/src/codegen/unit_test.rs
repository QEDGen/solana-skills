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
        "src/tests.rs",
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
            (
                cast_subscripts(
                    &crate::rust_codegen_util::strip_variant_prefix_for_flat_state(&field, spec),
                ),
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
    field: &str,
    value: &crate::mir::Expr,
    op_name: &str,
    spec: &ParsedSpec,
) -> bool {
    let dest_is_pubkey = spec
        .handlers
        .iter()
        .find(|o| o.name == op_name)
        .is_some_and(|op| crate::rust_codegen_util::field_type_is_pubkey(field, op, spec));
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
                crate::rust_codegen_util::strip_variant_prefix_for_flat_state(&field, spec),
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
    let triples: Vec<(String, &'static str, &crate::mir::Expr)> =
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

/// Rewrite `[ident]` subscripts to `[ident as usize]` — unit tests bind
/// params at their spec types (`u8` etc.) while Rust arrays index by
/// `usize`. Numeric and compound subscripts pass through unchanged.
fn cast_subscripts(field: &str) -> String {
    let mut out = String::new();
    let mut rest = field;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let Some(j) = rest.find(']') else {
            out.push_str(rest);
            return out;
        };
        let idx = &rest[..j];
        let is_numeric = !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit());
        let is_ident = !idx.is_empty()
            && idx.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !idx.starts_with(|c: char| c.is_ascii_digit());
        if is_ident && !is_numeric {
            out.push_str(&format!("{} as usize", idx));
        } else {
            out.push_str(idx);
        }
        out.push(']');
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    out
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
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_guard_accepts_valid() {{\n",
        op.name
    ));
    emit_state_literal(out, state_name, fields, op, &[], false);
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
        "        assert!(guard_{}(&state{}));\n",
        op.name,
        call_args(op)
    ));
    out.push_str("    }\n\n");

    // --- Test: guard REJECTS invalid inputs ---
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn test_{}_guard_rejects_invalid() {{\n",
        op.name
    ));

    // Try to derive a violating input from the guard
    let (state_overrides, param_overrides) = derive_guard_violation(guard_rust, op, fields);

    emit_state_literal_with(out, state_name, fields, op, &[], &state_overrides, false);
    for (pname, ptype) in &op.takes_params {
        if let Some(val) = param_overrides.iter().find(|(n, _)| n == pname) {
            out.push_str(&format!(
                "        let {}: {} = {};\n",
                pname,
                map_type(ptype, spec)?,
                val.1
            ));
        } else {
            let val = sensible_param(pname, ptype);
            out.push_str(&format!(
                "        let {}: {} = {};\n",
                pname,
                map_type(ptype, spec)?,
                val
            ));
        }
    }
    out.push_str(&format!(
        "        assert!(!guard_{}(&state{}));\n",
        op.name,
        call_args(op)
    ));
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
                    adjust(f, l + 1, &pinned)
                }
                ("!=", AtomSide::Lit(l), AtomSide::Field(f)) if vb == *l => {
                    adjust(f, l + 1, &pinned)
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
            // Value making `x <op> l` evaluate to `want_true`.
            let v: Option<u128> = match (op, want_true) {
                (Gt, true) => Some(l + 1),
                (Gt, false) => Some(l),
                (Ge, true) => Some(l),
                (Ge, false) => l.checked_sub(1),
                (Lt, true) => l.checked_sub(1),
                (Lt, false) => Some(l),
                (Le, true) => Some(l),
                (Le, false) => Some(l + 1),
                (Eq, true) => Some(l),
                (Eq, false) => Some(l + 1),
                (Ne, true) => Some(l + 1),
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
    // Structure-aware path first (correct for AND / OR / not / nesting).
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
        let value = match cmp {
            ">" => Some(l),
            ">=" if l > 0 => Some(l - 1),
            "<" => Some(l),
            "<=" => Some(l + 1),
            "==" => Some(l + 1),
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
}
