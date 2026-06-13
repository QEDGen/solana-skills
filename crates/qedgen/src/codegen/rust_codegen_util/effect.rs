//! Account-environment detection, value/state resolution, and effect-triple
//! lowering: the `(field, op_kind, value)` projection of MIR `Stmt`s and the
//! single-effect Rust emitters consumed by the transition bodies.

use super::*;

pub fn handler_needs_account_env(op: &ParsedHandler) -> bool {
    op.requires
        .iter()
        .any(|r| mentions_handler_account_pubkey(&r.rust_expr, &op.accounts))
        || op
            .guard_str
            .as_ref()
            .is_some_and(|g| mentions_handler_account_pubkey(g, &op.accounts))
        || op
            .effects
            .iter()
            .any(|(_, _, value)| is_account_pubkey_ref(value.trim(), &op.accounts))
        || op.effect_branches.as_ref().is_some_and(|branches| {
            branches.arms.iter().any(|arm| {
                arm.effects
                    .iter()
                    .any(|(_, _, value)| is_account_pubkey_ref(value.trim(), &op.accounts))
            })
        })
}

pub fn handler_account_env_struct_name(op_name: &str) -> String {
    let sanitized = crate::codegen_shared::sanitize_ident(op_name);
    let mut out = String::new();
    for part in sanitized.split('_').filter(|p| !p.is_empty()) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        "Handler".to_string()
    } else {
        format!("{}Accounts", out)
    }
}

/// Resolve an effect value to a Rust expression: handler param name,
/// declared constant, `let … = call` binding, state field (rebound to
/// `<state_binder>X` when provided), or pass-through literal.
///
/// State fields need a binder because upstream effect-RHS rendering already
/// stripped the `state.` prefix (chumsky_adapter::render_effect) and each
/// target binds state differently (proptest `s`, Anchor `self.<acct>`, …);
/// a bare field name would be E0425 at compile time. Pass `None` for
/// pass-through (bare identifier).
pub fn resolve_value(
    value: &str,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    state_binder: Option<&str>,
) -> String {
    if op.takes_params.iter().any(|(n, _)| n == value) {
        value.to_string()
    } else if let Some((_, const_val)) = spec.constants.iter().find(|(n, _)| n == value) {
        const_val.clone()
    } else if op
        .calls
        .iter()
        .any(|c| c.result_binding.as_deref() == Some(value))
    {
        // `let <name> = call …` binding is in scope for subsequent
        // effects / requires; render as the bare let-bound local.
        value.to_string()
    } else if let Some(binder) = state_binder {
        if is_state_field(value, spec) {
            format!("{}{}", binder, value)
        } else {
            value.to_string()
        }
    } else {
        value.to_string()
    }
}

pub fn resolve_value_with_account_env(
    value: &str,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    state_binder: Option<&str>,
    account_binder: Option<&str>,
) -> String {
    if let Some(binder) = account_binder {
        let rewritten = rewrite_account_pubkey_refs(value, &op.accounts, binder);
        if rewritten != value {
            return rewritten;
        }
    }
    resolve_value(value, op, spec, state_binder)
}

/// True when the bare identifier names a state field in the flat
/// `state_fields` list or any `account_types[*].fields` (multi-account).
fn is_state_field(name: &str, spec: &ParsedSpec) -> bool {
    if spec.state_fields.iter().any(|(n, _)| n == name) {
        return true;
    }
    for acct in &spec.account_types {
        if acct.fields.iter().any(|(n, _)| n == name) {
            return true;
        }
    }
    false
}

// ============================================================================
// Shared helpers — used by kani, proptest, unit_test, integration generators
// ============================================================================

/// Resolve state fields for the spec, handling multi-account layout.
/// Returns the fields for the primary account type.
pub fn resolve_state_fields(spec: &ParsedSpec) -> &[(String, String)] {
    if spec.account_types.len() > 1 {
        &spec.account_types[0].fields
    } else {
        &spec.state_fields
    }
}

