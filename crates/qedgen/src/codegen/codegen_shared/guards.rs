use super::*;
use crate::mir::Mir;

/// True if any rendered Rust expression in the spec references one of the
/// fixed-point helpers in `src/math.rs`. Used to gate the `use crate::math::*;`
/// import in `guards.rs` so legacy programs whose user-owned `lib.rs` doesn't
/// declare `pub mod math;` keep compiling.
pub(crate) fn guards_use_math_helpers(spec: &ParsedSpec) -> bool {
    let mut any = false;
    let probe = |s: &str| {
        s.contains("mul_div_floor_u128")
            || s.contains("mul_div_ceil_u128")
            || s.contains("mul_div_round_half_up_u128")
    };
    for h in &spec.handlers {
        if h.requires.iter().any(|r| probe(&r.rust_expr)) {
            any = true;
        }
        if h.ensures.iter().any(|e| probe(&e.rust_expr)) {
            any = true;
        }
        // Handler-level `let bindings: (lean_expr, rust_expr)` also lower to
        // `let X = mul_div_floor_u128(...)` in the emitted Rust handler body.
        // Without this, specs that compute fee math via a `let` (a common
        // pattern for splitting amounts before the effect block) wouldn't
        // pick up the math.rs import / inline helpers.
        if h.let_bindings.iter().any(|(_, _, r)| probe(r)) {
            any = true;
        }
        // Effect RHS can call the helpers directly (`fee := mul_div_floor(…)`)
        // — probe the Rust-form values the harness transition bodies emit.
        if h.effects.iter().any(|e| probe(&e.value_rust)) {
            any = true;
        }
        if let Some(br) = &h.effect_branches {
            if br
                .arms
                .iter()
                .any(|arm| arm.effects.iter().any(|e| probe(&e.value_rust)))
            {
                any = true;
            }
        }
    }
    for prop in &spec.properties {
        if let Some(ref r) = prop.rust_expression {
            if probe(r) {
                any = true;
            }
        }
    }
    // `ref_impl` bodies lower to standalone fns that call the helpers
    // (`fn bps_mul(…) { mul_div_floor_u128(…) }`) — without this probe the
    // helper definition is never emitted alongside them (issue #145).
    for r in &spec.ref_impls {
        if probe(&r.rust_body) {
            any = true;
        }
    }
    any
}

