use super::*;

/// Render the `#[derive(Accounts)] pub struct X<'info>? { fields }`
/// block for one handler. Used by `generate_lib` (Anchor target —
/// structs live at crate root so `#[program]` can find them) and by
/// `render_handler_scaffold` (Quasar target — struct + impl together
/// in `instructions/<name>.rs`).
pub(crate) fn render_handler_accounts_struct(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    is_multi: bool,
    default_state_name: &str,
    surface: &FrameworkSurface,
    target: Target,
) -> String {
    let pascal = to_pascal_case(&handler.name);
    let lifetime_params = surface.lifetime_params();
    let mut out = String::new();
    out.push_str("#[derive(Accounts)]\n");
    // Drop `<'info>` when no field references it (zero accounts and no
    // implicit signer) — rustc rejects an unused lifetime (E0392).
    let needs_lifetime = !handler.accounts.is_empty() || handler.who.is_some();
    let struct_lifetime: &str = if needs_lifetime { &lifetime_params } else { "" };
    out.push_str(&format!("pub struct {}{} {{\n", pascal, struct_lifetime));

    if !handler.accounts.is_empty() {
        // Canonical-fallback resolver: an ambiguous / readonly state
        // account must still type as `Account<'info, <StateStruct>>`,
        // not `AccountInfo<'info>` — otherwise downstream
        // `self.<acct>.<field>` reads fail.
        let state_acct = resolve_handler_state_account(handler, spec);
        for acct in &handler.accounts {
            let inferred_name = if is_multi {
                infer_state_name(acct, spec, default_state_name)
            } else {
                default_state_name.to_string()
            };
            // State-bearing if `find_state_account` picked it OR
            // `infer_state_name` matched it to a declared ADT (multi-PDA
            // handlers would otherwise drop the lifecycle target to
            // `UncheckedAccount`).
            let inferred_match = is_multi && inferred_name != default_state_name;
            let is_state =
                state_acct.map(|sa| sa.name == acct.name).unwrap_or(false) || inferred_match;
            let attr = quasar_account_attr(acct, handler, &inferred_name, target, spec, is_state);
            let field_type = render_account_field_type(acct, surface, is_state, &inferred_name);
            out.push_str(&format!("{}    pub {}: {},\n", attr, acct.name, field_type));
        }
    } else if handler.who.is_some() {
        let signer_ty = if surface.accounts_lifetime.is_empty() {
            "Signer".to_string()
        } else {
            format!("Signer<{}>", surface.accounts_lifetime)
        };
        out.push_str(&format!("    pub signer: {},\n", signer_ty));
    }

    out.push_str("}\n");
    out
}

