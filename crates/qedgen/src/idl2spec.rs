// IDL → .qedspec generator
//
// Generates a valid .qedspec scaffold from an Anchor IDL JSON file.
// Structural elements (state, accounts, handlers, contexts, PDAs, errors) are
// auto-derived. Semantic elements (guards, effects, properties) are stubbed with
// TODO comments for agent or human completion.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use crate::spec::{self, Idl, IdlInstruction, InstructionAnalysis};

// ── Type mapping ──────────────────────────────────────────────────────────

fn map_type(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => match s.as_str() {
            "u8" => "U8".into(),
            "u16" => "U16".into(),
            "u32" => "U32".into(),
            "u64" => "U64".into(),
            "u128" => "U128".into(),
            "i8" => "I8".into(),
            "i16" => "I16".into(),
            "i32" => "I32".into(),
            "i64" => "I64".into(),
            "i128" => "I128".into(),
            "bool" => "Bool".into(),
            "publicKey" | "pubkey" => "Pubkey".into(),
            "string" => "String".into(),
            other => {
                // PascalCase passthrough for unknown types
                let mut result = String::new();
                let mut upper_next = true;
                for ch in other.chars() {
                    if ch == '_' {
                        upper_next = true;
                    } else if upper_next {
                        result.push(ch.to_ascii_uppercase());
                        upper_next = false;
                    } else {
                        result.push(ch);
                    }
                }
                result
            }
        },
        serde_json::Value::Object(obj) => {
            if let Some(inner) = obj.get("defined") {
                if let Some(name) = inner.as_str() {
                    return name.to_string();
                }
            }
            // Complex types (option, vec, array, etc.) — fallback
            "U64".into()
        }
        _ => "U64".into(),
    }
}

// ── Lifecycle inference ───────────────────────────────────────────────────

fn infer_lifecycle(analyses: &[InstructionAnalysis]) -> Vec<String> {
    let has_init = analyses
        .iter()
        .any(|a| a.name.contains("init") || a.name.contains("create"));
    let has_close = analyses.iter().any(|a| a.has_close_semantics);

    match (has_init, has_close) {
        (true, true) => vec!["Uninitialized".into(), "Active".into(), "Closed".into()],
        (true, false) => vec!["Uninitialized".into(), "Active".into()],
        (false, true) => vec!["Active".into(), "Closed".into()],
        (false, false) => vec!["Active".into()],
    }
}

// ── When/then inference for a single instruction ──────────────────────────

fn infer_when(ix_name: &str, _analysis: &InstructionAnalysis) -> Option<&'static str> {
    if ix_name.contains("init") || ix_name.contains("create") {
        Some("Uninitialized")
    } else {
        Some("Active")
    }
}

fn infer_then(ix_name: &str, analysis: &InstructionAnalysis) -> Option<&'static str> {
    if ix_name.contains("init") || ix_name.contains("create") {
        Some("Active")
    } else if analysis.has_close_semantics {
        Some("Closed")
    } else {
        None // self-transition, omit `then`
    }
}

// ── PDA seed rendering ───────────────────────────────────────────────────

fn render_pda_seeds(pda: &spec::IdlPda) -> Vec<String> {
    pda.seeds
        .iter()
        .map(|seed| {
            if let Some(path) = &seed.path {
                // Account/arg path reference → use as ident
                path.split('.').next_back().unwrap_or(path).to_string()
            } else if let Some(serde_json::Value::Array(bytes)) = &seed.value {
                // Const byte array → try to decode as UTF-8 string
                let values: Vec<u8> = bytes
                    .iter()
                    .filter_map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect();
                match String::from_utf8(values) {
                    Ok(s) if !s.is_empty() => format!("\"{}\"", s),
                    _ => "\"const\"".into(),
                }
            } else {
                "\"const\"".into()
            }
        })
        .collect()
}

// ── Context attribute inference ──────────────────────────────────────────

