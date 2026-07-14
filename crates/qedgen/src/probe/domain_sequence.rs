//! Deterministic action-plan synthesis from a ratified domain dossier.
//!
//! The synthesizer is intentionally conservative. It only turns `auto` or
//! `user` ratified paired operations and lifecycle edges into actions. Account
//! bindings and handler arguments remain explicit unresolved inputs: this
//! module never guesses an account, amount, or lifecycle association.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const DOMAIN_SEQUENCES_SCHEMA_URI: &str =
    "https://qedgen.dev/schemas/auditor/domain-sequences-v1.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainSequenceDocument {
    pub schema_version: u32,
    pub schema_uri: String,
    pub audit_id: Option<String>,
    pub source_dossier_schema_version: Option<u64>,
    pub plans: Vec<DomainSequencePlan>,
    pub exclusions: Vec<ExcludedCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainSequencePlan {
    pub id: String,
    pub kind: PlanKind,
    pub title: String,
    pub setup: Vec<SequenceAction>,
    pub forward: Vec<SequenceAction>,
    pub reverse: Vec<SequenceAction>,
    pub teardown: Vec<SequenceAction>,
    pub provenance_candidate_ids: Vec<String>,
    pub unresolved_parameters: Vec<UnresolvedParameter>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    PairedRoundTrip,
    LifecycleTransition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SequenceAction {
    pub handler: String,
    pub role: ActionRole,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub guards: Vec<String>,
    pub provenance_candidate_ids: Vec<String>,
    pub unresolved_parameters: Vec<UnresolvedParameter>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionRole {
    Setup,
    Forward,
    Reverse,
    Teardown,
    LifecycleTransition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnresolvedParameter {
    pub handler: Option<String>,
    pub name: String,
    pub kind: UnresolvedParameterKind,
    pub declared_type: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedParameterKind {
    HandlerArgument,
    AccountBindings,
    LifecycleAssociation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcludedCandidate {
    pub candidate_id: String,
    pub collection: String,
    pub ratification: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
struct HandlerShape {
    accounts_type: Option<String>,
    args: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone)]
struct LifecycleEdge {
    id: String,
    account: Option<String>,
    handler: String,
    from: Option<String>,
    to: Option<String>,
    guards: Vec<String>,
    terminal: bool,
}

/// Synthesize stable, machine-readable action plans from canonical dossier
/// JSON. Invalid canonical shapes fail loudly; unratified domain facts are
/// reported under `exclusions` and never become executable actions.
pub fn synthesize_domain_sequences(dossier: &Value) -> Result<DomainSequenceDocument> {
    let object = dossier
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("domain dossier must be a JSON object"))?;
    let handlers = handler_inventory(object.get("handlers"))?;
    let mut exclusions = Vec::new();
    let lifecycle = accepted_lifecycle_edges(object.get("lifecycle_edges"), &mut exclusions)?;
    let contexts = lifecycle_contexts(&lifecycle);
    let mut plans = paired_plans(
        object.get("paired_operations"),
        &handlers,
        &contexts,
        &mut exclusions,
    )?;
    plans.extend(lifecycle_plans(&lifecycle, &handlers));
    plans.sort_by(|a, b| a.id.cmp(&b.id));
    exclusions
        .sort_by(|a, b| (&a.collection, &a.candidate_id).cmp(&(&b.collection, &b.candidate_id)));

    Ok(DomainSequenceDocument {
        schema_version: 1,
        schema_uri: DOMAIN_SEQUENCES_SCHEMA_URI.to_string(),
        audit_id: object
            .get("audit_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        source_dossier_schema_version: object.get("schema_version").and_then(Value::as_u64),
        plans,
        exclusions,
    })
}

fn handler_inventory(value: Option<&Value>) -> Result<BTreeMap<String, HandlerShape>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let Some(items) = value.as_array() else {
        bail!("domain dossier handlers must be an array");
    };
    let mut handlers = BTreeMap::new();
    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            bail!("every handler inventory item must have a string name");
        };
        let args = item
            .get("args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(|arg| {
                        let name = arg.get("name")?.as_str()?.to_owned();
                        let declared_type = arg
                            .get("qedspec_type")
                            .or_else(|| arg.get("type"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        Some((name, declared_type))
                    })
                    .collect()
            })
            .unwrap_or_default();
        handlers.insert(
            name.to_owned(),
            HandlerShape {
                accounts_type: item
                    .get("accounts_type")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                args,
            },
        );
    }
    Ok(handlers)
}

fn accepted_lifecycle_edges(
    value: Option<&Value>,
    exclusions: &mut Vec<ExcludedCandidate>,
) -> Result<Vec<LifecycleEdge>> {
    let items = canonical_array(value, "lifecycle_edges")?;
    let mut accepted = Vec::new();
    for item in items {
        let id = candidate_id(item, "lifecycle_edges")?;
        let state = ratification(item);
        if !is_accepted(state) {
            exclusions.push(exclusion(&id, "lifecycle_edges", state));
            continue;
        }
        let Some(handler) = item.get("handler").and_then(Value::as_str) else {
            exclusions.push(ExcludedCandidate {
                candidate_id: id,
                collection: "lifecycle_edges".to_string(),
                ratification: state.to_string(),
                reason: "accepted lifecycle edge has no handler; no action was invented"
                    .to_string(),
            });
            continue;
        };
        accepted.push(LifecycleEdge {
            id,
            account: item
                .get("account")
                .and_then(Value::as_str)
                .map(str::to_owned),
            handler: handler.to_owned(),
            from: item.get("from").and_then(Value::as_str).map(str::to_owned),
            to: item.get("to").and_then(Value::as_str).map(str::to_owned),
            guards: string_array(item.get("guards")),
            terminal: item
                .get("terminal")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    accepted.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(accepted)
}

#[derive(Debug, Clone)]
struct LifecycleContexts {
    setup: Vec<LifecycleEdge>,
    teardown: Vec<LifecycleEdge>,
    ambiguous_account_mapping: bool,
}

fn lifecycle_contexts(edges: &[LifecycleEdge]) -> LifecycleContexts {
    let accounts: BTreeSet<_> = edges
        .iter()
        .filter_map(|edge| edge.account.as_deref())
        .collect();
    let ambiguous_account_mapping = accounts.len() > 1;
    if ambiguous_account_mapping {
        return LifecycleContexts {
            setup: Vec::new(),
            teardown: Vec::new(),
            ambiguous_account_mapping,
        };
    }
    LifecycleContexts {
        setup: edges
            .iter()
            .filter(|edge| is_initial_state(edge.from.as_deref()) && !edge.terminal)
            .cloned()
            .collect(),
        teardown: edges.iter().filter(|edge| edge.terminal).cloned().collect(),
        ambiguous_account_mapping,
    }
}

fn paired_plans(
    value: Option<&Value>,
    handlers: &BTreeMap<String, HandlerShape>,
    contexts: &LifecycleContexts,
    exclusions: &mut Vec<ExcludedCandidate>,
) -> Result<Vec<DomainSequencePlan>> {
    let items = canonical_array(value, "paired_operations")?;
    let mut candidates: Vec<_> = items.iter().collect();
    candidates.sort_by_key(|item| item.get("id").and_then(Value::as_str).unwrap_or(""));
    let mut plans = Vec::new();
    for item in candidates {
        let id = candidate_id(item, "paired_operations")?;
        let state = ratification(item);
        if !is_accepted(state) {
            exclusions.push(exclusion(&id, "paired_operations", state));
            continue;
        }
        let left = string_array(item.get("left_handlers"));
        let right = string_array(item.get("right_handlers"));
        if left.is_empty() || right.is_empty() {
            exclusions.push(ExcludedCandidate {
                candidate_id: id,
                collection: "paired_operations".to_string(),
                ratification: state.to_string(),
                reason: "accepted pair lacks explicit left_handlers or right_handlers; operation names were not treated as handlers".to_string(),
            });
            continue;
        }
        let setup_choices: Vec<Option<&LifecycleEdge>> = if contexts.setup.is_empty() {
            vec![None]
        } else {
            contexts.setup.iter().map(Some).collect()
        };
        let teardown_choices: Vec<Option<&LifecycleEdge>> = if contexts.teardown.is_empty() {
            vec![None]
        } else {
            contexts.teardown.iter().map(Some).collect()
        };
        for left_handler in &left {
            for right_handler in &right {
                for setup in &setup_choices {
                    for teardown in &teardown_choices {
                        plans.push(round_trip_plan(
                            &id,
                            left_handler,
                            right_handler,
                            *setup,
                            *teardown,
                            contexts.ambiguous_account_mapping,
                            handlers,
                        ));
                    }
                }
            }
        }
    }
    Ok(plans)
}

fn round_trip_plan(
    pair_id: &str,
    left: &str,
    right: &str,
    setup: Option<&LifecycleEdge>,
    teardown: Option<&LifecycleEdge>,
    ambiguous_account_mapping: bool,
    handlers: &BTreeMap<String, HandlerShape>,
) -> DomainSequencePlan {
    let setup = setup.filter(|edge| edge.handler != left);
    let teardown = teardown.filter(|edge| edge.handler != right);
    let mut provenance = vec![pair_id.to_string()];
    provenance.extend(setup.map(|edge| edge.id.clone()));
    provenance.extend(teardown.map(|edge| edge.id.clone()));
    provenance.sort();
    provenance.dedup();
    let mut actions = Vec::new();
    if let Some(edge) = setup {
        actions.push(action_from_edge(edge, ActionRole::Setup, handlers));
    }
    actions.push(action_for_handler(
        left,
        ActionRole::Forward,
        pair_id,
        handlers,
    ));
    actions.push(action_for_handler(
        right,
        ActionRole::Reverse,
        pair_id,
        handlers,
    ));
    if let Some(edge) = teardown {
        actions.push(action_from_edge(edge, ActionRole::Teardown, handlers));
    }
    let mut unresolved = collect_unresolved(&actions);
    if ambiguous_account_mapping {
        unresolved.push(UnresolvedParameter {
            handler: None,
            name: "lifecycle_account_mapping".to_string(),
            kind: UnresolvedParameterKind::LifecycleAssociation,
            declared_type: None,
            reason: "multiple lifecycle accounts exist; select the setup and teardown associated with this operation pair".to_string(),
        });
    }
    unresolved.sort();
    unresolved.dedup();
    let setup_actions = actions
        .iter()
        .filter(|action| action.role == ActionRole::Setup)
        .cloned()
        .collect();
    let forward = actions
        .iter()
        .filter(|action| action.role == ActionRole::Forward)
        .cloned()
        .collect();
    let reverse = actions
        .iter()
        .filter(|action| action.role == ActionRole::Reverse)
        .cloned()
        .collect();
    let teardown_actions = actions
        .into_iter()
        .filter(|action| action.role == ActionRole::Teardown)
        .collect();
    let setup_id = setup.map(|edge| edge.id.as_str()).unwrap_or("none");
    let teardown_id = teardown.map(|edge| edge.id.as_str()).unwrap_or("none");
    DomainSequencePlan {
        id: format!("sequence:{pair_id}:{left}:{right}:setup={setup_id}:teardown={teardown_id}"),
        kind: PlanKind::PairedRoundTrip,
        title: format!("{left} then {right}"),
        setup: setup_actions,
        forward,
        reverse,
        teardown: teardown_actions,
        provenance_candidate_ids: provenance,
        unresolved_parameters: unresolved,
    }
}

fn lifecycle_plans(
    edges: &[LifecycleEdge],
    handlers: &BTreeMap<String, HandlerShape>,
) -> Vec<DomainSequencePlan> {
    edges
        .iter()
        .map(|edge| {
            let role = if edge.terminal {
                ActionRole::Teardown
            } else if is_initial_state(edge.from.as_deref()) {
                ActionRole::Setup
            } else {
                ActionRole::LifecycleTransition
            };
            let action = action_from_edge(edge, role, handlers);
            let unresolved = action.unresolved_parameters.clone();
            let (setup, forward, teardown) = match role {
                ActionRole::Setup => (vec![action], Vec::new(), Vec::new()),
                ActionRole::Teardown => (Vec::new(), Vec::new(), vec![action]),
                _ => (Vec::new(), vec![action], Vec::new()),
            };
            DomainSequencePlan {
                id: format!("sequence:lifecycle:{}", edge.id),
                kind: PlanKind::LifecycleTransition,
                title: format!(
                    "{}: {} -> {}",
                    edge.handler,
                    edge.from.as_deref().unwrap_or("?"),
                    edge.to.as_deref().unwrap_or("?")
                ),
                setup,
                forward,
                reverse: Vec::new(),
                teardown,
                provenance_candidate_ids: vec![edge.id.clone()],
                unresolved_parameters: unresolved,
            }
        })
        .collect()
}

fn action_for_handler(
    handler: &str,
    role: ActionRole,
    candidate_id: &str,
    handlers: &BTreeMap<String, HandlerShape>,
) -> SequenceAction {
    SequenceAction {
        handler: handler.to_string(),
        role,
        from_state: None,
        to_state: None,
        guards: Vec::new(),
        provenance_candidate_ids: vec![candidate_id.to_string()],
        unresolved_parameters: unresolved_for_handler(handler, handlers),
    }
}

fn action_from_edge(
    edge: &LifecycleEdge,
    role: ActionRole,
    handlers: &BTreeMap<String, HandlerShape>,
) -> SequenceAction {
    SequenceAction {
        handler: edge.handler.clone(),
        role,
        from_state: edge.from.clone(),
        to_state: edge.to.clone(),
        guards: edge.guards.clone(),
        provenance_candidate_ids: vec![edge.id.clone()],
        unresolved_parameters: unresolved_for_handler(&edge.handler, handlers),
    }
}

fn unresolved_for_handler(
    handler: &str,
    handlers: &BTreeMap<String, HandlerShape>,
) -> Vec<UnresolvedParameter> {
    let Some(shape) = handlers.get(handler) else {
        return vec![UnresolvedParameter {
            handler: Some(handler.to_string()),
            name: "handler_inputs".to_string(),
            kind: UnresolvedParameterKind::AccountBindings,
            declared_type: None,
            reason: "handler is absent from the dossier inventory; accounts and arguments require binding"
                .to_string(),
        }];
    };
    let mut unresolved = Vec::new();
    if let Some(accounts_type) = &shape.accounts_type {
        unresolved.push(UnresolvedParameter {
            handler: Some(handler.to_string()),
            name: accounts_type.clone(),
            kind: UnresolvedParameterKind::AccountBindings,
            declared_type: Some(accounts_type.clone()),
            reason: "bind each account explicitly from the handler's declared accounts type"
                .to_string(),
        });
    } else {
        unresolved.push(UnresolvedParameter {
            handler: Some(handler.to_string()),
            name: "accounts".to_string(),
            kind: UnresolvedParameterKind::AccountBindings,
            declared_type: None,
            reason: "the dossier does not describe this handler's account bindings".to_string(),
        });
    }
    for (name, declared_type) in &shape.args {
        unresolved.push(UnresolvedParameter {
            handler: Some(handler.to_string()),
            name: name.clone(),
            kind: UnresolvedParameterKind::HandlerArgument,
            declared_type: declared_type.clone(),
            reason: "choose a value from ratified constraints; no value was inferred".to_string(),
        });
    }
    unresolved.sort();
    unresolved.dedup();
    unresolved
}

fn collect_unresolved(actions: &[SequenceAction]) -> Vec<UnresolvedParameter> {
    let mut unresolved: Vec<_> = actions
        .iter()
        .flat_map(|action| action.unresolved_parameters.iter().cloned())
        .collect();
    unresolved.sort();
    unresolved.dedup();
    unresolved
}

fn canonical_array<'a>(value: Option<&'a Value>, name: &str) -> Result<&'a [Value]> {
    let Some(value) = value else {
        return Ok(&[]);
    };
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("domain dossier {name} must be an array"))
}

fn candidate_id(item: &Value, collection: &str) -> Result<String> {
    item.get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("every {collection} candidate must have a non-empty id"))
}

