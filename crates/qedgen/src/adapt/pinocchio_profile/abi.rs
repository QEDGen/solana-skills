//! ABI-schema parsing: reads committed `.schema` files into an intermediate
//! `PinocchioAbiSchema` and the helpers that fold one into the profile.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinocchioAbiSchema {
    pub(super) instructions: BTreeMap<String, u8>,
    pub(super) accounts: BTreeMap<String, Vec<IndexedName>>,
    pub(super) records: BTreeMap<String, AbiRecord>,
    pub(super) instruction_records: BTreeMap<String, String>,
    pub(super) account_records: BTreeMap<String, String>,
    pub(super) seeds: BTreeMap<String, String>,
    pub(super) magics: BTreeMap<String, String>,
}

impl PinocchioAbiSchema {
    pub(super) fn seed_literals(&self) -> BTreeMap<String, String> {
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

    pub(super) fn account_layouts(&self) -> BTreeMap<String, String> {
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
pub(super) struct IndexedName {
    pub(super) name: String,
    pub(super) index: usize,
    pub(super) role: PinocchioAccountRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AbiRecord {
    pub(super) fields: Vec<AbiField>,
    pub(super) repeats: Vec<AbiRepeat>,
    pub(super) len: usize,
}

impl AbiRecord {
    pub(super) fn to_profile_layout(
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
pub(super) struct AbiField {
    pub(super) name: String,
    pub(super) ty: String,
    pub(super) offset: usize,
    pub(super) len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AbiRepeat {
    pub(super) name: String,
    pub(super) ty: String,
    pub(super) max_count: String,
    pub(super) count_field: String,
    pub(super) offset: usize,
    pub(super) item_len: usize,
}

pub(super) fn load_nearby_abi_schemas(
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

pub(super) fn parse_abi_schema(source: &str) -> PinocchioAbiSchema {
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

pub(super) fn integer_rust_type(ty: &str) -> Option<&'static str> {
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

pub(super) fn abi_account_name_is_metadata(name: &str) -> bool {
    let name = normalize_schema_name(name);
    name.ends_with("_start")
        || name.ends_with("_stride")
        || name.ends_with("_relative")
        || name.ends_with("_count")
}

pub(super) fn abi_field_to_profile_param(field: &AbiField) -> Option<PinocchioParamField> {
    Some(PinocchioParamField {
        name: normalize_schema_name(&field.name),
        rust_type: abi_field_rust_type(&field.ty)
            .map(str::to_string)
            .unwrap_or_else(|| format!("unsupported:{}", field.ty)),
        start: field.offset,
        end: field.offset + field.len,
    })
}

pub(super) fn normalize_schema_name(name: &str) -> String {
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
