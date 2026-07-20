use super::*;
use crate::mir::Mir;
use crate::rust_codegen_util::tree_render::tree_mentions_state;

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
        if h.let_bindings.iter().any(|b| probe(&b.rust_expr)) {
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
        let let_acct_key = match target {
            Target::Quasar => crate::rust_codegen_util::tree_render::AcctKeyStyle::QuasarCtx,
            Target::Anchor | Target::Pinocchio => {
                crate::rust_codegen_util::tree_render::AcctKeyStyle::AnchorCtx
            }
        };
        for b in &handler.let_bindings {
            let rewritten = render_let_binding_rust(
                b,
                state_acct.map(|sa| format!("ctx.{}", sa.name)),
                pod_target,
                Some(let_acct_key),
                spec,
            );
            out.push_str(&format!(
                "    // let-binding from spec: {} = {}\n",
                b.name, b.rust_expr
            ));
            out.push_str(&format!("    let {} = {};\n", b.name, rewritten));
        }

        emit_requires_guards(
            &mut out, handler, spec, &surface, target, state_acct, pod_target, &err_enum,
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

/// R28: emit runtime PDA verification exactly when the account plan assigns
/// seed enforcement to `SeedPlan::Runtime`. The account macro and this guard
/// are complementary by construction; neither re-derives the other's
/// suppression predicate. The cost is one syscall (~544 CU on first-try bump
/// 255) per affected handler load.
fn emit_r28_pda_checks(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    target: Target,
    err_enum: &str,
) {
    if matches!(target, Target::Pinocchio) {
        return; // Pinocchio has a dedicated account/guard emitter.
    }
    // Same state-account resolution as the scaffold's attribute emission —
    // `derive` consults it for init detection on type-qualified
    // single-account specs (#263), and a mismatch here would re-emit the
    // macro-owned check at runtime.
    let state_acct = resolve_handler_state_account(handler, spec);
    for acct in &handler.accounts {
        let is_state = state_acct.map(|sa| sa.name == acct.name).unwrap_or(false);
        let plan = AccountPlan::derive(acct, handler, target, spec, is_state);
        if !matches!(plan.seeds, SeedPlan::Runtime) {
            continue;
        }
        let seeds = acct
            .pda_seeds
            .as_ref()
            .expect("SeedPlan::Runtime requires declared PDA seeds");
        let bound_account_names: std::collections::HashSet<&str> =
            handler.accounts.iter().map(|a| a.name.as_str()).collect();

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

/// Render a `let`-binding RHS for a scaffold position — tree-native
/// (#156 tail): state reads bind through `receiver` (multi-variant ADT
/// fields through the generated accessor), account reads through the
/// target's key-load style when given, and a `mul_div_*` spine narrows
/// back to `u64` at the binding site — the structural twin of the
/// adapter's `is_mul_div_let_rhs` gate on the retired string carrier.
pub(crate) fn render_let_binding_rust(
    b: &crate::check::ParsedLetBinding,
    receiver: Option<String>,
    pod_target: bool,
    acct_key: Option<crate::rust_codegen_util::tree_render::AcctKeyStyle>,
    spec: &ParsedSpec,
) -> String {
    use crate::rust_codegen_util::tree_render::{render_rust, Binder, RustCx};
    let tree = b
        .tree
        .as_ref()
        .expect("ParsedLetBinding.tree is always populated by the chumsky adapter (#151/#156)");
    let accessors = adt_accessor_field_names(spec);
    let binder = match &receiver {
        Some(r) => Binder::SelfAcct(r),
        // No resolvable state account — leave `s.` for the caller to
        // hand-edit (same R12 contract as the requires lane).
        None => Binder::S,
    };
    let cx = RustCx::native()
        .with_pod(pod_target.then_some(crate::rust_codegen_util::tree_render::PodStyle::Quasar))
        .with_binder(binder)
        .with_acct_key(acct_key)
        .with_adt_accessors((!accessors.is_empty()).then_some(&accessors));
    let rendered = render_rust(tree, cx);
    if tree.is_mul_div() {
        format!("({}) as u64", rendered)
    } else {
        rendered
    }
}

/// Emit the `requires` clause checks for one handler.
///
/// Tree-native (#151 Slice 3; the last string lanes went structural in
/// #223): one render under `Binder::SelfAcct("ctx.<state>")` (state
/// receiver) + `AcctKeyStyle` (account key loads) + `adt_accessors`
/// (multi-variant ADT fields → `(*ctx.<state>.inner.<field>())`) +
/// `acct_mirror` (imported-account field reads → the `crate::imported`
/// mirror) covers every clause shape. Non-imported account field
/// projections render as the bare `<acct>.<field>` path for the caller
/// to hand-edit — the same R12 contract the retired `bind_state_expr`
/// string rewriter carried.
// Internal seam of the generate_guards split; the params are the loop-local
// context, not an API — a carrier struct would just rename them.
#[allow(clippy::too_many_arguments)]
fn emit_requires_guards(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    surface: &FrameworkSurface,
    target: Target,
    state_acct: Option<&crate::check::ParsedHandlerAccount>,
    pod_target: bool,
    err_enum: &str,
) {
    use crate::rust_codegen_util::tree_render::{
        render_rust, tree_references_abstract_binder, AcctKeyStyle, Binder, PodStyle, RustCx,
    };

    let acct_key_style = match target {
        Target::Anchor => AcctKeyStyle::AnchorCtx,
        Target::Quasar | Target::Pinocchio => {
            // Pinocchio never reaches here (dedicated emitter above).
            AcctKeyStyle::QuasarCtx
        }
    };
    let guard_receiver = state_acct.map(|sa| format!("ctx.{}", sa.name));
    // v2.29 Slice B — multi-variant ADT fields route through the
    // generated inner-enum accessor.
    let accessors = adt_accessor_field_names(spec);
    // v2.29 Slice G.4 — imported accounts route `<acct>.<field>` reads
    // through the local mirror; the map value carries the imported
    // type's shape (multi-variant → accessor, flat → auto-deref). An
    // unresolvable namespace/type still routes (flat form), matching
    // the retired string rewriter.
    let mirror_map: std::collections::HashMap<String, bool> = handler
        .accounts
        .iter()
        .filter_map(|a| {
            let ns = a.imported_namespace.as_ref()?;
            let ty = a.account_type.as_ref()?;
            let multi = spec
                .imported_namespaces
                .get(ns)
                .and_then(|ins| ins.account_types.iter().find(|t| &t.name == ty))
                .map(|t| t.variants.len() > 1)
                .unwrap_or(false);
            Some((a.name.clone(), multi))
        })
        .collect();

    for req in &handler.requires {
        // Emit as a comment for human readers + an executable check.
        out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
        let tree = requires_tree(req);

        // v2.29 Slice B — abstract-binder defer. The guard runs
        // before the user's handler body computes the binder; the
        // verifier still enforces this clause via the binder's
        // symbolic value. The user should re-assert it in their
        // handler body after the binder is computed.
        if tree_references_abstract_binder(tree) {
            out.push_str("    //   DEFERRED — references an `abstract` binder; verifier still\n");
            out.push_str(
                "    //   enforces the clause symbolically. Re-assert in the handler body\n",
            );
            out.push_str(
                "    //   after the `let <binder> = …;` line if you want a runtime check.\n",
            );
            continue;
        }

        let binder = match &guard_receiver {
            Some(receiver) => Binder::SelfAcct(receiver),
            // No resolvable state account: leave `s.` unbound for the
            // caller to hand-edit (R12); `Binder::S` renders that form.
            None => Binder::S,
        };
        let cx = RustCx::native()
            .with_binder(binder)
            .with_acct_key(Some(acct_key_style))
            .with_pod(pod_target.then_some(PodStyle::Quasar))
            .with_adt_accessors((!accessors.is_empty()).then_some(&accessors))
            .with_acct_mirror((!mirror_map.is_empty()).then_some(&mirror_map));
        let rust = render_rust(tree, cx);
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

/// The typed tree of a requires clause. Post-#151 every production
/// `ParsedRequires` is adapter-built with `tree: Some(...)`; a `None`
/// here is a hand-built fixture that must be fixed, not worked around.
fn requires_tree(req: &crate::check::ParsedRequires) -> &crate::mir::ExprTree {
    req.tree
        .as_ref()
        .expect("ParsedRequires.tree is always populated by the chumsky adapter (#151/#156)")
}

/// Render a spec expression for a Pinocchio position — tree-native
/// (#223, was the `bind_pinocchio_expr` string rewriter): state reads
/// bind through the decoded zeropod view (`__state.<field>.get()` on
/// scalar Pod fields; `Pubkey` fields are raw `[u8; 32]` and read by
/// value), and bare `<acct>` / `<acct>.pubkey` reads lower to the
/// deref'd runtime key load. `acct_key` picks the receiver:
/// [`AcctKeyStyle::PinocchioCtx`] in the per-handler guard fn (its
/// param is `ctx: &<Pascal>`), [`AcctKeyStyle::PinocchioSelf`] in the
/// handler method's effect body (where the accounts struct is `self`).
pub(crate) fn render_pinocchio_expr(
    tree: &crate::mir::ExprTree,
    acct_key: crate::rust_codegen_util::tree_render::AcctKeyStyle,
) -> String {
    use crate::rust_codegen_util::tree_render::{render_rust, Binder, PodStyle, RustCx};
    let cx = RustCx::native()
        .with_binder(Binder::SelfAcct("__state"))
        .with_pod(Some(PodStyle::Zeropod))
        .with_acct_key(Some(acct_key));
    render_rust(tree, cx)
}

/// Emit `src/guards.rs` for the Pinocchio target (slice 6 4b). Per-handler
/// guard fns take `ctx: &<Pascal>` + params and return `ProgramResult`.
/// Handles signer-`auth` (`is_signer`) and `requires` (param clauses
/// directly; scalar state clauses via a one-time zeropod decode of the
/// state account, rendered tree-native under [`PodStyle::Zeropod`]).
/// Lifecycle pre-checks + PDA verification, and state clauses on
/// multi-account specs, are deferred (documented skip).
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
            .any(|r| tree_mentions_state(requires_tree(r)));
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
            let tree = requires_tree(req);
            if tree_mentions_state(tree) && !decoded {
                out.push_str(&format!(
                    "    // TODO(slice 6 4b-cont): state-referencing requires (multi-account /\n    //   unresolved state account) — not enforced yet: {}\n",
                    req.lean_expr.trim()
                ));
                continue;
            }
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let rust = render_pinocchio_expr(
                tree,
                crate::rust_codegen_util::tree_render::AcctKeyStyle::PinocchioCtx,
            );
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
/// `[profile.release]`, custom feature flags) are preserved verbatim
/// when the file already exists — see [`merge_cargo_toml`] /
/// PRD-v2.21 §S2.3.
///
/// `[dependencies]` / `[dev-dependencies]` are qedgen-owned but with a
/// sub-table preserve pass inside [`merge_cargo_toml`] (any user-added
/// crate stays; qedgen-owned crates are upserted).
pub(crate) const QEDGEN_OWNED_SECTIONS: &[&str] = &[
    "package",
    "lib",
    "features",
    "dependencies",
    "dev-dependencies",
    "workspace",
];

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

/// Crates qedgen manages inside `[dev-dependencies]` — kept separate
/// from [`QEDGEN_OWNED_DEPS`] so a user-added `proptest` line in
/// `[dependencies]` is never treated as qedgen-owned (and vice versa).
pub(crate) const QEDGEN_OWNED_DEV_DEPS: &[&str] = &["proptest"];
