//! Explicit user binding for synthesized domain action plans.
//!
//! This module deliberately has no inference path. A sequence is resolved only
//! when the bindings document names every unresolved plan/action parameter.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use super::domain_sequence::{
    ActionRole, DomainSequenceDocument, ExcludedCandidate, PlanKind, SequenceAction,
    UnresolvedParameter, UnresolvedParameterKind,
};

pub const DOMAIN_SEQUENCE_BINDINGS_SCHEMA_URI: &str =
    "https://qedgen.dev/schemas/auditor/domain-sequence-bindings-v1.schema.json";
pub const RESOLVED_DOMAIN_SEQUENCES_SCHEMA_URI: &str =
    "https://qedgen.dev/schemas/auditor/resolved-domain-sequences-v1.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomainSequenceBindings {
    pub schema_version: u32,
    pub schema_uri: String,
    pub source_sequence_schema_uri: String,
    pub source_audit_id: Option<String>,
    pub bindings: Vec<SequenceBinding>,
}

/// One explicit binding, keyed by plan, optional action, and parameter.
/// `action: null` is reserved for plan-level parameters such as lifecycle
/// association; action parameters must name their phase and zero-based index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SequenceBinding {
    pub plan_id: String,
    pub action: Option<ActionLocator>,
    pub parameter: ParameterLocator,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionLocator {
    pub phase: ActionPhase,
    pub index: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    Setup,
    Forward,
    Reverse,
    Teardown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ParameterLocator {
    pub name: String,
    pub kind: UnresolvedParameterKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedDomainSequenceDocument {
    pub schema_version: u32,
    pub schema_uri: String,
    pub source_sequence_schema_uri: String,
    pub source_bindings_schema_uri: String,
    pub audit_id: Option<String>,
    pub source_dossier_schema_version: Option<u64>,
    pub plans: Vec<ResolvedSequencePlan>,
    pub exclusions: Vec<ExcludedCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedSequencePlan {
    pub id: String,
    pub kind: PlanKind,
    pub title: String,
    pub setup: Vec<ResolvedSequenceAction>,
    pub forward: Vec<ResolvedSequenceAction>,
    pub reverse: Vec<ResolvedSequenceAction>,
    pub teardown: Vec<ResolvedSequenceAction>,
    pub provenance_candidate_ids: Vec<String>,
    pub resolved_plan_bindings: Vec<ResolvedParameterBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedSequenceAction {
    pub handler: String,
    pub role: ActionRole,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub guards: Vec<String>,
    pub provenance_candidate_ids: Vec<String>,
    pub resolved_bindings: Vec<ResolvedParameterBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedParameterBinding {
    pub parameter: UnresolvedParameter,
    pub value: Value,
    pub provenance: BindingProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindingProvenance {
    pub source: BindingSource,
    pub plan_id: String,
    pub action: Option<ActionLocator>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BindingSource {
    User,
}

type BindingKey = (String, Option<ActionLocator>, ParameterLocator);

/// Resolve every explicit sequence input or fail without producing a partial
/// document. Unknown and duplicate binding keys are rejected as strictly as
/// missing keys.
pub fn bind_domain_sequences(
    sequences: &DomainSequenceDocument,
    supplied: &DomainSequenceBindings,
) -> Result<ResolvedDomainSequenceDocument> {
    if supplied.schema_version != 1 || supplied.schema_uri != DOMAIN_SEQUENCE_BINDINGS_SCHEMA_URI {
        bail!("unsupported domain sequence bindings schema");
    }
    if supplied.source_sequence_schema_uri != sequences.schema_uri {
        bail!("bindings source_sequence_schema_uri does not match the sequence document");
    }
    if supplied.source_audit_id != sequences.audit_id {
        bail!("bindings source_audit_id does not match the sequence document");
    }

    let expected = expected_parameters(sequences)?;
    let mut values = BTreeMap::new();
    for binding in &supplied.bindings {
        let key = (
            binding.plan_id.clone(),
            binding.action.clone(),
            binding.parameter.clone(),
        );
        let Some(parameter) = expected.get(&key) else {
            bail!("unknown sequence binding `{}`", format_key(&key));
        };
        validate_value(parameter, &binding.value)?;
        if values.insert(key.clone(), binding.value.clone()).is_some() {
            bail!("duplicate sequence binding `{}`", format_key(&key));
        }
    }

    let missing: Vec<_> = expected
        .keys()
        .filter(|key| !values.contains_key(*key))
        .map(format_key)
        .collect();
    if !missing.is_empty() {
        bail!("missing sequence bindings: {}", missing.join(", "));
    }

    let plans = sequences
        .plans
        .iter()
        .map(|plan| {
            let plan_bindings = plan
                .unresolved_parameters
                .iter()
                .filter(|parameter| !parameter_appears_in_actions(plan, parameter))
                .map(|parameter| resolved_binding(&plan.id, None, parameter, &values))
                .collect::<Result<Vec<_>>>()?;
            Ok(ResolvedSequencePlan {
                id: plan.id.clone(),
                kind: plan.kind,
                title: plan.title.clone(),
                setup: resolve_actions(&plan.id, ActionPhase::Setup, &plan.setup, &values)?,
                forward: resolve_actions(&plan.id, ActionPhase::Forward, &plan.forward, &values)?,
                reverse: resolve_actions(&plan.id, ActionPhase::Reverse, &plan.reverse, &values)?,
                teardown: resolve_actions(
                    &plan.id,
                    ActionPhase::Teardown,
                    &plan.teardown,
                    &values,
                )?,
                provenance_candidate_ids: plan.provenance_candidate_ids.clone(),
                resolved_plan_bindings: plan_bindings,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ResolvedDomainSequenceDocument {
        schema_version: 1,
        schema_uri: RESOLVED_DOMAIN_SEQUENCES_SCHEMA_URI.to_string(),
        source_sequence_schema_uri: sequences.schema_uri.clone(),
        source_bindings_schema_uri: supplied.schema_uri.clone(),
        audit_id: sequences.audit_id.clone(),
        source_dossier_schema_version: sequences.source_dossier_schema_version,
        plans,
        exclusions: sequences.exclusions.clone(),
    })
}

fn expected_parameters(
    sequences: &DomainSequenceDocument,
) -> Result<BTreeMap<BindingKey, UnresolvedParameter>> {
    let mut expected = BTreeMap::new();
    let mut plan_ids = BTreeSet::new();
    for plan in &sequences.plans {
        if !plan_ids.insert(plan.id.as_str()) {
            bail!("duplicate sequence plan id `{}`", plan.id);
        }
        for (phase, actions) in [
            (ActionPhase::Setup, &plan.setup),
            (ActionPhase::Forward, &plan.forward),
            (ActionPhase::Reverse, &plan.reverse),
            (ActionPhase::Teardown, &plan.teardown),
        ] {
            for (index, action) in actions.iter().enumerate() {
                for parameter in &action.unresolved_parameters {
                    insert_expected(
                        &mut expected,
                        &plan.id,
                        Some(ActionLocator { phase, index }),
                        parameter,
                    )?;
                }
            }
        }
        for parameter in &plan.unresolved_parameters {
            if !parameter_appears_in_actions(plan, parameter) {
                insert_expected(&mut expected, &plan.id, None, parameter)?;
            }
        }
    }
    Ok(expected)
}

fn insert_expected(
    expected: &mut BTreeMap<BindingKey, UnresolvedParameter>,
    plan_id: &str,
    action: Option<ActionLocator>,
    parameter: &UnresolvedParameter,
) -> Result<()> {
    let key = (
        plan_id.to_string(),
        action,
        ParameterLocator {
            name: parameter.name.clone(),
            kind: parameter.kind,
        },
    );
    if expected.insert(key.clone(), parameter.clone()).is_some() {
        bail!("duplicate unresolved parameter `{}`", format_key(&key));
    }
    Ok(())
}

fn parameter_appears_in_actions(
    plan: &super::domain_sequence::DomainSequencePlan,
    parameter: &UnresolvedParameter,
) -> bool {
    plan.setup
        .iter()
        .chain(&plan.forward)
        .chain(&plan.reverse)
        .chain(&plan.teardown)
        .any(|action| action.unresolved_parameters.contains(parameter))
}

fn resolve_actions(
    plan_id: &str,
    phase: ActionPhase,
    actions: &[SequenceAction],
    values: &BTreeMap<BindingKey, Value>,
) -> Result<Vec<ResolvedSequenceAction>> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let locator = Some(ActionLocator { phase, index });
            let bindings = action
                .unresolved_parameters
                .iter()
                .map(|parameter| resolved_binding(plan_id, locator.clone(), parameter, values))
                .collect::<Result<Vec<_>>>()?;
            Ok(ResolvedSequenceAction {
                handler: action.handler.clone(),
                role: action.role,
                from_state: action.from_state.clone(),
                to_state: action.to_state.clone(),
                guards: action.guards.clone(),
                provenance_candidate_ids: action.provenance_candidate_ids.clone(),
                resolved_bindings: bindings,
            })
        })
        .collect()
}

fn resolved_binding(
    plan_id: &str,
    action: Option<ActionLocator>,
    parameter: &UnresolvedParameter,
    values: &BTreeMap<BindingKey, Value>,
) -> Result<ResolvedParameterBinding> {
    let key = (
        plan_id.to_string(),
        action.clone(),
        ParameterLocator {
            name: parameter.name.clone(),
            kind: parameter.kind,
        },
    );
    let value = values
        .get(&key)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing sequence binding `{}`", format_key(&key)))?;
    Ok(ResolvedParameterBinding {
        parameter: parameter.clone(),
        value,
        provenance: BindingProvenance {
            source: BindingSource::User,
            plan_id: plan_id.to_string(),
            action,
        },
    })
}

fn validate_value(parameter: &UnresolvedParameter, value: &Value) -> Result<()> {
    match parameter.kind {
        UnresolvedParameterKind::AccountBindings => {
            let Some(bindings) = value.as_object() else {
                bail!("account binding `{}` must be a JSON object", parameter.name);
            };
            if bindings.is_empty()
                || bindings.iter().any(|(name, value)| {
                    name.trim().is_empty()
                        || value
                            .as_str()
                            .is_none_or(|value| !valid_fixture_target(value))
                })
            {
                bail!(
                    "account binding `{}` must explicitly map non-empty account names to `fixture:<account>` identities",
                    parameter.name
                );
            }
        }
        UnresolvedParameterKind::LifecycleAssociation => {
            let valid = value.as_str().is_some_and(|value| !value.trim().is_empty())
                || value.as_object().is_some_and(|value| !value.is_empty());
            if !valid {
                bail!(
                    "lifecycle association `{}` must be a non-empty string or object",
                    parameter.name
                );
            }
        }
        UnresolvedParameterKind::HandlerArgument => validate_scalar_type(parameter, value)?,
    }
    Ok(())
}

fn valid_fixture_target(value: &str) -> bool {
    let Some(name) = value.strip_prefix("fixture:") else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn validate_scalar_type(parameter: &UnresolvedParameter, value: &Value) -> Result<()> {
    let Some(declared) = parameter.declared_type.as_deref() else {
        if value.is_null() {
            bail!("handler argument `{}` cannot be null", parameter.name);
        }
        return Ok(());
    };
    let ty = declared.trim().to_ascii_lowercase();
    let valid = match ty.as_str() {
        "bool" | "boolean" => value.is_boolean(),
        "string" | "str" | "pubkey" | "publickey" | "address" => value.is_string(),
        "u8" => unsigned_at_most(value, u8::MAX as u64),
        "u16" => unsigned_at_most(value, u16::MAX as u64),
        "u32" => unsigned_at_most(value, u32::MAX as u64),
        "u64" | "usize" | "u128" => value.as_u64().is_some(),
        "i8" => signed_between(value, i8::MIN as i64, i8::MAX as i64),
        "i16" => signed_between(value, i16::MIN as i64, i16::MAX as i64),
        "i32" => signed_between(value, i32::MIN as i64, i32::MAX as i64),
        "i64" | "isize" | "i128" => value.as_i64().is_some(),
        "f32" | "f64" | "number" => value.is_number(),
        _ if ty.starts_with("vec<") || ty.starts_with('[') => value.is_array(),
        _ => !value.is_null(),
    };
    if !valid {
        bail!(
            "binding for handler argument `{}` is incompatible with declared type `{declared}`",
            parameter.name
        );
    }
    Ok(())
}

fn unsigned_at_most(value: &Value, max: u64) -> bool {
    value.as_u64().is_some_and(|value| value <= max)
}

fn signed_between(value: &Value, min: i64, max: i64) -> bool {
    value
        .as_i64()
        .is_some_and(|value| (min..=max).contains(&value))
}

fn format_key(key: &BindingKey) -> String {
    let action = key
        .1
        .as_ref()
        .map(|action| format!("{:?}[{}]", action.phase, action.index).to_ascii_lowercase())
        .unwrap_or_else(|| "plan".to_string());
    format!("{}/{}/{}:{:?}", key.0, action, key.2.name, key.2.kind)
}

#[cfg(test)]
mod tests {
    use super::super::domain_sequence::{DomainSequencePlan, PlanKind};
    use super::*;
    use serde_json::json;

    fn parameter(
        name: &str,
        kind: UnresolvedParameterKind,
        ty: Option<&str>,
    ) -> UnresolvedParameter {
        UnresolvedParameter {
            handler: Some("deposit".to_string()),
            name: name.to_string(),
            kind,
            declared_type: ty.map(str::to_string),
            reason: "explicit input required".to_string(),
        }
    }

    fn sequences() -> DomainSequenceDocument {
        let accounts = parameter(
            "Deposit",
            UnresolvedParameterKind::AccountBindings,
            Some("Deposit"),
        );
        let amount = parameter(
            "amount",
            UnresolvedParameterKind::HandlerArgument,
            Some("U64"),
        );
        let action = SequenceAction {
            handler: "deposit".to_string(),
            role: ActionRole::Forward,
            from_state: None,
            to_state: None,
            guards: vec!["amount > 0".to_string()],
            provenance_candidate_ids: vec!["pair:deposit:withdraw".to_string()],
            unresolved_parameters: vec![accounts.clone(), amount.clone()],
        };
        DomainSequenceDocument {
            schema_version: 1,
            schema_uri: super::super::domain_sequence::DOMAIN_SEQUENCES_SCHEMA_URI.to_string(),
            audit_id: Some("audit-1".to_string()),
            source_dossier_schema_version: Some(1),
            plans: vec![DomainSequencePlan {
                id: "plan-1".to_string(),
                kind: PlanKind::PairedRoundTrip,
                title: "deposit then withdraw".to_string(),
                setup: vec![],
                forward: vec![action],
                reverse: vec![],
                teardown: vec![],
                provenance_candidate_ids: vec!["pair:deposit:withdraw".to_string()],
                unresolved_parameters: vec![accounts, amount],
            }],
            exclusions: vec![],
        }
    }

    fn binding(name: &str, kind: UnresolvedParameterKind, value: Value) -> SequenceBinding {
        SequenceBinding {
            plan_id: "plan-1".to_string(),
            action: Some(ActionLocator {
                phase: ActionPhase::Forward,
                index: 0,
            }),
            parameter: ParameterLocator {
                name: name.to_string(),
                kind,
            },
            value,
        }
    }

    fn complete_bindings() -> DomainSequenceBindings {
        DomainSequenceBindings {
            schema_version: 1,
            schema_uri: DOMAIN_SEQUENCE_BINDINGS_SCHEMA_URI.to_string(),
            source_sequence_schema_uri: super::super::domain_sequence::DOMAIN_SEQUENCES_SCHEMA_URI
                .to_string(),
            source_audit_id: Some("audit-1".to_string()),
            bindings: vec![
                binding(
                    "Deposit",
                    UnresolvedParameterKind::AccountBindings,
                    json!({"vault": "fixture:vault"}),
                ),
                binding(
                    "amount",
                    UnresolvedParameterKind::HandlerArgument,
                    json!(42),
                ),
            ],
        }
    }

    #[test]
    fn resolves_only_from_explicit_values_and_preserves_provenance() {
        let resolved = bind_domain_sequences(&sequences(), &complete_bindings()).unwrap();
        let action = &resolved.plans[0].forward[0];
        assert_eq!(action.resolved_bindings.len(), 2);
        assert_eq!(action.resolved_bindings[1].value, json!(42));
        assert_eq!(
            action.resolved_bindings[1].provenance.source,
            BindingSource::User
        );
        assert_eq!(action.resolved_bindings[1].provenance.plan_id, "plan-1");
        assert_eq!(action.provenance_candidate_ids, ["pair:deposit:withdraw"]);
    }

    #[test]
    fn rejects_missing_binding_without_partial_output() {
        let mut bindings = complete_bindings();
        bindings.bindings.pop();
        let error = bind_domain_sequences(&sequences(), &bindings).unwrap_err();
        assert!(error.to_string().contains("missing sequence bindings"));
        assert!(error.to_string().contains("amount"));
    }

    #[test]
    fn rejects_unknown_and_duplicate_bindings() {
        let mut unknown = complete_bindings();
        unknown.bindings[0].parameter.name = "authority".to_string();
        assert!(bind_domain_sequences(&sequences(), &unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown sequence binding"));

        let mut duplicate = complete_bindings();
        duplicate.bindings.push(duplicate.bindings[1].clone());
        assert!(bind_domain_sequences(&sequences(), &duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate sequence binding"));
    }

    #[test]
    fn rejects_unknown_action_and_obvious_scalar_type_mismatch() {
        let mut bad_action = complete_bindings();
        bad_action.bindings[1].action.as_mut().unwrap().index = 1;
        assert!(bind_domain_sequences(&sequences(), &bad_action)
            .unwrap_err()
            .to_string()
            .contains("unknown sequence binding"));

        let mut wrong_type = complete_bindings();
        wrong_type.bindings[1].value = json!("42");
        assert!(bind_domain_sequences(&sequences(), &wrong_type)
            .unwrap_err()
            .to_string()
            .contains("incompatible"));
    }

    #[test]
    fn rejects_account_inference_shortcuts() {
        let mut bindings = complete_bindings();
        bindings.bindings[0].value = json!("use the vault account");
        assert!(bind_domain_sequences(&sequences(), &bindings)
            .unwrap_err()
            .to_string()
            .contains("must be a JSON object"));

        let mut address = complete_bindings();
        address.bindings[0].value = json!({"vault": "11111111111111111111111111111111"});
        assert!(bind_domain_sequences(&sequences(), &address)
            .unwrap_err()
            .to_string()
            .contains("fixture:<account>"));
    }

    #[test]
    fn rejects_bindings_authored_for_another_audit() {
        let mut bindings = complete_bindings();
        bindings.source_audit_id = Some("audit-2".to_string());
        assert!(bind_domain_sequences(&sequences(), &bindings)
            .unwrap_err()
            .to_string()
            .contains("source_audit_id"));
    }

    #[test]
    fn resolves_plan_level_lifecycle_association_explicitly() {
        let mut sequences = sequences();
        let association = UnresolvedParameter {
            handler: None,
            name: "lifecycle_account_mapping".to_string(),
            kind: UnresolvedParameterKind::LifecycleAssociation,
            declared_type: None,
            reason: "multiple lifecycle accounts exist".to_string(),
        };
        sequences.plans[0].unresolved_parameters.push(association);
        let mut bindings = complete_bindings();
        bindings.bindings.push(SequenceBinding {
            plan_id: "plan-1".to_string(),
            action: None,
            parameter: ParameterLocator {
                name: "lifecycle_account_mapping".to_string(),
                kind: UnresolvedParameterKind::LifecycleAssociation,
            },
            value: json!({"account": "vault"}),
        });

        let resolved = bind_domain_sequences(&sequences, &bindings).unwrap();
        assert_eq!(resolved.plans[0].resolved_plan_bindings.len(), 1);
        assert_eq!(
            resolved.plans[0].resolved_plan_bindings[0]
                .provenance
                .action,
            None
        );
    }
}