/// Generate src/guards.rs — one function per handler containing all the
/// spec-declared guard checks. This file is always regenerated; any edit
/// is clobbered on the next `qedgen codegen` (by design).
pub(crate) fn generate_guards(
    mir: &Mir,
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let surface = FrameworkSurface::for_target(target);
    let lifetime_params = surface.lifetime_params();
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Pinocchio: dedicated guard emission (raw &AccountInfo ctx + zeropod
    // state decode), shared with the codegen_mir delegation. (slice 6 4b)
    if matches!(target, Target::Pinocchio) {
        return emit_pinocchio_guards(spec, fp, output_dir);
    }

    let mut out = String::new();
    out.push_str(&marker(
        "DO NOT EDIT — regenerated from .qedspec",
        fp,
        "src/guards.rs",
    ));
    out.push_str("//! Per-handler guard checks derived from the `.qedspec`.\n");
    out.push_str("//! Called from user-owned `instructions/<name>::handler` before\n");
    out.push_str("//! business logic; keep guard logic here, policy-free logic there.\n\n");
    out.push_str(
        "#![allow(unused_variables, unused_imports, dead_code, clippy::too_many_arguments)]\n\n",
    );
    out.push_str(surface.prelude_import);
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    // R26: `<ADT>Status` / `Status` enums live in `crate::state`. Pull
    // them in unconditionally — guards.rs always emits the enum-typed
    // pre-check / post-write when lifecycle is present, and a
    // never-used import is harmless under `#![allow(unused_imports)]`.
    out.push_str("use crate::state::*;\n");
    // `crate::math` carries `mul_div_floor_u128` / `mul_div_ceil_u128`.
    // Only import when a spec expression actually uses them, otherwise
    // existing `pub mod math;`-less lib.rs (user-owned, skip-if-exists)
    // would fail to resolve the path.
    if guards_use_math_helpers(spec) {
        out.push_str("use crate::math::*;\n");
    }
    // v2.26 Slice 3c — ref_impls are callable from `requires` bodies.
    // The spec's `expr_to_rust` lowering emits the call by name; the
    // ref_impl fn lives in `crate::ref_impls`, so import it
    // unconditionally whenever the spec declares any. Under
    // `#![allow(unused_imports)]` a never-used import is harmless.
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    // Pick up the per-handler `Accounts` structs. Anchor places them
    // at crate root (lib.rs); Quasar places them in
    // `instructions/<name>.rs` and re-exports via `instructions::*`.
    out.push_str(surface.guard_accounts_import());

    // Hoisted once: the guard error-enum name (`<Program>Error`) is
    // spec-constant; R28 / R27 / requires / aborts all format against it
    // (previously recomputed 4x per handler).
    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));

    // Walk MIR handlers in lockstep with their ParsedSpec source (1:1, same
    // order — `mir.handlers = parsed.handlers.map(lower_handler)`). `hm` is the
    // migration target; reads move from `handler`/`spec` to `hm`/`mir` slice by
    // slice, gated byte-identical by `codegen_snapshot`.
    for (hm, handler) in mir.handlers.iter().zip(&spec.handlers) {
        let pascal = to_pascal_case(&handler.name);
        let any_mut = hm.accounts.iter().any(|a| a.writable);
        let self_ref = if any_mut { "&mut " } else { "&" };
        // v2.29 — match the handler-scaffold + Accounts-struct
        // lifetime decision so the guard fn's ctx ref doesn't
        // reference an unused `<'info>` on a unit Accounts struct.
        let handler_needs_lifetime = !hm.accounts.is_empty() || hm.auth.is_some();
        let lp: &str = if handler_needs_lifetime {
            &lifetime_params
        } else {
            ""
        };
        let mut params = vec![format!("ctx: {}{}{}", self_ref, pascal, lp)];
        for (pname, ptype) in &handler.takes_params {
            params.push(format!(
                "{}: {}",
                pname,
                map_type_for_target(ptype, spec, target)?
            ));
        }
        out.push_str(&format!(
            "/// Guards for `{}`.  \n/// Generated from the `requires` clauses of the spec handler block.\n",
            handler.name
        ));
        out.push_str(&format!(
            "pub fn {}{}({}) -> {} {{\n",
            handler.name,
            lp,
            params.join(", "),
            surface.handler_result_type
        ));

        // R26: lifecycle pre-status check. The spec's `: State.Pre ->
        // State.Post` expresses a state-machine transition; without a
        // runtime guard, every handler is reachable in every state
        // (which is how the multisig::propose proposal-erasure CRIT
        // surfaced — calling `propose` again from `HasProposal` zeroes
        // approval/rejection counts). The pre-check uses the `status:
        // u8` field added by `generate_state` and the `<ADT>Status`
        // enum's discriminator. We elide the check on init handlers
        // (Quasar's `init` zeroes the account, so `status == 0` is the
        // default; we just write the post variant). We also elide when
        // the spec doesn't declare lifecycle states for the relevant
        // ADT.
        let lifecycle_pre_check = lifecycle_check_line(handler, spec, false, &surface);
        let lifecycle_post_write = lifecycle_check_line(handler, spec, true, &surface);
        if !lifecycle_pre_check.is_empty() {
            out.push_str(&lifecycle_pre_check);
        }

        // v2.24 S5c — auth guard for fields that live in a variant
        // payload. R25's `auth X → has_one = X` suppresses the
        // Anchor `has_one` attribute under multi-variant ADT
        // because the macro can't reach `wrapper.inner.<variant>.X`
        // (see `is_multi_variant_adt_with_field_in_variant` in
        // account_attr.rs). Replace it with an explicit destructure-then-
        // compare guard so the auth check still fires at runtime.
        // Requires:
        //   - multi-variant ADT spec
        //   - handler declares `auth X` where X is a variant-payload
        //     field on the pre-variant
        //   - handler binds a signer account named `X`
        //   - the spec declares `Unauthorized` in `type Error`
        // Missing any condition: silently skip — the auth gap shows
        // up as a `qedgen check` warning (`no_access_control` / R25
        // friend) rather than a compile error.
        let auth_guard = emit_variant_auth_guard(handler, spec, target);
        if !auth_guard.is_empty() {
            out.push_str(&auth_guard);
        }

        emit_r28_pda_checks(&mut out, handler, spec, target, &err_enum);

        emit_r27_authority_checks(&mut out, handler, spec, &surface, &err_enum);

        if handler.requires.is_empty()
            && lifecycle_pre_check.is_empty()
            && lifecycle_post_write.is_empty()
        {
            out.push_str("    // No guards declared in spec — nothing to check.\n");
        }

        // `rust_expr` references state fields as `s.<field>` (lowered from
        // `state.<field>` in the spec). Inside guards.rs the state-bearing
        // account is reached via `ctx.<state_account>.<field>` (Anchor's
        // `Account<T>` and Quasar's typed account both auto-deref to T).
        // When we can identify a single state account, rewrite `s.` to that
        // path so the guards compile. Multi-state handlers fall through with
        // the raw `s.` form — caller must hand-edit. R12 fix.
        //
        // v2.29.2 — use the canonical-fallback resolver so multi-
        // writable handlers whose state account is `readonly` still
        // get `s.<field>` rewritten to `ctx.<canonical>.<field>`
        // instead of left unbound.
        let state_acct = resolve_handler_state_account(handler, spec);

        // Pick the Pod-aware rust expression on Quasar so Pod field
        // accesses carry `.get()` and mixed-kind binops add `as i128`
        // casts — without it `state.foo.x + state.foo.y` fails when
        // `x: PodU128` and `y: PodI128`.
        let pod_target = matches!(target, Target::Quasar);

        // v2.29.2 — emit spec-level `let X = ref_impl(...)` bindings
        // here so `requires X > 0` clauses can reference them. Without
        // this, guards.rs emitted the requires check against a name
        // that's only bound later in the handler body (`let lp_out =
        // lp_token_out(...)` lives in the user-owned handler stub),
        // tripping `cannot find value 'lp_out' in this scope`. Each
        // RHS goes through `bind_state` so `s.<field>` reads route
        // through `ctx.<state>.<field>` (the guards binder).
        for (binding_name, _lean_expr, rust_expr) in &handler.let_bindings {
            let rewritten = bind_state_expr(rust_expr, handler, state_acct, spec, &surface);
            out.push_str(&format!(
                "    // let-binding from spec: {} = {}\n",
                binding_name, rust_expr
            ));
            out.push_str(&format!("    let {} = {};\n", binding_name, rewritten));
        }

        emit_requires_guards(
            &mut out, handler, hm, spec, &surface, target, state_acct, pod_target, &err_enum,
        );

        // R26: lifecycle post-status write — runs after all guards have
        // passed so a failed guard doesn't half-transition. Only emitted
        // when the post variant differs from the pre variant.
        if !lifecycle_post_write.is_empty() {
            out.push_str(&lifecycle_post_write);
        }

        out.push_str("    Ok(())\n");
        out.push_str("}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");
    write_generated_file(&src_dir.join("guards.rs"), &out)?;
    Ok(())
}

