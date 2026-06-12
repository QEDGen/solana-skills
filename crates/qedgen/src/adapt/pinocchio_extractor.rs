//! Pinocchio proto-clause extractor (v2.19 M1.3).
//!
//! Walks a slice of `Finding`s emitted by `pinocchio_probe::findings_from_catalogue`
//! and lifts each one into one or more `ProtoClause`s — the input to M1.4's
//! clustering algorithm.
//!
//! The mapping is intentionally narrow: every Pinocchio `Category` variant
//! maps to a fixed `ClusterKind` (or to multiple, when a SAFETY-comment
//! site carries claims about distinct preconditions). Per-runtime
//! variation lives entirely in this file; downstream clustering and spec
//! emission are runtime-agnostic.
//!
//! SAFETY-comment classification heuristic:
//!
//! Pinocchio's SAFETY comments tend to enumerate several preconditions in
//! one block (e.g. *"the account is guaranteed to be initialized and
//! different than `source_account_info`; it was also already validated to
//! be a token account"*). The extractor scans the SAFETY text for keyword
//! markers and emits one proto-clause per detected claim:
//!
//! - `owner`/`owned`/`token program`/`program-owned` → `AccountOwnerCheck`
//! - `init`/`initialized`/`uninitialized` → `AccountInitCheck`
//! - `distinct`/`different than`/`not the same` → `AccountDistinct`
//! - `token account`/`token-account` → `AccountTypeTagCheck`
//!
//! Sites without a SAFETY comment default to `AccountOwnerCheck` because
//! the most common Pinocchio `_unchecked` load delegates owner enforcement
//! to the runtime gate.

use crate::cluster::{ClusterKind, ProtoClause};
use crate::probe::{Category, Finding, Reproducer};

/// Extract proto-clauses from a Pinocchio finding set. Output feeds M1.4's
/// `cluster_protos` algorithm.
///
/// Filters out findings whose handler name looks like a test fixture
/// (`test_*`, `*_test_*`, helpers like `create_valid_data`). Tests
/// aren't the audit surface and would over-fire on byte-slice probes
/// against round-trip serialization tests.
pub fn extract_proto_clauses(findings: &[Finding]) -> Vec<ProtoClause> {
    let mut out = Vec::new();
    for finding in findings {
        if is_test_handler(&finding.handler) {
            continue;
        }
        let safety_text = safety_text_from(finding);
        match &finding.category {
            Category::PinocchioUncheckedAccountLoad => {
                // SAFETY-comment-driven: emit one proto-clause per detected
                // claim. If no comment, default to AccountOwnerCheck.
                let kinds = classify_safety_text(safety_text.as_deref());
                if kinds.is_empty() {
                    out.push(make(
                        ClusterKind::AccountOwnerCheck,
                        finding,
                        safety_text.clone().unwrap_or_default(),
                    ));
                } else {
                    for kind in kinds {
                        out.push(make(kind, finding, safety_text.clone().unwrap_or_default()));
                    }
                }
            }
            Category::PinocchioUncheckedArith => {
                out.push(make(
                    ClusterKind::ArithmeticNoOverflow,
                    finding,
                    safety_text.unwrap_or_else(|| finding.category_tag.clone()),
                ));
            }
            Category::PinocchioAccountTypeConfusion | Category::PinocchioPositionWithoutTypeTag => {
                out.push(make(
                    ClusterKind::AccountTypeTagCheck,
                    finding,
                    safety_text.unwrap_or_else(|| finding.category_tag.clone()),
                ));
            }
            Category::PinocchioMutableBorrowAliasing => {
                out.push(make(
                    ClusterKind::AccountDistinct,
                    finding,
                    safety_text.unwrap_or_else(|| finding.category_tag.clone()),
                ));
            }
            Category::PinocchioMissingPdaVerification => {
                out.push(make(
                    ClusterKind::PdaCanonicalDerivation,
                    finding,
                    safety_text.unwrap_or_else(|| finding.category_tag.clone()),
                ));
            }
            Category::PinocchioStaleSafetyComment => {
                // Stale-SAFETY findings are emitted alongside their
                // primary finding (the `_unchecked` load). The primary
                // already produced proto-clauses from the SAFETY text;
                // emitting again would double-count. Skip — the
                // stale-claim itself is surfaced as a bug-side cluster
                // outcome via the user's "[ ] bug" choice on the
                // primary cluster.
            }
            Category::PinocchioOffsetOverrun => {
                // Each byte-slice site (`data[OFFSET..OFFSET+N]`,
                // `_.try_into().unwrap()`) carries an implicit
                // precondition: the input buffer is at least
                // `OFFSET + N` bytes. Lift to ArithmeticBoundPre —
                // ratified as a per-program "input-data length is
                // adequate for all parsers" invariant (or per-handler
                // when only one handler is doing the slicing).
                //
                // The Solana-Foundation rewards program surfaced 252
                // such sites (all `data[N..M]` patterns in `state/*`
                // parsers). Without this lift they'd vanish into
                // <empty clusters>; with it, they collapse to one
                // High-confidence Program-scope cluster the auditor
                // can ratify or refine.
                out.push(make(
                    ClusterKind::ArithmeticBoundPre,
                    finding,
                    finding.category_tag.clone(),
                ));
            }
            // Pinocchio-specific categories below are emitted by the
            // probe but don't classify cleanly into the 14-kind taxonomy.
            // Re-add as targeted ClusterKind mappings as v2.20 dogfood
            // reveals demand.
            _ => {}
        }
    }
    out
}

