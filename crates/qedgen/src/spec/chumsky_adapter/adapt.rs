//! Top-level adapter: typed `ast::Spec` → `ParsedSpec`, plus instruction /
//! sBPF-property lowering, handler expansion, and the `parse_str` orchestrator.

use super::*;

/// Translate an `InstructionDecl` into the legacy `ParsedInstruction` shape.
fn adapt_instruction(instr: &a::InstructionDecl, top_consts: ConstTable) -> ParsedInstruction {
    let mut discriminant: Option<String> = None;
    let mut entry: Option<u64> = None;
    let mut constants: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<ParsedErrorCode> = Vec::new();
    let mut input_layout: Vec<ParsedLayoutField> = Vec::new();
    let mut insn_layout: Vec<ParsedLayoutField> = Vec::new();
    let mut guard_decls: Vec<&a::GuardDecl> = Vec::new();
    let mut prop_decls: Vec<&a::SbpfPropertyDecl> = Vec::new();

    for item in &instr.items {
        match item {
            a::InstructionItem::Discriminant(d) => discriminant = Some(d.clone()),
            a::InstructionItem::Entry(n) => entry = Some(*n),
            a::InstructionItem::Const { name, value } => {
                constants.push((name.clone(), value.to_string()));
            }
            a::InstructionItem::Errors(entries) => {
                for e in entries {
                    errors.push(ParsedErrorCode {
                        name: e.name.clone(),
                        value: e.code,
                        description: e.description.clone(),
                    });
                }
            }
            a::InstructionItem::InputLayout(fs) => {
                for f in fs {
                    input_layout.push(ParsedLayoutField {
                        name: f.name.clone(),
                        field_type: f.field_type.clone(),
                        offset: f.offset,
                        description: f.description.clone(),
                    });
                }
            }
            a::InstructionItem::InsnLayout(fs) => {
                for f in fs {
                    insn_layout.push(ParsedLayoutField {
                        name: f.name.clone(),
                        field_type: f.field_type.clone(),
                        offset: f.offset,
                        description: f.description.clone(),
                    });
                }
            }
            a::InstructionItem::Guard(g) => guard_decls.push(g),
            a::InstructionItem::SbpfProperty(p) => prop_decls.push(p),
        }
    }

    // Build a merged const table: top-level constants + this instruction's
    // local constants. Instruction-local wins on conflict (pest parity).
    let mut merged = top_consts.clone();
    for (name, value) in &constants {
        merged.insert(name.clone(), value.clone());
    }
    let merged_consts: ConstTable = &merged;

    let guards: Vec<ParsedGuard> = guard_decls
        .iter()
        .map(|g| {
            let (checks, checks_raw) = match &g.checks {
                Some(e) => (
                    Some(render_sbpf_check(&e.node, merged_consts, true)),
                    Some(render_sbpf_check(&e.node, merged_consts, false)),
                ),
                None => (None, None),
            };
            ParsedGuard {
                name: g.name.clone(),
                doc: g.doc.clone(),
                checks,
                checks_raw,
                error: g.error.clone(),
                fuel: g.fuel,
            }
        })
        .collect();

    let properties: Vec<ParsedSbpfProperty> =
        prop_decls.iter().map(|p| adapt_sbpf_property(p)).collect();

    ParsedInstruction {
        name: instr.name.clone(),
        doc: instr.doc.clone(),
        discriminant,
        entry,
        constants,
        errors,
        input_layout,
        insn_layout,
        guards,
        properties,
    }
}

/// Pending CPI envelope data accumulated while scanning an sBPF property's
/// clauses: (program, instruction, fields).
type PendingCpi = (String, String, Vec<(String, String)>);

fn adapt_sbpf_property(p: &a::SbpfPropertyDecl) -> ParsedSbpfProperty {
    // Decide kind from the clauses. Later clauses override earlier ones when
    // they set the same field. The presence of certain clauses determines the
    // variant.
    let mut scope_targets: Option<Vec<String>> = None;
    let mut flow: Option<(String, FlowKind)> = None;
    let mut cpi: Option<PendingCpi> = None;
    let mut after_all_guards = false;
    let mut exit: Option<u64> = None;
    let mut has_expr = false;

    for clause in &p.clauses {
        match clause {
            a::SbpfPropClause::Expr(_) => has_expr = true,
            a::SbpfPropClause::PreservedBy(_) => {}
            a::SbpfPropClause::Scope(names) => scope_targets = Some(names.clone()),
            a::SbpfPropClause::Flow { target, kind } => {
                let k = match kind {
                    a::SbpfFlowKind::FromSeeds(xs) => FlowKind::FromSeeds(xs.clone()),
                    a::SbpfFlowKind::Through(xs) => FlowKind::Through(xs.clone()),
                };
                flow = Some((target.clone(), k));
            }
            a::SbpfPropClause::Cpi {
                program,
                instruction,
                fields,
            } => {
                cpi = Some((program.clone(), instruction.clone(), fields.clone()));
            }
            a::SbpfPropClause::AfterAllGuards => after_all_guards = true,
            a::SbpfPropClause::Exit(n) => exit = Some(*n),
        }
    }

    let _ = has_expr; // accepted but currently unused for routing
    let kind = if let Some(targets) = scope_targets {
        SbpfPropertyKind::Scope { targets }
    } else if let Some((target, k)) = flow {
        SbpfPropertyKind::Flow { target, kind: k }
    } else if let Some((program, instruction, fields)) = cpi {
        SbpfPropertyKind::Cpi {
            program,
            instruction,
            fields,
        }
    } else if after_all_guards || exit.is_some() {
        SbpfPropertyKind::HappyPath {
            exit_code: exit.map(|n| n.to_string()).unwrap_or_default(),
        }
    } else {
        // Either an explicit `expr` body or empty — the generic stub covers both.
        SbpfPropertyKind::Generic
    };

    ParsedSbpfProperty {
        name: p.name.clone(),
        doc: p.doc.clone(),
        kind,
    }
}

// ============================================================================
// Top-level adapter
// ============================================================================

/// Convenience: parse a spec source string into a `ParsedSpec` in one step.
/// Used by tests and internal code paths that don't have a file on disk.
pub fn parse_str(src: &str) -> anyhow::Result<ParsedSpec> {
    let typed = crate::chumsky_parser::parse(src).map_err(|errs| {
        let msg = errs
            .iter()
            .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, src)))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!("parse error:\n{}", msg)
    })?;
    let parsed = adapt(&typed);
    typecheck_spec(&typed, &parsed)?;
    Ok(parsed)
}

