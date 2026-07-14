//! Validation and deterministic collapse of resolved sequence account inputs.
//!
//! Account targets are fixture identities, not public keys. Requiring the
//! explicit `fixture:` namespace prevents this layer from silently treating a
//! guessed or copied address as harness setup.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::check::{ParsedHandlerAccount, ParsedSpec};

use super::domain_sequence::UnresolvedParameterKind;
use super::domain_sequence_binding::{
    ActionLocator, ActionPhase, ResolvedDomainSequenceDocument, ResolvedParameterBinding,
    ResolvedSequenceAction,
};

pub const ACCOUNT_BINDING_OVERLAY_SCHEMA_URI: &str =
    "https://qedgen.dev/schemas/auditor/account-binding-overlay-v1.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountBindingOverlay {
    pub schema_version: u32,
    pub schema_uri: String,
    pub source_resolved_sequence_schema_uri: String,
    pub audit_id: Option<String>,
    /// Stable handler -> declared account -> fixture identifier mapping.
    pub handlers: BTreeMap<String, HandlerAccountOverlay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HandlerAccountOverlay {
    pub accounts: BTreeMap<String, String>,
    /// Every sequence site which supplied each collapsed mapping.
    pub provenance: BTreeMap<String, Vec<AccountBindingUse>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccountBindingUse {
    pub plan_id: String,
    pub action: ActionLocator,
    pub provenance_candidate_ids: Vec<String>,
}

/// Validate account names against the parsed spec and collapse all action
/// bindings into one deterministic per-handler map. Different fixture targets
/// for the same handler/account are an error even when they occur in separate
/// plans.
pub fn collapse_account_binding_overlay(
    spec: &ParsedSpec,
    sequences: &ResolvedDomainSequenceDocument,
) -> Result<AccountBindingOverlay> {
    let inventory = handler_account_inventory(spec)?;
    let fixture_inventory = fixture_account_inventory(&inventory);
    let mut collapsed: BTreeMap<String, HandlerAccountOverlay> = BTreeMap::new();

    for plan in &sequences.plans {
        for (phase, actions) in [
            (ActionPhase::Setup, &plan.setup),
            (ActionPhase::Forward, &plan.forward),
            (ActionPhase::Reverse, &plan.reverse),
            (ActionPhase::Teardown, &plan.teardown),
        ] {
            for (index, action) in actions.iter().enumerate() {
                let locator = ActionLocator { phase, index };
                collapse_action(
                    &inventory,
                    &fixture_inventory,
                    &mut collapsed,
                    &plan.id,
                    &locator,
                    action,
                )?;
            }
        }
    }

    for overlay in collapsed.values_mut() {
        for uses in overlay.provenance.values_mut() {
            uses.sort();
            uses.dedup();
        }
    }

    Ok(AccountBindingOverlay {
        schema_version: 1,
        schema_uri: ACCOUNT_BINDING_OVERLAY_SCHEMA_URI.to_string(),
        source_resolved_sequence_schema_uri: sequences.schema_uri.clone(),
        audit_id: sequences.audit_id.clone(),
        handlers: collapsed,
    })
}

#[derive(Debug)]
struct HandlerAccountInventory {
    bindable: BTreeMap<String, AccountSemanticShape>,
    generator_managed: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccountSemanticShape {
    is_signer: bool,
    is_writable: bool,
    is_program: bool,
    account_type: Option<String>,
    authority: Option<String>,
    imported_namespace: Option<String>,
}

fn handler_account_inventory(
    spec: &ParsedSpec,
) -> Result<BTreeMap<String, HandlerAccountInventory>> {
    let mut handlers = BTreeMap::new();
    for handler in &spec.handlers {
        if handler.name.trim().is_empty() {
            bail!("parsed spec contains a handler with an empty name");
        }
        let mut accounts = BTreeSet::new();
        let mut bindable = BTreeMap::new();
        let mut generator_managed = BTreeSet::new();
        for account in &handler.accounts {
            if account.name.trim().is_empty() {
                bail!(
                    "handler `{}` contains an account with an empty name",
                    handler.name
                );
            }
            if !accounts.insert(account.name.clone()) {
                bail!(
                    "handler `{}` declares duplicate account `{}`",
                    handler.name,
                    account.name
                );
            }
            if account.default_pubkey.is_some() || account.pda_seeds.is_some() {
                generator_managed.insert(account.name.clone());
            } else {
                bindable.insert(account.name.clone(), AccountSemanticShape::from(account));
            }
        }
        if handlers
            .insert(
                handler.name.clone(),
                HandlerAccountInventory {
                    bindable,
                    generator_managed,
                },
            )
            .is_some()
        {
            bail!("parsed spec declares duplicate handler `{}`", handler.name);
        }
    }
    Ok(handlers)
}

impl From<&ParsedHandlerAccount> for AccountSemanticShape {
    fn from(account: &ParsedHandlerAccount) -> Self {
        Self {
            is_signer: account.is_signer,
            is_writable: account.is_writable,
            is_program: account.is_program,
            account_type: account.account_type.clone(),
            authority: account.authority.clone(),
            imported_namespace: account.imported_namespace.clone(),
        }
    }
}

fn fixture_account_inventory(
    inventory: &BTreeMap<String, HandlerAccountInventory>,
) -> BTreeMap<String, Vec<AccountSemanticShape>> {
    let mut fixtures: BTreeMap<String, Vec<AccountSemanticShape>> = BTreeMap::new();
    for handler in inventory.values() {
        for (name, shape) in &handler.bindable {
            fixtures
                .entry(name.clone())
                .or_default()
                .push(shape.clone());
        }
    }
    for shapes in fixtures.values_mut() {
        shapes.sort_by_key(|shape| {
            (
                shape.is_signer,
                shape.is_writable,
                shape.is_program,
                shape.account_type.clone(),
                shape.authority.clone(),
                shape.imported_namespace.clone(),
            )
        });
        shapes.dedup();
    }
    fixtures
}

fn collapse_action(
    inventory: &BTreeMap<String, HandlerAccountInventory>,
    fixture_inventory: &BTreeMap<String, Vec<AccountSemanticShape>>,
    collapsed: &mut BTreeMap<String, HandlerAccountOverlay>,
    plan_id: &str,
    locator: &ActionLocator,
    action: &ResolvedSequenceAction,
) -> Result<()> {
    let declared = inventory.get(&action.handler).ok_or_else(|| {
        anyhow::anyhow!(
            "resolved sequence references handler `{}` absent from the parsed spec",
            action.handler
        )
    })?;
    let account_bindings: Vec<_> = action
        .resolved_bindings
        .iter()
        .filter(|binding| binding.parameter.kind == UnresolvedParameterKind::AccountBindings)
        .collect();
    if account_bindings.is_empty() && !declared.bindable.is_empty() {
        bail!(
            "handler `{}` action {plan_id}/{:?}[{}] has no account binding",
            action.handler,
            locator.phase,
            locator.index
        );
    }

    let mut action_mapping = BTreeMap::new();
    let mut reverse_mapping = BTreeMap::new();
    for binding in account_bindings {
        validate_binding_provenance(plan_id, locator, action, binding)?;
        let object = binding.value.as_object().ok_or_else(|| {
            anyhow::anyhow!(
                "account binding `{}` for handler `{}` is not an object",
                binding.parameter.name,
                action.handler
            )
        })?;
        for (account, target) in object {
            if declared.generator_managed.contains(account) {
                bail!(
                    "account binding source `{}.{account}` is generator-managed by default_pubkey or PDA seeds and cannot be overlaid",
                    action.handler
                );
            }
            let Some(source_shape) = declared.bindable.get(account) else {
                bail!(
                    "account binding key `{account}` is not declared by handler `{}`",
                    action.handler
                );
            };
            let fixture = target.as_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "account binding target for `{}.{account}` must be a fixture identifier string",
                    action.handler
                )
            })?;
            let fixture_name = validate_fixture_identifier(fixture)?;
            let Some(target_shapes) = fixture_inventory.get(fixture_name) else {
                bail!(
                    "fixture target `{fixture}` is absent from the parsed spec fixture inventory"
                );
            };
            validate_fixture_compatibility(
                &action.handler,
                account,
                source_shape,
                fixture,
                target_shapes,
            )?;
            if let Some(previous_account) =
                reverse_mapping.insert(fixture.to_string(), account.clone())
            {
                if previous_account != *account {
                    bail!(
                        "account binding target `{fixture}` is aliased by both `{}.{previous_account}` and `{}.{account}` in one action",
                        action.handler,
                        action.handler
                    );
                }
            }
            if let Some(previous) = action_mapping.insert(account.clone(), fixture.to_string()) {
                if previous != fixture {
                    bail!(
                        "conflicting account targets for `{}.{account}` within one action: `{previous}` vs `{fixture}`",
                        action.handler
                    );
                }
            }
        }
    }

    let supplied: BTreeSet<_> = action_mapping.keys().cloned().collect();
    let required: BTreeSet<_> = declared.bindable.keys().cloned().collect();
    if supplied != required {
        let missing: Vec<_> = required.difference(&supplied).cloned().collect();
        bail!(
            "handler `{}` action {plan_id}/{:?}[{}] is missing declared account bindings: {}",
            action.handler,
            locator.phase,
            locator.index,
            missing.join(", ")
        );
    }

    let overlay =
        collapsed
            .entry(action.handler.clone())
            .or_insert_with(|| HandlerAccountOverlay {
                accounts: BTreeMap::new(),
                provenance: BTreeMap::new(),
            });
    for (account, fixture) in action_mapping {
        if let Some(previous) = overlay.accounts.get(&account) {
            if previous != &fixture {
                bail!(
                    "conflicting account targets for `{}.{account}` across sequence actions: `{previous}` vs `{fixture}`",
                    action.handler
                );
            }
        } else {
            overlay.accounts.insert(account.clone(), fixture);
        }
        overlay
            .provenance
            .entry(account)
            .or_default()
            .push(AccountBindingUse {
                plan_id: plan_id.to_string(),
                action: locator.clone(),
                provenance_candidate_ids: {
                    let mut ids = action.provenance_candidate_ids.clone();
                    ids.sort();
                    ids.dedup();
                    ids
                },
            });
    }
    Ok(())
}

