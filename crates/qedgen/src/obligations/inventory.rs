//! Expected-obligation inventory + reconciliation (#332).
//!
//! `expected_obligations` enumerates, from the lowered spec, every
//! obligation a backend owes a terminal answer for. `reconcile` diffs that
//! inventory against what the backend's recorder actually captured: an
//! expected obligation the backend never reported becomes `failed` (or
//! `unsupported(multi_account_cross_account_obligation)` on multi-account
//! specs, where cross-module drops are the known #324/#331 gap). This is
//! the safety net that turns a FUTURE silent skip red instead of absent.
//!
//! Two obligation classes exist on purpose:
//!   * inventory kinds (enumerated here) — theorem/harness-backed
//!     obligations whose absence is a defect;
//!   * report-only kinds (`TransitionGuard`, `CpiEnsures` on some
//!     backends, `TransferEnvelope`, `EffectConformance`, `BackendExtra`,
//!     `AccountModel`, `StateRepresentation`) — recorded at emission/skip
//!     sites as evidence; unsupported entries still gate `--strict`, but
//!     no entry is synthesized when a backend records nothing (there is
//!     no per-item artifact owed, or the identity is backend-internal).
//!
//! The enumeration must stay conservative: it lists what the spec
//! REQUESTS, never predicts what a backend will decide. Status is the
//! backend's job; absence detection is ours.

use crate::check::ParsedSpec;
use crate::mir::Mir;

use super::{ObligationBackend, ObligationEntry, ObligationKind, ObligationStatus};

/// Identity of one expected obligation: `(kind, scope, key)` in one
/// backend's namespace (the backend is implied by the enumeration call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedObligation {
    pub kind: ObligationKind,
    pub scope: String,
    pub key: String,
}

fn expect(kind: ObligationKind, scope: &str, key: &str) -> ExpectedObligation {
    ExpectedObligation {
        kind,
        scope: scope.to_string(),
        key: key.to_string(),
    }
}

/// Handlers whose lowered MIR body deep-contains a checked `add` effect —
/// the same filter the Kani and proptest overflow emitters use
/// (`block_effect_triples_deep`), so agreement is by shared function, not
/// a hand-kept mirror.
fn handlers_with_add_effects<'a>(mir: &Mir, parsed: &'a ParsedSpec) -> Vec<&'a str> {
    parsed
        .handlers
        .iter()
        .filter(|op| {
            mir.handler_block(&op.name).is_some_and(|body| {
                crate::rust_codegen_util::block_effect_triples_deep(body)
                    .iter()
                    .any(|(_, kind, _)| *kind == "add")
            })
        })
        .map(|op| op.name.as_str())
        .collect()
}

/// Property × preserved_by pairs with a real predicate body and an
/// existing handler. Declaration-only properties generate no obligations
/// by design (a `check` lint owns that gap).
fn property_pairs(parsed: &ParsedSpec) -> Vec<(&str, &str)> {
    let mut pairs = Vec::new();
    for prop in &parsed.properties {
        if prop.expression.is_none() {
            continue;
        }
        for op_name in &prop.preserved_by {
            if parsed.handlers.iter().any(|h| &h.name == op_name) {
                pairs.push((op_name.as_str(), prop.name.as_str()));
            }
        }
    }
    pairs
}

/// Handler × invariant-clause pairs, keyed `preserves_<inv>` /
/// `establishes_<inv>`, restricted to invariants that exist.
fn invariant_pairs(parsed: &ParsedSpec) -> Vec<(&str, String)> {
    let mut pairs = Vec::new();
    for h in &parsed.handlers {
        for (names, verb) in [
            (&h.invariants, "preserves"),
            (&h.establishes, "establishes"),
        ] {
            for inv_name in names {
                if parsed.invariants.iter().any(|i| &i.name == inv_name) {
                    pairs.push((h.name.as_str(), format!("{}_{}", verb, inv_name)));
                }
            }
        }
    }
    pairs
}

