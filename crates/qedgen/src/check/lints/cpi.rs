//! CPI-composition lints: shared-field extraction across multiple calls,
//! Tier-0 missing-ensures, unverified-callee advisories, and shape-only
//! call-site diagnostics.

use super::*;

use regex::Regex;
use std::sync::LazyLock;

/// Extract `pre.<field>` / `post.<field>` references from a
/// `rust_expr_binary`-rendered expression. The binary-mode renderer is the
/// only source of these tokens, so a static regex is sufficient and stable.
/// `pre.X` and `post.X` both normalize to `X` — the Kani impl harness reads
/// both from the same snapshot pair, so either binds the same locals.
pub fn extract_pre_post_field_refs(expr: &str) -> std::collections::BTreeSet<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        // Word-boundary at the start ensures `xpre.foo` doesn't match.
        Regex::new(r"\b(?:pre|post)\.([A-Za-z_][A-Za-z0-9_]*)").expect("static regex")
    });
    let mut fields = std::collections::BTreeSet::new();
    for cap in RE.captures_iter(expr) {
        fields.insert(cap[1].to_string());
    }
    fields
}

/// Per-handler predicate shared by `check.rs` (lint) and `kani_impl.rs`
/// (breadcrumb comment). For each unordered call pair whose callees resolve
/// in `spec.interfaces`, runs the same substitution as
/// `emit_cpi_ensures_as_assume` and reports `pre.X` / `post.X` references
/// appearing in both callees' substituted ensures. Tier-0 callees are
/// silent. Returns `(call_i_label, call_j_label, shared_field)` triples;
/// label format `Iface.handler` mirrors the harness CPI-block comment.
pub fn multi_cpi_shared_fields(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
) -> Vec<(String, String, String)> {
    // Resolve every call's substituted-ensures field set up front. Tier-0
    // / unresolved callees get an empty set and effectively drop out of the
    // pairwise compare.
    let resolved: Vec<(String, std::collections::BTreeSet<String>)> = handler
        .calls
        .iter()
        .map(|call| {
            let label = format!("{}.{}", call.target_interface, call.target_handler);
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                return (label, std::collections::BTreeSet::new());
            };
            let Some(callee) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                return (label, std::collections::BTreeSet::new());
            };
            let mut fields = std::collections::BTreeSet::new();
            for ens in &callee.ensures {
                let substituted = crate::cpi_substitute::substitute_callee_ensures_rust_binary(
                    &ens.rust_expr_binary,
                    call,
                    &callee.params,
                    callee.result_binder.as_deref(),
                );
                fields.extend(extract_pre_post_field_refs(&substituted));
            }
            (label, fields)
        })
        .collect();

    let mut findings = Vec::new();
    for i in 0..resolved.len() {
        if resolved[i].1.is_empty() {
            continue;
        }
        for j in (i + 1)..resolved.len() {
            if resolved[j].1.is_empty() {
                continue;
            }
            if disjoint_token_transfer_resources(&handler.calls[i], &handler.calls[j]) {
                continue;
            }
            // Set intersection ordered by BTreeSet iteration (stable
            // alphabetical for deterministic lint output).
            for field in resolved[i].1.intersection(&resolved[j].1) {
                findings.push((resolved[i].0.clone(), resolved[j].0.clone(), field.clone()));
            }
        }
    }
    findings
}

pub(crate) fn disjoint_token_transfer_resources(left: &ParsedCall, right: &ParsedCall) -> bool {
    fn token_transfer_resources(call: &ParsedCall) -> Option<std::collections::BTreeSet<String>> {
        if call.target_interface != "Token" || call.target_handler != "transfer" {
            return None;
        }

        let mut resources = std::collections::BTreeSet::new();
        for arg_name in ["from", "to"] {
            let arg = call.args.iter().find(|arg| arg.name == arg_name)?;
            resources.insert(arg.rust_expr.trim().to_string());
        }
        Some(resources)
    }

    let Some(left_resources) = token_transfer_resources(left) else {
        return false;
    };
    let Some(right_resources) = token_transfer_resources(right) else {
        return false;
    };
    left_resources.is_disjoint(&right_resources)
}