/// R28: per-handler PDA verification. R13 suppresses
/// `seeds = [...]` on Quasar non-init handlers when seeds
/// reference state fields (the macro's `Bumps::seeds()` method
/// can't auto-capture `self.<state-field>`). Owner+discriminator
/// protects against type confusion but not wrong-PDA passing —
/// the audit's MED-tier finding. Emit a runtime
/// `verify_program_address` check using the stored bump for
/// every account whose `seeds = [...]` would have been
/// suppressed. The cost is one syscall (~544 CU on first-try
/// bump 255) per affected handler load.
fn emit_r28_pda_checks(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
    err_enum: &str,
) {
    for acct in &handler.accounts {
        let Some(ref seeds) = acct.pda_seeds else {
            continue;
        };
        let is_init_target = handler_is_init_for(handler, &acct.name) && !acct.is_signer;
        if is_init_target {
            continue; // init flow already verifies via #[account(seeds=…, bump)]
        }
        // Was R13 going to suppress on this handler? Mirror the
        // detection logic from `quasar_account_attr`. Quasar fires on
        // any state-field seed (its R13 suppression is broader);
        // Anchor (v2.29 extension) only when the seed references a
        // variant-payload field on a multi-variant ADT — the
        // `seeds = [...]` macro can't route through the accessor, so
        // `quasar_account_attr` suppressed it and the runtime check
        // emits here instead.
        let bound_account_names: std::collections::HashSet<&str> =
            handler.accounts.iter().map(|a| a.name.as_str()).collect();
        if !r28_pda_check_fires(target, spec, seeds, &bound_account_names) {
            continue;
        }

        let mut seed_exprs: Vec<String> = Vec::with_capacity(seeds.len() + 1);
        for seed in seeds {
            if let Some(inner) = seed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                seed_exprs.push(format!("b\"{}\"", inner));
            } else if bound_account_names.contains(seed.as_str()) {
                // Handler-bound account: read its address.
                match target {
                    Target::Anchor => seed_exprs.push(format!("ctx.{}.key().as_ref()", seed)),
                    _ => seed_exprs
                        .push(format!("ctx.{}.to_account_view().address().as_ref()", seed)),
                }
            } else {
                // State-field seed: read off the same PDA's stored
                // value. For multi-variant ADTs on Anchor, route
                // through the v2.29 Slice B accessor; for Quasar
                // / flat-state, read the field directly.
                let is_variant_field = matches!(target, Target::Anchor)
                    && spec.account_types.iter().any(|a| {
                        a.variants
                            .iter()
                            .any(|v| v.fields.iter().any(|(n, _)| n == seed))
                    });
                if is_variant_field {
                    seed_exprs.push(format!("ctx.{}.inner.{}().as_ref()", acct.name, seed));
                } else {
                    seed_exprs.push(format!("ctx.{}.{}.as_ref()", acct.name, seed));
                }
            }
        }

        match target {
            Target::Anchor => {
                // Anchor PDA verification uses
                // `anchor_lang::solana_program::pubkey::Pubkey::
                // create_program_address` with the stored bump
                // to avoid the find_program_address syscall cost.
                seed_exprs.push(format!("&[ctx.{}.bump]", acct.name));
                out.push_str(&format!(
                    "    // R28 PDA check: ctx.{acct} matches its declared seeds (Anchor)\n    {{\n        let __seeds: &[&[u8]] = &[{seeds}];\n        let __expected = anchor_lang::solana_program::pubkey::Pubkey::create_program_address(__seeds, &crate::ID).map_err(|_| {err_enum}::InvalidPda)?;\n        if ctx.{acct}.key() != __expected {{\n            return Err({err_enum}::InvalidPda.into());\n        }}\n    }}\n",
                    acct = acct.name,
                    seeds = seed_exprs.join(", "),
                ));
            }
            _ => {
                seed_exprs.push(format!("&[ctx.{}.bump]", acct.name));
                out.push_str(&format!(
                    "    // R28 PDA check: ctx.{acct} matches its declared seeds\n    {{\n        let __seeds: &[&[u8]] = &[{seeds}];\n        if quasar_lang::pda::verify_program_address(__seeds, &crate::ID, ctx.{acct}.to_account_view().address()).is_err() {{\n            return Err(ProgramError::from({err_enum}::InvalidPda));\n        }}\n    }}\n",
                    acct = acct.name,
                    seeds = seed_exprs.join(", "),
                ));
            }
        }
    }
}

