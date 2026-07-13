// IDL → .qedspec scaffold generator. Structural elements (state, accounts,
// handlers, contexts, PDAs, errors) are auto-derived; semantic elements
// (guards, effects, properties) are stubbed with TODOs for agent completion.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use crate::idl::{self, Idl, IdlInstruction, InstructionAnalysis};

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
                // Unknown types: snake_case → PascalCase passthrough
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
            // Container types → the DSL's `Option T` / `Vec T` field forms
            // (G9/G10). Anchor IDLs use `{"option"|"vec": <inner>}` and
            // `{"array": [<inner>, N]}`; the Codama normalizer emits the same
            // shapes. Recurse into the element so `Option<Pubkey>` renders
            // `Option Pubkey` (not the old lossy `U64` — which silently
            // mistyped e.g. a mint's `Option<Pubkey>` authority field).
            if let Some(inner) = obj.get("option") {
                return format!("Option {}", map_type(inner));
            }
            if let Some(inner) = obj.get("vec") {
                return format!("Vec {}", map_type(inner));
            }
            if let Some(serde_json::Value::Array(parts)) = obj.get("array") {
                // Fixed-length arrays have no DSL type; degrade to `Vec T`
                // (element type preserved, length dropped — better than U64).
                if let Some(inner) = parts.first() {
                    return format!("Vec {}", map_type(inner));
                }
            }
            // Genuinely unknown object shape — last-resort scalar.
            "U64".into()
        }
        _ => "U64".into(),
    }
}

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