/// P2 informational lint for the multi-CPI ordering gap; one warning per
/// shared field per call pair.
pub(crate) fn check_multi_cpi_same_field(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        let findings = multi_cpi_shared_fields(spec, handler);
        for (call_i_label, call_j_label, field) in findings {
            warnings.push(CompletenessWarning {
                rule: "multi_cpi_same_field".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{}' makes multiple CPI calls ({} and {}) whose \
                     substituted ensures both reference '{}'. Kani's impl-targeted \
                     harness has only one (pre_{}, post_{}) snapshot pair captured \
                     at handler boundary; both assumes will fire at the same splice \
                     point, which can over-constrain.",
                    handler.name, call_i_label, call_j_label, field, field, field
                ),
                subject: Some(handler.name.clone()),
                fix: "Until per-call snapshot frames land (v3.0), either: (1) \
                      merge the CPI calls into a single helper handler whose \
                      ensures captures the combined effect; (2) tighten each \
                      callee's ensures so they reference disjoint fields; or \
                      (3) split the multi-CPI handler into separate handlers \
                      (one per CPI) so each gets its own (pre, post) snapshot."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// `cpi_no_callee_ensures`: flags a call site whose interface handler has
/// no `ensures` — the caller's Lean proof carries `by sorry` (Tier-0
/// axiomatization) with no post-condition to discharge. Distinct from
/// `shape_only_cpi` (missing interface/handler declarations): this fires
/// on declared handlers that simply have no post-condition shape.
pub(crate) fn check_cpi_no_callee_ensures(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue; // shape_only_cpi handles undeclared interfaces.
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue; // shape_only_cpi handles undeclared handlers.
            };
            if !ih.ensures.is_empty() {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "cpi_no_callee_ensures".to_string(),
                severity: Severity::Info,
                priority: 1,
                message: format!(
                    "handler '{}' calls `{}.{}` — callee has no `ensures` clauses; \
                     caller's Lean theorem carries `by sorry` (Tier-0 axiomatization)",
                    handler.name, call.target_interface, call.target_handler,
                ),
                subject: Some(handler.name.clone()),
                fix: format!(
                    "Add at least one `ensures <expr>` inside `interface {} {{ handler {} {{ ... }} }}`, \
                     or commit to an `upstream {{ binary_hash = ... }}` pin on the interface so the \
                     caller can discharge via the bundled axiom module.",
                    call.target_interface, call.target_handler,
                ),
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* observable post-condition */\n    }}\n  }}",
                    call.target_interface, call.target_handler,
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// `cpi_unverified_callee`: callee has `ensures` but no imported proof
/// package. The caller still gets discharge via the bundled axiom (Stance
/// 1), but the trust anchor is "binary matches a pinned hash" rather than
/// "we have a proof against the callee's spec." Fires on bundled-stdlib
/// builtins (no proofs shipped) and external imports without
/// `<source>/.qed/proofs/<Iface>.lean` + `lakefile.lean`; suppressed when
/// `spec.verified_callees` has the interface. P2 advisory — `qedgen verify
/// --require-verified` escalates.
pub(crate) fn check_cpi_unverified_callee(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    // Only walk imports — in-spec interfaces declared inline by the
    // author aren't "callees" from a composition standpoint; they're
    // contracts the same author is committing to.
    let import_iface_names: std::collections::HashSet<&str> = spec
        .imports
        .iter()
        .map(|i| i.as_name.as_deref().unwrap_or(i.name.as_str()))
        .collect();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            if !import_iface_names.contains(call.target_interface.as_str()) {
                continue;
            }
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue;
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue;
            };
            if ih.ensures.is_empty() {
                // cpi_no_callee_ensures (P1) owns this case.
                continue;
            }
            if spec.verified_callees.contains_key(&iface.name) {
                continue;
            }
            // One warning per (interface, handler) pair — same call
            // site referenced from multiple handlers shouldn't fire N
            // times.
            let key = format!("{}.{}", iface.name, ih.name);
            if !seen.insert(key) {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "cpi_unverified_callee".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "import `{}` is unverified — `{}.{}` discharges via Stance-1 axiom (binary_hash pin) instead of an imported proof",
                    iface.name, iface.name, ih.name,
                ),
                subject: Some(iface.name.clone()),
                fix: format!(
                    "Ship a Lake-buildable proof package alongside the provider's qedspec at \
                     `<source>/.qed/proofs/{}.lean` (with a sibling `lakefile.lean` declaring \
                     `package {}`). The consumer's codegen will auto-detect the package and \
                     swap the caller's theorem from Stance 1 (axiom) to Stance 2 (imported proof).",
                    iface.name,
                    crate::lean_sidecars::proof_pkg_name(&iface.name),
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// One finding per imported interface that `qedgen verify
/// --require-verified` would reject; carries enough context for main.rs to
/// render a CRIT line and exit non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct UnverifiedCallee {
    pub interface_name: String,
    pub fix_hint: String,
}

