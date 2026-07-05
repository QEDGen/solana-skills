//! Guard-enforcement and abort-condition harnesses: monolithic-or-split
//! `rejects_invalid` proofs (with the `GuardRejectionHarness` carrier, the
//! `mul_bps_floor_u128` call-binding rewrite, and the term slug helper) plus
//! the per-`aborts_if` rejection proofs.

use super::*;

/// Emit `#[kani::proof] fn verify_<handler>_rejects_invalid()` per handler
/// with a guard or `requires` clause. One monolithic harness per handler up
/// to the split threshold, then one harness per guard term:
///   * symbolic state init (+ pre-status assume if lifecycle-gated)
///   * every param + abstract binder as `kani::any()`
///   * `kani::assume(!(full_guard))` — at least one guard component violated
///   * `assert!(!<handler>(&mut s, args...))` — handler must reject
pub(crate) fn emit_guard_enforcement_harnesses(
    out: &mut String,
    parsed: &ParsedSpec,
    progress: bool,
) -> Result<()> {
    use crate::rust_codegen_util as util;

    const GUARD_REJECTION_SPLIT_THRESHOLD: usize = 8;

    // Resolve view — same logic as `emit_account_section_structural`.
    let (state_fields, lifecycle): (&[(String, String)], &[String]) =
        if parsed.account_types.len() == 1 {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        } else if parsed.account_types.is_empty() {
            (
                util::resolve_state_fields(parsed),
                parsed.lifecycle_states.as_slice(),
            )
        } else {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        };
    let mutable = util::field_refs(state_fields);

    let guard_ops: Vec<&crate::check::ParsedHandler> =
        parsed.handlers.iter().filter(|op| op.has_guard()).collect();

    if guard_ops.is_empty() {
        return Ok(());
    }

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Guard enforcement — transitions reject invalid inputs\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in &guard_ops {
        let guard_terms = util::collect_guard_terms_with_account_env(
            op,
            false,
            util::handler_needs_account_env(op).then_some("accounts"),
        );
        if guard_terms.is_empty() {
            // `has_guard()` but no expressible negation — skip to avoid
            // a vacuous `kani::assume(!(true))` harness.
            continue;
        }

        if guard_terms.len() <= GUARD_REJECTION_SPLIT_THRESHOLD {
            let full_guard = util::collect_full_guard_with_account_env(
                op,
                false,
                util::handler_needs_account_env(op).then_some("accounts"),
            )
            .expect("guard terms came from an expressible full guard");
            let full_guard = util::rewrite_kani_pubkey_comparisons(&full_guard, op, parsed);
            let harness_name = format!("verify_{}_rejects_invalid", op.name);
            if progress {
                eprintln!("Rendering Kani guard proof: {harness_name}");
            }
            emit_guard_rejection_harness(
                out,
                GuardRejectionHarness {
                    parsed,
                    op,
                    mutable: &mutable,
                    lifecycle,
                    harness_name: &harness_name,
                    prefix_terms: &[],
                    violated_expr: &full_guard,
                    assert_message: &format!("{} must reject when guard is violated", op.name),
                    direct_negated_assume: false,
                },
            )?;
        } else {
            for (idx, term) in guard_terms.iter().enumerate() {
                let term_expr = util::rewrite_kani_pubkey_comparisons(&term.rust_expr, op, parsed);
                let prefix_terms = guard_terms[..idx]
                    .iter()
                    .map(|prefix| {
                        util::rewrite_kani_pubkey_comparisons(&prefix.rust_expr, op, parsed)
                    })
                    .collect::<Vec<_>>();
                let slug = guard_term_slug(&term_expr);
                let harness_name =
                    format!("verify_{}_rejects_invalid_{}_{}", op.name, idx + 1, slug);
                if progress {
                    eprintln!("Rendering Kani guard proof: {harness_name}");
                }
                emit_guard_rejection_harness(
                    out,
                    GuardRejectionHarness {
                        parsed,
                        op,
                        mutable: &mutable,
                        lifecycle,
                        harness_name: &harness_name,
                        prefix_terms: &prefix_terms,
                        violated_expr: &term_expr,
                        assert_message: &format!(
                            "{} must reject when guard term is violated",
                            op.name
                        ),
                        direct_negated_assume: true,
                    },
                )?;
            }
        }
    }

    Ok(())
}