/// R27: token-vault authority binding. The spec declares
/// `pool_vault : token, authority pool` — meaning the SPL token
/// account's `owner` field (i.e. the entity that can sign
/// transfers from it) must equal the `pool` PDA's address. R6
/// dropped Quasar's `token::authority = X` constraint on
/// non-init accounts (the macro rejects it without `init`), so
/// the static check is gone for every load after init. Without
/// a runtime equivalent the pool_vault parameter could be any
/// SPL-Token-program-owned account, breaking the deposit/repay/
/// liquidate transfer routing intent (audit HIGH 5).
///
/// Emit a runtime owner check on every non-init token account
/// that declares `authority X` — the token account's `owner()`
/// accessor returns the authority address, compared against the
/// bound account's address.
fn emit_r27_authority_checks(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    surface: &FrameworkSurface,
    err_enum: &str,
) {
    for acct in &handler.accounts {
        let is_init_target =
            handler_is_init_for(handler, &acct.name) && acct.pda_seeds.is_some() && !acct.is_signer;
        let is_token = acct.account_type.as_deref() == Some("token");
        if !is_token || is_init_target {
            continue;
        }
        let Some(ref auth_name) = acct.authority else {
            continue;
        };
        let unauthorized = if spec.error_codes.iter().any(|c| c == "Unauthorized") {
            "Unauthorized"
        } else {
            "InvalidLifecycle"
        };
        let err_expr = surface.error_expr(err_enum, unauthorized);
        let check_expr = surface.authority_check_expr(&acct.name, auth_name);
        out.push_str(&format!(
            "    // authority: {}\n    if {} {{ return Err({}); }}\n",
            check_expr, check_expr, err_expr,
        ));
    }
}