fn render_account_entry(
    acct: &spec::IdlAccount,
    _is_init_ix: bool,
    _first_signer: Option<&str>,
    type_names: &HashSet<String>,
    pda_names: &std::collections::HashMap<String, String>,
) -> String {
    let mut attrs = Vec::new();

    // Type inference — emit v2 grammar attributes
    if acct.signer && acct.pda.is_none() {
        attrs.push("signer".to_string());
    } else if acct.name.contains("token_program")
        || acct.name.contains("system_program")
        || acct.name.contains("associated_token")
    {
        attrs.push("program".to_string());
    } else if acct.name.contains("rent") {
        attrs.push("readonly".to_string());
    } else if (acct.name.contains("token") && !acct.name.contains("program"))
        || acct.name.ends_with("_ta")
    {
        attrs.push("token".to_string());
    } else {
        // Try to infer type from relations or type name matching
        let inner = acct
            .relations
            .first()
            .and_then(|r| {
                if type_names.contains(r) {
                    Some(r.clone())
                } else {
                    None
                }
            })
            .or_else(|| {
                let name_lower = acct.name.to_lowercase();
                type_names
                    .iter()
                    .find(|t| name_lower.contains(&t.to_lowercase()))
                    .cloned()
            });

        if let Some(type_name) = inner {
            attrs.push(format!("type {}", type_name));
        }
    }

    // Modifier flags
    if acct.writable {
        attrs.push("writable".to_string());
    }

    // PDA seeds — v2 uses `pda [seed1, seed2]` inline
    if let Some(pda_name) = pda_names.get(&acct.name) {
        attrs.push(format!("pda [{}]", pda_name));
    }

    // Authority from relations (first non-type relation)
    if let Some(rel) = acct.relations.first() {
        if !type_names.contains(rel) || acct.relations.len() > 1 {
            let auth_rel = acct
                .relations
                .iter()
                .find(|r| !type_names.contains(r.as_str()))
                .unwrap_or(rel);
            attrs.push(format!("authority {}", auth_rel));
        }
    }

    // Ensure at least one attribute — grammar requires `ident : acct_attr+`
    if attrs.is_empty() {
        attrs.push("readonly".to_string());
    }

    format!("    {} : {}", acct.name, attrs.join(", "))
}

// ── Main renderer ────────────────────────────────────────────────────────

