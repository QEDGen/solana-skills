//! Crucible corpus seeds for fully resolved domain action plans.
//!
//! Crucible's structured input format is intentionally encoded here instead
//! of approximated through JSON: u32 action count, then for each action a u16
//! variant index followed by fields in generated `action_*` parameter order.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::check::ParsedSpec;

use super::domain_sequence::UnresolvedParameterKind;
use super::domain_sequence_binding::{ResolvedDomainSequenceDocument, ResolvedSequenceAction};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DomainSeedReport {
    pub corpus_dir: PathBuf,
    pub seeds: Vec<DomainSeed>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DomainSeed {
    pub plan_id: String,
    pub path: PathBuf,
    pub action_count: usize,
}

/// Write one byte-stable seed per resolved plan. The content-addressed leaf
/// keeps previous runs from contaminating the corpus selected for this run.
pub fn write_domain_seed_corpus(
    spec: &ParsedSpec,
    resolved: &ResolvedDomainSequenceDocument,
    harness_dir: &Path,
) -> Result<DomainSeedReport> {
    let canonical = serde_json::to_vec(resolved)?;
    let digest = format!("{:x}", Sha256::digest(&canonical));
    let corpus_dir = harness_dir
        .join(".qedgen")
        .join("domain-sequence-corpus")
        .join(&digest[..16]);
    std::fs::create_dir_all(&corpus_dir)
        .with_context(|| format!("creating domain corpus {}", corpus_dir.display()))?;

    let mut seeds = Vec::with_capacity(resolved.plans.len());
    for (index, plan) in resolved.plans.iter().enumerate() {
        let actions: Vec<_> = plan
            .setup
            .iter()
            .chain(&plan.forward)
            .chain(&plan.reverse)
            .chain(&plan.teardown)
            .collect();
        if actions.is_empty() {
            bail!("resolved sequence plan `{}` has no actions", plan.id);
        }
        let bytes = encode_actions(spec, &actions)
            .with_context(|| format!("encoding domain sequence plan `{}`", plan.id))?;
        let safe_id: String = plan
            .id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = corpus_dir.join(format!("{index:03}-{safe_id}.seed"));
        std::fs::write(&path, bytes)
            .with_context(|| format!("writing domain seed {}", path.display()))?;
        seeds.push(DomainSeed {
            plan_id: plan.id.clone(),
            path,
            action_count: actions.len(),
        });
    }
    if seeds.is_empty() {
        bail!("resolved domain sequence document has no plans to replay");
    }
    Ok(DomainSeedReport { corpus_dir, seeds })
}

fn encode_actions(spec: &ParsedSpec, actions: &[&ResolvedSequenceAction]) -> Result<Vec<u8>> {
    let count = u32::try_from(actions.len()).context("domain sequence has too many actions")?;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_le_bytes());
    for action in actions {
        if action
            .resolved_bindings
            .iter()
            .any(|binding| binding.parameter.kind == UnresolvedParameterKind::AccountBindings)
        {
            bail!(
                "handler `{}` has an explicit account binding, but Crucible's structured seed format cannot encode account identity; materialize the binding in the generated fixture before replay",
                action.handler
            );
        }
        let (variant, handler) = spec
            .handlers
            .iter()
            .enumerate()
            .find(|(_, handler)| handler.name == action.handler)
            .ok_or_else(|| {
                anyhow::anyhow!("handler `{}` is absent from the spec", action.handler)
            })?;
        out.extend_from_slice(
            &u16::try_from(variant)
                .context("spec has too many handlers for Crucible's variant index")?
                .to_le_bytes(),
        );
        for (name, ty) in &handler.takes_params {
            // The generated harness deliberately omits Pubkey parameters from
            // its FuzzAction variant and fills them in the instruction body.
            if ty == "Pubkey" {
                continue;
            }
            let binding = action
                .resolved_bindings
                .iter()
                .find(|binding| {
                    binding.parameter.kind == UnresolvedParameterKind::HandlerArgument
                        && binding.parameter.name == *name
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "handler `{}` argument `{name}` has no explicit resolved binding",
                        action.handler
                    )
                })?;
            encode_field(&mut out, name, ty, &binding.value)?;
        }
    }
    Ok(out)
}

