//! Lift Pinocchio probe `Finding`s into `ProtoClause`s for runtime-agnostic
//! clustering. Each `Category` maps to a fixed `ClusterKind`; SAFETY comments
//! enumerating several preconditions emit one clause per keyword claim:
//! owner/owned/token program/program-owned → AccountOwnerCheck; init* →
//! AccountInitCheck; distinct/different than/not the same → AccountDistinct;
//! token account → AccountTypeTagCheck. No-SAFETY sites default to
//! AccountOwnerCheck — the common `_unchecked` load delegates owner
//! enforcement to the runtime gate.

use crate::cluster::{ClusterKind, ProtoClause};
use crate::probe::{Category, Finding, Reproducer};

/// Extract proto-clauses from a Pinocchio finding set (feeds
/// `cluster_protos`). Findings whose handler looks like a test fixture are
/// filtered — tests aren't the audit surface and over-fire on byte-slice
/// probes against round-trip serialization tests.
pub fn extract_proto_clauses(findings: &[Finding]) -> Vec<ProtoClause> {
    let mut out = Vec::new();
    for finding in findings {
        if is_test_handler(&finding.handler) {
            continue;
        }
        let safety_text = safety_text_from(finding);
        match &finding.category {
            Category::PinocchioUncheckedAccountLoad => {
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
                // Skip: the primary `_unchecked`-load finding already
                // produced proto-clauses from this SAFETY text; emitting
                // again would double-count.
            }
            Category::PinocchioOffsetOverrun => {
                // Byte-slice sites (`data[OFFSET..OFFSET+N]`) carry an
                // implicit "buffer is at least OFFSET + N bytes"
                // precondition. Lift to ArithmeticBoundPre so hundreds of
                // parser sites collapse into one ratifiable program-scope
                // cluster instead of vanishing as empty clusters.
                out.push(make(
                    ClusterKind::ArithmeticBoundPre,
                    finding,
                    finding.category_tag.clone(),
                ));
            }
            // Remaining probe categories don't classify cleanly into the
            // taxonomy; add targeted mappings as demand appears.
            _ => {}
        }
    }
    out
}

/// SAFETY-comment text from the reproducer's substitution map
/// (`SAFETY_CLAIM`), when the site had an adjacent `// SAFETY: …` block.
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

/// Keyword classifier over SAFETY text; one comment can legitimately
/// implicate multiple kinds.
fn classify_safety_text(safety: Option<&str>) -> Vec<ClusterKind> {
    let Some(text) = safety else {
        return Vec::new();
    };
    let lc = text.to_lowercase();
    let mut kinds = Vec::new();
    // Order is presentation-stable across runs.
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

/// Test-handler heuristic: unit-test fns (`test_*`, `*_test`) and obvious
/// helpers (`create_valid_data`, `mock_*`, `fixture_*`). Conservative.
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
        // Real p-token destination-load SAFETY (transfer.rs:68): claims
        // init, distinctness, AND token-account validation → one clause each.
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