fn render_pda_seeds(pda: &idl::IdlPda) -> Vec<String> {
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

fn render_account_entry(
    acct: &idl::IdlAccount,
    _is_init_ix: bool,
    _first_signer: Option<&str>,
    type_names: &HashSet<String>,
    pda_names: &std::collections::HashMap<String, String>,
) -> String {
    let mut attrs = Vec::new();

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
        // Infer type from relations or type-name matching
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

    if acct.writable {
        attrs.push("writable".to_string());
    }

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

    // Grammar requires `ident : acct_attr+` — ensure at least one attribute
    if attrs.is_empty() {
        attrs.push("readonly".to_string());
    }

    format!("    {} : {}", acct.name, attrs.join(", "))
}

pub(crate) fn render(idl: &Idl, analyses: &[InstructionAnalysis]) -> String {
    let mut s = String::new();
    let program_name = idl::snake_to_title(&idl.metadata.name).replace(' ', "");
    let lifecycle = infer_lifecycle(analyses);
    let multi_account = idl.types.iter().filter(|t| t.ty.kind == "struct").count() > 1;

    // Collect type names for context inference
    let type_names: HashSet<String> = idl
        .types
        .iter()
        .filter(|t| t.ty.kind == "struct")
        .map(|t| t.name.clone())
        .collect();

    // Collect PDA info: account_name → pda_name. Seedless PDA markers
    // (Codama `pdaValueNode` carries no seed data, #197) are excluded —
    // an empty `pda <name> []` declaration doesn't parse, so those degrade
    // to a TODO comment at the declaration site instead.
    let mut pda_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut seen_pdas: HashSet<String> = HashSet::new();
    for ix in &idl.instructions {
        for acct in &ix.accounts {
            if acct.pda.as_ref().is_some_and(|p| !p.seeds.is_empty())
                && seen_pdas.insert(acct.name.clone())
            {
                pda_names.insert(acct.name.clone(), acct.name.clone());
            }
        }
    }

    // ── Header ───────────────────────────────────────────────────────────
    writeln!(
        s,
        "// Generated from IDL (Anchor or Codama IR) — review and complete TODO items"
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
    // No `target quasar` — Anchor/Quasar is the default; sBPF is opted into
    // with `pragma sbpf { ... }`.
    writeln!(s, "spec {}", program_name).unwrap();
    writeln!(s).unwrap();
    // The IDL's own address is authoritative when present (Anchor 0.30+
    // root `address`, Codama `program.publicKey`); only fall back to the
    // TODO placeholder without one.
    match idl.address.as_deref() {
        Some(addr) => writeln!(s, "program_id \"{}\"", addr).unwrap(),
        None => {
            writeln!(s, "// TODO: Replace with deployed program ID").unwrap();
            writeln!(s, "program_id \"11111111111111111111111111111111\"").unwrap();
        }
    }
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

    // ── Enum defined types (#202) ────────────────────────────────────────
    // `enumTypeNode` (Codama) / `{"kind":"enum"}` (Anchor) defined types render
    // as DSL sum types carrying their real variants (e.g. authorityType,
    // accountState) — not the generic lifecycle state a fieldless struct falls
    // into. Data-carrying variants degrade to name-only (see IdlTypeBody).
    for ty in idl.types.iter().filter(|t| t.ty.kind == "enum") {
        writeln!(s, "type {}", ty.name).unwrap();
        for v in &ty.ty.variants {
            writeln!(s, "  | {}", v.name).unwrap();
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
                    if seeds.is_empty() {
                        // Codama `pdaValueNode` marks the account PDA but
                        // carries no seed data (#197) — an empty seed list
                        // doesn't parse, so leave the derivation to the user.
                        writeln!(
                            s,
                            "// TODO: `{}` is a PDA but the IDL carries no seeds — declare\n\
                             // them: pda {} [\"<literal>\", <account_or_arg>, ...]",
                            acct.name, acct.name
                        )
                        .unwrap();
                    } else {
                        writeln!(s, "pda {} [{}]", acct.name, seeds.join(", ")).unwrap();
                    }
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
        let transition = format!(
            " : {}.{} -> {}.{}",
            on_type, when_state, on_type, then_state
        );

        writeln!(s, "handler {}{}{} {{", ix.name, params, transition).unwrap();

        if let Some(signer) = analysis.signers.first() {
            writeln!(s, "  auth {}", signer).unwrap();
        }

        writeln!(s, "  // TODO: Add guard clause").unwrap();

        writeln!(s, "  // TODO: Add effect block").unwrap();

        if analysis.has_token_program {
            let writable_token: Vec<&idl::IdlAccount> = ix
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

fn infer_target_account(ix: &IdlInstruction, type_names: &HashSet<String>) -> Option<String> {
    // First writable PDA account whose name matches a type
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

pub fn generate_qedspec(idl_path: &Path, output_path: &Path) -> Result<()> {
    let (idl, analyses) = idl::parse_idl(idl_path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::{analyze_instruction, Idl};

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
    fn map_type_container_types() {
        // Container shapes recurse into the element (#197 real-world fidelity
        // from p-token: an `Option<Pubkey>` authority field was mistyped U64).
        assert_eq!(map_type(&serde_json::json!({"vec": "u8"})), "Vec U8");
        assert_eq!(
            map_type(&serde_json::json!({"option": "publicKey"})),
            "Option Pubkey"
        );
        assert_eq!(
            map_type(&serde_json::json!({"array": ["u8", 32]})),
            "Vec U8"
        );
        assert_eq!(
            map_type(&serde_json::json!({"option": {"defined": "Hook"}})),
            "Option Hook"
        );
        // A genuinely unknown object shape still degrades to a scalar.
        assert_eq!(map_type(&serde_json::json!({"mystery": 1})), "U64");
    }

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
        assert!(
            !spec.is_assembly_target(),
            "IDL-generated specs default to Quasar (no `pragma sbpf`)"
        );
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

    #[test]
    fn accounts_has_signer_and_program() {
        let (idl, analyses) = parse_test_idl(ESCROW_IDL);
        let content = render(&idl, &analyses);

        assert!(content.contains("signer"));
        assert!(content.contains("program"));
        assert!(content.contains("writable"));
        assert!(content.contains("pda [escrow]"));
    }

    #[test]
    fn pda_seeds_extracted() {
        let (idl, analyses) = parse_test_idl(ESCROW_IDL);
        let content = render(&idl, &analyses);

        assert!(content.contains("pda escrow"));
        assert!(content.contains("\"escrow\""));
        assert!(content.contains("initializer"));
    }

    /// #197: a Codama IR tree (the IDL format Pinocchio programs ship)
    /// normalizes into the same Anchor-shaped `Idl` — camelCase names
    /// snake_cased, type nodes lowered, omitted single-byte discriminator
    /// lifted, accountNode data structs exposed as `types`.
    const CODAMA_IDL: &str = r#"{
      "kind": "rootNode",
      "standard": "codama",
      "version": "1.0.0",
      "program": {
        "kind": "programNode",
        "name": "ctxGate",
        "publicKey": "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
        "version": "0.1.0",
        "instructions": [
          {
            "kind": "instructionNode",
            "name": "setThreshold",
            "docs": ["Update the gate threshold"],
            "arguments": [
              {
                "kind": "instructionArgumentNode",
                "name": "discriminator",
                "type": { "kind": "numberTypeNode", "format": "u8" },
                "defaultValue": { "kind": "numberValueNode", "number": 1 },
                "defaultValueStrategy": "omitted"
              },
              {
                "kind": "instructionArgumentNode",
                "name": "newThreshold",
                "type": { "kind": "numberTypeNode", "format": "u64" }
              }
            ],
            "accounts": [
              {
                "kind": "instructionAccountNode",
                "name": "settings",
                "isWritable": true,
                "isSigner": false,
                "defaultValue": { "kind": "pdaValueNode", "pda": { "kind": "pdaLinkNode", "name": "settings" } }
              },
              { "kind": "instructionAccountNode", "name": "admin", "isSigner": true, "isWritable": false }
            ]
          }
        ],
        "accounts": [
          {
            "kind": "accountNode",
            "name": "settings",
            "data": {
              "kind": "structTypeNode",
              "fields": [
                { "kind": "structFieldTypeNode", "name": "admin", "type": { "kind": "publicKeyTypeNode" } },
                { "kind": "structFieldTypeNode", "name": "threshold", "type": { "kind": "numberTypeNode", "format": "u64" } }
              ]
            }
          }
        ],
        "definedTypes": [
          {
            "kind": "definedTypeNode",
            "name": "accountState",
            "type": {
              "kind": "enumTypeNode",
              "variants": [
                { "kind": "enumEmptyVariantTypeNode", "name": "uninitialized" },
                { "kind": "enumEmptyVariantTypeNode", "name": "initialized" },
                { "kind": "enumEmptyVariantTypeNode", "name": "frozen" }
              ]
            }
          }
        ],
        "errors": [
          { "kind": "errorNode", "name": "notActive", "code": 6000, "message": "settings not active" }
        ],
        "pdas": [
          {
            "kind": "pdaNode",
            "name": "settings",
            "seeds": [
              {
                "kind": "constantPdaSeedNode",
                "type": { "kind": "stringTypeNode", "encoding": "utf8" },
                "value": { "kind": "stringValueNode", "string": "settings" }
              },
              {
                "kind": "variablePdaSeedNode",
                "name": "admin",
                "type": { "kind": "publicKeyTypeNode" }
              }
            ]
          }
        ]
      }
    }"#;

    #[test]
    fn codama_ir_normalizes_to_anchor_shaped_idl() {
        let tmp = std::env::temp_dir().join(format!("codama_{}.json", std::process::id()));
        std::fs::write(&tmp, CODAMA_IDL).unwrap();
        let (idl, analyses) = idl::parse_idl(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(idl.metadata.name, "ctxGate");
        assert_eq!(
            idl.address.as_deref(),
            Some("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS")
        );
        let ix = &idl.instructions[0];
        assert_eq!(ix.name, "set_threshold", "camelCase name snake_cased");
        assert_eq!(
            ix.discriminator,
            vec![1],
            "omitted numberValueNode arg lifted"
        );
        assert_eq!(
            ix.args.len(),
            1,
            "omitted discriminator arg dropped from args"
        );
        assert_eq!(ix.args[0].name, "new_threshold");
        assert_eq!(ix.args[0].ty, serde_json::json!("u64"));
        let settings = &ix.accounts[0];
        assert!(settings.writable && !settings.signer);
        // #200: the pdaLinkNode resolved through program.pdas[] to real seeds.
        let pda = settings.pda.as_ref().unwrap();
        assert_eq!(pda.seeds.len(), 2);
        assert_eq!(
            pda.seeds[0].value,
            Some(serde_json::json!([115, 101, 116, 116, 105, 110, 103, 115])),
            "const string seed lowered to utf8 bytes"
        );
        assert_eq!(pda.seeds[1].path.as_deref(), Some("admin"));
        let admin = &ix.accounts[1];
        assert!(admin.signer && !admin.writable);
        // accountNode data struct exposed as a state-layout candidate.
        let ty = idl.types.iter().find(|t| t.name == "Settings").unwrap();
        assert_eq!(ty.ty.fields.len(), 2);
        assert_eq!(idl.errors[0].name, "NotActive");
        // Signer inference flows through the shared analysis.
        assert_eq!(analyses[0].signers, vec!["admin".to_string()]);
    }

    #[test]
    fn codama_ir_renders_qedspec_scaffold() {
        let tmp = std::env::temp_dir().join(format!("codama_r_{}.json", std::process::id()));
        std::fs::write(&tmp, CODAMA_IDL).unwrap();
        let (idl, analyses) = idl::parse_idl(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let spec = render(&idl, &analyses);
        assert!(spec.contains("set_threshold"), "handler present:\n{spec}");
        assert!(
            spec.contains("threshold : U64") && spec.contains(": Pubkey"),
            "state fields lowered from the accountNode struct:\n{spec}"
        );
        assert!(
            spec.contains("program_id \"Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS\""),
            "Codama program.publicKey becomes the program_id:\n{spec}"
        );
        assert!(spec.contains("NotActive"), "error surfaced:\n{spec}");
        // #200: seeds mined from program.pdas[] — a real declaration, no TODO.
        assert!(
            spec.contains("pda settings [\"settings\", admin]"),
            "resolved PDA declaration:\n{spec}"
        );
        assert!(
            !spec.contains("IDL carries no seeds"),
            "no seedless TODO when seeds resolve:\n{spec}"
        );
    }

    /// #202: an `enumTypeNode` defined type renders as a DSL sum type carrying
    /// its real variants — not the generic `Uninitialized | Active` lifecycle a
    /// fieldless struct falls into (the old bug: `codama_struct_fields` read zero
    /// fields, so the render stamped a synthesized state).
    #[test]
    fn codama_enum_defined_type_renders_real_variants() {
        let tmp = std::env::temp_dir().join(format!("codama_e_{}.json", std::process::id()));
        std::fs::write(&tmp, CODAMA_IDL).unwrap();
        let (idl, analyses) = idl::parse_idl(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);

        // Normalized: the enum defined type carries variants, not zero fields.
        let acct_state = idl.types.iter().find(|t| t.name == "AccountState").unwrap();
        assert_eq!(acct_state.ty.kind, "enum", "enumTypeNode stamped as enum");
        assert!(
            acct_state.ty.fields.is_empty(),
            "no phantom struct fields for an enum type"
        );
        let variants: Vec<_> = acct_state
            .ty
            .variants
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(
            variants,
            vec!["Uninitialized", "Initialized", "Frozen"],
            "camelCase variant names PascalCased and preserved in order"
        );

        // Rendered: a sum type with the real variants. `Initialized` / `Frozen`
        // exist ONLY on the real enum — the generic lifecycle synthesis never
        // emits them — so their presence is the regression signal.
        let spec = render(&idl, &analyses);
        assert!(
            spec.contains("type AccountState"),
            "enum type emitted:\n{spec}"
        );
        assert!(
            spec.contains("| Uninitialized")
                && spec.contains("| Initialized")
                && spec.contains("| Frozen"),
            "real enum variants rendered, not the generic Uninitialized|Active lifecycle:\n{spec}"
        );
    }

    /// #200 negative: a `pdaValueNode` whose link has no matching
    /// `program.pdas[]` definition keeps the seedless marker → the
    /// scaffold degrades to the TODO comment (never unparseable `pda x []`).
    #[test]
    fn codama_dangling_pda_link_degrades_to_todo() {
        let dangling = CODAMA_IDL.replace(
            r#""pda": { "kind": "pdaLinkNode", "name": "settings" }"#,
            r#""pda": { "kind": "pdaLinkNode", "name": "missing" }"#,
        );
        let tmp = std::env::temp_dir().join(format!("codama_d_{}.json", std::process::id()));
        std::fs::write(&tmp, dangling).unwrap();
        let (idl, analyses) = idl::parse_idl(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        let spec = render(&idl, &analyses);
        assert!(
            // Line-anchored: the TODO's own example text mentions
            // `pda settings [`, but only inside a `//` comment.
            spec.contains("IDL carries no seeds") && !spec.contains("\npda settings ["),
            "dangling link degrades to the TODO comment:\n{spec}"
        );
    }
}
