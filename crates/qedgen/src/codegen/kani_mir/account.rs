//! Per-account structural body (records / sums / Status / State / property &
//! invariant predicates / transition fns / ref_impls) plus the symbolic
//! account-environment structs, their per-harness binding, and the shared
//! transition-call-args helper.

use super::*;

/// Per-account structural body: records / unit-enum sums / Status /
/// State / property predicates / invariant predicates / transition fns /
/// ref_impls. Harnesses are emitted by the later sections.
pub(crate) fn emit_account_section_structural(
    out: &mut String,
    mir: &Mir,
    parsed: &ParsedSpec,
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    let (state_fields, lifecycle) = resolve_account_view(parsed);

    // Ghosts are spec-only verification-State fields: present in the Kani
    // State struct + symbolic init + transitions (BMC can read them,
    // `emit_transition_fn` can assign them), never in the on-chain program.
    let state_fields_with_ghosts: Vec<(String, String)> = state_fields
        .iter()
        .cloned()
        .chain(parsed.ghosts.iter().map(|g| (g.name.clone(), g.ty.clone())))
        .collect();
    let state_fields: &[(String, String)] = &state_fields_with_ghosts;

    let mutable = util::field_refs(state_fields);
    let has_lifecycle = lifecycle.len() >= 2;

    util::emit_record_structs(out, parsed, "Clone, Copy, kani::Arbitrary", |t| {
        map_type(t, parsed)
    })?;

    util::emit_unit_enum_sums(out, parsed, "Clone, Copy, PartialEq, Eq, kani::Arbitrary")?;

    util::emit_lifecycle_status_enum_from(
        out,
        lifecycle,
        "Clone, Copy, PartialEq, Eq, kani::Arbitrary",
    );

    util::emit_state_struct_with_lifecycle(
        out,
        &mutable,
        "Clone, Copy",
        |t| map_type(t, parsed),
        has_lifecycle,
    )?;
    // #326 — ADT specs get the `state_repr_valid` invariant alongside the
    // flat struct; harness preambles assume it and transitions preserve it.
    if let Some(acct) = util::kani_adt_view(parsed) {
        util::emit_state_repr_validity_fn(out, parsed, acct);
    }
    emit_kani_account_env_structs(out, mir, parsed);

    let handlers: Vec<&crate::check::ParsedHandler> = parsed.handlers.iter().collect();
    let properties: Vec<&crate::check::ParsedProperty> = parsed.properties.iter().collect();
    if !properties.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Property predicates (from qedspec `property` declarations)\n");
        out.push_str(
            "// ============================================================================\n\n",
        );
        // `emit_property_predicates_with` takes `&[ParsedProperty]`, not
        // `&[&_]` — rebuild an owned Vec.
        let owned: Vec<crate::check::ParsedProperty> =
            properties.iter().map(|p| (*p).clone()).collect();
        util::emit_property_predicates_with(out, &owned, |t| map_type(t, parsed));
    }

    // Invariant predicates — only those linked from a handler in this section.
    let linked_invs: Vec<&crate::check::ParsedInvariant> = parsed
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
        util::emit_invariant_predicates(out, &linked_invs);
    }

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
    for op in &handlers {
        util::emit_transition_fn_for_kani(out, mir, op, parsed, false, |t| map_type(t, parsed))?;
    }

    // Reference implementations — pure-expression fns callable from
    // ensures-preservation harnesses.
    if !parsed.ref_impls.is_empty() {
        out.push_str(
            "// ============================================================================\n",
        );
        out.push_str("// Reference implementations (from qedspec ref_impl declarations).\n");
        out.push_str(
            "// ============================================================================\n\n",
        );
        for r in &parsed.ref_impls {
            let params = r
                .params
                .iter()
                .map(|(n, t)| {
                    map_type(t, parsed)
                        .map(|rt| format!("{}: {}", n, rt))
                        .unwrap_or_else(|_| format!("{}: {}", n, t))
                })
                .collect::<Vec<_>>()
                .join(", ");
            let ret = map_type(&r.return_type, parsed).unwrap_or_else(|_| r.return_type.clone());
            out.push_str(&format!(
                "fn {}({}) -> {} {{\n    {}\n}}\n\n",
                r.name, params, ret, r.rust_body
            ));
        }
    }

    Ok(())
}

