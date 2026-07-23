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

/// Single-account multi-variant ADT view for the Kani model lane (#326).
/// `Some(account)` when the flat `State` carrier can be constrained to the
/// declared variants via a `state_repr_valid` invariant:
///   * `pragma state_repr = adt` is declared;
///   * exactly one account type, with ≥ 2 variants (mirrors
///     `lean_gen_mir::is_multi_variant_adt`; multi-account sections strip
///     the pragma in `scope_parsed_to_account`);
///   * no `Map`-typed state field (indexed shapes have their own lane);
///   * every field absent from at least one variant has an `==`-comparable
///     type default, so the invariant is expressible.
///
/// `None` keeps the flat model AND the recorded `kani_adt_state_repr`
/// unsupported status — never a silent fallback.
pub fn kani_adt_view(spec: &ParsedSpec) -> Option<&crate::check::ParsedAccountType> {
    if !spec.state_repr_is_adt() || spec.account_types.len() != 1 {
        return None;
    }
    let acct = &spec.account_types[0];
    if acct.variants.len() < 2 {
        return None;
    }
    if acct
        .fields
        .iter()
        .any(|(_, t)| t.trim_start().starts_with("Map"))
    {
        return None;
    }
    for (fname, ftype) in &acct.fields {
        let absent_somewhere = acct
            .variants
            .iter()
            .any(|v| !v.fields.iter().any(|(n, _)| n == fname));
        if absent_somewhere && adt_absent_field_default(spec, ftype).is_none() {
            return None;
        }
    }
    Some(acct)
}

/// Default literal for a variant-absent field in the ADT canonical form —
/// the same value the Lean ADT field accessors return for variants that do
/// not carry the field. Restricted to `==`-comparable primitives; `None`
/// means the validity invariant is not expressible for this type and the
/// spec stays on the flat model (reported unsupported).
pub(crate) fn adt_absent_field_default(spec: &ParsedSpec, dsl_ty: &str) -> Option<String> {
    let mut ty = dsl_ty.trim();
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert(ty.to_string()) {
        match spec.type_aliases.iter().find(|(n, _)| n == ty) {
            Some((_, rhs)) => ty = rhs.trim(),
            None => break,
        }
    }
    match ty {
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" => {
            Some("0".to_string())
        }
        "Bool" => Some("false".to_string()),
        "Pubkey" | "Bytes32" => Some("[0u8; 32]".to_string()),
        "Bytes64" => Some("[0u8; 64]".to_string()),
        t if t.starts_with("Fin[") => Some("0".to_string()),
        _ => None,
    }
}