fn encode_field(out: &mut Vec<u8>, name: &str, ty: &str, value: &Value) -> Result<()> {
    match ty {
        "Bool" | "bool" => {
            let value = value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("argument `{name}` must be a boolean"))?;
            out.extend_from_slice(&(u64::from(value)).to_le_bytes());
        }
        "U8" | "U16" | "U32" | "U64" | "Usize" | "usize" => {
            let value = value
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("argument `{name}` must be an unsigned integer"))?;
            out.extend_from_slice(&value.to_le_bytes());
        }
        "I8" | "I16" | "I32" | "I64" | "Isize" | "isize" => {
            let value = value
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("argument `{name}` must be a signed integer"))?;
            out.extend_from_slice(&(value as u64).to_le_bytes());
        }
        "U128" => {
            let value = value.as_u64().ok_or_else(|| {
                anyhow::anyhow!("argument `{name}` must be a JSON unsigned integer")
            })?;
            out.extend_from_slice(&(value as u128).to_le_bytes());
        }
        "I128" => {
            let value = value.as_i64().ok_or_else(|| {
                anyhow::anyhow!("argument `{name}` must be a JSON signed integer")
            })?;
            out.extend_from_slice(&(value as i128 as u128).to_le_bytes());
        }
        unsupported => {
            bail!("handler argument `{name}` has unsupported Crucible seed type `{unsupported}`")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::domain_sequence::{ActionRole, PlanKind, UnresolvedParameter};
    use crate::probe::domain_sequence_binding::{
        BindingProvenance, BindingSource, ResolvedParameterBinding, ResolvedSequencePlan,
        RESOLVED_DOMAIN_SEQUENCES_SCHEMA_URI,
    };
    use serde_json::json;

    fn action(handler: &str, args: &[(&str, &str, Value)]) -> ResolvedSequenceAction {
        ResolvedSequenceAction {
            handler: handler.to_string(),
            role: ActionRole::Forward,
            from_state: None,
            to_state: None,
            guards: vec![],
            provenance_candidate_ids: vec![],
            resolved_bindings: args
                .iter()
                .map(|(name, ty, value)| ResolvedParameterBinding {
                    parameter: UnresolvedParameter {
                        handler: Some(handler.to_string()),
                        name: (*name).to_string(),
                        kind: UnresolvedParameterKind::HandlerArgument,
                        declared_type: Some((*ty).to_string()),
                        reason: "test".to_string(),
                    },
                    value: value.clone(),
                    provenance: BindingProvenance {
                        source: BindingSource::User,
                        plan_id: "plan".to_string(),
                        action: None,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn encodes_crucible_variant_and_field_order() {
        let spec = crate::chumsky_adapter::parse_str(
            "spec Replay\n\ntype State\n  | Active of { n : U64 }\n\nhandler first (amount : U64) : State.Active -> State.Active {\n  effect { n := n }\n}\n\nhandler second (flag : Bool) : State.Active -> State.Active {\n  effect { n := n }\n}\n",
        )
        .unwrap();
        let first = action("first", &[("amount", "U64", json!(42))]);
        let second = action("second", &[("flag", "Bool", json!(true))]);
        let bytes = encode_actions(&spec, &[&second, &first]).unwrap();
        assert_eq!(&bytes[0..4], &2u32.to_le_bytes());
        assert_eq!(&bytes[4..6], &1u16.to_le_bytes());
        assert_eq!(&bytes[6..14], &1u64.to_le_bytes());
        assert_eq!(&bytes[14..16], &0u16.to_le_bytes());
        assert_eq!(&bytes[16..24], &42u64.to_le_bytes());
    }

    #[test]
    fn rejects_unbound_and_unsupported_fields() {
        let spec = crate::chumsky_adapter::parse_str(
            "spec Replay\n\ntype State\n  | Active of { n : U64 }\n\nhandler first (amount : U64) : State.Active -> State.Active {\n  effect { n := n }\n}\n",
        )
        .unwrap();
        assert!(encode_actions(&spec, &[&action("first", &[])])
            .unwrap_err()
            .to_string()
            .contains("no explicit resolved binding"));

        let mut unsupported = action("first", &[("amount", "Map<U64,U64>", json!({}))]);
        unsupported.resolved_bindings[0].parameter.name = "amount".to_string();
        assert!(
            encode_field(&mut Vec::new(), "items", "Map<U64,U64>", &json!({}))
                .unwrap_err()
                .to_string()
                .contains("unsupported Crucible seed type")
        );
    }

    #[test]
    fn refuses_to_silently_drop_account_bindings() {
        let spec = crate::chumsky_adapter::parse_str(
            "spec Replay\n\ntype State\n  | Active of { n : U64 }\n\nhandler first (amount : U64) : State.Active -> State.Active {\n  effect { n := n }\n}\n",
        )
        .unwrap();
        let mut bound = action("first", &[("amount", "U64", json!(1))]);
        bound.resolved_bindings.push(ResolvedParameterBinding {
            parameter: UnresolvedParameter {
                handler: Some("first".to_string()),
                name: "First".to_string(),
                kind: UnresolvedParameterKind::AccountBindings,
                declared_type: Some("First".to_string()),
                reason: "test".to_string(),
            },
            value: json!({"vault": "fixture.vault"}),
            provenance: BindingProvenance {
                source: BindingSource::User,
                plan_id: "plan".to_string(),
                action: None,
            },
        });
        assert!(encode_actions(&spec, &[&bound])
            .unwrap_err()
            .to_string()
            .contains("cannot encode account identity"));
    }

    #[test]
    fn writes_content_addressed_corpus() {
        let spec = crate::chumsky_adapter::parse_str(
            "spec Replay\n\ntype State\n  | Active of { n : U64 }\n\nhandler first (amount : U64) : State.Active -> State.Active {\n  effect { n := n }\n}\n",
        )
        .unwrap();
        let resolved = ResolvedDomainSequenceDocument {
            schema_version: 1,
            schema_uri: RESOLVED_DOMAIN_SEQUENCES_SCHEMA_URI.to_string(),
            source_sequence_schema_uri: "sequences".to_string(),
            source_bindings_schema_uri: "bindings".to_string(),
            audit_id: None,
            source_dossier_schema_version: Some(1),
            plans: vec![ResolvedSequencePlan {
                id: "plan/one".to_string(),
                kind: PlanKind::PairedRoundTrip,
                title: "test".to_string(),
                setup: vec![],
                forward: vec![action("first", &[("amount", "U64", json!(7))])],
                reverse: vec![],
                teardown: vec![],
                provenance_candidate_ids: vec![],
                resolved_plan_bindings: vec![],
            }],
            exclusions: vec![],
        };
        let dir = tempfile::tempdir().unwrap();
        let report = write_domain_seed_corpus(&spec, &resolved, dir.path()).unwrap();
        assert_eq!(report.seeds[0].action_count, 1);
        assert!(report.seeds[0].path.ends_with("000-plan_one.seed"));
        assert_eq!(std::fs::read(&report.seeds[0].path).unwrap().len(), 14);
    }
}
