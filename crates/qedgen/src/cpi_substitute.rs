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

use crate::check::{ParsedCall, ParsedCallArg, ParsedStateBinder};

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
    let after_params = substitute_word_boundary(callee_ensures_lean, &subst);
    // v2.27 Track A — state-binder substitution. The callee's Lean
    // ensures references abstract fields as `<callee_field> pre` /
    // `<callee_field> post` (function application against accessor
    // params). At the caller site we lower each application to a
    // direct field projection on the caller's State snapshot:
    //   `<callee_field> pre`  → `pre.<caller_field>`
    //   `<callee_field> post` → `post.<caller_field>`
    // The Lean axiom signature still carries `<callee_field> : State →
    // Nat` as an accessor param; the axiom-application path (in
    // `render_cpi_theorems`) passes `(·.<caller_field>)` for that
    // slot, so β-reduction produces exactly the substituted form
    // above. The text-level substitution here is what the caller's
    // theorem *statement* reads.
    substitute_state_binders_lean(&after_params, &call.state_binders)
}

/// v2.27 Track A — apply the Lean state-binder substitution. The
/// callee's `ensures` lean_expr lowers `state.X` as `s'.X` (post,
/// `Ctx::Ensures`) and `old(state.X)` as `s.X` (pre, the
/// `path_or_expr_to_lean_old` branch). Each binder `(callee_field,
/// caller_field)` rewrites:
///   `s'.<callee_field>` → `post.<caller_field>`
///   `s.<callee_field>`  → `pre.<caller_field>`
///
/// The caller's theorem statement carries `(pre post : State)` as
/// binders, and the axiom is applied with `(·.<caller_field>)` for
/// each accessor slot — β-reduction matches the substituted text
/// produced here, so the application typechecks against the
/// substituted statement.
fn substitute_state_binders_lean(expr: &str, binders: &[ParsedStateBinder]) -> String {
    let mut out = expr.to_string();
    for b in binders {
        // Post-state: `s'.<callee_field>` → `post.<caller_field>`.
        // The `s'.` token is unambiguous in the Lean ensures lowering
        // (only `Ctx::Ensures` produces it), so a literal replace is
        // safe — no need for full regex word-boundary matching.
        let post_needle = format!("s'.{}", b.callee_field);
        let post_replacement = format!("post.{}", b.caller_field);
        out = replace_word(&out, &post_needle, &post_replacement);
        // Pre-state: `s.<callee_field>` → `pre.<caller_field>`.
        let pre_needle = format!("s.{}", b.callee_field);
        let pre_replacement = format!("pre.{}", b.caller_field);
        out = replace_word(&out, &pre_needle, &pre_replacement);
    }
    out
}

/// Replace `needle` in `haystack` only at word boundaries. Used by
/// the v2.27 Track A Lean state-binder substitution so a longer
/// callee field name (`from_balance_total`) doesn't accidentally match
/// a prefix (`from_balance`).
fn replace_word(haystack: &str, needle: &str, replacement: &str) -> String {
    let pattern = format!(r"{}\b", regex::escape(needle));
    let re = regex::Regex::new(&pattern).expect("regex compiles for word-boundary state replace");
    re.replace_all(haystack, regex::NoExpand(replacement))
        .into_owned()
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
    let after_params = substitute_word_boundary(callee_ensures_rust_binary, &subst);
    // v2.27 Track A — state-binder substitution. The callee's
    // `rust_expr_binary` ensures references abstract fields through
    // the `pre.<callee_field>` / `post.<callee_field>` projection
    // convention. Rewrite each occurrence to the caller's field name
    // so the substituted form composes with `rewrite_pre_post_paths`
    // (kani_impl.rs) — that helper then flattens to the harness-local
    // `pre_<caller_field>` / `post_<caller_field>` snapshots.
    substitute_state_binders_rust_binary(&after_params, &call.state_binders)
}

/// Scan a Rust binary-form `ensures` expression for abstract State-field
/// projections (`pre.X` / `post.X`), returning field names in first occurrence
/// order.
pub fn scan_rust_abstract_fields(expr: &str) -> Vec<String> {
    scan_prefixed_fields(expr, r"\b(?:pre|post)\.([A-Za-z_][A-Za-z0-9_]*)")
}

/// Scan a Lean-form `ensures` expression for abstract State-field projections
/// (`s.X` / `s'.X`), returning field names in first occurrence order.
pub fn scan_lean_abstract_fields(expr: &str) -> Vec<String> {
    scan_prefixed_fields(expr, r"\bs'?\.([A-Za-z_][A-Za-z0-9_]*)")
}

/// Return the abstract fields from `fields` that are not covered by a
/// `state_binders` entry. Backends use this to avoid importing callee ensures
/// into a caller frame that cannot name the callee's abstract state.
pub fn missing_state_binders(fields: &[String], binders: &[ParsedStateBinder]) -> Vec<String> {
    fields
        .iter()
        .filter(|f| !binders.iter().any(|b| &b.callee_field == *f))
        .cloned()
        .collect()
}

fn scan_prefixed_fields(expr: &str, pattern: &str) -> Vec<String> {
    let re = regex::Regex::new(pattern).expect("regex compiles for abstract-field scan");
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(expr) {
        let field = cap.get(1).unwrap().as_str().to_string();
        if seen.insert(field.clone()) {
            out.push(field);
        }
    }
    out
}

