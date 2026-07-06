//! Profile data model: the `Pinocchio*` proof-profile structs surfaced to the
//! Kani backend, plus their merge/lookup impls.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioProofProfile {
    pub handlers: BTreeMap<String, PinocchioHandlerProfile>,
    pub pda_derivations: BTreeMap<String, PinocchioPdaDerivation>,
    pub record_layouts: BTreeMap<String, PinocchioRecordLayout>,
    pub account_layouts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PinocchioHandlerProfile {
    pub name: String,
    pub instruction_tag: Option<u8>,
    pub accounts: Vec<String>,
    pub account_roles: BTreeMap<String, PinocchioAccountRole>,
    pub token_account_bindings: BTreeMap<String, PinocchioTokenAccountBinding>,
    pub mint_decimal_bindings: BTreeMap<String, String>,
    pub account_key_derivations: BTreeMap<String, PinocchioLocalKeyDerivation>,
    pub source_expr_aliases: BTreeMap<String, String>,
    pub verified_stubs: Vec<String>,
    pub params: Vec<PinocchioParamField>,
    pub repeats: Vec<PinocchioRepeatField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioTokenAccountBinding {
    pub mint_account: Option<String>,
    pub owner_account: Option<String>,
    pub owner_key_derivation: Option<PinocchioLocalKeyDerivation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioLocalKeyDerivation {
    pub derivation: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PinocchioAccountRole {
    pub is_signer: Option<bool>,
    pub is_writable: Option<bool>,
    pub is_program: Option<bool>,
    pub account_type: Option<String>,
}

impl PinocchioAccountRole {
    pub(super) fn is_empty(&self) -> bool {
        self.is_signer.is_none()
            && self.is_writable.is_none()
            && self.is_program.is_none()
            && self.account_type.is_none()
    }

    pub(super) fn merge(&mut self, other: PinocchioAccountRole) {
        if other.is_signer.is_some() {
            self.is_signer = other.is_signer;
        }
        if other.is_writable.is_some() {
            self.is_writable = other.is_writable;
        }
        if other.is_program.is_some() {
            self.is_program = other.is_program;
        }
        if other.account_type.is_some() {
            self.account_type = other.account_type;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioParamField {
    pub name: String,
    pub rust_type: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioRepeatField {
    pub name: String,
    pub count_field: String,
    pub offset: usize,
    pub item_len: usize,
    pub item_fields: Vec<PinocchioParamField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioPdaDerivation {
    pub name: String,
    pub params: Vec<String>,
    pub param_types: BTreeMap<String, String>,
    pub local_key_derivations: BTreeMap<String, PinocchioLocalKeyDerivation>,
    pub seeds: Vec<PinocchioPdaSeed>,
    pub program_id: String,
    pub returns_tuple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioPdaSeed {
    pub expr: String,
    pub literal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioRecordLayout {
    pub name: String,
    pub len: usize,
    pub fields: Vec<PinocchioLayoutField>,
    pub repeats: Vec<PinocchioLayoutRepeat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioLayoutField {
    pub name: String,
    pub ty: String,
    pub offset: usize,
    pub len: usize,
    pub fixed_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioLayoutRepeat {
    pub name: String,
    pub ty: String,
    pub count_field: String,
    pub offset: usize,
    pub item_len: usize,
}

impl PinocchioProofProfile {
    pub(crate) fn handler(&self, name: &str) -> Option<&PinocchioHandlerProfile> {
        if let Some((base, suffix)) = name.rsplit_once('_') {
            if suffix.parse::<usize>().is_ok() {
                if let Some(handler) = self.handlers.get(base) {
                    return Some(handler);
                }
            }
        }
        self.handlers.get(name)
    }

    pub(super) fn merge_profile(&mut self, other: PinocchioProofProfile) {
        for (name, handler) in other.handlers {
            let entry =
                self.handlers
                    .entry(name.clone())
                    .or_insert_with(|| PinocchioHandlerProfile {
                        name,
                        ..Default::default()
                    });
            if handler.instruction_tag.is_some() {
                entry.instruction_tag = handler.instruction_tag;
            }
            if !handler.accounts.is_empty() {
                entry.accounts = handler.accounts;
            }
            for (account, role) in handler.account_roles {
                entry.account_roles.entry(account).or_default().merge(role);
            }
            for (account, binding) in handler.token_account_bindings {
                entry.token_account_bindings.insert(account, binding);
            }
            for (account, param) in handler.mint_decimal_bindings {
                entry.mint_decimal_bindings.insert(account, param);
            }
            for (account, derivation) in handler.account_key_derivations {
                entry.account_key_derivations.insert(account, derivation);
            }
            for (expr, alias) in handler.source_expr_aliases {
                entry.source_expr_aliases.insert(expr, alias);
            }
            for stub in handler.verified_stubs {
                if !entry.verified_stubs.contains(&stub) {
                    entry.verified_stubs.push(stub);
                }
            }
            if !handler.params.is_empty() || !handler.repeats.is_empty() {
                entry.params = handler.params;
            }
            if !handler.repeats.is_empty() {
                entry.repeats = handler.repeats;
            }
        }
        for (name, derivation) in other.pda_derivations {
            self.pda_derivations.insert(name, derivation);
        }
        for (name, layout) in other.record_layouts {
            self.record_layouts.insert(name, layout);
        }
        for (account, record) in other.account_layouts {
            self.account_layouts.insert(account, record);
        }
    }

    pub(super) fn merge_abi_schema(&mut self, schema: PinocchioAbiSchema) {
        let seed_literals = schema.seed_literals();
        for derivation in self.pda_derivations.values_mut() {
            for seed in &mut derivation.seeds {
                if seed.literal.is_none() {
                    seed.literal = seed_literals.get(&seed.expr).cloned();
                }
            }
        }

        for (name, record) in &schema.records {
            self.record_layouts
                .entry(normalize_schema_name(name))
                .or_insert_with(|| record.to_profile_layout(name, &schema.magics));
        }
        for (account, record) in schema.account_layouts() {
            self.account_layouts
                .entry(account)
                .or_insert_with(|| normalize_schema_name(&record));
        }

        for (instruction, tag) in &schema.instructions {
            let handler_name = normalize_schema_name(instruction);
            let entry = self
                .handlers
                .entry(handler_name.clone())
                .or_insert_with(|| PinocchioHandlerProfile {
                    name: handler_name.clone(),
                    ..Default::default()
                });
            entry.instruction_tag = Some(*tag);

            if let Some(accounts) = schema.accounts.get(instruction) {
                entry.accounts = accounts
                    .iter()
                    .filter(|account| !abi_account_name_is_metadata(&account.name))
                    .map(|account| normalize_schema_name(&account.name))
                    .collect();
                for account in accounts {
                    if abi_account_name_is_metadata(&account.name) {
                        continue;
                    }
                    if !account.role.is_empty() {
                        entry
                            .account_roles
                            .entry(normalize_schema_name(&account.name))
                            .or_default()
                            .merge(account.role.clone());
                    }
                }
            }

            if let Some(record_name) = schema.instruction_records.get(instruction) {
                if let Some(record) = schema.records.get(record_name) {
                    let repeat_count_fields: std::collections::BTreeSet<_> = record
                        .repeats
                        .iter()
                        .map(|repeat| repeat.count_field.as_str())
                        .collect();
                    let params: Vec<_> = record
                        .fields
                        .iter()
                        .filter(|field| !repeat_count_fields.contains(field.name.as_str()))
                        .filter_map(abi_field_to_profile_param)
                        .collect();
                    entry.params = params;

                    let repeats: Vec<_> = record
                        .repeats
                        .iter()
                        .filter_map(|repeat| {
                            let item_record =
                                schema.records.get(&repeat.ty.to_ascii_uppercase())?;
                            let item_fields: Vec<_> = item_record
                                .fields
                                .iter()
                                .filter_map(abi_field_to_profile_param)
                                .collect();
                            if item_fields.is_empty() {
                                return None;
                            }
                            Some(PinocchioRepeatField {
                                name: normalize_schema_name(&repeat.name),
                                count_field: normalize_schema_name(&repeat.count_field),
                                offset: repeat.offset,
                                item_len: repeat.item_len,
                                item_fields,
                            })
                        })
                        .collect();
                    entry.repeats = repeats;
                }
            }
        }
    }
}