pub(crate) struct GuardRejectionHarness<'a> {
    pub(crate) parsed: &'a ParsedSpec,
    pub(crate) op: &'a crate::check::ParsedHandler,
    pub(crate) mutable: &'a [&'a (String, String)],
    pub(crate) lifecycle: &'a [String],
    pub(crate) harness_name: &'a str,
    pub(crate) prefix_terms: &'a [String],
    pub(crate) violated_expr: &'a str,
    pub(crate) assert_message: &'a str,
    pub(crate) direct_negated_assume: bool,
}

pub(crate) fn emit_guard_rejection_harness(
    out: &mut String,
    ctx: GuardRejectionHarness<'_>,
) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;
    let parsed = ctx.parsed;
    let op = ctx.op;
    let mutable = ctx.mutable;
    let lifecycle = ctx.lifecycle;
    let harness_name = ctx.harness_name;
    let prefix_terms = ctx.prefix_terms;
    let violated_expr = ctx.violated_expr;
    let assert_message = ctx.assert_message;
    let direct_negated_assume = ctx.direct_negated_assume;

    out.push_str("#[kani::proof]\n");
    out.push_str("#[kani::unwind(2)]\n");
    out.push_str("#[kani::solver(cadical)]\n");
    out.push_str(&format!("fn {harness_name}() {{\n"));

    util::emit_state_init_symbolic(out, mutable, lifecycle);
    util::emit_pre_status_assume(out, op, lifecycle);

    for (pname, ptype) in &op.takes_params {
        out.push_str(&format!(
            "    let {}: {} = kani::any();\n",
            pname,
            map_type(ptype, parsed)?
        ));
    }

    // Deliberate double-emit of abstract binders — bug-for-bug parity with
    // the snapshot-pinned output; cleanup deferred to v3.0.
    util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;
    util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;

    emit_kani_account_env_binding(out, op, "accounts", "    ");
    let mut helper_binding_counter = 0usize;
    for prefix in prefix_terms {
        let (prefix, bindings) = bind_kani_bps_floor_calls(prefix, &mut helper_binding_counter);
        for binding in bindings {
            out.push_str(&format!("    {binding}\n"));
        }
        out.push_str(&format!("    kani::assume({prefix});\n"));
    }
    let (violated_expr, bindings) =
        bind_kani_bps_floor_calls(violated_expr, &mut helper_binding_counter);
    for binding in bindings {
        out.push_str(&format!("    {binding}\n"));
    }
    // Assume the i-th guard component is violated. On the per-term split
    // path the prefix terms were already assumed true, so violating this
    // single term drives the full guard false.
    if direct_negated_assume {
        if let Some(negated) = util::negate_simple_top_level_comparison(&violated_expr) {
            out.push_str(&format!("    kani::assume({negated});\n"));
        } else {
            out.push_str(&format!("    kani::assume(!({violated_expr}));\n"));
        }
    } else {
        out.push_str(&format!("    kani::assume(!({violated_expr}));\n"));
    }

    // Per-arm vacuity guard: when term i is implied by the prefix terms,
    // this arm's assumption set is UNSAT and the assertion below is
    // unreachable — Kani would report SUCCESSFUL while proving nothing for
    // this arm. The cover makes that loud (an unsatisfied cover fails the
    // run) and doubles as a spec signal that the guard term is redundant.
    out.push_str("    kani::cover!(true, \"guard-violation domain is satisfiable\");\n");

    // With the guard violated, the generated transition must reject: it
    // returns `false` and applies none of its effects. Asserting the
    // negated term back (instead of calling the handler) would be a
    // tautology that proves nothing — the handler call is what gives the
    // harness teeth.
    let args = transition_call_args(
        op,
        util::handler_needs_account_env(op).then_some("accounts"),
    );
    out.push_str(&format!("    assert!(!{}(&mut s{}),\n", op.name, args));
    out.push_str(&format!("        \"{assert_message}\");\n"));
    out.push_str("}\n\n");

    Ok(())
}

