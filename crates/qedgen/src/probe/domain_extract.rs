//! Conservative, source-derived hints for the auditor's domain dossier.
//!
//! This scanner deliberately reports syntax, not program semantics.  A call
//! named `transfer`, for example, is evidence that an asset flow may exist;
//! it is not evidence that the transfer is authorized, balanced, or even
//! reachable.  The auditor presents these hints to the user for ratification.

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainSourceFacts {
    pub schema_version: u32,
    pub asset_flows: Vec<AssetFlowHint>,
    pub quantities: Vec<QuantityHint>,
    pub paired_operations: Vec<PairedOperationHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSpan {
    pub path: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetFlowHint {
    pub id: String,
    pub operation: String,
    pub flow_shape: FlowShape,
    pub handler: Option<String>,
    pub expression: String,
    pub quantity_references: Vec<String>,
    pub source_span: SourceSpan,
    pub confidence: HintConfidence,
    pub claim_limit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowShape {
    BetweenAccounts,
    Issuance,
    Destruction,
    AuthorityChange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuantityHint {
    pub id: String,
    pub identifier: String,
    pub rust_type: String,
    pub unit_hint: UnitHint,
    pub handler: Option<String>,
    pub source_span: SourceSpan,
    pub confidence: HintConfidence,
    pub claim_limit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnitHint {
    TokenAmount,
    Lamports,
    BasisPoints,
    Shares,
    PriceOrRate,
    UnixTime,
    SlotOrEpoch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedOperationHint {
    pub id: String,
    pub relationship: String,
    pub left_operation: String,
    pub right_operation: String,
    pub left_handlers: Vec<String>,
    pub right_handlers: Vec<String>,
    pub evidence: Vec<SourceSpan>,
    pub confidence: HintConfidence,
    pub claim_limit: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HintConfidence {
    High,
    Medium,
}

/// Scan all Rust source beneath `<program_root>/src`.
pub fn extract_program(program_root: &Path) -> Result<DomainSourceFacts> {
    let src = program_root.join("src");
    if !src.exists() {
        return Ok(DomainSourceFacts::empty());
    }
    let files = crate::fs_walk::collect_rs_files(&src, crate::fs_walk::DEFAULT_SKIP_DIRS);
    let mut combined = DomainSourceFacts::empty();
    let mut handlers = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let relative = file.strip_prefix(program_root).unwrap_or(&file);
        combined.extend(extract_source(relative, &source));
        handlers.extend(handler_declarations(relative, &source));
    }
    combined.paired_operations = pairs_from_handlers(&handlers);
    combined.finish();
    Ok(combined)
}

/// Scan one Rust source file. Exposed separately for focused tests and for
/// callers that already own an in-memory source snapshot.
pub fn extract_source(path: &Path, source: &str) -> DomainSourceFacts {
    let mut facts = DomainSourceFacts::empty();
    facts.asset_flows = extract_asset_flows(path, source);
    facts.quantities = extract_quantities(path, source);
    facts.paired_operations = extract_pairs(path, source);
    facts.finish();
    facts
}

impl DomainSourceFacts {
    fn empty() -> Self {
        Self {
            schema_version: 1,
            asset_flows: Vec::new(),
            quantities: Vec::new(),
            paired_operations: Vec::new(),
        }
    }

    fn extend(&mut self, other: Self) {
        self.asset_flows.extend(other.asset_flows);
        self.quantities.extend(other.quantities);
        self.paired_operations.extend(other.paired_operations);
    }

    fn finish(&mut self) {
        self.asset_flows.sort_by(|a, b| a.id.cmp(&b.id));
        self.asset_flows.dedup_by(|a, b| a.id == b.id);
        self.quantities.sort_by(|a, b| a.id.cmp(&b.id));
        self.quantities.dedup_by(|a, b| a.id == b.id);

        // Pairs found independently in separate files need to be merged.
        let mut merged: BTreeMap<String, PairedOperationHint> = BTreeMap::new();
        for pair in self.paired_operations.drain(..) {
            let entry = merged
                .entry(pair.id.clone())
                .or_insert_with(|| pair.clone());
            entry.left_handlers.extend(pair.left_handlers);
            entry.right_handlers.extend(pair.right_handlers);
            entry.evidence.extend(pair.evidence);
            entry.left_handlers.sort();
            entry.left_handlers.dedup();
            entry.right_handlers.sort();
            entry.right_handlers.dedup();
            entry.evidence.sort_by(|a, b| {
                (&a.path, a.start_line, a.start_column).cmp(&(
                    &b.path,
                    b.start_line,
                    b.start_column,
                ))
            });
            entry.evidence.dedup();
        }
        self.paired_operations = merged.into_values().collect();
    }
}

fn extract_asset_flows(path: &Path, source: &str) -> Vec<AssetFlowHint> {
    let call =
        Regex::new(r"(?m)\b(?P<op>transfer_checked|transfer|mint_to|burn|approve|revoke)\s*\(")
            .expect("static regex");
    let mut out = Vec::new();
    let mut covered_until = 0;
    for captures in call.captures_iter(source) {
        let matched = captures.get(0).expect("full match");
        // Context-builder methods can repeat the operation inside the outer
        // CPI call (`revoke(ctx.accounts.revoke())`). Keep one source fact.
        if matched.start() < covered_until {
            continue;
        }
        if line_is_commented(source, matched.start())
            || is_function_declaration(source, matched.start())
        {
            continue;
        }
        let operation = captures.name("op").expect("operation").as_str();
        let end = call_expression_end(source, matched.start()).unwrap_or(matched.end());
        covered_until = end;
        let expression = compact_excerpt(&source[matched.start()..end.min(source.len())], 240);
        let quantity_references = quantity_names(&expression);
        let flow_shape = match operation {
            "mint_to" => FlowShape::Issuance,
            "burn" => FlowShape::Destruction,
            "approve" | "revoke" => FlowShape::AuthorityChange,
            _ => FlowShape::BetweenAccounts,
        };
        let span = source_span(path, source, matched.start(), end);
        out.push(AssetFlowHint {
            id: stable_id(&format!(
                "flow:{}:{}:{}:{}",
                path.display(),
                span.start_line,
                span.start_column,
                operation
            )),
            operation: operation.to_string(),
            flow_shape,
            handler: enclosing_fn_name(source, matched.start()),
            expression,
            quantity_references,
            source_span: span,
            confidence: HintConfidence::High,
            claim_limit: "A matching call exists; account roles, reachability, authorization, and conservation remain unverified.".to_string(),
        });
    }
    out
}

fn extract_quantities(path: &Path, source: &str) -> Vec<QuantityHint> {
    let declaration = Regex::new(
        r"(?m)\b(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*:\s*(?P<type>[ui](?:8|16|32|64|128|size))\b",
    )
    .expect("static regex");
    let mut out = Vec::new();
    for captures in declaration.captures_iter(source) {
        let matched = captures.get(0).expect("full match");
        if line_is_commented(source, matched.start()) {
            continue;
        }
        let identifier = captures.name("name").expect("name").as_str();
        let Some(unit_hint) = infer_unit(identifier) else {
            continue;
        };
        let span = source_span(path, source, matched.start(), matched.end());
        out.push(QuantityHint {
            id: stable_id(&format!(
                "quantity:{}:{}:{}:{}",
                path.display(),
                span.start_line,
                span.start_column,
                identifier
            )),
            identifier: identifier.to_string(),
            rust_type: captures.name("type").expect("type").as_str().to_string(),
            unit_hint,
            handler: enclosing_fn_name(source, matched.start()),
            source_span: span,
            confidence: HintConfidence::Medium,
            claim_limit: "The unit is inferred from the identifier only; scale, denomination, rounding, and conversion rules require user ratification.".to_string(),
        });
    }
    out
}

fn extract_pairs(path: &Path, source: &str) -> Vec<PairedOperationHint> {
    pairs_from_handlers(&handler_declarations(path, source))
}

#[derive(Debug, Clone)]
struct HandlerDeclaration {
    name: String,
    span: SourceSpan,
}

fn handler_declarations(path: &Path, source: &str) -> Vec<HandlerDeclaration> {
    let function =
        Regex::new(r"(?m)\bfn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*[<(]").expect("static regex");
    function
        .captures_iter(source)
        .filter_map(|captures| {
            let matched = captures.get(0)?;
            if line_is_commented(source, matched.start()) {
                return None;
            }
            Some(HandlerDeclaration {
                name: captures.name("name")?.as_str().to_string(),
                span: source_span(path, source, matched.start(), matched.end()),
            })
        })
        .collect()
}

fn pairs_from_handlers(handlers: &[HandlerDeclaration]) -> Vec<PairedOperationHint> {
    const PAIRS: &[(&str, &str)] = &[
        ("deposit", "withdraw"),
        ("mint", "burn"),
        ("lock", "unlock"),
        ("initialize", "close"),
        ("init", "close"),
        ("approve", "revoke"),
        ("stake", "unstake"),
        ("freeze", "thaw"),
    ];
    let mut out = Vec::new();
    for &(left, right) in PAIRS {
        let left_matches: Vec<_> = handlers
            .iter()
            .filter(|handler| handler_has_verb(&handler.name, left))
            .collect();
        let right_matches: Vec<_> = handlers
            .iter()
            .filter(|handler| handler_has_verb(&handler.name, right))
            .collect();
        if left_matches.is_empty() || right_matches.is_empty() {
            continue;
        }
        let mut evidence = Vec::new();
        evidence.extend(left_matches.iter().map(|handler| handler.span.clone()));
        evidence.extend(right_matches.iter().map(|handler| handler.span.clone()));
        out.push(PairedOperationHint {
            id: stable_id(&format!("pair:{left}:{right}")),
            relationship: "candidate_inverse".to_string(),
            left_operation: left.to_string(),
            right_operation: right.to_string(),
            left_handlers: left_matches
                .iter()
                .map(|handler| handler.name.clone())
                .collect(),
            right_handlers: right_matches
                .iter()
                .map(|handler| handler.name.clone())
                .collect(),
            evidence,
            confidence: HintConfidence::Medium,
            claim_limit: "Handler names suggest an inverse pair; reversibility, symmetry, and shared units remain unverified.".to_string(),
        });
    }
    out
}

fn infer_unit(identifier: &str) -> Option<UnitHint> {
    let name = identifier.to_ascii_lowercase();
    if name.contains("lamport") {
        Some(UnitHint::Lamports)
    } else if name.contains("bps") || name.contains("basis_points") {
        Some(UnitHint::BasisPoints)
    } else if name.contains("share") {
        Some(UnitHint::Shares)
    } else if name.contains("price") || name.contains("rate") || name.contains("ratio") {
        Some(UnitHint::PriceOrRate)
    } else if name.contains("timestamp") || name.ends_with("_ts") || name.contains("unix_time") {
        Some(UnitHint::UnixTime)
    } else if name.contains("slot") || name.contains("epoch") {
        Some(UnitHint::SlotOrEpoch)
    } else if name.contains("amount")
        || name.contains("balance")
        || name.contains("supply")
        || name.contains("liquidity")
        || name.contains("fee")
    {
        Some(UnitHint::TokenAmount)
    } else {
        None
    }
}

fn quantity_names(expression: &str) -> Vec<String> {
    let words = Regex::new(r"\b[A-Za-z_][A-Za-z0-9_]*\b").expect("static regex");
    let mut names = BTreeSet::new();
    for matched in words.find_iter(expression) {
        if infer_unit(matched.as_str()).is_some() {
            names.insert(matched.as_str().to_string());
        }
    }
    names.into_iter().collect()
}

fn handler_has_verb(handler: &str, verb: &str) -> bool {
    handler == verb
        || handler.starts_with(&format!("{verb}_"))
        || handler.ends_with(&format!("_{verb}"))
        || handler.contains(&format!("_{verb}_"))
}

fn enclosing_fn_name(source: &str, offset: usize) -> Option<String> {
    let head = &source[..offset.min(source.len())];
    let function = Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]").expect("static regex");
    function
        .captures_iter(head)
        .last()
        .map(|captures| captures[1].to_string())
}

fn call_expression_end(source: &str, start: usize) -> Option<usize> {
    let open = source[start..].find('(')? + start;
    let mut depth = 0_u32;
    for (relative, ch) in source[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + relative + ch.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

fn source_span(path: &Path, source: &str, start: usize, end: usize) -> SourceSpan {
    let (start_line, start_column) = line_column(source, start);
    let (end_line, end_column) = line_column(source, end);
    SourceSpan {
        path: path.display().to_string(),
        start_line,
        start_column,
        end_line,
        end_column,
    }
}

fn line_column(source: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    let line = 1 + prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = 1 + source[line_start..offset].chars().count() as u32;
    (line, column)
}

fn compact_excerpt(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        compact
    } else {
        compact.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn line_is_commented(source: &str, offset: usize) -> bool {
    let line_start = source[..offset.min(source.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    source[line_start..offset.min(source.len())].contains("//")
}

fn is_function_declaration(source: &str, offset: usize) -> bool {
    let line_start = source[..offset.min(source.len())]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    source[line_start..offset.min(source.len())]
        .trim_end()
        .ends_with("fn")
}

fn stable_id(material: &str) -> String {
    let digest = Sha256::digest(material.as_bytes());
    let hex = format!("{:x}", digest);
    format!("src_{}", &hex[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
pub fn deposit(ctx: Context<Deposit>, token_amount: u64, fee_bps: u16) -> Result<()> {
    transfer(ctx.accounts.into_transfer(), token_amount)?;
    Ok(())
}

pub fn withdraw(ctx: Context<Withdraw>, share_amount: u64) -> Result<()> {
    token::transfer_checked(ctx.accounts.into_transfer(), share_amount, 6)?;
    Ok(())
}

pub fn issue(ctx: Context<Issue>, supply_amount: u64) -> Result<()> {
    mint_to(ctx.accounts.into_mint(), supply_amount)?;
    Ok(())
}
"#;

    #[test]
    fn extracts_flows_quantities_pairs_and_spans() {
        let facts = extract_source(Path::new("src/lib.rs"), SOURCE);
        assert_eq!(facts.asset_flows.len(), 3);
        assert_eq!(facts.asset_flows[0].source_span.path, "src/lib.rs");
        assert!(facts
            .asset_flows
            .iter()
            .any(|flow| flow.operation == "transfer_checked"
                && flow.handler.as_deref() == Some("withdraw")
                && flow.quantity_references == ["share_amount"]));
        assert!(facts
            .quantities
            .iter()
            .any(|quantity| quantity.identifier == "fee_bps"
                && quantity.unit_hint == UnitHint::BasisPoints));
        let pair = facts
            .paired_operations
            .iter()
            .find(|pair| pair.left_operation == "deposit")
            .expect("deposit/withdraw pair");
        assert_eq!(pair.right_handlers, ["withdraw"]);
        assert!(pair.evidence.iter().all(|span| span.start_line > 0));
    }

    #[test]
    fn does_not_claim_untyped_or_unrecognized_quantities() {
        let facts = extract_source(
            Path::new("src/lib.rs"),
            "fn update(ctx: Context<X>, count: u64, memo: String) {}",
        );
        assert!(facts.quantities.is_empty());
        assert!(facts.asset_flows.is_empty());
        assert!(facts.paired_operations.is_empty());
    }

    #[test]
    fn ignores_line_comments_and_marks_authority_operations() {
        let source = r#"
fn permissions(delegate_amount: u64) {
    // approve(delegate_amount);
    revoke(ctx.accounts.revoke());
}
"#;
        let facts = extract_source(Path::new("src/lib.rs"), source);
        assert_eq!(facts.asset_flows.len(), 1);
        assert_eq!(facts.asset_flows[0].operation, "revoke");
        assert_eq!(facts.asset_flows[0].flow_shape, FlowShape::AuthorityChange);
    }

    #[test]
    fn output_order_and_ids_are_stable() {
        let first = extract_source(Path::new("src/lib.rs"), SOURCE);
        let second = extract_source(Path::new("src/lib.rs"), SOURCE);
        assert_eq!(first, second);
        assert!(first
            .asset_flows
            .iter()
            .all(|flow| flow.id.len() == 20 && flow.id.starts_with("src_")));
    }

    #[test]
    fn pairs_handlers_declared_in_different_source_files() {
        let root = tempfile::tempdir().expect("temp project");
        let src = root.path().join("src");
        std::fs::create_dir(&src).expect("src directory");
        std::fs::write(
            src.join("deposit.rs"),
            "pub fn deposit(token_amount: u64) {}",
        )
        .expect("deposit source");
        std::fs::write(
            src.join("withdraw.rs"),
            "pub fn withdraw(share_amount: u64) {}",
        )
        .expect("withdraw source");

        let facts = extract_program(root.path()).expect("program facts");
        let pair = facts
            .paired_operations
            .iter()
            .find(|pair| pair.left_operation == "deposit")
            .expect("cross-file pair");
        assert_eq!(pair.left_handlers, ["deposit"]);
        assert_eq!(pair.right_handlers, ["withdraw"]);
        assert_eq!(pair.evidence.len(), 2);
    }
}