fn validate_fixture_compatibility(
    handler: &str,
    account: &str,
    source: &AccountSemanticShape,
    fixture: &str,
    candidates: &[AccountSemanticShape],
) -> Result<()> {
    if candidates
        .iter()
        .any(|candidate| candidate_compatible(source, candidate))
    {
        return Ok(());
    }

    bail!(
        "fixture target `{fixture}` is not semantically compatible with `{handler}.{account}`; signer/writable/program/type/authority constraints do not match"
    );
}

fn candidate_compatible(source: &AccountSemanticShape, candidate: &AccountSemanticShape) -> bool {
    if source.is_signer && !candidate.is_signer {
        return false;
    }
    if source.is_writable && !candidate.is_writable {
        return false;
    }
    if source.is_program != candidate.is_program {
        return false;
    }
    if let Some(source_type) = &source.account_type {
        if candidate.account_type.as_ref() != Some(source_type) {
            return false;
        }
    }
    if let Some(source_authority) = &source.authority {
        if candidate.authority.as_ref() != Some(source_authority) {
            return false;
        }
    }
    if let Some(source_namespace) = &source.imported_namespace {
        if candidate.imported_namespace.as_ref() != Some(source_namespace) {
            return false;
        }
    }
    true
}

fn validate_binding_provenance(
    plan_id: &str,
    locator: &ActionLocator,
    action: &ResolvedSequenceAction,
    binding: &ResolvedParameterBinding,
) -> Result<()> {
    if binding.provenance.plan_id != plan_id || binding.provenance.action.as_ref() != Some(locator)
    {
        bail!(
            "account binding provenance does not match action {plan_id}/{:?}[{}]",
            locator.phase,
            locator.index
        );
    }
    if binding.parameter.handler.as_deref() != Some(action.handler.as_str()) {
        bail!(
            "account binding parameter handler does not match action handler `{}`",
            action.handler
        );
    }
    Ok(())
}

