//! Impl-targeted Kani harness emission (v2.26 Batch 2 — Track H).
//!
//! The v2.25 ensures-preservation harness (`kani.rs`) verifies the spec's
//! own translated transition fn against its declared `ensures` clauses. That
//! catches spec-internal inconsistency — useful, but doesn't validate that
//! the *user's Rust handler* satisfies the contract.
//!
//! This module emits a parallel harness shape that calls the user's REAL
//! Anchor handler against a symbolic `Accounts` context. Pre/post account-
//! field snapshots replace the spec-model `pre = s.clone()`; the assertion
//! body reuses `ParsedEnsures.rust_expr_binary` so the same `pre.x` / `post.x`
//! rendering applies — but `pre`/`post` are now flat `pre_<field>` /
//! `post_<field>` locals reading from account data instead of `State` copies.
//!
//! ## Triggers (opt-in)
//!
//! 1. User passes `--kani-impl` to `qedgen codegen`.
//! 2. Auto-trigger: any handler has `modifies` listing fields not present in
//!    the effect block's LHS (the v2.25 LP-shape signal indicating the impl
//!    is expected to fill those fields via the agent-fill `todo!()` site).
//!    When auto-triggered, the file header carries a comment naming the
//!    triggering handler(s).
//!
//! ## CPI ensures-as-fact (Track I)
//!
//! When the handler does `call Foo.bar(...)` and the callee declares
//! `ensures`, we splice `kani::assume(<callee_ensures, substituted>)` lines
//! between `if result.is_ok()` and the first caller `assert!`. The
//! substitution maps each callee param to the caller's call-site expression
//! via `crate::cpi_substitute::substitute_callee_ensures_rust_binary` — the
//! same helper `kani.rs`'s spec-model harness uses. The substituted clauses
//! come back in `pre.X` / `post.X` form (from `rust_expr_binary`); we then
//! flatten those to the harness-local `pre_X` / `post_X` snapshots via
//! `rewrite_pre_post_paths`.
//!
//! Tier-0 callees (no `ensures` declared) emit nothing — same fallback as
//! the spec-model variant and `lean_gen.rs::render_cpi_theorems`'s
//! `:= by sorry`. The `cpi_no_callee_ensures` lint surfaces the gap at
//! check time.
//!
//! ## Out of scope (v2.26)
//!
//! - Pinocchio / native targets — Anchor only. For non-Anchor specs, the
//!   emission is a clean no-op (no file written) — see the per-handler
//!   gating in `generate_from_spec`.
//! - Quasar target — same shape as Anchor would work, but the v2.26 PRD
//!   scopes it explicitly to Anchor.

use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedSpec};
use crate::codegen::{map_type, to_pascal_case};

/// Predicate: a handler triggers auto-emission of an impl-targeted harness
/// when its `modifies` clause lists at least one field that does NOT appear
/// as the LHS of any effect in its `effect` block. This is the LP-shape
/// signal — the agent-fill `todo!()` site expects the user's Rust impl to
/// satisfy the contract for that field.
///
/// Mirrors the diff logic in `codegen.rs` Phase A so the trigger here and
/// the agent-fill emission there stay in lock step.
pub fn handler_triggers_impl_harness(handler: &ParsedHandler) -> bool {
    let Some(modifies) = &handler.modifies else {
        return false;
    };
    let effect_lhs: std::collections::BTreeSet<String> = handler
        .effects
        .iter()
        .map(|(lhs, _, _)| {
            // Strip array index suffix the same way Phase A does, so
            // `lp_supply[i]` doesn't false-positive against a bare
            // `lp_supply` in `modifies`.
            let bare = crate::rust_codegen_util::effect_target_base(lhs);
            bare.to_string()
        })
        .collect();
    modifies.iter().any(|f| !effect_lhs.contains(f))
}