/// Translate the typed AST into a `ParsedSpec` compatible with current consumers.
pub fn adapt(spec: &a::Spec) -> ParsedSpec {
    let mut out = ParsedSpec {
        program_name: spec.name.clone(),
        ..ParsedSpec::default()
    };

    // First pass: collect constants so guard rendering can substitute them.
    let mut consts_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // Integer-max builtins so `requires state.x + n <= U64_MAX` resolves
    // out of the box. User-defined `const` shadows the builtin (builtins
    // inserted first, user consts after).
    consts_map.insert("U64_MAX".to_string(), u64::MAX.to_string());
    consts_map.insert("U32_MAX".to_string(), u32::MAX.to_string());
    consts_map.insert("U128_MAX".to_string(), u128::MAX.to_string());
    consts_map.insert("I64_MAX".to_string(), i64::MAX.to_string());
    consts_map.insert("I128_MAX".to_string(), i128::MAX.to_string());
    for Node { node, .. } in &spec.items {
        if let TopItem::Const { name, value } = node {
            consts_map.insert(name.clone(), value.to_string());
        }
    }
    let consts: ConstTable = &consts_map;

    // Build the type environment for arithmetic coercion.
    let env = TypeEnv::from_spec(spec);

    // Ghost names + spec-level tree context for expression positions that
    // live outside any handler (properties, invariants, schemas). Handler
    // positions build their own `TreeCx::for_handler` in `expand_handler`.
    let ghosts = ghost_names(spec);
    let spec_tcx = TreeCx::spec_level(&env, consts, ghosts.clone());

    let mut constants = Vec::new();

    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Const { name, value } => {
                constants.push((name.clone(), value.to_string()));
            }
            TopItem::Record(r) => {
                out.records.push(ParsedRecordType {
                    name: r.name.clone(),
                    fields: typed_pairs(&r.fields),
                });
            }
            TopItem::Adt(adt) => {
                // Error ADT: populate error_codes / valued_errors.
                if adt.name == "Error" {
                    for v in &adt.variants {
                        push_error(&mut out, &v.name, v.code, &v.description);
                    }
                } else if is_map_value_sum_type(&adt.name, spec) {
                    // Real sum type used as a Map value → emit as proper Lean
                    // `inductive` later; preserve variant structure here.
                    out.sum_types.push(ParsedSumType {
                        name: adt.name.clone(),
                        variants: parsed_variants(&adt.variants),
                    });
                } else {
                    // State-ish ADT: collect lifecycle from variant names,
                    // fields from the payload-carrying variant(s). Flattened
                    // representation matches existing transition codegen.
                    let lifecycle: Vec<String> =
                        adt.variants.iter().map(|v| v.name.clone()).collect();
                    // Flatten variant fields into the state-field list,
                    // deduped by name — duplicate fields would emit invalid
                    // Rust in the Kani harness. First occurrence wins
                    // (variants usually share field shapes); same-name
                    // different-type collisions surface via the check.rs lint.
                    let mut fields: Vec<(String, String)> = Vec::new();
                    let mut seen: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for variant in &adt.variants {
                        for f in &variant.fields {
                            if seen.insert(f.name.clone()) {
                                fields.push((f.name.clone(), type_ref_to_string(&f.ty)));
                            }
                        }
                    }
                    // Preserve per-variant structure alongside the flattened
                    // `fields` view: enum-emitting codegen reads `variants`;
                    // the flat view stays for back-compat. Zero-payload
                    // variants (`| Inactive`) are kept for unit-style enum
                    // emission.
                    out.account_types.push(ParsedAccountType {
                        name: adt.name.clone(),
                        fields,
                        lifecycle,
                        pda_ref: None,
                        variants: parsed_variants(&adt.variants),
                    });
                }
            }
            TopItem::Handler(h) => {
                // If the handler has a `match` clause, expand into one
                // synthetic handler per arm. Otherwise, single handler.
                let expanded = expand_handler(h, consts, &env, &ghosts);
                out.handlers.extend(expanded);
            }
            TopItem::Property(p) => {
                // Pick state_mode / Lean ctx by the property's class. Unary
                // renders `s.x`; Binary (body contains `old(...)`) renders
                // `post.x` / `pre.x` on the Rust side and `s'.x` / `s.x`
                // (`Ctx::Ensures`) on the Lean side — rendering both classes
                // identically would collapse every `old(...)` preservation
                // property into a structural tautology (`s.x ≥ s.x`).
                let property_class = classify_property_body(&p.body);
                let lean_ctx = match property_class {
                    crate::check::PropertyClass::Unary => Ctx::Guard,
                    crate::check::PropertyClass::Binary => Ctx::Ensures,
                };
                let lean = expr_to_lean(&p.body.node, lean_ctx, consts, &env);
                let property_state_mode = match property_class {
                    crate::check::PropertyClass::Unary => StateMode::Unary,
                    crate::check::PropertyClass::Binary => StateMode::Binary,
                };
                let native_opts = opts_native(&env).with_state_mode(property_state_mode);
                let pod_opts = opts_pod(&env).with_state_mode(property_state_mode);
                let rust = expr_to_rust(&p.body.node, Ctx::Guard, consts, native_opts);
                let rust_pod = expr_to_rust(&p.body.node, Ctx::Guard, consts, pod_opts);
                let rust_math = expr_to_rust(
                    &p.body.node,
                    Ctx::Guard,
                    consts,
                    native_opts.with_widen_arith(),
                );
                let preserved = match &p.preserved_by {
                    // `preserved_by all` — kept as the sentinel "all".
                    // Expanded to the full handler-name list below after all
                    // handlers are known (matches pest parity).
                    a::PreservedBy::All => vec!["all".to_string()],
                    a::PreservedBy::Some(xs) => xs.clone(),
                    // `all except [...]`: tag each name with a `!` prefix so
                    // the second-pass expansion (after all handlers are
                    // known) can subtract them. `!` isn't legal in
                    // identifiers, so the tag is collision-free.
                    a::PreservedBy::AllExcept(xs) => {
                        let mut tagged = vec!["all".to_string()];
                        for x in xs {
                            tagged.push(format!("!{}", x));
                        }
                        tagged
                    }
                };
                // When the body is a single `forall <binder> : <T>, inner`
                // with a binder type wider than U8/I8 (so the standard
                // proptest lowering would emit the unsupported sentinel),
                // also render the inner body keeping the binder as a free
                // Rust variable. proptest_gen uses this to emit per-slot
                // `_at` predicates and have preservation tests for handlers
                // taking the binder as a param check at that slot.
                let per_slot = match &p.body.node {
                    Expr::Quant {
                        kind: a::Quantifier::Forall,
                        binder,
                        binder_ty,
                        body,
                    } if !matches!(binder_ty.as_str(), "U8" | "I8") => {
                        let body_rust =
                            expr_to_rust(&body.node, Ctx::Guard, consts, opts_native(&env));
                        // Only useful if the body itself rendered without any
                        // further unsupported quantifier — nested wide forall
                        // can't be flattened to a single per-slot param.
                        if !crate::check::rust_expr_is_unsupported(&body_rust) {
                            Some(crate::check::PerSlotForm {
                                binder_name: binder.clone(),
                                binder_type: binder_ty.clone(),
                                rust_body: body_rust,
                            })
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                // Classify the quantifier shape so `check_completeness` can
                // lint unsupported shapes. Supported shapes get `None`; the
                // per_slot field above carries what codegen needs.
                let quantifier_lint = match crate::quantifier::supported_shape(p) {
                    Ok(_) => None,
                    // A bounded `exists` (over `Fin[N]`, directly or via an
                    // alias such as `AccountIdx`) now lowers to a real
                    // `(0..N).any(…)` harness predicate — see the `expr_to_rust`
                    // quantifier arm. `supported_shape` rejects every `exists`
                    // because it can't resolve aliases to decide boundedness;
                    // the rendered Rust body is the ground truth, so when the
                    // body lowered cleanly (no `QEDGEN_UNSUPPORTED` sentinel)
                    // suppress the spurious P5. Unbounded `exists` (e.g. over
                    // `U64`) still emits the sentinel and keeps the lint.
                    Err(crate::quantifier::Reason::ExistsQuantifier { .. })
                        if !crate::check::rust_expr_is_unsupported(&rust) =>
                    {
                        None
                    }
                    Err(reason) => {
                        let kind = match &reason {
                            crate::quantifier::Reason::NestedQuantifier { .. } => {
                                "nested_quantifier"
                            }
                            crate::quantifier::Reason::UnboundedBinderType { .. } => {
                                "unbounded_binder"
                            }
                            crate::quantifier::Reason::ExistsQuantifier { .. } => {
                                "exists_quantifier"
                            }
                        };
                        let span = reason.span();
                        Some(crate::check::QuantifierLint {
                            kind: kind.to_string(),
                            message: reason.message(),
                            span_start: span.start,
                            span_end: span.end,
                        })
                    }
                };
                let class = classify_property_body(&p.body);
                out.properties.push(ParsedProperty {
                    name: p.name.clone(),
                    expression: Some(lean),
                    rust_expression: Some(rust),
                    rust_expression_pod: Some(rust_pod),
                    rust_expression_math: Some(rust_math),
                    preserved_by: preserved,
                    per_slot,
                    quantifier_lint,
                    class,
                    ast_body: Some(p.body.clone()),
                    tree: Some(build_expr_tree(&p.body, &spec_tcx)),
                });
            }
            TopItem::Cover(c) => {
                out.covers.push(ParsedCover {
                    name: c.name.clone(),
                    traces: c.traces.clone(),
                    reachable: c
                        .reachable
                        .iter()
                        .map(|(op, when)| {
                            (
                                op.clone(),
                                when.as_ref()
                                    .map(|e| expr_to_lean(&e.node, Ctx::Guard, consts, &env)),
                                when.as_ref().map(|e| build_expr_tree(e, &spec_tcx)),
                            )
                        })
                        .collect(),
                });
            }
            TopItem::Liveness(l) => {
                // Strip the type prefix: `State.Active` → `Active`.
                // Legacy code consumes the bare variant name.
                let last = |q: &crate::ast::QualifiedPath| -> String {
                    q.0.last().cloned().unwrap_or_default()
                };
                out.liveness_props.push(ParsedLiveness {
                    name: l.name.clone(),
                    from_state: last(&l.from_state),
                    leads_to_state: last(&l.to_state),
                    via_ops: l.via.clone(),
                    within_steps: Some(l.within),
                });
            }
            TopItem::Invariant(i) => {
                let parsed = match &i.body {
                    a::InvariantBody::Expr(e) => {
                        let lean = expr_to_lean(&e.node, Ctx::Guard, consts, &env);
                        let rust = expr_to_rust(&e.node, Ctx::Guard, consts, opts_native(&env));
                        crate::check::ParsedInvariant {
                            name: i.name.clone(),
                            doc: String::new(),
                            lean_expr: Some(lean),
                            rust_expr: Some(rust),
                            ast_body: Some(e.clone()),
                            tree: Some(build_expr_tree(e, &spec_tcx)),
                        }
                    }
                    a::InvariantBody::Description(s) => crate::check::ParsedInvariant {
                        name: i.name.clone(),
                        doc: s.clone(),
                        lean_expr: None,
                        rust_expr: None,
                        ast_body: None,
                        tree: None,
                    },
                };
                out.invariants.push(parsed);
            }
            TopItem::Pda(p) => {
                let seeds: Vec<String> = p
                    .seeds
                    .iter()
                    .map(|s| match s {
                        a::PdaSeed::Literal(lit) => format!("\"{}\"", lit),
                        a::PdaSeed::Ident(id) => id.clone(),
                    })
                    .collect();
                out.pdas.push(ParsedPda {
                    name: p.name.clone(),
                    seeds,
                });
            }
            TopItem::Event(ev) => {
                out.events.push(ParsedEvent {
                    name: ev.name.clone(),
                    fields: typed_pairs(&ev.fields),
                });
            }
            TopItem::TypeAlias(ta) => {
                out.type_aliases
                    .push((ta.name.clone(), type_ref_to_string(&ta.target)));
            }
            TopItem::Dimension(dimension) => {
                let base = type_ref_to_string(&dimension.base);
                out.type_aliases
                    .push((dimension.name.clone(), base.clone()));
                out.dimensions.push(crate::check::ParsedDimension {
                    name: dimension.name.clone(),
                    base,
                });
            }
            TopItem::ProgramId(pid) => {
                out.program_id = Some(pid.clone());
            }
            TopItem::Pubkey(p) => {
                out.pubkeys.push(ParsedPubkey {
                    name: p.name.clone(),
                    chunks: p.chunks.iter().map(|c| c.to_string()).collect(),
                });
            }
            TopItem::Errors(entries) => {
                // Mirror ADT-Error behavior: populate error_codes and valued_errors.
                push_errors(&mut out, entries);
            }
            TopItem::Instruction(instr) => {
                out.instructions.push(adapt_instruction(instr, consts));
            }
            TopItem::Environment(envd) => {
                let environment_tcx = TreeCx::for_environment(envd, &env, consts, ghosts.clone());
                let mut mutates: Vec<(String, String)> = Vec::new();
                let mut external_fields: Vec<(String, String, String)> = Vec::new();
                let mut typed_constraints: Vec<crate::check::ParsedEnvironmentConstraint> =
                    Vec::new();
                for Node { node: c, .. } in &envd.clauses {
                    match c {
                        a::EnvClause::Mutates { field, ty } => {
                            mutates.push((field.clone(), ty.clone()));
                        }
                        a::EnvClause::External { object, field, ty } => {
                            external_fields.push((
                                object.clone(),
                                field.clone(),
                                type_ref_to_string(ty),
                            ));
                        }
                        a::EnvClause::Constraint(e) => {
                            let lean_expr = expr_to_lean(&e.node, Ctx::Ensures, consts, &env);
                            let rust_expr =
                                expr_to_rust(&e.node, Ctx::Ensures, consts, opts_native(&env));
                            let _ = (lean_expr, rust_expr);
                            typed_constraints.push(crate::check::ParsedEnvironmentConstraint {
                                tree: Some(build_expr_tree(e, &environment_tcx)),
                                class: classify_property_body(e),
                            });
                        }
                    }
                }
                out.environments.push(ParsedEnvironment {
                    name: envd.name.clone(),
                    mutates,
                    external_fields,
                    typed_constraints,
                });
            }
            TopItem::Interface(iface) => {
                out.interfaces.push(adapt_interface(iface, consts, &env));
            }
            TopItem::Import {
                name,
                from,
                as_name,
            } => {
                out.imports.push(ParsedImport {
                    name: name.clone(),
                    from: from.clone(),
                    as_name: as_name.clone(),
                });
            }
            TopItem::PragmaAssign { name, value } => {
                // Push raw; lint validates the key against the known set and
                // the value against declared `type Error` variants.
                out.pragma_assignments.push((name.clone(), value.clone()));
            }
            TopItem::Schema(s) => {
                // Collect schema blocks for `include <name>` expansion;
                // adapt each requires eagerly so schema guards land in the
                // same shape as inline requires.
                let requires = s
                    .requires
                    .iter()
                    .map(|(guard, on_fail)| {
                        lower_requires(guard, on_fail.clone(), consts, &env, &spec_tcx)
                    })
                    .collect();
                out.schemas.push(crate::check::ParsedSchema {
                    name: s.name.clone(),
                    doc: s.doc.clone(),
                    requires,
                });
            }
            TopItem::RefImpl(r) => {
                // Lean lowering uses `lean_body`; Kani inlining uses
                // `rust_body`. The body is pure — use Guard ctx so bare
                // state refs render as `s.x` (single-state context).
                let params: Vec<(String, String)> = typed_pairs(&r.params);
                let lean_body = expr_to_lean(&r.body.node, Ctx::Guard, consts, &env);
                let rust_body = expr_to_rust(&r.body.node, Ctx::Guard, consts, opts_native(&env));
                // Narrow a `mul_div_*`-rooted body back to the declared
                // return width: the helpers are u128-typed for intermediate
                // precision, but the emitted `fn` signature carries the
                // spec's return type — without the cast the generated fn
                // doesn't compile (issue #145). Same precedent as the
                // `let X = mul_div_*(…)` narrowing in `adapt_handler`.
                let rust_body = if is_mul_div_let_rhs(&r.body.node) {
                    match type_ref_to_string(&r.return_type).as_str() {
                        "U128" => rust_body,
                        "U8" => format!("({}) as u8", rust_body),
                        "U16" => format!("({}) as u16", rust_body),
                        "U32" => format!("({}) as u32", rust_body),
                        "I64" => format!("({}) as i64", rust_body),
                        "I128" => format!("({}) as i128", rust_body),
                        _ => format!("({}) as u64", rust_body),
                    }
                } else {
                    rust_body
                };
                out.ref_impls.push(crate::check::ParsedRefImpl {
                    name: r.name.clone(),
                    doc: r.doc.clone(),
                    params,
                    return_type: type_ref_to_string(&r.return_type),
                    lean_body,
                    rust_body,
                });
            }
            TopItem::Ghost(g) => {
                // Init value — a constant expression in single-state context.
                let init_tree = build_expr_tree(&g.init, &spec_tcx);
                let mut updates = Vec::new();
                for u in &g.updates {
                    // Resolve the named handler's params so the update RHS can
                    // reference them; ghost names already resolve via `env`.
                    let handler_params: &[a::TypedField] = spec
                        .items
                        .iter()
                        .find_map(|it| match &it.node {
                            TopItem::Handler(h) if h.name == u.handler => Some(h.params.as_slice()),
                            _ => None,
                        })
                        .unwrap_or(&[]);
                    let uenv = env.clone().with_params(handler_params);
                    let rhs_rust =
                        expr_to_rust(&u.stmt.rhs.node, Ctx::Guard, consts, opts_native(&uenv));
                    let mut utcx = TreeCx::spec_level(&env, consts, ghosts.clone());
                    utcx.params
                        .extend(handler_params.iter().map(|p| p.name.clone()));
                    let rhs_tree = build_expr_tree(&u.stmt.rhs, &utcx);
                    let ghost_read = crate::mir::ExprTree::Path(crate::mir::expr_tree::TreePath {
                        root: "state".to_string(),
                        binding: crate::mir::expr_tree::BindingKind::Ghost,
                        segments: vec![crate::mir::expr_tree::TreeSeg::Field(g.name.clone())],
                        ty: None,
                    });
                    let fold =
                        |op: crate::mir::expr_tree::TreeArithOp| crate::mir::ExprTree::Arith {
                            op,
                            lhs: Box::new(ghost_read.clone()),
                            rhs: Box::new(rhs_tree.clone()),
                        };
                    // Fold the assignment operator into a complete new-value
                    // expression so each backend just emits `<ghost> := value`
                    // (the Lean form renders from `value_tree`).
                    let value_rust = match u.stmt.op {
                        a::EffectOp::Set => rhs_rust,
                        a::EffectOp::Add | a::EffectOp::AddSat | a::EffectOp::AddWrap => {
                            format!("s.{} + ({})", g.name, rhs_rust)
                        }
                        a::EffectOp::Sub | a::EffectOp::SubSat | a::EffectOp::SubWrap => {
                            format!("s.{} - ({})", g.name, rhs_rust)
                        }
                    };
                    let value_tree = match u.stmt.op {
                        a::EffectOp::Set => rhs_tree.clone(),
                        a::EffectOp::Add | a::EffectOp::AddSat | a::EffectOp::AddWrap => {
                            fold(crate::mir::expr_tree::TreeArithOp::Add)
                        }
                        a::EffectOp::Sub | a::EffectOp::SubSat | a::EffectOp::SubWrap => {
                            fold(crate::mir::expr_tree::TreeArithOp::Sub)
                        }
                    };
                    updates.push(crate::check::ParsedGhostUpdate {
                        handler: u.handler.clone(),
                        value_rust,
                        value_tree: Some(value_tree),
                    });
                }
                out.ghosts.push(crate::check::ParsedGhost {
                    name: g.name.clone(),
                    doc: g.doc.clone(),
                    ty: type_ref_to_string(&g.ty),
                    init_tree: Some(init_tree),
                    updates,
                });
            }
            TopItem::Hook(h) => {
                let kind = match &h.kind {
                    a::HookKind::AfterStore(field) => {
                        crate::check::ParsedHookKind::AfterStore(field.clone())
                    }
                    a::HookKind::BeforeCpi(iface) => {
                        crate::check::ParsedHookKind::BeforeCpi(iface.clone())
                    }
                };
                let asserts = h
                    .asserts
                    .iter()
                    .map(|e| crate::check::ParsedHookAssert {
                        tree: Some(build_expr_tree(e, &spec_tcx)),
                    })
                    .collect();
                out.hooks.push(crate::check::ParsedHook { kind, asserts });
            }
            TopItem::Pragma(p) => {
                // Record the pragma name for target inference. Any given
                // pragma may appear at most once per spec; duplicates are
                // flagged at lint time, not here.
                out.pragmas.push(p.name.clone());

                // Inline-adapt each nested item. The parser restricts pragma
                // bodies to a whitelist (const/pubkey/assembly/instruction/
                // errors), so only those cases matter.
                for Node { node: inner, .. } in &p.items {
                    match inner {
                        TopItem::Const { name, value } => {
                            constants.push((name.clone(), value.to_string()));
                        }
                        TopItem::Pubkey(pk) => {
                            out.pubkeys.push(ParsedPubkey {
                                name: pk.name.clone(),
                                chunks: pk.chunks.iter().map(|c| c.to_string()).collect(),
                            });
                        }
                        TopItem::Instruction(instr) => {
                            out.instructions.push(adapt_instruction(instr, consts));
                        }
                        TopItem::Errors(entries) => {
                            push_errors(&mut out, entries);
                        }
                        // Grammar already rejects non-whitelisted items; this
                        // arm is defensive and silently ignores anything that
                        // slipped through (would indicate a grammar bug).
                        _ => {}
                    }
                }
            }
        }
    }

    // Expand `preserved_by all` to the full handler-name list. `all except
    // [...]` arrives as `["all", "!h1", "!h2"]`; expand `all` then subtract
    // the `!`-prefixed excludes.
    let all_handler_names: Vec<String> = out.handlers.iter().map(|h| h.name.clone()).collect();
    for prop in &mut out.properties {
        if prop.preserved_by.first().map(|s| s.as_str()) == Some("all") {
            let excludes: std::collections::HashSet<String> = prop
                .preserved_by
                .iter()
                .filter_map(|s| s.strip_prefix('!').map(String::from))
                .collect();
            if excludes.is_empty() && prop.preserved_by.len() == 1 {
                prop.preserved_by = all_handler_names.clone();
            } else {
                prop.preserved_by = all_handler_names
                    .iter()
                    .filter(|n| !excludes.contains(n.as_str()))
                    .cloned()
                    .collect();
            }
        }
    }

    // Promote the `state { ... }` sugar's record into `account_types` so
    // lints that walk `account_types` (notably `check_map_and_subscript`)
    // see Map-typed fields — otherwise `lsts[idx].x := …` fires a spurious
    // `subscript_not_map`. Only synthesize when there's no explicit `type
    // State` ADT; the State record stays available via `out.records`.
    if out.account_types.is_empty() {
        if let Some(state_record) = out.records.iter().find(|r| r.name == "State") {
            out.account_types.push(ParsedAccountType {
                name: "State".to_string(),
                fields: state_record.fields.clone(),
                lifecycle: Vec::new(),
                pda_ref: None,
                variants: Vec::new(),
            });
        }
    }

    // Link account_types to PDAs by case-insensitive name match (pest parity).
    for acct in &mut out.account_types {
        if acct.pda_ref.is_none() {
            let lower = acct.name.to_lowercase();
            if let Some(pda) = out.pdas.iter().find(|p| p.name.to_lowercase() == lower) {
                acct.pda_ref = Some(pda.name.clone());
            }
        }
    }

    if let Some(first) = out.account_types.first() {
        out.state_fields = first.fields.clone();
        out.lifecycle_states = first.lifecycle.clone();
    }

    // When the spec uses `state { ... }` sugar or `type State = { ... }`
    // and a handler has no explicit `accounts { ... }`, synthesize a
    // default state-bearing account so codegen can bind `state.X` refs —
    // otherwise guards.rs emits raw `s.X` against no account. Gated on
    // `records.contains("State")` so ADT-state specs stay unchanged. The
    // synthetic name "state" matches lowercase "State" so
    // `infer_state_name`'s case-insensitive lookup binds it to the
    // canonical `<ProgramPascalCase>Account` struct.
    let is_state_record_form = out.records.iter().any(|r| r.name == "State");
    if is_state_record_form && !out.state_fields.is_empty() {
        // `path_to_lean` already lowered `state.X` to `s.X` (Ctx::Guard)
        // or `s'.X` (Ctx::Ensures) before storing in lean_expr, so the
        // textual marker we look for is the lowered prefix, not the
        // spec-level `state.` form. Word-bound the scan so identifiers
        // like `is_active` or method names ending in `s` don't trip it.
        let mentions_state = |text: &str| {
            let bytes = text.as_bytes();
            for i in 0..bytes.len().saturating_sub(1) {
                if bytes[i] != b's' {
                    continue;
                }
                let prev_word_char =
                    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                if prev_word_char {
                    continue;
                }
                let next = bytes[i + 1];
                if next == b'.' {
                    return true;
                }
                if next == b'\'' && i + 2 < bytes.len() && bytes[i + 2] == b'.' {
                    return true;
                }
            }
            false
        };
        for handler in &mut out.handlers {
            if !handler.accounts.is_empty() {
                continue;
            }
            let touches_state = !handler.effects.is_empty()
                || handler
                    .requires
                    .iter()
                    .any(|r| mentions_state(&r.lean_expr))
                || handler.ensures.iter().any(|e| mentions_state(&e.lean_expr));
            if !touches_state {
                continue;
            }
            handler.accounts.push(ParsedHandlerAccount {
                name: "state".to_string(),
                is_signer: false,
                is_writable: !handler.effects.is_empty(),
                is_program: false,
                pda_seeds: None,
                account_type: Some("State".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            });
        }
    }

    out.constants = constants;
    // Collect uninterpreted helpers after all other fields are populated —
    // the collector needs the full state_fields + records + sum_types
    // picture to infer argument types. Names declared as `ref_impl`s are
    // filtered out: they have real Lean bodies (`def`), so the
    // uninterpreted `opaque foo : T → Bool` shape would conflict.
    out.uninterpreted_helpers = collect_uninterpreted_helpers(spec, &out);
    let ref_impl_names: std::collections::HashSet<&str> =
        out.ref_impls.iter().map(|r| r.name.as_str()).collect();
    out.uninterpreted_helpers
        .retain(|(n, _, _)| !ref_impl_names.contains(n.as_str()));

    // Expand `include <schema>` clauses as a post-pass so the lookup sees
    // every declared schema regardless of source order, and synthetic
    // match-arm handlers (which inherit `schema_includes`) get the same
    // expansion.
    let schemas = out.schemas.clone();
    for handler in &mut out.handlers {
        if handler.schema_includes.is_empty() {
            continue;
        }
        for include in &handler.schema_includes {
            if let Some(schema) = schemas.iter().find(|s| s.name == *include) {
                for r in &schema.requires {
                    handler.requires.push(r.clone());
                }
            }
        }
    }

    // Desugar dotted-auth (`auth admin_config.admin_key`): split on `.`,
    // identify the signer, synthesize `requires <acct>.<field> ==
    // <signer>.pubkey else Unauthorized`, and rewrite `handler.who` to the
    // signer so "handler has auth" lints still fire. Bare `auth <name>`
    // stays untouched (lowers to Anchor `has_one = X`).
    desugar_dotted_auth(&mut out);

    out
}

/// Post-pass: `auth <acct>.<field>` → synthesized `requires <acct>.<field>
/// == <signer>.pubkey else Unauthorized` + `who = <signer>` rewrite.
fn desugar_dotted_auth(spec: &mut ParsedSpec) {
    for handler in &mut spec.handlers {
        let Some(actor) = handler.who.clone() else {
            continue;
        };
        let Some((acct_name, field)) = actor.split_once('.') else {
            continue;
        };
        // Identify the signer (single signer in accounts block).
        // Multiple signers → bail without desugaring; the user should
        // explicitly write the requires clause.
        let signers: Vec<&str> = handler
            .accounts
            .iter()
            .filter(|a| a.is_signer)
            .map(|a| a.name.as_str())
            .collect();
        if signers.len() != 1 {
            continue;
        }
        let signer = signers[0].to_string();
        // Synthesize the requires clause. Lean form uses `=` (the
        // adapter's Lean output convention); Rust form uses `==` per
        // `expr_to_rust(Ctx::Guard)`. Both forms read the imported
        // account through the handler-account binding name, which
        // `bind_state` in guards.rs rewrites to `ctx.<acct>.<field>`
        // (flat-struct path) or `(*ctx.<acct>.inner.<field>())`
        // (multi-variant ADT) at codegen time.
        let lean_expr = format!("{}.{} = {}.pubkey", acct_name, field, signer);
        let rust_expr = format!("{}.{} == {}.pubkey", acct_name, field, signer);
        // Hand-built tree for the synthesized guard: both sides are
        // account-field reads (`<acct>.<field>` / `<signer>.pubkey`), so
        // the structural form needs no scope resolution.
        let acct_path = |root: &str, field: &str| {
            crate::mir::ExprTree::Path(crate::mir::expr_tree::TreePath {
                root: root.to_string(),
                binding: crate::mir::expr_tree::BindingKind::Account,
                segments: vec![crate::mir::expr_tree::TreeSeg::Field(field.to_string())],
                ty: None,
            })
        };
        let tree = crate::mir::ExprTree::Cmp {
            op: crate::mir::expr_tree::TreeCmpOp::Eq,
            lhs: Box::new(acct_path(acct_name, field)),
            rhs: Box::new(acct_path(&signer, "pubkey")),
        };
        let synthesized = crate::check::ParsedRequires {
            lean_expr,
            rust_expr: rust_expr.clone(),
            rust_expr_pod: rust_expr.clone(),
            rust_expr_math: rust_expr,
            error_name: Some("Unauthorized".to_string()),
            ast_body: None,
            tree: Some(tree),
        };
        handler.requires.insert(0, synthesized);
        // Rewrite `who` to the signer so downstream consumers see a
        // simple-ident auth (no_access_control lint, lifecycle gates,
        // etc.).
        handler.who = Some(signer);
    }
}

/// `(name, type-string)` pairs from a typed field list — the shared shape
/// for record / event / variant fields and interface state fields.
fn typed_pairs(fields: &[a::TypedField]) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|f| (f.name.clone(), type_ref_to_string(&f.ty)))
        .collect()
}

