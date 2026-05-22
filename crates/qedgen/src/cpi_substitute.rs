//! Param-substitution helper for CPI ensures discharge / propagation.
//!
//! When a handler `H` does `call Iface.foo(args)` and `Iface.foo` declares
//! `ensures <expr>`, two backends need to propagate that contract into the
//! caller's verification:
//!
//! 1. **Lean** (`lean_gen.rs::render_cpi_theorems`) — emits a per-call-site
//!    theorem whose statement is the callee's `ensures` substituted with the
//!    call-site arguments. Tier-1 interfaces close the theorem via the bundled
//!    `Iface.foo.ensures_axiom_<idx>` axiom; Tier-0 emits `:= by sorry`.
//! 2. **Kani** (`kani.rs`, v2.26 Batch 2 Track G — ensures-preservation
//!    harness) — emits `kani::assume(<substituted_rust_binary>)` lines AFTER
//!    the spec-translated transition call and BEFORE the caller's own
//!    `assert!(post_ensures)` lines. The assume turns the callee's contract
//!    into a hypothesis the caller can rely on while verifying its own
//!    ensures.
//!
//! Both backends share the same substitution shape: map each callee
//! parameter name to the caller's expression at the call site, then
//! word-boundary replace. The Lean form uses `ParsedCallArg.lean_expr`;
//! the Kani form uses `ParsedCallArg.rust_expr` (the binary `pre`/`post`
//! split is handled by the callee's `ParsedEnsures.rust_expr_binary` field
//! before it reaches this module).
//!
//! v2.26 Batch 2 Track G extracted this from `lean_gen.rs::substitute_callee_ensures`
//! so `kani.rs` can reuse it without duplicating the substitution logic.

use std::collections::HashMap;

use crate::check::{ParsedCall, ParsedCallArg};

/// Word-boundary regex substitution. Replaces every occurrence of each
/// formal-param identifier in `expr` with its caller-side replacement.
///
/// `\b<param>\b` matching prevents `amount` from clobbering substrings of
/// other identifiers (`amount_squared`, `taker_amount`).
pub fn substitute_word_boundary(expr: &str, subst: &HashMap<&str, &str>) -> String {
    let mut out = expr.to_string();
    for (param, replacement) in subst {
        let pattern = format!(r"\b{}\b", regex::escape(param));
        let re = regex::Regex::new(&pattern).expect("regex compiles for word-boundary param name");
        out = re
            .replace_all(&out, regex::NoExpand(replacement))
            .into_owned();
    }
    out
}

/// Build the param-name → caller-Lean-expr substitution table from a
/// `ParsedCall`. Missing params (callee declared a param the caller
/// didn't bind) fall through unchanged; Lean would surface them as free
/// variables — that's the spec author's bug, not a substitution bug.
pub fn lean_subst_table(call: &ParsedCall) -> HashMap<&str, &str> {
    call.args
        .iter()
        .map(|a: &ParsedCallArg| (a.name.as_str(), a.lean_expr.as_str()))
        .collect()
}

/// Build the param-name → caller-Rust-expr substitution table from a
/// `ParsedCall`. The Rust form is what Kani harnesses and proptest
/// translations consume. Uses `rust_expr` (not `rust_expr_pod`) because
/// the Kani spec-model harness lives in the same target as the
/// transition functions — both render with the same primitive widths.
pub fn rust_subst_table(call: &ParsedCall) -> HashMap<&str, &str> {
    call.args
        .iter()
        .map(|a: &ParsedCallArg| (a.name.as_str(), a.rust_expr.as_str()))
        .collect()
}