/// `qedgen verify --require-verified` predicate. Yields one
/// [`UnverifiedCallee`] per imported interface that: was reached via
/// `import` (not declared inline); has at least one handler with non-empty
/// `ensures` (Tier-0 shape-only imports are exempt — `cpi_no_callee_ensures`
/// covers them); is absent from `spec.verified_callees`; and is NOT
/// sentinel-pinned (`sha256:00…00`). Sentinel-pinned native programs
/// (System) are documented runtime trust boundaries — their `ensures` are
/// discharged by the validator itself, so counting them "unverified" would
/// fail every spec that imports them. Empty vec = dep graph fully proven
/// from a Stance-2 standpoint; mirrors `check_cpi_unverified_callee`.
#[allow(dead_code)]
pub fn collect_require_verified_findings(spec: &ParsedSpec) -> Vec<UnverifiedCallee> {
    let import_iface_names: std::collections::HashSet<&str> = spec
        .imports
        .iter()
        .map(|i| i.as_name.as_deref().unwrap_or(i.name.as_str()))
        .collect();

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for iface in &spec.interfaces {
        if !import_iface_names.contains(iface.name.as_str()) {
            continue;
        }
        let has_ensures = iface.handlers.iter().any(|h| !h.ensures.is_empty());
        if !has_ensures {
            continue;
        }
        if spec.verified_callees.contains_key(&iface.name) {
            continue;
        }
        if iface
            .upstream
            .as_ref()
            .and_then(|u| u.binary_hash.as_deref())
            .map(crate::upstream_check::is_sentinel_hash)
            .unwrap_or(false)
        {
            continue;
        }
        if !seen.insert(iface.name.clone()) {
            continue;
        }
        let proof_pkg = crate::lean_sidecars::proof_pkg_name(&iface.name);
        results.push(UnverifiedCallee {
            interface_name: iface.name.clone(),
            fix_hint: format!(
                "provider must ship `<source>/.qed/proofs/{}.lean` + a sibling `lakefile.lean` \
                 declaring `package {}`. Run without --require-verified to accept Stance-1 \
                 axiom discharge instead.",
                iface.name, proof_pkg
            ),
        });
    }
    results
}

pub(crate) fn check_shape_only_cpi(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    for handler in &spec.handlers {
        for call in &handler.calls {
            let iface = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface);
            let target_handler =
                iface.and_then(|i| i.handlers.iter().find(|h| h.name == call.target_handler));

            let (reason, fix) = match (iface, target_handler) {
                (None, _) => (
                    format!(
                        "interface `{}` is not declared in this spec — the call compiles but has no contract",
                        call.target_interface
                    ),
                    format!(
                        "Declare `interface {} {{ ... }}` at the top level, or `qedgen interface --idl <path>` to scaffold one.",
                        call.target_interface
                    ),
                ),
                (Some(_), None) => (
                    format!(
                        "interface `{}` has no handler named `{}` — check for a typo or add the handler",
                        call.target_interface, call.target_handler
                    ),
                    format!(
                        "Add `handler {}` inside `interface {} {{ ... }}`, or update the call site to match a real handler.",
                        call.target_handler, call.target_interface
                    ),
                ),
                // Declared interface + declared handler: skip, even with no
                // `ensures`. Firing here pressured authors into `ensures
                // true` on shapes with no meaningful post-condition (Token
                // init / metadata-create / close); the import-level Tier
                // 0/1/2 signal already covers it.
                _ => continue,
            };

            warnings.push(CompletenessWarning {
                rule: "shape_only_cpi".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' calls `{}.{}` — {}",
                    handler.name, call.target_interface, call.target_handler, reason
                ),
                subject: Some(handler.name.clone()),
                fix,
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* what the callee guarantees */\n    }}\n  }}",
                    call.target_interface, call.target_handler
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    warnings
}