/// ADT variants → `ParsedVariant`s (structure-preserving view).
fn parsed_variants(variants: &[a::Variant]) -> Vec<ParsedVariant> {
    variants
        .iter()
        .map(|v| ParsedVariant {
            name: v.name.clone(),
            fields: typed_pairs(&v.fields),
        })
        .collect()
}

/// Register one error entry: the name always lands in `error_codes`; a
/// `ParsedErrorCode` is added when a code or description is present.
/// Shared by the `type Error` ADT, top-level `errors [...]`, and
/// pragma-nested `errors [...]` lowerings.
fn push_error(out: &mut ParsedSpec, name: &str, code: Option<u64>, description: &Option<String>) {
    out.error_codes.push(name.to_string());
    if code.is_some() || description.is_some() {
        out.valued_errors.push(ParsedErrorCode {
            name: name.to_string(),
            value: code,
            description: description.clone(),
        });
    }
}

/// `errors [...]` entry list — top-level and pragma-nested forms.
fn push_errors(out: &mut ParsedSpec, entries: &[a::ErrorEntry]) {
    for e in entries {
        push_error(out, &e.name, e.code, &e.description);
    }
}

/// Four-way requires render (Lean / native Rust / pod Rust / math Rust)
/// from an already-canonicalized guard. Shared by handler `requires`,
/// schema requires, match-arm guards, and interface requires.
fn lower_requires(
    guard: &Node<a::Expr>,
    error_name: Option<String>,
    consts: ConstTable,
    env: &TypeEnv,
    tcx: &TreeCx,
) -> ParsedRequires {
    ParsedRequires {
        lean_expr: expr_to_lean(&guard.node, Ctx::Guard, consts, env),
        rust_expr: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_native(env)),
        rust_expr_pod: expr_to_rust(&guard.node, Ctx::Guard, consts, opts_pod(env)),
        rust_expr_math: expr_to_rust(
            &guard.node,
            Ctx::Guard,
            consts,
            opts_native(env).with_widen_arith(),
        ),
        error_name,
        tree: Some(build_expr_tree(guard, tcx)),
        ast_body: Some(guard.clone()),
    }
}

