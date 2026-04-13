//! Parser for `.qedspec` files — the standalone spec format.
//!
//! Uses pest (PEG parser) to parse `.qedspec` into `ParsedSpec`,
//! the same IR consumed by codegen, kani, unit_test, check, and lean_gen.

use anyhow::{Context, Result};
use pest::Parser;
use pest_derive::Parser;
use std::path::Path;

use crate::check::{
    FlowKind, ParsedAccountEntry, ParsedAccountType, ParsedContext, ParsedErrorCode, ParsedEvent,
    ParsedGuard, ParsedInstruction, ParsedLayoutField, ParsedOperation, ParsedPda, ParsedProperty,
    ParsedPubkey, ParsedSbpfProperty, ParsedSpec, SbpfPropertyKind,
};

#[derive(Parser)]
#[grammar = "qedspec.pest"]
struct QedspecParser;

/// Strip underscores from an integer literal: "10_000_000" → "10000000"
fn clean_integer(s: &str) -> String {
    s.replace('_', "")
}

/// Parse a `.qedspec` file from disk into a `ParsedSpec`.
pub fn parse_file(path: &Path) -> Result<ParsedSpec> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&content)
}

/// Parse a `.qedspec` string into a `ParsedSpec`.
pub fn parse(content: &str) -> Result<ParsedSpec> {
    let file = QedspecParser::parse(Rule::spec_file, content)
        .map_err(|e| anyhow::anyhow!("Parse error:\n{}", e))?
        .next()
        .unwrap(); // spec_file is the top-level rule, always present

    let mut program_name = String::new();
    let mut program_id = None;
    let mut assembly_path = None;
    let mut constants: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut pubkeys: Vec<ParsedPubkey> = Vec::new();
    let mut state_fields: Vec<(String, String)> = Vec::new();
    let mut global_lifecycle: Vec<String> = Vec::new();
    let mut account_types: Vec<ParsedAccountType> = Vec::new();
    let mut pdas: Vec<ParsedPda> = Vec::new();
    let mut events: Vec<ParsedEvent> = Vec::new();
    let mut error_codes: Vec<String> = Vec::new();
    let mut valued_errors: Vec<ParsedErrorCode> = Vec::new();
    let mut instructions: Vec<ParsedInstruction> = Vec::new();
    let mut operations: Vec<ParsedOperation> = Vec::new();
    let mut properties: Vec<ParsedProperty> = Vec::new();
    let mut invariants: Vec<(String, String)> = Vec::new();
    let mut contexts: Vec<ParsedContext> = Vec::new();
    let mut target: Option<String> = None;

    for pair in file.into_inner() {
        match pair.as_rule() {
            Rule::spec_header => {
                for inner in pair.into_inner() {
                    if inner.as_rule() == Rule::ident {
                        program_name = inner.as_str().to_string();
                    }
                }
            }
            Rule::top_level_item => {
                let inner = pair.into_inner().next().unwrap();
                match inner.as_rule() {
                    Rule::target_decl => {
                        for t in inner.into_inner() {
                            if t.as_rule() == Rule::target_kind {
                                target = Some(t.as_str().to_string());
                            }
                        }
                    }
                    Rule::program_id_decl => {
                        program_id = Some(parse_string_lit(inner));
                    }
                    Rule::assembly_decl => {
                        assembly_path = Some(parse_string_lit(inner));
                    }
                    Rule::const_decl => {
                        let mut parts = inner.into_inner();
                        let cname = parts.next().unwrap().as_str().to_string();
                        let cval = clean_integer(parts.next().unwrap().as_str());
                        constants.insert(cname, cval);
                    }
                    Rule::pubkey_decl => {
                        pubkeys.push(parse_pubkey_decl(inner));
                    }
                    Rule::state_block => {
                        state_fields = parse_field_decls(inner);
                    }
                    Rule::account_block => {
                        account_types.push(parse_account_block(inner));
                    }
                    Rule::lifecycle_decl => {
                        global_lifecycle = parse_ident_list(inner);
                    }
                    Rule::pda_decl => {
                        pdas.push(parse_pda(inner));
                    }
                    Rule::event_decl => {
                        events.push(parse_event(inner));
                    }
                    Rule::errors_decl => {
                        let (codes, valued) = parse_errors_decl(inner);
                        error_codes = codes;
                        valued_errors = valued;
                    }
                    Rule::instruction_block => {
                        instructions.push(parse_instruction_block(inner, &constants));
                    }
                    Rule::operation_block => {
                        let (op, ctx) = parse_operation(inner, &constants);
                        if let Some(c) = ctx {
                            contexts.push(c);
                        }
                        operations.push(op);
                    }
                    Rule::property_block => {
                        properties.push(parse_property(inner, &constants));
                    }
                    Rule::invariant_decl => {
                        invariants.push(parse_invariant(inner));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // If we have named account types but no bare state_fields,
    // populate state_fields from the first account type (backward compat).
    if state_fields.is_empty() && !account_types.is_empty() {
        state_fields = account_types[0].fields.clone();
    }

    // If we have a bare state {} but no account types, create an implicit one
    // named after the program (backward compat).
    if !state_fields.is_empty() && account_types.is_empty() {
        account_types.push(ParsedAccountType {
            name: program_name.clone(),
            fields: state_fields.clone(),
            lifecycle: global_lifecycle.clone(),
            pda_ref: pdas.first().map(|p| p.name.clone()),
        });
    }

    // Link PDAs to account types by matching names
    for acct in &mut account_types {
        if acct.pda_ref.is_none() {
            // Try to find a PDA whose name matches the account name (case-insensitive)
            let lower = acct.name.to_lowercase();
            if let Some(pda) = pdas.iter().find(|p| p.name.to_lowercase() == lower) {
                acct.pda_ref = Some(pda.name.clone());
            }
        }
    }

    // Compute unified lifecycle_states for backward compat
    let mut lifecycle_states = global_lifecycle;
    // Merge in per-account lifecycles
    for acct in &account_types {
        for ls in &acct.lifecycle {
            if !lifecycle_states.contains(ls) {
                lifecycle_states.push(ls.clone());
            }
        }
    }
    // If still empty, derive from operations
    if lifecycle_states.is_empty() {
        for op in &operations {
            if let Some(ref pre) = op.pre_status {
                if !lifecycle_states.contains(pre) {
                    lifecycle_states.push(pre.clone());
                }
            }
            if let Some(ref post) = op.post_status {
                if !lifecycle_states.contains(post) {
                    lifecycle_states.push(post.clone());
                }
            }
        }
    }

    // Expand `preserved_by all` → list of all operation names
    let all_op_names: Vec<String> = operations.iter().map(|o| o.name.clone()).collect();
    for prop in &mut properties {
        if prop.preserved_by.len() == 1 && prop.preserved_by[0] == "all" {
            prop.preserved_by = all_op_names.clone();
        }
    }

    // Compute U64 field metadata
    let u64_field_names: Vec<String> = state_fields
        .iter()
        .filter(|(_, ty)| ty == "U64")
        .map(|(name, _)| name.clone())
        .collect();
    let has_u64_fields = !u64_field_names.is_empty();

    Ok(ParsedSpec {
        operations,
        invariants,
        properties,
        has_u64_fields,
        u64_field_names,
        program_id,
        program_name,
        state_fields,
        lifecycle_states,
        pdas,
        events,
        error_codes,
        contexts,
        account_types,
        target,
        assembly_path,
        pubkeys,
        instructions,
        valued_errors,
        constants: constants.into_iter().collect(),
    })
}

// ============================================================================
// Helper parsers
// ============================================================================

/// Extract string literal content (strip quotes).
fn parse_string_lit(pair: pest::iterators::Pair<Rule>) -> String {
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::string_lit {
            return inner
                .into_inner()
                .next()
                .map(|s| s.as_str().to_string())
                .unwrap_or_default();
        }
    }
    String::new()
}

/// Parse `{ name : Type \n ... }` field declarations.
fn parse_field_decls(pair: pest::iterators::Pair<Rule>) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::field_decl {
            let mut parts = inner.into_inner();
            let name = parts.next().unwrap().as_str().to_string();
            let ty = parts.next().unwrap().as_str().to_string();
            fields.push((name, ty));
        }
    }
    fields
}

/// Parse `[ident1, ident2, ...]` list.
fn parse_ident_list(pair: pest::iterators::Pair<Rule>) -> Vec<String> {
    let mut items = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::ident_list {
            for id in inner.into_inner() {
                if id.as_rule() == Rule::ident {
                    items.push(id.as_str().to_string());
                }
            }
        } else if inner.as_rule() == Rule::ident {
            items.push(inner.as_str().to_string());
        }
    }
    items
}

/// Parse `account Name { fields... lifecycle [...] }` block.
fn parse_account_block(pair: pest::iterators::Pair<Rule>) -> ParsedAccountType {
    let mut name = String::new();
    let mut fields = Vec::new();
    let mut lifecycle = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::account_item => {
                let item = inner.into_inner().next().unwrap();
                match item.as_rule() {
                    Rule::field_decl => {
                        let mut parts = item.into_inner();
                        let fname = parts.next().unwrap().as_str().to_string();
                        let ftype = parts.next().unwrap().as_str().to_string();
                        fields.push((fname, ftype));
                    }
                    Rule::account_lifecycle => {
                        lifecycle = parse_ident_list(item);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    ParsedAccountType {
        name,
        fields,
        lifecycle,
        pda_ref: None, // linked later in the main parse function
    }
}

/// Parse `pda name ["seed1", field_ref]`.
fn parse_pda(pair: pest::iterators::Pair<Rule>) -> ParsedPda {
    let mut name = String::new();
    let mut seeds = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::pda_seed_list => {
                for seed in inner.into_inner() {
                    if seed.as_rule() == Rule::pda_seed {
                        let val = seed.into_inner().next().unwrap();
                        match val.as_rule() {
                            Rule::string_lit => {
                                let s = val
                                    .into_inner()
                                    .next()
                                    .map(|v| v.as_str().to_string())
                                    .unwrap_or_default();
                                seeds.push(format!("\"{}\"", s));
                            }
                            Rule::ident => seeds.push(val.as_str().to_string()),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    ParsedPda { name, seeds }
}

/// Parse `event Name { field : Type ... }`.
fn parse_event(pair: pest::iterators::Pair<Rule>) -> ParsedEvent {
    let mut name = String::new();
    let mut fields = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::field_decl => {
                let mut parts = inner.into_inner();
                let fname = parts.next().unwrap().as_str().to_string();
                let ftype = parts.next().unwrap().as_str().to_string();
                fields.push((fname, ftype));
            }
            _ => {}
        }
    }
    ParsedEvent { name, fields }
}

type Constants = std::collections::BTreeMap<String, String>;

/// Reconstruct a guard expression from the pest AST into two forms:
/// 1. Lean form (with Unicode operators)
/// 2. Rust/plain form (with ASCII operators)
///
/// Named constants are expanded inline: `MAX_TVL` → `10000000`.
fn guard_expr_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    match pair.as_rule() {
        Rule::guard_expr => guard_expr_to_lean(pair.into_inner().next().unwrap(), consts),
        Rule::guard_or => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_lean(p, consts))
                .collect();
            parts.join(" \u{2228} ") // ∨
        }
        Rule::guard_and => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_lean(p, consts))
                .collect();
            parts.join(" \u{2227} ") // ∧
        }
        Rule::guard_atom => guard_expr_to_lean(pair.into_inner().next().unwrap(), consts),
        Rule::guard_comparison => {
            let mut inner = pair.into_inner();
            let lhs = guard_value_to_lean(inner.next().unwrap(), consts);
            let op = inner.next().unwrap().as_str();
            let rhs = guard_value_to_lean(inner.next().unwrap(), consts);
            let lean_op = match op {
                "<=" => "\u{2264}", // ≤
                ">=" => "\u{2265}", // ≥
                "!=" => "\u{2260}", // ≠
                other => other,
            };
            format!("{} {} {}", lhs, lean_op, rhs)
        }
        _ => pair.as_str().to_string(),
    }
}

fn guard_value_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    match pair.as_rule() {
        Rule::guard_value => {
            // guard_value = { guard_term ~ (add_op ~ guard_term)* }
            let mut parts = Vec::new();
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::guard_term => parts.push(guard_term_to_lean(inner, consts)),
                    Rule::add_op => parts.push(format!(" {} ", inner.as_str())),
                    _ => parts.push(inner.as_str().to_string()),
                }
            }
            parts.join("")
        }
        Rule::guard_term => guard_term_to_lean(pair, consts),
        _ => pair.as_str().to_string(),
    }
}