pub(crate) fn emit_kani_account_env_structs(out: &mut String, mir: &Mir, parsed: &ParsedSpec) {
    use crate::rust_codegen_util as util;

    let handlers: Vec<&crate::check::ParsedHandler> = parsed
        .handlers
        .iter()
        .filter(|op| util::handler_needs_account_env(op))
        .collect();
    if handlers.is_empty() {
        return;
    }

    out.push_str("#[derive(Clone, Copy)]\n");
    out.push_str("struct KaniAccount {\n");
    out.push_str("    pubkey: [u8; 32],\n");
    out.push_str("}\n\n");

    for op in handlers {
        out.push_str("#[derive(Clone, Copy)]\n");
        out.push_str(&format!(
            "struct {} {{\n",
            util::handler_account_env_struct_name(&op.name)
        ));
        for account in &op.accounts {
            out.push_str(&format!("    {}: KaniAccount,\n", account.name));
        }
        // #337 — flattened symbolic members for account state-field reads
        // (`admin_config.admin` → `admin_config_admin`): external state the
        // guard compares against, modeled as an arbitrary value.
        for (acct, field) in util::collect_account_state_field_reads(op) {
            let ty = account_state_field_rust_type(mir, parsed, op, &acct, &field);
            out.push_str(&format!("    {}_{}: {},\n", acct, field, ty));
        }
        out.push_str("}\n\n");
    }
}

/// Rust type for a handler-account state-field read (#337), resolved from
/// the account binding's declared type through the imported spec's
/// account-type declarations. Falls back to `[u8; 32]` — the guard shape
/// that motivates these reads is a pubkey comparison.
fn account_state_field_rust_type(
    mir: &Mir,
    parsed: &ParsedSpec,
    op: &crate::check::ParsedHandler,
    acct: &str,
    field: &str,
) -> String {
    use crate::codegen_shared::map_type;

    op.accounts
        .iter()
        .find(|a| a.name == acct)
        .and_then(|a| {
            let ns = a.imported_namespace.as_deref()?;
            let tname = a.account_type.as_deref()?;
            let import = mir.imports.get(ns)?;
            let at = import.account_types.iter().find(|t| t.name == tname)?;
            at.fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, t)| t.clone())
        })
        .and_then(|dsl_ty| map_type(&dsl_ty, parsed).ok())
        .unwrap_or_else(|| "[u8; 32]".to_string())
}

pub(crate) fn emit_kani_account_env_binding(
    out: &mut String,
    op: &crate::check::ParsedHandler,
    var_name: &str,
    indent: &str,
) {
    use crate::rust_codegen_util as util;

    if !util::handler_needs_account_env(op) {
        return;
    }

    out.push_str(&format!(
        "{indent}let {var_name} = {} {{\n",
        util::handler_account_env_struct_name(&op.name)
    ));
    for account in &op.accounts {
        out.push_str(&format!(
            "{indent}    {}: KaniAccount {{ pubkey: kani::any() }},\n",
            account.name
        ));
    }
    // #337 — symbolic external-state members, matching the struct emission.
    for (acct, field) in util::collect_account_state_field_reads(op) {
        out.push_str(&format!("{indent}    {}_{}: kani::any(),\n", acct, field));
    }
    out.push_str(&format!("{indent}}};\n"));
}

pub(crate) fn transition_call_args(
    op: &crate::check::ParsedHandler,
    account_var: Option<&str>,
) -> String {
    use crate::rust_codegen_util as util;

    let mut args = String::new();
    if util::handler_needs_account_env(op) {
        let account_var = account_var.expect("account var provided for account-env handler");
        args.push_str(&format!(", &{}", account_var));
    }
    for (n, _) in op.takes_params.iter().chain(op.abstract_binders.iter()) {
        args.push_str(&format!(", {}", n));
    }
    args
}