/// Five-way ensures render (Lean / native / pod / binary / binary-math)
/// from an already-canonicalized post-condition. Shared by handler and
/// interface `ensures` lowering.
fn lower_ensures(
    e: &Node<a::Expr>,
    consts: ConstTable,
    env: &TypeEnv,
    tcx: &TreeCx,
) -> ParsedEnsures {
    ParsedEnsures {
        lean_expr: expr_to_lean(&e.node, Ctx::Ensures, consts, env),
        rust_expr: expr_to_rust(&e.node, Ctx::Ensures, consts, opts_native(env)),
        rust_expr_pod: expr_to_rust(&e.node, Ctx::Ensures, consts, opts_pod(env)),
        // Binary rendering for the Kani ensures-preservation harness:
        // `state.x` → `post.x`; `old(state.x)` → `pre.x`.
        rust_expr_binary: expr_to_rust(
            &e.node,
            Ctx::Ensures,
            consts,
            opts_native(env).with_state_mode(StateMode::Binary),
        ),
        rust_expr_binary_math: expr_to_rust(
            &e.node,
            Ctx::Ensures,
            consts,
            opts_native(env)
                .with_state_mode(StateMode::Binary)
                .with_widen_arith(),
        ),
        tree: Some(build_expr_tree(e, tcx)),
    }
}

