//! Parser for `.qedspec` files — the standalone spec format.
//!
//! Uses pest (PEG parser) to parse `.qedspec` into `ParsedSpec`,
//! the same IR consumed by codegen, kani, unit_test, check, and lean_gen.

use anyhow::{Context, Result};
use pest::Parser;
use pest_derive::Parser;
use std::path::Path;

use crate::check::{
    FlowKind, ParsedAbort, ParsedAccountEntry, ParsedAccountType, ParsedContext, ParsedCover,
    ParsedEnvironment, ParsedErrorCode, ParsedEvent, ParsedGuard, ParsedHandler,
    ParsedHandlerAccount, ParsedInstruction, ParsedLayoutField, ParsedLiveness, ParsedOperation,
    ParsedPda, ParsedProperty, ParsedPubkey, ParsedSbpfProperty, ParsedSpec, ParsedTransfer,
    SbpfPropertyKind,
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
    let mut handlers: Vec<ParsedHandler> = Vec::new();
    let mut properties: Vec<ParsedProperty> = Vec::new();
    let mut invariants: Vec<(String, String)> = Vec::new();
    let mut contexts: Vec<ParsedContext> = Vec::new();
    let mut covers: Vec<ParsedCover> = Vec::new();
    let mut liveness_props: Vec<ParsedLiveness> = Vec::new();
    let mut environments: Vec<ParsedEnvironment> = Vec::new();
    let mut target: Option<String> = None;
    let mut schemas: std::collections::BTreeMap<String, ParsedHandler> =
        std::collections::BTreeMap::new();

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
                    Rule::schema_block => {
                        let (schema, _includes) = parse_handler_block(inner, &constants);
                        schemas.insert(schema.name.clone(), schema);
                    }
                    Rule::handler_block => {
                        let (mut handler, handler_includes) =
                            parse_handler_block(inner, &constants);
                        // Resolve schema includes: merge schema clauses as defaults
                        for schema_name in &handler_includes {
                            if let Some(schema) = schemas.get(schema_name) {
                                merge_schema_into_handler(&mut handler, schema);
                            }
                        }
                        handlers.push(handler);
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
                    Rule::cover_block => {
                        covers.push(parse_cover(inner, &constants));
                    }
                    Rule::liveness_block => {
                        liveness_props.push(parse_liveness(inner));
                    }
                    Rule::environment_block => {
                        environments.push(parse_environment(inner, &constants));
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

    // Expand `preserved_by all` to include both operations and handlers
    let all_handler_names: Vec<String> = handlers
        .iter()
        .map(|h| h.name.clone())
        .chain(operations.iter().map(|o| o.name.clone()))
        .collect();
    for prop in &mut properties {
        if prop.preserved_by.len() == 1 && prop.preserved_by[0] == "all" {
            prop.preserved_by = all_handler_names.clone();
        }
    }

    // Populate unified handlers from legacy operation blocks (backward compat)
    for op in &operations {
        let ctx = contexts.iter().find(|c| c.operation == op.name);
        handlers.push(operation_to_handler(op, ctx));
    }

    // Populate unified handlers from legacy instruction blocks (backward compat)
    for instr in &instructions {
        handlers.push(instruction_to_handler(instr));
    }

    // Compute U64 field metadata
    let u64_field_names: Vec<String> = state_fields
        .iter()
        .filter(|(_, ty)| ty == "U64")
        .map(|(name, _)| name.clone())
        .collect();
    let has_u64_fields = !u64_field_names.is_empty();

    Ok(ParsedSpec {
        handlers,
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
        covers,
        liveness_props,
        environments,
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

/// Expression context controls how `state.field` and `old(state.field)` are rendered.
/// - Guard: `state.field` → `s.field` (Lean) / `state.field` (Rust). `old()` is invalid.
/// - Ensures: `state.field` → `s'.field` (post-state), `old(state.field)` → `s.field` (pre-state).
#[derive(Debug, Clone, Copy, PartialEq)]
enum ExprContext {
    Guard,
    Ensures,
}

/// Reconstruct a guard expression from the pest AST into two forms:
/// 1. Lean form (with Unicode operators)
/// 2. Rust/plain form (with ASCII operators)
///
/// Named constants are expanded inline: `MAX_TVL` → `10000000`.
fn guard_expr_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_expr_to_lean_ctx(pair, consts, ExprContext::Guard)
}

fn guard_expr_to_lean_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_expr => guard_expr_to_lean_ctx(pair.into_inner().next().unwrap(), consts, ctx),
        Rule::guard_or => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_lean_ctx(p, consts, ctx))
                .collect();
            parts.join(" \u{2228} ") // ∨
        }
        Rule::guard_implies => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_lean_ctx(p, consts, ctx))
                .collect();
            parts.join(" \u{2192} ") // →
        }
        Rule::guard_and => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_lean_ctx(p, consts, ctx))
                .collect();
            parts.join(" \u{2227} ") // ∧
        }
        Rule::guard_not => {
            let mut inner = pair.into_inner();
            let first = inner.next().unwrap();
            if first.as_rule() == Rule::kw_not {
                // "not" ~ guard_not
                let operand = guard_expr_to_lean_ctx(inner.next().unwrap(), consts, ctx);
                format!("\u{00AC}({})", operand) // ¬(...)
            } else {
                // guard_atom passthrough
                guard_expr_to_lean_ctx(first, consts, ctx)
            }
        }
        Rule::guard_atom => guard_expr_to_lean_ctx(pair.into_inner().next().unwrap(), consts, ctx),
        Rule::guard_comparison => {
            let mut inner = pair.into_inner();
            let lhs = guard_value_to_lean_ctx(inner.next().unwrap(), consts, ctx);
            let op = inner.next().unwrap().as_str();
            let rhs = guard_value_to_lean_ctx(inner.next().unwrap(), consts, ctx);
            let lean_op = match op {
                "<=" => "\u{2264}", // ≤
                ">=" => "\u{2265}", // ≥
                "!=" => "\u{2260}", // ≠
                // == maps to propositional = in Lean (all types derive DecidableEq)
                "==" => "=",
                other => other,
            };
            format!("{} {} {}", lhs, lean_op, rhs)
        }
        // Quantifiers: forall/exists
        Rule::quantifier_expr => {
            let mut inner = pair.into_inner();
            let kind = inner.next().unwrap().as_str(); // "forall" | "exists"
            let var_name = inner.next().unwrap().as_str();
            let var_type = inner.next().unwrap().as_str();
            let body = guard_expr_to_lean_ctx(inner.next().unwrap(), consts, ctx);
            let lean_q = match kind {
                "forall" => "\u{2200}", // ∀
                "exists" => "\u{2203}", // ∃
                _ => kind,
            };
            let lean_type = match var_type {
                "U64" | "U32" | "U16" | "U8" => "Nat",
                "I64" | "I32" | "I16" | "I8" => "Int",
                other => other,
            };
            format!("{} {} : {}, {}", lean_q, var_name, lean_type, body)
        }
        // guard_value can appear directly in guard_atom (non-comparison expressions)
        Rule::guard_value => guard_value_to_lean_ctx(pair, consts, ctx),
        Rule::guard_product => guard_product_to_lean_ctx(pair, consts, ctx),
        Rule::guard_term => guard_term_to_lean_ctx(pair, consts, ctx),
        _ => pair.as_str().to_string(),
    }
}

#[allow(dead_code)]
fn guard_value_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_value_to_lean_ctx(pair, consts, ExprContext::Guard)
}