fn validate_fixture_identifier(value: &str) -> Result<&str> {
    let Some(identifier) = value.strip_prefix("fixture:") else {
        bail!("account target `{value}` is not a fixture identifier; expected `fixture:<name>`");
    };
    let mut chars = identifier.chars();
    let valid_start = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));
    if !valid_start || !valid_rest {
        bail!("invalid fixture account identifier `{value}`");
    }
    Ok(identifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{ParsedHandler, ParsedHandlerAccount};
    use serde_json::json;

    use super::super::domain_sequence::{ActionRole, PlanKind, UnresolvedParameter};
    use super::super::domain_sequence_binding::{
        BindingProvenance, BindingSource, ResolvedSequencePlan,
    };

    fn spec() -> ParsedSpec {
        ParsedSpec {
            handlers: vec![ParsedHandler {
                name: "deposit".to_string(),
                accounts: vec![
                    ParsedHandlerAccount {
                        name: "vault".to_string(),
                        ..Default::default()
                    },
                    ParsedHandlerAccount {
                        name: "authority".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn account_binding(plan: &str, index: usize, vault: &str) -> ResolvedParameterBinding {
        ResolvedParameterBinding {
            parameter: UnresolvedParameter {
                handler: Some("deposit".to_string()),
                name: "Deposit".to_string(),
                kind: UnresolvedParameterKind::AccountBindings,
                declared_type: Some("Deposit".to_string()),
                reason: "explicit accounts".to_string(),
            },
            value: json!({
                "vault": vault,
                "authority": "fixture:authority"
            }),
            provenance: BindingProvenance {
                source: BindingSource::User,
                plan_id: plan.to_string(),
                action: Some(ActionLocator {
                    phase: ActionPhase::Forward,
                    index,
                }),
            },
        }
    }

    fn action(plan: &str, index: usize, vault: &str) -> ResolvedSequenceAction {
        ResolvedSequenceAction {
            handler: "deposit".to_string(),
            role: ActionRole::Forward,
            from_state: None,
            to_state: None,
            guards: vec![],
            provenance_candidate_ids: vec![format!("candidate:{plan}")],
            resolved_bindings: vec![account_binding(plan, index, vault)],
        }
    }

    fn plan(id: &str, vault: &str) -> ResolvedSequencePlan {
        ResolvedSequencePlan {
            id: id.to_string(),
            kind: PlanKind::PairedRoundTrip,
            title: id.to_string(),
            setup: vec![],
            forward: vec![action(id, 0, vault)],
            reverse: vec![],
            teardown: vec![],
            provenance_candidate_ids: vec![format!("candidate:{id}")],
            resolved_plan_bindings: vec![],
        }
    }

    fn resolved(plans: Vec<ResolvedSequencePlan>) -> ResolvedDomainSequenceDocument {
        ResolvedDomainSequenceDocument {
            schema_version: 1,
            schema_uri:
                "https://qedgen.dev/schemas/auditor/resolved-domain-sequences-v1.schema.json"
                    .to_string(),
            source_sequence_schema_uri: "source".to_string(),
            source_bindings_schema_uri: "bindings".to_string(),
            audit_id: Some("audit-1".to_string()),
            source_dossier_schema_version: Some(1),
            plans,
            exclusions: vec![],
        }
    }

    #[test]
    fn collapses_consistent_actions_deterministically_with_provenance() {
        let input = resolved(vec![
            plan("plan-b", "fixture:vault"),
            plan("plan-a", "fixture:vault"),
        ]);
        let output = collapse_account_binding_overlay(&spec(), &input).unwrap();
        let deposit = &output.handlers["deposit"];
        assert_eq!(deposit.accounts["authority"], "fixture:authority");
        assert_eq!(deposit.accounts["vault"], "fixture:vault");
        assert_eq!(deposit.provenance["vault"].len(), 2);
        assert_eq!(deposit.provenance["vault"][0].plan_id, "plan-a");
        assert_eq!(
            serde_json::to_string(&output).unwrap(),
            serde_json::to_string(&collapse_account_binding_overlay(&spec(), &input).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn rejects_conflicts_across_plans() {
        let mut spec = spec();
        spec.handlers.push(ParsedHandler {
            name: "fixture_inventory".to_string(),
            accounts: vec![ParsedHandlerAccount {
                name: "vault_b".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let input = resolved(vec![
            plan("plan-a", "fixture:vault"),
            plan("plan-b", "fixture:vault_b"),
        ]);
        assert!(collapse_account_binding_overlay(&spec, &input)
            .unwrap_err()
            .to_string()
            .contains("across sequence actions"));
    }

    #[test]
    fn rejects_aliasing_one_fixture_into_multiple_action_accounts() {
        let input = resolved(vec![plan("plan-a", "fixture:authority")]);
        assert!(collapse_account_binding_overlay(&spec(), &input)
            .unwrap_err()
            .to_string()
            .contains("aliased by both"));
    }

    #[test]
    fn rejects_semantically_incompatible_fixture_targets() {
        let mut spec = spec();
        spec.handlers[0].accounts[0].is_writable = true;
        spec.handlers[0].accounts[0].account_type = Some("token".to_string());

        let input = resolved(vec![plan("plan-a", "fixture:authority")]);
        assert!(collapse_account_binding_overlay(&spec, &input)
            .unwrap_err()
            .to_string()
            .contains("not semantically compatible"));
    }

    #[test]
    fn rejects_fixture_targets_with_incompatible_authority() {
        let mut spec = spec();
        spec.handlers[0].accounts[0].authority = Some("vault_owner".to_string());

        let input = resolved(vec![plan("plan-a", "fixture:authority")]);
        assert!(collapse_account_binding_overlay(&spec, &input)
            .unwrap_err()
            .to_string()
            .contains("not semantically compatible"));
    }

    #[test]
    fn rejects_unknown_and_missing_account_keys() {
        let mut unknown = resolved(vec![plan("plan-a", "fixture:vault")]);
        unknown.plans[0].forward[0].resolved_bindings[0].value["mystery"] =
            json!("fixture:mystery");
        assert!(collapse_account_binding_overlay(&spec(), &unknown)
            .unwrap_err()
            .to_string()
            .contains("not declared"));

        let mut missing = resolved(vec![plan("plan-a", "fixture:vault")]);
        missing.plans[0].forward[0].resolved_bindings[0]
            .value
            .as_object_mut()
            .unwrap()
            .remove("authority");
        assert!(collapse_account_binding_overlay(&spec(), &missing)
            .unwrap_err()
            .to_string()
            .contains("missing declared account bindings"));
    }

    #[test]
    fn rejects_addresses_and_unscoped_strings_as_fixture_targets() {
        for invalid in [
            "11111111111111111111111111111111",
            "vault",
            "fixture:",
            "fixture:vault/account",
        ] {
            let input = resolved(vec![plan("plan-a", invalid)]);
            assert!(collapse_account_binding_overlay(&spec(), &input).is_err());
        }
    }

    #[test]
    fn rejects_well_formed_fixture_name_absent_from_spec_inventory() {
        let input = resolved(vec![plan("plan-a", "fixture:ghost")]);
        assert!(collapse_account_binding_overlay(&spec(), &input)
            .unwrap_err()
            .to_string()
            .contains("absent from the parsed spec fixture inventory"));
    }

    #[test]
    fn generator_managed_default_and_pda_sources_cannot_be_overlaid() {
        for managed in [
            ParsedHandlerAccount {
                name: "system_program".to_string(),
                default_pubkey: Some("11111111111111111111111111111111".to_string()),
                ..Default::default()
            },
            ParsedHandlerAccount {
                name: "vault_pda".to_string(),
                pda_seeds: Some(vec!["vault".to_string()]),
                ..Default::default()
            },
        ] {
            let mut spec = spec();
            let managed_name = managed.name.clone();
            spec.handlers[0].accounts.push(managed);
            let mut input = resolved(vec![plan("plan-a", "fixture:vault")]);
            input.plans[0].forward[0].resolved_bindings[0].value[&managed_name] =
                json!("fixture:vault");
            let error = collapse_account_binding_overlay(&spec, &input).unwrap_err();
            assert!(error.to_string().contains("generator-managed"));
        }
    }

    #[test]
    fn generator_managed_accounts_are_not_required_in_overlay() {
        let mut spec = spec();
        spec.handlers[0].accounts.push(ParsedHandlerAccount {
            name: "vault_pda".to_string(),
            pda_seeds: Some(vec!["vault".to_string()]),
            ..Default::default()
        });
        let output = collapse_account_binding_overlay(
            &spec,
            &resolved(vec![plan("plan-a", "fixture:vault")]),
        )
        .unwrap();
        assert!(!output.handlers["deposit"]
            .accounts
            .contains_key("vault_pda"));
    }

    #[test]
    fn rejects_tampered_provenance_and_unknown_handlers() {
        let mut provenance = resolved(vec![plan("plan-a", "fixture:vault")]);
        provenance.plans[0].forward[0].resolved_bindings[0]
            .provenance
            .plan_id = "other-plan".to_string();
        assert!(collapse_account_binding_overlay(&spec(), &provenance)
            .unwrap_err()
            .to_string()
            .contains("provenance"));

        let mut handler = resolved(vec![plan("plan-a", "fixture:vault")]);
        handler.plans[0].forward[0].handler = "withdraw".to_string();
        assert!(collapse_account_binding_overlay(&spec(), &handler)
            .unwrap_err()
            .to_string()
            .contains("absent from the parsed spec"));
    }
}