/// One `accounts { ... }` descriptor → `ParsedHandlerAccount`. Shared
/// verbatim by handler and interface-handler account lowering.
fn lower_account_descriptor(d: &a::AccountDescriptor) -> ParsedHandlerAccount {
    let mut acc = ParsedHandlerAccount {
        name: d.name.clone(),
        is_signer: false,
        is_writable: false,
        is_program: false,
        pda_seeds: None,
        account_type: None,
        authority: None,
        default_pubkey: None,
        imported_namespace: None,
    };
    for attr in &d.attrs {
        match attr {
            a::AccountAttr::Simple(s) => match s.as_str() {
                "signer" => acc.is_signer = true,
                "writable" => acc.is_writable = true,
                "readonly" => acc.is_writable = false,
                "program" => acc.is_program = true,
                _ => acc.account_type = Some(s.clone()),
            },
            a::AccountAttr::Type(t) => {
                // Dotted type ref (`Foreign.State`) splits into namespace
                // alias + source type; bare types keep
                // `imported_namespace = None`. `imported_namespaces` is
                // populated BEFORE handler adapt, so check.rs validates
                // the namespace is known.
                if let Some((ns, ty)) = t.split_once('.') {
                    acc.imported_namespace = Some(ns.to_string());
                    acc.account_type = Some(ty.to_string());
                } else {
                    acc.account_type = Some(t.clone());
                }
            }
            a::AccountAttr::Authority(x) => acc.authority = Some(x.clone()),
            a::AccountAttr::Pda(seeds) => acc.pda_seeds = Some(seeds.clone()),
        }
    }
    acc
}