/// Rewrite a spec-rendered guard expression for the guards.rs body: bare
/// handler-account idents (and `<acct>.pubkey`) become runtime address
/// loads, then the state binder `s.` is rebound to `ctx.<state_acct>.`
/// (flat state) or the ADT accessor (`(*ctx.<state>.inner.<field>())`).
/// Free-function sibling of [`bind_pinocchio_expr`]; formerly a 180-line
/// closure inside `generate_guards`.
fn bind_state_expr(
    expr: &str,
    handler: &ParsedHandler,
    state_acct: Option<&crate::check::ParsedHandlerAccount>,
    spec: &ParsedSpec,
    surface: &FrameworkSurface,
) -> String {
    // Bare handler-account idents in spec expressions (e.g. the
    // `approver` in `state.members[i] == approver`) need to be
    // lowered to the runtime pubkey load `*ctx.<name>.to_account_view().address()`.
    // Without this, the spec's signer-binding compiles to `... ==
    // approver` where `approver` resolves to nothing in scope.
    let handler_account_names: Vec<String> =
        handler.accounts.iter().map(|a| a.name.clone()).collect();
    // v2.29 Slice G.4 — index handler accounts by name so the
    // `<acct>.<field>` rewriting can dispatch on
    // `imported_namespace` and route through the local mirror.
    let handler_accounts_by_name: std::collections::HashMap<
        &str,
        &crate::check::ParsedHandlerAccount,
    > = handler
        .accounts
        .iter()
        .map(|a| (a.name.as_str(), a))
        .collect();
    // Step 1: rewrite handler-account idents to address loads.
    let mut after_accounts = String::with_capacity(expr.len() + 32);
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        let mut matched = false;
        if prev_ok {
            for name in &handler_account_names {
                let nbytes = name.as_bytes();
                if i + nbytes.len() <= bytes.len() && &bytes[i..i + nbytes.len()] == nbytes {
                    // Boundary check on the trailing edge: don't
                    // match `approver_x` when looking for `approver`.
                    let after = i + nbytes.len();
                    if after >= bytes.len() || !is_ident_char(bytes[after]) {
                        // `<acct>.pubkey` is the spec-author's
                        // way of saying "this account's address"
                        // — lower to the same address-load form
                        // we use for bare `<acct>` so a
                        // `requires acct.pubkey == state.field`
                        // clause compiles.
                        let pubkey_marker = b".pubkey";
                        let after_dot_end = after + pubkey_marker.len();
                        if after_dot_end <= bytes.len()
                            && &bytes[after..after_dot_end] == pubkey_marker
                            && (after_dot_end == bytes.len()
                                || !is_ident_char(bytes[after_dot_end]))
                        {
                            after_accounts.push_str(&surface.account_key_expr(name));
                            i = after_dot_end;
                            matched = true;
                            break;
                        }
                        // v2.29 Slice G.4 — `<imported_acct>.<field>`
                        // routes through the local mirror at
                        // `crate::imported::<ns>::<Type>`. Multi-
                        // variant ADT goes through the accessor
                        // (`(*ctx.<name>.inner.<field>())`); flat
                        // structs read directly off the wrapper's
                        // auto-deref (`ctx.<name>.<field>`).
                        if after < bytes.len() && bytes[after] == b'.' {
                            if let Some(acct_meta) = handler_accounts_by_name.get(name.as_str()) {
                                if let (Some(ns), Some(ty)) =
                                    (&acct_meta.imported_namespace, &acct_meta.account_type)
                                {
                                    // Extract the field ident.
                                    let mut j = after + 1;
                                    while j < bytes.len() && is_ident_char(bytes[j]) {
                                        j += 1;
                                    }
                                    let field = &expr[after + 1..j];
                                    if !field.is_empty() {
                                        let imported_ns = spec.imported_namespaces.get(ns.as_str());
                                        let imported_ty = imported_ns.and_then(|ins| {
                                            ins.account_types.iter().find(|a| &a.name == ty)
                                        });
                                        let is_multi_variant = imported_ty
                                            .map(|a| a.variants.len() > 1)
                                            .unwrap_or(false);
                                        if is_multi_variant {
                                            after_accounts.push_str(&format!(
                                                "(*ctx.{}.inner.{}())",
                                                name, field
                                            ));
                                        } else {
                                            after_accounts
                                                .push_str(&format!("ctx.{}.{}", name, field));
                                        }
                                        i = j;
                                        matched = true;
                                        break;
                                    }
                                }
                            }
                        }
                        // Don't rewrite `name.` (field access on
                        // the handler-account is a different
                        // expression — keep the `.` access path).
                        if after >= bytes.len() || bytes[after] != b'.' {
                            after_accounts.push_str(&surface.account_key_expr(name));
                            i = after;
                            matched = true;
                            break;
                        }
                    }
                }
            }
        }
        if !matched {
            after_accounts.push(bytes[i] as char);
            i += 1;
        }
    }

    // Step 2: rewrite `s.` to `ctx.<state>.` if we have a state
    // account. Word-bounded so `accounts[i].fee_credits.get()`
    // doesn't get corrupted to `fee_creditctx.vault.get()`.
    let Some(sa) = state_acct else {
        return after_accounts;
    };
    // v2.29 Slice B (#12 deep) — when the state is a multi-
    // variant ADT, fields that live on variant payloads can't
    // be reached through `ctx.<state>.<field>` directly
    // (the wrapper only carries `inner`). Route through the
    // accessor method emitted in generate_state. We look
    // ahead after each `s.` match to grab the identifier and
    // dispatch: known variant-payload field → accessor call,
    // otherwise → the bare `ctx.<state>.<field>` rewrite for
    // flat-state compatibility.
    let accessor_fields = adt_accessor_field_names(spec);
    let bare_target = format!("ctx.{}.", sa.name);
    let bytes = after_accounts.as_bytes();
    let mut out = String::with_capacity(after_accounts.len());
    let mut i = 0;
    while i < bytes.len() {
        let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        if prev_ok && i + 1 < bytes.len() && bytes[i] == b's' && bytes[i + 1] == b'.' {
            // Look ahead to extract the field identifier.
            let mut j = i + 2;
            while j < bytes.len() && is_ident_char(bytes[j]) {
                j += 1;
            }
            let field = &after_accounts[i + 2..j];
            if !field.is_empty() && accessor_fields.contains(field) {
                // v2.29 Slice B accessor call. Wrap in
                // parens + deref so subsequent ops (e.g.
                // `!*paused`, `state.lp_supply.bits`) parse
                // against the accessor return value.
                out.push_str(&format!("(*ctx.{}.inner.{}())", sa.name, field));
                i = j;
            } else {
                out.push_str(&bare_target);
                i += 2;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Emit the `requires` clause checks for one handler.
///
/// #151 Slice 3 — tree-native requires rendering. One render under
/// `Binder::SelfAcct("ctx.<state>")` (state receiver) +
/// `AcctKeyStyle` (account key loads) replaces `bind_state`'s
/// two-pass string rewrite. The tree path fires only where it
/// reproduces the legacy receivers exactly:
///   - flat state (multi-variant ADT fields route through the
///     `.inner.<field>()` accessor — a spec-indexed rewrite the
///     Copy render context can't carry), and
///   - key-style account reads only (imported-account field reads
///     need the `crate::imported` mirror dispatch).
///
/// Everything else — and tree-less clauses (IDL ingest, probes) —
/// keeps the legacy string path.
// Internal seam of the generate_guards split; the params are the loop-local
// context, not an API — a carrier struct would just rename them.
#[allow(clippy::too_many_arguments)]
fn emit_requires_guards(
    out: &mut String,
    handler: &ParsedHandler,
    hm: &crate::mir::HandlerMir,
    spec: &ParsedSpec,
    surface: &FrameworkSurface,
    target: Target,
    state_acct: Option<&crate::check::ParsedHandlerAccount>,
    pod_target: bool,
    err_enum: &str,
) {
    // v2.29 Slice B — collect abstract-binder names. Requires that
    // reference an abstract binder can't run in the guard fn (the
    // binder is computed AFTER the guard fires in the handler
    // scaffold). Defer to the handler body and document the skip.
    let abstract_binder_names: Vec<&str> = hm
        .abstract_binders
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();

    let acct_key_style = match target {
        Target::Anchor => crate::rust_codegen_util::tree_render::AcctKeyStyle::AnchorCtx,
        Target::Quasar | Target::Pinocchio => {
            // Pinocchio never reaches here (dedicated emitter above).
            crate::rust_codegen_util::tree_render::AcctKeyStyle::QuasarCtx
        }
    };
    let guard_receiver = state_acct.map(|sa| format!("ctx.{}", sa.name));
    let spec_is_adt = is_multi_variant_adt_state(spec);

    for req in &handler.requires {
        use crate::rust_codegen_util::tree_render::{
            account_reads_are_key_style, render_rust, tree_references_abstract_binder, Binder,
            RustCx,
        };
        // Emit as a comment for human readers + an executable check.
        out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
        let raw = if pod_target {
            req.rust_expr_pod.trim()
        } else {
            req.rust_expr.trim()
        };

        // v2.29 Slice B — abstract-binder defer. The guard runs
        // before the user's handler body computes the binder; the
        // verifier still enforces this clause via the binder's
        // symbolic value. The user should re-assert it in their
        // handler body after the binder is computed. Structural on
        // the tree; word-boundary substring scan for legacy strings.
        let references_abstract = match &req.tree {
            Some(tree) => tree_references_abstract_binder(tree),
            None => {
                !abstract_binder_names.is_empty()
                    && abstract_binder_names
                        .iter()
                        .any(|name| contains_word_boundary(raw, name))
            }
        };
        if references_abstract {
            out.push_str("    //   DEFERRED — references an `abstract` binder; verifier still\n");
            out.push_str(
                "    //   enforces the clause symbolically. Re-assert in the handler body\n",
            );
            out.push_str(
                "    //   after the `let <binder> = …;` line if you want a runtime check.\n",
            );
            continue;
        }

        let rust = match &req.tree {
            Some(tree) if !spec_is_adt && account_reads_are_key_style(tree) => {
                let binder = match &guard_receiver {
                    Some(receiver) => Binder::SelfAcct(receiver),
                    // No resolvable state account: the legacy path
                    // leaves `s.` unbound for the caller to hand-edit
                    // (R12); `Binder::S` renders the same form.
                    None => Binder::S,
                };
                let cx = RustCx::native()
                    .with_binder(binder)
                    .with_acct_key(Some(acct_key_style));
                let cx = if pod_target {
                    RustCx { pod: true, ..cx }
                } else {
                    cx
                };
                render_rust(tree, cx)
            }
            Some(_) | None => bind_state_expr(raw, handler, state_acct, spec, surface),
        };
        if let Some(err) = &req.error_name {
            out.push_str(&format!(
                "    if !({}) {{ return Err({}); }}\n",
                rust,
                surface.error_expr(err_enum, err),
            ));
        } else {
            // Bare `requires` (no `else <ErrorCode>`). Pre-v2.14 emitted
            // `debug_assert!`, which silently no-ops in release builds —
            // every bare requires would skip its check in production.
            // Emit a real runtime check with `ProgramError::Custom(0xFF)`
            // (sentinel "predicate violated, no specific error code").
            // The auditor's `bounty_intent_drift` predicate flags
            // bare requires as P3 — users should still add an explicit
            // `else <Error>` for diagnostic clarity, but the check now
            // runs either way.
            out.push_str(&format!(
                "    if !({}) {{ return Err({}); }}\n",
                rust,
                surface.generic_error_expr()
            ));
        }
    }
}

/// True when `expr` references the spec's state binder `s` (a word-bounded
/// `s` immediately followed by `.`), i.e. it reads a state field.
pub(crate) fn references_pinocchio_state(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b's'
            && (i == 0 || !is_ident_char(bytes[i - 1]))
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'.'
        {
            return true;
        }
    }
    false
}

/// `true` iff state field `field` is declared `Pubkey` in any account
/// variant. Pubkey fields lower to a raw `[u8; 32]` in the zeropod struct
/// (not a Pod scalar wrapper), so they are read by value — no `.get()` —
/// and compared against `*<acct>.key()` (also a `[u8; 32]` value).
pub(crate) fn state_field_is_pubkey(spec: &ParsedSpec, field: &str) -> bool {
    spec.account_types.iter().any(|a| {
        a.variants
            .iter()
            .any(|v| v.fields.iter().any(|(n, t)| n == field && t == "Pubkey"))
    })
}

/// Rewrite a spec-rendered Pod expression (`rust_expr_pod`) for the
/// Pinocchio guard / handler context: the state binder `s` → `state_var`
/// (the decoded zeropod `&Zc` view), and bare handler-account idents (or
/// `<acct>.pubkey`) → `*<acct_prefix>.<acct>.key()` (a `[u8; 32]` value).
/// `acct_prefix` is `"ctx"` in the per-handler guard fn (its param is
/// `ctx: &<Pascal>`) and `"self"` in the handler method's effect body
/// (where the accounts struct is `self`).
///
/// Scalar zeropod fields read through `.get()`; `Pubkey` fields are raw
/// `[u8; 32]` (no Pod wrapper) so they read by value with neither `.get()`
/// nor `&`, matching the deref'd `key()` form on the other side.
pub(crate) fn bind_pinocchio_expr(
    expr: &str,
    handler: &ParsedHandler,
    state_var: &str,
    acct_prefix: &str,
    spec: &ParsedSpec,
) -> String {
    let account_names: std::collections::HashSet<&str> =
        handler.accounts.iter().map(|a| a.name.as_str()).collect();
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let prev_ok = i == 0 || !is_ident_char(bytes[i - 1]);
        if prev_ok && is_ident_char(bytes[i]) {
            let start = i;
            let mut j = i;
            while j < bytes.len() && is_ident_char(bytes[j]) {
                j += 1;
            }
            let ident = &expr[start..j];
            if ident == "s" {
                // `s.<field>` → `<state_var>.<field>.get()` for a scalar
                // read (zeropod Pod fields need an explicit `.get()` to
                // produce the native integer). `Pubkey` fields are raw
                // `[u8; 32]` (no Pod wrapper) so they read by value with
                // no `.get()`. Complex paths (`s.x.y` / `s.x[i]`) emit the
                // path head without `.get()` — those nested/array reads
                // are a follow-on.
                if j < bytes.len() && bytes[j] == b'.' {
                    let fstart = j + 1;
                    let mut k = fstart;
                    while k < bytes.len() && is_ident_char(bytes[k]) {
                        k += 1;
                    }
                    let field = &expr[fstart..k];
                    let complex = k < bytes.len() && (bytes[k] == b'.' || bytes[k] == b'[');
                    if !field.is_empty() && !complex && !state_field_is_pubkey(spec, field) {
                        out.push_str(&format!("{}.{}.get()", state_var, field));
                    } else {
                        out.push_str(&format!("{}.{}", state_var, field));
                    }
                    i = k;
                    continue;
                }
                out.push_str(state_var);
                i = j;
                continue;
            }
            if account_names.contains(ident) {
                // `<acct>.pubkey` (spec's "this account's address") and a
                // bare `<acct>` both lower to the runtime key load,
                // deref'd to a `[u8; 32]` value so it compares against /
                // assigns into a raw-`[u8; 32]` Pubkey state field.
                let pubkey = b".pubkey";
                if j + pubkey.len() <= bytes.len()
                    && &bytes[j..j + pubkey.len()] == pubkey
                    && (j + pubkey.len() == bytes.len() || !is_ident_char(bytes[j + pubkey.len()]))
                {
                    out.push_str(&format!("*{}.{}.key()", acct_prefix, ident));
                    i = j + pubkey.len();
                    continue;
                }
                out.push_str(&format!("*{}.{}.key()", acct_prefix, ident));
                i = j;
                continue;
            }
            out.push_str(ident);
            i = j;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Emit `src/guards.rs` for the Pinocchio target (slice 6 4b). Per-handler
/// guard fns take `ctx: &<Pascal>` + params and return `ProgramResult`.
/// Handles signer-`auth` (`is_signer`) and `requires` (param
/// clauses directly; scalar state clauses via a one-time zeropod decode of
/// the state account, reusing `rust_expr_pod` since zeropod shares
/// quasar-pod's `.get()` API). Lifecycle pre-checks + PDA verification, and
/// state clauses on multi-account specs, are deferred (documented skip).
pub(crate) fn emit_pinocchio_guards(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    output_dir: &Path,
) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let mut out = String::new();
    out.push_str(&marker(
        "DO NOT EDIT — regenerated from .qedspec",
        fp,
        "src/guards.rs",
    ));
    out.push_str("//! Per-handler guard checks derived from the `.qedspec` (Pinocchio).\n\n");
    out.push_str(
        "#![allow(unused_variables, unused_imports, dead_code, clippy::too_many_arguments)]\n\n",
    );
    out.push_str(
        "use pinocchio::{account_info::AccountInfo, program_error::ProgramError, ProgramResult};\n",
    );
    out.push_str("use zeropod::ZeroPodFixed;\n");
    out.push_str("use crate::state::*;\n");
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    if !spec.ref_impls.is_empty() {
        out.push_str("use crate::ref_impls::*;\n");
    }
    out.push_str("use crate::instructions::*;\n\n");

    let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
    // Multi-account state decode is deferred — the decode type is
    // unambiguous only for single-account specs.
    let single_state = spec.account_types.len() <= 1;
    let state_type = format!("{}Account", to_pascal_case(&spec.program_name));

    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        let mut params = vec![format!("ctx: &{}", pascal)];
        for (pname, ptype) in &handler.takes_params {
            params.push(format!("{}: {}", pname, map_type_standalone(ptype, spec)?));
        }
        out.push_str(&format!(
            "/// Guards for `{}` — generated from the spec's `requires` / `auth` clauses.\n",
            handler.name
        ));
        out.push_str(&format!(
            "pub fn {}({}) -> ProgramResult {{\n",
            handler.name,
            params.join(", ")
        ));

        if let Some(who) = &handler.who {
            if handler
                .accounts
                .iter()
                .any(|a| &a.name == who && a.is_signer)
            {
                out.push_str(&format!("    // auth {}\n", who));
                out.push_str(&format!(
                    "    if !ctx.{}.is_signer() {{\n        return Err(ProgramError::MissingRequiredSignature);\n    }}\n",
                    who
                ));
            }
        }

        let needs_state = handler
            .requires
            .iter()
            .any(|r| references_pinocchio_state(&r.rust_expr));
        let decoded = if needs_state && single_state {
            match resolve_handler_state_account(handler, spec) {
                Some(acct) => {
                    out.push_str(&format!(
                        "    let __state = {}::from_bytes(unsafe {{ ctx.{}.borrow_data_unchecked() }})\n        .map_err(|_| ProgramError::InvalidAccountData)?;\n",
                        state_type, acct.name
                    ));
                    true
                }
                None => false,
            }
        } else {
            false
        };

        for req in &handler.requires {
            let raw = req.rust_expr.trim();
            if references_pinocchio_state(raw) && !decoded {
                out.push_str(&format!(
                    "    // TODO(slice 6 4b-cont): state-referencing requires (multi-account /\n    //   unresolved state account) — not enforced yet: {}\n",
                    req.lean_expr.trim()
                ));
                continue;
            }
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let rust = bind_pinocchio_expr(raw, handler, "__state", "ctx", spec);
            let err = match &req.error_name {
                Some(e) => format!("ProgramError::from({}::{})", err_enum, e),
                None => "ProgramError::Custom(0xFF)".to_string(),
            };
            out.push_str(&format!("    if !({}) {{ return Err({}); }}\n", rust, err));
        }

        out.push_str("    Ok(())\n}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");
    write_generated_file(&src_dir.join("guards.rs"), &out)?;
    Ok(())
}

/// The `on_account` ADT ↔ handler-account naming convention: the account
/// is named exactly the lowercased ADT, or prefixed by it.
pub(crate) fn account_name_matches_adt(adt: &str, acct_name: &str) -> bool {
    let lower = adt.to_lowercase();
    acct_name == lower || acct_name.starts_with(&lower)
}

/// True when `handler` is an init transition (`pre_status` Uninitialized /
/// Empty) targeting the account named `acct_name` — its `on_account` ADT
/// matches by naming convention, or no `on_account` is declared. Callers
/// compose their own extra conjuncts (signer / pda_seeds).
pub(crate) fn handler_is_init_for(handler: &ParsedHandler, acct_name: &str) -> bool {
    matches!(
        handler.pre_status.as_deref(),
        Some("Uninitialized") | Some("Empty")
    ) && match handler.on_account.as_deref() {
        Some(adt) => account_name_matches_adt(adt, acct_name),
        None => true,
    }
}

/// R28 firing predicate for one account's `pda_seeds`: does the runtime
/// PDA verification emit for this (target, seeds) pair? Quasar fires on
/// any state-field seed (non-literal, not a bound handler account);
/// Anchor only under a multi-variant ADT state when the seed is a
/// variant-payload field; Pinocchio never (dedicated emitter). Shared
/// between `generate_guards` (the check emission) and
/// `codegen_mir::emit_errors` (the `InvalidPda` variant) so the guard
/// can't reference a variant the error enum didn't declare.
pub(crate) fn r28_pda_check_fires(
    target: Target,
    spec: &ParsedSpec,
    seeds: &[String],
    bound_account_names: &std::collections::HashSet<&str>,
) -> bool {
    let is_state_seed = |seed: &String| {
        let is_literal = seed.starts_with('"') && seed.ends_with('"');
        !is_literal && !bound_account_names.contains(seed.as_str())
    };
    match target {
        Target::Quasar => seeds.iter().any(is_state_seed),
        Target::Anchor => {
            is_multi_variant_adt_state(spec)
                && seeds.iter().any(|seed| {
                    is_state_seed(seed)
                        && spec.account_types.iter().any(|a| {
                            a.variants
                                .iter()
                                .any(|v| v.fields.iter().any(|(n, _)| n == seed))
                        })
                })
        }
        Target::Pinocchio => false,
    }
}

/// Infer the state struct name for a handler account in multi-account specs.
pub(crate) fn infer_state_name(
    acct: &crate::check::ParsedHandlerAccount,
    spec: &ParsedSpec,
    default: &str,
) -> String {
    // Check if this account name matches any account type name (lowercase match)
    for at in &spec.account_types {
        if acct.name == at.name.to_lowercase() || acct.name.starts_with(&at.name.to_lowercase()) {
            return format!("{}Account", at.name);
        }
    }
    default.to_string()
}

/// Sections of `Cargo.toml` that qedgen owns and rewrites on every
/// `qedgen codegen` run. Sections outside this set (e.g.,
/// `[dev-dependencies]`, `[profile.release]`, custom feature flags) are
/// preserved verbatim when the file already exists — see
/// [`merge_cargo_toml`] / PRD-v2.21 §S2.3.
///
/// `[dependencies]` is qedgen-owned but with a sub-table preserve pass
/// inside [`merge_cargo_toml`] (any user-added crate stays; qedgen-owned
/// crates are upserted).
pub(crate) const QEDGEN_OWNED_SECTIONS: &[&str] =
    &["package", "lib", "features", "dependencies", "workspace"];

/// Crates qedgen manages inside `[dependencies]`. Other crates the user
/// adds to that section are preserved by [`merge_cargo_toml`].
pub(crate) const QEDGEN_OWNED_DEPS: &[&str] = &[
    "anchor-lang",
    "anchor-spl",
    "quasar-lang",
    "quasar-spl",
    "pinocchio",
    "pinocchio-token",
    "pinocchio-pubkey",
    "zeropod",
    "qedgen-macros",
];