pub(crate) fn bind_kani_bps_floor_calls(expr: &str, counter: &mut usize) -> (String, Vec<String>) {
    let helper_call = regex::Regex::new(
        r"mul_bps_floor_u128\((?P<a>[A-Za-z_][A-Za-z0-9_\.]*),\s*(?P<b>[A-Za-z_][A-Za-z0-9_\.]*)\)",
    )
    .expect("valid mul_bps_floor_u128 call regex");
    let mut rewritten = String::with_capacity(expr.len());
    let mut bindings = Vec::new();
    let mut cursor = 0usize;

    for captures in helper_call.captures_iter(expr) {
        let Some(call) = captures.get(0) else {
            continue;
        };
        rewritten.push_str(&expr[cursor..call.start()]);
        *counter += 1;
        let binding_name = format!("__qed_bps_floor_{counter}");
        let a = captures.name("a").expect("a capture").as_str();
        let b = captures.name("b").expect("b capture").as_str();
        bindings.push(format!(
            "let {binding_name} = mul_bps_floor_u128({a}, {b});"
        ));
        rewritten.push_str(&binding_name);
        cursor = call.end();
    }

    rewritten.push_str(&expr[cursor..]);
    (rewritten, bindings)
}

pub(crate) fn guard_term_slug(expr: &str) -> String {
    let mut slug = crate::codegen_shared::sanitize_ident(expr)
        .trim_matches('_')
        .to_ascii_lowercase();
    const MAX_SLUG_LEN: usize = 56;
    if slug.len() > MAX_SLUG_LEN {
        slug.truncate(MAX_SLUG_LEN);
        slug = slug.trim_end_matches('_').to_string();
    }
    if slug.is_empty() {
        "term".to_string()
    } else {
        slug
    }
}

/// Emit `#[kani::proof] fn verify_<handler>_aborts_if_<error>()` per
/// (handler, abort clause): symbolic state + pre-status assume + symbolic
/// params + (double-emit) abstract binders, `kani::assume(<abort.rust_expr>)`
/// (the condition that should trigger abortion), then
/// `assert!(!<handler>(...))` — handler must reject.
pub(crate) fn emit_abort_condition_harnesses(out: &mut String, parsed: &ParsedSpec) -> Result<()> {
    use crate::codegen_shared::map_type;
    use crate::rust_codegen_util as util;

    // Resolve view — same logic as `emit_account_section_structural`.
    let (state_fields, lifecycle): (&[(String, String)], &[String]) =
        if parsed.account_types.len() == 1 {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        } else if parsed.account_types.is_empty() {
            (
                util::resolve_state_fields(parsed),
                parsed.lifecycle_states.as_slice(),
            )
        } else {
            (
                &parsed.account_types[0].fields,
                parsed.account_types[0].lifecycle.as_slice(),
            )
        };
    let mutable = util::field_refs(state_fields);

    let abort_ops: Vec<&crate::check::ParsedHandler> = parsed
        .handlers
        .iter()
        .filter(|op| !op.aborts_if.is_empty())
        .collect();

    if abort_ops.is_empty() {
        return Ok(());
    }

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Abort conditions — operations must reject under specified conditions\n");
    out.push_str(
        "// ============================================================================\n\n",
    );

    for op in &abort_ops {
        for abort in &op.aborts_if {
            out.push_str("#[kani::proof]\n");
            out.push_str("#[kani::unwind(2)]\n");
            out.push_str("#[kani::solver(cadical)]\n");
            out.push_str(&format!(
                "fn verify_{}_aborts_if_{}() {{\n",
                op.name, abort.error_name
            ));

            util::emit_state_init_symbolic(out, &mutable, lifecycle);
            util::emit_pre_status_assume(out, op, lifecycle);

            for (pname, ptype) in &op.takes_params {
                out.push_str(&format!(
                    "    let {}: {} = kani::any();\n",
                    pname,
                    map_type(ptype, parsed)?
                ));
            }
            // Deliberate double-emit — see guard-enforcement comment.
            util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;
            util::emit_abstract_binders(out, op, "    ", "kani::any()", |t| map_type(t, parsed))?;

            emit_kani_account_env_binding(out, op, "accounts", "    ");
            let abort_expr =
                util::rewrite_account_pubkey_refs(&abort.rust_expr, &op.accounts, "accounts");
            let abort_expr = util::rewrite_kani_pubkey_comparisons(&abort_expr, op, parsed);
            out.push_str(&format!("    kani::assume({});\n", abort_expr));

            let args = transition_call_args(
                op,
                util::handler_needs_account_env(op).then_some("accounts"),
            );
            out.push_str(&format!("    assert!(!{}(&mut s{}),\n", op.name, args));
            out.push_str(&format!(
                "        \"{} must abort with {}\");\n",
                op.name, abort.error_name
            ));
            out.push_str("}\n\n");
        }
    }

    Ok(())
}