fn guard_term_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    match pair.as_rule() {
        Rule::guard_term => guard_term_to_lean(pair.into_inner().next().unwrap(), consts),
        Rule::field_ref => {
            let field = pair
                .as_str()
                .strip_prefix("state.")
                .unwrap_or(pair.as_str());
            format!("s.{}", field)
        }
        Rule::ident => {
            let name = pair.as_str();
            if let Some(val) = consts.get(name) {
                val.clone()
            } else {
                name.to_string()
            }
        }
        Rule::integer => clean_integer(pair.as_str()),
        _ => pair.as_str().to_string(),
    }
}

/// Guard expression to Rust-compatible form (ASCII operators).
fn guard_expr_to_rust(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    match pair.as_rule() {
        Rule::guard_expr => guard_expr_to_rust(pair.into_inner().next().unwrap(), consts),
        Rule::guard_or => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_rust(p, consts))
                .collect();
            parts.join(" || ")
        }
        Rule::guard_and => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_rust(p, consts))
                .collect();
            parts.join(" && ")
        }
        Rule::guard_atom => guard_expr_to_rust(pair.into_inner().next().unwrap(), consts),
        Rule::guard_comparison => {
            let mut inner = pair.into_inner();
            let lhs = guard_value_to_rust(inner.next().unwrap(), consts);
            let op = inner.next().unwrap().as_str();
            let rhs = guard_value_to_rust(inner.next().unwrap(), consts);
            format!("{} {} {}", lhs, op, rhs)
        }
        _ => pair.as_str().to_string(),
    }
}