/// Every declared state field flows through here (the "mutable" naming is
/// historical; Pubkey fields are first-class since they lower to
/// `[u8; 32]`).
pub fn mutable_fields(fields: &[(String, String)]) -> Vec<&(String, String)> {
    fields.iter().collect()
}

/// The base field name an effect targets. `accounts[i].active` → `accounts`;
/// `foo.bar` → `foo`; `plain` → `plain`. Used by `check_effect_targets` to
/// look up the target in the declared state schema.
pub fn effect_target_base(path: &str) -> &str {
    let path = path.trim();
    let end = path.find(['[', '.']).unwrap_or(path.len());
    &path[..end]
}

/// Strip a leading `<Variant>.` prefix when the root names a multi-variant
/// ADT variant; unchanged otherwise. Harness `State` models carry fields in
/// union form, so `Active.balance := …` must lower to `s.balance = …`.
/// Owned return so callers can pass through `&str`-only APIs.
pub fn strip_variant_prefix_for_flat_state(path: &str, spec: &ParsedSpec) -> String {
    if let Some(dot) = path.find('.') {
        let head = &path[..dot];
        let is_variant = spec
            .account_types
            .iter()
            .any(|a| a.variants.iter().any(|v| v.name == head));
        if is_variant {
            return path[dot + 1..].to_string();
        }
    }
    path.to_string()
}

/// Project an effect-shaped MIR `Stmt` back onto the `(field, op_kind,
/// value)` triple the string templates below consume — the #66 adaptor
/// that makes `Stmt` the iteration source for the Kani/proptest transition
/// bodies while keeping output byte-identical (`lower_body` preserves spec
/// order, `Path` round-trips the dotted field, `Expr::from_raw` carries
/// the RHS verbatim in `.rust`).
///
/// Returns `None` for every non-effect variant — each with the reason it
/// renders as *nothing* in the pure spec-model transition. The match is
/// exhaustive by discipline (no `_` arm; see the `Stmt` enum doc): a new
/// `Stmt` variant is a compile error here, forcing an explicit decision
/// for the Kani/proptest backends.
pub fn stmt_effect_triple(stmt: &crate::mir::Stmt) -> Option<(String, &'static str, &str)> {
    use crate::mir::Stmt;
    match stmt {
        Stmt::Assign { path, rhs } => Some((path.segments.join("."), "set", rhs.rust.as_str())),
        Stmt::CheckedAdd { path, delta, .. } => {
            // The lowered per-site error name is Lean/scaffold surface;
            // the pure model signals overflow via `return false`.
            Some((path.segments.join("."), "add", delta.rust.as_str()))
        }
        Stmt::CheckedSub { path, delta, .. } => {
            Some((path.segments.join("."), "sub", delta.rust.as_str()))
        }
        Stmt::SatAdd { path, delta } => {
            Some((path.segments.join("."), "add_sat", delta.rust.as_str()))
        }
        Stmt::SatSub { path, delta } => {
            Some((path.segments.join("."), "sub_sat", delta.rust.as_str()))
        }
        Stmt::WrapAdd { path, delta } => {
            Some((path.segments.join("."), "add_wrap", delta.rust.as_str()))
        }
        Stmt::WrapSub { path, delta } => {
            Some((path.segments.join("."), "sub_wrap", delta.rust.as_str()))
        }
        // Guard surface: `requires X else Err` folds into the guard
        // conjunction via `collect_full_guard*`, not the body.
        Stmt::RequireOrAbort { .. } => None,
        // CPI surface: no state mutation in the pure model; the Kani
        // CPI-fact harnesses read the call sites separately.
        Stmt::TokenTransfer { .. } => None,
        Stmt::Cpi { .. } => None,
        // Lifecycle surface: the transition body drives variant changes
        // through the pre/post-status writes, not a promote statement.
        Stmt::VariantPromote { .. } => None,
        // Branch arms are rendered by the per-arm match path; walking
        // them here would double-emit.
        Stmt::Branch { .. } => None,
        // Abort clauses are harnessed from the `aborts_if` predicate
        // surface; in the body they carry no state mutation.
        Stmt::Abort(_) => None,
        // Events: auxiliary, no state mutation in the pure model.
        Stmt::Emit { .. } => None,
    }
}

