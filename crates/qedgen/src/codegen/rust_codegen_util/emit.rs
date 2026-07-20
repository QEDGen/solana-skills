//! State/struct/enum/transition emission: symbolic & zeroed `State` init,
//! constants, record structs, unit-enum sums, the lifecycle `Status` enum,
//! invariant/property predicates, after-store hooks, and the shared
//! transition-fn emitters for the Kani/proptest backends.

use super::*;

/// Emit `let <name>: <T> = <source>;` for each `abstract <name> : <T>`
/// binder. `source` is the per-backend symbolic-input expression
/// (`kani::any()`, `todo!("…")`, …). Call after takes_params emission so
/// the binders are in scope for the following assume/prop_assume reads.
pub fn emit_abstract_binders(
    out: &mut String,
    handler: &crate::check::ParsedHandler,
    indent: &str,
    source: &str,
    map_ty: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    for (name, ty_str) in &handler.abstract_binders {
        let ty = map_ty(ty_str)?;
        out.push_str(&format!("{}let {}: {} = {};\n", indent, name, ty, source));
    }
    Ok(())
}

/// Emit `let mut s = State { ... };` with every mutable field bound to
/// `kani::any()`. When the per-account lifecycle has ≥2 states, the
/// synthetic `status` field is also `kani::any()` so callers can layer
/// `kani::assume(s.status == Status::<X>)` on top.
pub fn emit_state_init_symbolic(
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

/// Emit `let mut s = State { ... };` zeroed, with `status` set to the
/// initial lifecycle state — the canonical pre-state for init-handler
/// harnesses. Type-aware defaults come from the shared DSL type surface.
pub fn emit_state_init_zeroed(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
    spec: &crate::check::ParsedSpec,
) {
    out.push_str("    let mut s = State {\n");
    for (fname, ftype) in mutable_fields {
        if let Some(default) = spec.default_value_for_type(ftype) {
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
/// pre-status declaration AND this section has a lifecycle; no-op otherwise.
/// Without this, guard-rejection / abort harnesses can pass for the wrong
/// reason — the handler rejects on a mismatched symbolic status, not
/// because the requires/guard fired.
pub fn emit_pre_status_assume(
    out: &mut String,
    op: &crate::check::ParsedHandler,
    lifecycle_states: &[String],
) {
    if lifecycle_states.len() < 2 {
        return;
    }
    if let Some(ref pre) = op.pre_status {
        out.push_str(&format!("    kani::assume(s.status == Status::{});\n", pre));
    }
}

pub fn emit_constants(out: &mut String, constants: &[(String, String)]) {
    for (name, value) in constants {
        let upper = name.to_uppercase();
        let const_type = infer_const_type(value);
        out.push_str(&format!("const {}: {} = {};\n", upper, const_type, value));
    }
    if !constants.is_empty() {
        out.push('\n');
    }
}

/// Emit struct declarations for user-defined record types. Called before
/// `emit_state_struct` so records are in scope when State references them.
/// `derives` is the per-backend `#[derive(...)]` list. Empty records are
/// skipped.
pub fn emit_record_structs(
    out: &mut String,
    spec: &crate::check::ParsedSpec,
    derives: &str,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    for rec in &spec.records {
        if rec.fields.is_empty() {
            continue;
        }
        // Flat `state { … }` forms produce a record literally named
        // `State`; the state-machine `struct State` (lifecycle + ghost
        // fields) is emitted separately, so skip to avoid a duplicate.
        if rec.name == "State" {
            continue;
        }
        out.push_str(&format!("#[derive({})]\n", derives));
        out.push_str(&format!("struct {} {{\n", rec.name));
        for (fname, ftype) in &rec.fields {
            out.push_str(&format!("    {}: {},\n", fname, map_type_fn(ftype)?));
        }
        out.push_str("}\n\n");
    }
    Ok(())
}

/// Emit enums for sum-types whose variants are ALL unit (`type Error |
/// NotAdmin | …` → `enum Error { NotAdmin, … }`). Payload-carrying sums
/// (`type State | Active of { … }`) are skipped — codegen flattens those
/// into a `struct State`, and an `enum State` would collide.
pub fn emit_unit_enum_sums(
    out: &mut String,
    spec: &crate::check::ParsedSpec,
    derives: &str,
) -> anyhow::Result<()> {
    for sum in &spec.sum_types {
        let all_unit = sum.variants.iter().all(|v| v.fields.is_empty());
        if !all_unit || sum.variants.is_empty() {
            continue;
        }
        out.push_str(&format!("#[derive({})]\n", derives));
        out.push_str(&format!("enum {} {{\n", sum.name));
        for variant in &sum.variants {
            out.push_str(&format!("    {},\n", variant.name));
        }
        out.push_str("}\n\n");
    }
    Ok(())
}

/// True when the spec declares a multi-state lifecycle the harness layer
/// should model as a `Status` enum + `status` field; single-state / no
/// lifecycle needs no discriminator.
pub fn has_lifecycle(spec: &crate::check::ParsedSpec) -> bool {
    spec.lifecycle_states.len() >= 2
}

/// Emit the synthetic `Status` enum from a per-account or per-spec
/// lifecycle slice; no-op below two states. Synthetic: derived from the
/// State sum-type's variants, not user-declared — without a status field,
/// lifecycle-only handlers have nothing to write and every harness against
/// them is vacuous. Multi-ADT codegen must pass `acct.lifecycle` so each
/// `mod <acct>` gets its own variants, not the spec-level ones.
pub fn emit_lifecycle_status_enum_from(
    out: &mut String,
    lifecycle_states: &[String],
    derives: &str,
) {
    if lifecycle_states.len() < 2 {
        return;
    }
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str("enum Status {\n");
    for state in lifecycle_states {
        out.push_str(&format!("    {},\n", state));
    }
    out.push_str("}\n\n");
}

/// Emit a State struct with configurable derives. `map_type_fn` errors on
/// unrecognized DSL types so codegen fails loudly. `has_lifecycle` gates
/// the `status: Status` field — multi-ADT codegen threads the per-account
/// lifecycle, not the spec-level one. Callers must have already emitted
/// the `Status` enum via `emit_lifecycle_status_enum_from`.
pub fn emit_state_struct_with_lifecycle(
    out: &mut String,
    fields: &[&(String, String)],
    derives: &str,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
    has_lifecycle: bool,
) -> anyhow::Result<()> {
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str("struct State {\n");
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}: {},\n", fname, map_type_fn(ftype)?));
    }
    if has_lifecycle && !fields.iter().any(|(n, _)| n == "status") {
        out.push_str("    status: Status,\n");
    }
    out.push_str("}\n\n");
    Ok(())
}

/// Emit `fn {inv_name}(s: &State) -> bool { <rust_expr> }` per invariant
/// with a Rust body. Description-only invariants and unsupported
/// quantifier bodies are skipped silently; callers pre-filter to the
/// invariants relevant for the current account section / state shape.
pub fn emit_invariant_predicates(out: &mut String, invariants: &[&crate::check::ParsedInvariant]) {
    for inv in invariants {
        let Some(rust_expr) = inv.rust_expr.as_deref() else {
            continue;
        };
        if crate::check::rust_expr_is_unsupported(rust_expr) {
            continue;
        }
        let doc_body = inv
            .lean_expr
            .as_deref()
            .map(|le| format!(" — {}", le))
            .unwrap_or_default();
        out.push_str(&format!("/// Invariant: {}{}\n", inv.name, doc_body));
        out.push_str(&format!("fn {}(s: &State) -> bool {{\n", inv.name));
        out.push_str(&format!("    {}\n", rust_expr));
        out.push_str("}\n\n");
    }
}

/// Emit property predicate functions. `map_type_fn` lets the per-slot
/// `<prop>_at(s, <binder>)` predicate render a target-specific binder type
/// (Quasar Pod vs native Rust differ for non-primitive binders).
///
/// Emission shape:
///   - Always `fn <prop>(s: &State) -> bool` — the real expression, or
///     `true` when the body has a quantifier (the harness drives the check
///     via `<prop>_at` instead).
///   - When `prop.per_slot` is Some, also `fn <prop>_at(s: &State,
///     <binder>: <ty>) -> bool` — the `forall` inner expression with the
///     binder free; harnesses bind it symbolically for a non-vacuous check.
pub fn emit_property_predicates_with(
    out: &mut String,
    properties: &[ParsedProperty],
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) {
    for prop in properties {
        // Tree-native math-exact rendering (arithmetic widened so
        // evaluating the predicate can't overflow-panic — issue #146);
        // string fallbacks for tree-less properties (see
        // `property_predicate_rust`).
        let Some(rust_expr) = property_predicate_rust(prop) else {
            continue;
        };
        let doc = prop.expression.as_deref().unwrap_or("");
        out.push_str(&format!("/// {}: {}\n", prop.name, doc));
        // Binary properties (body contains `old(...)`) take `(pre, post)`;
        // the adapter renders `state.x` → `post.x`, `old(state.x)` →
        // `pre.x`. Kani's preservation harness dispatches assertion arity
        // on `prop.class`.
        let is_binary = prop.class == crate::check::PropertyClass::Binary;
        let sig = if is_binary {
            format!("fn {}(pre: &State, post: &State) -> bool", prop.name)
        } else {
            format!("fn {}(s: &State) -> bool", prop.name)
        };
        // Stubs underscore the params so the body `true` doesn't trip
        // `unused_variables`.
        let stub_sig = if is_binary {
            format!("fn {}(_pre: &State, _post: &State) -> bool", prop.name)
        } else {
            format!("fn {}(_s: &State) -> bool", prop.name)
        };
        if crate::check::rust_expr_is_unsupported(&rust_expr) {
            // Quantifier body: emit a `true` stub; the harness preamble
            // skips calling into these predicates.
            out.push_str(&format!("{} {{\n", stub_sig));
            out.push_str(&format!(
                "    // {} — property uses a quantifier; lower at the harness level.\n",
                rust_expr.trim()
            ));
            out.push_str("    true\n");
            out.push_str("}\n\n");
        } else {
            out.push_str(&format!("{} {{\n", sig));
            out.push_str(&format!("    {}\n", rust_expr));
            out.push_str("}\n\n");
        }
        // Per-slot predicate: the adapter populates `per_slot` for
        // mechanically-lowerable `forall <binder> : <ty>, body` properties;
        // harnesses bind `<binder>` symbolically and call `<prop>_at`.
        if let Some(slot) = &prop.per_slot {
            let rust_ty =
                map_type_fn(&slot.binder_type).unwrap_or_else(|_| slot.binder_type.clone());
            out.push_str(&format!(
                "/// {}: per-slot check at `{}: {}` (v2.20 forall lowering)\n",
                prop.name, slot.binder_name, slot.binder_type
            ));
            out.push_str("#[allow(unused_variables)]\n");
            out.push_str(&format!(
                "fn {}_at(s: &State, {}: {}) -> bool {{\n",
                prop.name, slot.binder_name, rust_ty
            ));
            out.push_str(&format!("    {}\n", slot.rust_body));
            out.push_str("}\n\n");
        }
    }
}

/// Emit `hook after_store(<field>)` assertions, anchored right after the
/// field's effect so they see the post-store state. A failed assertion
/// panics, which proptest/Kani surface as a failure. On-chain codegen
/// never uses this emitter, so hooks don't reach the program.
fn emit_after_store_hooks(
    out: &mut String,
    hooks: &[crate::mir::HookMir],
    field: &str,
    indent: &str,
) {
    let base = effect_target_base(field);
    for hook in hooks {
        if let crate::mir::HookKind::AfterStore(f) = &hook.kind {
            if f == base {
                for a in &hook.asserts {
                    out.push_str(&format!(
                        "{}assert!({}, \"hook after_store({}) violated\");\n",
                        indent,
                        mir_expr_rust(a),
                        base
                    ));
                }
            }
        }
    }
}

/// Both transition emitters iterate the handler's lowered MIR body for
/// effects (`stmt_effect_triple`; #66 — a new `Stmt` variant is a compile
/// error at the adaptor). The guard / status / let-binding / ghost
/// scaffold stays `ParsedHandler`-fed by design — predicate/account
/// surface, same boundary as `codegen_mir`'s guards.
pub fn emit_transition_fn(
    out: &mut String,
    mir: &crate::mir::Mir,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    emit_transition_fn_inner(out, mir, op, spec, wrapping, None, false, map_type_fn)
}

pub fn emit_transition_fn_for_kani(
    out: &mut String,
    mir: &crate::mir::Mir,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    let account_env =
        handler_needs_account_env(op).then(|| handler_account_env_struct_name(&op.name));
    emit_transition_fn_inner(
        out,
        mir,
        op,
        spec,
        wrapping,
        account_env.as_deref(),
        true,
        map_type_fn,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn emit_transition_fn_inner(
    out: &mut String,
    mir: &crate::mir::Mir,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    account_env_struct: Option<&str>,
    rewrite_pubkey_comparisons: bool,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    let body = mir
        .handler_block(&op.name)
        .ok_or_else(|| anyhow::anyhow!("MIR has no handler `{}`", op.name))?;
    if let Some(ref doc) = op.doc {
        out.push_str(&format!("/// {}\n", doc.trim()));
    }

    let mut params = String::new();
    if let Some(account_env_struct) = account_env_struct {
        params.push_str(&format!(", accounts: &{}", account_env_struct));
    }
    params.push_str(
        &op.takes_params
            .iter()
            .chain(op.abstract_binders.iter())
            .map(|(n, t)| map_type_fn(t).map(|rt| format!(", {}: {}", n, rt)))
            .collect::<anyhow::Result<Vec<_>>>()?
            .concat(),
    );
    // Abstract binders ride alongside real handler params; callers pass a
    // symbolic / arbitrary value for each.
    out.push_str(&format!(
        "fn {}(s: &mut State{}) -> bool {{\n",
        op.name, params
    ));

    // Guard check (requires clauses)
    if let Some(guard_expr) =
        collect_full_guard_with_account_env(op, wrapping, account_env_struct.map(|_| "accounts"))
    {
        let guard_terms = collect_guard_terms_with_account_env(
            op,
            wrapping,
            account_env_struct.map(|_| "accounts"),
        );
        if rewrite_pubkey_comparisons && guard_terms.len() > 8 {
            for term in guard_terms {
                let term_expr = rewrite_kani_pubkey_comparisons(&term, op, spec);
                if let Some(negated) = negate_simple_top_level_comparison(&term_expr) {
                    out.push_str(&format!("    if {} {{\n", negated));
                } else {
                    out.push_str(&format!("    if !({}) {{\n", term_expr));
                }
                out.push_str("        return false;\n");
                out.push_str("    }\n");
            }
        } else {
            let guard_expr = if rewrite_pubkey_comparisons {
                rewrite_kani_pubkey_comparisons(&guard_expr, op, spec)
            } else {
                guard_expr
            };
            out.push_str(&format!("    if !({}) {{\n", guard_expr));
            out.push_str("        return false;\n");
            out.push_str("    }\n");
        }
    }

    // Pre-status check — handlers declared `State.X -> State.Y` must reject
    // when the current lifecycle state isn't `X`. Without this, lifecycle-
    // only handlers (whose effects don't touch user fields) would have
    // empty bodies and every cover/liveness harness against them would
    // pass tautologically.
    if has_lifecycle(spec) {
        if let Some(ref pre) = op.pre_status {
            out.push_str(&format!("    if s.status != Status::{} {{\n", pre));
            out.push_str("        return false;\n");
            out.push_str("    }\n");
        }
    }

    // Effect-subscript bounds (#298): the model state space allows count
    // fields past a bounded container's capacity, so an effect write like
    // `s.voted[member_index] = 1` can index out of range where deployed
    // code would abort the transaction. Reject instead of panicking.
    // Requires-derived subscripts are already guarded (bounds terms lead
    // the collected guard above); only effect-only subscripts emit here.
    {
        let guarded = requires_bounds_pairs(op);
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (field, _, value) in block_effect_triples_deep(body) {
            field_string_subscripts(
                &strip_variant_prefix_for_flat_state(&effect_path_source(field), spec),
                &mut pairs,
            );
            if let Some(tree) = value.tree.as_ref() {
                collect_tree_subscripts(tree, &mut pairs);
            }
        }
        for term in render_bounds_terms(
            &pairs
                .iter()
                .filter(|p| !guarded.contains(p))
                .cloned()
                .collect::<Vec<_>>(),
        ) {
            out.push_str(&format!("    if !({term}) {{\n"));
            out.push_str("        return false;\n");
            out.push_str("    }\n");
        }
    }

    // Spec-level `let` bindings emit BEFORE the effect block so effect
    // RHSs can reference them.
    for b in &op.let_bindings {
        out.push_str(&format!("    let {} = {};\n", b.name, b.rust_expr));
    }

    // Parallel effect semantics: snapshot every field the block both
    // writes and reads, so a later statement's RHS observes the
    // PRE-state value — matching the Lean model's record update and the
    // conformance harnesses' `pre_<field>` assertions instead of the
    // emission order. Computed from the exact triples emitted below
    // (post pubkey-skip), so every snapshot is referenced.
    let emitted_triples: Vec<(&crate::mir::Path, &'static str, &crate::mir::Expr)> =
        block_effect_triples_deep(body)
            .into_iter()
            .filter(|(field, _, _)| {
                account_env_struct.is_some()
                    || !field_type_is_pubkey(&effect_path_source(field), op, spec)
            })
            .collect();
    let pre_fields = parallel_snapshot_fields(&emitted_triples, spec);
    for f in &pre_fields {
        out.push_str(&format!("    let pre_{f} = s.{f};\n"));
    }

    // Apply effects. Per-effect arithmetic semantics: `+=` → checked_add
    // (short-circuit via `return false`, matching deployed
    // `checked_add(..).ok_or(err)?`), `+=!` → saturating, `+=?` → wrapping
    // (same tiers for `-=`). The `wrapping` flag forces default `+=`/`-=`
    // to wrap (proptest full-state-space mode); explicit `+=!`/`+=?`
    // always honor their declared semantics.
    //
    // Effects targeting `Pubkey` fields are skipped when there's no
    // account env: accounts aren't carried into the pure model, and pubkey
    // identity is validated by the accounts struct at handler entry.
    //
    // `match` inside `effect { … }` lowers to `Stmt::Branch` (suppressing
    // the flat union `op.effects` still carries for back-compat readers).
    // Emit a real Rust `match` when present; else fall through to the flat
    // list.
    if let Some((scrutinee, arms, default)) = body.stmts.iter().find_map(|st| match st {
        crate::mir::Stmt::Branch {
            scrutinee,
            arms,
            default,
        } => Some((scrutinee, arms, default)),
        _ => None,
    }) {
        let scrutinee_rust = match scrutinee {
            crate::mir::BranchScrutinee::Match(e) => mir_expr_rust(e),
            crate::mir::BranchScrutinee::Predicate(p) => mir_expr_rust(&p.0),
        };
        out.push_str(&format!("    match {} {{\n", scrutinee_rust));
        let emit_arm_block = |out: &mut String, block: &crate::mir::Block| {
            for (field, op_kind, value) in block_effect_triples(block) {
                let field_name = effect_path_source(field);
                if account_env_struct.is_none() && field_type_is_pubkey(&field_name, op, spec) {
                    continue;
                }
                if account_env_struct.is_some() {
                    emit_one_effect_with_account_env(
                        out,
                        spec,
                        wrapping,
                        field,
                        op_kind,
                        value,
                        "            ",
                        "accounts",
                        &pre_fields,
                    );
                } else {
                    emit_one_effect(
                        out,
                        spec,
                        wrapping,
                        field,
                        op_kind,
                        value,
                        "            ",
                        &pre_fields,
                    );
                }
                emit_after_store_hooks(out, &mir.hooks, &field_name, "            ");
            }
        };
        for arm in arms {
            let pattern = arm
                .pattern
                .as_ref()
                .map(mir_expr_rust)
                .unwrap_or_else(|| "_".to_string());
            out.push_str(&format!("        {} => {{\n", pattern));
            emit_arm_block(out, &arm.block);
            out.push_str("        }\n");
        }
        if let Some(default_block) = default {
            out.push_str("        _ => {\n");
            emit_arm_block(out, default_block);
            out.push_str("        }\n");
        } else {
            // Spec patterns are literal-only, so synthesize a no-op
            // wildcard to keep the match exhaustive even if the spec
            // forgot the catch-all. The drift hash still records the
            // spec's actual arms.
            out.push_str("        _ => {}\n");
        }
        out.push_str("    }\n");
    } else {
        // #66 — iterate the lowered MIR body, not `op.effects`:
        // `stmt_effect_triple` projects effect-shaped stmts onto the
        // triple these templates consume (byte-identical; see its doc)
        // and skips non-effect variants in-stream without reordering.
        for (field, op_kind, value) in block_effect_triples(body) {
            let field_name = effect_path_source(field);
            if account_env_struct.is_none() && field_type_is_pubkey(&field_name, op, spec) {
                continue;
            }
            if account_env_struct.is_some() {
                emit_one_effect_with_account_env(
                    out,
                    spec,
                    wrapping,
                    field,
                    op_kind,
                    value,
                    "    ",
                    "accounts",
                    &pre_fields,
                );
            } else {
                emit_one_effect(
                    out,
                    spec,
                    wrapping,
                    field,
                    op_kind,
                    value,
                    "    ",
                    &pre_fields,
                );
            }
            emit_after_store_hooks(out, &mir.hooks, &field_name, "    ");
        }
    }

    // Post-status assignment — drives the lifecycle transition declared in
    // the handler signature (`State.X -> State.Y`). Combined with the pre-
    // status check above, this turns lifecycle-only handlers into real
    // state machines instead of `fn h() -> bool { true }` stubs.
    if has_lifecycle(spec) {
        if let Some(ref post) = op.post_status {
            out.push_str(&format!("    s.status = Status::{};\n", post));
        }
    }

    // Ghost (spec-only) field updates: a ghost with `on <this handler>`
    // assigns after the normal effects; others are framed (unchanged).
    // Values read `s.<ghost>` + params, matching the Lean transition.
    // Arithmetic wraps in release (the `verify --proptest` path), so an
    // arbitrary-state aggregate never panics on model overflow.
    for ghost in &spec.ghosts {
        for u in &ghost.updates {
            if u.handler == op.name {
                out.push_str(&format!("    s.{} = {};\n", ghost.name, u.value_rust));
            }
        }
    }

    out.push_str("    true\n");
    out.push_str("}\n\n");
    Ok(())
}