/// v2.27 Track A — apply the Rust binary state-binder substitution.
/// Each binder `(callee_field, caller_field)` rewrites
/// `pre.<callee_field>` → `pre.<caller_field>` and
/// `post.<callee_field>` → `post.<caller_field>`. The trailing
/// `rewrite_pre_post_paths` step (caller of this helper, in
/// kani_impl.rs) is what produces the harness-local `pre_X` /
/// `post_X` form.
fn substitute_state_binders_rust_binary(expr: &str, binders: &[ParsedStateBinder]) -> String {
    let mut out = expr.to_string();
    for b in binders {
        for state_kw in ["pre", "post"] {
            // Match `pre.<callee_field>` or `post.<callee_field>` —
            // anchor the `<state>.` prefix so a substring match like
            // `nested.from_balance` doesn't fire spuriously.
            let pattern = format!(r"\b{}\.{}\b", state_kw, regex::escape(&b.callee_field));
            let re = regex::Regex::new(&pattern)
                .expect("regex compiles for Rust state-binder substitution");
            let replacement = format!("{}.{}", state_kw, b.caller_field);
            out = re
                .replace_all(&out, regex::NoExpand(replacement.as_str()))
                .into_owned();
        }
    }
    out
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
            state_binders: Vec::new(),
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
    fn substitute_lean_with_state_binders() {
        // v2.27 Track A — state_binders rewrites `<callee_field> pre`
        // → `pre.<caller_field>` and similarly for `post`.
        let mut call = mk_call("Token", "transfer", &[("amount", "amount")]);
        call.state_binders = vec![
            ParsedStateBinder {
                callee_field: "from_balance".to_string(),
                caller_field: "pool_balance".to_string(),
            },
            ParsedStateBinder {
                callee_field: "to_balance".to_string(),
                caller_field: "user_balance".to_string(),
            },
        ];
        let params = vec![("amount".to_string(), "U64".to_string())];
        // Lean lowering form: `state.X` → `s'.X` (Ctx::Ensures) and
        // `old(state.X)` → `s.X`. The state_binders substitution targets
        // those exact patterns and rewrites them to the caller's State
        // field projection.
        let out = substitute_callee_ensures_lean(
            "s'.from_balance + amount = s.from_balance \u{2227} s'.to_balance = s.to_balance + amount",
            &call,
            &params,
            None,
        );
        assert_eq!(
            out,
            "post.pool_balance + amount = pre.pool_balance \u{2227} post.user_balance = pre.user_balance + amount"
        );
    }

    #[test]
    fn substitute_rust_binary_with_state_binders() {
        // v2.27 Track A — the Rust binary form references abstract
        // fields as `pre.<callee_field>` / `post.<callee_field>`; the
        // binder substitution rewrites the field name in place. The
        // downstream `rewrite_pre_post_paths` (in kani_impl.rs) does
        // the final flatten to `pre_<caller_field>` / `post_<caller_field>`.
        let mut call = mk_call("Token", "transfer", &[("amount", "amount")]);
        call.state_binders = vec![
            ParsedStateBinder {
                callee_field: "from_balance".to_string(),
                caller_field: "pool_balance".to_string(),
            },
            ParsedStateBinder {
                callee_field: "to_balance".to_string(),
                caller_field: "user_balance".to_string(),
            },
        ];
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out = substitute_callee_ensures_rust_binary(
            "post.from_balance + amount == pre.from_balance && post.to_balance == pre.to_balance + amount",
            &call,
            &params,
            None,
        );
        assert_eq!(
            out,
            "post.pool_balance + amount == pre.pool_balance && post.user_balance == pre.user_balance + amount"
        );
    }

    #[test]
    fn scans_and_reports_missing_state_binders() {
        let fields = scan_rust_abstract_fields(
            "post.from_balance + amount == pre.from_balance && post.to_balance == pre.to_balance + amount",
        );
        assert_eq!(fields, vec!["from_balance", "to_balance"]);

        let missing_all = missing_state_binders(&fields, &[]);
        assert_eq!(missing_all, fields);

        let missing_partial = missing_state_binders(
            &fields,
            &[ParsedStateBinder {
                callee_field: "from_balance".into(),
                caller_field: "pool_balance".into(),
            }],
        );
        assert_eq!(missing_partial, vec!["to_balance"]);
    }

    #[test]
    fn scans_lean_abstract_fields() {
        let fields = scan_lean_abstract_fields(
            "s'.from_balance + amount = s.from_balance \u{2227} s'.to_balance = s.to_balance + amount",
        );
        assert_eq!(fields, vec!["from_balance", "to_balance"]);
    }

    #[test]
    fn binders_do_not_affect_param_only_ensures() {
        // v2.27 back-compat: a callee ensures that only references
        // params (no abstract State fields) passes through unchanged
        // even when binders are declared. This is the v2.26-and-prior
        // shape (e.g., the bundled SPL Token interface's `amount > 0`).
        let mut call = mk_call("Token", "transfer", &[("amount", "amount")]);
        call.state_binders = vec![ParsedStateBinder {
            callee_field: "from_balance".to_string(),
            caller_field: "pool_balance".to_string(),
        }];
        let params = vec![("amount".to_string(), "U64".to_string())];
        let out_lean = substitute_callee_ensures_lean("amount > 0", &call, &params, None);
        let out_rust = substitute_callee_ensures_rust_binary("amount > 0", &call, &params, None);
        assert_eq!(out_lean, "amount > 0");
        assert_eq!(out_rust, "amount > 0");
    }

    #[test]
    fn empty_binders_match_v226_behaviour() {
        // Explicit empty state_binders list — the substitution helper
        // must produce the exact same output as a v2.26-shape callsite.
        let call = mk_call("Token", "transfer", &[("amount", "amount")]);
        let params = vec![("amount".to_string(), "U64".to_string())];
        let with_binders_lean = substitute_callee_ensures_lean("amount > 0", &call, &params, None);
        let with_binders_rust =
            substitute_callee_ensures_rust_binary("amount > 0", &call, &params, None);
        assert_eq!(with_binders_lean, "amount > 0");
        assert_eq!(with_binders_rust, "amount > 0");
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
