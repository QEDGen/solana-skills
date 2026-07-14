//! Deterministic interview generation and answer application for a domain
//! dossier.
//!
//! This module intentionally operates on `serde_json::Value`: the dossier is
//! an auditor interchange artifact whose domain collections can grow without
//! coupling the CLI to every schema revision.  Known collections are handled
//! conservatively; unknown objects are left untouched.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainArea {
    AssetFlow,
    QuantityUnit,
    PairedOperation,
    AuthorityRole,
    Lifecycle,
    IdentityUniqueness,
}

impl DomainArea {
    fn tag(self) -> &'static str {
        match self {
            Self::AssetFlow => "asset_flow",
            Self::QuantityUnit => "quantity_unit",
            Self::PairedOperation => "paired_operation",
            Self::AuthorityRole => "authority_role",
            Self::Lifecycle => "lifecycle",
            Self::IdentityUniqueness => "identity_uniqueness",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainQuestion {
    /// Stable across runs while the candidate ID and its domain area remain
    /// stable. It is deliberately readable so answers can be reviewed by hand.
    pub question_id: String,
    pub candidate_id: String,
    pub area: DomainArea,
    pub source_collection: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainDecision {
    Confirm,
    Reject,
    Bug,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainAnswer {
    pub candidate_id: String,
    pub decision: DomainDecision,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplySummary {
    pub confirmed: usize,
    pub rejected: usize,
    pub bugs: usize,
}

#[derive(Debug, Clone)]
struct LocatedCandidate {
    collection: String,
    index: usize,
}

/// Generate one concise question for every pending domain candidate in a
/// recognized dossier collection. Output is sorted by area and candidate ID,
/// independent of JSON array order.
///
/// Besides the v1 collections, this recognizes forward-compatible
/// `paired_operations`, `identity_constraints`, and `uniqueness_constraints`
/// arrays. Identity- and pair-shaped structural candidates are included too,
/// which lets an older dossier ask the important questions before its schema
/// grows dedicated collections.
pub fn generate_questions(dossier: &Value) -> Vec<DomainQuestion> {
    let mut questions = Vec::new();

    collect_questions(
        dossier,
        "asset_flows",
        DomainArea::AssetFlow,
        asset_flow_prompt,
        &mut questions,
    );
    collect_questions(
        dossier,
        "quantities",
        DomainArea::QuantityUnit,
        quantity_prompt,
        &mut questions,
    );
    collect_questions(
        dossier,
        "paired_operations",
        DomainArea::PairedOperation,
        paired_operation_prompt,
        &mut questions,
    );
    collect_questions(
        dossier,
        "authority_capabilities",
        DomainArea::AuthorityRole,
        authority_prompt,
        &mut questions,
    );
    collect_questions(
        dossier,
        "lifecycle_edges",
        DomainArea::Lifecycle,
        lifecycle_prompt,
        &mut questions,
    );
    for collection in ["identity_constraints", "uniqueness_constraints"] {
        collect_questions(
            dossier,
            collection,
            DomainArea::IdentityUniqueness,
            identity_prompt,
            &mut questions,
        );
    }

    if let Some(candidates) = dossier
        .get("structural_candidates")
        .and_then(Value::as_array)
    {
        for candidate in candidates {
            let Some(id) = candidate.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !is_pending(candidate) {
                continue;
            }
            let kind = candidate
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let (area, prompt) = if contains_any(
                &kind,
                &["pair", "reverse", "inverse", "complement", "symmetr"],
            ) {
                (
                    DomainArea::PairedOperation,
                    format!(
                        "Must `{id}` have a matching reverse or complementary operation, and what must the pair conserve?"
                    ),
                )
            } else if contains_any(&kind, &["identity", "unique", "pda", "address", "seed"]) {
                (
                    DomainArea::IdentityUniqueness,
                    format!(
                        "Does `{id}` define the canonical identity, and which fields make that identity unique?"
                    ),
                )
            } else {
                continue;
            };
            questions.push(question(id, area, "structural_candidates", prompt));
        }
    }

    questions.sort_by(|left, right| {
        (
            left.area,
            left.candidate_id.as_str(),
            left.source_collection.as_str(),
        )
            .cmp(&(
                right.area,
                right.candidate_id.as_str(),
                right.source_collection.as_str(),
            ))
    });
    questions
        .dedup_by(|left, right| left.candidate_id == right.candidate_id && left.area == right.area);
    questions
}

/// Apply answers atomically to a dossier clone. Unknown, duplicate, already
/// decided, or ambiguous candidate IDs are errors; rejection and bug answers
/// require rationale. This avoids silently ratifying a similarly named item or
/// overwriting an earlier human decision.
///
/// Domain candidates store the decision in `metadata.ratification`;
/// structural candidates store it directly. `bug` is emitted explicitly so a
/// caller cannot accidentally turn a suspected implementation defect into a
/// specification requirement.
pub fn apply_answers(dossier: &Value, answers: &[DomainAnswer]) -> Result<(Value, ApplySummary)> {
    let locations = candidate_locations(dossier);
    let mut seen = BTreeSet::new();
    for answer in answers {
        if !seen.insert(answer.candidate_id.as_str()) {
            bail!("duplicate answer for candidate `{}`", answer.candidate_id);
        }
        if matches!(
            answer.decision,
            DomainDecision::Reject | DomainDecision::Bug
        ) && answer.rationale.trim().is_empty()
        {
            bail!(
                "candidate `{}` requires rationale for {:?}",
                answer.candidate_id,
                answer.decision
            );
        }
        match locations.get(answer.candidate_id.as_str()) {
            None => bail!("unknown domain candidate `{}`", answer.candidate_id),
            Some(found) if found.len() > 1 => bail!(
                "ambiguous domain candidate `{}` occurs in {} collections",
                answer.candidate_id,
                found.len()
            ),
            Some(_) => {}
        }
    }

    let mut updated = dossier.clone();
    let mut summary = ApplySummary::default();
    for answer in answers {
        let location = &locations[answer.candidate_id.as_str()][0];
        let candidate = updated
            .get_mut(&location.collection)
            .and_then(Value::as_array_mut)
            .and_then(|items| items.get_mut(location.index))
            .ok_or_else(|| anyhow!("candidate location changed while applying answers"))?;

        let current = ratification(candidate).unwrap_or("pending");
        if current != "pending" {
            bail!(
                "candidate `{}` is already ratified as `{}`",
                answer.candidate_id,
                current
            );
        }
        let new_state = match answer.decision {
            DomainDecision::Confirm => {
                summary.confirmed += 1;
                "user"
            }
            DomainDecision::Reject => {
                summary.rejected += 1;
                "rejected"
            }
            DomainDecision::Bug => {
                summary.bugs += 1;
                "bug"
            }
        };
        let target = if location.collection == "structural_candidates" {
            candidate
        } else {
            candidate
                .get_mut("metadata")
                .ok_or_else(|| anyhow!("candidate `{}` has no metadata", answer.candidate_id))?
        };
        target["ratification"] = Value::String(new_state.to_string());
        target["rationale"] = if answer.rationale.trim().is_empty() {
            Value::Null
        } else {
            Value::String(answer.rationale.trim().to_string())
        };
    }
    Ok((updated, summary))
}

fn collect_questions(
    dossier: &Value,
    collection: &str,
    area: DomainArea,
    render: fn(&str, &Value) -> String,
    output: &mut Vec<DomainQuestion>,
) {
    let Some(candidates) = dossier.get(collection).and_then(Value::as_array) else {
        return;
    };
    for candidate in candidates {
        let Some(id) = candidate.get("id").and_then(Value::as_str) else {
            continue;
        };
        if is_pending(candidate) {
            output.push(question(id, area, collection, render(id, candidate)));
        }
    }
}

fn question(id: &str, area: DomainArea, collection: &str, prompt: String) -> DomainQuestion {
    DomainQuestion {
        question_id: format!("domain:{}:{}", area.tag(), id),
        candidate_id: id.to_string(),
        area,
        source_collection: collection.to_string(),
        prompt,
    }
}

fn asset_flow_prompt(id: &str, value: &Value) -> String {
    format!(
        "Does `{id}` move {} from {} to {} in `{}`, and is {} the intended amount?",
        field(value, "asset"),
        field(value, "source"),
        field(value, "destination"),
        field(value, "handler"),
        field(value, "nominal_amount")
    )
}

fn quantity_prompt(id: &str, value: &Value) -> String {
    format!(
        "Is `{id}` ({}) measured in {} at scale {}, with {} rounding?",
        field(value, "symbol"),
        field(value, "unit"),
        field(value, "scale"),
        field(value, "rounding")
    )
}

fn paired_operation_prompt(id: &str, value: &Value) -> String {
    let left = first_field(value, &["forward", "open", "first", "handler"]);
    let right = first_field(value, &["reverse", "close", "second", "paired_handler"]);
    format!(
        "Must `{id}` pair `{left}` with `{right}`, and which effects must be reversed or conserved?"
    )
}

fn authority_prompt(id: &str, value: &Value) -> String {
    format!(
        "May role {} identified by {} perform `{}` in `{id}`, and what limits or revocation apply?",
        field(value, "role"),
        field(value, "identity_anchor"),
        field(value, "handler")
    )
}

fn lifecycle_prompt(id: &str, value: &Value) -> String {
    format!(
        "May `{}` transition {} from {} to {} in `{id}`, and is the destination terminal or reversible?",
        field(value, "handler"),
        field(value, "account"),
        field(value, "from"),
        field(value, "to")
    )
}

fn identity_prompt(id: &str, value: &Value) -> String {
    format!(
        "Does `{id}` define the canonical identity for {}, and which fields make it unique?",
        first_field(value, &["account", "entity", "subject", "identity"])
    )
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
}

fn first_field<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .unwrap_or("<unknown>")
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn ratification(candidate: &Value) -> Option<&str> {
    candidate
        .get("ratification")
        .and_then(Value::as_str)
        .or_else(|| {
            candidate
                .get("metadata")
                .and_then(|metadata| metadata.get("ratification"))
                .and_then(Value::as_str)
        })
}

fn is_pending(candidate: &Value) -> bool {
    matches!(ratification(candidate), None | Some("pending"))
}

fn candidate_locations(dossier: &Value) -> BTreeMap<&str, Vec<LocatedCandidate>> {
    let mut locations: BTreeMap<&str, Vec<LocatedCandidate>> = BTreeMap::new();
    let collections = [
        "asset_flows",
        "quantities",
        "paired_operations",
        "authority_capabilities",
        "lifecycle_edges",
        "identity_constraints",
        "uniqueness_constraints",
        "structural_candidates",
    ];
    for collection in collections {
        let Some(candidates) = dossier.get(collection).and_then(Value::as_array) else {
            continue;
        };
        for (index, candidate) in candidates.iter().enumerate() {
            if let Some(id) = candidate.get("id").and_then(Value::as_str) {
                locations.entry(id).or_default().push(LocatedCandidate {
                    collection: collection.to_string(),
                    index,
                });
            }
        }
    }
    locations
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pending() -> Value {
        json!({"ratification": "pending", "rationale": null})
    }

    #[test]
    fn questions_are_deterministic_and_cover_domain_areas() {
        let dossier = json!({
            "asset_flows": [{
                "id": "flow_withdraw", "handler": "withdraw", "asset": "USDC",
                "source": "vault", "destination": "user", "nominal_amount": "amount",
                "metadata": pending()
            }],
            "quantities": [{
                "id": "qty_shares", "symbol": "shares", "unit": "vault share",
                "scale": "1e9", "rounding": "floor", "metadata": pending()
            }],
            "paired_operations": [{
                "id": "pair_deposit_withdraw", "forward": "deposit", "reverse": "withdraw",
                "metadata": pending()
            }],
            "authority_capabilities": [{
                "id": "role_admin_pause", "role": "admin", "identity_anchor": "config.admin",
                "handler": "pause", "metadata": pending()
            }],
            "lifecycle_edges": [{
                "id": "life_vault_close", "account": "vault", "handler": "close",
                "from": "active", "to": "closed", "metadata": pending()
            }],
            "identity_constraints": [{
                "id": "identity_vault", "account": "vault", "metadata": pending()
            }],
            "structural_candidates": [{
                "id": "seed:user_vault", "kind": "pda_seed_uniqueness", "ratification": "pending"
            }]
        });

        let first = generate_questions(&dossier);
        let second = generate_questions(&dossier);
        assert_eq!(first, second);
        assert_eq!(first.len(), 7);
        assert_eq!(first[0].candidate_id, "flow_withdraw");
        assert_eq!(first[1].candidate_id, "qty_shares");
        assert!(first.iter().any(|q| q.area == DomainArea::PairedOperation));
        assert!(first.iter().any(|q| q.area == DomainArea::AuthorityRole));
        assert!(first.iter().any(|q| q.area == DomainArea::Lifecycle));
        assert_eq!(
            first.last().unwrap().question_id,
            "domain:identity_uniqueness:seed:user_vault"
        );
    }

    #[test]
    fn skips_candidates_already_decided() {
        let dossier = json!({
            "asset_flows": [
                {"id": "pending_flow", "metadata": pending()},
                {"id": "confirmed_flow", "metadata": {"ratification": "user"}}
            ]
        });
        let questions = generate_questions(&dossier);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].candidate_id, "pending_flow");
    }

    #[test]
    fn applies_confirm_reject_and_bug_without_mutating_input() {
        let dossier = json!({
            "asset_flows": [{"id": "flow", "metadata": pending()}],
            "quantities": [{"id": "quantity", "metadata": pending()}],
            "structural_candidates": [{"id": "paired:close", "ratification": "pending"}]
        });
        let answers = vec![
            DomainAnswer {
                candidate_id: "flow".into(),
                decision: DomainDecision::Confirm,
                rationale: "Matches token transfer semantics".into(),
            },
            DomainAnswer {
                candidate_id: "quantity".into(),
                decision: DomainDecision::Reject,
                rationale: "This value is basis points, not tokens".into(),
            },
            DomainAnswer {
                candidate_id: "paired:close".into(),
                decision: DomainDecision::Bug,
                rationale: "Close fails to revoke the delegate".into(),
            },
        ];
        let (updated, summary) = apply_answers(&dossier, &answers).unwrap();

        assert_eq!(summary.confirmed, 1);
        assert_eq!(summary.rejected, 1);
        assert_eq!(summary.bugs, 1);
        assert_eq!(
            dossier["asset_flows"][0]["metadata"]["ratification"],
            "pending"
        );
        assert_eq!(
            updated["asset_flows"][0]["metadata"]["ratification"],
            "user"
        );
        assert_eq!(
            updated["quantities"][0]["metadata"]["ratification"],
            "rejected"
        );
        assert_eq!(updated["structural_candidates"][0]["ratification"], "bug");
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_answers_atomically() {
        let dossier = json!({
            "asset_flows": [{"id": "same", "metadata": pending()}],
            "quantities": [{"id": "same", "metadata": pending()}]
        });
        let error = apply_answers(
            &dossier,
            &[DomainAnswer {
                candidate_id: "same".into(),
                decision: DomainDecision::Confirm,
                rationale: String::new(),
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("ambiguous"));

        let error = apply_answers(
            &json!({"asset_flows": [{"id": "flow", "metadata": pending()}]}),
            &[DomainAnswer {
                candidate_id: "flow".into(),
                decision: DomainDecision::Bug,
                rationale: "  ".into(),
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires rationale"));
    }

    #[test]
    fn will_not_overwrite_an_existing_human_decision() {
        let dossier = json!({
            "lifecycle_edges": [{
                "id": "closed", "metadata": {"ratification": "user", "rationale": "confirmed"}
            }]
        });
        let error = apply_answers(
            &dossier,
            &[DomainAnswer {
                candidate_id: "closed".into(),
                decision: DomainDecision::Reject,
                rationale: "changed mind".into(),
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("already ratified"));
    }
}
