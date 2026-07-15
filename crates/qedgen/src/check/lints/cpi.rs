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
fn extract_pre_post_field_refs(expr: &str) -> std::collections::BTreeSet<String> {
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
pub(crate) fn multi_cpi_shared_fields(
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
                let ensures_tree = ens.tree.as_ref().expect(
                    "interface ensures tree is always populated by the chumsky adapter (#151/#156)",
                );
                let substituted = crate::rust_codegen_util::tree_render::render_rust(
                    &crate::cpi_substitute::substitute_callee_ensures_tree(
                        ensures_tree,
                        call,
                        callee.result_binder.as_deref(),
                    ),
                    crate::rust_codegen_util::tree_render::RustCx::native(),
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

fn disjoint_token_transfer_resources(left: &ParsedCall, right: &ParsedCall) -> bool {
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
pub(super) fn check_multi_cpi_same_field(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        let findings = multi_cpi_shared_fields(spec, handler);
        for (call_i_label, call_j_label, field) in findings {
            warnings.push(
                warn(
                    "multi_cpi_same_field",
                    Severity::Info,
                    2,
                    format!(
                        "handler '{}' makes multiple CPI calls ({} and {}) whose \
                     substituted ensures both reference '{}'. Kani's impl-targeted \
                     harness has only one (pre_{}, post_{}) snapshot pair captured \
                     at handler boundary; both assumes will fire at the same splice \
                     point, which can over-constrain.",
                        handler.name, call_i_label, call_j_label, field, field, field
                    ),
                )
                .subject(handler.name.clone())
                .fix(
                    "Until per-call snapshot frames land (v3.0), either: (1) \
                      merge the CPI calls into a single helper handler whose \
                      ensures captures the combined effect; (2) tighten each \
                      callee's ensures so they reference disjoint fields; or \
                      (3) split the multi-CPI handler into separate handlers \
                      (one per CPI) so each gets its own (pre, post) snapshot.",
                ),
            );
        }
    }
    warnings
}

/// `cpi_no_callee_ensures`: flags a call site whose interface handler has
/// no `ensures` — the caller's Lean proof carries `by sorry` (Tier-0
/// axiomatization) with no post-condition to discharge. Distinct from
/// `shape_only_cpi` (missing interface/handler declarations): this fires
/// on declared handlers that simply have no post-condition shape.
pub(super) fn check_cpi_no_callee_ensures(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
            warnings.push(warn("cpi_no_callee_ensures", Severity::Info, 1, format!(
                    "handler '{}' calls `{}.{}` — callee has no `ensures` clauses; \
                     caller's Lean theorem carries `by sorry` (Tier-0 axiomatization)",
                    handler.name, call.target_interface, call.target_handler,
                )).subject(handler.name.clone()).fix(format!(
                    "Add at least one `ensures <expr>` inside `interface {} {{ handler {} {{ ... }} }}`, \
                     or commit to an `upstream {{ binary_hash = ... }}` pin on the interface so the \
                     caller can discharge via the bundled axiom module.",
                    call.target_interface, call.target_handler,
                )).example(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* observable post-condition */\n    }}\n  }}",
                    call.target_interface, call.target_handler,
                )));
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
pub(super) fn check_cpi_unverified_callee(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
            warnings.push(warn("cpi_unverified_callee", Severity::Info, 2, format!(
                    "import `{}` is unverified — `{}.{}` discharges via Stance-1 axiom (binary_hash pin) instead of an imported proof",
                    iface.name, iface.name, ih.name,
                )).subject(iface.name.clone()).fix(format!(
                    "Ship a Lake-buildable proof package alongside the provider's qedspec at \
                     `<source>/.qed/proofs/{}.lean` (with a sibling `lakefile.lean` declaring \
                     `package {}`). The consumer's codegen will auto-detect the package and \
                     swap the caller's theorem from Stance 1 (axiom) to Stance 2 (imported proof).",
                    iface.name,
                    crate::lean_sidecars::proof_pkg_name(&iface.name),
                )));
        }
    }
    warnings
}

