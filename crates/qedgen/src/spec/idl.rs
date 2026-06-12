//! Anchor IDL parsing and pattern inference.
//!
//! Reads Anchor IDL JSON, decodes the schema to typed structs, and
//! performs first-pass pattern inference (signers, writable accounts,
//! PDA usage, has_one relations, token-program presence, close
//! semantics, numeric args). Consumed by `idl2spec` (IDL → `.qedspec`
//! scaffolder) and `interface_gen` (IDL → spec interface block).
//!
//! v2.10 cleanup: this file replaces `spec.rs`. The SPEC.md generators
//! that previously lived here (`generate_spec`, `generate_spec_from_qedspec`,
//! `build_spec`) are removed — the `.qedspec` is QEDGen's front-door
//! human-readable artifact (per `feedback_spec_design.md`); generating a
//! parallel SPEC.md was duplicate Markdown that drifted from the spec.
//! `qedgen spec` now exclusively scaffolds IDL → `.qedspec`.

use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub(crate) struct Idl {
    pub metadata: IdlMetadata,
    /// Anchor 0.30+ emits `address` at the IDL root for the deployed program
    /// ID. Older IDLs put it under metadata; we fall back on both.
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
    /// Anchor 0.30+ emits an 8-byte discriminator. Older IDLs omit it; the
    /// interface-from-IDL generator leaves the `discriminant` line as a
    /// TODO in that case.
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
    #[allow(dead_code)]
    pub msg: String,
}

/// First-pass pattern inference over an IDL instruction. Drives
/// `idl2spec` scaffolding heuristics (which clauses to suggest based
/// on signer/writable/PDA/has_one signals).
///
/// Fields tagged `#[allow(dead_code)]` were originally consumed by the
/// SPEC.md narrative generator removed in v2.10. Kept on the struct
/// as a stable analysis surface for future scaffolders (richer
/// idl2spec emit, brownfield reverse-engineering, audit hints).
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
    let idl: Idl = serde_json::from_str(&idl_source)?;
    let analyses: Vec<InstructionAnalysis> =
        idl.instructions.iter().map(analyze_instruction).collect();
    Ok((idl, analyses))
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

    // Close semantics: non-init instruction with a writable PDA state account
    // and either has_one relations or no args (terminal operations typically take no args)
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