/// Substitute the caller's call-site Lean arguments into a callee's
/// Lean-rendered `ensures` expression. The result is the form the
/// caller proves at the call site.
///
/// `callee_params` are the formal parameter names declared on the
/// interface handler. Unmatched params (no corresponding caller arg)
/// keep their formal name, which Lean flags as a free variable; the
/// spec-author lint catches this.
///
/// `callee_result_binder` (v2.26 Track K) is the identifier the callee's
/// `ensures` uses to refer to its own return value, as declared by
/// `handler foo (…) -> <ident> : Type`. When the caller binds the
/// result via `let X = call …`, the identifier is rewritten to `X`.
/// `None` falls back to the conventional `"result"` literal for
/// back-compat with v2.24 #11 specs that don't declare a binder.
pub fn substitute_callee_ensures_lean(
    callee_ensures_lean: &str,
    call: &ParsedCall,
    callee_params: &[(String, String)],
    callee_result_binder: Option<&str>,
) -> String {
    let mut subst: HashMap<&str, &str> = lean_subst_table(call);
    // Defensive: ensure every callee param has *some* entry (default to
    // formal name) so the substitution walks every binder, even when
    // the caller omitted a non-positional arg.
    for (pn, _) in callee_params {
        subst.entry(pn.as_str()).or_insert(pn.as_str());
    }
    // v2.24 #11 `let X = call ...` — bind the caller's result identifier
    // into the substitution table so a callee `ensures` referencing the
    // return-value position resolves to the caller's binder.
    if let Some(ref result_name) = call.result_binding {
        // The binder name on the callee side comes from the interface
        // declaration (`-> <ident> : Type`); `None` defaults to the
        // historical literal `"result"`.
        let binder = callee_result_binder.unwrap_or("result");
        subst.entry(binder).or_insert(result_name.as_str());
    }
    substitute_word_boundary(callee_ensures_lean, &subst)
}