fn guard_value_to_rust(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    match pair.as_rule() {
        Rule::guard_value => {
            let mut parts = Vec::new();
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::guard_term => parts.push(guard_term_to_rust(inner, consts)),
                    Rule::add_op => parts.push(format!(" {} ", inner.as_str())),
                    _ => parts.push(inner.as_str().to_string()),
                }
            }
            parts.join("")
        }
        Rule::guard_term => guard_term_to_rust(pair, consts),
        _ => pair.as_str().to_string(),
    }
}

fn guard_term_to_rust(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    match pair.as_rule() {
        Rule::guard_term => guard_term_to_rust(pair.into_inner().next().unwrap(), consts),
        Rule::field_ref => pair.as_str().to_string(),
        Rule::ident => {
            let name = pair.as_str();
            if let Some(val) = consts.get(name) {
                val.clone()
            } else {
                name.to_string()
            }
        }
        Rule::integer => clean_integer(pair.as_str()),
        _ => pair.as_str().to_string(),
    }
}

/// Parse operation block into ParsedOperation + optional ParsedContext.
fn parse_operation(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
) -> (ParsedOperation, Option<ParsedContext>) {
    let mut name = String::new();
    let mut doc_lines: Vec<String> = Vec::new();
    let mut who = None;
    let mut on_account = None;
    let mut pre_status = None;
    let mut post_status = None;
    let mut takes_params: Vec<(String, String)> = Vec::new();
    let mut guard_str_lean = None;
    let mut _guard_str_rust = None;
    let mut effects: Vec<(String, String, String)> = Vec::new();
    let mut calls_program = None;
    let mut calls_discriminator = None;
    let mut calls_accounts: Vec<(String, String)> = Vec::new();
    let mut emits: Vec<String> = Vec::new();
    let mut ctx_accounts: Vec<ParsedAccountEntry> = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::doc_comment => {
                // Strip "///" prefix and optional leading space
                let raw = inner.as_str();
                let text = raw.strip_prefix("///").unwrap_or(raw);
                let text = text.strip_prefix(' ').unwrap_or(text);
                doc_lines.push(text.to_string());
            }
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::op_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::who_clause => {
                        who = Some(extract_ident(clause));
                    }
                    Rule::on_clause => {
                        on_account = Some(extract_ident(clause));
                    }
                    Rule::when_clause => {
                        pre_status = Some(extract_ident(clause));
                    }
                    Rule::then_clause => {
                        post_status = Some(extract_ident(clause));
                    }
                    Rule::takes_block => {
                        takes_params = parse_field_decls(clause);
                    }
                    Rule::guard_clause => {
                        let expr = clause.into_inner().next().unwrap();
                        guard_str_lean = Some(guard_expr_to_lean(expr.clone(), consts));
                        _guard_str_rust = Some(guard_expr_to_rust(expr, consts));
                    }
                    Rule::effect_block => {
                        effects = parse_effect_stmts(clause);
                    }
                    Rule::calls_clause => {
                        let mut parts = clause.into_inner();
                        calls_program = Some(parts.next().unwrap().as_str().to_string());
                        calls_discriminator = Some(parts.next().unwrap().as_str().to_string());
                        // Parse call accounts
                        for p in parts {
                            if p.as_rule() == Rule::call_account_list {
                                for acct in p.into_inner() {
                                    if acct.as_rule() == Rule::call_account {
                                        let mut ap = acct.into_inner();
                                        let aname = ap.next().unwrap().as_str().to_string();
                                        let aflag = ap.next().unwrap().as_str().to_string();
                                        calls_accounts.push((aname, aflag));
                                    }
                                }
                            }
                        }
                    }
                    Rule::emits_clause => {
                        emits.push(extract_ident(clause));
                    }
                    Rule::context_block => {
                        ctx_accounts = parse_context_entries(clause);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Self-transition: `when X` without `then` implies `then X`
    if pre_status.is_some() && post_status.is_none() {
        post_status = pre_status.clone();
    }

    let has_when = pre_status.is_some();
    let has_calls = calls_program.is_some();
    let has_takes = !takes_params.is_empty();
    let has_guard = guard_str_lean.is_some();
    let has_effect = !effects.is_empty();
    let has_u64_fields = false; // filled at spec level

    let ctx = if !ctx_accounts.is_empty() {
        Some(ParsedContext {
            operation: name.clone(),
            accounts: ctx_accounts,
        })
    } else {
        None
    };

    // Store the Lean-form guard in guard_str for Lean generation
    // The Rust-form is available via guard_str_rust for codegen/unit_test
    // For now, store Lean form since that's what check.rs expects.
    // The Rust parser's guard_str was previously a Lean string anyway.
    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join(" "))
    };

    let op = ParsedOperation {
        name,
        doc,
        who,
        on_account,
        has_when,
        pre_status,
        post_status,
        has_calls,
        program_id: calls_program,
        has_u64_fields,
        has_takes,
        has_guard,
        guard_str: guard_str_lean,
        has_effect,
        takes_params,
        effects,
        calls_accounts,
        calls_discriminator,
        emits,
    };

    (op, ctx)
}