/// Lower a `call Interface.handler(...)` expression to a `ParsedCall`.
/// Splits `Interface.handler` from the qualified path — longer paths
/// (unusual — e.g. nested namespacing) flatten with '.' into the handler
/// name so the call still records, and the resolver decides what to do.
/// Shared by the top-level `Call` clause and match-arm `Call` bodies.
fn lower_call(c: &a::CallExpr, consts: ConstTable, env: &TypeEnv, tcx: &TreeCx) -> ParsedCall {
    let segs = &c.target.0;
    let (iface, handler_name) = match segs.as_slice() {
        [] => (String::new(), String::new()),
        [only] => (String::new(), only.clone()),
        [head, tail @ ..] => (head.clone(), tail.join(".")),
    };
    let args = c
        .args
        .iter()
        .map(|arg| ParsedCallArg {
            name: arg.name.clone(),
            rust_expr: expr_to_rust(&arg.value.node, Ctx::Guard, consts, opts_native(env)),
            tree: Some(build_expr_tree(&arg.value, tcx)),
        })
        .collect();
    ParsedCall {
        target_interface: iface,
        target_handler: handler_name,
        args,
        result_binding: c.result_binding.clone(),
        // Empty when no binders declared; backends keep the callee-frame,
        // param-only axiom shape in that case.
        state_binders: lower_state_binders(&c.state_binders),
    }
}