pub(crate) fn render(idl: &Idl, analyses: &[InstructionAnalysis]) -> String {
    let mut s = String::new();
    let program_name = spec::snake_to_title(&idl.metadata.name).replace(' ', "");
    let lifecycle = infer_lifecycle(analyses);
    let multi_account = idl.types.iter().filter(|t| t.ty.kind == "struct").count() > 1;

    // Collect type names for context inference
    let type_names: HashSet<String> = idl
        .types
        .iter()
        .filter(|t| t.ty.kind == "struct")
        .map(|t| t.name.clone())
        .collect();

    // Collect PDA info: account_name → pda_name
    let mut pda_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut seen_pdas: HashSet<String> = HashSet::new();
    for ix in &idl.instructions {
        for acct in &ix.accounts {
            if acct.pda.is_some() && seen_pdas.insert(acct.name.clone()) {
                pda_names.insert(acct.name.clone(), acct.name.clone());
            }
        }
    }

    // ── Header ───────────────────────────────────────────────────────────
    writeln!(
        s,
        "// Generated from Anchor IDL — review and complete TODO items"
    )
    .unwrap();
    writeln!(s, "//").unwrap();
    writeln!(
        s,
        "// Auto-derived: state fields, handlers, contexts, PDAs, errors"
    )
    .unwrap();
    writeln!(
        s,
        "// TODO: guards, effects, lifecycle transitions, properties, invariants"
    )
    .unwrap();
    writeln!(s).unwrap();

    // ── spec header ──────────────────────────────────────────────────────
    writeln!(s, "spec {}", program_name).unwrap();
    writeln!(s).unwrap();
    writeln!(s, "target quasar").unwrap();
    writeln!(s, "// TODO: Replace with deployed program ID").unwrap();
    writeln!(s, "program_id \"11111111111111111111111111111111\"").unwrap();
    writeln!(s).unwrap();

    // ── State / Account blocks ───────────────────────────────────────────
    let struct_types: Vec<_> = idl.types.iter().filter(|t| t.ty.kind == "struct").collect();

    if multi_account {
        for ty in &struct_types {
            writeln!(s, "type {}", ty.name).unwrap();
            // Emit lifecycle variants as ADT constructors.
            // The "Active" variant carries the account fields.
            for state in &lifecycle {
                if state == "Active" && !ty.ty.fields.is_empty() {
                    writeln!(s, "  | {} of {{", state).unwrap();
                    let max_name = ty.ty.fields.iter().map(|f| f.name.len()).max().unwrap_or(0);
                    let field_strs: Vec<String> = ty
                        .ty
                        .fields
                        .iter()
                        .map(|f| {
                            format!(
                                "      {:<width$} : {}",
                                f.name,
                                map_type(&f.ty),
                                width = max_name
                            )
                        })
                        .collect();
                    writeln!(s, "{}", field_strs.join(",\n")).unwrap();
                    writeln!(s, "    }}").unwrap();
                } else {
                    writeln!(s, "  | {}", state).unwrap();
                }
            }
            writeln!(s).unwrap();
        }
    } else if let Some(ty) = struct_types.first() {
        // Emit canonical `type State | Active of { ... } | <lifecycle> ...` form.
        // First variant carries the struct fields; the rest are lifecycle-only.
        writeln!(s, "type State").unwrap();
        let mut variants = lifecycle.clone();
        if variants.is_empty() {
            variants.push("Active".to_string());
        }
        let first = variants.remove(0);
        writeln!(s, "  | {} of {{", first).unwrap();
        let max_name = ty.ty.fields.iter().map(|f| f.name.len()).max().unwrap_or(0);
        for field in &ty.ty.fields {
            writeln!(
                s,
                "      {:<width$} : {},",
                field.name,
                map_type(&field.ty),
                width = max_name
            )
            .unwrap();
        }
        writeln!(s, "    }}").unwrap();
        for v in &variants {
            writeln!(s, "  | {}", v).unwrap();
        }
        writeln!(s).unwrap();
    }

    // ── PDA declarations ─────────────────────────────────────────────────
    seen_pdas.clear();
    for ix in &idl.instructions {
        for acct in &ix.accounts {
            if let Some(pda) = &acct.pda {
                if seen_pdas.insert(acct.name.clone()) {
                    let seeds = render_pda_seeds(pda);
                    writeln!(s, "pda {} [{}]", acct.name, seeds.join(", ")).unwrap();
                }
            }
        }
    }
    if !seen_pdas.is_empty() {
        writeln!(s).unwrap();
    }

    // ── Errors ───────────────────────────────────────────────────────────
    // Emit canonical `type Error | Name | ...` (no legacy `errors [...]` sugar).
    if !idl.errors.is_empty() {
        writeln!(s, "type Error").unwrap();
        for e in &idl.errors {
            writeln!(s, "  | {}", e.name).unwrap();
        }
        writeln!(s).unwrap();
    }

    // ── Handlers ────────────────────────────────────────────────────────
    // Emit canonical `handler name (arg : T) ... : Type.From -> Type.To { ... }` form.
    for (ix, analysis) in idl.instructions.iter().zip(analyses.iter()) {
        if !analysis.docs.is_empty() {
            writeln!(s, "/// {}", analysis.docs).unwrap();
        }

        // Build ML-curried param list.
        let mut params = String::new();
        for arg in &ix.args {
            params.push_str(&format!(" ({} : {})", arg.name, map_type(&arg.ty)));
        }

        // Transition signature from inferred when/then lifecycle states.
        let on_type = if multi_account {
            infer_target_account(ix, &type_names).unwrap_or_else(|| "State".to_string())
        } else {
            "State".to_string()
        };
        let when_state = infer_when(&ix.name, analysis).unwrap_or("Active");
        let then_state = infer_then(&ix.name, analysis).unwrap_or("Active");
        let transition = format!(" : {}.{} -> {}.{}", on_type, when_state, on_type, then_state);

        writeln!(s, "handler {}{}{} {{", ix.name, params, transition).unwrap();

        // auth
        if let Some(signer) = analysis.signers.first() {
            writeln!(s, "  auth {}", signer).unwrap();
        }

        // guard stub
        writeln!(s, "  // TODO: Add guard clause").unwrap();

        // effect stub
        writeln!(s, "  // TODO: Add effect block").unwrap();

        // transfers hint (if token program present)
        if analysis.has_token_program {
            let writable_token: Vec<&spec::IdlAccount> = ix
                .accounts
                .iter()
                .filter(|a| a.writable && a.name.contains("token") && !a.name.contains("program"))
                .collect();
            if writable_token.len() >= 2 {
                writeln!(
                    s,
                    "  // TODO: Add transfers block for token transfer between {} and {}",
                    writable_token[0].name, writable_token[1].name
                )
                .unwrap();
            }
        }

        // accounts
        let is_init_ix = ix.name.contains("init") || ix.name.contains("create");
        let first_signer = analysis.signers.first().map(|s| s.as_str());
        writeln!(s, "  accounts {{").unwrap();
        for acct in &ix.accounts {
            writeln!(
                s,
                "{}",
                render_account_entry(acct, is_init_ix, first_signer, &type_names, &pda_names)
            )
            .unwrap();
        }
        writeln!(s, "  }}").unwrap();

        writeln!(s, "}}").unwrap();
        writeln!(s).unwrap();
    }

    // ── Properties / invariants stub ─────────────────────────────────────
    writeln!(s, "// TODO: Add properties").unwrap();
    writeln!(
        s,
        "// Example: property conservation {{ expr state.total_in >= state.total_out  preserved_by all }}"
    )
    .unwrap();
    writeln!(s).unwrap();
    writeln!(s, "// TODO: Add invariants").unwrap();
    writeln!(
        s,
        "// Example: invariant conservation \"total tokens preserved\""
    )
    .unwrap();

    s
}