/// Parse effect statements: `field = value`, `field += value`, `field -= value`.
fn parse_effect_stmts(pair: pest::iterators::Pair<Rule>) -> Vec<(String, String, String)> {
    let mut effects = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::effect_stmt {
            let raw = inner.as_str();
            // Determine operator by checking the raw text
            let (_op_str, op_name) = if raw.contains("+=") {
                ("+=", "add")
            } else if raw.contains("-=") {
                ("-=", "sub")
            } else {
                ("=", "set")
            };

            let mut parts = inner.into_inner();
            let field = parts.next().unwrap().as_str().to_string();
            let value_pair = parts.next().unwrap();
            let value = effect_value_to_string(value_pair);

            effects.push((field, op_name.to_string(), value));
        }
    }
    effects
}

fn effect_value_to_string(pair: pest::iterators::Pair<Rule>) -> String {
    match pair.as_rule() {
        Rule::effect_value => effect_value_to_string(pair.into_inner().next().unwrap()),
        Rule::field_ref => {
            // state.field -> just the field name for effect values
            pair.as_str()
                .strip_prefix("state.")
                .unwrap_or(pair.as_str())
                .to_string()
        }
        Rule::ident => pair.as_str().to_string(),
        Rule::integer => clean_integer(pair.as_str()),
        _ => pair.as_str().to_string(),
    }
}

/// Parse context entries into ParsedAccountEntry list.
///
/// Grammar: `context_attr = { ident ~ ("(" ~ ident ~ ")")? }`
/// First attr is the type. If it has a paren arg, that's the inner type.
/// E.g., `Account(Multisig)` → type=Account, inner=Multisig.
fn parse_context_entries(pair: pest::iterators::Pair<Rule>) -> Vec<ParsedAccountEntry> {
    let mut accounts = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::context_entry {
            let mut parts = inner.into_inner();
            let acct_name = parts.next().unwrap().as_str().to_string();

            // Collect (name, optional_paren_arg) for each attr
            let mut attrs: Vec<(String, Option<String>)> = Vec::new();
            for attr in parts {
                if attr.as_rule() == Rule::context_attr {
                    let mut idents = attr.into_inner();
                    let name = idents.next().unwrap().as_str().to_string();
                    let arg = idents.next().map(|p| p.as_str().to_string());
                    attrs.push((name, arg));
                }
            }

            if attrs.is_empty() {
                continue;
            }

            // First attr is account type; its paren arg is the inner type
            let account_type = attrs[0].0.clone();
            let inner_type = attrs[0].1.clone();

            // Remaining attrs are modifiers
            let modifiers = &attrs[1..];

            let is_mut = modifiers.iter().any(|(n, _)| n == "mut");
            let is_init = modifiers.iter().any(|(n, _)| n == "init");
            let is_init_if_needed = modifiers.iter().any(|(n, _)| n == "init_if_needed");
            let has_bump = modifiers.iter().any(|(n, _)| n == "bump");

            let payer = modifiers
                .iter()
                .find(|(n, _)| n == "payer")
                .and_then(|(_, v)| v.clone());
            let seeds_ref = modifiers
                .iter()
                .find(|(n, _)| n == "seeds")
                .and_then(|(_, v)| v.clone());
            let close_target = modifiers
                .iter()
                .find(|(n, _)| n == "close")
                .and_then(|(_, v)| v.clone());
            let has_one = modifiers
                .iter()
                .find(|(n, _)| n == "has_one")
                .and_then(|(_, v)| v.clone());
            let token_mint = modifiers
                .iter()
                .find(|(n, _)| n == "token_mint")
                .and_then(|(_, v)| v.clone());
            let token_authority = modifiers
                .iter()
                .find(|(n, _)| n == "token_authority")
                .and_then(|(_, v)| v.clone());

            accounts.push(ParsedAccountEntry {
                name: acct_name,
                account_type,
                inner_type,
                is_mut,
                is_init,
                is_init_if_needed,
                payer,
                seeds_ref,
                has_bump,
                close_target,
                has_one,
                token_mint,
                token_authority,
            });
        }
    }
    accounts
}

/// Parse `pubkey NAME [chunk0, chunk1, chunk2, chunk3]`.
fn parse_pubkey_decl(pair: pest::iterators::Pair<Rule>) -> ParsedPubkey {
    let mut name = String::new();
    let mut chunks = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => name = inner.as_str().to_string(),
            Rule::integer_list => {
                for val in inner.into_inner() {
                    if val.as_rule() == Rule::integer {
                        chunks.push(clean_integer(val.as_str()));
                    }
                }
            }
            _ => {}
        }
    }
    ParsedPubkey { name, chunks }
}

/// Parse errors_decl — handles both simple `[A, B]` and valued `[A = 1 "desc", ...]`.
fn parse_errors_decl(pair: pest::iterators::Pair<Rule>) -> (Vec<String>, Vec<ParsedErrorCode>) {
    let mut codes = Vec::new();
    let mut valued = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::error_valued_list => {
                for entry in inner.into_inner() {
                    if entry.as_rule() == Rule::error_valued_entry {
                        let mut parts = entry.into_inner();
                        let name = parts.next().unwrap().as_str().to_string();
                        let val_str = clean_integer(parts.next().unwrap().as_str());
                        let value = val_str.parse::<u64>().ok();
                        let description = parts.next().map(|p| {
                            p.into_inner()
                                .next()
                                .map(|s| s.as_str().to_string())
                                .unwrap_or_default()
                        });
                        codes.push(name.clone());
                        valued.push(ParsedErrorCode {
                            name,
                            value,
                            description,
                        });
                    }
                }
            }
            Rule::ident_list => {
                for id in inner.into_inner() {
                    if id.as_rule() == Rule::ident {
                        codes.push(id.as_str().to_string());
                    }
                }
            }
            _ => {}
        }
    }
    (codes, valued)
}

