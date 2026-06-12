//! Source-derived Pinocchio proof profile.
//!
//! This module is the extraction side of the Pinocchio `--kani-impl`
//! profile layer. It reads the user's committed Pinocchio source and
//! recovers facts the generic Kani emitter needs: dispatcher tags,
//! account slice order, numeric instruction-data fields, and PDA derivation
//! seeds. Richer ABI schema integration can extend this profile without
//! teaching the Kani backend about any specific program.

use anyhow::Result;
use quote::ToTokens;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::{Expr, Item, ItemFn, Pat, Stmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinocchioProofProfile {
    pub handlers: BTreeMap<String, PinocchioHandlerProfile>,
    pub pda_derivations: BTreeMap<String, PinocchioPdaDerivation>,
    pub record_layouts: BTreeMap<String, PinocchioRecordLayout>,
    pub account_layouts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    fn is_empty(&self) -> bool {
        self.is_signer.is_none()
            && self.is_writable.is_none()
            && self.is_program.is_none()
            && self.account_type.is_none()
    }

    fn merge(&mut self, other: PinocchioAccountRole) {
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

    fn merge_profile(&mut self, other: PinocchioProofProfile) {
        for (name, handler) in other.handlers {
            let entry =
                self.handlers
                    .entry(name.clone())
                    .or_insert_with(|| PinocchioHandlerProfile {
                        name,
                        instruction_tag: None,
                        accounts: Vec::new(),
                        account_roles: BTreeMap::new(),
                        token_account_bindings: BTreeMap::new(),
                        mint_decimal_bindings: BTreeMap::new(),
                        account_key_derivations: BTreeMap::new(),
                        source_expr_aliases: BTreeMap::new(),
                        verified_stubs: Vec::new(),
                        params: Vec::new(),
                        repeats: Vec::new(),
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

    fn merge_abi_schema(&mut self, schema: PinocchioAbiSchema) {
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
                    instruction_tag: None,
                    accounts: Vec::new(),
                    account_roles: BTreeMap::new(),
                    token_account_bindings: BTreeMap::new(),
                    mint_decimal_bindings: BTreeMap::new(),
                    account_key_derivations: BTreeMap::new(),
                    source_expr_aliases: BTreeMap::new(),
                    verified_stubs: Vec::new(),
                    params: Vec::new(),
                    repeats: Vec::new(),
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

/// Infer a proof profile from a Pinocchio crate's `src/` directory. Missing
/// or unrecognized patterns produce an empty/partial profile instead of an
/// error; the Kani emitter can then fall back to spec-order generation.
#[cfg(test)]
pub(crate) fn infer_from_src_dir(src_dir: &Path) -> Result<PinocchioProofProfile> {
    infer_from_src_dirs([(src_dir.to_path_buf(), false)])
}

/// Infer a proof profile from the generated output location plus the source
/// tree implied by the `.qedspec` path. Later candidates override earlier
/// candidates, so committed source/ABI facts win over generated scaffolds.
pub(crate) fn infer_from_context(
    output_src_dir: &Path,
    spec_path: Option<&Path>,
) -> Result<PinocchioProofProfile> {
    let cwd = std::env::current_dir()?;
    let output_src_dir = absolutize_context_path(output_src_dir, &cwd);
    let spec_path = spec_path.map(|path| absolutize_context_path(path, &cwd));
    let mut candidates = vec![(output_src_dir, false)];
    if let Some(spec_path) = spec_path.as_deref() {
        if let Some(parent) = spec_path.parent() {
            candidates.push((parent.join("src"), true));
            if let Some(program_root) = parent.parent() {
                candidates.push((program_root.join("src"), true));
            }
        }
    }
    infer_from_src_dirs(candidates)
}

fn absolutize_context_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn infer_from_src_dirs<I>(src_dirs: I) -> Result<PinocchioProofProfile>
where
    I: IntoIterator<Item = (PathBuf, bool)>,
{
    let mut merged = PinocchioProofProfile {
        handlers: BTreeMap::new(),
        pda_derivations: BTreeMap::new(),
        record_layouts: BTreeMap::new(),
        account_layouts: BTreeMap::new(),
    };
    let mut candidates = Vec::<(PathBuf, bool)>::new();
    for (src_dir, include_siblings) in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        if let Some((_, existing)) = candidates.iter_mut().find(|(path, _)| path == &src_dir) {
            *existing |= include_siblings;
        } else {
            candidates.push((src_dir, include_siblings));
        }
    }
    for (src_dir, include_siblings) in candidates {
        let profile = infer_single_src_dir(&src_dir, include_siblings)?;
        merged.merge_profile(profile);
    }
    Ok(merged)
}

fn infer_single_src_dir(src_dir: &Path, include_siblings: bool) -> Result<PinocchioProofProfile> {
    let mut files = Vec::new();
    collect_rust_files(src_dir, &mut files);
    files.sort();

    let mut handlers: BTreeMap<String, PinocchioHandlerProfile> = BTreeMap::new();
    let mut pda_derivations: BTreeMap<String, PinocchioPdaDerivation> = BTreeMap::new();
    let mut parsed_files = Vec::new();
    let mut contracted_fns = BTreeMap::new();
    let mut call_graph = BTreeMap::new();
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(syntax) = syn::parse_file(&source) {
            let fns = collect_item_fns(&syntax.items);
            for item_fn in &fns {
                call_graph.insert(
                    item_fn.sig.ident.to_string(),
                    infer_called_fn_names_from_block(&item_fn.block),
                );
                if item_fn_has_kani_contract(item_fn) {
                    contracted_fns.insert(
                        item_fn.sig.ident.to_string(),
                        crate_fn_path(src_dir, &path, &item_fn.sig.ident.to_string()),
                    );
                }
            }
            parsed_files.push((path, source, syntax));
        } else {
            parsed_files.push((
                path,
                source,
                syn::File {
                    shebang: None,
                    attrs: Vec::new(),
                    items: Vec::new(),
                },
            ));
        }
    }
    for (_path, source, syntax) in parsed_files {
        if !syntax.items.is_empty() {
            let fns = collect_item_fns(&syntax.items);
            for item_fn in &fns {
                let Some(name) = process_handler_name(item_fn) else {
                    continue;
                };
                let entry = handlers
                    .entry(name.clone())
                    .or_insert_with(|| empty_handler_profile(name));
                if entry.accounts.is_empty() {
                    entry.accounts = infer_accounts_from_block(&item_fn.block);
                }
                let role_accounts = if entry.accounts.is_empty() {
                    infer_accounts_from_block(&item_fn.block)
                } else {
                    entry.accounts.clone()
                };
                for (account, role) in
                    infer_account_roles_from_block(&item_fn.block, &role_accounts)
                {
                    entry.account_roles.entry(account).or_default().merge(role);
                }
                for (account, param) in infer_mint_decimal_bindings_from_block(&item_fn.block) {
                    entry.mint_decimal_bindings.insert(account, param);
                }
                let key_account_aliases = infer_key_account_aliases_from_block(&item_fn.block);
                let local_key_derivations = infer_local_key_derivations_from_block(&item_fn.block);
                for (account, binding) in infer_token_account_bindings_from_block(
                    &item_fn.block,
                    &key_account_aliases,
                    &local_key_derivations,
                ) {
                    entry.token_account_bindings.insert(account, binding);
                }
                for (account, derivation) in
                    infer_account_key_derivations_from_block(&item_fn.block, &local_key_derivations)
                {
                    entry.account_key_derivations.insert(account, derivation);
                }
                for (expr, alias) in infer_source_expr_aliases_from_block(&item_fn.block) {
                    entry.source_expr_aliases.insert(expr, alias);
                }
                for stub in
                    infer_verified_stubs_from_block(&item_fn.block, &contracted_fns, &call_graph)
                {
                    if !entry.verified_stubs.contains(&stub) {
                        entry.verified_stubs.push(stub);
                    }
                }
                if entry.params.is_empty() {
                    entry.params = infer_params_from_block(&item_fn.block);
                }
            }
            infer_dispatch_tags_from_items(&syntax.items, &mut handlers);
            infer_pda_derivations_from_fns(&fns, &mut pda_derivations);
        } else {
            for (name, body) in process_fn_bodies(&source) {
                let entry = handlers
                    .entry(name.clone())
                    .or_insert_with(|| empty_handler_profile(name));
                if entry.accounts.is_empty() {
                    entry.accounts = infer_accounts(&body);
                }
                let role_accounts = if entry.accounts.is_empty() {
                    infer_accounts(&body)
                } else {
                    entry.accounts.clone()
                };
                for (account, role) in infer_account_roles(&body, &role_accounts) {
                    entry.account_roles.entry(account).or_default().merge(role);
                }
                for (account, param) in infer_mint_decimal_bindings(&body) {
                    entry.mint_decimal_bindings.insert(account, param);
                }
                let key_account_aliases = infer_key_account_aliases(&body);
                let local_key_derivations = infer_local_key_derivations(&body);
                for (account, binding) in infer_token_account_bindings(
                    &body,
                    &key_account_aliases,
                    &local_key_derivations,
                ) {
                    entry.token_account_bindings.insert(account, binding);
                }
                for (account, derivation) in
                    infer_account_key_derivations(&body, &local_key_derivations)
                {
                    entry.account_key_derivations.insert(account, derivation);
                }
                for (expr, alias) in infer_source_expr_aliases(&body) {
                    entry.source_expr_aliases.insert(expr, alias);
                }
                for stub in infer_verified_stubs_from_body(&body, &contracted_fns, &call_graph) {
                    if !entry.verified_stubs.contains(&stub) {
                        entry.verified_stubs.push(stub);
                    }
                }
                if entry.params.is_empty() {
                    entry.params = infer_params(&body);
                }
            }
            infer_dispatch_tags(&source, &mut handlers);
            infer_pda_derivations(&source, &mut pda_derivations);
        }
    }

    let mut profile = PinocchioProofProfile {
        handlers,
        pda_derivations,
        record_layouts: BTreeMap::new(),
        account_layouts: BTreeMap::new(),
    };
    for schema in load_nearby_abi_schemas(src_dir, include_siblings)? {
        profile.merge_abi_schema(schema);
    }
    Ok(profile)
}

fn empty_handler_profile(name: String) -> PinocchioHandlerProfile {
    PinocchioHandlerProfile {
        name,
        instruction_tag: None,
        accounts: Vec::new(),
        account_roles: BTreeMap::new(),
        token_account_bindings: BTreeMap::new(),
        mint_decimal_bindings: BTreeMap::new(),
        account_key_derivations: BTreeMap::new(),
        source_expr_aliases: BTreeMap::new(),
        verified_stubs: Vec::new(),
        params: Vec::new(),
        repeats: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PinocchioAbiSchema {
    instructions: BTreeMap<String, u8>,
    accounts: BTreeMap<String, Vec<IndexedName>>,
    records: BTreeMap<String, AbiRecord>,
    instruction_records: BTreeMap<String, String>,
    account_records: BTreeMap<String, String>,
    seeds: BTreeMap<String, String>,
    magics: BTreeMap<String, String>,
}

impl PinocchioAbiSchema {
    fn seed_literals(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (name, literal) in &self.seeds {
            out.insert(name.clone(), literal.clone());
            out.insert(
                normalize_schema_name(name).to_ascii_uppercase(),
                literal.clone(),
            );
            out.insert(normalize_schema_name(name), literal.clone());
        }
        out
    }

    fn account_layouts(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (account, record) in &self.account_records {
            out.insert(normalize_schema_name(account), record.clone());
        }
        for record in self.records.keys() {
            let normalized = normalize_schema_name(record);
            if let Some(account) = normalized.strip_suffix("_account") {
                out.entry(account.to_string())
                    .or_insert_with(|| record.clone());
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedName {
    name: String,
    index: usize,
    role: PinocchioAccountRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AbiRecord {
    fields: Vec<AbiField>,
    repeats: Vec<AbiRepeat>,
    len: usize,
}

impl AbiRecord {
    fn to_profile_layout(
        &self,
        name: &str,
        magics: &BTreeMap<String, String>,
    ) -> PinocchioRecordLayout {
        PinocchioRecordLayout {
            name: normalize_schema_name(name),
            len: self.len,
            fields: self
                .fields
                .iter()
                .map(|field| PinocchioLayoutField {
                    name: normalize_schema_name(&field.name),
                    ty: field.ty.clone(),
                    offset: field.offset,
                    len: field.len,
                    fixed_bytes: fixed_field_bytes(field, magics),
                })
                .collect(),
            repeats: self
                .repeats
                .iter()
                .map(|repeat| PinocchioLayoutRepeat {
                    name: normalize_schema_name(&repeat.name),
                    ty: normalize_schema_name(&repeat.ty),
                    count_field: normalize_schema_name(&repeat.count_field),
                    offset: repeat.offset,
                    item_len: repeat.item_len,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AbiField {
    name: String,
    ty: String,
    offset: usize,
    len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AbiRepeat {
    name: String,
    ty: String,
    max_count: String,
    count_field: String,
    offset: usize,
    item_len: usize,
}

fn load_nearby_abi_schemas(
    src_dir: &Path,
    include_siblings: bool,
) -> Result<Vec<PinocchioAbiSchema>> {
    let Some(crate_root) = src_dir.parent() else {
        return Ok(Vec::new());
    };
    let mut schema_dirs = Vec::new();
    collect_schema_dir(&crate_root.join("schema"), &mut schema_dirs);
    if include_siblings {
        if let Some(workspace_dir) = crate_root.parent() {
            if looks_like_schema_workspace(workspace_dir) {
                let Ok(entries) = std::fs::read_dir(workspace_dir) else {
                    return Ok(Vec::new());
                };
                for entry in entries {
                    let Ok(entry) = entry else {
                        continue;
                    };
                    let path = entry.path();
                    if path.is_dir() && path != crate_root {
                        collect_schema_dir(&path.join("schema"), &mut schema_dirs);
                    }
                }
            }
        }
    }
    schema_dirs.sort();
    schema_dirs.dedup();

    let mut schemas = Vec::new();
    let mut candidates = Vec::new();
    for schema_dir in schema_dirs {
        let Ok(entries) = std::fs::read_dir(schema_dir) else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("schema") {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    for path in candidates {
        if let Ok(source) = std::fs::read_to_string(path) {
            schemas.push(parse_abi_schema(&source));
        }
    }
    Ok(schemas)
}

fn collect_schema_dir(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        out.push(path.to_path_buf());
    }
}

fn looks_like_schema_workspace(path: &Path) -> bool {
    path.join("Cargo.toml").is_file()
        || path
            .parent()
            .is_some_and(|parent| parent.join("Cargo.toml").is_file())
        || direct_child_schema_dir_exists(path)
}

fn direct_child_schema_dir_exists(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let child = entry.path();
        child.is_dir() && child.join("schema").is_dir()
    })
}

fn parse_abi_schema(source: &str) -> PinocchioAbiSchema {
    let mut instructions = BTreeMap::new();
    let mut accounts: BTreeMap<String, Vec<IndexedName>> = BTreeMap::new();
    let mut records = BTreeMap::new();
    let mut instruction_records = BTreeMap::new();
    let mut account_records = BTreeMap::new();
    let mut limits = BTreeMap::new();
    let mut seeds = BTreeMap::new();
    let mut magics = BTreeMap::new();
    let mut current_record: Option<(String, Vec<AbiField>, Vec<AbiRepeat>, usize)> = None;

    for line in source.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["limit", name, value, ..] => {
                if let Ok(value) = value.parse::<usize>() {
                    limits.insert((*name).to_string(), value);
                }
            }
            ["seed", name, literal, ..] => {
                seeds.insert((*name).to_string(), (*literal).to_string());
            }
            ["magic", name, literal, ..] => {
                magics.insert((*name).to_string(), (*literal).to_string());
            }
            ["instruction", name, tag, ..] => {
                if let Ok(tag) = tag.parse::<u8>() {
                    instructions.insert((*name).to_string(), tag);
                }
            }
            ["account", instruction, name, index, rest @ ..] => {
                if let Ok(index) = index.parse::<usize>() {
                    let role = parse_account_role(rest);
                    accounts
                        .entry((*instruction).to_string())
                        .or_default()
                        .push(IndexedName {
                            name: (*name).to_string(),
                            index,
                            role,
                        });
                }
            }
            ["record", name, ..] => {
                if let Some((name, fields, repeats, len)) = current_record.take() {
                    records.insert(
                        name,
                        AbiRecord {
                            fields,
                            repeats,
                            len,
                        },
                    );
                }
                current_record = Some(((*name).to_string(), Vec::new(), Vec::new(), 0));
            }
            ["field", name, ty, ..] => {
                if let Some((_, fields, _, offset)) = current_record.as_mut() {
                    if let Some(len) = abi_type_len(ty, &records) {
                        fields.push(AbiField {
                            name: (*name).to_string(),
                            ty: (*ty).to_string(),
                            offset: *offset,
                            len,
                        });
                        *offset += len;
                    }
                }
            }
            ["repeat", name, ty, max_count, count_field, ..] => {
                if let Some((_, _, repeats, offset)) = current_record.as_mut() {
                    if let Some(item_len) = abi_type_len(ty, &records) {
                        let count = max_count
                            .parse::<usize>()
                            .ok()
                            .or_else(|| limits.get(*max_count).copied())
                            .unwrap_or(0);
                        repeats.push(AbiRepeat {
                            name: (*name).to_string(),
                            ty: (*ty).to_string(),
                            max_count: (*max_count).to_string(),
                            count_field: (*count_field).to_string(),
                            offset: *offset,
                            item_len,
                        });
                        *offset += item_len * count;
                    }
                }
            }
            ["instruction_record", instruction, record, ..] => {
                instruction_records.insert((*instruction).to_string(), (*record).to_string());
            }
            ["account_record", account, record, ..] => {
                account_records.insert((*account).to_string(), (*record).to_string());
            }
            ["end", ..] => {
                if let Some((name, fields, repeats, len)) = current_record.take() {
                    records.insert(
                        name,
                        AbiRecord {
                            fields,
                            repeats,
                            len,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    if let Some((name, fields, repeats, len)) = current_record.take() {
        records.insert(
            name,
            AbiRecord {
                fields,
                repeats,
                len,
            },
        );
    }

    for accounts in accounts.values_mut() {
        accounts.sort_by_key(|account| account.index);
    }

    PinocchioAbiSchema {
        instructions,
        accounts,
        records,
        instruction_records,
        account_records,
        seeds,
        magics,
    }
}

fn fixed_field_bytes(field: &AbiField, magics: &BTreeMap<String, String>) -> Option<Vec<u8>> {
    if !field.ty.to_ascii_lowercase().starts_with("bytes") {
        return None;
    }
    let field_name = normalize_schema_name(&field.name);
    let mut matches = magics
        .iter()
        .filter_map(|(name, literal)| {
            let magic_name = normalize_schema_name(name);
            (magic_name == field_name || magic_name.ends_with(&format!("_{field_name}")))
                .then(|| literal.as_bytes().to_vec())
        })
        .filter(|bytes| bytes.len() == field.len);

    let first = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(first)
    }
}

fn abi_type_len(ty: &str, records: &BTreeMap<String, AbiRecord>) -> Option<usize> {
    match ty.to_ascii_lowercase().as_str() {
        "u8" | "i8" | "bool" => Some(1),
        "u16" | "i16" => Some(2),
        "u32" | "i32" => Some(4),
        "u64" | "i64" => Some(8),
        "u128" | "i128" => Some(16),
        "pubkey" => Some(32),
        ty if ty.starts_with("bytes") => ty.strip_prefix("bytes")?.parse::<usize>().ok(),
        _ => records
            .get(&ty.to_ascii_uppercase())
            .map(|record| record.len),
    }
}

fn parse_account_role(tokens: &[&str]) -> PinocchioAccountRole {
    let mut role = PinocchioAccountRole::default();
    let mut i = 0usize;
    while i < tokens.len() {
        let token = tokens[i].trim_end_matches(',').to_ascii_lowercase();
        match token.as_str() {
            "signer" => role.is_signer = Some(true),
            "writable" | "mut" | "mutable" => role.is_writable = Some(true),
            "readonly" | "readable" => role.is_writable = Some(false),
            "program" => role.is_program = Some(true),
            "token" | "mint" => role.account_type = Some(token),
            "type" => {
                if let Some(next) = tokens.get(i + 1) {
                    role.account_type = Some(
                        next.trim_end_matches(',')
                            .trim_start_matches('=')
                            .to_ascii_lowercase(),
                    );
                    i += 1;
                }
            }
            _ if token.starts_with("type=") => {
                role.account_type = Some(token.trim_start_matches("type=").to_string());
            }
            _ => {}
        }
        i += 1;
    }
    role
}

fn integer_rust_type(ty: &str) -> Option<&'static str> {
    match ty.to_ascii_lowercase().as_str() {
        "u8" => Some("u8"),
        "i8" => Some("i8"),
        "u16" => Some("u16"),
        "i16" => Some("i16"),
        "u32" => Some("u32"),
        "i32" => Some("i32"),
        "u64" => Some("u64"),
        "i64" => Some("i64"),
        "u128" => Some("u128"),
        "i128" => Some("i128"),
        _ => None,
    }
}

fn abi_field_rust_type(ty: &str) -> Option<&'static str> {
    integer_rust_type(ty).or(match ty.to_ascii_lowercase().as_str() {
        "pubkey" => Some("pubkey"),
        "bool" => Some("bool"),
        _ => None,
    })
}

fn abi_account_name_is_metadata(name: &str) -> bool {
    let name = normalize_schema_name(name);
    name.ends_with("_start")
        || name.ends_with("_stride")
        || name.ends_with("_relative")
        || name.ends_with("_count")
}

fn abi_field_to_profile_param(field: &AbiField) -> Option<PinocchioParamField> {
    Some(PinocchioParamField {
        name: normalize_schema_name(&field.name),
        rust_type: abi_field_rust_type(&field.ty)
            .map(str::to_string)
            .unwrap_or_else(|| format!("unsupported:{}", field.ty)),
        start: field.offset,
        end: field.offset + field.len,
    })
}

fn normalize_schema_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "target" | ".git" | "node_modules") {
            continue;
        }
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") && name != "kani_impl.rs"
        {
            out.push(path);
        }
    }
}

fn collect_item_fns(items: &[Item]) -> Vec<&ItemFn> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Fn(item_fn) => out.push(item_fn),
            Item::Mod(item_mod) => {
                if let Some((_brace, items)) = &item_mod.content {
                    out.extend(collect_item_fns(items));
                }
            }
            _ => {}
        }
    }
    out
}

fn item_fn_has_kani_contract(item_fn: &ItemFn) -> bool {
    item_fn.attrs.iter().any(|attr| {
        let path = attr.path();
        if path.is_ident("requires") || path.is_ident("ensures") || path.is_ident("modifies") {
            return true;
        }
        let tokens = attr.to_token_stream().to_string();
        tokens.contains("kani :: requires")
            || tokens.contains("kani::requires")
            || tokens.contains("kani :: ensures")
            || tokens.contains("kani::ensures")
            || tokens.contains("kani :: modifies")
            || tokens.contains("kani::modifies")
    })
}

fn crate_fn_path(src_dir: &Path, file_path: &Path, fn_name: &str) -> String {
    let rel = file_path.strip_prefix(src_dir).unwrap_or(file_path);
    let mut modules = Vec::new();
    for component in rel.components() {
        let Some(part) = component.as_os_str().to_str() else {
            continue;
        };
        let part = part.trim_end_matches(".rs");
        if matches!(part, "lib" | "main" | "mod") {
            continue;
        }
        modules.push(part.replace('-', "_"));
    }
    if modules.is_empty() {
        format!("crate::{fn_name}")
    } else {
        format!("crate::{}::{fn_name}", modules.join("::"))
    }
}

fn infer_verified_stubs_from_block(
    block: &syn::Block,
    contracted_fns: &BTreeMap<String, String>,
    call_graph: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    verified_stubs_for_calls(
        infer_called_fn_names_from_stmts(&block.stmts),
        contracted_fns,
        call_graph,
    )
}

fn infer_verified_stubs_from_body(
    body: &str,
    contracted_fns: &BTreeMap<String, String>,
    call_graph: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let calls: Vec<_> = contracted_fns
        .iter()
        .filter(|(name, _path)| !call_arguments(body, name).is_empty())
        .map(|(name, _path)| name.clone())
        .collect();
    verified_stubs_for_calls(calls, contracted_fns, call_graph)
}

fn infer_called_fn_names_from_block(block: &syn::Block) -> Vec<String> {
    infer_called_fn_names_from_stmts(&block.stmts)
}

fn infer_called_fn_names_from_stmts(stmts: &[Stmt]) -> Vec<String> {
    let mut calls = Vec::new();
    walk_exprs_in_stmts(stmts, &mut |expr| {
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(name) = call_name(&call.func) else {
            return;
        };
        if !calls.contains(&name) {
            calls.push(name);
        }
    });
    calls
}

fn verified_stubs_for_calls(
    calls: Vec<String>,
    contracted_fns: &BTreeMap<String, String>,
    call_graph: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut stubs = Vec::new();
    let mut stack = calls;
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(path) = contracted_fns.get(&name) {
            if !stubs.contains(path) {
                stubs.push(path.clone());
            }
        }
        if let Some(callees) = call_graph.get(&name) {
            stack.extend(callees.iter().cloned());
        }
    }
    stubs.sort();
    stubs
}

fn process_handler_name(item_fn: &ItemFn) -> Option<String> {
    let name = item_fn.sig.ident.to_string();
    name.strip_prefix("process_")
        .filter(|handler| *handler != "instruction")
        .map(ToOwned::to_owned)
}

fn infer_accounts_from_block(block: &syn::Block) -> Vec<String> {
    let mut accounts = Vec::new();
    collect_accounts_from_stmts(&block.stmts, &mut accounts);
    accounts
}

fn collect_accounts_from_stmts(stmts: &[Stmt], accounts: &mut Vec<String>) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            if let Some(from_destructure) = accounts_from_destructure_pat(&local.pat) {
                if !from_destructure.is_empty() {
                    *accounts = from_destructure;
                    return;
                }
            }
            if local_init_calls(&local.init, "next_account_info") {
                if let Some(name) = simple_pat_ident(&local.pat) {
                    accounts.push(name);
                }
            }
        }
        if let Some(expr) = stmt_expr(stmt) {
            collect_accounts_from_expr(expr, accounts);
        }
    }
}

fn collect_accounts_from_expr(expr: &Expr, accounts: &mut Vec<String>) {
    match expr {
        Expr::Block(block) => collect_accounts_from_stmts(&block.block.stmts, accounts),
        Expr::If(expr_if) => {
            collect_accounts_from_stmts(&expr_if.then_branch.stmts, accounts);
            if let Some((_else, else_expr)) = &expr_if.else_branch {
                collect_accounts_from_expr(else_expr, accounts);
            }
        }
        Expr::Match(expr_match) => {
            for arm in &expr_match.arms {
                collect_accounts_from_expr(&arm.body, accounts);
            }
        }
        _ => {}
    }
}

fn accounts_from_destructure_pat(pat: &Pat) -> Option<Vec<String>> {
    let Pat::Slice(slice) = pat else {
        return None;
    };
    let mut accounts = Vec::new();
    for elem in &slice.elems {
        match elem {
            Pat::Ident(ident) => accounts.push(normalize_schema_name(&ident.ident.to_string())),
            Pat::Rest(_) => break,
            _ => return None,
        }
    }
    Some(accounts)
}

fn infer_account_roles_from_block(
    block: &syn::Block,
    accounts: &[String],
) -> BTreeMap<String, PinocchioAccountRole> {
    let mut roles = BTreeMap::<String, PinocchioAccountRole>::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        infer_role_from_expr(expr, accounts, &mut roles);
    });
    roles.retain(|_, role| !role.is_empty());
    roles
}

fn infer_role_from_expr(
    expr: &Expr,
    accounts: &[String],
    roles: &mut BTreeMap<String, PinocchioAccountRole>,
) {
    match expr {
        Expr::MethodCall(call) => {
            let receiver = normalize_expr_tokens(&call.receiver);
            let account = normalize_schema_name(&receiver);
            if accounts.iter().any(|candidate| candidate == &account) {
                let role = roles.entry(account).or_default();
                match call.method.to_string().as_str() {
                    "is_signer" => role.is_signer = Some(true),
                    "is_writable" => role.is_writable = Some(true),
                    "is_executable" | "executable" => role.is_program = Some(true),
                    _ => {}
                }
            }
        }
        Expr::Call(call) => {
            let Some(fn_name) = call_name(&call.func) else {
                return;
            };
            let args: Vec<_> = call.args.iter().collect();
            match fn_name.as_str() {
                "require_key" if args.len() >= 2 => {
                    if let Some(account) = expr_ident(args[0]) {
                        if accounts.iter().any(|candidate| candidate == &account)
                            && expr_mentions_token_program(args[1])
                        {
                            let role = roles.entry(account).or_default();
                            role.is_program = Some(true);
                            role.account_type = Some("token".to_string());
                        }
                    }
                }
                "read_mint_decimals" | "from_mint_account" => {
                    if let Some(account) = args.first().and_then(|arg| expr_ident(arg)) {
                        let role = roles.entry(account).or_default();
                        role.account_type = Some("mint".to_string());
                    }
                }
                "require_token_account" | "read_token_amount" | "write_token_amount" => {
                    if let Some(account) = args.first().and_then(|arg| expr_ident(arg)) {
                        let role = roles.entry(account).or_default();
                        role.account_type = Some("token".to_string());
                    }
                }
                "from_account_info" => {
                    if let Some(account) = args.first().and_then(|arg| expr_ident(arg)) {
                        let rendered = normalize_expr_tokens(expr);
                        let role = roles.entry(account).or_default();
                        if rendered.contains("Mint") {
                            role.account_type = Some("mint".to_string());
                        } else if rendered.contains("TokenAccount") {
                            role.account_type = Some("token".to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn infer_key_account_aliases_from_block(block: &syn::Block) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        if let Expr::Call(call) = expr {
            if call_name(&call.func).as_deref() == Some("require_key") && call.args.len() == 2 {
                let args: Vec<_> = call.args.iter().collect();
                if let (Some(account), Some(key)) = (expr_ident(args[0]), expr_ref_ident(args[1])) {
                    aliases.insert(normalize_schema_name(&key), normalize_schema_name(&account));
                }
            }
        }
    });
    aliases
}

fn infer_local_key_derivations_from_block(
    block: &syn::Block,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    collect_local_key_derivations_from_stmts(&block.stmts, &mut derivations);
    derivations
}

fn collect_local_key_derivations_from_stmts(
    stmts: &[Stmt],
    out: &mut BTreeMap<String, PinocchioLocalKeyDerivation>,
) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            if let (Some(name), Some(init)) = (simple_pat_ident(&local.pat), local.init.as_ref()) {
                if let Some(derivation) = derive_call_from_expr(&init.expr) {
                    out.insert(name, derivation);
                }
            }
        }
        if let Some(expr) = stmt_expr(stmt) {
            match expr {
                Expr::Block(block) => {
                    collect_local_key_derivations_from_stmts(&block.block.stmts, out)
                }
                Expr::If(expr_if) => {
                    collect_local_key_derivations_from_stmts(&expr_if.then_branch.stmts, out);
                    if let Some((_else, else_expr)) = &expr_if.else_branch {
                        if let Expr::Block(block) = &**else_expr {
                            collect_local_key_derivations_from_stmts(&block.block.stmts, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn infer_account_key_derivations_from_block(
    block: &syn::Block,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        let Expr::Call(call) = expr else {
            return;
        };
        if call_name(&call.func).as_deref() != Some("require_key") || call.args.len() != 2 {
            return;
        }
        let args: Vec<_> = call.args.iter().collect();
        let Some(account) = expr_ident(args[0]) else {
            return;
        };
        if let Some(derivation) = derive_call_from_expr(args[1]) {
            derivations.insert(account, derivation);
        } else if let Some(key_name) = expr_ref_ident(args[1]) {
            if let Some(local) = local_key_derivations.get(&key_name) {
                derivations.insert(account, local.clone());
            }
        }
    });
    derivations
}

fn infer_token_account_bindings_from_block(
    block: &syn::Block,
    key_account_aliases: &BTreeMap<String, String>,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioTokenAccountBinding> {
    let mut bindings = BTreeMap::new();
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(fn_name) = call_name(&call.func) else {
            return;
        };
        let args: Vec<_> = call.args.iter().collect();
        match fn_name.as_str() {
            "require_token_account" if args.len() == 3 => {
                let Some(account) = expr_ident(args[0]) else {
                    return;
                };
                let mint_account =
                    expr_key_receiver(args[1]).map(|name| normalize_schema_name(&name));
                let owner_account = expr_key_receiver(args[2])
                    .map(|name| normalize_schema_name(&name))
                    .or_else(|| {
                        expr_ref_ident(args[2])
                            .and_then(|var| key_account_aliases.get(&var).cloned())
                    });
                let owner_key_derivation = expr_ref_ident(args[2])
                    .and_then(|var| local_key_derivations.get(&var).cloned());
                bindings.insert(
                    account,
                    PinocchioTokenAccountBinding {
                        mint_account,
                        owner_account,
                        owner_key_derivation,
                    },
                );
            }
            "require_matching_token_mint" | "require_token_mint" if args.len() == 2 => {
                let (Some(account), Some(mint)) = (expr_ident(args[0]), expr_key_receiver(args[1]))
                else {
                    return;
                };
                bindings
                    .entry(account)
                    .or_insert_with(|| PinocchioTokenAccountBinding {
                        mint_account: None,
                        owner_account: None,
                        owner_key_derivation: None,
                    })
                    .mint_account = Some(normalize_schema_name(&mint));
            }
            _ => {}
        }
    });
    bindings
}

fn infer_mint_decimal_bindings_from_block(block: &syn::Block) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    for stmt in &block.stmts {
        let Stmt::Local(local) = stmt else {
            continue;
        };
        let Some(param) = simple_pat_ident(&local.pat).map(|name| normalize_schema_name(&name))
        else {
            continue;
        };
        let Some(init) = &local.init else {
            continue;
        };
        let Some(account) = read_mint_decimals_arg(&init.expr) else {
            continue;
        };
        bindings.insert(account, param);
    }
    bindings
}

fn read_mint_decimals_arg(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Try(expr_try) => read_mint_decimals_arg(&expr_try.expr),
        Expr::Call(call) => {
            let fn_name = call_name(&call.func)?;
            if fn_name != "read_mint_decimals" {
                return None;
            }
            call.args
                .first()
                .and_then(expr_ident)
                .map(|name| normalize_schema_name(&name))
        }
        _ => None,
    }
}

fn infer_source_expr_aliases_from_block(block: &syn::Block) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    collect_source_expr_aliases_from_stmts(&block.stmts, &mut aliases);
    aliases
}

fn collect_source_expr_aliases_from_stmts(stmts: &[Stmt], aliases: &mut BTreeMap<String, String>) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            let Some(name) = simple_pat_ident(&local.pat) else {
                continue;
            };
            let Some(init) = &local.init else {
                continue;
            };
            if let Some(value) = wrapper_ctor_arg(&init.expr) {
                aliases.insert(format!("{name}.0"), value);
            }
            if let Expr::Struct(expr_struct) = &*init.expr {
                for field in &expr_struct.fields {
                    let field_name = field.member.to_token_stream().to_string();
                    let value = normalize_ast_expr_alias(&field.expr);
                    if !value.is_empty() {
                        aliases.insert(
                            format!("{name}.{}", normalize_schema_name(&field_name)),
                            value.clone(),
                        );
                        aliases.insert(
                            format!("{name}.{}.0", normalize_schema_name(&field_name)),
                            value,
                        );
                    }
                }
            }
        }
    }
}

fn infer_params_from_block(block: &syn::Block) -> Vec<PinocchioParamField> {
    let mut params = Vec::new();
    collect_params_from_stmts(&block.stmts, &mut params);
    params.sort_by_key(|p| p.start);
    params
}

fn collect_params_from_stmts(stmts: &[Stmt], params: &mut Vec<PinocchioParamField>) {
    for stmt in stmts {
        if let Stmt::Local(local) = stmt {
            if let (Some(name), Some(init)) = (simple_pat_ident(&local.pat), local.init.as_ref()) {
                if let Some((rust_type, start, end)) = from_le_bytes_instruction_slice(&init.expr) {
                    params.push(PinocchioParamField {
                        name,
                        rust_type,
                        start,
                        end,
                    });
                }
            }
        }
        if let Some(expr) = stmt_expr(stmt) {
            match expr {
                Expr::Block(block) => collect_params_from_stmts(&block.block.stmts, params),
                Expr::If(expr_if) => {
                    collect_params_from_stmts(&expr_if.then_branch.stmts, params);
                    if let Some((_else, else_expr)) = &expr_if.else_branch {
                        if let Expr::Block(block) = &**else_expr {
                            collect_params_from_stmts(&block.block.stmts, params);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn infer_dispatch_tags_from_items(
    items: &[Item],
    handlers: &mut BTreeMap<String, PinocchioHandlerProfile>,
) {
    for item in items {
        match item {
            Item::Fn(item_fn) if item_fn.sig.ident == "process_instruction" => {
                walk_exprs_in_stmts(&item_fn.block.stmts, &mut |expr| {
                    let Expr::Match(expr_match) = expr else {
                        return;
                    };
                    for arm in &expr_match.arms {
                        let Some(tag) = pat_u8_literal(&arm.pat) else {
                            continue;
                        };
                        let Some(name) = first_process_callee(&arm.body) else {
                            continue;
                        };
                        let entry = handlers
                            .entry(name.clone())
                            .or_insert_with(|| empty_handler_profile(name));
                        entry.instruction_tag = Some(tag);
                    }
                });
            }
            Item::Mod(item_mod) => {
                if let Some((_brace, items)) = &item_mod.content {
                    infer_dispatch_tags_from_items(items, handlers);
                }
            }
            _ => {}
        }
    }
}

fn infer_pda_derivations_from_fns(
    item_fns: &[&ItemFn],
    derivations: &mut BTreeMap<String, PinocchioPdaDerivation>,
) {
    for item_fn in item_fns {
        let fn_name = item_fn.sig.ident.to_string();
        let Some(name) = fn_name.strip_prefix("derive_").map(normalize_schema_name) else {
            continue;
        };
        let Some((seeds, program_id)) = first_find_program_address_call_from_block(&item_fn.block)
        else {
            continue;
        };
        let params = parse_syn_fn_params(&item_fn.sig);
        let param_names = params.iter().map(|(name, _ty)| name.clone()).collect();
        let param_types = params.into_iter().collect();
        derivations.insert(
            name.clone(),
            PinocchioPdaDerivation {
                name,
                params: param_names,
                param_types,
                local_key_derivations: infer_local_key_derivations_from_block(&item_fn.block),
                seeds: seeds
                    .into_iter()
                    .map(|expr| PinocchioPdaSeed {
                        expr,
                        literal: None,
                    })
                    .collect(),
                program_id,
                returns_tuple: pda_derivation_returns_tuple(item_fn),
            },
        );
    }
}

fn pda_derivation_returns_tuple(item_fn: &ItemFn) -> bool {
    matches!(
        &item_fn.sig.output,
        syn::ReturnType::Type(_, ty) if matches!(ty.as_ref(), syn::Type::Tuple(_))
    )
}

fn stmt_expr(stmt: &Stmt) -> Option<&Expr> {
    match stmt {
        Stmt::Expr(expr, _) => Some(expr),
        Stmt::Local(local) => local.init.as_ref().map(|init| init.expr.as_ref()),
        _ => None,
    }
}

fn walk_exprs_in_stmts<'a>(stmts: &'a [Stmt], visit: &mut impl FnMut(&'a Expr)) {
    for stmt in stmts {
        if let Some(expr) = stmt_expr(stmt) {
            walk_expr(expr, visit);
        }
    }
}

fn walk_expr<'a>(expr: &'a Expr, visit: &mut impl FnMut(&'a Expr)) {
    visit(expr);
    match expr {
        Expr::Array(array) => {
            for elem in &array.elems {
                walk_expr(elem, visit);
            }
        }
        Expr::Assign(assign) => {
            walk_expr(&assign.left, visit);
            walk_expr(&assign.right, visit);
        }
        Expr::Async(expr_async) => walk_exprs_in_stmts(&expr_async.block.stmts, visit),
        Expr::Await(await_expr) => walk_expr(&await_expr.base, visit),
        Expr::Binary(binary) => {
            walk_expr(&binary.left, visit);
            walk_expr(&binary.right, visit);
        }
        Expr::Block(block) => walk_exprs_in_stmts(&block.block.stmts, visit),
        Expr::Break(expr_break) => {
            if let Some(value) = &expr_break.expr {
                walk_expr(value, visit);
            }
        }
        Expr::Call(call) => {
            walk_expr(&call.func, visit);
            for arg in &call.args {
                walk_expr(arg, visit);
            }
        }
        Expr::Cast(cast) => walk_expr(&cast.expr, visit),
        Expr::Closure(closure) => walk_expr(&closure.body, visit),
        Expr::Field(field) => walk_expr(&field.base, visit),
        Expr::ForLoop(loop_expr) => walk_exprs_in_stmts(&loop_expr.body.stmts, visit),
        Expr::Group(group) => walk_expr(&group.expr, visit),
        Expr::If(expr_if) => {
            walk_expr(&expr_if.cond, visit);
            walk_exprs_in_stmts(&expr_if.then_branch.stmts, visit);
            if let Some((_else, else_expr)) = &expr_if.else_branch {
                walk_expr(else_expr, visit);
            }
        }
        Expr::Index(index) => {
            walk_expr(&index.expr, visit);
            walk_expr(&index.index, visit);
        }
        Expr::Let(expr_let) => walk_expr(&expr_let.expr, visit),
        Expr::Loop(loop_expr) => walk_exprs_in_stmts(&loop_expr.body.stmts, visit),
        Expr::Match(expr_match) => {
            walk_expr(&expr_match.expr, visit);
            for arm in &expr_match.arms {
                walk_expr(&arm.body, visit);
            }
        }
        Expr::MethodCall(call) => {
            walk_expr(&call.receiver, visit);
            for arg in &call.args {
                walk_expr(arg, visit);
            }
        }
        Expr::Paren(paren) => walk_expr(&paren.expr, visit),
        Expr::Range(range) => {
            if let Some(start) = &range.start {
                walk_expr(start, visit);
            }
            if let Some(end) = &range.end {
                walk_expr(end, visit);
            }
        }
        Expr::Reference(reference) => walk_expr(&reference.expr, visit),
        Expr::Repeat(repeat) => {
            walk_expr(&repeat.expr, visit);
            walk_expr(&repeat.len, visit);
        }
        Expr::Return(ret) => {
            if let Some(value) = &ret.expr {
                walk_expr(value, visit);
            }
        }
        Expr::Struct(expr_struct) => {
            for field in &expr_struct.fields {
                walk_expr(&field.expr, visit);
            }
            if let Some(rest) = &expr_struct.rest {
                walk_expr(rest, visit);
            }
        }
        Expr::Try(expr_try) => walk_expr(&expr_try.expr, visit),
        Expr::TryBlock(try_block) => walk_exprs_in_stmts(&try_block.block.stmts, visit),
        Expr::Tuple(tuple) => {
            for elem in &tuple.elems {
                walk_expr(elem, visit);
            }
        }
        Expr::Unary(unary) => walk_expr(&unary.expr, visit),
        Expr::Unsafe(unsafe_expr) => walk_exprs_in_stmts(&unsafe_expr.block.stmts, visit),
        Expr::While(while_expr) => {
            walk_expr(&while_expr.cond, visit);
            walk_exprs_in_stmts(&while_expr.body.stmts, visit);
        }
        Expr::Yield(yield_expr) => {
            if let Some(value) = &yield_expr.expr {
                walk_expr(value, visit);
            }
        }
        _ => {}
    }
}

fn simple_pat_ident(pat: &Pat) -> Option<String> {
    match pat {
        Pat::Ident(ident) => Some(normalize_schema_name(&ident.ident.to_string())),
        Pat::Type(typed) => simple_pat_ident(&typed.pat),
        _ => None,
    }
}

fn local_init_calls(init: &Option<syn::LocalInit>, name: &str) -> bool {
    init.as_ref()
        .is_some_and(|init| expr_contains_call_name(&init.expr, name))
}

fn expr_contains_call_name(expr: &Expr, name: &str) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |expr| {
        if let Expr::Call(call) = expr {
            if call_name(&call.func).as_deref() == Some(name) {
                found = true;
            }
        }
    });
    found
}

fn call_name(func: &Expr) -> Option<String> {
    match func {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Expr::MethodCall(call) => Some(call.method.to_string()),
        _ => None,
    }
}

fn expr_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(path) if path.path.segments.len() == 1 => Some(normalize_schema_name(
            &path.path.segments.first()?.ident.to_string(),
        )),
        Expr::Reference(reference) => expr_ident(&reference.expr),
        Expr::Paren(paren) => expr_ident(&paren.expr),
        _ => None,
    }
}

fn expr_ref_ident(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Reference(reference) => expr_ident(&reference.expr),
        Expr::Paren(paren) => expr_ref_ident(&paren.expr),
        _ => None,
    }
}

fn expr_key_receiver(expr: &Expr) -> Option<String> {
    match expr {
        Expr::MethodCall(call) if call.method == "key" && call.args.is_empty() => {
            expr_ident(&call.receiver)
        }
        Expr::Reference(reference) => expr_key_receiver(&reference.expr),
        Expr::Paren(paren) => expr_key_receiver(&paren.expr),
        _ => None,
    }
}

fn expr_mentions_token_program(expr: &Expr) -> bool {
    let rendered = normalize_expr_tokens(expr);
    rendered.contains("SPL_TOKEN_ID")
        || rendered.contains("TOKEN_PROGRAM_ID")
        || rendered.contains("pinocchio_tkn")
}

fn derive_call_from_expr(expr: &Expr) -> Option<PinocchioLocalKeyDerivation> {
    match expr {
        Expr::Reference(reference) => derive_call_from_expr(&reference.expr),
        Expr::Paren(paren) => derive_call_from_expr(&paren.expr),
        Expr::Try(expr_try) => derive_call_from_expr(&expr_try.expr),
        Expr::Field(field) => {
            if matches!(&field.member, syn::Member::Unnamed(index) if index.index == 0) {
                derive_call_from_expr(&field.base)
            } else {
                None
            }
        }
        Expr::Call(call) => {
            let fn_name = call_name(&call.func)?;
            let derivation = fn_name.strip_prefix("derive_")?;
            Some(PinocchioLocalKeyDerivation {
                derivation: normalize_schema_name(derivation),
                args: call.args.iter().map(normalize_expr_tokens).collect(),
            })
        }
        _ => None,
    }
}

fn wrapper_ctor_arg(expr: &Expr) -> Option<String> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let Expr::Path(path) = &*call.func else {
        return None;
    };
    if path.path.segments.len() != 1 || call.args.len() != 1 {
        return None;
    }
    Some(normalize_ast_expr_alias(call.args.first()?))
}

fn normalize_ast_expr_alias(expr: &Expr) -> String {
    match expr {
        Expr::Reference(reference) => normalize_ast_expr_alias(&reference.expr),
        Expr::Unary(unary) => normalize_ast_expr_alias(&unary.expr),
        Expr::Paren(paren) => normalize_ast_expr_alias(&paren.expr),
        Expr::Call(call) if call.args.len() == 1 => normalize_ast_expr_alias(&call.args[0]),
        _ => normalize_expr_tokens(expr),
    }
}

fn from_le_bytes_instruction_slice(expr: &Expr) -> Option<(String, usize, usize)> {
    let Expr::Call(call) = peel_expr(expr) else {
        return None;
    };
    let Expr::Path(path) = &*call.func else {
        return None;
    };
    if path.path.segments.last()?.ident != "from_le_bytes" {
        return None;
    }
    let rust_type = path.path.segments.iter().rev().nth(1)?.ident.to_string();
    integer_rust_type(&rust_type)?;
    let arg = call.args.first()?;
    let (start, end) = instruction_data_range(arg)?;
    Some((rust_type, start, end))
}

fn peel_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::Try(expr_try) => peel_expr(&expr_try.expr),
        Expr::Paren(paren) => peel_expr(&paren.expr),
        Expr::Reference(reference) => peel_expr(&reference.expr),
        _ => expr,
    }
}

fn instruction_data_range(expr: &Expr) -> Option<(usize, usize)> {
    match peel_expr(expr) {
        Expr::MethodCall(call) => {
            if call.method == "get" && normalize_expr_tokens(&call.receiver) == "instruction_data" {
                let range = call.args.first()?;
                return literal_range(range);
            }
            if matches!(
                call.method.to_string().as_str(),
                "ok_or" | "map_err" | "try_into"
            ) {
                return instruction_data_range(&call.receiver);
            }
            None
        }
        Expr::Call(call) => call.args.iter().find_map(instruction_data_range),
        _ => None,
    }
}

fn literal_range(expr: &Expr) -> Option<(usize, usize)> {
    let Expr::Range(range) = expr else {
        return None;
    };
    Some((
        usize_lit(range.start.as_deref()?)?,
        usize_lit(range.end.as_deref()?)?,
    ))
}

fn usize_lit(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Lit(lit) => {
            if let syn::Lit::Int(int) = &lit.lit {
                int.base10_parse::<usize>().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn pat_u8_literal(pat: &Pat) -> Option<u8> {
    match pat {
        Pat::Lit(lit) => {
            if let syn::Lit::Int(int) = &lit.lit {
                return int.base10_parse::<u8>().ok();
            }
            None
        }
        Pat::Or(or) => or.cases.iter().find_map(pat_u8_literal),
        _ => None,
    }
}

fn first_process_callee(expr: &Expr) -> Option<String> {
    let mut found = None;
    walk_expr(expr, &mut |expr| {
        if found.is_some() {
            return;
        }
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(fn_name) = call_name(&call.func) else {
            return;
        };
        if let Some(handler) = fn_name.strip_prefix("process_") {
            found = Some(normalize_schema_name(handler));
        }
    });
    found
}

fn first_find_program_address_call_from_block(block: &syn::Block) -> Option<(Vec<String>, String)> {
    let mut found = None;
    walk_exprs_in_stmts(&block.stmts, &mut |expr| {
        if found.is_some() {
            return;
        }
        let Expr::Call(call) = expr else {
            return;
        };
        let Some(fn_name) = call_name(&call.func) else {
            return;
        };
        if !matches!(
            fn_name.as_str(),
            "find_program_address" | "try_find_program_address"
        ) {
            return;
        }
        if call.args.len() < 2 {
            return;
        }
        let Some(seeds) = seed_exprs_from_arg(&call.args[0]) else {
            return;
        };
        let program_id = normalize_program_id_arg(&call.args[1]);
        if !seeds.is_empty() && !program_id.is_empty() {
            found = Some((seeds, program_id));
        }
    });
    found
}

fn seed_exprs_from_arg(expr: &Expr) -> Option<Vec<String>> {
    let expr = match expr {
        Expr::Reference(reference) => reference.expr.as_ref(),
        _ => expr,
    };
    let Expr::Array(array) = expr else {
        return None;
    };
    Some(array.elems.iter().map(normalize_seed_ast_expr).collect())
}

fn normalize_seed_ast_expr(expr: &Expr) -> String {
    match expr {
        Expr::Reference(reference) => normalize_seed_ast_expr(&reference.expr),
        Expr::Path(path) => normalize_schema_path(&path.path),
        Expr::Array(array) => {
            let inner = array
                .elems
                .iter()
                .map(normalize_expr_tokens)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Expr::MethodCall(call) if call.method == "as_ref" => {
            format!("{}.as_ref()", normalize_seed_ast_expr(&call.receiver))
        }
        _ => normalize_expr_tokens(expr)
            .trim_start_matches("crate::")
            .to_string(),
    }
}

fn normalize_program_id_arg(expr: &Expr) -> String {
    normalize_expr_tokens(expr)
        .trim_start_matches('&')
        .trim()
        .trim_start_matches("crate::")
        .to_string()
}

fn parse_syn_fn_params(sig: &syn::Signature) -> Vec<(String, String)> {
    sig.inputs
        .iter()
        .filter_map(|arg| {
            let syn::FnArg::Typed(pat_type) = arg else {
                return None;
            };
            let name = simple_pat_ident(&pat_type.pat)?;
            Some((name, normalize_type_tokens(&pat_type.ty)))
        })
        .collect()
}

fn normalize_type_tokens(ty: &syn::Type) -> String {
    ty.to_token_stream().to_string().replace(' ', "")
}

fn normalize_expr_tokens(expr: &Expr) -> String {
    expr.to_token_stream().to_string().replace(' ', "")
}

fn normalize_schema_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
        .trim_start_matches("crate::")
        .to_string()
}

fn process_fn_bodies(source: &str) -> Vec<(String, String)> {
    let re = Regex::new(r"(?:pub\s+)?fn\s+process_([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    let mut out = Vec::new();
    for cap in re.captures_iter(source) {
        let Some(mat) = cap.get(0) else {
            continue;
        };
        let Some(open_rel) = source[mat.end()..].find('{') else {
            continue;
        };
        let open = mat.end() + open_rel;
        let Some(close) = matching_brace(source, open) else {
            continue;
        };
        out.push((cap[1].to_string(), source[open + 1..close].to_string()));
    }
    out
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn infer_accounts(body: &str) -> Vec<String> {
    let re = Regex::new(r"(?s)let\s*\[([^\]]+),\s*\.\.\]\s*=\s*accounts").unwrap();
    if let Some(cap) = re.captures(body) {
        let accounts: Vec<_> = cap[1]
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if !accounts.is_empty() {
            return accounts;
        }
    }

    let re = Regex::new(r"let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*next_account_info\(").unwrap();
    re.captures_iter(body)
        .map(|cap| cap[1].to_string())
        .collect()
}

fn infer_account_roles(body: &str, accounts: &[String]) -> BTreeMap<String, PinocchioAccountRole> {
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    let mut roles = BTreeMap::new();
    for account in accounts {
        let ident = normalize_schema_name(account);
        let mut role = PinocchioAccountRole::default();
        let dotted = format!("{ident}.");
        if compact.contains(&format!("{dotted}is_signer()"))
            || compact.contains(&format!("{dotted}is_signer"))
        {
            role.is_signer = Some(true);
        }
        if compact.contains(&format!("{dotted}is_writable()"))
            || compact.contains(&format!("{dotted}is_writable"))
        {
            role.is_writable = Some(true);
        }
        if compact.contains(&format!("{dotted}is_executable()"))
            || compact.contains(&format!("{dotted}executable()"))
            || compact.contains(&format!("{dotted}executable"))
        {
            role.is_program = Some(true);
        }
        if compact.contains(&format!("require_key({ident},&SPL_TOKEN_ID)"))
            || compact.contains(&format!(
                "require_key({ident},&pinocchio_tkn::TOKEN_PROGRAM_ID)"
            ))
            || compact.contains(&format!("{dotted}key()!=&pinocchio_tkn::TOKEN_PROGRAM_ID"))
            || compact.contains(&format!("{dotted}key()!=&SPL_TOKEN_ID"))
        {
            role.is_program = Some(true);
            role.account_type = Some("token".to_string());
        }
        if compact.contains(&format!("read_mint_decimals({ident})"))
            || compact.contains(&format!("Mint::from_account_info({ident})"))
        {
            role.account_type = Some("mint".to_string());
        }
        if compact.contains(&format!("require_token_account({ident},"))
            || compact.contains(&format!("read_token_amount({ident})"))
            || compact.contains(&format!("write_token_amount({ident},"))
            || compact.contains(&format!("TokenAccount::from_account_info({ident})"))
        {
            role.account_type = Some("token".to_string());
        }
        if !role.is_empty() {
            roles.insert(ident, role);
        }
    }
    roles
}

fn infer_key_account_aliases(body: &str) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();
    let re = Regex::new(
        r"require_key\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        aliases.insert(
            normalize_schema_name(&cap[2]),
            normalize_schema_name(&cap[1]),
        );
    }
    aliases
}

fn infer_local_key_derivations(body: &str) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    let re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*derive_([A-Za-z_][A-Za-z0-9_]*)\s*\((.*?)\)\s*(?:\.0)?\s*;",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        derivations.insert(
            normalize_schema_name(&cap[1]),
            PinocchioLocalKeyDerivation {
                derivation: normalize_schema_name(&cap[2]),
                args: split_top_level_commas(&cap[3])
                    .into_iter()
                    .map(|arg| arg.trim().to_string())
                    .filter(|arg| !arg.is_empty())
                    .collect(),
            },
        );
    }
    derivations
}

fn infer_source_expr_aliases(body: &str) -> BTreeMap<String, String> {
    let mut aliases = BTreeMap::new();

    for (name, value) in infer_wrapper_ctor_aliases(body) {
        aliases.insert(format!("{name}.0"), value);
    }

    let wrapper_re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[A-Za-z_][A-Za-z0-9_]*\s*\(\s*(.*?)\s*\)\s*;",
    )
    .unwrap();
    for cap in wrapper_re.captures_iter(body) {
        let name = normalize_schema_name(&cap[1]);
        let value = normalize_source_expr_alias(&cap[2]);
        if !value.is_empty() {
            aliases.insert(format!("{name}.0"), value);
        }
    }

    let struct_re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[A-Za-z_][A-Za-z0-9_]*\s*\{(.*?)\}\s*;",
    )
    .unwrap();
    for cap in struct_re.captures_iter(body) {
        let name = normalize_schema_name(&cap[1]);
        for field in split_top_level_commas(&cap[2]) {
            let field = field.trim();
            if field.is_empty() {
                continue;
            }
            let (field_name, value) = field
                .split_once(':')
                .map(|(field_name, value)| (field_name.trim(), value.trim()))
                .unwrap_or((field, field));
            let field_name = normalize_schema_name(field_name);
            let value = normalize_source_expr_alias(value);
            if value.is_empty() {
                continue;
            }
            aliases.insert(format!("{name}.{field_name}"), value.clone());
            aliases.insert(format!("{name}.{field_name}.0"), value);
        }
    }

    aliases
}

fn infer_wrapper_ctor_aliases(body: &str) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = body[pos..].find("let ") {
        let let_start = pos + rel;
        let after_let = let_start + "let ".len();
        let Some(eq_rel) = body[after_let..].find('=') else {
            break;
        };
        let eq = after_let + eq_rel;
        let name = body[after_let..eq].trim();
        if !is_ident(name) {
            pos = after_let;
            continue;
        }
        let rhs_start = eq + 1 + body[eq + 1..].len() - body[eq + 1..].trim_start().len();
        let ctor_len = body[rhs_start..]
            .chars()
            .take_while(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
            .map(char::len_utf8)
            .sum::<usize>();
        if ctor_len == 0 {
            pos = rhs_start;
            continue;
        }
        let open = rhs_start + ctor_len;
        if body.as_bytes().get(open) != Some(&b'(') {
            pos = rhs_start + ctor_len;
            continue;
        }
        let Some(close) = matching_bracket(body, open, '(', ')') else {
            pos = open + 1;
            continue;
        };
        if !body[close + 1..].trim_start().starts_with(';') {
            pos = close + 1;
            continue;
        }
        let value = normalize_source_expr_alias(&body[open + 1..close]);
        if !value.is_empty() {
            aliases.push((normalize_schema_name(name), value));
        }
        pos = close + 1;
    }
    aliases
}

fn normalize_source_expr_alias(expr: &str) -> String {
    let expr = expr
        .trim()
        .trim_start_matches('&')
        .trim()
        .trim_start_matches('*')
        .trim();
    if let Some(open) = expr.find('(') {
        if expr.ends_with(')') && is_ident(&expr[..open]) {
            return normalize_source_expr_alias(&expr[open + 1..expr.len() - 1]);
        }
    }
    expr.to_string()
}

fn infer_account_key_derivations(
    body: &str,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioLocalKeyDerivation> {
    let mut derivations = BTreeMap::new();
    for args in call_arguments(body, "require_key") {
        if args.len() != 2 {
            continue;
        }
        let account = args[0].trim();
        if !is_ident(account) {
            continue;
        }
        if let Some((derivation, call_args)) = strip_ref_derive_call(&args[1]) {
            derivations.insert(
                normalize_schema_name(account),
                PinocchioLocalKeyDerivation {
                    derivation: normalize_schema_name(derivation),
                    args: call_args,
                },
            );
            continue;
        }
        if let Some(key_name) = strip_ref_ident(&args[1]) {
            if let Some(local) = local_key_derivations.get(key_name) {
                derivations.insert(normalize_schema_name(account), local.clone());
            }
        }
    }
    derivations
}

fn infer_token_account_bindings(
    body: &str,
    key_account_aliases: &BTreeMap<String, String>,
    local_key_derivations: &BTreeMap<String, PinocchioLocalKeyDerivation>,
) -> BTreeMap<String, PinocchioTokenAccountBinding> {
    let mut bindings = BTreeMap::new();
    for args in call_arguments(body, "require_token_account") {
        if args.len() != 3 {
            continue;
        }
        let Some(mint_account) = strip_key_call(&args[1]) else {
            continue;
        };
        let owner_account = strip_key_call(&args[2]).map(str::to_string).or_else(|| {
            strip_ref_ident(&args[2]).and_then(|var| key_account_aliases.get(var).cloned())
        });
        let owner_key_derivation =
            strip_ref_ident(&args[2]).and_then(|var| local_key_derivations.get(var).cloned());
        bindings.insert(
            normalize_schema_name(&args[0]),
            PinocchioTokenAccountBinding {
                mint_account: Some(normalize_schema_name(mint_account)),
                owner_account,
                owner_key_derivation,
            },
        );
    }

    let re = Regex::new(
        r"require_token_account\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        bindings.insert(
            normalize_schema_name(&cap[1]),
            PinocchioTokenAccountBinding {
                mint_account: Some(normalize_schema_name(&cap[2])),
                owner_account: Some(normalize_schema_name(&cap[3])),
                owner_key_derivation: None,
            },
        );
    }

    let re = Regex::new(
        r"require_token_account\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*,\s*&\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        let owner_account = key_account_aliases
            .get(&normalize_schema_name(&cap[3]))
            .cloned();
        let owner_key_derivation = local_key_derivations
            .get(&normalize_schema_name(&cap[3]))
            .cloned();
        bindings.insert(
            normalize_schema_name(&cap[1]),
            PinocchioTokenAccountBinding {
                mint_account: Some(normalize_schema_name(&cap[2])),
                owner_account,
                owner_key_derivation,
            },
        );
    }

    let re = Regex::new(
        r"require_(?:matching_token_mint|token_mint)\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\.key\(\)\s*\)",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        bindings
            .entry(normalize_schema_name(&cap[1]))
            .or_insert_with(|| PinocchioTokenAccountBinding {
                mint_account: None,
                owner_account: None,
                owner_key_derivation: None,
            })
            .mint_account = Some(normalize_schema_name(&cap[2]));
    }

    bindings
}

fn infer_mint_decimal_bindings(body: &str) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    let re = Regex::new(
        r"let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*read_mint_decimals\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\?",
    )
    .unwrap();
    for cap in re.captures_iter(body) {
        bindings.insert(
            normalize_schema_name(&cap[2]),
            normalize_schema_name(&cap[1]),
        );
    }
    bindings
}

fn call_arguments(body: &str, fn_name: &str) -> Vec<Vec<String>> {
    let mut calls = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = body[cursor..].find(fn_name) {
        let name_start = cursor + offset;
        let mut open = name_start + fn_name.len();
        while body
            .as_bytes()
            .get(open)
            .is_some_and(u8::is_ascii_whitespace)
        {
            open += 1;
        }
        if body.as_bytes().get(open) != Some(&b'(') {
            cursor = open;
            continue;
        }
        let mut depth = 0usize;
        let mut arg_start = open + 1;
        let mut args = Vec::new();
        let mut close = None;
        for (idx, byte) in body[open..].bytes().enumerate() {
            let pos = open + idx;
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let arg = body[arg_start..pos].trim();
                        if !arg.is_empty() {
                            args.push(arg.to_string());
                        }
                        close = Some(pos + 1);
                        break;
                    }
                }
                b',' if depth == 1 => {
                    args.push(body[arg_start..pos].trim().to_string());
                    arg_start = pos + 1;
                }
                _ => {}
            }
        }
        if let Some(close) = close {
            calls.push(args);
            cursor = close;
        } else {
            break;
        }
    }
    calls
}

fn strip_key_call(expr: &str) -> Option<&str> {
    expr.trim().strip_suffix(".key()").and_then(|name| {
        let name = name.trim();
        if is_ident(name) {
            Some(name)
        } else {
            None
        }
    })
}

fn strip_ref_ident(expr: &str) -> Option<&str> {
    let ident = expr.trim().strip_prefix('&')?.trim();
    if is_ident(ident) {
        Some(ident)
    } else {
        None
    }
}

fn strip_ref_derive_call(expr: &str) -> Option<(&str, Vec<String>)> {
    let expr = expr.trim().strip_prefix('&')?.trim();
    let after_prefix = expr.strip_prefix("derive_")?;
    let open = after_prefix.find('(')?;
    let derivation = &after_prefix[..open];
    if !is_ident(derivation) {
        return None;
    }
    let arg_start = "derive_".len() + open;
    let close = matching_bracket(expr, arg_start, '(', ')')?;
    let tail = expr[close + 1..].trim();
    if !tail.is_empty() && tail != ".0" {
        return None;
    }
    let args = split_top_level_commas(&expr[arg_start + 1..close])
        .into_iter()
        .map(str::trim)
        .filter(|arg| !arg.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Some((derivation, args))
}

fn is_ident(input: &str) -> bool {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn infer_params(body: &str) -> Vec<PinocchioParamField> {
    let re = Regex::new(
        r"(?s)let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([ui](?:8|16|32|64|128))::from_le_bytes\(\s*instruction_data\s*\.get\((\d+)\.\.(\d+)\)",
    )
    .unwrap();
    let mut params = Vec::new();
    for cap in re.captures_iter(body) {
        let Ok(start) = cap[3].parse::<usize>() else {
            continue;
        };
        let Ok(end) = cap[4].parse::<usize>() else {
            continue;
        };
        params.push(PinocchioParamField {
            name: cap[1].to_string(),
            rust_type: cap[2].to_string(),
            start,
            end,
        });
    }
    params.sort_by_key(|p| p.start);
    params
}

fn infer_dispatch_tags(source: &str, handlers: &mut BTreeMap<String, PinocchioHandlerProfile>) {
    let re = Regex::new(
        r"(?m)(\d+)\s*=>\s*instructions::([A-Za-z_][A-Za-z0-9_]*)::process_([A-Za-z_][A-Za-z0-9_]*)\(",
    )
    .unwrap();
    for cap in re.captures_iter(source) {
        let Ok(tag) = cap[1].parse::<u8>() else {
            continue;
        };
        let name = cap[3].to_string();
        let entry = handlers
            .entry(name.clone())
            .or_insert_with(|| PinocchioHandlerProfile {
                name,
                instruction_tag: None,
                accounts: Vec::new(),
                account_roles: BTreeMap::new(),
                token_account_bindings: BTreeMap::new(),
                mint_decimal_bindings: BTreeMap::new(),
                account_key_derivations: BTreeMap::new(),
                source_expr_aliases: BTreeMap::new(),
                verified_stubs: Vec::new(),
                params: Vec::new(),
                repeats: Vec::new(),
            });
        entry.instruction_tag = Some(tag);
    }
}

fn infer_pda_derivations(source: &str, derivations: &mut BTreeMap<String, PinocchioPdaDerivation>) {
    for (name, params, returns_tuple, body) in derive_fn_bodies(source) {
        let Some((seeds, program_id)) = first_find_program_address_call(&body) else {
            continue;
        };
        let param_names = params
            .iter()
            .map(|(name, _ty)| name.clone())
            .collect::<Vec<_>>();
        let param_types = params.into_iter().collect::<BTreeMap<_, _>>();
        let local_key_derivations = infer_local_key_derivations(&body);
        derivations.insert(
            name.clone(),
            PinocchioPdaDerivation {
                name,
                params: param_names,
                param_types,
                local_key_derivations,
                seeds: seeds
                    .into_iter()
                    .map(|expr| PinocchioPdaSeed {
                        expr,
                        literal: None,
                    })
                    .collect(),
                program_id,
                returns_tuple,
            },
        );
    }
}

type DeriveFnBody = (String, Vec<(String, String)>, bool, String);

fn derive_fn_bodies(source: &str) -> Vec<DeriveFnBody> {
    let re = Regex::new(r"pub\s+fn\s+derive_([A-Za-z_][A-Za-z0-9_]*)\s*\(([^)]*)\)\s*(->[^{]+)?")
        .unwrap();
    let mut out = Vec::new();
    for cap in re.captures_iter(source) {
        let Some(mat) = cap.get(0) else {
            continue;
        };
        let Some(open_rel) = source[mat.end()..].find('{') else {
            continue;
        };
        let open = mat.end() + open_rel;
        let Some(close) = matching_brace(source, open) else {
            continue;
        };
        out.push((
            cap[1].to_string(),
            parse_fn_params(&cap[2]),
            cap.get(3).is_some_and(|ret| ret.as_str().contains('(')),
            source[open + 1..close].to_string(),
        ));
    }
    out
}

fn parse_fn_params(params: &str) -> Vec<(String, String)> {
    split_top_level_commas(params)
        .into_iter()
        .filter_map(|param| param.split_once(':'))
        .map(|(name, ty)| (normalize_schema_name(name.trim()), ty.trim().to_string()))
        .filter(|(name, _ty)| !name.is_empty())
        .collect()
}

fn first_find_program_address_call(body: &str) -> Option<(Vec<String>, String)> {
    for needle in ["find_program_address", "try_find_program_address"] {
        let Some(call_start) = body.find(needle) else {
            continue;
        };
        let after_name = &body[call_start + needle.len()..];
        let Some(seed_rel) = after_name.find("&[") else {
            continue;
        };
        let seed_list_start = call_start + needle.len() + seed_rel + 1;
        let Some(seed_list_end) = matching_bracket(body, seed_list_start, '[', ']') else {
            continue;
        };
        let after_seed_list = &body[seed_list_end + 1..];
        let Some(comma) = after_seed_list.find(',') else {
            continue;
        };
        let after_comma = after_seed_list[comma + 1..].trim_start();
        let program_id = after_comma
            .split([')', ';', '\n'])
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches(',')
            .trim()
            .trim_start_matches('&')
            .trim()
            .to_string();
        let seeds = split_top_level_commas(&body[seed_list_start + 1..seed_list_end])
            .into_iter()
            .map(normalize_seed_expr)
            .filter(|seed| !seed.is_empty())
            .collect::<Vec<_>>();
        if !seeds.is_empty() && !program_id.is_empty() {
            return Some((seeds, program_id));
        }
    }
    None
}

fn matching_bracket(source: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        if ch == left {
            depth += 1;
        } else if ch == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}

fn split_top_level_commas(source: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    for (index, ch) in source.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            ',' if paren_depth == 0 && bracket_depth == 0 => {
                parts.push(source[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(source[start..].trim());
    parts
}

fn normalize_seed_expr(expr: &str) -> String {
    expr.trim()
        .trim_start_matches('&')
        .trim()
        .trim_start_matches("crate::")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_account_order_params_and_dispatch_tag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("instructions")).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            r#"
pub fn process_instruction(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminant, data) = instruction_data.split_first().unwrap();
    match *discriminant {
        7 => instructions::transfer::process_transfer(accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("instructions/transfer.rs"),
            r#"
pub fn process_transfer(accounts: &[AccountInfo], instruction_data: &[u8]) -> ProgramResult {
    let [source, destination, authority, mint, token_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !authority.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !source.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }
    require_token_account(source, mint.key(), authority.key())?;
    let decimals = read_mint_decimals(mint)?;
    require_key(token_program, &SPL_TOKEN_ID)?;
    let amount = u64::from_le_bytes(
        instruction_data
            .get(0..8)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    let lane = u8::from_le_bytes(
        instruction_data
            .get(8..9)
            .ok_or(ProgramError::InvalidInstructionData)?
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?,
    );
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        let transfer = profile.handler("transfer").expect("transfer profile");
        assert_eq!(transfer.instruction_tag, Some(7));
        assert_eq!(
            transfer.accounts,
            [
                "source",
                "destination",
                "authority",
                "mint",
                "token_program"
            ]
        );
        assert_eq!(
            transfer.params,
            [
                PinocchioParamField {
                    name: "amount".to_string(),
                    rust_type: "u64".to_string(),
                    start: 0,
                    end: 8
                },
                PinocchioParamField {
                    name: "lane".to_string(),
                    rust_type: "u8".to_string(),
                    start: 8,
                    end: 9
                }
            ]
        );
        assert_eq!(
            transfer.account_roles.get("authority"),
            Some(&PinocchioAccountRole {
                is_signer: Some(true),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.account_roles.get("source"),
            Some(&PinocchioAccountRole {
                is_writable: Some(true),
                account_type: Some("token".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.account_roles.get("mint"),
            Some(&PinocchioAccountRole {
                account_type: Some("mint".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.account_roles.get("token_program"),
            Some(&PinocchioAccountRole {
                is_program: Some(true),
                account_type: Some("token".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.token_account_bindings.get("source"),
            Some(&PinocchioTokenAccountBinding {
                mint_account: Some("mint".to_string()),
                owner_account: Some("authority".to_string()),
                owner_key_derivation: None,
            })
        );
        assert!(profile.pda_derivations.is_empty());
    }

    #[test]
    fn skips_generated_kani_impl_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("kani_impl.rs"),
            "pub fn process_fake(accounts: &[AccountInfo], instruction_data: &[u8]) {}\n",
        )
        .unwrap();
        let profile = infer_from_src_dir(&src).expect("profile");
        assert!(profile.handlers.is_empty());
        assert!(profile.pda_derivations.is_empty());
    }

    #[test]
    fn infers_multiline_token_bindings_through_key_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("processor.rs"),
            r#"
fn process_rebalance(accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let mint = next_account_info(account_info_iter)?;
    let source_authority = next_account_info(account_info_iter)?;
    let source_inventory = next_account_info(account_info_iter)?;
    let destination_inventory = next_account_info(account_info_iter)?;
    let source_authority_key = derive_authority(0).0;
    let destination_authority_key = derive_authority(1).0;
    require_key(source_authority, &source_authority_key)?;
    require_token_account(source_inventory, mint.key(), &source_authority_key)?;
    require_token_account(
        destination_inventory,
        mint.key(),
        &destination_authority_key,
    )?;
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        let rebalance = profile.handler("rebalance").expect("rebalance profile");
        assert_eq!(
            rebalance.token_account_bindings.get("source_inventory"),
            Some(&PinocchioTokenAccountBinding {
                mint_account: Some("mint".to_string()),
                owner_account: Some("source_authority".to_string()),
                owner_key_derivation: Some(PinocchioLocalKeyDerivation {
                    derivation: "authority".to_string(),
                    args: vec!["0".to_string()],
                }),
            })
        );
        assert_eq!(
            rebalance
                .token_account_bindings
                .get("destination_inventory"),
            Some(&PinocchioTokenAccountBinding {
                mint_account: Some("mint".to_string()),
                owner_account: None,
                owner_key_derivation: Some(PinocchioLocalKeyDerivation {
                    derivation: "authority".to_string(),
                    args: vec!["1".to_string()],
                }),
            })
        );
    }

    #[test]
    fn infers_account_order_from_next_account_info_sequence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("processor.rs"),
            r#"
fn process_route(accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let config = next_account_info(account_info_iter)?;
    let mint = next_account_info(account_info_iter)?;
    let token_program = next_account_info(account_info_iter)?;
    let decimals = read_mint_decimals(mint)?;
    require_key(token_program, &SPL_TOKEN_ID)?;
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        let route = profile.handler("route").expect("route profile");
        assert_eq!(route.accounts, ["config", "mint", "token_program"]);
        assert_eq!(
            route.account_roles.get("mint"),
            Some(&PinocchioAccountRole {
                account_type: Some("mint".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            route.account_roles.get("token_program"),
            Some(&PinocchioAccountRole {
                is_program: Some(true),
                account_type: Some("token".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
    }

    #[test]
    fn infers_pda_derivations_from_source_and_schema_seed_literals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(dir.path().join("schema")).unwrap();
        std::fs::write(
            src.join("state.rs"),
            r#"
pub fn derive_config(program_id: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(&[CONFIG_SEED], program_id)
}

pub fn derive_vault_authority(program_id: &Pubkey, lane_id: u8) -> (Pubkey, u8) {
    pinocchio::pubkey::try_find_program_address(
        &[VAULT_AUTHORITY_SEED, &[lane_id]],
        program_id,
    )
    .unwrap()
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("schema/test.schema"),
            r#"
seed CONFIG_SEED config
seed VAULT_AUTHORITY_SEED vault-authority
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        assert_eq!(
            profile.pda_derivations.get("config"),
            Some(&PinocchioPdaDerivation {
                name: "config".to_string(),
                params: vec!["program_id".to_string()],
                param_types: BTreeMap::from([("program_id".to_string(), "&Pubkey".to_string(),)]),
                local_key_derivations: BTreeMap::new(),
                seeds: vec![PinocchioPdaSeed {
                    expr: "CONFIG_SEED".to_string(),
                    literal: Some("config".to_string()),
                }],
                program_id: "program_id".to_string(),
                returns_tuple: true,
            })
        );
        assert_eq!(
            profile.pda_derivations.get("vault_authority"),
            Some(&PinocchioPdaDerivation {
                name: "vault_authority".to_string(),
                params: vec!["program_id".to_string(), "lane_id".to_string()],
                param_types: BTreeMap::from([
                    ("program_id".to_string(), "&Pubkey".to_string()),
                    ("lane_id".to_string(), "u8".to_string()),
                ]),
                local_key_derivations: BTreeMap::new(),
                seeds: vec![
                    PinocchioPdaSeed {
                        expr: "VAULT_AUTHORITY_SEED".to_string(),
                        literal: Some("vault-authority".to_string()),
                    },
                    PinocchioPdaSeed {
                        expr: "[lane_id]".to_string(),
                        literal: None,
                    },
                ],
                program_id: "program_id".to_string(),
                returns_tuple: true,
            })
        );
    }

    #[test]
    fn infers_account_key_derivation_from_syn_require_key_direct_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            r#"
pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

fn process_route(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority, mint, vault, ..] = accounts else {
        return Ok(());
    };
    require_key(vault, &derive_token_vault(authority.key(), mint.key()).0)?;
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        let route = profile.handler("route").expect("route profile");
        assert_eq!(route.accounts, ["authority", "mint", "vault"]);
        assert_eq!(
            route.account_key_derivations.get("vault"),
            Some(&PinocchioLocalKeyDerivation {
                derivation: "token_vault".to_string(),
                args: vec!["authority.key()".to_string(), "mint.key()".to_string()],
            })
        );
        assert_eq!(
            profile.pda_derivations.get("token_vault"),
            Some(&PinocchioPdaDerivation {
                name: "token_vault".to_string(),
                params: vec!["authority".to_string(), "mint".to_string()],
                param_types: BTreeMap::from([
                    ("authority".to_string(), "&Pubkey".to_string()),
                    ("mint".to_string(), "&Pubkey".to_string()),
                ]),
                local_key_derivations: BTreeMap::new(),
                seeds: vec![
                    PinocchioPdaSeed {
                        expr: "authority.as_ref()".to_string(),
                        literal: None,
                    },
                    PinocchioPdaSeed {
                        expr: "mint.as_ref()".to_string(),
                        literal: None,
                    },
                ],
                program_id: "ASSOCIATED_TOKEN_PROGRAM_ID".to_string(),
                returns_tuple: true,
            })
        );
    }

    #[test]
    fn context_inference_keeps_source_account_key_derivations() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace.path().join("program");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::write(
            program_root.join("src/lib.rs"),
            r#"
pub fn derive_token_vault(authority: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    pinocchio::pubkey::find_program_address(
        &[authority.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

fn process_route(accounts: &[AccountInfo]) -> ProgramResult {
    let [authority, mint, vault, ..] = accounts else {
        return Ok(());
    };
    require_key(vault, &derive_token_vault(authority.key(), mint.key()).0)?;
    Ok(())
}
"#,
        )
        .unwrap();
        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(&spec_path, "spec Program\n").unwrap();
        let output_src = program_root.join("src");

        let profile = infer_from_context(&output_src, Some(&spec_path)).expect("profile");
        let route = profile.handler("route").expect("route profile");
        assert!(route.account_key_derivations.contains_key("vault"));
        assert!(profile.pda_derivations.contains_key("token_vault"));
    }

    #[test]
    fn parses_line_oriented_abi_schema_records_and_offsets() {
        let schema = parse_abi_schema(
            r#"
limit MAX_STEPS 3
instruction TRANSFER 7
account TRANSFER AUTHORITY 2
account TRANSFER DESTINATION 1
account TRANSFER SOURCE 0

record STEP
field LANE_ID u8
field AMOUNT u64
end

record TRANSFER_ARGS
field LANE_ID u8
field AMOUNT u64
repeat STEP step MAX_STEPS LANE_ID
end

record VAULT_ACCOUNT
field MAGIC bytes8
field OWNER pubkey
field BALANCE u64
end

record CUSTOM_STATE
field FLAG bool
end

magic VAULT_MAGIC VAULTMAG
instruction_record TRANSFER TRANSFER_ARGS
account_record CUSTOM CUSTOM_STATE
"#,
        );

        assert_eq!(schema.instructions.get("TRANSFER"), Some(&7));
        assert_eq!(
            schema.accounts.get("TRANSFER").unwrap(),
            &[
                IndexedName {
                    name: "SOURCE".to_string(),
                    index: 0,
                    role: PinocchioAccountRole::default()
                },
                IndexedName {
                    name: "DESTINATION".to_string(),
                    index: 1,
                    role: PinocchioAccountRole::default()
                },
                IndexedName {
                    name: "AUTHORITY".to_string(),
                    index: 2,
                    role: PinocchioAccountRole::default()
                }
            ]
        );

        let args = schema.records.get("TRANSFER_ARGS").unwrap();
        assert_eq!(
            args.fields,
            [
                AbiField {
                    name: "LANE_ID".to_string(),
                    ty: "u8".to_string(),
                    offset: 0,
                    len: 1
                },
                AbiField {
                    name: "AMOUNT".to_string(),
                    ty: "u64".to_string(),
                    offset: 1,
                    len: 8
                }
            ]
        );
        assert_eq!(
            args.repeats,
            [AbiRepeat {
                name: "STEP".to_string(),
                ty: "step".to_string(),
                max_count: "MAX_STEPS".to_string(),
                count_field: "LANE_ID".to_string(),
                offset: 9,
                item_len: 9
            }]
        );
        assert_eq!(args.len, 36);
        assert_eq!(
            schema.account_layouts(),
            BTreeMap::from([
                ("custom".to_string(), "CUSTOM_STATE".to_string()),
                ("vault".to_string(), "VAULT_ACCOUNT".to_string())
            ])
        );
    }

    #[test]
    fn abi_schema_exposes_repeated_record_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(dir.path().join("schema")).unwrap();
        std::fs::write(src.join("lib.rs"), "").unwrap();
        std::fs::write(
            dir.path().join("schema/test.schema"),
            r#"
limit MAX_TRANSFERS 4
instruction MOVE_BATCH 5

record TRANSFER
field FROM_LANE_ID u8
field TO_LANE_ID u8
field AMOUNT u64
end

record MOVE_BATCH_ARGS
field TRANSFER_COUNT u8
repeat TRANSFER transfer MAX_TRANSFERS TRANSFER_COUNT
end

record VAULT_ACCOUNT
field MAGIC bytes8
field OWNER pubkey
field BALANCE u64
end

magic VAULT_MAGIC VAULTMAG
instruction_record MOVE_BATCH MOVE_BATCH_ARGS
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        let handler = profile.handler("move_batch").expect("move batch profile");

        assert!(handler.params.is_empty());
        assert_eq!(
            handler.repeats,
            [PinocchioRepeatField {
                name: "transfer".to_string(),
                count_field: "transfer_count".to_string(),
                offset: 1,
                item_len: 10,
                item_fields: vec![
                    PinocchioParamField {
                        name: "from_lane_id".to_string(),
                        rust_type: "u8".to_string(),
                        start: 0,
                        end: 1
                    },
                    PinocchioParamField {
                        name: "to_lane_id".to_string(),
                        rust_type: "u8".to_string(),
                        start: 1,
                        end: 2
                    },
                    PinocchioParamField {
                        name: "amount".to_string(),
                        rust_type: "u64".to_string(),
                        start: 2,
                        end: 10
                    }
                ]
            }]
        );
        assert_eq!(
            profile.account_layouts.get("vault"),
            Some(&"vault_account".to_string())
        );
        assert_eq!(
            profile.record_layouts.get("vault_account"),
            Some(&PinocchioRecordLayout {
                name: "vault_account".to_string(),
                len: 48,
                fields: vec![
                    PinocchioLayoutField {
                        name: "magic".to_string(),
                        ty: "bytes8".to_string(),
                        offset: 0,
                        len: 8,
                        fixed_bytes: Some(b"VAULTMAG".to_vec()),
                    },
                    PinocchioLayoutField {
                        name: "owner".to_string(),
                        ty: "pubkey".to_string(),
                        offset: 8,
                        len: 32,
                        fixed_bytes: None,
                    },
                    PinocchioLayoutField {
                        name: "balance".to_string(),
                        ty: "u64".to_string(),
                        offset: 40,
                        len: 8,
                        fixed_bytes: None,
                    }
                ],
                repeats: Vec::new(),
            })
        );
    }

    #[test]
    fn abi_schema_fills_profile_tag_accounts_and_param_widths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(dir.path().join("schema")).unwrap();
        std::fs::write(src.join("lib.rs"), "").unwrap();
        std::fs::write(
            dir.path().join("schema/test.schema"),
            r#"
instruction TRANSFER 7
account TRANSFER AUTHORITY 2 signer
account TRANSFER DESTINATION 1 writable type token
account TRANSFER SOURCE 0 writable type token
account TRANSFER TOKEN_PROGRAM 3 program type token

record TRANSFER_ARGS
field LANE_ID u8
field AMOUNT u64
field MEMO bytes8
end

instruction_record TRANSFER TRANSFER_ARGS
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(&src).expect("profile");
        let transfer = profile.handler("transfer").expect("transfer profile");

        assert_eq!(transfer.instruction_tag, Some(7));
        assert_eq!(
            transfer.accounts,
            ["source", "destination", "authority", "token_program"]
        );
        assert_eq!(
            transfer.account_roles.get("source"),
            Some(&PinocchioAccountRole {
                is_writable: Some(true),
                account_type: Some("token".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.account_roles.get("authority"),
            Some(&PinocchioAccountRole {
                is_signer: Some(true),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.account_roles.get("token_program"),
            Some(&PinocchioAccountRole {
                is_program: Some(true),
                account_type: Some("token".to_string()),
                ..PinocchioAccountRole::default()
            })
        );
        assert_eq!(
            transfer.params,
            [
                PinocchioParamField {
                    name: "lane_id".to_string(),
                    rust_type: "u8".to_string(),
                    start: 0,
                    end: 1
                },
                PinocchioParamField {
                    name: "amount".to_string(),
                    rust_type: "u64".to_string(),
                    start: 1,
                    end: 9
                },
                PinocchioParamField {
                    name: "memo".to_string(),
                    rust_type: "unsupported:bytes8".to_string(),
                    start: 9,
                    end: 17
                }
            ]
        );
    }

    #[test]
    fn handler_lookup_falls_back_to_numeric_arity_base() {
        let mut handlers = BTreeMap::new();
        handlers.insert(
            "batch".to_string(),
            PinocchioHandlerProfile {
                name: "batch".to_string(),
                instruction_tag: Some(4),
                accounts: Vec::new(),
                account_roles: BTreeMap::new(),
                token_account_bindings: BTreeMap::new(),
                mint_decimal_bindings: BTreeMap::new(),
                account_key_derivations: BTreeMap::new(),
                source_expr_aliases: BTreeMap::new(),
                verified_stubs: Vec::new(),
                params: Vec::new(),
                repeats: Vec::new(),
            },
        );
        let profile = PinocchioProofProfile {
            handlers,
            pda_derivations: BTreeMap::new(),
            record_layouts: BTreeMap::new(),
            account_layouts: BTreeMap::new(),
        };
        assert_eq!(
            profile.handler("batch_16").unwrap().instruction_tag,
            Some(4)
        );
    }

    #[test]
    fn context_profile_prefers_spec_schema_over_generated_output() {
        let output_dir = tempfile::tempdir().expect("output tempdir");
        let output_src = output_dir.path().join("programs/src");
        std::fs::create_dir_all(&output_src).unwrap();
        std::fs::write(
            output_src.join("lib.rs"),
            r#"
pub fn process_instruction(
    _program_id: &pinocchio::pubkey::Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminant, data) = instruction_data.split_first().unwrap();
    match *discriminant {
        3 => instructions::transfer::process_transfer(accounts, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
"#,
        )
        .unwrap();

        let workspace_dir = tempfile::tempdir().expect("workspace tempdir");
        let program_root = workspace_dir.path().join("program");
        let abi_root = workspace_dir.path().join("program-abi");
        std::fs::create_dir_all(program_root.join("src")).unwrap();
        std::fs::create_dir_all(program_root.join("verification")).unwrap();
        std::fs::create_dir_all(abi_root.join("schema")).unwrap();
        std::fs::write(program_root.join("src/lib.rs"), "").unwrap();
        std::fs::write(
            abi_root.join("schema/program.schema"),
            r#"
instruction TRANSFER 1
account TRANSFER SOURCE 0
account TRANSFER DESTINATION 1

record TRANSFER_ARGS
field AMOUNT u64
end

instruction_record TRANSFER TRANSFER_ARGS
"#,
        )
        .unwrap();
        let spec_path = program_root.join("verification/program.qedspec");
        std::fs::write(&spec_path, "spec Program\n").unwrap();

        let profile = infer_from_context(&output_src, Some(&spec_path)).expect("profile");
        let transfer = profile.handler("transfer").expect("transfer profile");

        assert_eq!(transfer.instruction_tag, Some(1));
        assert_eq!(transfer.accounts, ["source", "destination"]);
        assert_eq!(
            transfer.params,
            [PinocchioParamField {
                name: "amount".to_string(),
                rust_type: "u64".to_string(),
                start: 0,
                end: 8
            }]
        );
    }

    #[test]
    fn infers_verified_stubs_from_contracted_helper_calls() {
        let dir = tempfile::tempdir().expect("src tempdir");
        let src = dir.path();
        std::fs::write(src.join("lib.rs"), "mod processor;\nmod validation;\n").unwrap();
        std::fs::write(
            src.join("validation.rs"),
            r#"
#[cfg_attr(kani, kani::requires(amount > 0))]
#[cfg_attr(kani, kani::ensures(|result| result.is_ok()))]
pub fn check_amount_inner(amount: u64) -> Result<(), ()> {
    if amount == 0 { Err(()) } else { Ok(()) }
}

pub fn check_amount(amount: u64) -> Result<(), ()> {
    check_amount_inner(amount)
}
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("processor.rs"),
            r#"
use crate::validation::check_amount;

pub fn process_transfer(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    check_amount(amount)?;
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(src).expect("profile");
        let transfer = profile.handler("transfer").expect("transfer profile");

        assert_eq!(
            transfer.verified_stubs,
            ["crate::validation::check_amount_inner".to_string()]
        );
    }

    #[test]
    fn infers_verified_stubs_from_contracted_bool_guard_helpers() {
        let dir = tempfile::tempdir().expect("src tempdir");
        let src = dir.path();
        std::fs::write(src.join("lib.rs"), "mod processor;\nmod validation;\n").unwrap();
        std::fs::write(
            src.join("validation.rs"),
            r#"
#[cfg(kani)]
#[cfg_attr(kani, kani::requires(amount <= 100))]
#[cfg_attr(kani, kani::ensures(|result| *result))]
pub(crate) fn amount_in_envelope_kani(amount: u64) -> bool {
    amount <= 100
}

pub fn check_amount(amount: u64) -> Result<(), ()> {
    #[cfg(kani)]
    {
        if !amount_in_envelope_kani(amount) {
            return Err(());
        }
        return Ok(());
    }

    #[cfg(not(kani))]
    {
        if amount <= 100 { Ok(()) } else { Err(()) }
    }
}
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("processor.rs"),
            r#"
use crate::validation::check_amount;

pub fn process_transfer(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    check_amount(amount)?;
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(src).expect("profile");
        let transfer = profile.handler("transfer").expect("transfer profile");

        assert_eq!(
            transfer.verified_stubs,
            ["crate::validation::amount_in_envelope_kani".to_string()]
        );
    }

    #[test]
    fn infers_verified_stubs_from_cfg_kani_and_not_kani_helper_branches() {
        let dir = tempfile::tempdir().expect("src tempdir");
        let src = dir.path();
        std::fs::write(src.join("lib.rs"), "mod processor;\nmod validation;\n").unwrap();
        std::fs::write(
            src.join("validation.rs"),
            r#"
#[cfg(kani)]
#[cfg_attr(kani, kani::requires(amount <= 100))]
#[cfg_attr(kani, kani::ensures(|result| *result))]
pub(crate) fn amount_in_envelope_kani(amount: u64) -> bool {
    amount <= 100
}

#[cfg(kani)]
#[cfg_attr(kani, kani::requires(amount <= 100))]
#[cfg_attr(kani, kani::ensures(|result| *result))]
pub(crate) fn amount_threshold_kani(amount: u64) -> bool {
    amount <= 100
}

pub fn check_amount(amount: u64) -> Result<(), ()> {
    #[cfg(kani)]
    {
        if !amount_in_envelope_kani(amount) {
            return Err(());
        }
        return Ok(());
    }

    #[cfg(not(kani))]
    {
        if !amount_threshold_kani(amount) {
            return Err(());
        }
        Ok(())
    }
}
"#,
        )
        .unwrap();
        std::fs::write(
            src.join("processor.rs"),
            r#"
use crate::validation::check_amount;

pub fn process_transfer(accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let amount = u64::from_le_bytes(data[0..8].try_into().unwrap());
    check_amount(amount)?;
    Ok(())
}
"#,
        )
        .unwrap();

        let profile = infer_from_src_dir(src).expect("profile");
        let transfer = profile.handler("transfer").expect("transfer profile");

        assert_eq!(
            transfer.verified_stubs,
            [
                "crate::validation::amount_in_envelope_kani".to_string(),
                "crate::validation::amount_threshold_kani".to_string(),
            ]
        );
    }
}