/// Expand a handler declaration into one or more `ParsedHandler`s.
/// Handlers without a `branch` clause produce exactly one. Handlers with
/// branches produce one synthetic handler per arm, each carrying the
/// parent's auth/accounts/requires plus the arm's guard and body.
fn expand_handler(
    h: &a::HandlerDecl,
    consts: ConstTable,
    base_env: &TypeEnv,
    ghosts: &std::collections::BTreeSet<String>,
) -> Vec<ParsedHandler> {
    // Per-handler env carries the handler's params for bare-ident lookup.
    let env = TypeEnv {
        state_fields: base_env.state_fields.clone(),
        records: base_env.records.clone(),
        params: h.params.iter().map(|f| (f.name.clone(), &f.ty)).collect(),
        external_fields: base_env.external_fields.clone(),
        aliases: base_env.aliases.clone(),
        adts: base_env.adts.clone(),
    };
    let env = &env;
    let tcx = TreeCx::for_handler(h, env, consts, ghosts.clone());
    let tcx = &tcx;
    // Detect a single branch clause (phase 1: at most one branch per handler).
    let match_clause: Option<&a::MatchClause> = h.clauses.iter().find_map(|c| match &c.node {
        a::HandlerClause::Match(b) => Some(b),
        _ => None,
    });

    let Some(branch) = match_clause else {
        return vec![adapt_handler(h, consts, env, tcx)];
    };

    // Build a shared base handler (parent without the branch clause).
    let base = adapt_handler(h, consts, env, tcx);
    let canon = canon_scope_for_handler(h, consts, env);

    // Accumulate negated guards so that earlier arms' failure implies
    // later arms' precondition (first-match semantics). Tuple is
    // (lean, rust_native, rust_pod, rust_math, tree).
    #[allow(clippy::type_complexity)]
    let mut prior_conds: Vec<(String, String, String, String, Option<crate::mir::ExprTree>)> =
        Vec::new();
    let mut out = Vec::with_capacity(branch.arms.len());

    for arm in &branch.arms {
        let mut synth = base.clone();
        synth.name = format!("{}_{}", h.name, arm.label);

        // Add all prior-arm negations to this arm's requires.
        for (lean_neg, rust_neg, rust_pod_neg, rust_math_neg, tree_neg) in &prior_conds {
            synth.requires.push(ParsedRequires {
                lean_expr: lean_neg.clone(),
                rust_expr: rust_neg.clone(),
                rust_expr_pod: rust_pod_neg.clone(),
                rust_expr_math: rust_math_neg.clone(),
                error_name: None,
                // Synthetic (prior arms' negations, no single source AST
                // node); the `old_in_single_state_context` lint skips these.
                ast_body: None,
                tree: tree_neg.clone(),
            });
        }

        // Current arm's guard (if any) becomes a requires; negation is
        // recorded for subsequent arms.
        if let Some(guard) = &arm.guard {
            let guard = canonicalize_state_refs(guard, &canon);
            let req = lower_requires(&guard, None, consts, env, tcx);
            prior_conds.push((
                format!("\u{00AC}({})", req.lean_expr),
                format!("!({})", req.rust_expr),
                format!("!({})", req.rust_expr_pod),
                format!("!({})", req.rust_expr_math),
                req.tree
                    .clone()
                    .map(|t| crate::mir::ExprTree::Not(Box::new(t))),
            ));
            synth.requires.push(req);
        }

        // Arm body: abort → additional aborting requires; effect → effects
        match &arm.body {
            a::MatchBody::Abort(err) => {
                // Aborting case: synth is guaranteed to fail if its arm fires.
                // Express as `requires false else <err>` so the handler aborts
                // when reached. The `false` is written as `0 == 1` for
                // downstream simplicity (no dedicated False literal).
                synth.requires.push(ParsedRequires {
                    lean_expr: "0 = 1".to_string(),
                    rust_expr: "false".to_string(),
                    rust_expr_pod: "false".to_string(),
                    rust_expr_math: "false".to_string(),
                    error_name: Some(err.clone()),
                    // Synthetic: arm-abort lowers to literal `false` with no
                    // source AST; lint skips. The tree carries the literal.
                    ast_body: None,
                    tree: Some(crate::mir::ExprTree::Bool(false)),
                });
            }
            a::MatchBody::Effect(stmts) => {
                for Node { node: stmt, .. } in stmts {
                    for eff in render_effect_or_expand_variant_promotion(
                        stmt,
                        &base.takes_params,
                        consts,
                        &canon,
                        env,
                        tcx,
                    ) {
                        synth.effects.push(eff.into_parsed_effect());
                    }
                }
            }
            // Synth handler issues the CPI plus any alongside effects —
            // mirrors the top-level `Call` clause lowering so backends see
            // the same ParsedCall shape either way. (Match-arm CPIs always
            // carry an empty binder list at the parser; lower_state_binders
            // still runs so the codepath stays uniform.)
            a::MatchBody::Call(call, effects) => {
                synth.calls.push(lower_call(call, consts, env, tcx));
                for Node { node: stmt, .. } in effects {
                    for eff in render_effect_or_expand_variant_promotion(
                        stmt,
                        &base.takes_params,
                        consts,
                        &canon,
                        env,
                        tcx,
                    ) {
                        synth.effects.push(eff.into_parsed_effect());
                    }
                }
            }
            a::MatchBody::Noop => {}
        }

        out.push(synth);
    }

    out
}

/// Lower `state_binders { callee_field = state.X, ... }` into the
/// `ParsedStateBinder` shape backends thread to the Lean axiom signature
/// and Kani harness substitution. Each RHS must be exactly
/// `state.<ident>`; richer paths drop the binder with a stderr warning —
/// the spec lint catches the same shape pre-codegen, so this is a
/// defensive guard. Duplicate callee_fields keep the first occurrence
/// (dedup runs across coalesced blocks).
fn lower_state_binders(binders: &[a::StateBinder]) -> Vec<ParsedStateBinder> {
    let mut out: Vec<ParsedStateBinder> = Vec::new();
    for b in binders {
        // RHS must be `state.<ident>` — a `Path { root: "state",
        // segments: [Field(ident)] }`, possibly `Paren`-wrapped.
        let caller_field = extract_state_field(&b.caller_expr.node);
        match caller_field {
            Some(field) => {
                if !out.iter().any(|p| p.callee_field == b.callee_field) {
                    out.push(ParsedStateBinder {
                        callee_field: b.callee_field.clone(),
                        caller_field: field,
                    });
                }
            }
            None => {
                eprintln!(
                    "warning: state_binders RHS for `{}` must be `state.<field>` \
                     (v2.27 Track A restriction); binder dropped",
                    b.callee_field,
                );
            }
        }
    }
    out
}

/// Extract the trailing field ident from a `state.<ident>` binder RHS
/// (optional `Paren` wrappers); `None` if the shape doesn't match.
fn extract_state_field(e: &Expr) -> Option<String> {
    match e {
        Expr::Paren(inner) => extract_state_field(&inner.node),
        Expr::Path(p) => {
            if p.root == "state" && p.segments.len() == 1 {
                if let a::PathSeg::Field(f) = &p.segments[0] {
                    return Some(f.clone());
                }
            }
            None
        }
        _ => None,
    }
}