fn ratification(item: &Value) -> &str {
    item.pointer("/metadata/ratification")
        .or_else(|| item.get("ratification"))
        .and_then(Value::as_str)
        .unwrap_or("pending")
}

fn is_accepted(state: &str) -> bool {
    matches!(state, "auto" | "user")
}

fn exclusion(id: &str, collection: &str, state: &str) -> ExcludedCandidate {
    ExcludedCandidate {
        candidate_id: id.to_string(),
        collection: collection.to_string(),
        ratification: state.to_string(),
        reason: format!("ratification state `{state}` is not executable"),
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    let mut values: Vec<_> = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    values.sort();
    values.dedup();
    values
}

fn is_initial_state(state: Option<&str>) -> bool {
    state.is_some_and(crate::check::state_name_is_nonexistent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dossier() -> Value {
        json!({
            "schema_version": 1,
            "audit_id": "vault-audit",
            "handlers": [
                {"name": "initialize", "accounts_type": "Initialize", "args": []},
                {"name": "deposit", "accounts_type": "Deposit", "args": [
                    {"name": "amount", "qedspec_type": "U64"}
                ]},
                {"name": "withdraw", "accounts_type": "Withdraw", "args": [
                    {"name": "shares", "qedspec_type": "U64"}
                ]},
                {"name": "close", "accounts_type": "Close", "args": []}
            ],
            "paired_operations": [{
                "id": "pair:deposit:withdraw",
                "left_handlers": ["deposit"],
                "right_handlers": ["withdraw"],
                "metadata": {"ratification": "user"}
            }],
            "lifecycle_edges": [
                {"id": "edge:init", "account": "vault", "handler": "initialize",
                 "from": "Uninitialized", "to": "Active", "terminal": false,
                 "guards": ["not initialized"], "metadata": {"ratification": "auto"}},
                {"id": "edge:close", "account": "vault", "handler": "close",
                 "from": "Active", "to": "Closed", "terminal": true,
                 "guards": ["balance == 0"], "metadata": {"ratification": "user"}}
            ]
        })
    }

    #[test]
    fn builds_round_trip_with_setup_teardown_and_explicit_inputs() {
        let output = synthesize_domain_sequences(&dossier()).unwrap();
        let plan = output
            .plans
            .iter()
            .find(|plan| plan.kind == PlanKind::PairedRoundTrip)
            .unwrap();
        assert_eq!(plan.setup[0].handler, "initialize");
        assert_eq!(plan.forward[0].handler, "deposit");
        assert_eq!(plan.reverse[0].handler, "withdraw");
        assert_eq!(plan.teardown[0].handler, "close");
        assert_eq!(
            plan.provenance_candidate_ids,
            ["edge:close", "edge:init", "pair:deposit:withdraw"]
        );
        assert!(plan.unresolved_parameters.iter().any(|parameter| {
            parameter.handler.as_deref() == Some("deposit")
                && parameter.name == "amount"
                && parameter.declared_type.as_deref() == Some("U64")
        }));
        assert!(!serde_json::to_string(&output)
            .unwrap()
            .contains("amount\":1"));
    }

    #[test]
    fn pending_rejected_and_bug_candidates_never_become_actions() {
        let mut input = dossier();
        input["paired_operations"] = json!([
            {"id": "pending", "left_handlers": ["deposit"], "right_handlers": ["withdraw"],
             "metadata": {"ratification": "pending"}},
            {"id": "rejected", "left_handlers": ["deposit"], "right_handlers": ["withdraw"],
             "metadata": {"ratification": "rejected"}},
            {"id": "bug", "left_handlers": ["deposit"], "right_handlers": ["withdraw"],
             "metadata": {"ratification": "bug"}}
        ]);
        input["lifecycle_edges"] = json!([]);
        let output = synthesize_domain_sequences(&input).unwrap();
        assert!(output.plans.is_empty());
        assert_eq!(output.exclusions.len(), 3);
        assert_eq!(
            output
                .exclusions
                .iter()
                .map(|item| item.ratification.as_str())
                .collect::<Vec<_>>(),
            ["bug", "pending", "rejected"]
        );
    }

    #[test]
    fn accepted_pair_without_explicit_handlers_is_excluded() {
        let mut input = dossier();
        input["paired_operations"] = json!([{
            "id": "pair:abstract",
            "left_operation": "deposit",
            "right_operation": "withdraw",
            "metadata": {"ratification": "auto"}
        }]);
        input["lifecycle_edges"] = json!([]);
        let output = synthesize_domain_sequences(&input).unwrap();
        assert!(output.plans.is_empty());
        assert!(output.exclusions[0]
            .reason
            .contains("were not treated as handlers"));
    }

    #[test]
    fn multiple_lifecycle_accounts_are_not_attached_to_pair() {
        let mut input = dossier();
        input["lifecycle_edges"][1]["account"] = json!("position");
        let output = synthesize_domain_sequences(&input).unwrap();
        let pair = output
            .plans
            .iter()
            .find(|plan| plan.kind == PlanKind::PairedRoundTrip)
            .unwrap();
        assert!(pair.setup.is_empty());
        assert!(pair.teardown.is_empty());
        assert!(pair
            .unresolved_parameters
            .iter()
            .any(|parameter| { parameter.kind == UnresolvedParameterKind::LifecycleAssociation }));
    }

    #[test]
    fn output_order_and_serialization_are_stable() {
        let first = synthesize_domain_sequences(&dossier()).unwrap();
        let second = synthesize_domain_sequences(&dossier()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_string_pretty(&first).unwrap(),
            serde_json::to_string_pretty(&second).unwrap()
        );
        assert!(first
            .plans
            .windows(2)
            .all(|plans| plans[0].id <= plans[1].id));
    }

    #[test]
    fn lifecycle_edges_alone_produce_plans() {
        let mut input = dossier();
        input["paired_operations"] = json!([]);
        let output = synthesize_domain_sequences(&input).unwrap();
        assert_eq!(output.plans.len(), 2);
        assert_eq!(output.plans[0].kind, PlanKind::LifecycleTransition);
        assert!(output.plans.iter().any(|plan| !plan.setup.is_empty()));
        assert!(output.plans.iter().any(|plan| !plan.teardown.is_empty()));
    }
}