/// Predicate: any handler in the spec triggers the auto-emission. The CLI
/// consults this before emitting the impl harness file when `--kani-impl`
/// was NOT passed explicitly.
///
/// Two trigger conditions:
///   1. Handler `modifies ⊋ effect.lhs` — the LP-shape signal (Track H).
///   2. Any `ref_impl` carries potentially-overflowing arithmetic over
///      bounded-numeric params (`ref_impl_has_overflow_risk`). Lean
///      proves on unbounded `Nat`/`Int`; Kani is the only verification
///      surface that catches the `u64`/`i64` overflow.
pub fn spec_triggers_impl_harness(spec: &ParsedSpec) -> bool {
    spec.handlers.iter().any(handler_triggers_impl_harness)
        || spec
            .ref_impls
            .iter()
            .any(crate::check::ref_impl_has_overflow_risk)
}

/// Names of handlers whose `modifies ⊋ effect.lhs` causes the auto-trigger.
/// Surfaces in the generated file's header so the user understands why an
/// impl harness appeared without `--kani-impl`.
fn auto_triggered_handlers(spec: &ParsedSpec) -> Vec<&str> {
    spec.handlers
        .iter()
        .filter(|h| handler_triggers_impl_harness(h))
        .map(|h| h.name.as_str())
        .collect()
}

/// Emit `programs/tests/kani_impl.rs` against the user's real Anchor
/// handlers. `explicit_flag` is true when `--kani-impl` was passed; auto-
/// triggered emission stamps a header comment naming the triggering
/// handlers. Non-Anchor specs are a clean no-op (no file written).
///
/// Per-handler emission is gated on the handler having at least one
/// `ensures` clause — without ensures there's nothing to assert.
pub fn generate(spec_path: &Path, output_path: &Path, explicit_flag: bool) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;
    generate_from_spec(&spec, output_path, explicit_flag)
}