/// One finding per imported interface that `qedgen verify
/// --require-verified` would reject; carries enough context for main.rs to
/// render a CRIT line and exit non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnverifiedCallee {
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
pub(crate) fn collect_require_verified_findings(spec: &ParsedSpec) -> Vec<UnverifiedCallee> {
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

pub(super) fn check_shape_only_cpi(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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

            warnings.push(
                warn(
                    "shape_only_cpi",
                    Severity::Info,
                    3,
                    format!(
                        "handler '{}' calls `{}.{}` — {}",
                        handler.name, call.target_interface, call.target_handler, reason
                    ),
                )
                .subject(handler.name.clone())
                .fix(fix)
                .example(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* what the callee guarantees */\n    }}\n  }}",
                    call.target_interface, call.target_handler
                )),
            );
        }
    }

    warnings
}

/// Rule 10: handler has token program in accounts but no transfers.
///
/// Suppressed on lifecycle-init handlers that create a token account:
/// Anchor's `#[account(init, token::… / associated_token::…)]` handles
/// the SPL Token CPI implicitly — no explicit `transfers` / `call
/// Token.*` needed. Init detection is a shape predicate (pre-state
/// variant carries no payload fields = freshly-created account), not a
/// hardcoded name list, which over-fired on specs naming the pre-state
/// `Uninit` / `Created` / etc. Unit variants come from both
/// `account_types[*].variants` and `sum_types`.
pub(super) fn check_missing_cpi_for_token_context(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let unit_variant_names: std::collections::HashSet<&str> = spec
        .account_types
        .iter()
        .flat_map(|a| a.variants.iter())
        .chain(spec.sum_types.iter().flat_map(|s| s.variants.iter()))
        .filter(|v| v.fields.is_empty())
        .map(|v| v.name.as_str())
        .collect();
    for handler in &spec.handlers {
        if !handler.has_token_program() {
            continue;
        }
        if !handler.has_calls() {
            let is_lifecycle_init = handler
                .pre_status
                .as_deref()
                .map(|s| unit_variant_names.contains(s))
                .unwrap_or(false);
            // No writable-token-account sub-condition: real specs often
            // leave token accounts bare-typed and let Anchor resolve via
            // init constraints; `is_lifecycle_init && !has_calls()` already
            // captures the shape Anchor's init macro covers implicitly.
            if is_lifecycle_init {
                continue;
            }
            let writable_tokens: Vec<&str> = handler
                .accounts
                .iter()
                .filter(|a| {
                    a.is_writable && a.account_type.as_deref() == Some("token") && !a.is_program
                })
                .map(|a| a.name.as_str())
                .collect();
            let signer_name = handler
                .signer_account()
                .map(|a| a.name.as_str())
                .unwrap_or("authority");
            let accounts_str = if writable_tokens.len() >= 2 {
                format!(
                    "from {} to {} authority {}",
                    writable_tokens[0], writable_tokens[1], signer_name
                )
            } else if writable_tokens.len() == 1 {
                format!(
                    "from {} to dest authority {}",
                    writable_tokens[0], signer_name
                )
            } else {
                format!("from source to dest authority {}", signer_name)
            };
            warnings.push(
                warn(
                    "missing_cpi_for_token_context",
                    Severity::Warning,
                    2,
                    format!(
                        "handler '{}' has token_program in accounts but no `transfers` block",
                        handler.name
                    ),
                )
                .subject(handler.name.clone())
                .fix("Add a `transfers` block to specify token movements")
                .example(format!(
                    "  handler {}\n    transfers {{\n      {} amount <expr>\n    }}",
                    handler.name, accounts_str
                )),
            );
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::test_support::*;

    // ========================================================================
    // multi_cpi_same_field lint
    // ========================================================================

    /// Two CPI calls whose substituted ensures both reference the same
    /// caller-state field (`post.vault_balance`) → lint fires P2 Info.
    /// Mirrors the bear-hug scenario where two `Token.transfer` calls
    /// drain the same vault. Without per-call snapshot frames (v3.0),
    /// the Kani harness can over-constrain.
    #[test]
    fn multi_cpi_same_field_fires_on_two_token_transfers_from_same_vault() {
        let src = r#"spec MultiCpi
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
      call Token.transfer(from = 0, to = 1, amount = a, authority = 0)
      call Token.transfer(from = 0, to = 2, amount = b, authority = 0)
      effect { vault_balance -= a }
      ensures state.vault_balance == old(state.vault_balance) - a - b
    }"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "multi_cpi_same_field")
            .unwrap_or_else(|| {
                panic!(
                    "multi_cpi_same_field must fire; got: {:?}",
                    warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
                )
            });
        assert_eq!(hit.severity, Severity::Info);
        assert_eq!(hit.priority, 2);
        assert!(
            hit.message.contains("'vault_balance'"),
            "message must name the shared field; got: {}",
            hit.message
        );
        assert!(
            hit.message.contains("Token.transfer"),
            "message must name the call pair; got: {}",
            hit.message
        );
        assert_eq!(hit.subject.as_deref(), Some("split"));
    }

    /// Disjoint Token.transfer resources are handled by the Pinocchio
    /// impl-targeted token projection backend. The abstract callee fields
    /// are the same, but the generated proof reads and asserts each token
    /// account's concrete amount independently.
    #[test]
    fn multi_cpi_same_field_silent_on_disjoint_token_transfer_resources() {
        let src = r#"spec MultiCpiDisjointToken
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
        ensures state.from_balance == old(state.from_balance) - amount
        ensures state.to_balance == old(state.to_balance) + amount
      }
    }

    state { from_balance : U64, to_balance : U64 }

    handler swap_like (a : U64) (b : U64) {
      permissionless
      requires a > 0 else InvalidAmount
      requires b > 0 else InvalidAmount
      call Token.transfer(from = user_input, to = hub_input, amount = a, authority = auth)
      call Token.transfer(from = hub_output, to = user_output, amount = b, authority = auth)
      ensures state.from_balance == old(state.from_balance) - a
    }"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        assert!(
            warnings.is_empty(),
            "disjoint Token.transfer resources use per-account projections; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Two CPI calls whose substituted ensures reference disjoint
    /// caller-state fields → lint stays silent. No (pre, post) snapshot
    /// pair is shared, so the over-constraint risk doesn't apply.
    #[test]
    fn multi_cpi_same_field_silent_on_disjoint_fields() {
        let src = r#"spec MultiCpiDisjoint
    program_id "11111111111111111111111111111111"

    interface VaultA {
      program_id "11111111111111111111111111111111"
      handler debit (amount : U64) {
        accounts { vault : writable }
        requires amount > 0
        ensures state.vault_a_balance == old(state.vault_a_balance) - amount
      }
    }

    interface VaultB {
      program_id "11111111111111111111111111111111"
      handler debit (amount : U64) {
        accounts { vault : writable }
        requires amount > 0
        ensures state.vault_b_balance == old(state.vault_b_balance) - amount
      }
    }

    state { vault_a_balance : U64, vault_b_balance : U64 }

    handler tap_both (a : U64) (b : U64) {
      permissionless
      requires a > 0 else InvalidAmount
      requires b > 0 else InvalidAmount
      call VaultA.debit(amount = a)
      call VaultB.debit(amount = b)
      effect { vault_a_balance -= a }
      effect { vault_b_balance -= b }
      ensures state.vault_a_balance == old(state.vault_a_balance) - a
      ensures state.vault_b_balance == old(state.vault_b_balance) - b
    }"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        assert!(
            warnings.is_empty(),
            "disjoint-field CPI ensures must not fire multi_cpi_same_field; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    /// Tier-0 callees (no `ensures` declared) → no substituted field
    /// references → lint stays silent regardless of CPI multiplicity.
    /// Catches the spec-shape where the user hasn't yet declared the
    /// callee's contract; the `cpi_no_callee_ensures` lint surfaces
    /// that gap separately.
    #[test]
    fn multi_cpi_same_field_silent_on_tier0_callees() {
        let src = r#"spec MultiCpiTier0
    program_id "11111111111111111111111111111111"

    interface Logger {
      program_id "11111111111111111111111111111111"
      handler log (msg : U64) {
        accounts { sink : writable }
      }
    }

    state { counter : U64 }

    handler tick_twice (a : U64) (b : U64) {
      permissionless
      requires a > 0 else InvalidAmount
      requires b > 0 else InvalidAmount
      call Logger.log(msg = a)
      call Logger.log(msg = b)
      effect { counter += a }
      ensures state.counter == old(state.counter) + a
    }"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_multi_cpi_same_field(&spec);
        assert!(
            warnings.is_empty(),
            "tier-0 callees produce no field refs → lint must stay silent; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // [shape_only_cpi] lint
    // ──────────────────────────────────────────────────────────────────────

    /// Declared Tier-0 interfaces with no `ensures` must not fire
    /// `shape_only_cpi` — firing would force `ensures true` tautologies on
    /// handlers with no meaningful post-condition. The lint still fires for
    /// undeclared interfaces / missing handlers (real spec bugs).
    #[test]
    fn shape_only_cpi_silent_on_declared_tier0_interface() {
        let src = r#"spec Demo

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert!(
            hits.is_empty(),
            "Tier-0 interface with no `ensures` should not fire shape_only_cpi; got: {:?}",
            hits.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn shape_only_cpi_fires_on_undeclared_interface() {
        let src = r#"spec Demo

    handler pay : State.A -> State.A {
      call Jupiter.swap(pool = amm, amount_in = 100, min_out = 90)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert_eq!(
            hits.len(),
            1,
            "expected one shape_only_cpi warning, got {:?}",
            ws
        );
        assert!(hits[0].message.contains("not declared"));
    }

    #[test]
    fn shape_only_cpi_silent_on_tier1_interface() {
        // Interface declares at least one ensures — no lint should fire.
        let src = r#"spec Demo

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_completeness(&parsed);
        let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
        assert!(
            hits.is_empty(),
            "Tier 1 interfaces should not lint, got: {:?}",
            hits
        );
    }

    // ----- cpi_unverified_callee P2 lint -----

    #[test]
    fn cpi_unverified_callee_fires_on_unverified_import() {
        // Simulates an `import Token from "..."` whose provider didn't
        // ship a proof package. The resolver wouldn't have populated
        // `verified_callees` so the lint should fire.
        let src = r#"spec Demo

    import Token from "spl_token"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:0000" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert_eq!(
            ws.len(),
            1,
            "expected one unverified-callee warning; got: {ws:?}"
        );
        assert_eq!(ws[0].rule, "cpi_unverified_callee");
        assert_eq!(ws[0].priority, 2);
        assert!(ws[0].message.contains("Stance-1 axiom"));
        assert!(ws[0].fix.contains(".qed/proofs"));
        assert!(
            ws[0].fix.contains("tokenProofs"),
            "fix message should name the expected lake package; got: {}",
            ws[0].fix
        );
    }

    #[test]
    fn cpi_unverified_callee_silent_when_verified_callees_lists_iface() {
        // Same shape but `verified_callees` has the import registered,
        // simulating a provider that did ship proofs.
        let src = r#"spec Demo

    import Token from "spl_token"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:0000" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let mut parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        parsed
            .verified_callees
            .insert("Token".to_string(), std::path::PathBuf::from("/tmp/x"));
        let ws = check_cpi_unverified_callee(&parsed);
        assert!(
            ws.is_empty(),
            "verified callee should suppress the lint; got: {ws:?}"
        );
    }

    #[test]
    fn cpi_unverified_callee_silent_on_in_spec_interfaces() {
        // Interface declared inline (no `import` statement) — the
        // author owns both the contract and the call, so there's no
        // external trust gap to surface.
        let src = r#"spec Demo

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:0000" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert!(
            ws.is_empty(),
            "inline interface (no import) should not fire; got: {ws:?}"
        );
    }

    #[test]
    fn cpi_unverified_callee_silent_on_tier0_imports() {
        // Imported interface with no `ensures` — cpi_no_callee_ensures
        // (P1) owns that case; cpi_unverified_callee should stay quiet.
        let src = r#"spec Demo

    import Token from "spl_token"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert!(
            ws.is_empty(),
            "Tier-0 imports should not double-fire; got: {ws:?}"
        );
    }

    #[test]
    fn cpi_unverified_callee_deduplicates_repeated_calls() {
        // Two handlers both calling Token.transfer — the lint should
        // surface the trust-gap once per (interface, handler), not per
        // call site.
        let src = r#"spec Demo

    import Token from "spl_token"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:0000" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay_a : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }

    handler pay_b : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 2)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let ws = check_cpi_unverified_callee(&parsed);
        assert_eq!(ws.len(), 1, "should dedupe across call sites; got: {ws:?}");
    }

    // ------------------------------------------------------------------
    // collect_require_verified_findings
    // ------------------------------------------------------------------

    #[test]
    fn require_verified_fires_on_unverified_import_with_ensures() {
        // Non-sentinel binary_hash so the sentinel exemption doesn't
        // intercept. `verified_callees` is empty → provider shipped no
        // proof package → finding.
        let src = r#"spec Demo

    import Token from "amm_lib"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:abc123" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert_eq!(
            findings.len(),
            1,
            "expected one finding for unverified Token; got: {findings:?}"
        );
        assert_eq!(findings[0].interface_name, "Token");
        assert!(
            findings[0].fix_hint.contains(".qed/proofs"),
            "fix hint should point at the proof-package path; got: {}",
            findings[0].fix_hint
        );
    }

    #[test]
    fn require_verified_silent_when_provider_shipped_proofs() {
        // verified_callees populated → provider has proofs → no finding.
        let src = r#"spec Demo

    import Token from "amm_lib"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:abc123" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let mut parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        parsed
            .verified_callees
            .insert("Token".to_string(), std::path::PathBuf::from("/tmp/x"));
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "verified callee must suppress the finding; got: {findings:?}"
        );
    }

    #[test]
    fn require_verified_silent_on_tier0_imports() {
        // No ensures clauses on any handler → Tier 0. Owned by the
        // cpi_no_callee_ensures P1 lint, not by --require-verified.
        let src = r#"spec Demo

    import Token from "amm_lib"

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:abc123" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "Tier-0 (no ensures) imports must not fire --require-verified; got: {findings:?}"
        );
    }

    #[test]
    fn require_verified_silent_on_sentinel_pinned_natives() {
        // Sentinel binary_hash (sha256:00…00) marks a native program
        // (System Program style) — the validator runtime is the trust
        // boundary, not a proof package. `--require-verified` exempts
        // these so any spec that imports `from "system"` doesn't
        // false-fail.
        let src = r#"spec Demo

    import System from "system_lib"

    interface System {
      program_id "11111111111111111111111111111111"
      upstream { binary_hash "sha256:0000000000000000000000000000000000000000000000000000000000000000" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call System.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "sentinel-pinned native must be exempt; got: {findings:?}"
        );
    }

    #[test]
    fn require_verified_silent_on_inline_interfaces() {
        // Interface declared inline (no `import` statement) — author
        // owns both sides of the contract. `--require-verified` only
        // gates on imported interfaces.
        let src = r#"spec Demo

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
      upstream { binary_hash "sha256:abc123" }
      handler transfer (amount : U64) {
        accounts {
          from      : writable
          to        : writable
          authority : signer
        }
        ensures amount > 0
      }
    }

    handler pay : State.A -> State.A {
      call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let findings = collect_require_verified_findings(&parsed);
        assert!(
            findings.is_empty(),
            "inline interfaces must not fire; got: {findings:?}"
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context() {
        let mut h = make_handler("transfer");
        // Has token program in accounts but no transfers block
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "source".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "dest".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "expected missing_cpi_for_token_context, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_suppressed_on_lifecycle_init() {
        // An `initialize` handler creating a writable token account via
        // Anchor's `#[account(init, ...)]` needs no explicit `transfers` /
        // `call Token.*` — the init macro handles the SPL CPI implicitly.
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninitialized".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "vault".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: Some(vec!["vault".to_string(), "authority".to_string()]),
                account_type: Some("token".to_string()),
                authority: Some("vault_pda".to_string()),
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Uninitialized".to_string(), "Active".to_string()],
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Uninitialized".to_string(),
                        fields: vec![],
                    },
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("balance".to_string(), "U64".to_string())],
                    },
                ],
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "lifecycle-init handler creating a token account should NOT fire \
                 missing_cpi_for_token_context; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_suppressed_on_non_canonical_init_name() {
        // The suppression keys on "pre-state variant has no payload", not
        // a hardcoded name list — specs naming the pre-init variant
        // `Uninit` / `Created` / etc. must stay silent too. Mirror of the
        // canonical-name test above with `Uninit` substituted.
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninit".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "vault".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: Some(vec!["vault".to_string(), "authority".to_string()]),
                account_type: Some("token".to_string()),
                authority: Some("vault_pda".to_string()),
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Uninit".to_string(), "Active".to_string()],
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Uninit".to_string(),
                        fields: vec![],
                    },
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("balance".to_string(), "U64".to_string())],
                    },
                ],
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "init handler with non-canonical pre-state variant `Uninit` \
                 must NOT fire missing_cpi_for_token_context (v2.29.2 shape \
                 predicate); got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_suppressed_when_no_typed_token_account() {
        // The suppression must not require a writable account typed
        // `token`: real specs leave token accounts bare-typed and rely on
        // Anchor's `init, associated_token::*` constraints to resolve the
        // type. `is_lifecycle_init && !has_calls()` is sufficient.
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninit".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                // Bare writable, no `type token` — Anchor would type it
                // via an `init, associated_token::*` constraint set the
                // spec doesn't repeat.
                name: "pool_balance_account".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Uninit".to_string(), "Active".to_string()],
            account_types: vec![ParsedAccountType {
                name: "State".to_string(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Uninit".to_string(),
                        fields: vec![],
                    },
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("balance".to_string(), "U64".to_string())],
                    },
                ],
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "lifecycle-init handler with token_program but no `type token` \
                 writable account must NOT fire missing_cpi_for_token_context \
                 (v2.29.2 — Anchor init handles SPL implicitly via constraint \
                 set); got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_cpi_for_token_context_still_fires_on_non_init() {
        // Complement to the suppression: a handler in a non-init
        // lifecycle (e.g. Active → Active) with token_program and a
        // writable token account but no transfers SHOULD still fire —
        // Anchor's init macro doesn't apply, so the missing CPI is a
        // real spec gap.
        let mut h = make_handler("transfer");
        h.pre_status = Some("Active".to_string());
        h.post_status = Some("Active".to_string());
        h.accounts = vec![
            ParsedHandlerAccount {
                name: "authority".to_string(),
                is_signer: true,
                is_writable: false,
                is_program: false,
                pda_seeds: None,
                account_type: None,
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "source".to_string(),
                is_signer: false,
                is_writable: true,
                is_program: false,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
            ParsedHandlerAccount {
                name: "token_program".to_string(),
                is_signer: false,
                is_writable: false,
                is_program: true,
                pda_seeds: None,
                account_type: Some("token".to_string()),
                authority: None,
                default_pubkey: None,
                imported_namespace: None,
            },
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_cpi_for_token_context"),
            "non-init handler with token_program and no transfers SHOULD \
                 still fire; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }
}
