//! Account-environment detection, value/state resolution, and effect-triple
//! lowering: the `(field, op_kind, value)` projection of MIR `Stmt`s and the
//! single-effect Rust emitters consumed by the transition bodies.

use super::*;

pub fn handler_needs_account_env(op: &ParsedHandler) -> bool {
    op.requires
        .iter()
        .any(|r| mentions_handler_account_pubkey(&r.rust_expr, &op.accounts))
        || op
            .effects
            .iter()
            .any(|e| is_account_pubkey_ref(e.value.trim(), &op.accounts))
        || op.effect_branches.as_ref().is_some_and(|branches| {
            branches.arms.iter().any(|arm| {
                arm.effects
                    .iter()
                    .any(|e| is_account_pubkey_ref(e.value.trim(), &op.accounts))
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

/// The typed tree of a MIR expression. Post-#151 every production effect
/// RHS carries a tree copied from the adapter-built `ParsedEffect.tree`;
/// a `None` here is a hand-built fixture that must be fixed, not worked
/// around.
pub fn mir_expr_tree(e: &crate::mir::Expr) -> &crate::mir::ExprTree {
    e.tree
        .as_ref()
        .expect("effect-RHS Expr.tree is always populated by the chumsky adapter (#151/#156)")
}

/// Rust scalar type of a flat state field, for annotating the checked-RHS
/// `Option` closure. `None` for indexed / dotted / non-scalar targets —
/// callers fall back to unannotated inference.
fn field_rust_scalar_ty(spec: &ParsedSpec, field: &str) -> Option<&'static str> {
    if field.contains('[') || field.contains('.') {
        return None;
    }
    let dsl_ty = spec
        .state_fields
        .iter()
        .chain(spec.account_types.iter().flat_map(|a| a.fields.iter()))
        .find(|(n, _)| n == field)
        .map(|(_, t)| t.as_str())?;
    match dsl_ty.trim() {
        "U8" => Some("u8"),
        "U16" => Some("u16"),
        "U32" => Some("u32"),
        "U64" => Some("u64"),
        "U128" => Some("u128"),
        "I64" => Some("i64"),
        "I128" => Some("i128"),
        _ => None,
    }
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

/// Borrow every declared state field (was `mutable_fields` — the "mutable"
/// naming was historical; Pubkey fields are first-class since they lower to
/// `[u8; 32]`, so nothing is filtered).
pub fn field_refs(fields: &[(String, String)]) -> Vec<&(String, String)> {
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
pub fn stmt_effect_triple(
    stmt: &crate::mir::Stmt,
) -> Option<(String, &'static str, &crate::mir::Expr)> {
    use crate::mir::Stmt;
    match stmt {
        Stmt::Assign { path, rhs } => Some((path.segments.join("."), "set", rhs)),
        Stmt::CheckedAdd { path, delta, .. } => {
            // The lowered per-site error name is Lean/scaffold surface;
            // the pure model signals overflow via `return false`.
            Some((path.segments.join("."), "add", delta))
        }
        Stmt::CheckedSub { path, delta, .. } => Some((path.segments.join("."), "sub", delta)),
        Stmt::SatAdd { path, delta } => Some((path.segments.join("."), "add_sat", delta)),
        Stmt::SatSub { path, delta } => Some((path.segments.join("."), "sub_sat", delta)),
        Stmt::WrapAdd { path, delta } => Some((path.segments.join("."), "add_wrap", delta)),
        Stmt::WrapSub { path, delta } => Some((path.segments.join("."), "sub_wrap", delta)),
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
pub fn block_effect_triples(
    body: &crate::mir::Block,
) -> Vec<(String, &'static str, &crate::mir::Expr)> {
    body.stmts.iter().filter_map(stmt_effect_triple).collect()
}

/// Like [`block_effect_triples`] but descends into `Stmt::Branch`
/// arms/default. For *may-this-effect-fire* analyses (overflow filters
/// and tests) where a conditionally-applied effect counts.
pub fn block_effect_triples_deep(
    body: &crate::mir::Block,
) -> Vec<(String, &'static str, &crate::mir::Expr)> {
    fn walk<'a>(
        stmts: &'a [crate::mir::Stmt],
        out: &mut Vec<(String, &'static str, &'a crate::mir::Expr)>,
    ) {
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
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &crate::mir::Expr,
    indent: &str,
) {
    emit_one_effect_inner(out, spec, wrapping, field, op_kind, value, indent, None);
}

#[allow(clippy::too_many_arguments)]
fn emit_one_effect_inner(
    out: &mut String,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &crate::mir::Expr,
    indent: &str,
    account_binder: Option<&str>,
) {
    use super::tree_render::{self, ArithMode, RustCx};

    // Harnesses run against a flat `State` (union-of-variant-fields view):
    // strip the variant prefix so `Variant.field := …` emits `s.field = …`;
    // the variant itself is tracked via `s.status`.
    let field_owned = strip_variant_prefix_for_flat_state(field, spec);
    let field = field_owned.as_str();
    // Tree-native RHS (#151 Slice 1): one render call replaces the
    // adapter's pre-rendered string + `resolve_value`'s binder surgery +
    // the account-pubkey substring rewrite. Fallibility is structural,
    // not a `contains('?')` scan.
    let (rust_value, fallible) = {
        let tree = mir_expr_tree(value);
        let cx = RustCx::native()
            .with_arith(ArithMode::Checked)
            .with_acct_env(account_binder.map(|b| b.trim_end_matches('.')));
        (
            tree_render::render_rust(tree, cx),
            tree_render::contains_fallible_arith(tree),
        )
    };
    // Checked-expression RHS (bare arithmetic lowered to `checked_*` + `?`):
    // give the `?` ops an `Option` context via an immediately-invoked
    // closure; `None` (over/underflow, div-by-zero, failed narrowing)
    // rejects the transition, matching the checked `+=`/`-=` doctrine
    // (issue #146). The return-type annotation pins inference for
    // `try_into()`-narrowed helpers; unresolvable field types fall back
    // to inference.
    let rust_value = if fallible {
        match field_rust_scalar_ty(spec, field) {
            Some(ty) => format!(
                "match (|| -> Option<{ty}> {{ Some({rust_value}) }})() {{ Some(__rhs) => __rhs, None => return false }}"
            ),
            None => format!(
                "match (|| Some({rust_value}))() {{ Some(__rhs) => __rhs, None => return false }}"
            ),
        }
    } else {
        rust_value
    };
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
                "{indent}// unknown effect: {field} {op_kind} {}\n",
                value.rust
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn emit_one_effect_with_account_env(
    out: &mut String,
    spec: &ParsedSpec,
    wrapping: bool,
    field: &str,
    op_kind: &str,
    value: &crate::mir::Expr,
    indent: &str,
    account_binder: &str,
) {
    emit_one_effect_inner(
        out,
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
        for eff in &handler.effects {
            let field = &eff.field;
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