pub(crate) fn render_handler_scaffold(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    is_multi: bool,
    default_state_name: &str,
    spec_src: &str,
    spec_attr: &str,
    target: Target,
) -> Result<String> {
    let surface = FrameworkSurface::for_target(target);
    let pascal = to_pascal_case(&handler.name);
    let bumps_name = format!("{}Bumps", pascal);
    let any_mut = handler.accounts.iter().any(|a| a.is_writable);
    // Drop `<'info>` from the impl + guard sigs when the Accounts struct
    // itself dropped it (must match render_handler_accounts_struct).
    let handler_needs_lifetime = !handler.accounts.is_empty() || handler.who.is_some();
    let lifetime_params: String = if handler_needs_lifetime {
        surface.lifetime_params()
    } else {
        String::new()
    };
    // Anchor's `#[derive(Accounts)]` struct lives at crate root so
    // `#[program]` can find it; Quasar keeps struct + impl together
    // here. The flag also flips the imports.
    let render_struct = matches!(target, Target::Quasar);

    let mut out = String::new();
    out.push_str("// User-owned. Regenerating the spec does NOT overwrite this file.\n");
    out.push_str("// Guard checks live in the sibling `crate::guards` module and ARE\n");
    out.push_str("// regenerated on every `qedgen codegen`. Drift between the spec\n");
    out.push_str("// handler block and the `spec_hash` below fires a compile_error!\n");
    out.push_str("// via the `#[qed(verified, ...)]` macro.\n\n");
    out.push_str(surface.prelude_import);
    // Only Quasar handler files need a per-handler SPL import (local
    // Accounts struct); Anchor's struct lives in lib.rs which already
    // imports SPL types.
    if matches!(target, Target::Quasar) {
        let has_token = handler
            .accounts
            .iter()
            .any(|a| a.account_type.as_deref() == Some("token") || a.name == "token_program");
        let has_mint = handler
            .accounts
            .iter()
            .any(|a| a.account_type.as_deref() == Some("mint"));
        let imports = surface.token_imports(has_token, has_mint);
        if !imports.is_empty() {
            out.push_str(&imports);
        }
    }
    // Quasar's local Accounts struct needs state types in scope; Anchor
    // only needs `use crate::state::*;` when a handler param's type
    // names a user-defined record / sum type (so the signature
    // resolves) — otherwise the import would sit unused until the agent
    // fills the body.
    let handler_param_uses_user_type = handler.takes_params.iter().any(|(_, ty)| {
        let bare = ty.trim();
        spec.records.iter().any(|r| r.name == bare)
            || spec.sum_types.iter().any(|s| s.name == bare)
            || spec.account_types.iter().any(|a| a.name == bare)
    });
    if render_struct || handler_param_uses_user_type {
        out.push_str("use crate::state::*;\n");
    }
    // Handler bodies may call `ref_impl` fns by bare name; import only
    // when the spec declares any (avoids unused-import warnings).
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    out.push_str("use crate::guards;\n");
    out.push_str("use qedgen_macros::qed;\n");
    // The error enum must be in scope for checked-arith effects
    // (`MathOverflow`) and for the variant-state `WrongState` gate
    // (multi-variant ADT on Anchor with a non-init pre — the same
    // condition under which `emit_cross_variant_promotion` emits the
    // `matches!` check).
    let uses_wrong_state_check = is_multi_variant_adt_state(spec)
        && matches!(target, Target::Anchor)
        && handler
            .pre_status
            .as_deref()
            .is_some_and(|p| !matches!(p, "Uninitialized" | "Empty"));
    let body_uses_error_enum = !spec.error_codes.is_empty()
        && (uses_wrong_state_check
            || handler
                .effects
                .iter()
                .any(|e| e.op == "add" || e.op == "sub"));
    if body_uses_error_enum {
        out.push_str("use crate::errors::*;\n");
    }
    // Variant-state lowering references `<Name>AccountInner` by name in
    // the body; import it explicitly (Anchor scaffolds skip `state::*`).
    // Anchor-only: no such type exists on Quasar's flat path.
    if is_multi_variant_adt_state(spec) && matches!(target, Target::Anchor) {
        let inner_name = format!("{}AccountInner", to_pascal_case(&spec.program_name));
        out.push_str(&format!("use crate::state::{};\n", inner_name));
    }
    if !render_struct {
        // Anchor: bring the Accounts struct (defined in lib.rs) into
        // scope so the impl block can reference it bare.
        if surface.needs_bumps_import(handler) {
            out.push_str(&format!("use crate::{{{}, {}}};\n", pascal, bumps_name));
        } else {
            out.push_str(&format!("use crate::{};\n", pascal));
        }
    }
    out.push('\n');

    if render_struct {
        out.push_str(&render_handler_accounts_struct(
            handler,
            spec,
            is_multi,
            default_state_name,
            &surface,
            target,
        ));
        out.push('\n');
    }

    // impl block with handler — lifetime threaded for Anchor.
    out.push_str(&format!(
        "impl{} {}{} {{\n",
        lifetime_params, pascal, lifetime_params
    ));
    if let Some(ref doc) = handler.doc {
        out.push_str(&format!("    /// {}\n", doc));
    }

    // Emit the spec-bound #[qed(...)] attribute with a body-hash
    // sentinel; the fixup pass at the bottom splices in the real hash
    // (both sides normalize via `proc_macro2::TokenStream::from_str`,
    // so codegen-time and compile-time agree). Match-arm-derived
    // handlers (`x_case_0`, `x_otherwise`) don't appear in the source by
    // their split name — attribute + spec_hash reference the parent so
    // every arm shares one drift-tracking key.
    let parent_name: &str = if let Some(stripped) = handler.name.strip_suffix("_otherwise") {
        stripped.strip_suffix('_').unwrap_or(stripped)
    } else if let Some(idx) = handler.name.rfind("_case_") {
        &handler.name[..idx]
    } else {
        handler.name.as_str()
    };
    let parent_exists = spec_hash::spec_hash_for_handler(spec_src, parent_name).is_some();
    let attr_handler_name = if parent_exists {
        parent_name
    } else {
        handler.name.as_str()
    };
    let spec_h = spec_hash::spec_hash_for_handler(spec_src, attr_handler_name).unwrap_or_default();
    out.push_str(&format!(
        "    #[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\")]\n",
        spec_attr, attr_handler_name, BODY_HASH_PLACEHOLDER, spec_h
    ));

    out.push_str("    #[inline(always)]\n");

    let self_ref = if any_mut { "&mut self" } else { "&self" };
    let mut handler_params = vec![self_ref.to_string()];
    let mut param_names: Vec<String> = Vec::new();
    for (pname, ptype) in &handler.takes_params {
        handler_params.push(format!(
            "{}: {}",
            pname,
            map_type_for_target(ptype, spec, target)?
        ));
        param_names.push(pname.clone());
    }
    if handler.has_bumps() {
        handler_params.push(format!("bumps: &{}", bumps_name));
    }

    out.push_str(&format!(
        "    pub fn handler({}) -> {} {{\n",
        handler_params.join(", "),
        surface.handler_result_type
    ));

    // Call the always-regenerated guards module. Signature: takes `&Self`
    // plus every handler-level parameter, returns `Result<(), ProgramError>`.
    let guard_args = if param_names.is_empty() {
        "self".to_string()
    } else {
        format!("self, {}", param_names.join(", "))
    };
    out.push_str(&format!(
        "        guards::{}({})?;\n",
        handler.name, guard_args
    ));
    if handler.has_bumps() {
        out.push_str("        let _ = bumps;\n");
    }

    // `abstract <name> : <Type>` clauses become user-fillable `todo!()`
    // bindings whose prompt lists the active `requires` constraints.
    // Emitted BEFORE let_bindings so spec-level `let`s can reference
    // them.
    for (binder_name, binder_ty_str) in &handler.abstract_binders {
        let ty = map_type_for_target(binder_ty_str, spec, target)?;
        let requires_summary: Vec<String> = handler
            .requires
            .iter()
            .map(|r| r.rust_expr.clone())
            .collect();
        let constraint_hint = if requires_summary.is_empty() {
            String::new()
        } else {
            format!(
                " Constraints from `requires`: {}.",
                requires_summary.join(" && ")
            )
        };
        out.push_str(&format!(
            "        let {}: {} = todo!(\"v2.29 abstract binder `{}` — fill with the concrete library / math value.{}\");\n",
            binder_name, ty, binder_name, constraint_hint
        ));
    }

    // Spec-level `let` bindings emit BEFORE the effect block (effect
    // RHSs reference them). The RHS carries the spec's `s.<field>`
    // shorthand, unbound here — rewrite through the same accessor logic
    // the CPI-arg path uses.
    for b in &handler.let_bindings {
        let rewritten = render_let_binding_rust(
            b,
            resolve_handler_state_account(handler, spec).map(|sa| format!("self.{}", sa.name)),
            /*pod_target=*/ false,
            /*acct_key=*/ None,
            spec,
        );
        out.push_str(&format!("        let {} = {};\n", b.name, rewritten));
    }

    // `let X = call …` bindings must be in scope for subsequent effects
    // / requires, so bound calls emit BEFORE the effect block; unbound
    // calls fire at the tail. Track emitted indices so the tail skips
    // them.
    let mut emitted_call_indices = std::collections::HashSet::new();
    let mut any_unmechanized_call_pre = false;
    for (idx, c) in handler.calls.iter().enumerate() {
        if c.result_binding.is_none() {
            continue;
        }
        match plan_cpi(c, handler, spec, target) {
            CpiPlan::Complete(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{} (binding: {})\n",
                    c.target_interface,
                    c.target_handler,
                    c.result_binding.as_deref().unwrap_or("_")
                ));
                out.push_str(&rendered);
                emitted_call_indices.insert(idx);
            }
            CpiPlan::AgentFill(reason) => {
                let args = c
                    .args
                    .iter()
                    .map(|a| format!("{}={}", a.name, a.rust_expr))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "        // Spec call: {}.{}({}) (binding: {}) — agent fill: {}\n",
                    c.target_interface,
                    c.target_handler,
                    args,
                    c.result_binding.as_deref().unwrap_or("_"),
                    reason.render(),
                ));
                any_unmechanized_call_pre = true;
                emitted_call_indices.insert(idx);
            }
        }
    }

    // Mechanical-effect expansion: emit a real Rust statement per spec
    // effect; anything non-mechanical stays as a comment and forces a
    // trailing `todo!()`. Multi-variant ADT specs try the variant-aware
    // emitter first (effects must run inside the `match … inner` block);
    // on `None`, fall through to the per-effect path.
    let state_acct = find_state_account(handler);
    let mut any_unmechanized = false;
    let variant_body =
        state_acct.and_then(|sa| emit_variant_state_handler_body(handler, spec, target, sa));
    let variant_body_emitted = variant_body.is_some();
    if let Some(VariantHandlerBody {
        body,
        needs_fill_tail,
    }) = variant_body
    {
        out.push_str(&body);
        if needs_fill_tail {
            any_unmechanized = true;
        }
    } else {
        // Parallel effect semantics: snapshot fields the block both
        // writes and reads so RHS reads observe pre-state — the meaning
        // the Lean model and Kani conformance assertions give the spec.
        // Two-phase so snapshots bind before the first effect line, and
        // only referenced snapshots are emitted.
        let pre_fields = parallel_pre_fields_for_handler(handler, spec);
        let mechanized_lines: Vec<Option<String>> = handler
            .effects
            .iter()
            .map(|effect| {
                state_acct
                    .and_then(|sa| mechanize_effect(effect, sa, spec, target, handler, &pre_fields))
            })
            .collect();
        if let Some(sa) = state_acct {
            for f in &pre_fields {
                let used = mechanized_lines
                    .iter()
                    .flatten()
                    .any(|l| l.contains(&format!("pre_{f}")));
                if used {
                    out.push_str(&format!("        let pre_{f} = self.{}.{f};\n", sa.name));
                }
            }
        }
        for (effect, mechanized) in handler.effects.iter().zip(&mechanized_lines) {
            match mechanized {
                Some(line) => out.push_str(line),
                None => {
                    out.push_str(&format!(
                        "        // Spec effect (needs fill): {} {} {}\n",
                        effect.field, effect.op, effect.value
                    ));
                    any_unmechanized = true;
                }
            }
        }
    }

    // `modifies [X, Y]` declared but unwritten in `effect { … }`: emit a
    // structured agent-fill site per field — spec declares write set +
    // ensures, agent fills the math, harnesses check the contract.
    // Flat-fields path only; multi-variant ADT specs route through
    // `emit_variant_state_handler_body`.
    if !variant_body_emitted && !is_multi_variant_adt_state(spec) {
        if let (Some(modifies), Some(sa)) = (handler.modifies.as_ref(), state_acct) {
            let mut effect_fields: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for eff in &handler.effects {
                let stripped = strip_variant_prefix(&eff.field, spec);
                let bare = strip_array_index_suffix(&stripped);
                effect_fields.insert(bare);
            }
            let acct_name = &sa.name;
            for field in modifies {
                if effect_fields.contains(field) {
                    continue;
                }
                // Textual match: `rust_expr` carries `post.<field>` /
                // `pre.<field>` for state refs.
                let mut referencing: Vec<&str> = Vec::new();
                for e in &handler.ensures {
                    if e.rust_expr.contains(field) {
                        referencing.push(e.rust_expr.as_str());
                    }
                }
                out.push_str(&format!(
                    "        // QED agent-fill site: `{}` is in `modifies` but not in `effect`.\n",
                    field
                ));
                if referencing.is_empty() {
                    out.push_str(&format!(
                        "        //   No `ensures` clause references `{}` — the field is\n",
                        field
                    ));
                    out.push_str(
                        "        //   unconstrained. Either add an `ensures` constraint or\n",
                    );
                    out.push_str(&format!(
                        "        //   remove `{}` from `modifies`. (Lint: unconstrained_modifies)\n",
                        field
                    ));
                } else {
                    out.push_str("        //   Implement against the spec's ensures:\n");
                    for r in &referencing {
                        out.push_str(&format!("        //     ensures {}\n", r));
                    }
                    out.push_str(
                        "        //   The Kani / proptest harness verifies the impl satisfies\n",
                    );
                    out.push_str(
                        "        //   these clauses against the pre-state captured before the call.\n",
                    );
                }
                out.push_str(&format!(
                    "        self.{}.{} = todo!(\"compute {} to satisfy ensures above\");\n",
                    acct_name, field, field
                ));
                any_unmechanized = true;
            }
        }
    }

    // Events are agent-fill: the spec declares the event name but not
    // the payload binding.
    for emit in &handler.emits {
        out.push_str(&format!("        // Spec: emit!({})\n", emit));
    }
    let has_events = !handler.emits.is_empty();

    // Token transfers are agent-fill: building the CPI context involves
    // framework-specific helpers that differ per target.
    let has_transfers = !handler.transfers.is_empty();
    for t in &handler.transfers {
        out.push_str(&format!(
            "        // Spec transfer: {} -> {} amount={}\n",
            t.from,
            t.to,
            t.amount.as_deref().unwrap_or("?")
        ));
    }

    // `call Interface.handler(...)` sites: mechanized via try_emit_cpi
    // where possible; unmechanized cases emit a structured comment and
    // set the flag so the tail `todo!()` fires.
    let mut any_unmechanized_call = false;
    for (idx, c) in handler.calls.iter().enumerate() {
        // Bound calls already emitted before the effect block; skip to
        // avoid double-emitting.
        if emitted_call_indices.contains(&idx) {
            continue;
        }
        match plan_cpi(c, handler, spec, target) {
            CpiPlan::Complete(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{} (complete CPI emitted)\n",
                    c.target_interface, c.target_handler
                ));
                out.push_str(&rendered);
            }
            CpiPlan::AgentFill(reason) => {
                let args = c
                    .args
                    .iter()
                    .map(|a| format!("{}={}", a.name, a.rust_expr))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "        // Spec call: {}.{}({}) — agent fill: {}\n",
                    c.target_interface,
                    c.target_handler,
                    args,
                    reason.render(),
                ));
                any_unmechanized_call = true;
            }
        }
    }

    let needs_fill = any_unmechanized
        || has_events
        || has_transfers
        || any_unmechanized_call
        || any_unmechanized_call_pre;
    if needs_fill {
        out.push_str("        todo!(\"fill non-mechanical effects, events, transfers, calls\")\n");
    } else {
        out.push_str("        Ok(())\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n");

    // Format BEFORE the hash fixup: rustfmt is not token-neutral (it adds
    // trailing commas), and the body hash is over the canonical token
    // stream — hashing unformatted text would leave a stamp that the
    // proc macro (reading the formatted file) can never match.
    out = format_rust_source(&out);

    // Fixup: compute the impl method's body hash and splice it into the
    // placeholder. Both sides normalize via
    // `proc_macro2::TokenStream::from_str`, so codegen-time and
    // compile-time agree and the first `cargo build` is clean.
    if let Some(body_hash) = precompute_body_hash(&out) {
        out = out.replace(BODY_HASH_PLACEHOLDER, &body_hash);
    }
    Ok(out)
}