/// Everything `backend` owes a terminal answer for on this spec.
/// sBPF assembly specs return an empty inventory — the manifest does not
/// model the sBPF proof surface yet (Lean-only, different obligation
/// shapes; see `references/sbpf.md`).
pub fn expected_obligations(
    mir: &Mir,
    parsed: &ParsedSpec,
    backend: ObligationBackend,
) -> Vec<ExpectedObligation> {
    if mir.is_assembly {
        return Vec::new();
    }

    let mut expected = Vec::new();

    // Kinds shared by every backend: property preservation.
    for (op, prop) in property_pairs(parsed) {
        expected.push(expect(ObligationKind::PropertyPreservation, op, prop));
    }

    match backend {
        ObligationBackend::Kani => {
            for op in parsed.handlers.iter().filter(|op| op.has_guard()) {
                expected.push(expect(ObligationKind::GuardRejection, &op.name, &op.name));
            }
            for op in &parsed.handlers {
                for idx in 0..op.ensures.len() {
                    expected.push(expect(
                        ObligationKind::EnsuresPreservation,
                        &op.name,
                        &idx.to_string(),
                    ));
                }
            }
            for (op, key) in invariant_pairs(parsed) {
                expected.push(expect(ObligationKind::InvariantPreservation, op, &key));
            }
            for op in handlers_with_add_effects(mir, parsed) {
                expected.push(expect(ObligationKind::Overflow, op, op));
            }
            for cover in &parsed.covers {
                for i in 0..cover.traces.len() {
                    expected.push(expect(
                        ObligationKind::Cover,
                        "file",
                        &format!("{}::{}", cover.name, i),
                    ));
                }
            }
            for liveness in &parsed.liveness_props {
                expected.push(expect(ObligationKind::Liveness, "file", &liveness.name));
            }
            for env in &parsed.environments {
                for prop in &parsed.properties {
                    if prop.expression.is_none() {
                        continue;
                    }
                    expected.push(expect(
                        ObligationKind::Environment,
                        "file",
                        &format!("{}::{}", prop.name, env.name),
                    ));
                }
            }
            if parsed.state_repr_is_adt() {
                expected.push(expect(
                    ObligationKind::StateRepresentation,
                    "file",
                    "state_repr",
                ));
            }
        }
        ObligationBackend::Proptest => {
            for op in parsed.handlers.iter().filter(|op| op.has_guard()) {
                expected.push(expect(ObligationKind::GuardRejection, &op.name, &op.name));
            }
            for (op, key) in invariant_pairs(parsed) {
                expected.push(expect(ObligationKind::InvariantPreservation, op, &key));
            }
            // Per-(handler, field) — mirrors the proptest emitter's
            // iteration over deep add-effect triples.
            for op in &parsed.handlers {
                let Some(body) = mir.handler_block(&op.name) else {
                    continue;
                };
                let mut seen = std::collections::BTreeSet::new();
                for (field_raw, kind, _) in
                    crate::rust_codegen_util::block_effect_triples_deep(body)
                {
                    if kind != "add" {
                        continue;
                    }
                    let field = crate::rust_codegen_util::strip_variant_prefix_for_flat_state(
                        &crate::rust_codegen_util::effect_path_source(field_raw),
                        parsed,
                    );
                    if seen.insert(field.clone()) {
                        expected.push(expect(ObligationKind::Overflow, &op.name, &field));
                    }
                }
            }
        }
        ObligationBackend::Lean => {
            for op in &parsed.handlers {
                for idx in 0..op.ensures.len() {
                    expected.push(expect(
                        ObligationKind::EnsuresPreservation,
                        &op.name,
                        &idx.to_string(),
                    ));
                }
            }
            // Abort theorems: one per `requires … else Err` clause, keyed
            // by position — matching the Lean emitters' clause indices.
            for h in &mir.handlers {
                for idx in 0..h.requires_or_abort.len() {
                    expected.push(expect(
                        ObligationKind::Abort,
                        h.name.as_str(),
                        &format!("abort_{}", idx),
                    ));
                }
            }
            for cover in &parsed.covers {
                for i in 0..cover.traces.len() {
                    expected.push(expect(
                        ObligationKind::Cover,
                        "file",
                        &format!("{}::{}", cover.name, i),
                    ));
                }
            }
            for liveness in &parsed.liveness_props {
                expected.push(expect(ObligationKind::Liveness, "file", &liveness.name));
            }
            for env in &parsed.environments {
                for prop in &parsed.properties {
                    if prop.expression.is_none() {
                        continue;
                    }
                    expected.push(expect(
                        ObligationKind::Environment,
                        "file",
                        &format!("{}::{}", prop.name, env.name),
                    ));
                }
            }
        }
    }

    expected
}