fn guard_value_to_lean_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_value => {
            // guard_value = { guard_product ~ (add_op ~ guard_product)* }
            let mut parts = Vec::new();
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::guard_product => {
                        parts.push(guard_product_to_lean_ctx(inner, consts, ctx))
                    }
                    Rule::add_op => parts.push(format!(" {} ", inner.as_str())),
                    _ => parts.push(inner.as_str().to_string()),
                }
            }
            parts.join("")
        }
        Rule::guard_product => guard_product_to_lean_ctx(pair, consts, ctx),
        Rule::guard_term => guard_term_to_lean_ctx(pair, consts, ctx),
        _ => pair.as_str().to_string(),
    }
}

#[allow(dead_code)]
fn guard_product_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_product_to_lean_ctx(pair, consts, ExprContext::Guard)
}

fn guard_product_to_lean_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_product => {
            // guard_product = { guard_term ~ (mul_op ~ guard_term)* }
            let mut parts = Vec::new();
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::guard_term => parts.push(guard_term_to_lean_ctx(inner, consts, ctx)),
                    Rule::mul_op => parts.push(format!(" {} ", inner.as_str())),
                    _ => parts.push(inner.as_str().to_string()),
                }
            }
            parts.join("")
        }
        Rule::guard_term => guard_term_to_lean_ctx(pair, consts, ctx),
        _ => pair.as_str().to_string(),
    }
}

#[allow(dead_code)]
fn guard_term_to_lean(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_term_to_lean_ctx(pair, consts, ExprContext::Guard)
}

fn guard_term_to_lean_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_term => guard_term_to_lean_ctx(pair.into_inner().next().unwrap(), consts, ctx),
        Rule::old_expr => {
            // old(state.field) — only valid in ensures context
            let inner = pair.into_inner().next().unwrap(); // field_ref
            let field = inner
                .as_str()
                .strip_prefix("state.")
                .unwrap_or(inner.as_str());
            match ctx {
                ExprContext::Ensures => format!("s.{}", field), // pre-state
                ExprContext::Guard => {
                    // old() in guard context is a spec error — render with marker
                    format!("«old({})»", field)
                }
            }
        }
        Rule::field_ref => {
            let field = pair
                .as_str()
                .strip_prefix("state.")
                .unwrap_or(pair.as_str());
            match ctx {
                ExprContext::Guard => format!("s.{}", field),
                ExprContext::Ensures => format!("s'.{}", field), // post-state
            }
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
    guard_expr_to_rust_ctx(pair, consts, ExprContext::Guard)
}

fn guard_expr_to_rust_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_expr => guard_expr_to_rust_ctx(pair.into_inner().next().unwrap(), consts, ctx),
        Rule::guard_or => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_rust_ctx(p, consts, ctx))
                .collect();
            parts.join(" || ")
        }
        Rule::guard_implies => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_rust_ctx(p, consts, ctx))
                .collect();
            if parts.len() == 2 {
                format!("!({})) || ({})", parts[0], parts[1])
            } else {
                // Single element, no implication
                parts.join("")
            }
        }
        Rule::guard_and => {
            let parts: Vec<String> = pair
                .into_inner()
                .map(|p| guard_expr_to_rust_ctx(p, consts, ctx))
                .collect();
            parts.join(" && ")
        }
        Rule::guard_not => {
            let mut inner = pair.into_inner();
            let first = inner.next().unwrap();
            if first.as_rule() == Rule::kw_not {
                let operand = guard_expr_to_rust_ctx(inner.next().unwrap(), consts, ctx);
                format!("!({})", operand)
            } else {
                guard_expr_to_rust_ctx(first, consts, ctx)
            }
        }
        Rule::guard_atom => guard_expr_to_rust_ctx(pair.into_inner().next().unwrap(), consts, ctx),
        Rule::guard_comparison => {
            let mut inner = pair.into_inner();
            let lhs = guard_value_to_rust_ctx(inner.next().unwrap(), consts, ctx);
            let op = inner.next().unwrap().as_str();
            let rhs = guard_value_to_rust_ctx(inner.next().unwrap(), consts, ctx);
            format!("{} {} {}", lhs, op, rhs)
        }
        // Quantifiers: rendered as comments in Rust (not runtime-checkable)
        Rule::quantifier_expr => {
            let mut inner = pair.into_inner();
            let kind = inner.next().unwrap().as_str();
            let var_name = inner.next().unwrap().as_str();
            let var_type = inner.next().unwrap().as_str();
            let body = guard_expr_to_rust_ctx(inner.next().unwrap(), consts, ctx);
            format!("/* {} {} : {}, {} */", kind, var_name, var_type, body)
        }
        // guard_value can appear directly in guard_atom (non-comparison expressions)
        Rule::guard_value => guard_value_to_rust_ctx(pair, consts, ctx),
        Rule::guard_product => guard_product_to_rust_ctx(pair, consts, ctx),
        Rule::guard_term => guard_term_to_rust_ctx(pair, consts, ctx),
        _ => pair.as_str().to_string(),
    }
}

#[allow(dead_code)]
fn guard_value_to_rust(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_value_to_rust_ctx(pair, consts, ExprContext::Guard)
}

fn guard_value_to_rust_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_value => {
            let mut parts = Vec::new();
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::guard_product => {
                        parts.push(guard_product_to_rust_ctx(inner, consts, ctx))
                    }
                    Rule::add_op => parts.push(format!(" {} ", inner.as_str())),
                    _ => parts.push(inner.as_str().to_string()),
                }
            }
            parts.join("")
        }
        Rule::guard_product => guard_product_to_rust_ctx(pair, consts, ctx),
        Rule::guard_term => guard_term_to_rust_ctx(pair, consts, ctx),
        _ => pair.as_str().to_string(),
    }
}

#[allow(dead_code)]
fn guard_product_to_rust(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_product_to_rust_ctx(pair, consts, ExprContext::Guard)
}

fn guard_product_to_rust_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_product => {
            let mut parts = Vec::new();
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::guard_term => parts.push(guard_term_to_rust_ctx(inner, consts, ctx)),
                    Rule::mul_op => parts.push(format!(" {} ", inner.as_str())),
                    _ => parts.push(inner.as_str().to_string()),
                }
            }
            parts.join("")
        }
        Rule::guard_term => guard_term_to_rust_ctx(pair, consts, ctx),
        _ => pair.as_str().to_string(),
    }
}