/// Parse an instruction block (sBPF).
fn parse_instruction_block(
    pair: pest::iterators::Pair<Rule>,
    global_consts: &Constants,
) -> ParsedInstruction {
    let mut name = String::new();
    let mut doc_lines: Vec<String> = Vec::new();
    let mut discriminant = None;
    let mut entry = None;
    let mut local_consts = global_consts.clone();
    let mut errors = Vec::new();
    let mut input_layout = Vec::new();
    let mut insn_layout = Vec::new();
    let mut guards = Vec::new();
    let mut properties = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::doc_comment => {
                let raw = inner.as_str();
                let text = raw.strip_prefix("///").unwrap_or(raw);
                doc_lines.push(text.strip_prefix(' ').unwrap_or(text).to_string());
            }
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::instruction_item => {
                let item = inner.into_inner().next().unwrap();
                match item.as_rule() {
                    Rule::discriminant_clause => {
                        let val = item.into_inner().next().unwrap();
                        let raw = val.as_str().to_string();
                        // Expand constant reference
                        discriminant = Some(local_consts.get(&raw).cloned().unwrap_or(raw));
                    }
                    Rule::entry_clause => {
                        let val = item.into_inner().next().unwrap();
                        entry = clean_integer(val.as_str()).parse::<u64>().ok();
                    }
                    Rule::const_decl => {
                        let mut parts = item.into_inner();
                        let cname = parts.next().unwrap().as_str().to_string();
                        let cval = clean_integer(parts.next().unwrap().as_str());
                        local_consts.insert(cname, cval);
                    }
                    Rule::errors_decl => {
                        let (_, valued) = parse_errors_decl(item);
                        errors = valued;
                    }
                    Rule::input_layout_block => {
                        input_layout = parse_layout_fields(item);
                    }
                    Rule::insn_layout_block => {
                        insn_layout = parse_layout_fields(item);
                    }
                    Rule::guard_block => {
                        guards.push(parse_guard_block(item, &local_consts));
                    }
                    Rule::property_block => {
                        properties.push(parse_sbpf_property(item));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join(" "))
    };

    // Collect local constants (instruction-scoped only)
    let mut inst_consts = Vec::new();
    for (k, v) in &local_consts {
        if !global_consts.contains_key(k) {
            inst_consts.push((k.clone(), v.clone()));
        }
    }

    ParsedInstruction {
        name,
        doc,
        discriminant,
        entry,
        constants: inst_consts,
        errors,
        input_layout,
        insn_layout,
        guards,
        properties,
    }
}

/// Parse layout fields: `name : Type @ offset "description"`.
fn parse_layout_fields(pair: pest::iterators::Pair<Rule>) -> Vec<ParsedLayoutField> {
    let mut fields = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::layout_field {
            let mut parts = inner.into_inner();
            let name = parts.next().unwrap().as_str().to_string();
            let field_type = parts.next().unwrap().as_str().to_string();
            let offset_str = clean_integer(parts.next().unwrap().as_str());
            let offset = offset_str.parse::<i64>().unwrap_or(0);
            let description = parts.next().map(|p| {
                p.into_inner()
                    .next()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default()
            });
            fields.push(ParsedLayoutField {
                name,
                field_type,
                offset,
                description,
            });
        }
    }
    fields
}