/// Re-parse a rendered handler scaffold (with `BODY_HASH_PLACEHOLDER`
/// still in the `#[qed]` attribute), find the impl method named
/// `handler`, and compute its body hash. MUST mirror
/// `qedgen-macros::FnLike::from_tokens`'s parse order (try `ItemFn`
/// first, fall back to `ImplItemFn`) so we hit the same arm — both
/// produce the same canonical bytes after the `from_str`
/// normalization in `body_hash_for_*`, but only when fed equivalent
/// inputs.
pub(crate) fn precompute_body_hash(scaffold_source: &str) -> Option<String> {
    use quote::ToTokens;
    let file: syn::File = syn::parse_str(scaffold_source).ok()?;
    for item in &file.items {
        if let syn::Item::Impl(item_impl) = item {
            for impl_item in &item_impl.items {
                if let syn::ImplItem::Fn(impl_fn) = impl_item {
                    if impl_fn.sig.ident == "handler" {
                        let tokens = impl_fn.to_token_stream();
                        if let Ok(item_fn) = syn::parse2::<syn::ItemFn>(tokens.clone()) {
                            return Some(spec_hash::body_hash_for_fn(&item_fn));
                        }
                        if let Ok(impl_fn2) = syn::parse2::<syn::ImplItemFn>(tokens) {
                            return Some(spec_hash::body_hash_for_impl_fn(&impl_fn2));
                        }
                    }
                }
            }
        }
    }
    None
}
