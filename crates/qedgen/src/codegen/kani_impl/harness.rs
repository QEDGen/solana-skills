use super::*;

/// Emit `mod symbolic_accounts { ... }` with one `build_<handler>` ctor per
/// emit target. The body is a `todo!()` skeleton that lists each account
/// field with its derivation rule (PDA seed expression vs `kani::any()`)
/// as inline comments. The agent (or user) replaces the body with the
/// concrete `crate::<HandlerPascal> { ... }` construction.
pub(crate) fn emit_symbolic_accounts_module(
    out: &mut String,
    spec: &ParsedSpec,
    targets: &[&ParsedHandler],
    framework: &str,
) -> Result<()> {
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str(&format!(
        "// Symbolic {} `Accounts` context builders.\n",
        framework
    ));
    out.push_str("//\n");
    out.push_str("// Each ctor returns a context with:\n");
    out.push_str("//   - PDA-derived pubkeys computed from the spec's `pda` declarations\n");
    out.push_str("//   - `kani::any()` for non-PDA addresses + account-data fields\n");
    out.push_str("//   - Well-known program IDs for `token_program`, `system_program`, etc.\n");
    out.push_str("//\n");
    out.push_str("// The ctors are AGENT-FILL skeletons: the data-bearing fields\n");
    out.push_str("// (state struct contents, token amounts, mints) get populated to\n");
    out.push_str("// match the user's handler signature. Without that fill, the file\n");
    out.push_str("// won't compile — by design — so it surfaces as a `todo!()` to address.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    out.push_str("mod symbolic_accounts {\n");
    out.push_str(&format!(
        "    // The user's program crate is the host for this harness. {}\n",
        framework
    ));
    out.push_str("    // re-exports `#[derive(Accounts)]` structs at crate root via\n");
    out.push_str("    // `#[program]`, so the handler's accounts struct resolves via\n");
    out.push_str("    // `crate::<HandlerPascal>`.\n");
    out.push_str("    #![allow(unused_imports, dead_code)]\n");

    for handler in targets {
        emit_symbolic_accounts_ctor(out, handler, spec, framework)?;
    }

    out.push_str("} // mod symbolic_accounts\n\n");
    Ok(())
}

/// Emit a single `pub fn build_<handler>() -> crate::<Pascal>` constructor.
/// Body is a `todo!()` skeleton with per-account-field derivation comments
/// for agent fill-in.
fn emit_symbolic_accounts_ctor(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    framework: &str,
) -> Result<()> {
    let pascal = to_pascal_case(&handler.name);
    out.push_str(&format!(
        "\n    /// Symbolic `Accounts` context for the user's `{}` handler.\n",
        handler.name
    ));
    out.push_str("    ///\n");
    out.push_str("    /// AGENT-FILL: replace the `todo!()` body with the concrete\n");
    out.push_str("    /// construction. Each account field is annotated with its\n");
    out.push_str("    /// derivation rule below.\n");
    out.push_str(&format!(
        "    pub fn build_{}() -> crate::{} {{\n",
        handler.name, pascal
    ));

    if handler.accounts.is_empty() {
        if handler.who.is_some() {
            out.push_str(
                "        // No explicit accounts; spec declares an `auth` actor → signer.\n",
            );
            out.push_str(&format!(
                "        todo!(\"Construct crate::{} with a symbolic signer\")\n",
                pascal
            ));
        } else {
            out.push_str("        // No accounts declared on this handler.\n");
            out.push_str(&format!(
                "        todo!(\"Construct crate::{} with the handler's account context\")\n",
                pascal
            ));
        }
    } else {
        for acct in &handler.accounts {
            emit_account_field_skeleton(out, acct, handler, spec);
        }
        out.push_str("        //\n");
        out.push_str("        // AGENT: assemble the fields above into the concrete\n");
        out.push_str(&format!(
            "        // `crate::{}` struct. The {} `#[derive(Accounts)]`\n",
            pascal, framework
        ));
        out.push_str("        // expansion gives the exact field layout.\n");
        out.push_str(&format!("        todo!(\"assemble crate::{}\")\n", pascal));
    }

    out.push_str("    }\n");
    Ok(())
}

/// `true` for the DSL's integer scalar types — these serialize to a seed
/// via `to_le_bytes()` rather than `as_ref()`.
fn is_integer_dsl_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" | "Nat"
    )
}

/// Emit one commented-out line per account field with its derivation rule.
/// PDA-bound accounts get a `Pubkey::find_program_address` template using
/// the spec's `pda <name> [seeds]` declaration; non-PDA fields default to
/// `kani::any()`; programs use their well-known IDs.
fn emit_account_field_skeleton(
    out: &mut String,
    acct: &crate::check::ParsedHandlerAccount,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) {
    if acct.is_program {
        out.push_str(&format!(
            "        // `{}`: well-known program ID (e.g. token / system / rent)\n",
            acct.name
        ));
        return;
    }
    if let Some(seeds) = &acct.pda_seeds {
        // Prefer the top-level `pda <name> [seeds]` declaration when it
        // matches by name; fall back to the inline seeds otherwise.
        let pda_seeds: Vec<String> = spec
            .pdas
            .iter()
            .find(|p| p.name == acct.name)
            .map(|p| p.seeds.clone())
            .unwrap_or_else(|| seeds.clone());
        let seed_exprs: Vec<String> = pda_seeds
            .iter()
            .map(|s| {
                if (s.starts_with('"') && s.ends_with('"'))
                    || (s.starts_with('\'') && s.ends_with('\''))
                {
                    let inner = &s[1..s.len() - 1];
                    format!("b\"{}\"", inner)
                } else if handler
                    .takes_params
                    .iter()
                    .any(|(n, t)| n == s && is_integer_dsl_type(t))
                {
                    // An integer handler param used as a seed must be
                    // serialized to bytes — `u64::as_ref()` doesn't exist.
                    // (`Pubkey` params / account keys keep `.as_ref()`.)
                    format!("{}.to_le_bytes().as_ref()", s)
                } else {
                    format!("{}.as_ref()", s)
                }
            })
            .collect();
        out.push_str(&format!(
            "        // `{}`: PDA derived from `[{}]`\n",
            acct.name,
            seed_exprs.join(", ")
        ));
        out.push_str(&format!(
            "        //   let ({0}_key, _bump) = solana_program::pubkey::Pubkey::find_program_address(&[{1}], &crate::ID);\n",
            acct.name,
            seed_exprs.join(", ")
        ));
        return;
    }
    if acct.is_signer {
        out.push_str(&format!(
            "        // `{}`: signer — symbolic address via `kani::any()`\n",
            acct.name
        ));
        return;
    }
    out.push_str(&format!(
        "        // `{}`: non-PDA account — symbolic address + data via `kani::any()`\n",
        acct.name
    ));
}

