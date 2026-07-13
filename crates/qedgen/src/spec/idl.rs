//! IDL parsing and pattern inference.
//!
//! Decodes IDL JSON to typed structs and infers patterns (signers,
//! writable accounts, PDAs, has_one relations, token-program presence, close
//! semantics, numeric args). Consumed by `idl2spec` (IDL → `.qedspec`
//! scaffolder) and `interface_gen` (IDL → spec interface block).
//!
//! Two wire formats are accepted (#197):
//! - **Anchor** (pre-0.30 and 0.30+) — parsed directly into [`Idl`].
//! - **Codama IR** (`{"kind":"rootNode","program":{...}}`, the IDL format
//!   Pinocchio programs ship) — normalized into the same Anchor-shaped
//!   [`Idl`] by [`codama_to_idl`], so `qedgen spec --idl` and
//!   `qedgen interface --idl` work identically for both. Names are
//!   snake_cased (Codama uses camelCase), type nodes are lowered to the
//!   Anchor labels `idl2spec::map_type` understands, and `omitted`
//!   discriminator arguments become the instruction `discriminator`.

use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub(crate) struct Idl {
    pub metadata: IdlMetadata,
    /// Anchor 0.30+ puts the program ID at the root; older IDLs under
    /// metadata. We fall back on both.
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub instructions: Vec<IdlInstruction>,
    #[serde(default)]
    pub types: Vec<IdlTypeDef>,
    #[serde(default)]
    pub errors: Vec<IdlError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlMetadata {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlInstruction {
    pub name: String,
    #[serde(default)]
    pub docs: Vec<String>,
    #[serde(default)]
    pub accounts: Vec<IdlAccount>,
    #[serde(default)]
    pub args: Vec<IdlArg>,
    /// Anchor 0.30+ 8-byte discriminator; when absent (older IDLs) the
    /// interface generator leaves the `discriminant` line as a TODO.
    #[serde(default)]
    pub discriminator: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlAccount {
    pub name: String,
    #[serde(default)]
    pub signer: bool,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub pda: Option<IdlPda>,
    #[serde(default)]
    pub relations: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlPda {
    #[serde(default)]
    pub seeds: Vec<IdlSeed>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlSeed {
    #[serde(default)]
    #[allow(dead_code)]
    // IDL wire-shape field; deserialized but unread (seeds resolve via value/path)
    pub kind: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlArg {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlTypeDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: IdlTypeBody,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlTypeBody {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub fields: Vec<IdlField>,
    // Enum defined types (`kind == "enum"`): the sum-type variants. Empty for
    // struct types. Anchor IDLs carry `{"kind":"enum","variants":[{"name":..}]}`
    // natively; the Codama normalizer synthesizes the same shape from an
    // `enumTypeNode`. Variant data payloads are intentionally not modeled —
    // account-state / authority-type enums are fieldless, and a name-only sum
    // type is the correct fidelity fix (#202); data variants degrade to name-only.
    #[serde(default)]
    pub variants: Vec<IdlEnumVariant>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlEnumVariant {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdlError {
    pub name: String,
    #[allow(dead_code)] // IDL wire-shape field; deserialized but unread (errors surface by name)
    pub msg: String,
}

/// First-pass pattern inference over an IDL instruction; drives `idl2spec`
/// scaffolding heuristics. `#[allow(dead_code)]` fields are kept as a stable
/// analysis surface for future scaffolders.
pub(crate) struct InstructionAnalysis {
    pub name: String,
    #[allow(dead_code)]
    pub display_name: String,
    pub docs: String,
    pub signers: Vec<String>,
    #[allow(dead_code)]
    pub writable_accounts: Vec<String>,
    #[allow(dead_code)]
    pub pda_accounts: Vec<String>,
    #[allow(dead_code)]
    pub has_one_relations: Vec<(String, String)>, // (account, related_to)
    #[allow(dead_code)]
    pub args: Vec<(String, String)>, // (name, type)
    pub has_token_program: bool,
    pub has_close_semantics: bool,
    #[allow(dead_code)]
    pub has_numeric_args: bool,
}

pub(crate) fn parse_idl(idl_path: &Path) -> Result<(Idl, Vec<InstructionAnalysis>)> {
    let idl_source = std::fs::read_to_string(idl_path)?;
    let raw: serde_json::Value = serde_json::from_str(&idl_source)?;
    // Codama IR carries the program under a `program` envelope; an Anchor
    // IDL never has one. Normalize Codama into the Anchor-shaped `Idl`.
    let idl: Idl = if raw
        .get("program")
        .is_some_and(|p| p.get("instructions").is_some())
    {
        codama_to_idl(&raw)?
    } else {
        serde_json::from_str(&idl_source)?
    };
    let analyses: Vec<InstructionAnalysis> =
        idl.instructions.iter().map(analyze_instruction).collect();
    Ok((idl, analyses))
}

/// camelCase → snake_case (Codama names instructions/accounts/args in
/// camelCase; qedspec conventions and Anchor 0.30 IDLs are snake_case).
fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Lower a Codama type node to the Anchor-style label `idl2spec::map_type`
/// and `interface_gen` understand. Recognized nodes map precisely; anything
/// else falls back to the node's `kind` string — the same
/// fail-visible-not-silent behavior unknown Anchor types get.
fn codama_type_label(node: &serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    // Anchor string shorthand may appear nested inside Codama trees.
    if node.is_string() {
        return node.clone();
    }
    let kind = node.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    match kind {
        "numberTypeNode" => node.get("format").cloned().unwrap_or_else(|| json!("u64")),
        "publicKeyTypeNode" => json!("pubkey"),
        "booleanTypeNode" => json!("bool"),
        "stringTypeNode" => json!("string"),
        "bytesTypeNode" => json!("bytes"),
        "optionTypeNode" | "zeroableOptionTypeNode" => {
            let inner = node
                .get("item")
                .map(codama_type_label)
                .unwrap_or_else(|| json!("u64"));
            json!({ "option": inner })
        }
        "definedTypeLinkNode" => {
            let name = node.get("name").and_then(|n| n.as_str()).unwrap_or("");
            json!({ "defined": name })
        }
        "arrayTypeNode" => {
            let inner = node
                .get("item")
                .map(codama_type_label)
                .unwrap_or_else(|| json!("u8"));
            let count = node
                .get("count")
                .and_then(|c| c.get("value"))
                .cloned()
                .unwrap_or_else(|| json!(0));
            json!({ "array": [inner, count] })
        }
        other => json!(other),
    }
}

/// Lower a Codama `pdaNode`'s `seeds[]` to the Anchor-shaped seed array
/// `IdlPda` deserializes (#200):
/// - `constantPdaSeedNode` with a `stringValueNode` → `{"kind":"const",
///   "value":[<utf8 bytes>]}` (what `render_pda_seeds` decodes back to a
///   string literal); other constant value kinds keep an empty value →
///   the `"const"` placeholder.
/// - `variablePdaSeedNode { name }` → `{"kind":"variable","path":<snake>}`
///   (an account/arg reference).
fn codama_pda_seeds(pda_node: &serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    let seeds: Vec<serde_json::Value> = pda_node
        .get("seeds")
        .and_then(|s| s.as_array())
        .map(|seeds| {
            seeds
                .iter()
                .filter_map(|seed| {
                    let kind = seed.get("kind").and_then(|k| k.as_str())?;
                    match kind {
                        "constantPdaSeedNode" => {
                            let bytes: Vec<serde_json::Value> = seed
                                .get("value")
                                .filter(|v| {
                                    v.get("kind").and_then(|k| k.as_str())
                                        == Some("stringValueNode")
                                })
                                .and_then(|v| v.get("string"))
                                .and_then(|s| s.as_str())
                                .map(|s| s.bytes().map(|b| json!(b)).collect())
                                .unwrap_or_default();
                            Some(json!({ "kind": "const", "value": bytes }))
                        }
                        "variablePdaSeedNode" => {
                            let name = seed.get("name").and_then(|n| n.as_str())?;
                            Some(json!({
                                "kind": "variable",
                                "path": camel_to_snake(name),
                            }))
                        }
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    json!(seeds)
}

/// Codama struct fields (`structTypeNode.fields[]` of
/// `structFieldTypeNode { name, type }`) → Anchor-shaped field list.
fn codama_struct_fields(struct_node: &serde_json::Value) -> Vec<serde_json::Value> {
    struct_node
        .get("fields")
        .and_then(|f| f.as_array())
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| {
                    let name = f.get("name").and_then(|n| n.as_str())?;
                    let ty = f.get("type").map(codama_type_label)?;
                    Some(serde_json::json!({ "name": camel_to_snake(name), "type": ty }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Codama `enumTypeNode.variants[]` → Anchor-shaped variant list
/// (`[{"name": "<PascalCase>"}]`). Each variant node
/// (`enumEmptyVariantTypeNode` / `enumStructVariantTypeNode` /
/// `enumTupleVariantTypeNode`) carries a `name`; variant data payloads are not
/// modeled (#202 renders a name-only sum type). Names are PascalCased to match
/// DSL variant convention (`uninitialized` → `Uninitialized`).
fn codama_enum_variants(enum_node: &serde_json::Value) -> Vec<serde_json::Value> {
    enum_node
        .get("variants")
        .and_then(|v| v.as_array())
        .map(|variants| {
            variants
                .iter()
                .filter_map(|v| {
                    let name = v.get("name").and_then(|n| n.as_str())?;
                    let pascal = snake_to_title(&camel_to_snake(name)).replace(' ', "");
                    Some(serde_json::json!({ "name": pascal }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Normalize a Codama IR root into the Anchor-shaped [`Idl`] (#197). Builds
/// an Anchor-form JSON value and reuses the existing serde derives, so the
/// two formats can never drift in what downstream consumers see.
///
/// Mapping:
/// - `program.name` → `metadata.name`; `program.publicKey` → `address`.
/// - `instructions[]`: `arguments[]` → `args[]` (dropping
///   `defaultValueStrategy: "omitted"` args — a single-byte
///   `numberValueNode` default among them becomes the `discriminator`);
///   account `isSigner`/`isWritable` → `signer`/`writable`; a `pda` object
///   or `pdaValueNode` default marks the account PDA, and a `pdaLinkNode`
///   under it resolves through `program.pdas[]` to real seeds (#200) —
///   only a link with no matching definition degrades to the seedless
///   TODO marker. Codama has no `has_one`, so `relations` stays empty.
/// - `definedTypes[]` AND `accounts[]` (account-state structs live under
///   `accountNode.data` in Codama) → `types[]`, so state-layout inference
///   sees them exactly like Anchor `types`.
/// - `errors[]` (`message`) → `errors[]` (`msg`).
fn codama_to_idl(root: &serde_json::Value) -> Result<Idl> {
    use serde_json::json;
    let program = root
        .get("program")
        .expect("caller checked program presence");
    let name = program
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("program");
    let address = program.get("publicKey").and_then(|k| k.as_str());

    // `program.pdas[]` seed definitions, keyed by pdaNode name — resolved
    // when an instruction account's `pdaValueNode` links to one (#200).
    let pda_defs: std::collections::HashMap<String, serde_json::Value> = program
        .get("pdas")
        .and_then(|v| v.as_array())
        .map(|pdas| {
            pdas.iter()
                .filter_map(|p| {
                    let pname = p.get("name").and_then(|n| n.as_str())?;
                    Some((pname.to_string(), codama_pda_seeds(p)))
                })
                .collect()
        })
        .unwrap_or_default();

    let instructions: Vec<serde_json::Value> = program
        .get("instructions")
        .and_then(|v| v.as_array())
        .map(|ixs| {
            ixs.iter()
                .filter_map(|ix| {
                    let ix_name = ix.get("name").and_then(|n| n.as_str())?;
                    let mut discriminator: Vec<u8> = Vec::new();
                    let args: Vec<serde_json::Value> = ix
                        .get("arguments")
                        .and_then(|a| a.as_array())
                        .map(|args| {
                            args.iter()
                                .filter_map(|a| {
                                    let omitted =
                                        a.get("defaultValueStrategy").and_then(|s| s.as_str())
                                            == Some("omitted");
                                    if omitted {
                                        // A fixed single-byte default among the
                                        // omitted args IS the discriminator.
                                        if let Some(n) = a
                                            .get("defaultValue")
                                            .filter(|d| {
                                                d.get("kind").and_then(|k| k.as_str())
                                                    == Some("numberValueNode")
                                            })
                                            .and_then(|d| d.get("number"))
                                            .and_then(|n| n.as_u64())
                                        {
                                            if discriminator.is_empty() && n <= u8::MAX as u64 {
                                                discriminator.push(n as u8);
                                            }
                                        }
                                        return None;
                                    }
                                    let arg_name = a.get("name").and_then(|n| n.as_str())?;
                                    let ty = a.get("type").map(codama_type_label)?;
                                    Some(json!({
                                        "name": camel_to_snake(arg_name),
                                        "type": ty,
                                    }))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let accounts: Vec<serde_json::Value> = ix
                        .get("accounts")
                        .and_then(|a| a.as_array())
                        .map(|accts| {
                            accts
                                .iter()
                                .filter_map(|a| {
                                    let acct_name = a.get("name").and_then(|n| n.as_str())?;
                                    let signer = a
                                        .get("isSigner")
                                        .or_else(|| a.get("signer"))
                                        .and_then(|b| b.as_bool())
                                        .unwrap_or(false);
                                    let writable = a
                                        .get("isWritable")
                                        .or_else(|| a.get("writable"))
                                        .and_then(|b| b.as_bool())
                                        .unwrap_or(false);
                                    let default = a.get("defaultValue");
                                    let is_pda_value = default
                                        .and_then(|d| d.get("kind"))
                                        .and_then(|k| k.as_str())
                                        == Some("pdaValueNode");
                                    let mut acct = json!({
                                        "name": camel_to_snake(acct_name),
                                        "signer": signer,
                                        "writable": writable,
                                    });
                                    if a.get("pda").is_some() || is_pda_value {
                                        // Resolve the pdaLinkNode through
                                        // `program.pdas[]` (#200); an inline
                                        // pdaNode carries its own seeds; a
                                        // dangling link keeps the seedless
                                        // marker (→ scaffold TODO).
                                        let seeds = default
                                            .and_then(|d| d.get("pda"))
                                            .and_then(|link| {
                                                match link.get("kind").and_then(|k| k.as_str()) {
                                                    Some("pdaNode") => Some(codama_pda_seeds(link)),
                                                    _ => link
                                                        .get("name")
                                                        .and_then(|n| n.as_str())
                                                        .and_then(|n| pda_defs.get(n))
                                                        .cloned(),
                                                }
                                            })
                                            .unwrap_or_else(|| json!([]));
                                        acct["pda"] = json!({ "seeds": seeds });
                                    }
                                    Some(acct)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(json!({
                        "name": camel_to_snake(ix_name),
                        "docs": ix.get("docs").cloned().unwrap_or_else(|| json!([])),
                        "accounts": accounts,
                        "args": args,
                        "discriminator": discriminator,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    // State-layout candidates: Codama's `definedTypes[]` plus the struct
    // under each `accountNode.data` — both become Anchor-shaped `types[]`.
    let mut types: Vec<serde_json::Value> = Vec::new();
    if let Some(dts) = program.get("definedTypes").and_then(|v| v.as_array()) {
        for dt in dts {
            let Some(tname) = dt.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(ty) = dt.get("type") else { continue };
            // #202: an `enumTypeNode` defined type carries variants, not fields.
            // Stamp it as an enum sum type so the render keeps the real variants
            // instead of yielding zero fields → generic lifecycle state.
            let type_body = if ty.get("kind").and_then(|k| k.as_str()) == Some("enumTypeNode") {
                json!({ "kind": "enum", "variants": codama_enum_variants(ty) })
            } else {
                json!({ "kind": "struct", "fields": codama_struct_fields(ty) })
            };
            types.push(json!({
                "name": snake_to_title(&camel_to_snake(tname)).replace(' ', ""),
                "type": type_body,
            }));
        }
    }
    if let Some(accts) = program.get("accounts").and_then(|v| v.as_array()) {
        for acct in accts {
            let Some(aname) = acct.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(data) = acct.get("data") else {
                continue;
            };
            types.push(json!({
                "name": snake_to_title(&camel_to_snake(aname)).replace(' ', ""),
                "type": { "kind": "struct", "fields": codama_struct_fields(data) },
            }));
        }
    }

    let errors: Vec<serde_json::Value> = program
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|errs| {
            errs.iter()
                .filter_map(|e| {
                    let ename = e.get("name").and_then(|n| n.as_str())?;
                    let msg = e
                        .get("message")
                        .or_else(|| e.get("msg"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");
                    Some(json!({
                        "name": snake_to_title(&camel_to_snake(ename)).replace(' ', ""),
                        "msg": msg,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let anchor_shaped = json!({
        "metadata": { "name": name },
        "address": address,
        "instructions": instructions,
        "types": types,
        "errors": errors,
    });
    Ok(serde_json::from_value(anchor_shaped)?)
}

pub(crate) fn analyze_instruction(ix: &IdlInstruction) -> InstructionAnalysis {
    let signers: Vec<String> = ix
        .accounts
        .iter()
        .filter(|a| a.signer)
        .map(|a| a.name.clone())
        .collect();

    let writable_accounts: Vec<String> = ix
        .accounts
        .iter()
        .filter(|a| a.writable)
        .map(|a| a.name.clone())
        .collect();

    let pda_accounts: Vec<String> = ix
        .accounts
        .iter()
        .filter(|a| a.pda.is_some())
        .map(|a| a.name.clone())
        .collect();

    let has_one_relations: Vec<(String, String)> = ix
        .accounts
        .iter()
        .flat_map(|a| a.relations.iter().map(move |r| (a.name.clone(), r.clone())))
        .collect();

    let args: Vec<(String, String)> = ix
        .args
        .iter()
        .map(|a| (a.name.clone(), type_label(&a.ty)))
        .collect();

    let has_token_program = ix.accounts.iter().any(|a| a.name.contains("token_program"));

    // Close semantics: non-init with a writable PDA and either has_one
    // relations or no args (terminal ops typically take no args).
    let has_writable_pda = ix.accounts.iter().any(|a| a.writable && a.pda.is_some());
    let has_relations = ix.accounts.iter().any(|a| !a.relations.is_empty());
    let is_init = ix.name.contains("init");
    let has_close_semantics = has_writable_pda && !is_init && (has_relations || ix.args.is_empty());

    let has_numeric_args = args
        .iter()
        .any(|(_, ty)| ty.starts_with('u') || ty.starts_with('i'));

    InstructionAnalysis {
        name: ix.name.clone(),
        display_name: snake_to_title(&ix.name),
        docs: ix.docs.join(" ").trim().to_string(),
        signers,
        writable_accounts,
        pda_accounts,
        has_one_relations,
        args,
        has_token_program,
        has_close_semantics,
        has_numeric_args,
    }
}

pub(crate) fn type_label(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub(crate) fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