/// Same as `generate` but takes a pre-parsed spec. Used by the CLI when
/// it already has a `ParsedSpec` in hand (avoids the second parse).
pub fn generate_from_spec(
    spec: &ParsedSpec,
    output_path: &Path,
    explicit_flag: bool,
) -> Result<()> {
    let auto_handlers = auto_triggered_handlers(spec);

    // Skip emission entirely if neither the explicit flag NOR an auto-
    // trigger applies. Belt-and-suspenders check — the CLI's `want_kani_impl`
    // already gates the call, but keeping the check here lets the regen-drift
    // path call `generate` unconditionally without producing stale files on
    // specs that wouldn't normally emit.
    if !explicit_flag && auto_handlers.is_empty() {
        return Ok(());
    }

    let handlers_with_ensures: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
        .filter(|h| !h.ensures.is_empty())
        .collect();

    // No ensures anywhere → nothing to assert. Auto-trigger could still
    // fire (modifies-only fill without ensures is its own lint), but the
    // harness body asserts ensures specifically; skip the file.
    if handlers_with_ensures.is_empty() {
        return Ok(());
    }

    // Restrict per-handler emission to handlers that BOTH have ensures
    // AND either (a) the explicit flag is on OR (b) the handler itself
    // triggers auto-emission. Without (b), a flag-less invocation with
    // one LP-shape handler in a spec full of other handlers would emit
    // a harness for every handler — noise.
    let emit_targets: Vec<&ParsedHandler> = handlers_with_ensures
        .iter()
        .copied()
        .filter(|h| explicit_flag || handler_triggers_impl_harness(h))
        .collect();

    if emit_targets.is_empty() {
        return Ok(());
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let fp = crate::fingerprint::compute_fingerprint(spec);
    let hash = fp
        .file_hashes
        .get("tests/kani_impl.rs")
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();

    // ── File header ──────────────────────────────────────────────────────
    out.push_str(&crate::banner::banner(None, &hash));
    out.push_str("//\n");
    out.push_str("// Impl-targeted Kani harnesses — call the user's real Anchor handler\n");
    out.push_str("// against a symbolic `Accounts` context and assert the spec's\n");
    out.push_str("// `ensures` clauses against pre/post account-field snapshots.\n");
    out.push_str("//\n");
    out.push_str("// Pairs with `tests/kani.rs` (spec-model harness) — that file checks\n");
    out.push_str("// the spec's effect block satisfies its own ensures; this file checks\n");
    out.push_str("// the user's Rust impl does. A counterexample here blames the impl,\n");
    out.push_str("// not the spec.\n");
    if !explicit_flag {
        out.push_str("//\n");
        out.push_str("// Auto-triggered: the following handlers declare `modifies` fields\n");
        out.push_str("// that are NOT written in their `effect` block (the v2.25 LP-shape\n");
        out.push_str("// signal). The agent-fill `todo!()` site is expected to compute\n");
        out.push_str("// those fields against the spec's ensures; this harness verifies\n");
        out.push_str("// the result.\n");
        for name in &auto_handlers {
            out.push_str(&format!("//   - {}\n", name));
        }
        out.push_str("//\n");
        out.push_str("// Pass `--kani-impl` to `qedgen codegen` to force emission for\n");
        out.push_str("// every handler with `ensures`, regardless of the modifies-diff.\n");
    }
    out.push_str("//\n");
    out.push_str("// To run:  cargo kani --harness <name>   (requires cargo-kani)\n");
    out.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    out.push_str("#![cfg(kani)]\n\n");

    // ── Symbolic-accounts builder module ────────────────────────────────
    //
    // One `build_<handler>()` per emit target. Each builds a fully-symbolic
    // Anchor accounts context: PDA-derived addresses bind to the spec's
    // declared `pda <name> [seeds]`; account-data fields are `kani::any()`.
    //
    // The shape mirrors what `tests/integration_tests.rs` builds at runtime,
    // but with `kani::any()` substituted for concrete keypair init. The
    // user's handler is called via `accounts.handler(<params>)` — same
    // method signature the integration test invokes.
    emit_symbolic_accounts_module(&mut out, spec, &emit_targets)?;

    // ── Per-handler proof harnesses ─────────────────────────────────────
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Impl-targeted ensures-preservation proofs\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    let mut emitted_count = 0;
    for handler in &emit_targets {
        for (idx, ensures) in handler.ensures.iter().enumerate() {
            emit_handler_harness(&mut out, handler, idx, ensures, spec)?;
            emitted_count += 1;
        }
    }

    out.push_str("// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");

    std::fs::write(output_path, &out)?;

    eprintln!(
        "Generated {} impl-targeted Kani harness(es) in {}",
        emitted_count,
        output_path.display()
    );

    Ok(())
}

/// Emit `mod symbolic_accounts { ... }` with one `build_<handler>` ctor per
/// emit target. The body is a `todo!()` skeleton that lists each account
/// field with its derivation rule (PDA seed expression vs `kani::any()`)
/// as inline comments. The agent (or user) replaces the body with the
/// concrete `crate::<HandlerPascal> { ... }` construction.
fn emit_symbolic_accounts_module(
    out: &mut String,
    spec: &ParsedSpec,
    targets: &[&ParsedHandler],
) -> Result<()> {
    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Symbolic Anchor `Accounts` context builders.\n");
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
    out.push_str("    // The user's program crate is the host for this test file. Anchor\n");
    out.push_str("    // re-exports `#[derive(Accounts)]` structs at crate root via\n");
    out.push_str("    // `#[program]`, so the handler's accounts struct resolves via\n");
    out.push_str("    // `crate::<HandlerPascal>`.\n");
    out.push_str("    #![allow(unused_imports, dead_code)]\n");

    for handler in targets {
        emit_symbolic_accounts_ctor(out, handler, spec)?;
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
            emit_account_field_skeleton(out, acct, spec);
        }
        out.push_str("        //\n");
        out.push_str("        // AGENT: assemble the fields above into the concrete\n");
        out.push_str(&format!(
            "        // `crate::{}` struct. The Anchor `#[derive(Accounts)]`\n",
            pascal
        ));
        out.push_str("        // expansion gives the exact field layout.\n");
        out.push_str(&format!("        todo!(\"assemble crate::{}\")\n", pascal));
    }

    out.push_str("    }\n");
    Ok(())
}