/// Emit the `state_repr_valid` predicate for a single-account ADT spec
/// (#326): under `pragma state_repr = adt` the flat `State` struct is a
/// tagged product, and this invariant pins every field the active variant
/// does not carry to its type default. With the harness preamble assuming
/// it and transitions preserving it (see the canonicalization step in
/// `emit_transition_fn_inner`), the reachable state space is isomorphic to
/// the declared variants — Kani cannot construct cross-variant field
/// combinations the inductive Lean model excludes.
pub fn emit_state_repr_validity_fn(
    out: &mut String,
    spec: &ParsedSpec,
    acct: &crate::check::ParsedAccountType,
) {
    out.push_str("/// ADT canonical form (#326): fields the active variant does not carry\n");
    out.push_str("/// hold their type defaults, matching the Lean field-accessor semantics.\n");
    out.push_str("fn state_repr_valid(s: &State) -> bool {\n");
    out.push_str("    match s.status {\n");
    for v in &acct.variants {
        let conjuncts: Vec<String> = acct
            .fields
            .iter()
            .filter(|(fname, _)| !v.fields.iter().any(|(n, _)| n == fname))
            .map(|(fname, ftype)| {
                let default = adt_absent_field_default(spec, ftype)
                    .expect("kani_adt_view checked absent-field defaults");
                format!("s.{} == {}", fname, default)
            })
            .collect();
        let body = if conjuncts.is_empty() {
            "true".to_string()
        } else {
            conjuncts.join(" && ")
        };
        out.push_str(&format!("        Status::{} => {},\n", v.name, body));
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");
}

/// Append `kani::assume(state_repr_valid(&<var>));` after a symbolic state
/// init when the spec is in ADT mode; no-op otherwise.
pub fn emit_state_repr_valid_assume(out: &mut String, spec: &ParsedSpec, var: &str, indent: &str) {
    if kani_adt_view(spec).is_some() {
        out.push_str(&format!(
            "{indent}kani::assume(state_repr_valid(&{var}));\n"
        ));
    }
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

/// Item visibility for the shared struct/enum/fn emitters: `""` at file
/// scope (single-account artifacts), `"pub "` inside a per-account `mod`
/// so the sibling `mod product` (#324/#331) can name the items. A plain
/// prefix, not an enum: the only two values are compile-checked at the
/// emission sites and anything else fails the generated-artifact gate.
pub const VIS_PRIVATE: &str = "";
pub const VIS_PUB: &str = "pub ";

/// Emit struct declarations for user-defined record types. Called before
/// `emit_state_struct` so records are in scope when State references them.
/// `derives` is the per-backend `#[derive(...)]` list. Empty records are
/// skipped.
pub fn emit_record_structs(
    out: &mut String,
    spec: &crate::check::ParsedSpec,
    derives: &str,
    vis: &str,
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
        out.push_str(&format!("{}struct {} {{\n", vis, rec.name));
        for (fname, ftype) in &rec.fields {
            out.push_str(&format!("    {}{}: {},\n", vis, fname, map_type_fn(ftype)?));
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
    vis: &str,
) -> anyhow::Result<()> {
    for sum in &spec.sum_types {
        let all_unit = sum.variants.iter().all(|v| v.fields.is_empty());
        if !all_unit || sum.variants.is_empty() {
            continue;
        }
        out.push_str(&format!("#[derive({})]\n", derives));
        out.push_str(&format!("{}enum {} {{\n", vis, sum.name));
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
    vis: &str,
) {
    if lifecycle_states.len() < 2 {
        return;
    }
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str(&format!("{}enum Status {{\n", vis));
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
    vis: &str,
) -> anyhow::Result<()> {
    out.push_str(&format!("#[derive({})]\n", derives));
    out.push_str(&format!("{}struct State {{\n", vis));
    for (fname, ftype) in fields {
        out.push_str(&format!("    {}{}: {},\n", vis, fname, map_type_fn(ftype)?));
    }
    if has_lifecycle && !fields.iter().any(|(n, _)| n == "status") {
        out.push_str(&format!("    {}status: Status,\n", vis));
    }
    out.push_str("}\n\n");
    Ok(())
}

/// Emit `fn {inv_name}(s: &State) -> bool { <rust_expr> }` per invariant
/// with a Rust body. Description-only invariants and unsupported
/// quantifier bodies are skipped silently; callers pre-filter to the
/// invariants relevant for the current account section / state shape.
pub fn emit_invariant_predicates(
    out: &mut String,
    invariants: &[&crate::check::ParsedInvariant],
    vis: &str,
) {
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
        out.push_str(&format!("{}fn {}(s: &State) -> bool {{\n", vis, inv.name));
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
    vis: &str,
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
            format!("{}fn {}(pre: &State, post: &State) -> bool", vis, prop.name)
        } else {
            format!("{}fn {}(s: &State) -> bool", vis, prop.name)
        };
        // Stubs underscore the params so the body `true` doesn't trip
        // `unused_variables`.
        let stub_sig = if is_binary {
            format!(
                "{}fn {}(_pre: &State, _post: &State) -> bool",
                vis, prop.name
            )
        } else {
            format!("{}fn {}(_s: &State) -> bool", vis, prop.name)
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
                "{}fn {}_at(s: &State, {}: {}) -> bool {{\n",
                vis, prop.name, slot.binder_name, rust_ty
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
    vis: &str,
    map_type_fn: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<()> {
    emit_transition_fn_inner(out, mir, op, spec, wrapping, None, false, vis, map_type_fn)
}

pub fn emit_transition_fn_for_kani(
    out: &mut String,
    mir: &crate::mir::Mir,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    vis: &str,
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
        vis,
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
    vis: &str,
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
        "{}fn {}(s: &mut State{}) -> bool {{\n",
        vis, op.name, params
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

    // #326 — ADT canonical form (Kani model lane only): fields the
    // post-variant does not carry reset to their type defaults, so
    // transitions preserve `state_repr_valid` and the flat carrier stays
    // isomorphic to the declared variants. Ghosts are not variant fields
    // and are never reset.
    if rewrite_pubkey_comparisons {
        if let Some(acct) = kani_adt_view(spec) {
            if let Some(ref post) = op.post_status {
                if let Some(post_v) = acct.variants.iter().find(|v| &v.name == post) {
                    for (fname, ftype) in &acct.fields {
                        if !post_v.fields.iter().any(|(n, _)| n == fname) {
                            let default = adt_absent_field_default(spec, ftype)
                                .expect("kani_adt_view checked absent-field defaults");
                            out.push_str(&format!(
                                "    s.{} = {}; // State::{} does not carry this field\n",
                                fname, default, post
                            ));
                        }
                    }
                }
            }
        }
    }

    // Ghost (spec-only) field updates: a ghost with `on <this handler>`
    // assigns after the normal effects; others are framed (unchanged).
    // Values read `s.<ghost>` + params, matching the Lean transition.
    // Arithmetic wraps in release (the `verify --proptest` path); the
    // sequence harness additionally bounds `arb_op` numeric params so an
    // aggregate cannot overflow across a run (see `emit_sequence_test_for`),
    // which keeps the debug (`cargo test`) path panic-free too.
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