/// Substitute the caller's call-site Rust arguments into a callee's
/// `rust_expr_binary`-rendered `ensures` expression. Used by the
/// v2.26 Batch 2 ensures-preservation Kani harness to propagate
/// callee contracts as `kani::assume` facts.
///
/// The callee's `rust_expr_binary` form already renders `state.x` as
/// `post.x` and `old(state.x)` as `pre.x`. For interface handlers,
/// callees typically don't reference state at all — only their declared
/// `params` — so the substitution is purely a param swap. The
/// `state.x`/`pre.x`/`post.x` forms pass through unchanged and bind to
/// the caller's own pre/post snapshots at the assume site.
///
/// `callee_result_binder` (v2.26 Track K) is the identifier the callee's
/// `ensures` uses to refer to its own return value, as declared by
/// `handler foo (…) -> <ident> : Type`. `None` falls back to the
/// literal `"result"` for back-compat.
pub fn substitute_callee_ensures_rust_binary(
    callee_ensures_rust_binary: &str,
    call: &ParsedCall,
    callee_params: &[(String, String)],
    callee_result_binder: Option<&str>,
) -> String {
    let mut subst: HashMap<&str, &str> = rust_subst_table(call);
    for (pn, _) in callee_params {
        subst.entry(pn.as_str()).or_insert(pn.as_str());
    }
    if let Some(ref result_name) = call.result_binding {
        let binder = callee_result_binder.unwrap_or("result");
        subst.entry(binder).or_insert(result_name.as_str());
    }
    substitute_word_boundary(callee_ensures_rust_binary, &subst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_call(target_iface: &str, target_handler: &str, args: &[(&str, &str)]) -> ParsedCall {
        ParsedCall {
            target_interface: target_iface.to_string(),
            target_handler: target_handler.to_string(),
            args: args
                .iter()
                .map(|(n, expr)| ParsedCallArg {
                    name: n.to_string(),
                    lean_expr: expr.to_string(),
                    rust_expr: expr.to_string(),
                    rust_expr_pod: expr.to_string(),
                })
                .collect(),
            result_binding: None,
        }
    }

    #[test]
    fn word_boundary_does_not_substring_match() {
        let mut subst = HashMap::new();
        subst.insert("amount", "x");
        // `amount_squared` must NOT be touched
        let out = substitute_word_boundary("amount + amount_squared", &subst);
        assert_eq!(out, "x + amount_squared");
    }

    #[test]
    fn substitute_lean_swaps_param_for_caller_expr() {
        let call = mk_call(
            "Token",
            "transfer",
            &[("amount", "s.taker_amount"), ("from", "taker_ta")],
        );
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out = substitute_callee_ensures_lean("amount > 0", &call, &params, None);
        assert_eq!(out, "s.taker_amount > 0");
    }

    #[test]
    fn substitute_rust_swaps_param_for_caller_expr() {
        let call = mk_call(
            "Token",
            "transfer",
            &[("amount", "amount"), ("from", "taker_ta")],
        );
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out = substitute_callee_ensures_rust_binary("amount > 0", &call, &params, None);
        assert_eq!(out, "amount > 0");
    }

    #[test]
    fn substitute_rust_preserves_post_state_refs() {
        // Even if a future SPL ensures references state, the `post.x`
        // form passes through unchanged.
        let call = mk_call("Foo", "bar", &[("amount", "amount")]);
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out = substitute_callee_ensures_rust_binary(
            "post.balance == pre.balance + amount",
            &call,
            &params,
            None,
        );
        assert_eq!(out, "post.balance == pre.balance + amount");
    }

    #[test]
    fn substitute_rust_let_binding_result() {
        // `let x = call Foo.bar(...)` — the caller's binder participates
        // in the substituted ensures via the conventional `result` name.
        let mut call = mk_call("Foo", "bar", &[("amount", "amount")]);
        call.result_binding = Some("delta".to_string());
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out =
            substitute_callee_ensures_rust_binary("result == amount * 2", &call, &params, None);
        assert_eq!(out, "delta == amount * 2");
    }

    #[test]
    fn substitute_rust_defaults_to_result_when_unspecified() {
        // v2.26 Track K back-compat: `None` for `callee_result_binder`
        // makes the substitution fall back to the literal `"result"`,
        // matching pre-Track-K behavior (and the existing
        // `substitute_rust_let_binding_result` test above).
        let mut call = mk_call("Foo", "bar", &[("amount", "amount")]);
        call.result_binding = Some("out".to_string());
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out = substitute_callee_ensures_rust_binary(
            "result <= amount",
            &call,
            &params,
            None, // no declared binder ⇒ default to literal "result"
        );
        assert_eq!(out, "out <= amount");
    }

    #[test]
    fn substitute_rust_uses_declared_binder_name() {
        // v2.26 Track K — the callee declared `-> price : U64`, so its
        // ensures uses the identifier `price` to refer to the return.
        // The caller's `let X = call …` binder substitutes for `price`
        // (NOT the literal `result`).
        let mut call = mk_call("Oracle", "quote", &[("base", "base"), ("quote", "qmint")]);
        call.result_binding = Some("p".to_string());
        let params = vec![
            ("base".to_string(), "Pubkey".to_string()),
            ("quote".to_string(), "Pubkey".to_string()),
        ];
        // Callee's ensures refers to its return as `price`.
        let out = substitute_callee_ensures_rust_binary(
            "price > 0 && price < u64::MAX",
            &call,
            &params,
            Some("price"),
        );
        // `price` rewrites to `p` (caller's binder), not to `result`.
        assert_eq!(out, "p > 0 && p < u64::MAX");
    }

    #[test]
    fn substitute_lean_uses_declared_binder_name() {
        // Lean counterpart of `substitute_rust_uses_declared_binder_name`.
        let mut call = mk_call("Oracle", "quote", &[("base", "s.base_mint")]);
        call.result_binding = Some("p".to_string());
        let params = vec![("base".to_string(), "Pubkey".to_string())];
        let out = substitute_callee_ensures_lean("price > 0", &call, &params, Some("price"));
        assert_eq!(out, "p > 0");
    }

    #[test]
    fn substitute_keeps_unmatched_params_as_formal_names() {
        // A caller that omits a callee param keeps the formal-param name
        // in the substituted form (rendered as a free variable by Lean /
        // a compile error by Rust — that's the lint's job to surface).
        let call = mk_call("Foo", "bar", &[("amount", "amount")]);
        // Callee declared an extra `recipient` param the caller didn't bind
        let params = vec![
            ("amount".to_string(), "U64".to_string()),
            ("recipient".to_string(), "Pubkey".to_string()),
        ];
        let out = substitute_callee_ensures_rust_binary(
            "amount > 0 && recipient != Pubkey::default()",
            &call,
            &params,
            None,
        );
        // Both params present; only `amount` was bound to a caller arg
        assert_eq!(out, "amount > 0 && recipient != Pubkey::default()");
    }
}