/// Emit one commented-out line per account field with its derivation rule.
/// PDA-bound accounts get a `Pubkey::find_program_address` template using
/// the spec's `pda <name> [seeds]` declaration; non-PDA fields default to
/// `kani::any()`; programs use their well-known IDs.
fn emit_account_field_skeleton(
    out: &mut String,
    acct: &crate::check::ParsedHandlerAccount,
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
///      ensures' `rust_expr_binary` reads via `pre.<field>`).
///   3. Declare symbolic params + `kani::assume` the handler's requires.
///   4. Call the user's real handler method.
///   5. On `Ok`, snapshot post-state fields, splice CPI ensures-as-fact
///      `kani::assume` lines for each `call Iface.foo(...)` whose callee
///      declares ensures (Track I), then assert the caller's own ensures.
fn emit_handler_harness(
    out: &mut String,
    handler: &ParsedHandler,
    idx: usize,
    ensures: &crate::check::ParsedEnsures,
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("#[kani::proof]\n");
    out.push_str("#[kani::unwind(2)]\n");
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
    let snapshot_fields = collect_snapshot_fields(handler);
    if !snapshot_fields.is_empty() {
        out.push_str(
            "    // Pre-state snapshot — fields the ensures clause reads via `pre.<x>`.\n",
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
    // it returns Err and the ensures don't fire — vacuous pass).
    if let Some(full_guard) = crate::rust_codegen_util::collect_full_guard(handler, false) {
        out.push_str(&format!("    kani::assume({});\n", full_guard));
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
    // line so the caller's downstream assert! can rely on the CPI's
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

/// Walk `handler.calls` and, for each CPI whose callee declares ensures,
/// emit a `// CPI ensures-as-fact (Iface.handler):` comment followed by one
/// `kani::assume(<substituted_clause>);` per ensures clause. Tier-0 callees
/// (empty ensures) emit nothing — same fallback as the spec-model harness
/// in `kani.rs` and `lean_gen.rs::render_cpi_theorems`'s `:= by sorry`.
///
/// Substitution reuses `crate::cpi_substitute::substitute_callee_ensures_rust_binary`
/// — the same helper the spec-model harness uses, so the two backends
/// agree on the `let X = call ...` `result` convention and word-boundary
/// param matching. After substitution we apply `rewrite_pre_post_paths`
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
            let substituted = crate::cpi_substitute::substitute_callee_ensures_rust_binary(
                &callee_ens.rust_expr_binary,
                call,
                &callee.params,
                // v2.26 Track K — propagate the declared return-binder
                // name. `None` keeps the literal "result" convention.
                callee.result_binder.as_deref(),
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
fn find_state_account_name(handler: &ParsedHandler) -> Option<&str> {
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

/// The union of `modifies` and effect-LHS bare field names — every field
/// the ensures clause might read across the pre/post boundary. Used to
/// drive snapshot emission.
fn collect_snapshot_fields(handler: &ParsedHandler) -> Vec<String> {
    let mut fields: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(modifies) = &handler.modifies {
        for f in modifies {
            fields.insert(f.clone());
        }
    }
    for (lhs, _, _) in &handler.effects {
        let bare = crate::rust_codegen_util::effect_target_base(lhs);
        fields.insert(bare.to_string());
    }
    fields.into_iter().collect()
}

/// Rewrite `pre.<field>` → `pre_<field>` and `post.<field>` → `post_<field>`
/// in the rendered ensures expression. The chumsky_adapter renders
/// `state.x` / `old(state.x)` into exactly `post.x` / `pre.x` in the
/// binary-mode form — no other source produces these tokens — so a plain
/// string replace is safe.
fn rewrite_pre_post_paths(expr: &str) -> String {
    expr.replace("pre.", "pre_").replace("post.", "post_")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter::parse_str;

    /// Auto-trigger fires when a handler has `modifies` listing a field
    /// that's absent from the effect block's LHS set (the LP-deposit
    /// shape).
    #[test]
    fn auto_trigger_fires_on_lp_shape() {
        let src = r#"spec Pool
state { pool_balance : U64, lp_supply : U64 }
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  ensures state.pool_balance == old(state.pool_balance) + amount
  effect {
    pool_balance += amount
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(
            handler_triggers_impl_harness(h),
            "modifies = [pool_balance, lp_supply] but effect only writes pool_balance → trigger",
        );
        assert!(spec_triggers_impl_harness(&spec));
    }

    /// Auto-trigger does NOT fire when modifies matches the effect-LHS
    /// set (no LP-shape gap — the spec's effect block covers every
    /// declared write).
    #[test]
    fn auto_trigger_silent_when_modifies_matches_effects() {
        let src = r#"spec Counter
state { count : U64 }
handler bump (delta : U64) {
  requires delta > 0 else InvalidAmount
  modifies [count]
  ensures state.count == old(state.count) + delta
  effect {
    count += delta
  }
}"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(
            !handler_triggers_impl_harness(h),
            "modifies = [count] = effect LHS = {{count}} → no trigger",
        );
        assert!(!spec_triggers_impl_harness(&spec));
    }

    /// Auto-trigger silent when no `modifies` clause is declared at all.
    /// Bundled examples today take this path.
    #[test]
    fn auto_trigger_silent_without_modifies() {
        let src = r#"spec NoModifies
state { x : U64 }
handler set_x (v : U64) {
  ensures state.x == v
  effect { x := v }
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(!spec_triggers_impl_harness(&spec));
    }

    /// `--kani-impl` flag explicitly forces emission for every handler
    /// with ensures, regardless of the modifies-diff.
    #[test]
    fn explicit_flag_forces_emission_for_handlers_with_ensures() {
        let src = r#"spec ExplicitFlag
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
        let spec = parse_str(src).expect("parse");
        // Auto-trigger silent (no modifies declared).
        assert!(!spec_triggers_impl_harness(&spec));

        let tmp =
            std::env::temp_dir().join(format!("kani_impl_explicit_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ true).expect("generate");
        assert!(tmp.is_file(), "explicit flag must emit the file");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("fn verify_bump_impl_ensures_0()"),
            "explicit flag must emit per-handler harness; got:\n{}",
            body
        );
        assert!(
            body.contains("accounts.handler(delta)"),
            "harness must call the user's real handler; got:\n{}",
            body
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// PDA-derived account addresses bind to the spec-declared seeds
    /// rather than `kani::any()`.
    #[test]
    fn pda_derived_accounts_bind_seed_expressions() {
        let src = r#"spec EscrowLite
state { initializer : Pubkey, amount : U64 }
pda escrow ["escrow", initializer]
handler open (deposit_amount : U64) {
  accounts {
    initializer : signer, writable
    escrow      : writable, pda ["escrow", initializer]
  }
  modifies [amount, initializer]
  ensures state.amount == deposit_amount
  effect { amount := deposit_amount }
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_pda_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(
            body.contains("find_program_address(&[b\"escrow\", initializer.as_ref()]"),
            "PDA derivation must come from the spec's `pda` declaration; got:\n{}",
            body
        );
        assert!(
            body.contains("`initializer`: signer"),
            "signer account must appear in the symbolic builder; got:\n{}",
            body
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// No emit when neither the explicit flag is on NOR any handler
    /// triggers auto-emission.
    #[test]
    fn no_emit_when_neither_flag_nor_auto_trigger() {
        let src = r#"spec Silent
state { x : U64 }
handler bump (delta : U64) {
  ensures state.x == old(state.x) + delta
  effect { x += delta }
}"#;
        let spec = parse_str(src).expect("parse");
        let tmp = std::env::temp_dir().join(format!("kani_impl_silent_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        assert!(
            !tmp.is_file(),
            "no flag + no auto-trigger must skip file emission"
        );
    }

    // ========================================================================
    // v2.26 Batch 2 Track I — CPI ensures-as-fact in impl-targeted harness
    // ========================================================================

    /// A handler with its own `ensures` AND a `call Iface.foo(args)` to an
    /// interface that declares ensures must emit `kani::assume(...)` lines
    /// between `if result.is_ok()` and the first caller `assert!`,
    /// substituting the callee's param names with the caller's call-site
    /// expressions. Mirror of `kani.rs`'s
    /// `cpi_ensures_lowers_to_kani_assume_in_preservation_harness` for the
    /// impl-targeted variant.
    #[test]
    fn cpi_ensures_as_assume_emits_at_splice_point() {
        let src = r#"spec CpiImplTest
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures amount > 0
  }
}

state { pool : U64 }

handler deposit (amt : U64) {
  permissionless
  requires amt > 0 else InvalidAmount
  modifies [pool, lp_supply]
  call Token.transfer(from = 0, to = 0, amount = amt, authority = 0)
  effect { pool += amt }
  ensures state.pool == old(state.pool) + amt
}"#;
        let spec = parse_str(src).expect("parse");
        // The LP-shape diff (modifies = {pool, lp_supply}, effect-LHS = {pool})
        // triggers auto-emission.
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_track_i_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // 1. The splice-marker comment from Track H must be GONE — Track I
        //    replaces it with the actual emission, no stale marker.
        assert!(
            !body.contains("<Track I CPI ensures-as-fact splice point>"),
            "Track H's splice marker must be removed once Track I has emitted; got:\n{}",
            body
        );

        // 2. The CPI ensures-as-fact comment + assume line must be present,
        //    with `amount` substituted to the caller's `amt` expression.
        assert!(
            body.contains("// CPI ensures-as-fact (Token.transfer):"),
            "missing CPI ensures-as-fact comment for Token.transfer; got:\n{}",
            body
        );
        assert!(
            body.contains("kani::assume(amt > 0)"),
            "missing substituted kani::assume(amt > 0); got:\n{}",
            body
        );

        // 3. Ordering: assume must sit between `if result.is_ok()` and the
        //    caller's first `assert!`.
        let is_ok_pos = body
            .find("if result.is_ok()")
            .expect("harness must have `if result.is_ok()`");
        let assume_pos = body
            .find("kani::assume(amt > 0)")
            .expect("assume present (just asserted above)");
        let assert_pos = body[is_ok_pos..]
            .find("assert!")
            .map(|i| is_ok_pos + i)
            .expect("caller's assert! must follow");
        assert!(
            is_ok_pos < assume_pos && assume_pos < assert_pos,
            "CPI assume must sit between is_ok() and assert!; got:\n{}",
            body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.26 Track K — impl-targeted variant of the spec-model
    /// `named_return_binder_substitutes_into_kani_assume` test.
    /// `let p = call Oracle.quote(…)` with `-> price : U64` declared
    /// must rewrite `price` to `p` in the emitted `kani::assume`.
    #[test]
    fn named_return_binder_substitutes_in_impl_harness() {
        let src = r#"spec NamedBinderImpl
program_id "11111111111111111111111111111111"

interface Oracle {
  program_id "11111111111111111111111111111111"
  handler quote (base : U64) -> price : U64 {
    ensures price > 0
  }
}

state { last_price : U64, lp_supply : U64 }

handler refresh (b : U64) {
  permissionless
  modifies [last_price, lp_supply]
  let p = call Oracle.quote(base = b)
  effect { last_price := b }
  ensures state.last_price == b
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_track_k_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            body.contains("// CPI ensures-as-fact (Oracle.quote):"),
            "missing CPI ensures-as-fact comment for Oracle.quote; got:\n{}",
            body,
        );
        // The callee uses `price` as its return binder; the caller's
        // `let p = …` makes `p` the substituted form.
        assert!(
            body.contains("kani::assume(p > 0)"),
            "expected `kani::assume(p > 0)` from named binder substitution; got:\n{}",
            body,
        );
        assert!(
            !body.contains("price > 0"),
            "binder name `price` must be substituted away; got:\n{}",
            body,
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// Tier-0 callees (interface declares no `ensures`) must not emit any
    /// `kani::assume` lines in the impl harness. Mirrors the spec-model
    /// variant's `tier0_callee_emits_no_kani_assume_lines` test.
    #[test]
    fn tier0_callee_emits_no_kani_assume_lines() {
        let src = r#"spec Tier0Impl
program_id "11111111111111111111111111111111"

interface Logger {
  program_id "11111111111111111111111111111111"
  handler log (msg : U64) {
    accounts {
      sink : writable
    }
  }
}

state { counter : U64 }

handler tick (val : U64) {
  permissionless
  requires val > 0 else Bad
  modifies [counter, shadow]
  call Logger.log(msg = val)
  effect { counter += val }
  ensures state.counter == old(state.counter) + val
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_tier0_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            !body.contains("CPI ensures-as-fact (Logger.log)"),
            "Tier-0 callee (no ensures) must not emit any CPI assume block; got:\n{}",
            body
        );
        // Caller's own assert! still emits.
        assert!(
            body.contains("assert!("),
            "caller's own assert! must still emit; got:\n{}",
            body
        );
        // And no `kani::assume(` introduced by Track I — the only assumes
        // that may appear are the caller's own requires-guard assume (none
        // here, since `val > 0` is the requires).
        // (We check by counting: the requires-guard assume is `val > 0`,
        // so a Logger-derived assume would appear separately.)
        let assume_count = body.matches("kani::assume(").count();
        // Exactly one assume — the caller's own requires-guard
        // (`val > 0 else Bad`).
        assert_eq!(
            assume_count, 1,
            "Tier-0 callee must not add any kani::assume lines; got {} assumes in:\n{}",
            assume_count, body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// `let X = call Foo.bar(...)` puts `X` in scope in the substituted
    /// ensures via the `result` convention (v2.24 #11). Mirrors the
    /// spec-model variant's `let_call_binding_participates_in_substitution`
    /// test.
    #[test]
    fn let_binding_participates_in_substitution() {
        let src = r#"spec LetCallImpl
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb (amount : U64) -> U64 {
    accounts {
      vault : writable
    }
    requires amount > 0
    ensures result <= amount
  }
}

state { total_loss : U64 }

handler liquidate (loss : U64) {
  permissionless
  requires loss > 0 else Bad
  modifies [total_loss, shadow]
  let burned = call Pool.absorb(amount = loss)
  effect { total_loss += loss }
  ensures state.total_loss == old(state.total_loss) + loss
}"#;
        let spec = parse_str(src).expect("parse");
        assert!(spec_triggers_impl_harness(&spec));

        let tmp = std::env::temp_dir().join(format!("kani_impl_letcall_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        assert!(
            body.contains("// CPI ensures-as-fact (Pool.absorb):"),
            "missing CPI ensures-as-fact for Pool.absorb; got:\n{}",
            body
        );
        // `result <= amount` substitutes `amount → loss` and
        // `result → burned`.
        assert!(
            body.contains("kani::assume(burned <= loss)"),
            "let-binding result must substitute to caller's binder; got:\n{}",
            body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.26 Track J — when `multi_cpi_shared_fields` fires for a
    /// handler, the impl harness emits a WARNING breadcrumb comment
    /// above the CPI assume block so a reader of the generated file
    /// sees the over-constraint risk without cross-referencing the lint
    /// output. The breadcrumb sits between the post-snapshot and the
    /// first `kani::assume` from any CPI.
    #[test]
    fn multi_cpi_breadcrumb_emits_above_assume_block() {
        let src = r#"spec MultiCpiKaniImpl
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.vault_balance == old(state.vault_balance) - amount
  }
}

state { vault_balance : U64 }

handler split (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  modifies [vault_balance, shadow]
  call Token.transfer(from = 0, to = 1, amount = a, authority = 0)
  call Token.transfer(from = 0, to = 2, amount = b, authority = 0)
  effect { vault_balance -= a }
  ensures state.vault_balance == old(state.vault_balance) - a - b
}"#;
        let spec = parse_str(src).expect("parse");
        // LP-shape gap (modifies = {vault_balance, shadow}, effect-LHS =
        // {vault_balance}) triggers auto-emission.
        assert!(spec_triggers_impl_harness(&spec));

        let tmp =
            std::env::temp_dir().join(format!("kani_impl_multi_cpi_{}.rs", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        generate_from_spec(&spec, &tmp, /*explicit_flag=*/ false).expect("generate");
        let body = std::fs::read_to_string(&tmp).unwrap();

        // 1. Two CPI assume lines must be present (one per call).
        let assume_count = body
            .matches("// CPI ensures-as-fact (Token.transfer):")
            .count();
        assert_eq!(
            assume_count, 2,
            "two CPI assume blocks must emit; got {} in:\n{}",
            assume_count, body
        );

        // 2. The breadcrumb WARNING must appear in the harness body
        //    (above the CPI assume block).
        assert!(
            body.contains("WARNING: multi-CPI ordering"),
            "Track J breadcrumb must emit when multi_cpi_shared_fields fires; got:\n{}",
            body
        );
        assert!(
            body.contains("`multi_cpi_same_field`"),
            "breadcrumb must reference the lint rule name; got:\n{}",
            body
        );

        // 3. Ordering: WARNING sits between the `if result.is_ok()`
        //    branch open and the first `kani::assume` of the CPI block.
        let is_ok_pos = body
            .find("if result.is_ok()")
            .expect("`if result.is_ok()` must be present");
        let warn_pos = body
            .find("WARNING: multi-CPI ordering")
            .expect("breadcrumb present (just asserted)");
        let first_cpi_assume = body[is_ok_pos..]
            .find("// CPI ensures-as-fact")
            .map(|i| is_ok_pos + i)
            .expect("CPI assume block must follow is_ok()");
        assert!(
            is_ok_pos < warn_pos && warn_pos < first_cpi_assume,
            "WARNING breadcrumb must sit between is_ok() and the first \
             CPI ensures-as-fact comment; positions: is_ok={} warn={} cpi={}; got:\n{}",
            is_ok_pos,
            warn_pos,
            first_cpi_assume,
            body
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// v2.26 fold-in — a spec with no LP-shape handler but a ref_impl
    /// that carries potentially-overflowing arithmetic still auto-triggers
    /// the impl-targeted harness. Lean proves on `Nat`; Kani is the only
    /// verification surface that catches the `u64` overflow.
    #[test]
    fn ref_impl_overflow_risk_auto_triggers_impl_harness() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl scaled (a : U64) (b : U64) : U64 = a * b

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
  ensures state.x == scaled(old(state.x), amt)
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        // No handler trips the LP-shape signal (`set` declares no modifies).
        assert!(
            !spec.handlers.iter().any(handler_triggers_impl_harness),
            "no handler should trip the modifies-driven trigger in this fixture"
        );
        // But the ref_impl `scaled` has `*` over U64, so the auto-trigger
        // still fires through the ref_impl overflow-risk predicate.
        assert!(
            spec_triggers_impl_harness(&spec),
            "ref_impl with multiplication over bounded-numeric params \
             must auto-trigger the impl harness"
        );
    }

    /// Symmetric negative: ref_impl with only division (no overflow risk)
    /// AND no LP-shape handler — auto-trigger stays quiet.
    #[test]
    fn ref_impl_without_overflow_risk_does_not_auto_trigger() {
        let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl half (a : U64) : U64 = a / 2

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        assert!(
            !spec_triggers_impl_harness(&spec),
            "ref_impl with only division must not auto-trigger \
             (no overflow risk, nothing for Kani to catch)"
        );
    }
}