/// Parse a guard block: `guard name { checks ..., error ... }`.
fn parse_guard_block(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> ParsedGuard {
    let mut name = String::new();
    let mut doc_lines: Vec<String> = Vec::new();
    let mut checks = None;
    let mut checks_raw = None;
    let mut error = String::new();
    let mut fuel = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::doc_comment => {
                let raw = inner.as_str();
                let text = raw.strip_prefix("///").unwrap_or(raw);
                doc_lines.push(text.strip_prefix(' ').unwrap_or(text).to_string());
            }
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::guard_item => {
                let item = inner.into_inner().next().unwrap();
                match item.as_rule() {
                    Rule::checks_clause => {
                        let expr = item.into_inner().next().unwrap();
                        // Save raw expression (original constant names preserved)
                        let empty_consts = std::collections::BTreeMap::new();
                        checks_raw = Some(guard_expr_to_rust(expr.clone(), &empty_consts));
                        // Save resolved expression (constants expanded to values)
                        checks = Some(guard_expr_to_rust(expr, consts));
                    }
                    Rule::error_clause => {
                        error = extract_ident(item);
                    }
                    Rule::fuel_clause => {
                        let val_str = item.into_inner().next().unwrap().as_str().replace('_', "");
                        fuel = val_str.parse::<u64>().ok();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join(" "))
    };

    ParsedGuard {
        name,
        doc,
        checks,
        checks_raw,
        error,
        fuel,
    }
}

/// Parse a property block within an sBPF instruction into SbpfProperty.
fn parse_sbpf_property(pair: pest::iterators::Pair<Rule>) -> ParsedSbpfProperty {
    let mut name = String::new();
    let mut doc_lines: Vec<String> = Vec::new();
    let mut kind = SbpfPropertyKind::Generic;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::doc_comment => {
                let raw = inner.as_str();
                let text = raw.strip_prefix("///").unwrap_or(raw);
                doc_lines.push(text.strip_prefix(' ').unwrap_or(text).to_string());
            }
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::prop_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::scope_clause => {
                        let mut targets = Vec::new();
                        for p in clause.into_inner() {
                            match p.as_rule() {
                                Rule::scope_guards => {
                                    targets = vec!["guards".to_string()];
                                }
                                Rule::ident_list => {
                                    for id in p.into_inner() {
                                        if id.as_rule() == Rule::ident {
                                            targets.push(id.as_str().to_string());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        kind = SbpfPropertyKind::Scope { targets };
                    }
                    Rule::flow_clause => {
                        let mut target = String::new();
                        let mut flow_kind = FlowKind::Through(Vec::new());
                        for p in clause.into_inner() {
                            match p.as_rule() {
                                Rule::ident => target = p.as_str().to_string(),
                                Rule::flow_kind => {
                                    // flow_kind raw text starts with "from seeds" or "through"
                                    let raw = p.as_str();
                                    let is_from_seeds = raw.starts_with("from");
                                    let mut items = Vec::new();
                                    for inner in p.into_inner() {
                                        if inner.as_rule() == Rule::ident_list {
                                            for id in inner.into_inner() {
                                                if id.as_rule() == Rule::ident {
                                                    items.push(id.as_str().to_string());
                                                }
                                            }
                                        }
                                    }
                                    flow_kind = if is_from_seeds {
                                        FlowKind::FromSeeds(items)
                                    } else {
                                        FlowKind::Through(items)
                                    };
                                }
                                _ => {}
                            }
                        }
                        kind = SbpfPropertyKind::Flow {
                            target,
                            kind: flow_kind,
                        };
                    }
                    Rule::cpi_block => {
                        let mut program = String::new();
                        let mut instruction = String::new();
                        let mut fields = Vec::new();
                        let mut ident_idx = 0;
                        for p in clause.into_inner() {
                            match p.as_rule() {
                                Rule::ident => {
                                    if ident_idx == 0 {
                                        program = p.as_str().to_string();
                                    } else {
                                        instruction = p.as_str().to_string();
                                    }
                                    ident_idx += 1;
                                }
                                Rule::cpi_field => {
                                    let mut parts = p.into_inner();
                                    let key = parts.next().unwrap().as_str().to_string();
                                    let val = parts
                                        .next()
                                        .map(|v| match v.as_rule() {
                                            Rule::ident_list => {
                                                let items: Vec<String> = v
                                                    .into_inner()
                                                    .filter(|i| i.as_rule() == Rule::ident)
                                                    .map(|i| i.as_str().to_string())
                                                    .collect();
                                                format!("[{}]", items.join(", "))
                                            }
                                            Rule::ident => v.as_str().to_string(),
                                            _ => v.as_str().to_string(),
                                        })
                                        .unwrap_or_default();
                                    fields.push((key, val));
                                }
                                _ => {}
                            }
                        }
                        kind = SbpfPropertyKind::Cpi {
                            program,
                            instruction,
                            fields,
                        };
                    }
                    Rule::after_clause => {
                        // after all guards — look for exit clause next
                        kind = SbpfPropertyKind::HappyPath {
                            exit_code: "0".to_string(),
                        };
                    }
                    Rule::exit_clause => {
                        let code = clause
                            .into_inner()
                            .next()
                            .map(|v| clean_integer(v.as_str()))
                            .unwrap_or_else(|| "0".to_string());
                        kind = SbpfPropertyKind::HappyPath { exit_code: code };
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join(" "))
    };

    ParsedSbpfProperty { name, doc, kind }
}

/// Extract the first ident child from a pair.
fn extract_ident(pair: pest::iterators::Pair<Rule>) -> String {
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::ident {
            return inner.as_str().to_string();
        }
    }
    String::new()
}

/// Parse `invariant name "description"`.
fn parse_invariant(pair: pest::iterators::Pair<Rule>) -> (String, String) {
    let mut name = String::new();
    let mut desc = String::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::string_lit => {
                desc = inner
                    .into_inner()
                    .next()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }
    (name, desc)
}

/// Parse property block.
fn parse_property(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> ParsedProperty {
    let mut name = String::new();
    let mut expression_lean = None;
    let mut preserved_by = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::doc_comment => {
                // doc comments on properties accepted but not stored (for now)
            }
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::prop_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::expr_clause => {
                        let expr = clause.into_inner().next().unwrap();
                        expression_lean = Some(guard_expr_to_lean(expr, consts));
                    }
                    Rule::preserved_by_clause => {
                        // Check for `preserved_by all`
                        let mut is_all = false;
                        let mut idents = Vec::new();
                        for p in clause.into_inner() {
                            match p.as_rule() {
                                Rule::preserved_by_all => {
                                    is_all = true;
                                }
                                Rule::ident_list => {
                                    for id in p.into_inner() {
                                        if id.as_rule() == Rule::ident {
                                            idents.push(id.as_str().to_string());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if is_all {
                            // Sentinel — expanded later in parse() after all ops are collected
                            preserved_by = vec!["all".to_string()];
                        } else {
                            preserved_by = idents;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    ParsedProperty {
        name,
        expression: expression_lean,
        preserved_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTISIG_SPEC: &str =
        include_str!("../../../examples/rust/multisig/multisig.qedspec");

    #[test]
    fn parse_multisig_header() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.program_name, "Multisig");
        assert_eq!(spec.target.as_deref(), Some("quasar"));
        assert_eq!(
            spec.program_id.as_deref(),
            Some("MSig111111111111111111111111111111111111111")
        );
    }

    #[test]
    fn parse_multisig_state() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.state_fields.len(), 4);
        assert_eq!(spec.state_fields[0], ("creator".into(), "Pubkey".into()));
        assert_eq!(spec.state_fields[1], ("threshold".into(), "U8".into()));
        assert_eq!(spec.state_fields[2], ("member_count".into(), "U8".into()));
        assert_eq!(spec.state_fields[3], ("approval_count".into(), "U8".into()));
    }

    #[test]
    fn parse_multisig_lifecycle() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(
            spec.lifecycle_states,
            vec!["Uninitialized", "Active", "HasProposal"]
        );
    }

    #[test]
    fn parse_multisig_pda() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.pdas.len(), 1);
        assert_eq!(spec.pdas[0].name, "vault");
        assert_eq!(spec.pdas[0].seeds, vec!["\"vault\"", "creator"]);
    }

    #[test]
    fn parse_multisig_events() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.events.len(), 5);
        assert_eq!(spec.events[0].name, "VaultCreated");
        assert_eq!(spec.events[0].fields.len(), 3);
    }

    #[test]
    fn parse_multisig_errors() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.error_codes.len(), 5);
        assert_eq!(spec.error_codes[0], "InvalidThreshold");
    }

    #[test]
    fn parse_multisig_operations() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.operations.len(), 6);

        let create = &spec.operations[0];
        assert_eq!(create.name, "create_vault");
        assert_eq!(create.who.as_deref(), Some("creator"));
        assert_eq!(create.pre_status.as_deref(), Some("Uninitialized"));
        assert_eq!(create.post_status.as_deref(), Some("Active"));
        assert!(create.has_guard);
        assert_eq!(create.takes_params.len(), 2);
        assert_eq!(create.effects.len(), 3);
        assert_eq!(
            create.effects[0],
            ("threshold".into(), "set".into(), "threshold".into())
        );
        assert_eq!(
            create.effects[2],
            ("approval_count".into(), "set".into(), "0".into())
        );

        let approve = &spec.operations[2];
        assert_eq!(approve.name, "approve");
        assert_eq!(
            approve.effects[0],
            ("approval_count".into(), "add".into(), "1".into())
        );

        let remove = &spec.operations[5];
        assert_eq!(remove.name, "remove_member");
        assert_eq!(
            remove.effects[0],
            ("member_count".into(), "sub".into(), "1".into())
        );
    }

    #[test]
    fn parse_multisig_guards_lean_form() {
        let spec = parse(MULTISIG_SPEC).unwrap();

        // create_vault guard: threshold > 0 and threshold <= member_count and member_count <= 32
        let create = &spec.operations[0];
        let guard = create.guard_str.as_deref().unwrap();
        assert!(guard.contains("\u{2227}")); // ∧
        assert!(guard.contains("\u{2264}")); // ≤
        assert!(guard.contains("threshold > 0"));

        // approve guard: member_index < state.member_count
        let approve = &spec.operations[2];
        let guard = approve.guard_str.as_deref().unwrap();
        // state.member_count -> s.member_count in Lean form
        assert!(guard.contains("s.member_count"));
    }

    #[test]
    fn parse_multisig_properties() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.properties.len(), 2);

        let tb = &spec.properties[0];
        assert_eq!(tb.name, "threshold_bounded");
        assert!(tb.expression.is_some());
        let expr = tb.expression.as_ref().unwrap();
        assert!(expr.contains("s.threshold"));
        assert!(expr.contains("\u{2264}")); // ≤
        assert_eq!(tb.preserved_by.len(), 6);
    }

    #[test]
    fn parse_multisig_contexts() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.contexts.len(), 5); // 5 operations have context blocks
        let create_ctx = &spec.contexts[0];
        assert_eq!(create_ctx.operation, "create_vault");
        assert_eq!(create_ctx.accounts.len(), 3);
        assert_eq!(create_ctx.accounts[0].name, "creator");
        assert_eq!(create_ctx.accounts[0].account_type, "Signer");
        assert!(create_ctx.accounts[0].is_mut);

        assert_eq!(create_ctx.accounts[1].name, "vault");
        assert_eq!(create_ctx.accounts[1].account_type, "Account");
        assert_eq!(
            create_ctx.accounts[1].inner_type.as_deref(),
            Some("Multisig")
        );
        assert!(create_ctx.accounts[1].is_init);
        assert_eq!(create_ctx.accounts[1].payer.as_deref(), Some("creator"));
        assert!(create_ctx.accounts[1].has_bump);
    }

    // ========================================================================
    // Multi-account (Lending) tests
    // ========================================================================

    const LENDING_SPEC: &str =
        include_str!("../../../examples/rust/lending/lending.qedspec");

    #[test]
    fn parse_lending_account_types() {
        let spec = parse(LENDING_SPEC).unwrap();
        assert_eq!(spec.account_types.len(), 2);

        let pool = &spec.account_types[0];
        assert_eq!(pool.name, "Pool");
        assert_eq!(pool.fields.len(), 4);
        assert_eq!(pool.fields[0], ("authority".into(), "Pubkey".into()));
        assert_eq!(pool.fields[1], ("total_deposits".into(), "U64".into()));
        assert_eq!(pool.lifecycle, vec!["Uninitialized", "Active", "Paused"]);

        let loan = &spec.account_types[1];
        assert_eq!(loan.name, "Loan");
        assert_eq!(loan.fields.len(), 4);
        assert_eq!(loan.fields[0], ("borrower".into(), "Pubkey".into()));
        assert_eq!(loan.lifecycle, vec!["Empty", "Active", "Liquidated"]);
    }

    #[test]
    fn parse_lending_state_fields_from_first_account() {
        let spec = parse(LENDING_SPEC).unwrap();
        // state_fields should be populated from the first account type (Pool)
        assert_eq!(spec.state_fields.len(), 4);
        assert_eq!(spec.state_fields[0].0, "authority");
    }

    #[test]
    fn parse_lending_unified_lifecycle() {
        let spec = parse(LENDING_SPEC).unwrap();
        // lifecycle_states is the union of all account lifecycles
        assert!(spec.lifecycle_states.contains(&"Uninitialized".to_string()));
        assert!(spec.lifecycle_states.contains(&"Active".to_string()));
        assert!(spec.lifecycle_states.contains(&"Paused".to_string()));
        assert!(spec.lifecycle_states.contains(&"Empty".to_string()));
        assert!(spec.lifecycle_states.contains(&"Liquidated".to_string()));
    }

    #[test]
    fn parse_lending_on_clause() {
        let spec = parse(LENDING_SPEC).unwrap();

        let init_pool = &spec.operations[0];
        assert_eq!(init_pool.name, "init_pool");
        assert_eq!(init_pool.on_account.as_deref(), Some("Pool"));

        let borrow = &spec.operations[2];
        assert_eq!(borrow.name, "borrow");
        assert_eq!(borrow.on_account.as_deref(), Some("Loan"));

        // deposit has `on Pool` but no `who`
        let deposit = &spec.operations[1];
        assert_eq!(deposit.on_account.as_deref(), Some("Pool"));
        assert_eq!(deposit.who, None);
    }

    #[test]
    fn parse_lending_pda_linkage() {
        let spec = parse(LENDING_SPEC).unwrap();
        assert_eq!(spec.pdas.len(), 2);
        assert_eq!(spec.pdas[0].name, "pool");
        assert_eq!(spec.pdas[1].name, "loan");

        // Account types should be linked to PDAs by name match
        let pool = &spec.account_types[0];
        assert_eq!(pool.pda_ref.as_deref(), Some("pool"));

        let loan = &spec.account_types[1];
        assert_eq!(loan.pda_ref.as_deref(), Some("loan"));
    }

    #[test]
    fn parse_lending_no_bare_state() {
        // Multi-account specs have no bare `state {}` block.
        // state_fields comes from the first account type.
        let spec = parse(LENDING_SPEC).unwrap();
        // Verify we don't have an implicit account named "Lending"
        assert!(spec.account_types.iter().all(|a| a.name != "Lending"));
    }

    // ========================================================================
    // sBPF (Dropset) tests
    // ========================================================================

    const DROPSET_SPEC: &str =
        include_str!("../../../examples/sbpf/dropset/dropset.qedspec");

    #[test]
    fn parse_dropset_header() {
        let spec = parse(DROPSET_SPEC).unwrap();
        assert_eq!(spec.program_name, "Dropset");
        assert_eq!(spec.target.as_deref(), Some("assembly"));
        assert_eq!(spec.assembly_path.as_deref(), Some("src/dropset.s"));
        assert!(spec.program_id.is_none());
    }

    #[test]
    fn parse_dropset_pubkeys() {
        let spec = parse(DROPSET_SPEC).unwrap();
        assert_eq!(spec.pubkeys.len(), 1);
        assert_eq!(spec.pubkeys[0].name, "RENT");
        assert_eq!(spec.pubkeys[0].chunks.len(), 4);
        assert_eq!(spec.pubkeys[0].chunks[0], "5862609301215225606");
        assert_eq!(spec.pubkeys[0].chunks[3], "2329533411");
    }

    #[test]
    fn parse_dropset_global_errors() {
        let spec = parse(DROPSET_SPEC).unwrap();
        assert_eq!(spec.error_codes.len(), 13);
        assert_eq!(spec.error_codes[0], "InvalidDiscriminant");
        assert_eq!(spec.error_codes[12], "InvalidRentSysvarPubkey");

        assert_eq!(spec.valued_errors.len(), 13);
        assert_eq!(spec.valued_errors[0].name, "InvalidDiscriminant");
        assert_eq!(spec.valued_errors[0].value, Some(1));
        assert_eq!(
            spec.valued_errors[0].description.as_deref(),
            Some("Discriminant is not REGISTER_MARKET")
        );
        assert_eq!(spec.valued_errors[12].value, Some(13));
    }

    #[test]
    fn parse_dropset_instruction() {
        let spec = parse(DROPSET_SPEC).unwrap();
        assert_eq!(spec.instructions.len(), 1);

        let rm = &spec.instructions[0];
        assert_eq!(rm.name, "RegisterMarket");
        assert_eq!(rm.discriminant.as_deref(), Some("0")); // expanded from DISC_REGISTER_MARKET
        assert_eq!(rm.entry, Some(24));
        assert!(rm.doc.is_some());
        assert!(rm.doc.as_ref().unwrap().contains("Validates accounts"));
    }

    #[test]
    fn parse_dropset_instruction_constants() {
        let spec = parse(DROPSET_SPEC).unwrap();
        let rm = &spec.instructions[0];
        // Should have instruction-scoped constants
        assert!(rm.constants.iter().any(|(k, _)| k == "ACCOUNTS_REQUIRED"));
        assert!(rm
            .constants
            .iter()
            .any(|(k, _)| k == "INSTRUCTION_DATA_LEN"));
    }

    #[test]
    fn parse_dropset_input_layout() {
        let spec = parse(DROPSET_SPEC).unwrap();
        let rm = &spec.instructions[0];
        assert_eq!(rm.input_layout.len(), 7);

        let n_accts = &rm.input_layout[0];
        assert_eq!(n_accts.name, "n_accounts");
        assert_eq!(n_accts.field_type, "U64");
        assert_eq!(n_accts.offset, 0);
        assert_eq!(
            n_accts.description.as_deref(),
            Some("Number of accounts in input buffer")
        );

        let mkt_dup = &rm.input_layout[2];
        assert_eq!(mkt_dup.name, "market_dup");
        assert_eq!(mkt_dup.offset, 10344);
    }

    #[test]
    fn parse_dropset_insn_layout() {
        let spec = parse(DROPSET_SPEC).unwrap();
        let rm = &spec.instructions[0];
        assert_eq!(rm.insn_layout.len(), 2);
        assert_eq!(rm.insn_layout[0].name, "insn_len");
        assert_eq!(rm.insn_layout[0].offset, -8);
    }

    #[test]
    fn parse_dropset_guards() {
        let spec = parse(DROPSET_SPEC).unwrap();
        let rm = &spec.instructions[0];
        assert_eq!(rm.guards.len(), 13);

        let g1 = &rm.guards[0];
        assert_eq!(g1.name, "rejects_invalid_discriminant");
        assert_eq!(g1.error, "InvalidDiscriminant");
        assert!(g1.checks.is_some());
        // DISC_REGISTER_MARKET should be expanded to "0"
        assert!(g1.checks.as_ref().unwrap().contains("0"));
        assert!(g1.doc.is_some());

        // P8 has no checks (dynamic offset)
        let g8 = &rm.guards[7];
        assert_eq!(g8.name, "rejects_quote_mint_duplicate");
        assert!(g8.checks.is_none());
        assert_eq!(g8.error, "QuoteMintIsDuplicate");

        // Last guard
        let g13 = &rm.guards[12];
        assert_eq!(g13.name, "rejects_invalid_rent_sysvar_pubkey");
        assert_eq!(g13.error, "InvalidRentSysvarPubkey");
    }

    #[test]
    fn parse_dropset_properties() {
        use crate::check::{FlowKind, SbpfPropertyKind};

        let spec = parse(DROPSET_SPEC).unwrap();
        let rm = &spec.instructions[0];
        assert_eq!(rm.properties.len(), 5);

        // memory_safety — scope guards
        let p0 = &rm.properties[0];
        assert_eq!(p0.name, "memory_safety");
        match &p0.kind {
            SbpfPropertyKind::Scope { targets } => {
                assert_eq!(targets, &["guards"]);
            }
            _ => panic!("expected Scope"),
        }

        // pda_derivation — flow from seeds
        let p1 = &rm.properties[1];
        assert_eq!(p1.name, "pda_derivation");
        match &p1.kind {
            SbpfPropertyKind::Flow { target, kind } => {
                assert_eq!(target, "market_pda");
                match kind {
                    FlowKind::FromSeeds(seeds) => {
                        assert_eq!(seeds, &["base_mint_addr", "quote_mint_addr"]);
                    }
                    _ => panic!("expected FromSeeds"),
                }
            }
            _ => panic!("expected Flow"),
        }

        // account_pointer_flow — flow through
        let p2 = &rm.properties[2];
        match &p2.kind {
            SbpfPropertyKind::Flow { kind, .. } => match kind {
                FlowKind::Through(accounts) => {
                    assert_eq!(accounts, &["market", "system_program", "rent_sysvar"]);
                }
                _ => panic!("expected Through"),
            },
            _ => panic!("expected Flow"),
        }

        // cpi_create_account — CPI block
        let p3 = &rm.properties[3];
        match &p3.kind {
            SbpfPropertyKind::Cpi {
                program,
                instruction,
                fields,
            } => {
                assert_eq!(program, "system_program");
                assert_eq!(instruction, "CreateAccount");
                assert!(fields.iter().any(|(k, v)| k == "payer" && v == "user"));
                assert!(fields
                    .iter()
                    .any(|(k, v)| k == "target" && v == "market_pda"));
            }
            _ => panic!("expected Cpi"),
        }

        // accepts_valid_input — happy path
        let p4 = &rm.properties[4];
        match &p4.kind {
            SbpfPropertyKind::HappyPath { exit_code } => {
                assert_eq!(exit_code, "0");
            }
            _ => panic!("expected HappyPath"),
        }
    }
}