/// Extract the SAFETY-comment text from a finding's reproducer, if
/// present. The probe's substitution map carries it under `SAFETY_CLAIM`
/// when the site had an adjacent `// SAFETY: …` block.
fn safety_text_from(finding: &Finding) -> Option<String> {
    let reproducer = finding.reproducer.as_ref()?;
    match reproducer {
        Reproducer::MolluskPrompt { substitutions, .. }
        | Reproducer::MiriPrompt { substitutions, .. } => {
            substitutions.get("SAFETY_CLAIM").cloned()
        }
        _ => None,
    }
}

/// Run the keyword classifier over a SAFETY-comment text. Returns the set
/// of cluster kinds the text implicates. A single comment can legitimately
/// produce multiple kinds when it enumerates several preconditions.
fn classify_safety_text(safety: Option<&str>) -> Vec<ClusterKind> {
    let Some(text) = safety else {
        return Vec::new();
    };
    let lc = text.to_lowercase();
    let mut kinds = Vec::new();
    // Order is presentation-stable — the same SAFETY string always
    // produces the same proto-clause order across runs.
    if lc.contains("owner")
        || lc.contains("owned")
        || lc.contains("token program")
        || lc.contains("program-owned")
    {
        kinds.push(ClusterKind::AccountOwnerCheck);
    }
    if lc.contains("init") {
        kinds.push(ClusterKind::AccountInitCheck);
    }
    if lc.contains("distinct")
        || lc.contains("different than")
        || lc.contains("different from")
        || lc.contains("not the same")
    {
        kinds.push(ClusterKind::AccountDistinct);
    }
    if lc.contains("token account") || lc.contains("token-account") {
        kinds.push(ClusterKind::AccountTypeTagCheck);
    }
    kinds
}

fn make(kind: ClusterKind, finding: &Finding, evidence_text: String) -> ProtoClause {
    ProtoClause {
        kind,
        handler: finding.handler.clone(),
        finding_id: finding.id.clone(),
        evidence_text,
    }
}

