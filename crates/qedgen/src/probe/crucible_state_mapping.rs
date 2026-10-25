//! Explicit, checked bindings from spec state fields to Anchor IDL accounts.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::check::ParsedSpec;
use crate::codegen::rust_codegen_util;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MappingDocument {
    schema_version: u32,
    fields: BTreeMap<String, FieldBinding>,
    setup: Vec<SetupAction>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupAction {
    handler: String,
    args: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSetupAction {
    pub handler: String,
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldBinding {
    account_type: String,
    data_field: String,
    handlers: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedField {
    pub account_type: String,
    pub data_field: String,
    pub discriminator: Vec<u8>,
    pub serialized_len: usize,
    pub handlers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedStateMapping {
    pub fields: BTreeMap<String, ResolvedField>,
    pub setup: Vec<ResolvedSetupAction>,
}

fn scalar_type(idl_type: &str) -> Option<(&'static str, usize)> {
    Some(match idl_type {
        "bool" => ("Bool", 1),
        "u8" => ("U8", 1),
        "i8" => ("I8", 1),
        "u16" => ("U16", 2),
        "i16" => ("I16", 2),
        "u32" => ("U32", 4),
        "i32" => ("I32", 4),
        "u64" => ("U64", 8),
        "i64" => ("I64", 8),
        "u128" => ("U128", 16),
        "i128" => ("I128", 16),
        _ => return None,
    })
}

fn scalar_literal(value: &serde_json::Value, ty: &str) -> Result<String> {
    if ty == "Bool" {
        return value
            .as_bool()
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("expected bool"));
    }
    let (signed, bits) = match ty {
        "U8" => (false, 8),
        "U16" => (false, 16),
        "U32" => (false, 32),
        "U64" => (false, 64),
        "U128" => (false, 128),
        "I8" => (true, 8),
        "I16" => (true, 16),
        "I32" => (true, 32),
        "I64" => (true, 64),
        "I128" => (true, 128),
        _ => bail!("unsupported setup scalar type `{ty}`"),
    };
    let raw = if bits == 128 {
        value
            .as_str()
            .ok_or_else(|| anyhow!("128-bit setup argument must be a decimal string"))?
            .to_string()
    } else {
        value
            .as_number()
            .ok_or_else(|| anyhow!("setup argument `{ty}` must be an integer"))?
            .to_string()
    };
    if signed {
        let parsed: i128 = raw
            .parse()
            .map_err(|_| anyhow!("setup argument `{ty}` must be a decimal integer"))?;
        let (min, max) = if bits == 128 {
            (i128::MIN, i128::MAX)
        } else {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        };
        if parsed < min || parsed > max {
            bail!("setup argument `{ty}` out of range");
        }
    } else {
        let parsed: u128 = raw
            .parse()
            .map_err(|_| anyhow!("setup argument `{ty}` must be a decimal integer"))?;
        let max = if bits == 128 {
            u128::MAX
        } else {
            (1u128 << bits) - 1
        };
        if parsed > max {
            bail!("setup argument `{ty}` out of range");
        }
    }
    Ok(format!("{raw}{}", ty.to_ascii_lowercase()))
}

/// Resolve only fixed-layout, named Borsh structs. The generated `state::*`
/// type and the account discriminator come from this same IDL.
pub fn resolve(spec: &ParsedSpec, idl: &[u8], mapping: &[u8]) -> Result<ResolvedStateMapping> {
    let doc: MappingDocument =
        serde_json::from_slice(mapping).context("parsing Crucible state mapping")?;
    if doc.schema_version != 1 || doc.fields.is_empty() {
        bail!("Crucible state mapping requires schema_version 1 and nonempty fields");
    }
    let idl: serde_json::Value =
        serde_json::from_slice(idl).context("parsing Anchor IDL for Crucible state mapping")?;
    let accounts = idl["accounts"]
        .as_array()
        .ok_or_else(|| anyhow!("Anchor IDL has no accounts array"))?;
    let types = idl["types"]
        .as_array()
        .ok_or_else(|| anyhow!("Anchor IDL has no types array"))?;
    let handler_names: BTreeSet<_> = spec.handlers.iter().map(|h| h.name.as_str()).collect();
    let mut resolved = ResolvedStateMapping::default();
    if doc.setup.len() != 1 {
        bail!("Crucible state mapping v1 requires exactly one explicit setup action initializing all mapped accounts");
    }
    for action in &doc.setup {
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == action.handler)
            .ok_or_else(|| anyhow!("unknown setup handler `{}`", action.handler))?;
        if action.args.keys().collect::<BTreeSet<_>>()
            != handler
                .takes_params
                .iter()
                .map(|(n, _)| n)
                .collect::<BTreeSet<_>>()
        {
            bail!(
                "setup handler `{}` arguments must exactly match spec arguments",
                handler.name
            );
        }
        let mut args = Vec::new();
        for (name, ty) in &handler.takes_params {
            let value = &action.args[name];
            args.push(
                scalar_literal(value, ty)
                    .with_context(|| format!("setup argument `{name}` for `{}`", handler.name))?,
            );
        }
        resolved.setup.push(ResolvedSetupAction {
            handler: handler.name.clone(),
            args,
        });
    }
    for (field, binding) in doc.fields {
        let spec_ty = rust_codegen_util::resolve_state_fields(spec)
            .iter()
            .find(|(name, _)| name == &field)
            .map(|(_, ty)| ty.as_str())
            .ok_or_else(|| {
                anyhow!("unknown spec state field `{field}` in Crucible state mapping")
            })?;
        if !matches!(
            spec_ty,
            "Bool" | "U8" | "I8" | "U16" | "I16" | "U32" | "I32" | "U64" | "I64" | "U128" | "I128"
        ) {
            bail!("unsupported mapped spec field `{field}` type `{spec_ty}`; primitive scalar required");
        }
        let matching_accounts = accounts
            .iter()
            .filter(|a| a["name"].as_str() == Some(&binding.account_type))
            .collect::<Vec<_>>();
        if matching_accounts.len() != 1 {
            bail!(
                "IDL account type `{}` must occur exactly once",
                binding.account_type
            );
        }
        let account = matching_accounts[0];
        let discriminator = account["discriminator"]
            .as_array()
            .ok_or_else(|| anyhow!("IDL account `{}` lacks discriminator", binding.account_type))?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| {
                        anyhow!("invalid discriminator byte for `{}`", binding.account_type)
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        if discriminator.is_empty() || discriminator.len() > 32 {
            bail!(
                "IDL account `{}` discriminator length must be 1..=32",
                binding.account_type
            );
        }
        let matching_types = types
            .iter()
            .filter(|t| t["name"].as_str() == Some(&binding.account_type))
            .collect::<Vec<_>>();
        if matching_types.len() != 1 {
            bail!(
                "IDL type `{}` must occur exactly once",
                binding.account_type
            );
        }
        let type_def = matching_types[0];
        if !type_def["generics"].as_array().is_none_or(Vec::is_empty) || !type_def["repr"].is_null()
        {
            bail!(
                "IDL account `{}` uses unsupported generic or zero-copy layout",
                binding.account_type
            );
        }
        if !type_def["serialization"].is_null()
            && type_def["serialization"].as_str() != Some("borsh")
        {
            bail!(
                "IDL account `{}` must use Borsh serialization",
                binding.account_type
            );
        }
        if type_def["type"]["kind"].as_str() != Some("struct") {
            bail!(
                "IDL account `{}` must be a named Borsh struct",
                binding.account_type
            );
        }
        let fields = type_def["type"]["fields"].as_array().ok_or_else(|| {
            anyhow!(
                "IDL account `{}` must be a named Borsh struct",
                binding.account_type
            )
        })?;
        if fields.is_empty() || fields.len() > 64 {
            bail!(
                "IDL account `{}` must have 1..=64 scalar fields",
                binding.account_type
            );
        }
        let mut serialized_len = 0usize;
        let mut selected_ty = None;
        let mut seen_fields = BTreeSet::new();
        for idl_field in fields {
            let name = idl_field["name"].as_str().ok_or_else(|| {
                anyhow!(
                    "IDL account `{}` has an unnamed field",
                    binding.account_type
                )
            })?;
            if !seen_fields.insert(name) {
                bail!(
                    "IDL account `{}` has duplicate field `{name}`",
                    binding.account_type
                );
            }
            let ty = idl_field["type"].as_str().and_then(scalar_type)
                .ok_or_else(|| anyhow!("IDL account `{}` field `{name}` has unsupported layout; only fixed primitive scalars are supported", binding.account_type))?;
            serialized_len += ty.1;
            if name == binding.data_field {
                selected_ty = Some(ty.0);
            }
        }
        let selected_ty = selected_ty.ok_or_else(|| {
            anyhow!(
                "IDL account `{}` has no data field `{}`",
                binding.account_type,
                binding.data_field
            )
        })?;
        if spec_ty != selected_ty {
            bail!("mapped field `{field}` type mismatch: spec `{spec_ty}`, IDL `{selected_ty}`");
        }
        if binding
            .handlers
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != handler_names
        {
            bail!("mapped field `{field}` must bind an account role for every spec handler");
        }
        for handler in &spec.handlers {
            let role = &binding.handlers[&handler.name];
            let account = handler
                .accounts
                .iter()
                .find(|a| &a.name == role)
                .ok_or_else(|| {
                    anyhow!(
                        "mapped field `{field}` handler `{}` has no account role `{role}`",
                        handler.name
                    )
                })?;
            if account.default_pubkey.is_some() || account.is_program {
                bail!(
                    "mapped field `{field}` handler `{}` role `{role}` is not a state account",
                    handler.name
                );
            }
        }
        resolved.fields.insert(
            field,
            ResolvedField {
                account_type: binding.account_type,
                data_field: binding.data_field,
                discriminator,
                serialized_len,
                handlers: binding.handlers,
            },
        );
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{ParsedHandler, ParsedHandlerAccount};

    fn spec() -> ParsedSpec {
        ParsedSpec {
            program_name: "counter".into(),
            state_fields: vec![("count".into(), "U64".into())],
            handlers: ["initialize", "increment"]
                .into_iter()
                .map(|name| ParsedHandler {
                    name: name.into(),
                    accounts: vec![ParsedHandlerAccount {
                        name: "counter".into(),
                        is_writable: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    const IDL: &str = r#"{"accounts":[{"name":"Counter","discriminator":[1,2,3]}],"types":[{"name":"Counter","type":{"kind":"struct","fields":[{"name":"count","type":"u64"}]}}]}"#;
    const MAPPING: &str = r#"{"schema_version":1,"fields":{"count":{"account_type":"Counter","data_field":"count","handlers":{"initialize":"counter","increment":"counter"}}},"setup":[{"handler":"initialize","args":{}}]}"#;

    #[test]
    fn resolves_exact_scalar_and_all_handler_roles() {
        let mapping = resolve(&spec(), IDL.as_bytes(), MAPPING.as_bytes()).unwrap();
        assert_eq!(mapping.fields["count"].discriminator, [1, 2, 3]);
        assert_eq!(mapping.fields["count"].serialized_len, 8);
        assert_eq!(mapping.setup[0].handler, "initialize");
    }

    #[test]
    fn rejects_missing_role_and_unsupported_layout() {
        let missing = MAPPING.replace("\"increment\":\"counter\"", "");
        let missing = missing.replace(",}", "}");
        assert!(resolve(&spec(), IDL.as_bytes(), missing.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("every spec handler"));
        let unsupported = IDL.replace("\"u64\"", "{\"vec\":\"u8\"}");
        assert!(resolve(&spec(), unsupported.as_bytes(), MAPPING.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("unsupported layout"));
        let zero_copy = IDL.replace("\"type\":", "\"repr\":{\"kind\":\"c\"},\"type\":");
        assert!(resolve(&spec(), zero_copy.as_bytes(), MAPPING.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("zero-copy"));
    }

    fn err(spec: &ParsedSpec, idl: &str, mapping: &str) -> String {
        format!(
            "{:#}",
            resolve(spec, idl.as_bytes(), mapping.as_bytes()).unwrap_err()
        )
    }

    #[test]
    fn rejects_unknown_names() {
        let field = MAPPING.replace("\"count\":{\"account_type\"", "\"total\":{\"account_type\"");
        assert!(err(&spec(), IDL, &field).contains("unknown spec state field `total`"));
        let account = MAPPING.replace("\"Counter\"", "\"Vault\"");
        assert!(err(&spec(), IDL, &account).contains("IDL account type `Vault`"));
        let data = MAPPING.replace("\"data_field\":\"count\"", "\"data_field\":\"value\"");
        assert!(err(&spec(), IDL, &data).contains("no data field `value`"));
        let setup = MAPPING.replace("\"handler\":\"initialize\"", "\"handler\":\"reset\"");
        assert!(err(&spec(), IDL, &setup).contains("unknown setup handler `reset`"));
        let extra = MAPPING.replace(
            "\"schema_version\":1",
            "\"schema_version\":1,\"extra\":true",
        );
        assert!(err(&spec(), IDL, &extra).contains("unknown field"));
        let version = MAPPING.replace("\"schema_version\":1", "\"schema_version\":2");
        assert!(err(&spec(), IDL, &version).contains("schema_version 1"));
    }

    #[test]
    fn rejects_type_mismatch_and_non_scalar_spec_field() {
        let idl = IDL.replace("\"u64\"", "\"u32\"");
        assert!(err(&spec(), &idl, MAPPING).contains("type mismatch: spec `U64`, IDL `U32`"));
        let mut pubkey = spec();
        pubkey.state_fields = vec![("count".into(), "Pubkey".into())];
        assert!(err(&pubkey, IDL, MAPPING).contains("primitive scalar required"));
    }

    #[test]
    fn rejects_invalid_discriminator() {
        let missing = IDL.replace(",\"discriminator\":[1,2,3]", "");
        assert!(err(&spec(), &missing, MAPPING).contains("lacks discriminator"));
        let empty = IDL.replace("[1,2,3]", "[]");
        assert!(err(&spec(), &empty, MAPPING).contains("discriminator length"));
        let wide = IDL.replace("[1,2,3]", "[1,256]");
        assert!(err(&spec(), &wide, MAPPING).contains("invalid discriminator byte"));
    }

    #[test]
    fn rejects_role_absent_or_not_a_state_account() {
        let absent = MAPPING.replace("\"increment\":\"counter\"", "\"increment\":\"vault\"");
        assert!(
            err(&spec(), IDL, &absent).contains("handler `increment` has no account role `vault`")
        );
        let mut program = spec();
        program.handlers[1].accounts[0].is_program = true;
        assert!(err(&program, IDL, MAPPING).contains("is not a state account"));
    }

    #[test]
    fn validates_setup_arguments_against_handler() {
        let mut typed = spec();
        typed.handlers[0].takes_params = vec![("seed".into(), "U8".into())];
        assert!(err(&typed, IDL, MAPPING).contains("must exactly match spec arguments"));
        let with_arg = MAPPING.replace("\"args\":{}", "\"args\":{\"seed\":7}");
        assert_eq!(
            resolve(&typed, IDL.as_bytes(), with_arg.as_bytes())
                .unwrap()
                .setup[0]
                .args,
            ["7u8"]
        );
        let overflow = MAPPING.replace("\"args\":{}", "\"args\":{\"seed\":256}");
        assert!(err(&typed, IDL, &overflow).contains("out of range"));
        let two = MAPPING.replace("[{\"handler\":\"initialize\",\"args\":{}}]", "[]");
        assert!(err(&spec(), IDL, &two).contains("exactly one explicit setup action"));
    }
}