/// Diff the inventory against what the backend recorded. Recorded entries
/// pass through untouched (including report-only extras); every expected
/// identity the backend never reported is synthesized with
/// `missing_status` — the caller (`obligations::reconciled`) picks it
/// from the known capability boundaries, defaulting to `failed`.
pub fn reconcile(
    backend: ObligationBackend,
    expected: Vec<ExpectedObligation>,
    recorded: Vec<ObligationEntry>,
    missing_status: ObligationStatus,
) -> Vec<ObligationEntry> {
    let mut entries = recorded;
    let have: std::collections::BTreeSet<(ObligationKind, String, String)> = entries
        .iter()
        .map(|e| (e.kind, e.scope.clone(), e.key.clone()))
        .collect();

    for exp in expected {
        if have.contains(&(exp.kind, exp.scope.clone(), exp.key.clone())) {
            continue;
        }
        entries.push(ObligationEntry {
            backend,
            kind: exp.kind,
            scope: exp.scope,
            key: exp.key,
            status: missing_status.clone(),
        });
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obligations::{ObligationStatus, UnsupportedReason};

    fn failed_status() -> ObligationStatus {
        ObligationStatus::Failed {
            reason: "requested by the spec but not reported by the kani backend".to_string(),
        }
    }

    #[test]
    fn reconcile_synthesizes_missing_with_given_status() {
        let expected = vec![expect(
            ObligationKind::PropertyPreservation,
            "deposit",
            "solvent",
        )];
        let out = reconcile(
            ObligationBackend::Kani,
            expected.clone(),
            Vec::new(),
            failed_status(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, failed_status());
        assert_eq!(out[0].backend, ObligationBackend::Kani);

        let cross_account = ObligationStatus::Unsupported {
            reason: UnsupportedReason::MultiAccountCrossAccountObligation,
        };
        let out = reconcile(
            ObligationBackend::Proptest,
            expected,
            Vec::new(),
            cross_account.clone(),
        );
        assert_eq!(out[0].status, cross_account);
        assert_eq!(out[0].backend, ObligationBackend::Proptest);
    }

    #[test]
    fn reconcile_passes_recorded_and_extras_through() {
        let expected = vec![expect(ObligationKind::Overflow, "deposit", "deposit")];
        let recorded = vec![
            ObligationEntry {
                backend: ObligationBackend::Kani,
                kind: ObligationKind::Overflow,
                scope: "deposit".to_string(),
                key: "deposit".to_string(),
                status: ObligationStatus::Emitted {
                    artifact: "verify_deposit_no_overflow".to_string(),
                },
            },
            // Report-only extra with no expected counterpart.
            ObligationEntry {
                backend: ObligationBackend::Kani,
                kind: ObligationKind::EffectConformance,
                scope: "deposit".to_string(),
                key: "verify_deposit_effect_balance".to_string(),
                status: ObligationStatus::Emitted {
                    artifact: "verify_deposit_effect_balance".to_string(),
                },
            },
        ];
        let out = reconcile(ObligationBackend::Kani, expected, recorded, failed_status());
        assert_eq!(out.len(), 2);
        assert!(out
            .iter()
            .all(|e| matches!(e.status, ObligationStatus::Emitted { .. })));
    }
}