/// Heuristic test-handler detector. Filters out findings whose handler
/// name is a unit-test fn (`test_*`, `*_test`) or an obvious test
/// helper (`create_valid_data`, `mock_*`). Conservative — any false
/// negatives surface in dogfood and tighten the rule.
fn is_test_handler(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("test_")
        || n.ends_with("_test")
        || n.contains("create_valid_data")
        || n.starts_with("mock_")
        || n.starts_with("fixture_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Severity;
    use std::collections::BTreeMap;

    fn finding_with(category: Category, handler: &str, safety: Option<&str>) -> Finding {
        let mut subs = BTreeMap::new();
        if let Some(s) = safety {
            subs.insert("SAFETY_CLAIM".to_string(), s.to_string());
        }
        Finding {
            id: format!("{}-{}", handler, category_tag_for(&category)),
            category,
            severity: Severity::High,
            handler: handler.to_string(),
            spec_silent_on: String::new(),
            suppression_hint: String::new(),
            investigation_hint: String::new(),
            category_tag: "test".to_string(),
            reproducer: Some(Reproducer::MolluskPrompt {
                template_path: String::new(),
                substitutions: subs,
                repro_path: String::new(),
            }),
            gated_by: None,
        }
    }

    fn category_tag_for(c: &Category) -> &'static str {
        match c {
            Category::PinocchioUncheckedAccountLoad => "uncheckedload",
            Category::PinocchioUncheckedArith => "uncheckedarith",
            Category::PinocchioAccountTypeConfusion => "typeconf",
            Category::PinocchioMutableBorrowAliasing => "alias",
            _ => "other",
        }
    }

    #[test]
    fn unchecked_load_with_owner_safety_yields_owner_check() {
        let f = finding_with(
            Category::PinocchioUncheckedAccountLoad,
            "process_transfer",
            Some("SAFETY: account is owned by the token program"),
        );
        let protos = extract_proto_clauses(&[f]);
        assert_eq!(protos.len(), 1);
        assert_eq!(protos[0].kind, ClusterKind::AccountOwnerCheck);
        assert_eq!(protos[0].handler, "process_transfer");
    }

    #[test]
    fn unchecked_load_without_safety_defaults_to_owner_check() {
        let f = finding_with(Category::PinocchioUncheckedAccountLoad, "burn", None);
        let protos = extract_proto_clauses(&[f]);
        assert_eq!(protos.len(), 1);
        assert_eq!(protos[0].kind, ClusterKind::AccountOwnerCheck);
    }

    #[test]
    fn ptoken_destination_safety_emits_three_proto_clauses() {
        // This is the actual p-token destination-load SAFETY at transfer.rs:68 —
        // it claims initialization, distinctness, AND token-account validation.
        // The extractor should emit one proto-clause per claim.
        let f = finding_with(
            Category::PinocchioUncheckedAccountLoad,
            "process_transfer",
            Some(
                "SAFETY: the account is guaranteed to be initialized and different \
                 than `source_account_info`; it was also already validated to be \
                 a token account.",
            ),
        );
        let protos = extract_proto_clauses(&[f]);
        let kinds: Vec<_> = protos.iter().map(|p| p.kind).collect();
        assert!(
            kinds.contains(&ClusterKind::AccountInitCheck),
            "expected AccountInitCheck in {:?}",
            kinds
        );
        assert!(
            kinds.contains(&ClusterKind::AccountDistinct),
            "expected AccountDistinct in {:?}",
            kinds
        );
        assert!(
            kinds.contains(&ClusterKind::AccountTypeTagCheck),
            "expected AccountTypeTagCheck in {:?}",
            kinds
        );
    }

    #[test]
    fn unchecked_arith_yields_no_overflow_clause() {
        let f = finding_with(Category::PinocchioUncheckedArith, "process_transfer", None);
        let protos = extract_proto_clauses(&[f]);
        assert_eq!(protos.len(), 1);
        assert_eq!(protos[0].kind, ClusterKind::ArithmeticNoOverflow);
    }

    #[test]
    fn aliasing_yields_distinct_clause() {
        let f = finding_with(
            Category::PinocchioMutableBorrowAliasing,
            "process_transfer",
            None,
        );
        let protos = extract_proto_clauses(&[f]);
        assert_eq!(protos.len(), 1);
        assert_eq!(protos[0].kind, ClusterKind::AccountDistinct);
    }

    #[test]
    fn stale_safety_finding_is_dropped_to_avoid_double_counting() {
        let f = finding_with(
            Category::PinocchioStaleSafetyComment,
            "process_transfer",
            Some("SAFETY: account is owned by the token program"),
        );
        let protos = extract_proto_clauses(&[f]);
        assert!(
            protos.is_empty(),
            "PinocchioStaleSafetyComment should produce no proto-clauses \
             (its claims are surfaced via the primary _unchecked load finding); \
             got {:?}",
            protos
        );
    }

    #[test]
    fn offset_overrun_lifts_to_arith_bound_pre() {
        let f = finding_with(Category::PinocchioOffsetOverrun, "parse_from_bytes", None);
        let protos = extract_proto_clauses(&[f]);
        assert_eq!(protos.len(), 1);
        assert_eq!(protos[0].kind, ClusterKind::ArithmeticBoundPre);
    }

    #[test]
    fn test_handlers_are_filtered_out() {
        let cases = [
            ("test_distribution_event_to_bytes", true),
            ("test_merkle_root_set_event", true),
            ("create_valid_data", true),
            ("mock_account", true),
            ("fixture_setup", true),
            ("process_transfer", false),
            ("parse_from_bytes", false),
        ];
        for (name, expect_filtered) in cases {
            let f = finding_with(Category::PinocchioOffsetOverrun, name, None);
            let protos = extract_proto_clauses(&[f]);
            if expect_filtered {
                assert!(
                    protos.is_empty(),
                    "expected `{}` to be filtered as a test handler",
                    name
                );
            } else {
                assert!(
                    !protos.is_empty(),
                    "expected `{}` to pass the test-filter",
                    name
                );
            }
        }
    }

    #[test]
    fn proto_clause_preserves_finding_id_for_back_reference() {
        let f = finding_with(
            Category::PinocchioUncheckedAccountLoad,
            "process_transfer",
            Some("SAFETY: token program owned"),
        );
        let want_id = f.id.clone();
        let protos = extract_proto_clauses(&[f]);
        assert_eq!(protos[0].finding_id, want_id);
    }
}