fn adapt_handler(
    h: &a::HandlerDecl,
    consts: ConstTable,
    env: &TypeEnv,
    tcx: &TreeCx,
) -> ParsedHandler {
    let params: Vec<(String, String)> = typed_pairs(&h.params);
    let canon = canon_scope_for_handler(h, consts, env);

    // `on_account` is the type prefix of the pre-state ref, if qualified.
    //   `Loan.Active` → on_account = Some("Loan"), pre_status = Some("Active")
    //   `Active`      → on_account = None,         pre_status = Some("Active")
    let on_account = h.pre.as_ref().and_then(|p| {
        if p.0.len() >= 2 {
            p.0.get(p.0.len() - 2).cloned()
        } else {
            None
        }
    });

    let mut handler = ParsedHandler {
        name: h.name.clone(),
        doc: h.doc.clone(),
        on_account,
        pre_status: h.pre.as_ref().and_then(|p| p.0.last().cloned()),
        post_status: h.post.as_ref().and_then(|p| p.0.last().cloned()),
        takes_params: params.clone(),
        ..Default::default()
    };

    for Node { node: clause, .. } in &h.clauses {
        match clause {
            a::HandlerClause::Auth(actor) => handler.who = Some(actor.clone()),
            a::HandlerClause::Accounts(descs) => {
                for d in descs {
                    handler.accounts.push(lower_account_descriptor(d));
                }
            }
            a::HandlerClause::Requires { guard, on_fail } => {
                let guard = canonicalize_state_refs(guard, &canon);
                handler
                    .requires
                    .push(lower_requires(&guard, on_fail.clone(), consts, env, tcx));
            }
            a::HandlerClause::Ensures(e) => {
                let e = canonicalize_state_refs(e, &canon);
                handler.ensures.push(lower_ensures(&e, consts, env, tcx));
            }
            a::HandlerClause::Modifies(fs) => {
                handler.modifies = Some(fs.clone());
            }
            a::HandlerClause::Let { name, value } => {
                let value = canonicalize_state_refs(value, &canon);
                let rust = expr_to_rust(&value.node, Ctx::Guard, consts, opts_native(env));
                // Narrow `let X = mul_div_*(...)` to U64 at the binding
                // site — the spec-level operation is U64 → U64 but the
                // u128 helper is used for intermediate `a * b` width.
                // Without this, the canonical `let fee =
                // mul_div_floor(total, fee_bps, BPS); let to_provider =
                // total - fee` lowers to `u64 - u128`, which `cargo
                // build` rejects. Wider arbitrary-expression contexts
                // (`requires X mul_div_floor(...) <= U128_LIT …`) keep
                // the u128 width via the `expr_to_rust` rendering above.
                //
                // `is_mul_div_let_rhs` peels `Paren` / `Old` wrappers so
                // `let X = (mul_div_floor(...))` and `let X =
                // old(mul_div_floor(...))` get the same narrowing as the
                // bare form — matches the precedent in `rust_infer_kind`.
                let rust = if is_mul_div_let_rhs(&value.node) {
                    format!("({}) as u64", rust)
                } else {
                    rust
                };
                handler.let_bindings.push(crate::check::ParsedLetBinding {
                    name: name.clone(),
                    rust_expr: rust,
                    tree: Some(build_expr_tree(&value, tcx)),
                });
            }
            a::HandlerClause::Effect(blocks) => {
                // `effect { … }` may contain a top-level `match` block
                // alongside leaf statements. Two outputs:
                //   1. `handler.effects` — flat union of all leaves.
                //   2. `handler.effect_branches` — `Some` iff the spec
                //      uses `match`. Carries arm structure for branched
                //      emission in the Rust/Kani/proptest backends.
                let mut branches: Option<crate::check::ParsedEffectBranches> = None;
                for Node { node: block, .. } in blocks {
                    match block {
                        a::EffectBlock::Stmt(stmt) => {
                            for eff in render_effect_or_expand_variant_promotion(
                                stmt, &params, consts, &canon, env, tcx,
                            ) {
                                handler.effects.push(eff.into_parsed_effect());
                            }
                        }
                        a::EffectBlock::Match { scrutinee, arms } => {
                            let mut parsed_arms: Vec<crate::check::ParsedEffectArm> = Vec::new();
                            for arm in arms {
                                let mut arm_effects: Vec<crate::check::ParsedEffect> = Vec::new();
                                for nested in &arm.body {
                                    let mut leaves = Vec::new();
                                    nested.node.collect_leaves(&mut leaves);
                                    for stmt in leaves {
                                        for eff in render_effect_or_expand_variant_promotion(
                                            stmt, &params, consts, &canon, env, tcx,
                                        ) {
                                            let eff = eff.into_parsed_effect();
                                            // Mirror into union so flat
                                            // readers see this potential
                                            // write.
                                            handler.effects.push(eff.clone());
                                            arm_effects.push(eff);
                                        }
                                    }
                                }
                                let (pattern_lean, pattern_tree, is_wildcard) = match &arm.pattern {
                                    a::EffectPattern::Literal(v) => {
                                        (v.to_string(), Some(crate::mir::ExprTree::Int(*v)), false)
                                    }
                                    a::EffectPattern::Wildcard => ("_".to_string(), None, true),
                                };
                                parsed_arms.push(crate::check::ParsedEffectArm {
                                    pattern_lean,
                                    pattern_tree,
                                    is_wildcard,
                                    effects: arm_effects,
                                });
                            }
                            branches = Some(crate::check::ParsedEffectBranches {
                                scrutinee_lean: expr_to_lean(
                                    &scrutinee.node,
                                    Ctx::Guard,
                                    consts,
                                    env,
                                ),
                                scrutinee_tree: Some(build_expr_tree(
                                    &canonicalize_state_refs(scrutinee, &canon),
                                    tcx,
                                )),
                                arms: parsed_arms,
                            });
                        }
                    }
                }
                if branches.is_some() {
                    handler.effect_branches = branches;
                }
            }
            a::HandlerClause::Takes(fields) => {
                // Legacy sugar — append to takes_params.
                for f in fields {
                    handler
                        .takes_params
                        .push((f.name.clone(), type_ref_to_string(&f.ty)));
                }
            }
            a::HandlerClause::Transfers(clauses) => {
                for tc in clauses {
                    let amount = tc.amount.as_ref().map(|a| match a {
                        crate::ast::TransferAmount::Literal(v) => v.to_string(),
                        crate::ast::TransferAmount::Path(p) => {
                            // Pest captures amount as raw ident source — emit plain path.
                            p.to_source_string()
                        }
                    });
                    let amount_tree = tc.amount.as_ref().map(|a| match a {
                        crate::ast::TransferAmount::Literal(v) => crate::mir::ExprTree::Int(*v),
                        crate::ast::TransferAmount::Path(p) => {
                            build_expr_tree_raw(&a::Expr::Path(p.clone()), tcx)
                        }
                    });
                    handler.transfers.push(crate::check::ParsedTransfer {
                        from: tc.from.clone(),
                        to: tc.to.clone(),
                        amount,
                        amount_tree,
                        authority: tc.authority.clone(),
                    });
                }
            }
            a::HandlerClause::Emits(ev) => handler.emits.push(ev.clone()),
            a::HandlerClause::AbortsTotal => handler.aborts_total = true,
            a::HandlerClause::Permissionless => handler.permissionless = true,
            a::HandlerClause::Invariant(name) => handler.invariants.push(name.clone()),
            a::HandlerClause::Establishes(name) => handler.establishes.push(name.clone()),
            a::HandlerClause::Abstract { name, ty } => {
                // Stored as (name, DSL-type-string); each backend maps the
                // DSL type to its own representation.
                handler
                    .abstract_binders
                    .push((name.clone(), type_ref_to_string(ty)));
            }
            a::HandlerClause::Include(schema_name) => {
                // Record the reference only; expansion happens in the
                // post-pass at the bottom of `adapt()` once every schema is
                // known. Storing the list on the handler lets it survive the
                // synthetic-match-arm expansion (each arm inherits it).
                handler.schema_includes.push(schema_name.clone());
            }
            a::HandlerClause::Match(_) => {
                // Branches are expanded into synthetic handlers by
                // `expand_handler`; this function only builds the shared
                // base and must ignore the branch clause itself.
            }
            a::HandlerClause::Call(c) => {
                handler.calls.push(lower_call(c, consts, env, tcx));
            }
        }
    }

    handler
}

// ----------------------------------------------------------------------------
// Interface adaptation
// ----------------------------------------------------------------------------

fn adapt_interface<'a>(
    iface: &'a a::InterfaceDecl,
    consts: ConstTable<'a>,
    env: &TypeEnv<'a>,
) -> ParsedInterface {
    let handlers = iface
        .handlers
        .iter()
        .map(|h| adapt_interface_handler(h, consts, env))
        .collect();
    ParsedInterface {
        name: iface.name.clone(),
        doc: iface.doc.clone(),
        program_id: iface.program_id.clone(),
        upstream: iface.upstream.as_ref().map(|u| ParsedUpstream {
            package: u.package.clone(),
            version: u.version.clone(),
            source: u.source.clone(),
            binary_hash: u.binary_hash.clone(),
            idl_hash: u.idl_hash.clone(),
            verified_with: u.verified_with.clone(),
            verified_at: u.verified_at.clone(),
        }),
        // Interface-level `state { ... }` block as (name, type-string)
        // pairs; empty when not declared.
        state_fields: typed_pairs(&iface.state_fields),
        handlers,
    }
}

fn adapt_interface_handler<'a>(
    h: &'a a::InterfaceHandlerDecl,
    consts: ConstTable<'a>,
    env: &TypeEnv<'a>,
) -> ParsedInterfaceHandler {
    let tcx = TreeCx::for_interface_handler(h, env, consts);
    let mut out = ParsedInterfaceHandler {
        name: h.name.clone(),
        doc: h.doc.clone(),
        params: typed_pairs(&h.params),
        discriminant: None,
        accounts: Vec::new(),
        requires: Vec::new(),
        ensures: Vec::new(),
        return_type: h.return_type.as_ref().map(type_ref_to_string),
        // `None` = no binder written (bare `-> Type` or nothing);
        // downstream substitution defaults to the literal `"result"`.
        result_binder: h.result_binder.clone(),
    };

    for Node { node: clause, .. } in &h.clauses {
        match clause {
            a::InterfaceHandlerClause::Discriminant(s) => {
                out.discriminant = Some(s.clone());
            }
            a::InterfaceHandlerClause::Accounts(descs) => {
                for d in descs {
                    out.accounts.push(lower_account_descriptor(d));
                }
            }
            a::InterfaceHandlerClause::Requires { guard, on_fail } => {
                out.requires
                    .push(lower_requires(guard, on_fail.clone(), consts, env, &tcx));
            }
            a::InterfaceHandlerClause::Ensures(e) => {
                out.ensures.push(lower_ensures(e, consts, env, &tcx));
            }
        }
    }

    out
}