/// Emit one `#[kani::proof]` for a (handler, ensures) pair. Shape:
///   1. Build symbolic accounts context via the `symbolic_accounts` module.
///   2. Snapshot pre-state fields (the modifies set, plus any field the
///      requires/ensures read via `pre.<field>` / `post.<field>`).
///   3. Declare symbolic params + `kani::assume` the handler's requires
///      (state reads rewritten `s.<field>` → `pre_<field>` snapshots).
///   4. Call the user's real handler method.
///   5. On `Ok`, snapshot post-state fields, splice CPI ensures-as-fact
///      `kani::assume` lines for each `call Iface.foo(...)` whose callee
///      declares ensures (Track I), then assert the caller's own ensures.
pub(crate) fn emit_handler_harness(
    out: &mut String,
    handler: &ParsedHandler,
    idx: usize,
    ensures: &crate::check::ParsedEnsures,
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("#[kani::proof]\n");
    // Greenfield doesn't emit the Pubkey stub → keep the memcmp-driven bound.
    let (unwind, why) = suggested_unwind(handler, spec, /*abstract_pubkey=*/ false);
    out.push_str(&format!("#[kani::unwind({unwind})] // {why}\n"));
    out.push_str("#[kani::solver(cadical)]\n");
    out.push_str(&format!(
        "fn verify_{}_impl_ensures_{}() {{\n",
        handler.name, idx
    ));

    // 1. Build the symbolic accounts context.
    out.push_str(&format!(
        "    let mut accounts = symbolic_accounts::build_{}();\n",
        handler.name
    ));

    // 2. Pre-snapshot. Snapshot every field the ensures clause may compare
    //    across the call (union of `modifies` and effect-LHS bare field
    //    names). Path is `accounts.<state_account>.<field>` when the
    //    state account is uniquely identifiable; otherwise the snapshot
    //    falls back to a `todo!()` placeholder for the agent.
    let state_acct = find_state_account_name(handler);

    // The handler's `requires` clauses become the precondition
    // `kani::assume(...)`. `collect_full_guard` renders state reads with the
    // pure-model accessor `s.<field>`; in the impl harness those name the
    // PRE-call state, so rewrite `s.<field>` → `pre.<field>` (flattened to a
    // `pre_<field>` local by `rewrite_pre_post_paths` at emit time below).
    let guard_predot = crate::rust_codegen_util::collect_full_guard(handler, false)
        .map(|g| rewrite_state_var_to_pre(&g));

    // Snapshot set: the modifies/effect/CPI-binder base, PLUS every state
    // field the precondition (`pre.`) and this postcondition (`pre.`/`post.`)
    // reference. Without the latter, a read-only field named in a
    // `requires`/`ensures` but never written (e.g. `num_voters` in
    // `threshold <= num_voters`) yields an unbound `pre_`/`post_` local and
    // the harness fails to compile.
    let snapshot_fields = collect_snapshot_fields(handler, guard_predot.as_deref(), ensures);
    if !snapshot_fields.is_empty() {
        out.push_str(
            "    // Pre-state snapshot — fields the requires/ensures read via `pre.<x>`.\n",
        );
        for field in &snapshot_fields {
            match state_acct {
                Some(acct) => {
                    out.push_str(&format!(
                        "    let pre_{0} = accounts.{1}.{0};\n",
                        field, acct
                    ));
                }
                None => {
                    out.push_str(&format!(
                        "    let pre_{0} = todo!(\"snapshot pre.{0} from the symbolic accounts context\");\n",
                        field
                    ));
                }
            }
        }
    }

    // 3. Symbolic params + preconditions.
    for (pname, ptype) in &handler.takes_params {
        out.push_str(&format!(
            "    let {}: {} = kani::any();\n",
            pname,
            map_type(ptype, spec)?
        ));
    }
    // Apply the handler's `requires` clauses as Kani assumptions so we
    // explore inputs the user's handler would actually accept (otherwise
    // it returns Err and the ensures don't fire — vacuous pass). State reads
    // resolve to the `pre_<field>` snapshots taken above.
    if let Some(guard) = &guard_predot {
        out.push_str(&format!(
            "    kani::assume({});\n",
            rewrite_pre_post_paths(guard)
        ));
    }

    // 4. Call the user's real handler. Anchor handler methods take
    //    `&mut self` and the param list — same shape `cargo build`
    //    expands `#[derive(Accounts)]` + `#[program]` into.
    let args: String = handler
        .takes_params
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("    let result = accounts.handler({});\n", args));

    // 5. Post-snapshot + assertion. The Track I splice point sits between
    //    `if result.is_ok()` and the `assert!` so CPI ensures can be
    //    layered in as `kani::assume` facts.
    out.push_str("    if result.is_ok() {\n");
    if !snapshot_fields.is_empty() {
        out.push_str(
            "        // Post-state snapshot — same fields, read from post-call accounts.\n",
        );
        for field in &snapshot_fields {
            match state_acct {
                Some(acct) => {
                    out.push_str(&format!(
                        "        let post_{0} = accounts.{1}.{0};\n",
                        field, acct
                    ));
                }
                None => {
                    out.push_str(&format!(
                        "        let post_{0} = todo!(\"snapshot post.{0} from the symbolic accounts context\");\n",
                        field
                    ));
                }
            }
        }
    }

    // ── CPI ensures-as-fact (Track I) ──────────────────────────────────
    // For every `call Iface.foo(args)` site whose callee declares its own
    // `ensures`, splice a `kani::assume(<callee_ensures, substituted>)`
    // line so the caller's later assert! can rely on the CPI's
    // contract. Tier-0 callees (no ensures declared) emit nothing —
    // matching the spec-model harness behavior in `kani.rs` and the
    // `lean_gen.rs::render_cpi_theorems` `:= by sorry` fallback.
    //
    // The substituted clauses come back in `pre.X` / `post.X` form (from
    // `rust_expr_binary`); we flatten those to the harness-local
    // `pre_X` / `post_X` snapshots via the same `rewrite_pre_post_paths`
    // helper used on the caller's own ensures below.
    emit_cpi_ensures_as_assume(out, handler, spec);

    // The ensures clause's `rust_expr_binary` uses `pre.<field>` and
    // `post.<field>` paths. Our snapshots are flat `pre_<field>` /
    // `post_<field>` locals (no struct), so we rewrite the path
    // separators. The chumsky_adapter renders `state.x` / `old(state.x)`
    // into exactly `post.x` / `pre.x` — no other source produces these
    // tokens in `rust_expr_binary`, so a string-replace is safe.
    let lowered = rewrite_pre_post_paths(&ensures.rust_expr_binary);
    out.push_str(&format!("        assert!(\n            {},\n", lowered));
    out.push_str(&format!(
        "            \"ensures clause {} on {} (impl) violated\"\n",
        idx, handler.name
    ));
    out.push_str("        );\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Brownfield-Anchor variant of `emit_handler_harness` (#162). Instead of a
/// symbolic `Accounts` context + `accounts.handler(param)` — the greenfield
/// convention that does not match real Anchor (handlers share one Accounts
/// struct, take `Context<T>` + `Args`, are associated fns) — emit a
/// **state-struct** harness: symbolic state → (agent-fill) apply the real
/// effect + validity gate → assert `ensures`. Reuses the exact snapshot /
/// requires-assume / ensures-assert lowering as the greenfield path (incl.
/// the read-only-field snapshot fix), so only the two parts that genuinely
/// need the real source — constructing the struct and applying the effect —
/// are left as `todo!()`. This is the shape both bundled brownfield harnesses
/// (settings well-formedness, delegation conservation) were hand-written to.
/// `true` when the spec's State declares `field` as a non-`Copy` type — a
/// `Vec`/`Map` collection or a custom ADT/record (which derive `Clone`, not
/// `Copy`). Such a field's harness snapshot must `.clone()` rather than move
/// it out of `state`, else the subsequent `&mut state` method call sees a
/// partially-moved value and fails to compile. The fixed-width integers,
/// `Bool`, `Fin[N]` (→ integer) and `Pubkey` (→ `[u8; 32]`) are `Copy` and
/// move freely. Requires the `state_struct` pragma (generated construction);
/// otherwise the field types aren't known and we keep the move (agent-fill
/// construction).
pub(crate) fn state_field_needs_clone(spec: &ParsedSpec, field: &str) -> bool {
    super::state_ctor::resolve_state_struct(spec)
        .map(|(_, fields)| {
            fields
                .iter()
                .any(|(n, t)| n == field && !is_copy_scalar_ty(t))
        })
        .unwrap_or(false)
}

/// The `Copy` scalar surface of the DSL as it lowers into a Kani harness:
/// snapshotting one of these can move without `.clone()`. Everything else —
/// `Vec`/`Map` and custom nominal types — is `Clone`-not-`Copy` and must be
/// cloned (see [`state_field_needs_clone`]).
fn is_copy_scalar_ty(t: &str) -> bool {
    let t = t.trim();
    t.starts_with("Fin")
        || matches!(
            t,
            "U8" | "U16"
                | "U32"
                | "U64"
                | "U128"
                | "I8"
                | "I16"
                | "I32"
                | "I64"
                | "I128"
                | "Bool"
                | "Pubkey"
                | "Bytes32"
                | "Bytes64"
        )
}

/// Emit the shared brownfield-harness proof attributes — `#[kani::proof]`, the
/// computed `#[kani::unwind]`, and every opted-in #182 stub attr — up to (but
/// not including) the `fn <name>() {` line. Shared by the ensures-preservation
/// and reject (guard-enforcement) emitters so a new stub is wired in one place.
pub(crate) fn emit_impl_proof_attrs(out: &mut String, handler: &ParsedHandler, spec: &ParsedSpec) {
    out.push_str("#[kani::proof]\n");
    // `pragma kani_solver = <z3|cvc5|…>` bakes the solver into the harness
    // (`#[kani::solver(z3)]`) so it's reproducible without a `--solver` flag.
    // z3/cvc5 (SMT) reason about bit-vector division/modulo natively — much
    // faster than CaDiCaL's SAT bit-blasting on symbolic `checked_div`.
    if let Some(solver) = spec.pragma_value("kani_solver") {
        out.push_str(&format!("#[kani::solver({solver})]\n"));
    }
    // Unwind bound follows the harness: a `Pubkey` / byte-array compare is a
    // 32-byte `memcmp` loop needing ≥ 34; a numeric-only harness closes at a
    // small bound and runs faster (`suggested_unwind`).
    let abstract_pk = super::state_ctor::wants_pubkey_abstraction(spec);
    let (unwind, why) = suggested_unwind(handler, spec, abstract_pk);
    out.push_str(&format!("#[kani::unwind({unwind})] // {why}\n"));
    // #182 Tier 1 — redirect Pubkey's derived `==` to the abstract wide-integer
    // compare (no 32-byte memcmp loop). Sound (verified equivalent), so it can't
    // change the result; needs `-Z stubbing`.
    if abstract_pk {
        out.push_str(super::state_ctor::pubkey_stub_attr());
    }
    // #182 Tier 2 — redirect PDA derivation to a deterministic uninterpreted
    // function (skip the sha256 + bump-search bit-blast; #189).
    if super::state_ctor::wants_pda_abstraction(spec) {
        out.push_str(super::state_ctor::pda_stub_attr());
    }
    // #189 Tier 2 — hash primitives and secp256k1 recovery as deterministic
    // uninterpreted functions (opt-in).
    if super::state_ctor::wants_hash_stub(spec) {
        out.push_str(super::state_ctor::hash_stub_attr());
    }
    if super::state_ctor::wants_secp256k1_stub(spec) {
        out.push_str(super::state_ctor::secp256k1_stub_attr());
    }
    // G14 — the agent-fill effect calls a `Clock::get()`-reading method; stub it.
    if super::state_ctor::wants_clock_stub(spec) {
        out.push_str(
            "#[kani::stub(anchor_lang::solana_program::clock::Clock::get, stub_clock_get)]\n",
        );
    }
    // #182 Tier 4 — logging no-op + CPI success (opt-in).
    if super::state_ctor::wants_log_stub(spec) {
        out.push_str(super::state_ctor::log_stub_attr());
    }
    if super::state_ctor::wants_cpi_stub(spec) {
        out.push_str(super::state_ctor::cpi_stub_attr());
    }
    // #182 arithmetic tier — abstract `i64::checked_div` (opt-in).
    if super::state_ctor::wants_div_abstraction(spec) {
        out.push_str(super::state_ctor::div_stub_attr());
    }
}

/// Emit the shared symbolic-state construction: the generated
/// `symbolic_<struct>()` ctor call + the `pragma state_invariant` pre-state
/// validity assume, or the agent-fill construction `todo!()` when the State
/// isn't fully constructible. Shared by the ensures and reject emitters.
fn emit_symbolic_state(out: &mut String, spec: &ParsedSpec, state_struct: Option<&str>) {
    match state_struct {
        Some(struct_name) => {
            out.push_str(&format!(
                "    // Symbolic `{struct_name}` — fully generated from the spec's State\n"
            ));
            out.push_str("    // (every field `kani::any()`).\n");
            // Wrap in `ManuallyDrop` so the symbolic state's destructor —
            // `drop_in_place::<[T]>` / `RawVec::deallocate` for nested `Vec`s — is
            // NEVER emitted. That teardown machinery, not the property itself, is
            // what blows CBMC's propositional reduction to an OOM on deeply-nested
            // state (an `Option<Hook{ Vec<AccountConstraint{ Vec<Pubkey> }> }>`
            // measured 20,322 VCCs → OOM; suppressing the drop → 2,395 VCCs, closes
            // in seconds). Sound: skipping a destructor cannot affect a property
            // checked before it, and the harness is `#[cfg(kani)]`-only (never runs
            // on-chain). State is still read via `Deref`/`DerefMut` — method calls,
            // field snapshots, and the by-reference `post.<field>` reads below all
            // work unchanged. See docs/toolchain-backlog.md R2/R4.
            out.push_str(&format!(
                "    let mut state = core::mem::ManuallyDrop::new({}());\n",
                super::state_ctor::ctor_fn_name(struct_name)
            ));
            // Pre-state validity: `pragma state_invariant = <method>` (default
            // `invariant`) → assume it so Kani explores only well-formed states.
            // `= none` skips it — for a struct with no validity method, or a
            // property that is independent of the invariant AND whose invariant
            // has unwraps/arithmetic that panic on fully-symbolic input (the
            // symbolic ctor is stricter than a scoped hand-written harness).
            match super::state_ctor::invariant_method(spec) {
                Some(m) => {
                    out.push_str(&format!(
                        "    kani::assume(state.{m}().is_ok()); // pre-state validity\n\n"
                    ));
                }
                None => out.push('\n'),
            }
        }
        None => {
            out.push_str(
                "    // AGENT-FILL (1/2): build a symbolic instance of the real `#[account]`\n",
            );
            out.push_str(
                "    // struct this spec's `State` models. Fields the spec reasons about →\n",
            );
            out.push_str(
                "    // `kani::any()`; the rest → concrete. Annotate the real type, e.g.:\n",
            );
            out.push_str("    //   let mut state: crate::<RealStateStruct> = todo!();\n");
            out.push_str(
                "    let mut state = todo!(\"build a symbolic state account struct\");\n\n",
            );
        }
    }
}

pub(crate) fn emit_brownfield_handler_harness(
    out: &mut String,
    handler: &ParsedHandler,
    idx: usize,
    ensures: &crate::check::ParsedEnsures,
    spec: &ParsedSpec,
    // `Some(struct_name)` when the file emitted a `symbolic_<struct>()` ctor
    // (the State is fully constructible); the harness calls it instead of a
    // construction `todo!()`. `None` keeps the agent-fill fallback.
    state_struct: Option<&str>,
) -> Result<()> {
    emit_impl_proof_attrs(out, handler, spec);
    out.push_str(&format!(
        "fn verify_{}_impl_ensures_{}() {{\n",
        handler.name, idx
    ));

    // 1. Symbolic state struct. Generated from the spec's State when it fully
    //    mirrors the real `#[account]` struct; otherwise an agent-fill `todo!()`.
    emit_symbolic_state(out, spec, state_struct);

    // 2. Pre-snapshot — reuse the greenfield field set (modifies ∪ effects ∪
    //    CPI-binders ∪ requires/ensures fields) and `s.`→`pre.` guard lowering.
    let guard_predot = crate::rust_codegen_util::collect_full_guard(handler, false)
        .map(|g| rewrite_state_var_to_pre(&g));
    let snapshot = collect_snapshot_fields_split(handler, guard_predot.as_deref(), ensures);
    if !snapshot.pre.is_empty() {
        out.push_str(
            "    // Pre-state snapshot — fields the requires/ensures read via `pre.<x>`.\n",
        );
        for field in &snapshot.pre {
            // A `Vec` field is non-Copy: moving it out here would break the
            // `&mut state` method call below (partial move). Clone it (Copy
            // fields — scalars, `Pubkey` — move/copy as before).
            let rhs = if state_field_needs_clone(spec, field) {
                format!("state.{field}.clone()")
            } else {
                format!("state.{field}")
            };
            out.push_str(&format!("    let pre_{field} = {rhs};\n"));
        }
    }

    // 3. Symbolic params + precondition (reads the pre-snapshots).
    for (pname, ptype) in &handler.takes_params {
        // Brownfield targets the REAL struct, so a `Pubkey` param stays a real
        // `Pubkey` (matching the ctor + the struct's `Vec<Pubkey>` fields) — NOT
        // the spec-model `[u8; 32]` lowering, which wouldn't unify with them.
        if ptype == "Pubkey" {
            out.push_str(&format!(
                "    let {pname}: anchor_lang::prelude::Pubkey = \
                 anchor_lang::prelude::Pubkey::new_from_array(kani::any());\n"
            ));
        } else {
            out.push_str(&format!(
                "    let {}: {} = kani::any();\n",
                pname,
                map_type(ptype, spec)?
            ));
        }
    }
    if let Some(guard) = &guard_predot {
        out.push_str(&format!(
            "    kani::assume({});\n",
            rewrite_pre_post_paths(guard)
        ));
    }

    // 4. Apply the real effect + validity gate — generated from
    //    `pragma kani_target` when the real logic is a state-struct method
    //    (#163/G2); agent-fill otherwise.
    emit_effect_call_binding(out, handler, spec, EffectCallSite::Ensures);

    // 5. Assert the ensures. `post.<field>` reads the mutated-in-place state
    //    field DIRECTLY (`state.<field>`, a place behind `ManuallyDrop`'s
    //    `Deref`), matched by reference — NO owned `post_<field>` snapshot is
    //    bound, so the symbolic nested container is never moved out of `state`
    //    and never dropped. That is the drop-suppression that lets CBMC close on
    //    deeply-nested state (backlog R2). Reading `state.<field>` at assert time
    //    is the post-effect value (the AGENT-FILL mutates `state` in place), so it
    //    is equivalent to the old post-snapshot. `pre.<field>` still reads its
    //    pre-effect snapshot local.
    out.push_str("    if ok {\n");
    let lowered = rewrite_ensures_post_to_state(&ensures.rust_expr_binary);
    out.push_str(&format!("        assert!(\n            {},\n", lowered));
    out.push_str(&format!(
        "            \"ensures clause {} on {} (impl, brownfield) violated\"\n",
        idx, handler.name
    ));
    out.push_str("        );\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Emit a guard-enforcement (reject) proof for a handler with a precondition:
/// assume the `requires` / lifecycle `when` guard is VIOLATED and assert the
/// real handler returns `Err`. This verifies the code ENFORCES the guard the
/// spec declares — the converse of the ensures-preservation proof. Opt-in via
/// `pragma kani_reject`. Returns `false` (emits nothing) for a guardless
/// handler.
pub(crate) fn emit_brownfield_reject_harness(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    state_struct: Option<&str>,
) -> Result<bool> {
    // Only a handler with a precondition (lifecycle `when` and/or `requires`)
    // has a guard to enforce.
    let Some(guard) = crate::rust_codegen_util::collect_full_guard(handler, false) else {
        return Ok(false);
    };
    let guard_predot = rewrite_state_var_to_pre(&guard);

    emit_impl_proof_attrs(out, handler, spec);
    out.push_str(&format!("fn verify_{}_rejects() {{\n", handler.name));

    emit_symbolic_state(out, spec, state_struct);

    // Snapshot the fields the guard reads. The guard is evaluated pre-call on
    // the unmutated state, so `pre_<field>` == `state.<field>` here.
    let mut gfields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    collect_prefixed_fields(&guard_predot, "pre.", &mut gfields);
    let gfields: Vec<String> = gfields.into_iter().collect();
    if !gfields.is_empty() {
        out.push_str("    // Pre-state snapshot — fields the `requires` guard reads.\n");
        for field in &gfields {
            let rhs = if state_field_needs_clone(spec, field) {
                format!("state.{field}.clone()")
            } else {
                format!("state.{field}")
            };
            out.push_str(&format!("    let pre_{field} = {rhs};\n"));
        }
    }

    // Symbolic params (same lowering as the ensures harness).
    for (pname, ptype) in &handler.takes_params {
        if ptype == "Pubkey" {
            out.push_str(&format!(
                "    let {pname}: anchor_lang::prelude::Pubkey = \
                 anchor_lang::prelude::Pubkey::new_from_array(kani::any());\n"
            ));
        } else {
            out.push_str(&format!(
                "    let {}: {} = kani::any();\n",
                pname,
                map_type(ptype, spec)?
            ));
        }
    }

    // The precondition is VIOLATED (at least one `requires`/`when` clause fails).
    out.push_str(&format!(
        "    kani::assume(!({}));\n",
        rewrite_pre_post_paths(&guard_predot)
    ));

    // The SAME real handler call as the ensures harness — generated from
    // `pragma kani_target` when declared (#163/G2), agent-fill otherwise.
    emit_effect_call_binding(out, handler, spec, EffectCallSite::Reject);

    // Guard enforcement: a violated precondition MUST be rejected.
    out.push_str(&format!(
        "    assert!(!ok, \"{} must reject when its `requires`/`when` guard is violated\");\n",
        handler.name
    ));
    out.push_str("}\n\n");
    Ok(true)
}

/// Emit a panic-freedom proof for a handler: construct symbolic state (with the
/// pre-state invariant assumed) + symbolic params, then CALL the real handler
/// with no assertion — Kani's built-in checks (unwrap / overflow / division /
/// index / explicit panic) verify the call cannot panic on any symbolic input.
/// Opt-in via `pragma kani_panic_free`. The natural shape for a `()`-returning
/// method (e.g. `reset_if_needed`) whose only property is that it doesn't abort.
pub(crate) fn emit_brownfield_panic_free_harness(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    state_struct: Option<&str>,
) -> Result<()> {
    emit_impl_proof_attrs(out, handler, spec);
    out.push_str(&format!("fn verify_{}_panic_free() {{\n", handler.name));

    emit_symbolic_state(out, spec, state_struct);

    for (pname, ptype) in &handler.takes_params {
        if ptype == "Pubkey" {
            out.push_str(&format!(
                "    let {pname}: anchor_lang::prelude::Pubkey = \
                 anchor_lang::prelude::Pubkey::new_from_array(kani::any());\n"
            ));
        } else {
            out.push_str(&format!(
                "    let {}: {} = kani::any();\n",
                pname,
                map_type(ptype, spec)?
            ));
        }
    }

    // Assume the handler's preconditions (`requires`/`when`) — panic-freedom is
    // claimed UNDER them: e.g. `current >= last_reset` rules out a `checked_sub`
    // underflow that a fully-symbolic `last_reset` would spuriously trigger but
    // that can't arise on-chain. Snapshot the fields the guard reads (evaluated
    // pre-call, so `pre_<field>` == the unmutated `state.<field>`).
    if let Some(guard) = crate::rust_codegen_util::collect_full_guard(handler, false) {
        let guard_predot = rewrite_state_var_to_pre(&guard);
        let mut gfields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        collect_prefixed_fields(&guard_predot, "pre.", &mut gfields);
        if !gfields.is_empty() {
            out.push_str("    // Pre-state snapshot — fields the `requires` guard reads.\n");
            for field in &gfields {
                let rhs = if state_field_needs_clone(spec, field) {
                    format!("state.{field}.clone()")
                } else {
                    format!("state.{field}")
                };
                out.push_str(&format!("    let pre_{field} = {rhs};\n"));
            }
        }
        out.push_str(&format!(
            "    kani::assume({});\n",
            rewrite_pre_post_paths(&guard_predot)
        ));
    }

    // Call the real handler as a statement (no bind, no assert) — Kani's
    // built-in checks verify panic-freedom during the call. Generated from
    // `pragma kani_target` when declared (#163/G2), agent-fill otherwise.
    match super::state_ctor::kani_target_of(spec, &handler.name) {
        Some(target) => {
            out.push_str(
                "\n    // Call generated from `pragma kani_target` — no assertion; Kani's\n\
                 \x20   // built-in unwrap/overflow/div/index/panic checks verify the call\n\
                 \x20   // cannot abort on any symbolic input.\n",
            );
            // `let _ =` swallows a `#[must_use]` Result/bool return uniformly.
            out.push_str(&format!(
                "    let _ = {};\n",
                kani_target_call_expr(&target, handler)
            ));
        }
        None => {
            out.push_str(
                "\n    // AGENT-FILL: call the real handler here (a statement, e.g.\n\
                 \x20   //   state.<method>(<params>);\n\
                 \x20   // No assertion — Kani's built-in unwrap/overflow/div/index/panic\n\
                 \x20   // checks verify the call cannot abort on any symbolic input.\n",
            );
            out.push_str("    todo!(\"call the real handler (statement, no bind)\");\n");
        }
    }
    out.push_str("}\n\n");
    Ok(())
}

/// Which harness shape an effect-call binding is emitted for — only the
/// agent-fill comment text differs.
#[derive(Clone, Copy)]
enum EffectCallSite {
    Ensures,
    Reject,
}

/// The generated `state.<method>(<params>)` call expression for a resolved
/// `pragma kani_target` (#163/G2). `state` is behind `ManuallyDrop`, whose
/// `DerefMut` makes the method call transparent.
fn kani_target_call_expr(
    target: &super::state_ctor::KaniTarget,
    handler: &ParsedHandler,
) -> String {
    let args: Vec<&str> = handler
        .takes_params
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    format!("state.{}({})", target.method, args.join(", "))
}

/// Emit the `let ok: bool = …;` effect-call binding: generated from
/// `pragma kani_target` when the handler's real logic is a state-struct
/// method, agent-fill `todo!()` otherwise. The target's `kind` segment maps
/// the return shape to the success gate (`result` → `.is_ok()`, `bool` →
/// direct, `unit` → call-then-true).
fn emit_effect_call_binding(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    site: EffectCallSite,
) {
    match super::state_ctor::kani_target_of(spec, &handler.name) {
        Some(target) => {
            out.push_str(
                "\n    // Effect call generated from `pragma kani_target` — the real\n\
                 \x20   // state-struct method; no agent-fill.\n",
            );
            let call = kani_target_call_expr(&target, handler);
            match target.kind {
                super::state_ctor::KaniTargetKind::Result => {
                    out.push_str(&format!("    let ok: bool = {call}.is_ok();\n"));
                }
                super::state_ctor::KaniTargetKind::Bool => {
                    out.push_str(&format!("    let ok: bool = {call};\n"));
                }
                super::state_ctor::KaniTargetKind::Unit => {
                    out.push_str(&format!("    {call};\n    let ok: bool = true;\n"));
                }
            }
        }
        None => match site {
            EffectCallSite::Ensures => {
                out.push_str(
                    "\n    // AGENT-FILL (2/2): apply the real handler's state effect on `state`\n",
                );
                out.push_str(
                    "    // (call the real logic, or replicate the short mutation), then gate on\n",
                );
                out.push_str(
                    "    // the validity check the handler runs (e.g. `state.invariant()?`). Bind\n",
                );
                out.push_str("    // whether it succeeded to `ok`.\n");
                out.push_str(
                    "    let ok: bool = todo!(\"apply effect + validity gate → success?\");\n",
                );
            }
            EffectCallSite::Reject => {
                out.push_str(
                    "\n    // AGENT-FILL: call the real handler (same call as the ensures harness); bind\n",
                );
                out.push_str("    // whether it succeeded to `ok`.\n");
                out.push_str(
                    "    let ok: bool = todo!(\"apply the real handler call → success?\");\n",
                );
            }
        },
    }
}

/// Walk `handler.calls` and, for each CPI whose callee declares ensures,
/// emit a `// CPI ensures-as-fact (Iface.handler):` comment followed by one
/// `kani::assume(<substituted_clause>);` per ensures clause. Tier-0 callees
/// (empty ensures) emit nothing — same fallback as the spec-model harness
/// in `kani.rs` and `lean_gen.rs::render_cpi_theorems`'s `:= by sorry`.
///
/// Substitution reuses `crate::cpi_substitute::substitute_callee_ensures_tree`
/// — the same helper the spec-model harness uses, so the two backends
/// agree on the `let X = call ...` `result` convention and structural
/// param splicing. After rendering we apply `rewrite_pre_post_paths`
/// (same transformation step the caller's own `assert!` emission uses)
/// to flatten `pre.X` / `post.X` paths to the harness-local
/// `pre_X` / `post_X` snapshots.
///
/// **Track J breadcrumb**: when `check::multi_cpi_shared_fields` reports any
/// shared `pre.X` / `post.X` reference across two callees of this handler,
/// emit a WARNING comment above the assume block. The lint
/// `multi_cpi_same_field` carries the structured guidance; this is a
/// reader-of-generated-code breadcrumb so the harness itself flags the
/// over-constraint risk without the user needing to cross-reference the
/// lint output.
fn emit_cpi_ensures_as_assume(out: &mut String, handler: &ParsedHandler, spec: &ParsedSpec) {
    // Track J — emit the breadcrumb once, above the entire CPI assume block,
    // when the lint predicate fires for this handler.
    let shared = check::multi_cpi_shared_fields(spec, handler);
    if !shared.is_empty() {
        out.push_str("        // WARNING: multi-CPI ordering — this handler has ≥2 calls whose\n");
        out.push_str(
            "        // ensures reference the same caller-state field. Both kani::assume\n",
        );
        out.push_str("        // lines fire at the same splice point against one (pre, post)\n");
        out.push_str("        // snapshot pair, which may over-constrain. See lint\n");
        out.push_str("        // `multi_cpi_same_field` for context.\n");
    }
    for call in &handler.calls {
        let Some(iface) = spec
            .interfaces
            .iter()
            .find(|i| i.name == call.target_interface)
        else {
            continue;
        };
        let Some(callee) = iface
            .handlers
            .iter()
            .find(|h| h.name == call.target_handler)
        else {
            continue;
        };
        if callee.ensures.is_empty() {
            // Tier-0 callee — `cpi_no_callee_ensures` lint surfaces the gap.
            continue;
        }
        out.push_str(&format!(
            "        // CPI ensures-as-fact ({}.{}):\n",
            call.target_interface, call.target_handler,
        ));
        for callee_ens in &callee.ensures {
            let ensures_tree = callee_ens.tree.as_ref().expect(
                "interface ensures tree is always populated by the chumsky adapter (#151/#156)",
            );
            let substituted = crate::rust_codegen_util::tree_render::render_rust(
                &crate::cpi_substitute::substitute_callee_ensures_tree(
                    ensures_tree,
                    call,
                    // v2.26 Track K — propagate the declared return-binder
                    // name. `None` keeps the literal "result" convention.
                    callee.result_binder.as_deref(),
                ),
                crate::rust_codegen_util::tree_render::RustCx::native(),
            );
            let lowered = rewrite_pre_post_paths(&substituted);
            out.push_str(&format!("        kani::assume({});\n", lowered));
        }
    }
}

/// Find the handler's writable state account by name. v2.26 Slice 1 uses a
/// simple heuristic: the unique writable non-program, non-signer, non-token,
/// non-mint account. Matches the integration_test scaffolding convention
/// (the program's state PDA is the canonical "state" account; signers /
/// mints / token accounts are separate). Returns `None` when the heuristic
/// can't pick a unique state account — the harness then emits per-field
/// `todo!()` snapshot placeholders for the agent to resolve.
pub(crate) fn find_state_account_name(handler: &ParsedHandler) -> Option<&str> {
    let candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| {
            a.is_writable
                && !a.is_program
                && !a.is_signer
                && a.account_type.as_deref() != Some("token")
                && a.account_type.as_deref() != Some("mint")
        })
        .collect();
    if candidates.len() == 1 {
        Some(candidates[0].name.as_str())
    } else {
        None
    }
}

/// Every field the harness snapshots across the call: the `modifies` set,
/// effect-LHS bare names, v2.27 Track A state-binder caller fields, PLUS
/// every read-only field named by the precondition (`guard_predot`, in
/// `pre.<field>` form) and this postcondition (`ensures.rust_expr_binary`,
/// in `pre.<field>` / `post.<field>` form). Used to drive snapshot emission.
///
/// Track A: when a `call X.y(state_binders { from_balance = state.X })`
/// is present, the CPI assume splice references `pre.X` / `post.X`
/// (the substitution rewrote `pre.from_balance` → `pre.X`). The
/// `rewrite_pre_post_paths` flatten then turns those into `pre_X` /
/// `post_X` locals — which only exist if the snapshot emitter
/// captured `X`. Including binder caller fields here closes that loop.
///
/// The requires/ensures scan closes the same loop for plain read-only
/// fields: `threshold <= num_voters` names `num_voters`, which is never
/// written, so it is absent from `modifies`/effects but still referenced by
/// the emitted `assume`/`assert`. Extracting from the rendered strings the
/// assume/assert actually emit guarantees every named local is declared.
fn collect_snapshot_fields(
    handler: &ParsedHandler,
    guard_predot: Option<&str>,
    ensures: &crate::check::ParsedEnsures,
) -> Vec<String> {
    let split = collect_snapshot_fields_split(handler, guard_predot, ensures);
    let mut all: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    all.extend(split.pre);
    all.extend(split.post);
    all.into_iter().collect()
}

/// Snapshot field sets, split by which side (`pre_` / `post_`) actually reads
/// them. A field read *only* via `post.<x>` needs no `pre_<x>` clone, and vice
/// versa. This matters when the field is a non-`Copy` `Vec` (e.g. a nested
/// `Vec<Hook>` policy field): the dead-side clone deep-copies + drops the whole
/// container, which multiplies CBMC's VCC count and can push a nested-container
/// harness over the SAT/SMT resource wall. Fields that participate in the
/// effect (`modifies` ∪ `effects` ∪ CPI binders) stay on *both* sides — the
/// ensures compares their post value against the `pre_` snapshot.
pub(crate) struct SplitSnapshotFields {
    pub(crate) pre: Vec<String>,
    pub(crate) post: Vec<String>,
}

pub(crate) fn collect_snapshot_fields_split(
    handler: &ParsedHandler,
    guard_predot: Option<&str>,
    ensures: &crate::check::ParsedEnsures,
) -> SplitSnapshotFields {
    // Effect-participating fields: the ensures reads both `pre.` and `post.`.
    let mut both: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(modifies) = &handler.modifies {
        for f in modifies {
            both.insert(f.clone());
        }
    }
    for eff in &handler.effects {
        let bare = crate::rust_codegen_util::effect_target_base(&eff.field);
        both.insert(bare.to_string());
    }
    // v2.27 Track A — caller-side fields bound by a CPI `state_binder` are read
    // by the CPI assume splice on both sides; keep them in `both`.
    for call in &handler.calls {
        for binder in &call.state_binders {
            both.insert(binder.caller_field.clone());
        }
    }
    // Pure read-only reads: the precondition and `old(...)` reads take `pre.`;
    // the plain postcondition reads take `post.`. A field read on exactly one
    // side (not effect-participating) is snapshotted on that side only.
    let mut pre_only: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut post_only: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(guard) = guard_predot {
        collect_prefixed_fields(guard, "pre.", &mut pre_only);
    }
    collect_prefixed_fields(&ensures.rust_expr_binary, "pre.", &mut pre_only);
    collect_prefixed_fields(&ensures.rust_expr_binary, "post.", &mut post_only);

    let mut pre = both.clone();
    pre.extend(pre_only);
    let mut post = both;
    post.extend(post_only);
    SplitSnapshotFields {
        pre: pre.into_iter().collect(),
        post: post.into_iter().collect(),
    }
}

/// True for bytes that may appear inside a Rust identifier.
fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

/// Rewrite the pure-model state accessor `s.<field>` (as produced by
/// `translate_guard_to_rust`) into `pre.<field>`, so a `requires` lowered for
/// the impl harness reads the PRE-call snapshot. Token-aware: only a
/// standalone `s` (at an identifier boundary) immediately followed by `.` is
/// rewritten, so `accounts.`/`is_signer` and the like are untouched. Guard
/// expressions are ASCII.
pub(crate) fn rewrite_state_var_to_pre(expr: &str) -> String {
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 8);
    let mut i = 0;
    while i < bytes.len() {
        let at_boundary = i == 0 || !is_ident_byte(bytes[i - 1]);
        if bytes[i] == b's' && at_boundary && i + 1 < bytes.len() && bytes[i + 1] == b'.' {
            out.push_str("pre");
        } else {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out
}

/// Collect the bare identifiers appearing as `<prefix><ident>` in `expr`
/// (e.g. prefix `"post."` in `post.num_voters` → `num_voters`). Boundary-aware
/// so a prefix embedded in a longer token is not matched.
fn collect_prefixed_fields(expr: &str, prefix: &str, out: &mut std::collections::BTreeSet<String>) {
    let bytes = expr.as_bytes();
    let mut search = 0;
    while let Some(rel) = expr[search..].find(prefix) {
        let start = search + rel;
        let after = start + prefix.len();
        let at_boundary = start == 0 || !is_ident_byte(bytes[start - 1]);
        let mut j = after;
        while j < bytes.len() && is_ident_byte(bytes[j]) {
            j += 1;
        }
        if at_boundary && j > after {
            out.insert(expr[after..j].to_string());
        }
        search = after;
    }
}

/// Rewrite `pre.<field>` → `pre_<field>` and `post.<field>` → `post_<field>`
/// in the rendered ensures expression. The chumsky_adapter renders
/// `state.x` / `old(state.x)` into exactly `post.x` / `pre.x` in the
/// binary-mode form — no other source produces these tokens — so a plain
/// string replace is safe.
pub(crate) fn rewrite_pre_post_paths(expr: &str) -> String {
    expr.replace("pre.", "pre_").replace("post.", "post_")
}

/// Ensures lowering for the `ManuallyDrop` brownfield handler harness. `pre.X`
/// stays its pre-effect snapshot local (`pre_X`); `post.X` reads the mutated
/// state field DIRECTLY (`state.X`, a place behind `ManuallyDrop`'s `Deref`),
/// matched by reference. No owned `post_X` snapshot is bound, so the symbolic
/// nested container is never moved out of `state` and never dropped — the
/// drop-suppression that keeps CBMC from OOMing on deeply-nested state (R2).
///
/// Also strips the defensive `.clone()` the shared renderer puts on an inner
/// `.iter()` match scrutinee (`match (c.kind).clone() {` → `match &(c.kind) {`).
/// That clone materializes an OWNED enum whose `Vec` payloads regenerate the
/// `drop_in_place`/`RawVec` teardown ManuallyDrop just suppressed — measured as
/// the difference between a harness that TIMES OUT (clone kept) and one that
/// closes in seconds (by-ref). Sound here because a brownfield ensures reads its
/// match binders by reference (`.contains()`/`.iter()`/field access); the
/// clone-form is preserved everywhere else (proptest, greenfield, `requires`
/// guards) for the scalar-payload-binder case (`Custom(s) => s > 0`).
fn rewrite_ensures_post_to_state(expr: &str) -> String {
    expr.replace("pre.", "pre_")
        .replace("post.", "state.")
        .replace("match (", "match &(")
        .replace(").clone() {", ") {")
}

/// True for the DSL `Pubkey` type. It lowers to `[u8; 32]` in the standalone
/// harness (`map_type` Standalone context), so equality against it becomes a
/// 32-byte `memcmp` loop that Kani must unwind past.
fn is_pubkey_type(ty: &str) -> bool {
    ty.trim() == "Pubkey"
}

/// Bare names of every `Pubkey`-typed state field across the spec's state
/// shapes (flat `state_fields`, per-account types, and record / sum payloads).
/// A snapshotted field in this set lowers to a `[u8; 32]` snapshot that the
/// ensures may compare.
fn pubkey_state_field_names(spec: &ParsedSpec) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut scan = |fields: &[(String, String)]| {
        for (name, ty) in fields {
            if is_pubkey_type(ty) {
                names.insert(name.clone());
            }
        }
    };
    scan(&spec.state_fields);
    for acct in &spec.account_types {
        scan(&acct.fields);
    }
    for rec in &spec.records {
        scan(&rec.fields);
    }
    for sum in &spec.sum_types {
        for variant in &sum.variants {
            scan(&variant.fields);
        }
    }
    names
}

/// Suggest the `#[kani::unwind(N)]` bound for one impl harness. A `Pubkey`
/// (→ `[u8; 32]`) field or param lowers to a 32-byte `memcmp` loop that Kani
/// only fully unwinds at N ≥ 34 (32 bytes + slack); a harness that snapshots
/// or takes no byte-array value closes at a small bound and runs faster.
/// Returns `(bound, reason)` — `reason` becomes the trailing `//` comment.
///
/// Shared by both struct-framework (Anchor / Quasar) and brownfield emit
/// paths. Pinocchio computes its own bound in `pinocchio.rs`.
fn suggested_unwind(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    // Only the brownfield path emits the Pubkey `==` stub, so only it may drop
    // the memcmp-driven ≥34 bound. The greenfield path passes `false`.
    abstract_pubkey: bool,
) -> (u32, &'static str) {
    // Impl-targeted harnesses CALL real code (the handler / `invariant()` /
    // helper), which operates on the WHOLE account struct — not just the fields
    // this harness snapshots. So a byte-token anywhere in the model — a param,
    // or ANY state field (snapshotted or not) — signals the callee likely does
    // a byte-array `memcmp` (owner / `has_one` / dedup / `windows` checks),
    // which only fully unwinds at N ≥ width + 2. Bias conservative: a too-low
    // bound fails with an "unwinding assertion" (the exact trial-and-error F2
    // removes), whereas a too-high bound is merely slower.
    //
    // #182 Tier 1 covers `Pubkey` (a NEWTYPE, so its derived `==` is a
    // stubbable named impl). `Bytes32`/`Bytes64` (#191) lower to raw
    // `[u8; N]`, whose `PartialEq` is a generic core impl Kani cannot stub
    // (model-checking/kani#1997) — so a Bytes field keeps the memcmp floor
    // even when the Pubkey abstraction is active.
    let (base, why) = if spec_mentions_type(spec, handler, "Bytes64") {
        (
            66u32,
            "Bytes64 in state/params → raw [u8; 64] memcmp (unstubbable, kani#1997); needs ≥ 66",
        )
    } else if spec_mentions_type(spec, handler, "Bytes32") {
        (
            34,
            "Bytes32 in state/params → raw [u8; 32] memcmp (unstubbable, kani#1997); needs ≥ 34",
        )
    } else if abstract_pubkey {
        // #182 Tier 1: when Pubkey `==` is abstracted (stubbed to a
        // wide-integer compare), the 32-byte memcmp that forced ≥34 is gone —
        // the remaining loops iterate `kani_vec_bound`-sized collections.
        (
            super::state_ctor::vec_bound_of(spec) as u32 + 4,
            "Pubkey `==` abstracted (#182) — no memcmp; small bound",
        )
    } else if handler.takes_params.iter().any(|(_, t)| is_pubkey_type(t))
        || !pubkey_state_field_names(spec).is_empty()
    {
        (
            34,
            "Pubkey in state/params → callee does a 32-byte memcmp; needs ≥ 34",
        )
    } else {
        (4, "no byte-token fields — no memcmp")
    };

    // #189: the PDA / hash / secp256k1 uninterpreted-function stubs scan a
    // CAP=8 memo table per call — the harness bound must cover that loop.
    let uses_ufmap_stub = super::state_ctor::wants_pda_abstraction(spec)
        || super::state_ctor::wants_hash_stub(spec)
        || super::state_ctor::wants_secp256k1_stub(spec);
    if uses_ufmap_stub && base < 10 {
        return (10, "UfMap memo scan (CAP 8) in a #189 stub; needs ≥ 10");
    }
    (base, why)
}

/// True when any state field, record / sum payload, or handler param mentions
/// the given DSL type name (word-boundary match, so `Option Bytes32` /
/// `Vec Bytes64` count). The byte-token analogue of the `Pubkey` scans above.
fn spec_mentions_type(spec: &ParsedSpec, handler: &ParsedHandler, type_name: &str) -> bool {
    let mentions = |t: &str| {
        t.split(|c: char| !c.is_alphanumeric())
            .any(|w| w == type_name)
    };
    spec.state_fields.iter().any(|(_, t)| mentions(t))
        || spec
            .account_types
            .iter()
            .any(|a| a.fields.iter().any(|(_, t)| mentions(t)))
        || spec
            .records
            .iter()
            .any(|r| r.fields.iter().any(|(_, t)| mentions(t)))
        || spec.sum_types.iter().any(|s| {
            s.variants
                .iter()
                .any(|v| v.fields.iter().any(|(_, t)| mentions(t)))
        })
        || handler.takes_params.iter().any(|(_, t)| mentions(t))
}