/// All effect triples of a lowered handler body, in spec order. The
/// shared iteration source for the Kani/proptest transition emitters,
/// conformance harnesses, and overflow filters. Top-level only —
/// `Stmt::Branch` arms are NOT descended (per-arm rendering owns
/// those); use [`block_effect_triples_deep`] for analyses that must
/// see conditionally-applied effects.
pub fn block_effect_triples(body: &crate::mir::Block) -> Vec<(String, &'static str, &str)> {
    body.stmts.iter().filter_map(stmt_effect_triple).collect()
}

/// Like [`block_effect_triples`] but descends into `Stmt::Branch`
/// arms/default. For *may-this-effect-fire* analyses (overflow filters
/// and tests) where a conditionally-applied effect counts.
pub fn block_effect_triples_deep(body: &crate::mir::Block) -> Vec<(String, &'static str, &str)> {
    fn walk<'a>(stmts: &'a [crate::mir::Stmt], out: &mut Vec<(String, &'static str, &'a str)>) {
        for s in stmts {
            if let Some(t) = stmt_effect_triple(s) {
                out.push(t);
            }
            if let crate::mir::Stmt::Branch { arms, default, .. } = s {
                for a in arms {
                    walk(&a.block.stmts, out);
                }
                if let Some(d) = default {
                    walk(&d.stmts, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&body.stmts, &mut out);
    out
}

/// Render a single `(field, op_kind, value)` triple into Rust at the given
/// indent. The helper writes the trailing newline; the caller controls
/// where the statement sits relative to its surrounding block.
#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &str,
    indent: &str,
) {
    emit_one_effect_inner(out, op, spec, wrapping, field, op_kind, value, indent, None);
}

#[allow(clippy::too_many_arguments)]
fn emit_one_effect_inner(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &str,
    indent: &str,
    account_binder: Option<&str>,
) {
    // Harnesses run against a flat `State` (union-of-variant-fields view):
    // strip the variant prefix so `Variant.field := …` emits `s.field = …`;
    // the variant itself is tracked via `s.status`.
    let field_owned = strip_variant_prefix_for_flat_state(field, spec);
    let field = field_owned.as_str();
    // Body binds state as `s` — pass that binder so a bare state-field RHS
    // renders as `s.<field>`.
    let rust_value = resolve_value_with_account_env(value, op, spec, Some("s."), account_binder);
    match op_kind {
        "set" => {
            out.push_str(&format!("{indent}s.{field} = {rust_value};\n"));
        }
        "add" => {
            if wrapping {
                out.push_str(&format!(
                    "{indent}s.{field} = s.{field}.wrapping_add({rust_value});\n"
                ));
            } else {
                out.push_str(&format!(
                    "{indent}match s.{field}.checked_add({rust_value}) {{\n\
                     {indent}    Some(__v) => s.{field} = __v,\n\
                     {indent}    None => return false,\n\
                     {indent}}}\n"
                ));
            }
        }
        "add_sat" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.saturating_add({rust_value});\n"
            ));
        }
        "add_wrap" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.wrapping_add({rust_value});\n"
            ));
        }
        "sub" => {
            if wrapping {
                out.push_str(&format!(
                    "{indent}s.{field} = s.{field}.wrapping_sub({rust_value});\n"
                ));
            } else {
                out.push_str(&format!(
                    "{indent}match s.{field}.checked_sub({rust_value}) {{\n\
                     {indent}    Some(__v) => s.{field} = __v,\n\
                     {indent}    None => return false,\n\
                     {indent}}}\n"
                ));
            }
        }
        "sub_sat" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.saturating_sub({rust_value});\n"
            ));
        }
        "sub_wrap" => {
            out.push_str(&format!(
                "{indent}s.{field} = s.{field}.wrapping_sub({rust_value});\n"
            ));
        }
        _ => {
            out.push_str(&format!(
                "{indent}// unknown effect: {field} {op_kind} {value}\n"
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect_with_account_env(
    out: &mut String,
    op: &ParsedHandler,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &str,
    indent: &str,
    account_binder: &str,
) {
    emit_one_effect_inner(
        out,
        op,
        spec,
        wrapping,
        field,
        op_kind,
        value,
        indent,
        Some(account_binder),
    );
}

/// Verify every effect-target field is declared somewhere in the state
/// schema (`state_fields`, per-account fields, or a sum-variant payload).
/// Errors name the handler and field — catching this at codegen time beats
/// a `cargo check` error 1000 lines into the generated harness.
pub fn check_effect_targets(spec: &ParsedSpec) -> anyhow::Result<()> {
    use std::collections::HashSet;

    // Collect every declared field name from every place fields can live.
    let mut declared: HashSet<&str> = HashSet::new();
    for (n, _) in &spec.state_fields {
        declared.insert(n.as_str());
    }
    for acct in &spec.account_types {
        for (n, _) in &acct.fields {
            declared.insert(n.as_str());
        }
    }
    for rec in &spec.records {
        for (n, _) in &rec.fields {
            declared.insert(n.as_str());
        }
    }
    for sum in &spec.sum_types {
        for variant in &sum.variants {
            for (n, _) in &variant.fields {
                declared.insert(n.as_str());
            }
        }
    }

    // Variant-prefixed targets (`Active.balance`) are legal: index variant
    // fields so the check re-targets at the field beneath the prefix
    // instead of false-positive-bailing on the variant name.
    let mut variant_fields: std::collections::HashMap<&str, HashSet<&str>> =
        std::collections::HashMap::new();
    for acct in &spec.account_types {
        for variant in &acct.variants {
            let entry = variant_fields.entry(variant.name.as_str()).or_default();
            for (n, _) in &variant.fields {
                entry.insert(n.as_str());
            }
        }
    }

    for handler in &spec.handlers {
        for (field, _kind, _value) in &handler.effects {
            let base = effect_target_base(field);
            // Variant-prefixed: the root is a variant name, so check
            // the field beneath it against that variant's payload.
            if let Some(variant_payload) = variant_fields.get(base) {
                let after = field.trim_start_matches(base).trim_start_matches('.');
                let nested_base = effect_target_base(after);
                if !nested_base.is_empty()
                    && !variant_payload.contains(nested_base)
                    && !declared.contains(nested_base)
                {
                    anyhow::bail!(
                        "handler `{}` writes effect target `{}` but `{}` is not declared in variant `{}`'s payload — add it to the variant or rename the effect",
                        handler.name,
                        field,
                        nested_base,
                        base,
                    );
                }
                continue;
            }
            if !declared.contains(base) {
                // `state := .Variant { … }` desugars to per-field effects
                // at the adapter, but non-RecordLit / unit-variant shapes
                // can survive here as a single bare-`state` effect. Accept
                // `state` whenever the spec has a multi-variant ADT —
                // downstream either handles it (RecordLit) or bails to a
                // `todo!()`.
                if base == "state" && spec.account_types.iter().any(|a| !a.variants.is_empty()) {
                    continue;
                }
                anyhow::bail!(
                    "handler `{}` writes effect target `{}` but `{}` is not declared in any state, account, record, or sum-variant payload — add it to the state declaration or remove the effect",
                    handler.name,
                    field,
                    base,
                );
            }
        }
    }
    Ok(())
}