#[allow(dead_code)]
fn guard_term_to_rust(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> String {
    guard_term_to_rust_ctx(pair, consts, ExprContext::Guard)
}

fn guard_term_to_rust_ctx(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
    ctx: ExprContext,
) -> String {
    match pair.as_rule() {
        Rule::guard_term => guard_term_to_rust_ctx(pair.into_inner().next().unwrap(), consts, ctx),
        Rule::old_expr => {
            // old(state.field) — only valid in ensures context
            let inner = pair.into_inner().next().unwrap(); // field_ref
            let raw = inner.as_str();
            match ctx {
                ExprContext::Ensures => format!("old_{}", raw), // old_state.field
                ExprContext::Guard => format!("/*old({})*/", raw),
            }
        }
        Rule::field_ref => {
            match ctx {
                ExprContext::Guard => pair.as_str().to_string(),
                ExprContext::Ensures => {
                    // state.field → new_state.field in ensures
                    let raw = pair.as_str();
                    format!("new_{}", raw)
                }
            }
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
    let mut aborts_if: Vec<ParsedAbort> = Vec::new();

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
                    Rule::aborts_if_clause => {
                        let mut parts = clause.into_inner();
                        let expr = parts.next().unwrap();
                        let lean_expr = guard_expr_to_lean(expr.clone(), consts);
                        let rust_expr = guard_expr_to_rust(expr, consts);
                        let error_name = parts.next().unwrap().as_str().to_string();
                        aborts_if.push(ParsedAbort {
                            lean_expr,
                            rust_expr,
                            error_name,
                        });
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
        aborts_if,
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

// ============================================================================
// Unified handler parsing (v3)
// ============================================================================

/// Parse a `handler Name { ... }` or `schema Name { ... }` block into a ParsedHandler.
/// Returns the handler and a list of schema names to include (resolved later).
fn parse_handler_block(
    pair: pest::iterators::Pair<Rule>,
    consts: &Constants,
) -> (ParsedHandler, Vec<String>) {
    let mut name = String::new();
    let mut doc_lines: Vec<String> = Vec::new();
    let mut who = None;
    let mut on_account = None;
    let mut pre_status = None;
    let mut post_status = None;
    let mut takes_params: Vec<(String, String)> = Vec::new();
    let mut requires: Vec<crate::check::ParsedRequires> = Vec::new();
    let mut ensures: Vec<crate::check::ParsedEnsures> = Vec::new();
    let mut modifies: Option<Vec<String>> = None;
    let mut let_bindings: Vec<(String, String, String)> = Vec::new();
    let mut aborts_total = false;
    let mut includes: Vec<String> = Vec::new();
    let mut effects: Vec<(String, String, String)> = Vec::new();
    let mut accounts: Vec<ParsedHandlerAccount> = Vec::new();
    let mut transfers: Vec<ParsedTransfer> = Vec::new();
    let mut emits: Vec<String> = Vec::new();
    let mut invariants: Vec<String> = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::doc_comment => {
                let raw = inner.as_str();
                let text = raw.strip_prefix("///").unwrap_or(raw);
                let text = text.strip_prefix(' ').unwrap_or(text);
                doc_lines.push(text.to_string());
            }
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::handler_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::include_clause => {
                        includes.push(extract_ident(clause));
                    }
                    Rule::aborts_total_clause => {
                        aborts_total = true;
                    }
                    Rule::who_clause => who = Some(extract_ident(clause)),
                    Rule::on_clause => on_account = Some(extract_ident(clause)),
                    Rule::when_clause => pre_status = Some(extract_ident(clause)),
                    Rule::then_clause => post_status = Some(extract_ident(clause)),
                    Rule::takes_block => takes_params = parse_field_decls(clause),
                    Rule::requires_clause => {
                        let mut parts = clause.into_inner();
                        let expr = parts.next().unwrap();
                        let lean_expr = guard_expr_to_lean(expr.clone(), consts);
                        let rust_expr = guard_expr_to_rust(expr, consts);
                        let error_name = parts.next().map(|p| p.as_str().to_string());
                        requires.push(crate::check::ParsedRequires {
                            lean_expr,
                            rust_expr,
                            error_name,
                        });
                    }
                    Rule::ensures_clause => {
                        let expr = clause.into_inner().next().unwrap();
                        let lean_expr =
                            guard_expr_to_lean_ctx(expr.clone(), consts, ExprContext::Ensures);
                        let rust_expr = guard_expr_to_rust_ctx(expr, consts, ExprContext::Ensures);
                        ensures.push(crate::check::ParsedEnsures {
                            lean_expr,
                            rust_expr,
                        });
                    }
                    Rule::modifies_clause => {
                        let mut fields = Vec::new();
                        for inner in clause.into_inner() {
                            if inner.as_rule() == Rule::ident_list {
                                for id in inner.into_inner() {
                                    if id.as_rule() == Rule::ident {
                                        fields.push(id.as_str().to_string());
                                    }
                                }
                            }
                        }
                        modifies = Some(fields);
                    }
                    Rule::let_clause => {
                        let mut parts = clause.into_inner();
                        let binding_name = parts.next().unwrap().as_str().to_string();
                        let expr = parts.next().unwrap();
                        let lean_expr = guard_expr_to_lean(expr.clone(), consts);
                        let rust_expr = guard_expr_to_rust(expr, consts);
                        let_bindings.push((binding_name, lean_expr, rust_expr));
                    }
                    Rule::effect_block => effects = parse_effect_stmts(clause),
                    Rule::accounts_block => accounts = parse_accounts_block(clause),
                    Rule::transfers_block => transfers = parse_transfers_block(clause),
                    Rule::emits_clause => emits.push(extract_ident(clause)),
                    Rule::handler_invariant_clause => {
                        invariants.push(extract_ident(clause));
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

    let doc = if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join(" "))
    };

    (
        ParsedHandler {
            name,
            doc,
            who,
            on_account,
            pre_status,
            post_status,
            takes_params,
            guard_str: None,
            guard_str_rust: None,
            aborts_if: Vec::new(),
            requires,
            ensures,
            modifies,
            let_bindings,
            aborts_total,
            effects,
            accounts,
            transfers,
            emits,
            invariants,
            properties: Vec::new(),
        },
        includes,
    )
}

/// Parse `accounts { name : attr, attr, ... }` block into IDL-level descriptors.
/// Merge schema clauses into a handler. Schema provides defaults:
/// - Scalar fields (who, on, when, then, guard): schema value used only if handler doesn't set it
/// - Collection fields (requires, ensures, let_bindings, etc.): schema entries prepended
/// - Boolean fields (aborts_total): OR'd together
fn merge_schema_into_handler(handler: &mut ParsedHandler, schema: &ParsedHandler) {
    // Scalar defaults
    if handler.who.is_none() {
        handler.who = schema.who.clone();
    }
    if handler.on_account.is_none() {
        handler.on_account = schema.on_account.clone();
    }
    if handler.pre_status.is_none() {
        handler.pre_status = schema.pre_status.clone();
    }
    if handler.post_status.is_none() {
        handler.post_status = schema.post_status.clone();
    }
    if handler.modifies.is_none() {
        handler.modifies = schema.modifies.clone();
    }

    // Collection prepend (schema clauses come before handler's own)
    let mut merged_requires = schema.requires.clone();
    merged_requires.append(&mut handler.requires);
    handler.requires = merged_requires;

    let mut merged_ensures = schema.ensures.clone();
    merged_ensures.append(&mut handler.ensures);
    handler.ensures = merged_ensures;

    let mut merged_let = schema.let_bindings.clone();
    merged_let.append(&mut handler.let_bindings);
    handler.let_bindings = merged_let;

    let mut merged_effects = schema.effects.clone();
    merged_effects.append(&mut handler.effects);
    handler.effects = merged_effects;

    let mut merged_invariants = schema.invariants.clone();
    merged_invariants.append(&mut handler.invariants);
    handler.invariants = merged_invariants;

    // Takes params: merge, avoiding duplicates by name
    for param in &schema.takes_params {
        if !handler.takes_params.iter().any(|(n, _)| n == &param.0) {
            handler.takes_params.push(param.clone());
        }
    }

    // Boolean OR
    handler.aborts_total = handler.aborts_total || schema.aborts_total;
}

fn parse_accounts_block(pair: pest::iterators::Pair<Rule>) -> Vec<ParsedHandlerAccount> {
    let mut accounts = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::account_descriptor {
            let mut parts = inner.into_inner();
            let name = parts.next().unwrap().as_str().to_string();

            let mut is_signer = false;
            let mut is_writable = false;
            let mut is_program = false;
            let mut pda_seeds = None;
            let mut account_type = None;
            let mut authority = None;

            for attr in parts {
                if attr.as_rule() == Rule::acct_attr {
                    let inner_attr = attr.into_inner().next().unwrap();
                    match inner_attr.as_rule() {
                        Rule::acct_simple_attr => {
                            let kw = inner_attr.into_inner().next().unwrap().as_str();
                            match kw {
                                "signer" => is_signer = true,
                                "writable" => is_writable = true,
                                "readonly" => {} // default
                                "program" => is_program = true,
                                _ => {}
                            }
                        }
                        Rule::acct_pda_attr => {
                            let mut seeds = Vec::new();
                            for seed_part in inner_attr.into_inner() {
                                if seed_part.as_rule() == Rule::pda_seed_list {
                                    for seed in seed_part.into_inner() {
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
                            }
                            pda_seeds = Some(seeds);
                        }
                        Rule::acct_type_attr => {
                            let ty = inner_attr.into_inner().next().unwrap().as_str().to_string();
                            account_type = Some(ty);
                        }
                        Rule::acct_authority_attr => {
                            let auth = inner_attr.into_inner().next().unwrap().as_str().to_string();
                            authority = Some(auth);
                        }
                        _ => {}
                    }
                }
            }

            accounts.push(ParsedHandlerAccount {
                name,
                is_signer,
                is_writable,
                is_program,
                pda_seeds,
                account_type,
                authority,
            });
        }
    }
    accounts
}

/// Parse `transfers { from A to B amount X authority Y }` block.
fn parse_transfers_block(pair: pest::iterators::Pair<Rule>) -> Vec<ParsedTransfer> {
    let mut transfers = Vec::new();
    for inner in pair.into_inner() {
        if inner.as_rule() == Rule::transfer_clause {
            let mut parts = inner.into_inner();
            let from = parts.next().unwrap().as_str().to_string();
            let to = parts.next().unwrap().as_str().to_string();
            let mut amount = None;
            let mut authority = None;

            for field in parts {
                if field.as_rule() == Rule::transfer_fields {
                    let raw = field.as_str();
                    let inner_pair = field.into_inner().next().unwrap();
                    if raw.starts_with("amount") {
                        amount = Some(inner_pair.as_str().to_string());
                    } else if raw.starts_with("authority") {
                        authority = Some(inner_pair.as_str().to_string());
                    }
                }
            }

            transfers.push(ParsedTransfer {
                from,
                to,
                amount,
                authority,
            });
        }
    }
    transfers
}

/// Convert a legacy ParsedOperation into a ParsedHandler (backward compat).
fn operation_to_handler(op: &ParsedOperation, ctx: Option<&ParsedContext>) -> ParsedHandler {
    let accounts = if let Some(ctx) = ctx {
        ctx.accounts
            .iter()
            .map(|a| ParsedHandlerAccount {
                name: a.name.clone(),
                is_signer: a.account_type == "Signer",
                is_writable: a.is_mut || a.is_init,
                is_program: a.account_type == "Program",
                pda_seeds: a.seeds_ref.as_ref().map(|_s| Vec::new()), // seeds ref, not inline
                account_type: a.inner_type.clone(),
                authority: a.token_authority.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };

    let transfers = if op.has_calls {
        vec![ParsedTransfer {
            from: op
                .calls_accounts
                .first()
                .map(|(n, _)| n.clone())
                .unwrap_or_default(),
            to: op
                .calls_accounts
                .get(1)
                .map(|(n, _)| n.clone())
                .unwrap_or_default(),
            amount: None,
            authority: op.calls_accounts.last().map(|(n, _)| n.clone()),
        }]
    } else {
        Vec::new()
    };

    ParsedHandler {
        name: op.name.clone(),
        doc: op.doc.clone(),
        who: op.who.clone(),
        on_account: op.on_account.clone(),
        pre_status: op.pre_status.clone(),
        post_status: op.post_status.clone(),
        takes_params: op.takes_params.clone(),
        guard_str: op.guard_str.clone(),
        guard_str_rust: None,
        aborts_if: op.aborts_if.clone(),
        requires: Vec::new(),
        ensures: Vec::new(),
        modifies: None,
        let_bindings: Vec::new(),
        aborts_total: false,
        effects: op.effects.clone(),
        accounts,
        transfers,
        emits: op.emits.clone(),
        invariants: Vec::new(),
        properties: Vec::new(),
    }
}

/// Convert a legacy ParsedInstruction into a ParsedHandler (backward compat).
fn instruction_to_handler(instr: &ParsedInstruction) -> ParsedHandler {
    // Collect guard names and property names as handler-level properties
    let properties: Vec<String> = instr
        .guards
        .iter()
        .map(|g| g.name.clone())
        .chain(instr.properties.iter().map(|p| p.name.clone()))
        .collect();

    ParsedHandler {
        name: instr.name.clone(),
        doc: instr.doc.clone(),
        who: None,
        on_account: None,
        pre_status: None,
        post_status: None,
        takes_params: Vec::new(),
        guard_str: None,
        guard_str_rust: None,
        aborts_if: Vec::new(),
        requires: Vec::new(),
        ensures: Vec::new(),
        modifies: None,
        let_bindings: Vec::new(),
        aborts_total: false,
        effects: Vec::new(),
        accounts: Vec::new(),
        transfers: Vec::new(),
        emits: Vec::new(),
        invariants: Vec::new(),
        properties,
    }
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

/// Parse a cover block (reachability).
fn parse_cover(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> ParsedCover {
    let mut name = String::new();
    let mut traces = Vec::new();
    let mut reachable = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::cover_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::trace_clause => {
                        let mut ops = Vec::new();
                        for p in clause.into_inner() {
                            if p.as_rule() == Rule::ident_list {
                                for id in p.into_inner() {
                                    if id.as_rule() == Rule::ident {
                                        ops.push(id.as_str().to_string());
                                    }
                                }
                            }
                        }
                        traces.push(ops);
                    }
                    Rule::reachable_clause => {
                        let mut op_name = String::new();
                        let mut when_expr = None;
                        for p in clause.into_inner() {
                            match p.as_rule() {
                                Rule::ident => {
                                    op_name = p.as_str().to_string();
                                }
                                Rule::guard_expr => {
                                    when_expr = Some(guard_expr_to_lean(p, consts));
                                }
                                _ => {}
                            }
                        }
                        reachable.push((op_name, when_expr));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    ParsedCover {
        name,
        traces,
        reachable,
    }
}

/// Parse a liveness block (leads-to).
fn parse_liveness(pair: pest::iterators::Pair<Rule>) -> ParsedLiveness {
    let mut name = String::new();
    let mut from_state = String::new();
    let mut leads_to_state = String::new();
    let mut via_ops = Vec::new();
    let mut within_steps = None;

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::liveness_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::from_clause => {
                        from_state = extract_ident(clause);
                    }
                    Rule::leads_to_clause => {
                        leads_to_state = extract_ident(clause);
                    }
                    Rule::via_clause => {
                        for p in clause.into_inner() {
                            if p.as_rule() == Rule::ident_list {
                                for id in p.into_inner() {
                                    if id.as_rule() == Rule::ident {
                                        via_ops.push(id.as_str().to_string());
                                    }
                                }
                            }
                        }
                    }
                    Rule::within_clause => {
                        for p in clause.into_inner() {
                            if p.as_rule() == Rule::integer {
                                within_steps =
                                    Some(clean_integer(p.as_str()).parse::<u64>().unwrap_or(0));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    ParsedLiveness {
        name,
        from_state,
        leads_to_state,
        via_ops,
        within_steps,
    }
}

/// Parse an environment block (external state).
fn parse_environment(pair: pest::iterators::Pair<Rule>, consts: &Constants) -> ParsedEnvironment {
    let mut name = String::new();
    let mut mutates = Vec::new();
    let mut constraints = Vec::new();
    let mut constraints_rust = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => {
                name = inner.as_str().to_string();
            }
            Rule::env_clause => {
                let clause = inner.into_inner().next().unwrap();
                match clause.as_rule() {
                    Rule::mutates_clause => {
                        let mut parts = clause.into_inner();
                        let field_name = parts.next().unwrap().as_str().to_string();
                        let field_type = parts.next().unwrap().as_str().to_string();
                        mutates.push((field_name, field_type));
                    }
                    Rule::constraint_clause => {
                        let expr = clause.into_inner().next().unwrap();
                        constraints.push(guard_expr_to_lean(expr.clone(), consts));
                        constraints_rust.push(guard_expr_to_rust(expr, consts));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    ParsedEnvironment {
        name,
        mutates,
        constraints,
        constraints_rust,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTISIG_SPEC: &str = include_str!("../../../examples/rust/multisig/multisig.qedspec");

    #[test]
    fn parse_multisig_header() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.program_name, "Multisig");
        // New unified syntax: no target or program_id
        assert!(spec.target.is_none());
        assert!(spec.program_id.is_none());
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
    fn parse_multisig_handlers() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.handlers.len(), 6);

        let create = &spec.handlers[0];
        assert_eq!(create.name, "create_vault");
        assert_eq!(create.who.as_deref(), Some("creator"));
        assert_eq!(create.pre_status.as_deref(), Some("Uninitialized"));
        assert_eq!(create.post_status.as_deref(), Some("Active"));
        assert!(create.has_guard());
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

        let approve = &spec.handlers[2];
        assert_eq!(approve.name, "approve");
        assert_eq!(
            approve.effects[0],
            ("approval_count".into(), "add".into(), "1".into())
        );

        let remove = &spec.handlers[5];
        assert_eq!(remove.name, "remove_member");
        assert_eq!(
            remove.effects[0],
            ("member_count".into(), "sub".into(), "1".into())
        );
    }

    #[test]
    fn parse_multisig_requires_lean_form() {
        let spec = parse(MULTISIG_SPEC).unwrap();

        // create_vault: requires threshold > 0 and threshold <= member_count else InvalidThreshold
        let create = &spec.handlers[0];
        assert!(create.requires.len() >= 1);
        let req = &create.requires[0];
        assert!(req.lean_expr.contains("\u{2227}")); // ∧
        assert!(req.lean_expr.contains("\u{2264}")); // ≤
        assert!(req.lean_expr.contains("threshold > 0"));

        // approve: requires member_index < state.member_count else NotAMember
        let approve = &spec.handlers[2];
        assert!(approve.requires.len() >= 1);
        let req = &approve.requires[0];
        // state.member_count -> s.member_count in Lean form
        assert!(req.lean_expr.contains("s.member_count"));
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
    fn parse_multisig_handler_accounts() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        // 6 handlers have accounts blocks
        assert_eq!(spec.handlers.len(), 6);
        let create = &spec.handlers[0];
        assert_eq!(create.name, "create_vault");
        assert_eq!(create.accounts.len(), 3);
        assert_eq!(create.accounts[0].name, "creator");
        assert!(create.accounts[0].is_signer);
        assert!(create.accounts[0].is_writable);

        assert_eq!(create.accounts[1].name, "vault");
        assert!(create.accounts[1].is_writable);
        assert!(create.accounts[1].pda_seeds.is_some());
    }

    // ========================================================================
    // Multi-account (Lending) tests
    // ========================================================================

    const LENDING_SPEC: &str = include_str!("../../../examples/rust/lending/lending.qedspec");

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

        let init_pool = &spec.handlers[0];
        assert_eq!(init_pool.name, "init_pool");
        assert_eq!(init_pool.on_account.as_deref(), Some("Pool"));

        let borrow = &spec.handlers[2];
        assert_eq!(borrow.name, "borrow");
        assert_eq!(borrow.on_account.as_deref(), Some("Loan"));

        // deposit has `on Pool` but no `who`
        let deposit = &spec.handlers[1];
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
    // sBPF (Dropset) tests — use inline old-syntax spec for backward compat
    // ========================================================================

    const DROPSET_SPEC: &str = r#"
spec Dropset

target assembly
assembly "src/dropset.s"

const DISC_REGISTER_MARKET     = 0
const ACCT_NON_DUP_MARKER      = 255
const DATA_LEN_ZERO             = 0
const SIZE_OF_EMPTY_ACCOUNT     = 10_336
const SIZE_OF_MARKET_HEADER     = 40
const SIZE_OF_ADDRESS           = 32
const SIZE_OF_CREATE_ACCOUNT    = 56

pubkey RENT [
  5_862_609_301_215_225_606,
  9_219_231_539_345_853_473,
  4_971_307_250_928_769_624,
  2_329_533_411
]

errors [
  InvalidDiscriminant         = 1   "Discriminant is not REGISTER_MARKET",
  InvalidInstructionLength    = 2   "Instruction data is not 1 byte",
  InvalidNumberOfAccounts     = 3   "Fewer than 10 accounts provided",
  UserHasData                 = 4   "User account already has data",
  MarketAccountIsDuplicate    = 5   "Market account is a duplicate",
  MarketHasData               = 6   "Market account already has data",
  BaseMintIsDuplicate         = 7   "Base mint account is a duplicate",
  QuoteMintIsDuplicate        = 8   "Quote mint account is a duplicate",
  InvalidMarketPubkey         = 9   "Market pubkey does not match derived PDA",
  SystemProgramIsDuplicate    = 10  "System Program account is a duplicate",
  InvalidSystemProgramPubkey  = 11  "System Program pubkey is wrong",
  RentSysvarIsDuplicate       = 12  "Rent sysvar account is a duplicate",
  InvalidRentSysvarPubkey     = 13  "Rent sysvar pubkey is wrong"
]

/// Validates accounts, derives market PDA, creates market account via CPI
instruction RegisterMarket {
  discriminant DISC_REGISTER_MARKET
  entry 24

  const ACCOUNTS_REQUIRED    = 10
  const INSTRUCTION_DATA_LEN = 1

  input_layout {
    n_accounts       : U64    @ 0       "Number of accounts in input buffer"
    user_data_len    : U64    @ 88      "Data length of user account"
    market_dup       : U8     @ 10344   "Market account duplicate flag"
    market_data_len  : U64    @ 10424   "Market account data length"
    market_pubkey    : Pubkey @ 10352   "Market account address (4 chunks)"
    base_mint_dup    : U8     @ 20680   "Base mint duplicate flag"
    base_data_len    : U64    @ 20760   "Base mint data length"
  }

  insn_layout {
    insn_len         : U64    @ -8      "Instruction data length"
    discriminant     : U8     @ 0       "Instruction discriminant byte"
  }

  /// Instruction byte must be REGISTER_MARKET
  guard rejects_invalid_discriminant {
    checks discriminant == DISC_REGISTER_MARKET
    error InvalidDiscriminant
    fuel 8
  }
  guard rejects_invalid_account_count {
    checks n_accounts >= ACCOUNTS_REQUIRED
    error InvalidNumberOfAccounts
    fuel 10
  }
  guard rejects_invalid_instruction_length {
    checks insn_len == INSTRUCTION_DATA_LEN
    error InvalidInstructionLength
    fuel 12
  }
  guard rejects_user_has_data {
    checks user_data_len == DATA_LEN_ZERO
    error UserHasData
    fuel 14
  }
  guard rejects_market_duplicate {
    checks market_dup == ACCT_NON_DUP_MARKER
    error MarketAccountIsDuplicate
    fuel 16
  }
  guard rejects_market_has_data {
    checks market_data_len == DATA_LEN_ZERO
    error MarketHasData
    fuel 18
  }
  guard rejects_base_mint_duplicate {
    checks base_mint_dup == ACCT_NON_DUP_MARKER
    error BaseMintIsDuplicate
    fuel 20
  }
  guard rejects_quote_mint_duplicate {
    error QuoteMintIsDuplicate
    fuel 30
  }
  guard rejects_invalid_market_pubkey {
    checks market_pubkey == derived_pda
    error InvalidMarketPubkey
    fuel 61
  }
  guard rejects_system_program_duplicate {
    error SystemProgramIsDuplicate
    fuel 74
  }
  guard rejects_invalid_system_program_pubkey {
    error InvalidSystemProgramPubkey
    fuel 86
  }
  guard rejects_rent_sysvar_duplicate {
    error RentSysvarIsDuplicate
    fuel 96
  }
  guard rejects_invalid_rent_sysvar_pubkey {
    checks rent_pubkey == RENT
    error InvalidRentSysvarPubkey
    fuel 108
  }

  property memory_safety {
    scope guards
  }
  property pda_derivation {
    flow market_pda from seeds [base_mint_addr, quote_mint_addr]
  }
  property account_pointer_flow {
    flow r9 through [market, system_program, rent_sysvar]
  }
  property cpi_create_account {
    cpi system_program CreateAccount {
      payer        user
      target       market_pda
      space        SIZE_OF_MARKET_HEADER
      signer_seeds [base_mint_addr, quote_mint_addr, bump]
    }
  }
  property accepts_valid_input {
    after all guards
    exit 0
  }
}
"#;

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

    // ========================================================================
    // v2.0 feature parsing tests
    // ========================================================================

    const PERCOLATOR_SPEC: &str =
        include_str!("../../../examples/rust/percolator/percolator.qedspec");

    #[test]
    fn parse_requires_from_percolator() {
        let spec = parse(PERCOLATOR_SPEC).unwrap();
        let withdraw = spec.handlers.iter().find(|h| h.name == "withdraw").unwrap();
        assert_eq!(withdraw.requires.len(), 1);
        assert_eq!(
            withdraw.requires[0].error_name,
            Some("InsufficientFunds".to_string())
        );
        assert!(withdraw.requires[0].rust_expr.contains("C_tot"));
    }

    #[test]
    fn parse_requires_multiple() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        let create = spec
            .handlers
            .iter()
            .find(|h| h.name == "create_vault")
            .unwrap();
        // Two requires with error names
        let with_errors: Vec<_> = create
            .requires
            .iter()
            .filter(|r| r.error_name.is_some())
            .collect();
        assert_eq!(with_errors.len(), 2);
        assert_eq!(
            with_errors[0].error_name,
            Some("InvalidThreshold".to_string())
        );
        assert_eq!(
            with_errors[1].error_name,
            Some("TooManyMembers".to_string())
        );
    }

    #[test]
    fn parse_cover_blocks() {
        let spec = parse(PERCOLATOR_SPEC).unwrap();
        assert!(!spec.covers.is_empty());
        let happy = spec.covers.iter().find(|c| c.name == "happy_path").unwrap();
        assert_eq!(happy.traces[0], vec!["deposit", "withdraw"]);
    }

    #[test]
    fn parse_cover_multi_trace() {
        let spec = parse(MULTISIG_SPEC).unwrap();
        assert_eq!(spec.covers.len(), 2);
        let lifecycle = spec
            .covers
            .iter()
            .find(|c| c.name == "proposal_lifecycle")
            .unwrap();
        assert_eq!(
            lifecycle.traces[0],
            vec!["create_vault", "propose", "approve", "execute"]
        );
        let cancel = spec
            .covers
            .iter()
            .find(|c| c.name == "cancel_flow")
            .unwrap();
        assert_eq!(
            cancel.traces[0],
            vec!["create_vault", "propose", "cancel_proposal"]
        );
    }

    #[test]
    fn parse_liveness_block() {
        let spec = parse(PERCOLATOR_SPEC).unwrap();
        assert_eq!(spec.liveness_props.len(), 1);
        let lv = &spec.liveness_props[0];
        assert_eq!(lv.name, "drain_completes");
        assert_eq!(lv.from_state, "Draining");
        assert_eq!(lv.leads_to_state, "Active");
        assert_eq!(lv.via_ops, vec!["complete_drain", "reset"]);
        assert_eq!(lv.within_steps, Some(2));
    }

    #[test]
    fn parse_liveness_multi_account() {
        let spec = parse(LENDING_SPEC).unwrap();
        assert_eq!(spec.liveness_props.len(), 1);
        let lv = &spec.liveness_props[0];
        assert_eq!(lv.name, "loan_settles");
        assert_eq!(lv.from_state, "Active");
        assert_eq!(lv.leads_to_state, "Empty");
        assert_eq!(lv.via_ops, vec!["repay"]);
        assert_eq!(lv.within_steps, Some(1));
    }

    #[test]
    fn parse_environment_block() {
        let spec = parse(LENDING_SPEC).unwrap();
        assert_eq!(spec.environments.len(), 1);
        let env = &spec.environments[0];
        assert_eq!(env.name, "interest_rate_change");
        assert_eq!(
            env.mutates,
            vec![("interest_rate".to_string(), "U64".to_string())]
        );
        assert!(!env.constraints.is_empty());
    }

    #[test]
    fn parse_escrow_cover_and_liveness() {
        let escrow_spec = include_str!("../../../examples/rust/escrow/escrow.qedspec");
        let spec = parse(escrow_spec).unwrap();

        // Cover blocks
        assert_eq!(spec.covers.len(), 2);
        let happy = spec.covers.iter().find(|c| c.name == "happy_path").unwrap();
        assert_eq!(happy.traces[0], vec!["initialize", "exchange"]);
        let cancel = spec
            .covers
            .iter()
            .find(|c| c.name == "cancel_path")
            .unwrap();
        assert_eq!(cancel.traces[0], vec!["initialize", "cancel"]);

        // Liveness
        assert_eq!(spec.liveness_props.len(), 1);
        assert_eq!(spec.liveness_props[0].from_state, "Open");
        assert_eq!(spec.liveness_props[0].leads_to_state, "Closed");

        // requires on initialize
        let init = spec
            .handlers
            .iter()
            .find(|h| h.name == "initialize")
            .unwrap();
        let with_errors: Vec<_> = init
            .requires
            .iter()
            .filter(|r| r.error_name.is_some())
            .collect();
        assert_eq!(with_errors.len(), 1);
        assert_eq!(with_errors[0].error_name, Some("InvalidAmount".to_string()));
    }

    // ========================================================================
    // Phase 1 v2 tests: not, implies, *, /, %, requires
    // ========================================================================

    #[test]
    fn parse_not_expr() {
        let spec_str = r#"
spec Test
state { active : Bool }
lifecycle [Off, On]
handler toggle {
  when On
  requires not (state.active == 0)
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        let req = &handler.requires[0];
        assert!(
            req.lean_expr.contains("\u{00AC}"),
            "should contain ¬: {}",
            req.lean_expr
        );
        assert!(
            req.lean_expr.contains("s.active"),
            "should reference s.active: {}",
            req.lean_expr
        );
    }

    #[test]
    fn parse_implies_expr() {
        let spec_str = r#"
spec Test
state { balance : U64 }
property positive_implies_nonzero {
  expr state.balance > 0 implies state.balance >= 1
  preserved_by all
}
handler noop {}
"#;
        let spec = parse(spec_str).unwrap();
        let prop = &spec.properties[0];
        let expr = prop.expression.as_ref().unwrap();
        assert!(
            expr.contains("\u{2192}"),
            "should contain → for implies: {}",
            expr
        );
    }

    #[test]
    fn parse_mul_div_mod_expr() {
        let spec_str = r#"
spec Test
state { fee : U64 }
handler charge {
  requires state.fee * 100 / 10000 >= 1
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        let req = &handler.requires[0];
        assert!(
            req.lean_expr.contains("*"),
            "should contain *: {}",
            req.lean_expr
        );
        assert!(
            req.lean_expr.contains("/"),
            "should contain /: {}",
            req.lean_expr
        );
    }

    #[test]
    fn parse_requires_clause_with_error() {
        let spec_str = r#"
spec Test
state { balance : U64 }
errors [InsufficientBalance]
handler withdraw {
  requires state.balance > 0 else InsufficientBalance
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert_eq!(handler.requires.len(), 1);
        let req = &handler.requires[0];
        assert!(req.lean_expr.contains("s.balance > 0"));
        assert_eq!(req.error_name, Some("InsufficientBalance".to_string()));
    }

    #[test]
    fn parse_requires_clause_without_error() {
        let spec_str = r#"
spec Test
state { count : U64 }
handler increment {
  requires state.count > 0
  effect { count += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert_eq!(handler.requires.len(), 1);
        let req = &handler.requires[0];
        assert!(req.lean_expr.contains("s.count > 0"));
        assert_eq!(req.error_name, None);
    }

    #[test]
    fn parse_requires_replaces_guard_and_aborts_if() {
        // Verify that requires with else is equivalent to guard + aborts_if
        let spec_str = r#"
spec Test
state { amount : U64 }
errors [InvalidAmount]
handler deposit {
  requires state.amount > 0 and state.amount <= 1000 else InvalidAmount
  effect { amount += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert_eq!(handler.requires.len(), 1);
        let req = &handler.requires[0];
        // Positive form should have ∧
        assert!(
            req.lean_expr.contains("\u{2227}"),
            "lean: {}",
            req.lean_expr
        );
        // Rust form should have &&
        assert!(req.rust_expr.contains("&&"), "rust: {}", req.rust_expr);
        assert_eq!(req.error_name, Some("InvalidAmount".to_string()));
    }

    #[test]
    fn parse_ensures_clause() {
        let spec_str = r#"
spec Test
state { balance : U64  fee : U64 }
handler deposit {
  takes { amount : U64 }
  ensures state.balance == old(state.balance) + amount
  effect { balance += amount }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert_eq!(handler.ensures.len(), 1);
        let ens = &handler.ensures[0];
        // In ensures context: state.balance → s'.balance, old(state.balance) → s.balance
        assert!(
            ens.lean_expr.contains("s'.balance"),
            "lean: {}",
            ens.lean_expr
        );
        assert!(
            ens.lean_expr.contains("s.balance"),
            "lean should have pre-state: {}",
            ens.lean_expr
        );
        assert!(
            !ens.lean_expr.contains("old"),
            "should not contain raw 'old': {}",
            ens.lean_expr
        );
        // Rust form: state.balance → new_state.balance, old(state.balance) → old_state.field
        assert!(
            ens.rust_expr.contains("new_state.balance"),
            "rust: {}",
            ens.rust_expr
        );
        assert!(
            ens.rust_expr.contains("old_state.balance"),
            "rust: {}",
            ens.rust_expr
        );
    }

    #[test]
    fn parse_ensures_multiple() {
        let spec_str = r#"
spec Test
state { x : U64  y : U64 }
handler update {
  ensures state.x > old(state.x)
  ensures state.y == old(state.y)
  effect { x += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert_eq!(handler.ensures.len(), 2);
        // First ensures: s'.x > s.x
        assert!(handler.ensures[0].lean_expr.contains("s'.x"));
        assert!(handler.ensures[0].lean_expr.contains("s.x"));
        // Second ensures: s'.y == s.y (frame-like)
        assert!(handler.ensures[1].lean_expr.contains("s'.y"));
        assert!(handler.ensures[1].lean_expr.contains("s.y"));
    }

    #[test]
    fn parse_modifies_clause() {
        let spec_str = r#"
spec Test
state { balance : U64  fee : U64  owner : Pubkey }
handler deposit {
  modifies [balance]
  effect { balance += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert!(handler.modifies.is_some());
        let mods = handler.modifies.as_ref().unwrap();
        assert_eq!(mods, &["balance"]);
    }

    #[test]
    fn parse_modifies_multiple_fields() {
        let spec_str = r#"
spec Test
state { x : U64  y : U64  z : U64 }
handler swap {
  modifies [x, y]
  effect { x += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        let mods = handler.modifies.as_ref().unwrap();
        assert_eq!(mods, &["x", "y"]);
    }

    #[test]
    fn parse_let_binding() {
        let spec_str = r#"
spec Test
state { total : U64  used : U64 }
handler allocate {
  takes { amount : U64 }
  let available = state.total - state.used
  requires available >= amount else InsufficientSpace
  effect { used += amount }
}
"#;
        let spec = parse(spec_str).unwrap();
        let handler = &spec.handlers[0];
        assert_eq!(handler.let_bindings.len(), 1);
        let (name, lean, rust) = &handler.let_bindings[0];
        assert_eq!(name, "available");
        // In guard context: state.total → s.total
        assert!(lean.contains("s.total"), "lean: {}", lean);
        assert!(lean.contains("s.used"), "lean: {}", lean);
        // Rust keeps state.field
        assert!(rust.contains("state.total"), "rust: {}", rust);
        assert!(rust.contains("state.used"), "rust: {}", rust);
    }

    #[test]
    fn parse_handler_no_modifies_is_none() {
        // Handlers without modifies should have modifies == None
        let spec_str = r#"
spec Test
state { x : U64 }
handler noop {
  effect { x += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        assert!(spec.handlers[0].modifies.is_none());
    }

    #[test]
    fn parse_old_in_ensures_renders_correctly() {
        // Detailed check of old() rendering in both Lean and Rust
        let spec_str = r#"
spec Test
state { count : U64 }
handler increment {
  ensures state.count == old(state.count) + 1
  effect { count += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let ens = &spec.handlers[0].ensures[0];
        // Lean: s'.count = s.count + 1 (propositional equality for theorem goals)
        assert_eq!(ens.lean_expr, "s'.count = s.count + 1");
        // Rust: new_state.count == old_state.count + 1
        assert_eq!(ens.rust_expr, "new_state.count == old_state.count + 1");
    }

    #[test]
    fn parse_all_phase2_constructs_together() {
        // Integration test: handler with let + requires + ensures + modifies
        let spec_str = r#"
spec Test
state { balance : U64  total_fees : U64 }
const MAX_BALANCE = 1000000
errors [Overflow, InvalidAmount]

handler deposit {
  takes { amount : U64 }
  let new_balance = state.balance + amount
  requires amount > 0 else InvalidAmount
  requires new_balance <= MAX_BALANCE else Overflow
  modifies [balance]
  effect { balance += amount }
  ensures state.balance == old(state.balance) + amount
}
"#;
        let spec = parse(spec_str).unwrap();
        let h = &spec.handlers[0];

        // Let bindings
        assert_eq!(h.let_bindings.len(), 1);
        assert_eq!(h.let_bindings[0].0, "new_balance");

        // Requires
        assert_eq!(h.requires.len(), 2);
        assert_eq!(h.requires[0].error_name, Some("InvalidAmount".to_string()));
        assert_eq!(h.requires[1].error_name, Some("Overflow".to_string()));
        // MAX_BALANCE should be expanded
        assert!(
            h.requires[1].lean_expr.contains("1000000"),
            "const expansion: {}",
            h.requires[1].lean_expr
        );

        // Modifies
        assert_eq!(h.modifies.as_ref().unwrap(), &["balance"]);

        // Ensures
        assert_eq!(h.ensures.len(), 1);
        assert!(h.ensures[0].lean_expr.contains("s'.balance"));
        assert!(h.ensures[0].lean_expr.contains("s.balance"));

        // Effects still work
        assert_eq!(h.effects.len(), 1);
    }

    // ========================================================================
    // Phase 3 tests: schemas, include, quantifiers, aborts_total
    // ========================================================================

    #[test]
    fn parse_schema_include_basic() {
        let spec_str = r#"
spec Test
state { balance : U64 }
errors [Unauthorized]

schema authorized {
  who owner
  requires signer == state.balance else Unauthorized
}

handler deposit {
  include authorized
  takes { amount : U64 }
  effect { balance += amount }
}
"#;
        let spec = parse(spec_str).unwrap();
        let h = &spec.handlers[0];
        // Schema's `who` should be merged
        assert_eq!(h.who, Some("owner".to_string()));
        // Schema's `requires` should be merged
        assert_eq!(h.requires.len(), 1);
        assert_eq!(h.requires[0].error_name, Some("Unauthorized".to_string()));
    }

    #[test]
    fn parse_schema_handler_override() {
        // Handler's own values take precedence over schema defaults
        let spec_str = r#"
spec Test
state { balance : U64 }

schema base {
  who creator
}

handler deposit {
  include base
  who admin
  effect { balance += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        let h = &spec.handlers[0];
        // Handler's `who admin` overrides schema's `who creator`
        assert_eq!(h.who, Some("admin".to_string()));
    }

    #[test]
    fn parse_schema_collection_merge() {
        // Schema's collection items are prepended to handler's
        let spec_str = r#"
spec Test
state { balance : U64 }
errors [Unauthorized, InvalidAmount]

schema guarded {
  requires state.balance > 0 else Unauthorized
}

handler withdraw {
  include guarded
  takes { amount : U64 }
  requires amount > 0 else InvalidAmount
  effect { balance -= amount }
}
"#;
        let spec = parse(spec_str).unwrap();
        let h = &spec.handlers[0];
        // Schema's requires comes first, handler's second
        assert_eq!(h.requires.len(), 2);
        assert_eq!(h.requires[0].error_name, Some("Unauthorized".to_string()));
        assert_eq!(h.requires[1].error_name, Some("InvalidAmount".to_string()));
    }

    #[test]
    fn parse_aborts_total_flag() {
        let spec_str = r#"
spec Test
state { balance : U64 }
errors [InvalidAmount, Overflow]

handler deposit {
  takes { amount : U64 }
  requires amount > 0 else InvalidAmount
  requires state.balance + amount <= 1000000 else Overflow
  aborts_total
  effect { balance += amount }
}
"#;
        let spec = parse(spec_str).unwrap();
        let h = &spec.handlers[0];
        assert!(h.aborts_total);
        assert_eq!(h.requires.len(), 2);
    }

    #[test]
    fn parse_aborts_total_default_false() {
        let spec_str = r#"
spec Test
state { x : U64 }
handler noop {
  effect { x += 1 }
}
"#;
        let spec = parse(spec_str).unwrap();
        assert!(!spec.handlers[0].aborts_total);
    }

    #[test]
    fn parse_quantifier_forall() {
        let spec_str = r#"
spec Test
state { count : U64 }
property all_positive {
  expr forall i : Nat, i < state.count implies state.count > 0
  preserved_by all
}
"#;
        let spec = parse(spec_str).unwrap();
        let prop = &spec.properties[0];
        // Lean form should contain ∀
        assert!(
            prop.expression.as_ref().unwrap().contains("\u{2200}"),
            "lean: {}",
            prop.expression.as_ref().unwrap()
        );
        assert!(
            prop.expression.as_ref().unwrap().contains("Nat"),
            "lean: {}",
            prop.expression.as_ref().unwrap()
        );
        // Should contain → for implies
        assert!(
            prop.expression.as_ref().unwrap().contains("\u{2192}"),
            "lean: {}",
            prop.expression.as_ref().unwrap()
        );
    }

    #[test]
    fn parse_quantifier_exists() {
        let spec_str = r#"
spec Test
state { count : U64 }
property some_active {
  expr exists i : U64, i < state.count
  preserved_by all
}
"#;
        let spec = parse(spec_str).unwrap();
        let prop = &spec.properties[0];
        // Lean form should contain ∃ and Nat (U64 → Nat)
        assert!(
            prop.expression.as_ref().unwrap().contains("\u{2203}"),
            "lean: {}",
            prop.expression.as_ref().unwrap()
        );
        assert!(
            prop.expression.as_ref().unwrap().contains("Nat"),
            "lean: {}",
            prop.expression.as_ref().unwrap()
        );
    }

    #[test]
    fn parse_all_phase3_constructs_together() {
        let spec_str = r#"
spec Test
state { balance : U64  owner : Pubkey }
lifecycle [Uninitialized, Active]
errors [Unauthorized, InvalidAmount, Overflow]

schema authorized {
  who owner
  requires signer == state.owner else Unauthorized
}

handler initialize {
  when Uninitialized
  then Active
  takes { initial_balance : U64 }
  requires initial_balance > 0 else InvalidAmount
  requires initial_balance <= 1000000 else Overflow
  aborts_total
  modifies [balance, owner]
  effect {
    balance = initial_balance
    owner = signer
  }
  ensures state.balance == initial_balance
}

handler deposit {
  include authorized
  when Active
  takes { amount : U64 }
  requires amount > 0 else InvalidAmount
  modifies [balance]
  effect { balance += amount }
  ensures state.balance == old(state.balance) + amount
}

property balance_bounded {
  expr forall amt : Nat, amt <= state.balance implies state.balance > 0
  preserved_by all
}
"#;
        let spec = parse(spec_str).unwrap();

        // Initialize handler has aborts_total
        let init = &spec.handlers[0];
        assert_eq!(init.name, "initialize");
        assert!(init.aborts_total);
        assert_eq!(init.requires.len(), 2);
        assert_eq!(init.ensures.len(), 1);

        // Deposit handler has schema-merged who + requires
        let deposit = &spec.handlers[1];
        assert_eq!(deposit.name, "deposit");
        assert_eq!(deposit.who, Some("owner".to_string()));
        // Schema requires + handler requires = 2
        assert_eq!(deposit.requires.len(), 2);
        assert_eq!(
            deposit.requires[0].error_name,
            Some("Unauthorized".to_string())
        );
        assert_eq!(
            deposit.requires[1].error_name,
            Some("InvalidAmount".to_string())
        );
        assert!(!deposit.aborts_total);

        // Property with quantifier
        let prop = &spec.properties[0];
        assert!(prop.expression.as_ref().unwrap().contains("\u{2200}"));
    }
}