// ── Target account inference (multi-account) ─────────────────────────────

fn infer_target_account(ix: &IdlInstruction, type_names: &HashSet<String>) -> Option<String> {
    // Find the first writable PDA account whose name matches a type
    for acct in &ix.accounts {
        if acct.writable && acct.pda.is_some() {
            let name_lower = acct.name.to_lowercase();
            for type_name in type_names {
                if name_lower.contains(&type_name.to_lowercase()) {
                    return Some(type_name.clone());
                }
            }
        }
    }
    // Fallback: first writable account matching a type name
    for acct in &ix.accounts {
        if acct.writable {
            let name_lower = acct.name.to_lowercase();
            for type_name in type_names {
                if name_lower.contains(&type_name.to_lowercase()) {
                    return Some(type_name.clone());
                }
            }
        }
    }
    None
}

// ── Public API ────────────────────────────────────────────────────────────

pub fn generate_qedspec(idl_path: &Path, output_path: &Path) -> Result<()> {
    let (idl, analyses) = spec::parse_idl(idl_path)?;
    let content = render(&idl, &analyses);

    // Round-trip validation: ensure generated output parses cleanly
    crate::chumsky_adapter::parse_str(&content).context(
        "Generated .qedspec failed to parse — this is a bug in idl2spec. \
         Please report at https://github.com/qedgen/solana-skills/issues",
    )?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &content)?;
    eprintln!("Wrote {}", output_path.display());

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{analyze_instruction, Idl};

    const ESCROW_IDL: &str = r#"{
        "metadata": { "name": "escrow" },
        "instructions": [
            {
                "name": "initialize",
                "docs": ["Initialize a new escrow"],
                "accounts": [
                    { "name": "initializer", "signer": true, "writable": true },
                    { "name": "escrow", "writable": true, "pda": { "seeds": [{"kind":"const","value":[101,115,99,114,111,119]},{"kind":"account","path":"initializer"}] } },
                    { "name": "mint" },
                    { "name": "initializer_ta", "writable": true },
                    { "name": "escrow_ta", "writable": true },
                    { "name": "token_program" },
                    { "name": "system_program" }
                ],
                "args": [
                    { "name": "deposit_amount", "type": "u64" },
                    { "name": "receive_amount", "type": "u64" }
                ]
            },
            {
                "name": "exchange",
                "docs": ["Complete the trade"],
                "accounts": [
                    { "name": "taker", "signer": true, "writable": true },
                    { "name": "escrow", "writable": true, "pda": { "seeds": [{"kind":"const","value":[101,115,99,114,111,119]},{"kind":"account","path":"initializer"}] }, "relations": ["initializer"] },
                    { "name": "initializer_ta", "writable": true },
                    { "name": "taker_ta", "writable": true },
                    { "name": "escrow_ta", "writable": true },
                    { "name": "token_program" }
                ],
                "args": []
            },
            {
                "name": "cancel",
                "docs": ["Cancel and reclaim deposit"],
                "accounts": [
                    { "name": "initializer", "signer": true, "writable": true },
                    { "name": "escrow", "writable": true, "pda": { "seeds": [{"kind":"const","value":[101,115,99,114,111,119]},{"kind":"account","path":"initializer"}] }, "relations": ["initializer"] },
                    { "name": "escrow_ta", "writable": true },
                    { "name": "initializer_ta", "writable": true },
                    { "name": "token_program" }
                ],
                "args": []
            }
        ],
        "types": [
            {
                "name": "Escrow",
                "type": {
                    "kind": "struct",
                    "fields": [
                        { "name": "initializer", "type": "publicKey" },
                        { "name": "taker", "type": "publicKey" },
                        { "name": "initializer_amount", "type": "u64" },
                        { "name": "taker_amount", "type": "u64" },
                        { "name": "escrow_token_account", "type": "publicKey" }
                    ]
                }
            }
        ],
        "errors": [
            { "name": "InvalidAmount", "msg": "Amount must be positive" },
            { "name": "Unauthorized", "msg": "Unauthorized" }
        ]
    }"#;

    const LENDING_IDL: &str = r#"{
        "metadata": { "name": "lending" },
        "instructions": [
            {
                "name": "initialize_pool",
                "docs": ["Create a new lending pool"],
                "accounts": [
                    { "name": "authority", "signer": true, "writable": true },
                    { "name": "pool", "writable": true, "pda": { "seeds": [{"kind":"const","value":[112,111,111,108]},{"kind":"account","path":"authority"}] } },
                    { "name": "system_program" }
                ],
                "args": [
                    { "name": "interest_rate", "type": "u64" }
                ]
            },
            {
                "name": "deposit",
                "docs": ["Deposit into pool"],
                "accounts": [
                    { "name": "depositor", "signer": true, "writable": true },
                    { "name": "pool", "writable": true, "pda": { "seeds": [{"kind":"const","value":[112,111,111,108]},{"kind":"account","path":"authority"}] } },
                    { "name": "token_program" }
                ],
                "args": [
                    { "name": "amount", "type": "u64" }
                ]
            }
        ],
        "types": [
            {
                "name": "Pool",
                "type": {
                    "kind": "struct",
                    "fields": [
                        { "name": "authority", "type": "publicKey" },
                        { "name": "total_deposits", "type": "u64" },
                        { "name": "interest_rate", "type": "u64" }
                    ]
                }
            },
            {
                "name": "Loan",
                "type": {
                    "kind": "struct",
                    "fields": [
                        { "name": "borrower", "type": "publicKey" },
                        { "name": "amount", "type": "u64" },
                        { "name": "collateral", "type": "u64" }
                    ]
                }
            }
        ],
        "errors": []
    }"#;

    fn parse_test_idl(json: &str) -> (Idl, Vec<InstructionAnalysis>) {
        let idl: Idl = serde_json::from_str(json).unwrap();
        let analyses = idl.instructions.iter().map(analyze_instruction).collect();
        (idl, analyses)
    }

    // ── Type mapping ─────────────────────────────────────────────────────

    #[test]
    fn map_type_primitives() {
        assert_eq!(map_type(&serde_json::json!("u64")), "U64");
        assert_eq!(map_type(&serde_json::json!("u8")), "U8");
        assert_eq!(map_type(&serde_json::json!("u128")), "U128");
        assert_eq!(map_type(&serde_json::json!("i128")), "I128");
        assert_eq!(map_type(&serde_json::json!("bool")), "Bool");
        assert_eq!(map_type(&serde_json::json!("publicKey")), "Pubkey");
        assert_eq!(map_type(&serde_json::json!("pubkey")), "Pubkey");
        assert_eq!(map_type(&serde_json::json!("string")), "String");
    }

    #[test]
    fn map_type_defined() {
        assert_eq!(
            map_type(&serde_json::json!({"defined": "Escrow"})),
            "Escrow"
        );
    }

    #[test]
    fn map_type_complex_fallback() {
        assert_eq!(map_type(&serde_json::json!({"vec": "u8"})), "U64");
    }

    // ── Lifecycle inference ──────────────────────────────────────────────

    #[test]
    fn lifecycle_init_and_close() {
        let (_, analyses) = parse_test_idl(ESCROW_IDL);
        let lc = infer_lifecycle(&analyses);
        assert_eq!(lc, vec!["Uninitialized", "Active", "Closed"]);
    }

    #[test]
    fn lifecycle_init_only() {
        let (_, analyses) = parse_test_idl(LENDING_IDL);
        let lc = infer_lifecycle(&analyses);
        assert_eq!(lc, vec!["Uninitialized", "Active"]);
    }

    // ── Round-trip: escrow ────────────────────────────────────────────────

    #[test]
    fn round_trip_escrow() {
        let (idl, analyses) = parse_test_idl(ESCROW_IDL);
        let content = render(&idl, &analyses);

        let spec = crate::chumsky_adapter::parse_str(&content).unwrap_or_else(|e| {
            panic!(
                "Generated .qedspec failed to parse:\n{}\n\nContent:\n{}",
                e, content
            )
        });

        assert_eq!(spec.program_name, "Escrow");
        assert_eq!(spec.target.as_deref(), Some("quasar"));
        assert_eq!(spec.handlers.len(), 3);
        assert_eq!(spec.handlers[0].name, "initialize");
        assert_eq!(spec.handlers[1].name, "exchange");
        assert_eq!(spec.handlers[2].name, "cancel");
        assert!(spec.handlers[0].who.as_deref() == Some("initializer"));
        assert!(!spec.pdas.is_empty());
        assert_eq!(spec.error_codes.len(), 2);
        assert!(!spec.state_fields.is_empty());
        assert!(!spec.lifecycle_states.is_empty());
    }

    // ── Round-trip: multi-account (lending) ──────────────────────────────

    #[test]
    fn round_trip_multi_account() {
        let (idl, analyses) = parse_test_idl(LENDING_IDL);
        let content = render(&idl, &analyses);

        let spec = crate::chumsky_adapter::parse_str(&content).unwrap_or_else(|e| {
            panic!(
                "Generated .qedspec failed to parse:\n{}\n\nContent:\n{}",
                e, content
            )
        });

        assert_eq!(spec.program_name, "Lending");
        assert_eq!(spec.account_types.len(), 2);
        assert!(spec.account_types.iter().any(|a| a.name == "Pool"));
        assert!(spec.account_types.iter().any(|a| a.name == "Loan"));
        assert_eq!(spec.handlers.len(), 2);
    }

    // ── Accounts generation ─────────────────────────────────────────────

    #[test]
    fn accounts_has_signer_and_program() {
        let (idl, analyses) = parse_test_idl(ESCROW_IDL);
        let content = render(&idl, &analyses);

        // Verify key accounts attributes appear
        assert!(content.contains("signer"));
        assert!(content.contains("program"));
        assert!(content.contains("writable"));
        assert!(content.contains("pda [escrow]"));
    }

    // ── PDA extraction ───────────────────────────────────────────────────

    #[test]
    fn pda_seeds_extracted() {
        let (idl, analyses) = parse_test_idl(ESCROW_IDL);
        let content = render(&idl, &analyses);

        assert!(content.contains("pda escrow"));
        assert!(content.contains("\"escrow\""));
        assert!(content.contains("initializer"));
    }
}
