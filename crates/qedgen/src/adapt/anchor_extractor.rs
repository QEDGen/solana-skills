//! Anchor proto-clause extractor (v2.19 M3.1).
//!
//! Walks an Anchor program's source tree, surfaces the canonical
//! escape-hatch patterns the auditor SKILL.md documents, and lifts each
//! into `ProtoClause`s the runtime-agnostic clustering algorithm
//! consumes. Same output shape as the Pinocchio extractor; downstream
//! ratification pipeline doesn't care which runtime contributed.
//!
//! Patterns detected (v1):
//!
//! 1. **AccountInfo / UncheckedAccount on Accounts struct fields** →
//!    `AccountTypeTagCheck`. Anchor's typed wrappers (`Account<T>`,
//!    `Signer`, `Program<T>`, `Sysvar<T>`) close the type-tag question
//!    by construction; `AccountInfo<'info>` / `UncheckedAccount<'info>`
//!    are explicit opt-outs.
//!
//! 2. **`#[account(seeds = [...])]` without `bump`** →
//!    `PdaCanonicalDerivation`. Missing `bump` keyword signals the
//!    seeds aren't constrained to the canonical PDA derivation.
//!
//! 3. **Raw arithmetic on u64/u128 in handler bodies** →
//!    `ArithmeticNoOverflow`. `+ -` `* / %` outside `checked_*` /
//!    `saturating_*` / `wrapping_*` family.
//!
//! 4. **`init_if_needed` constraint** → `LifecycleOneShot`.
//!    `init_if_needed` opts out of Anchor's default "fresh account
//!    only" init invariant; without an explicit discriminator guard
//!    this is replay-vulnerable.
//!
//! Out of scope for v1 (defer to v3 polish):
//! - close-account redirection (`close = receiver`)
//! - oracle staleness
//! - account-not-reloaded-after-cpi
//! - transfer-hook reentrancy
//!
//! These categories are well-documented in
//! `skills/qedgen-auditor/SKILL.md` and the auditor subagent applies
//! them via Read+Grep — the extractor doesn't need to duplicate the
//! coverage in v1.

use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};

use crate::cluster::{ClusterKind, ProtoClause};

/// Entry point — walks the project root and emits proto-clauses.
pub fn extract_proto_clauses(project_root: &Path) -> Result<Vec<ProtoClause>> {
    let rs_files = collect_rust_files(project_root)?;
    let mut out = Vec::new();
    let pat = AnchorPatterns::new();

    // Pass 1: per-Accounts-struct fingerprinting. Determine which
    // handler each Accounts struct serves (matches Context<X> in
    // handler signatures) so the resulting proto-clauses are
    // handler-scoped.
    let accounts_structs = scan_accounts_structs(&rs_files, &pat);
    let handler_context_map = scan_handler_context_map(&rs_files, &pat);

    for acc in &accounts_structs {
        // Map Accounts struct name → handler name via Context<X>
        // signatures. Fall back to the struct name if no handler is
        // found (common when the handler is defined in a separate file
        // we haven't scanned).
        let handler = handler_context_map
            .iter()
            .find(|(_, ctx)| ctx == &acc.name)
            .map(|(h, _)| h.clone())
            .unwrap_or_else(|| acc.name.to_lowercase());

        for ai in &acc.account_info_fields {
            out.push(ProtoClause {
                kind: ClusterKind::AccountTypeTagCheck,
                handler: handler.clone(),
                finding_id: format!(
                    "anchor:{}:{}:type_confusion:{}",
                    acc.file.display(),
                    acc.line,
                    ai
                ),
                evidence_text: format!(
                    "Accounts struct `{}` field `{}: AccountInfo<'info>` — opts out of Anchor's typed-account validation.",
                    acc.name, ai
                ),
            });
        }

        for pda in &acc.pda_without_bump {
            out.push(ProtoClause {
                kind: ClusterKind::PdaCanonicalDerivation,
                handler: handler.clone(),
                finding_id: format!(
                    "anchor:{}:{}:no_bump:{}",
                    acc.file.display(),
                    acc.line,
                    pda
                ),
                evidence_text: format!(
                    "Accounts struct `{}` has `#[account(seeds = ...)]` on `{}` with no `bump` constraint — non-canonical derivation possible.",
                    acc.name, pda
                ),
            });
        }

        if acc.has_init_if_needed {
            out.push(ProtoClause {
                kind: ClusterKind::LifecycleOneShot,
                handler: handler.clone(),
                finding_id: format!(
                    "anchor:{}:{}:init_if_needed:{}",
                    acc.file.display(),
                    acc.line,
                    acc.name
                ),
                evidence_text: format!(
                    "Accounts struct `{}` uses `init_if_needed` — opts out of Anchor's fresh-account-only init guard.",
                    acc.name
                ),
            });
        }
    }

    // Pass 2: raw arithmetic in handler bodies. We've already
    // identified the handler set via `handler_context_map`; for each
    // file containing a handler, scan for un-checked arithmetic
    // patterns.
    let handler_set: std::collections::BTreeSet<String> =
        handler_context_map.iter().map(|(h, _)| h.clone()).collect();
    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let arith_sites = scan_arith_sites(&source, &pat);
        if arith_sites.is_empty() {
            continue;
        }
        // Best-effort: attribute the site to the most-recently-seen
        // handler fn. For lib.rs-style multi-handler files this is
        // sometimes imprecise; v3 polish refines using the
        // syn::visit::Visit infrastructure.
        let attributed = attribute_arith_to_handlers(&source, &arith_sites, &handler_set, &pat);
        for (handler, line) in attributed {
            out.push(ProtoClause {
                kind: ClusterKind::ArithmeticNoOverflow,
                handler,
                finding_id: format!("anchor:{}:{}:raw_arith", file.display(), line),
                evidence_text: format!(
                    "Raw arithmetic on numeric field at {}:{} — no checked_* / saturating_* / wrapping_* wrapper.",
                    file.display(),
                    line
                ),
            });
        }
    }

    Ok(out)
}

// ── Pattern compilers ─────────────────────────────────────────────────

struct AnchorPatterns {
    accounts_derive: Regex,
    field_account_info: Regex,
    seeds_attr: Regex,
    bump_keyword: Regex,
    init_if_needed: Regex,
    handler_signature: Regex,
    fn_signature: Regex,
    checked_call: Regex,
}

impl AnchorPatterns {
    fn new() -> Self {
        Self {
            // `#[derive(Accounts)]` followed by `pub struct Name<'info>`
            // optionally with whitespace/comments between.
            accounts_derive: Regex::new(
                r"#\[derive\(([^\)]*\bAccounts\b[^\)]*)\)\]\s*(?:pub\s+)?struct\s+(\w+)",
            )
            .unwrap(),
            // Match `pub field: AccountInfo<'info>` and `pub field: UncheckedAccount<'info>`
            // (with various lifetimes).
            field_account_info: Regex::new(
                r"(?m)^\s*(?:pub\s+)?(\w+)\s*:\s*(?:AccountInfo|UncheckedAccount)\b",
            )
            .unwrap(),
            seeds_attr: Regex::new(r"#\[account\([^\)]*\bseeds\s*=").unwrap(),
            bump_keyword: Regex::new(r"\bbump\b").unwrap(),
            init_if_needed: Regex::new(r"\binit_if_needed\b").unwrap(),
            // Handler signatures: `pub fn name(ctx: Context<X>, ...) -> ...`
            handler_signature: Regex::new(
                r"(?m)^\s*pub\s+fn\s+(\w+)\s*\(\s*(?:mut\s+)?ctx\s*:\s*Context\s*<\s*(\w+)\s*>",
            )
            .unwrap(),
            fn_signature: Regex::new(r"(?m)^\s*(?:pub\s+)?fn\s+(\w+)\s*\(").unwrap(),
            checked_call: Regex::new(
                r"\b(?:checked|saturating|wrapping|overflowing)_(?:add|sub|mul|div|rem)\b",
            )
            .unwrap(),
        }
    }
}

// ── Scanners ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct AccountsStruct {
    name: String,
    file: PathBuf,
    line: usize,
    account_info_fields: Vec<String>,
    pda_without_bump: Vec<String>,
    has_init_if_needed: bool,
}

fn scan_accounts_structs(rs_files: &[PathBuf], pat: &AnchorPatterns) -> Vec<AccountsStruct> {
    let mut out = Vec::new();
    for file in rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        // Walk every `#[derive(... Accounts ...)] struct Name` block.
        // We find the brace-delimited struct body by scanning forward
        // from the struct declaration. Crude but adequate.
        for caps in pat.accounts_derive.captures_iter(&source) {
            let name = caps.get(2).unwrap().as_str().to_string();
            let start = caps.get(0).unwrap().start();
            let line = source[..start].matches('\n').count() + 1;
            // Find the body: from `{` after the struct line to the
            // matching `}`. Simple brace-balanced walk; nesting inside
            // `#[account(...)]` attributes doesn't use `{}` so this
            // works for typical Anchor sources.
            let after = caps.get(0).unwrap().end();
            let Some(open_brace) = source[after..].find('{') else {
                continue;
            };
            let body_start = after + open_brace + 1;
            let body_end = match_brace(&source, body_start);
            let body = &source[body_start..body_end];

            let mut account_info_fields = Vec::new();
            for fcap in pat.field_account_info.captures_iter(body) {
                let field = fcap.get(1).unwrap().as_str().to_string();
                account_info_fields.push(field);
            }

            // PDAs without `bump`: split body into `#[account(...)]` segments,
            // check each for `seeds =` without `bump`.
            let mut pda_without_bump = Vec::new();
            for attr_block in extract_account_attr_blocks(body) {
                if pat.seeds_attr.is_match(&attr_block.contents)
                    && !pat.bump_keyword.is_match(&attr_block.contents)
                {
                    pda_without_bump
                        .push(attr_block.field_name.unwrap_or_else(|| "<unknown>".into()));
                }
            }

            let has_init_if_needed = pat.init_if_needed.is_match(body);

            out.push(AccountsStruct {
                name,
                file: file.clone(),
                line,
                account_info_fields,
                pda_without_bump,
                has_init_if_needed,
            });
        }
    }
    out
}

/// Find balanced closing brace for an open brace at `open_pos`.
fn match_brace(source: &str, open_pos: usize) -> usize {
    let bytes = source.as_bytes();
    let mut depth = 1;
    let mut i = open_pos;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    source.len()
}

#[derive(Debug)]
struct AttrBlock {
    contents: String,
    field_name: Option<String>,
}

/// Extract `#[account(...)]` attribute blocks paired with the field
/// they decorate (the next field declaration after the attribute).
fn extract_account_attr_blocks(body: &str) -> Vec<AttrBlock> {
    let mut out = Vec::new();
    let attr_re = Regex::new(r"#\[account\(").unwrap();
    let field_re = Regex::new(r"(?m)^\s*(?:pub\s+)?(\w+)\s*:").unwrap();
    for m in attr_re.find_iter(body) {
        // Find the matching closing paren.
        let open = m.end() - 1;
        let close = match_paren(body, open + 1);
        if close >= body.len() {
            continue;
        }
        let contents = body[m.start()..=close].to_string();
        // The decorated field is the next `name : Type` after the attr's `]`.
        let after = body[close..]
            .find(']')
            .map(|i| close + i + 1)
            .unwrap_or(close);
        let field_name = field_re
            .captures(&body[after..])
            .map(|c| c.get(1).unwrap().as_str().to_string());
        out.push(AttrBlock {
            contents,
            field_name,
        });
    }
    out
}

fn match_paren(source: &str, open_pos: usize) -> usize {
    let bytes = source.as_bytes();
    let mut depth = 1;
    let mut i = open_pos;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    source.len()
}

/// Build `handler_name -> Context<X> type` map from `pub fn` declarations.
fn scan_handler_context_map(rs_files: &[PathBuf], pat: &AnchorPatterns) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for file in rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        for caps in pat.handler_signature.captures_iter(&source) {
            let handler = caps.get(1).unwrap().as_str().to_string();
            let ctx_type = caps.get(2).unwrap().as_str().to_string();
            out.push((handler, ctx_type));
        }
    }
    out
}

/// Find lines containing raw arithmetic that isn't part of a
/// `checked_*`/`saturating_*`/`wrapping_*`/`overflowing_*` call. Returns
/// 1-based line numbers.
///
/// Tracks per-line `#[` attribute depth via byte-level scanning so
/// arithmetic inside Anchor macros (`#[account(space = 8 + N)]`) is
/// excluded — those compute account layout at macro-expansion time,
/// not at runtime.
fn scan_arith_sites(source: &str, pat: &AnchorPatterns) -> Vec<usize> {
    let inside_attr = compute_inside_attr_lines(source);
    let mut out = Vec::new();
    for (i, line) in source.lines().enumerate() {
        if inside_attr[i] {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }
        if pat.checked_call.is_match(line) {
            continue;
        }
        if is_anchor_attr_field(line) {
            continue;
        }
        if !contains_assignment(line) {
            continue;
        }
        let after_eq = line.split_once('=').map(|(_, r)| r).unwrap_or("");
        if has_arith_operator(after_eq) {
            out.push(i + 1);
        }
    }
    out
}

/// Walk bytes and compute, per line index, whether the line falls
/// inside an open `#[ ... ]` attribute span. A line is "inside" if at
/// the start of the line the attribute-stack depth is > 0, OR if it
/// contains an `#[` that opens an attribute on this line. Multi-line
/// `#[account(...)]` blocks are correctly tracked.
fn compute_inside_attr_lines(source: &str) -> Vec<bool> {
    let total_lines = source.lines().count();
    let mut out = vec![false; total_lines];
    let bytes = source.as_bytes();
    let mut line_idx = 0usize;
    let mut depth = 0i32;
    let mut line_started_in_attr = depth > 0;
    let mut line_saw_open = false;

    let mut i = 0;
    while i < bytes.len() {
        // Newline finalizes the current line.
        if bytes[i] == b'\n' {
            if line_idx < out.len() {
                out[line_idx] = line_started_in_attr || line_saw_open;
            }
            line_idx += 1;
            line_started_in_attr = depth > 0;
            line_saw_open = false;
            i += 1;
            continue;
        }
        // Look for `#[` opening.
        if bytes[i] == b'#' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            depth += 1;
            line_saw_open = true;
            i += 2;
            continue;
        }
        // Closing `]` only counts when inside an open attribute.
        if bytes[i] == b']' && depth > 0 {
            depth -= 1;
            i += 1;
            continue;
        }
        i += 1;
    }
    // Final line (no trailing newline).
    if line_idx < out.len() {
        out[line_idx] = line_started_in_attr || line_saw_open;
    }
    out
}

/// Recognize Anchor attribute-field assignments (`space = N`,
/// `payer = X`, `seeds = [...]`, `bump = X`, `address = X`,
/// `constraint = X`, `has_one = X`, `init`, `init_if_needed`, `mut`,
/// `close = X`). Any line containing these keywords-with-equals at the
/// start of the line is treated as macro syntax, not handler code.
fn is_anchor_attr_field(line: &str) -> bool {
    let trimmed = line.trim_start();
    const ANCHOR_FIELDS: &[&str] = &[
        "space",
        "payer",
        "seeds",
        "bump",
        "address",
        "constraint",
        "has_one",
        "close",
        "owner",
        "mint",
        "associated_token",
        "token",
        "executable",
        "realloc",
    ];
    for f in ANCHOR_FIELDS {
        let prefix = format!("{} =", f);
        if trimmed.starts_with(&prefix) || trimmed.starts_with(&format!("{}=", f)) {
            return true;
        }
    }
    false
}

fn contains_assignment(line: &str) -> bool {
    // True for `x = y`, `x += y`, etc. False for `x == y` or `if a < b`.
    // We detect by looking for `=` that isn't preceded by `<>!=` and
    // isn't part of `==`.
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b' '
            };
            // Skip `==`, `!=`, `<=`, `>=`. Accept `=`, `+=`, `-=`, `*=`, `/=`.
            if next == b'=' || matches!(prev, b'!' | b'<' | b'>' | b'=') {
                continue;
            }
            return true;
        }
    }
    false
}

fn has_arith_operator(s: &str) -> bool {
    // Look for `+`, `-`, `*`, `/` outside of `+=`/`-=`/`*=`/`/=` and `//` comments.
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b'+' | b'-' | b'*' | b'/') {
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b' '
            };
            if next == b'=' {
                continue;
            }
            if b == b'/' && next == b'/' {
                // Comment marker.
                return false;
            }
            // Skip `->` (return arrow).
            if b == b'-' && next == b'>' {
                continue;
            }
            return true;
        }
    }
    false
}

/// Attribute arithmetic-site line numbers to the most recently-declared
/// fn name in that file. Imprecise but adequate when handlers are
/// physically separated.
fn attribute_arith_to_handlers(
    source: &str,
    sites: &[usize],
    handler_set: &std::collections::BTreeSet<String>,
    pat: &AnchorPatterns,
) -> Vec<(String, usize)> {
    // Pre-compute (fn_name, line_number) pairs in source order.
    let mut fn_lines: Vec<(String, usize)> = Vec::new();
    for caps in pat.fn_signature.captures_iter(source) {
        let line = source[..caps.get(0).unwrap().start()].matches('\n').count() + 1;
        fn_lines.push((caps.get(1).unwrap().as_str().to_string(), line));
    }

    let mut out = Vec::new();
    for &site_line in sites {
        // Find the most recent fn declaration at or before site_line.
        let attribution = fn_lines
            .iter()
            .rfind(|(_, l)| *l <= site_line)
            .map(|(name, _)| name.clone());
        if let Some(name) = attribution {
            // Only attribute to handlers we know about (filter out
            // helpers like `pub fn handler` inside instructions/*.rs
            // forwarders).
            if handler_set.contains(&name) || name == "handler" {
                // If the immediate enclosing fn is `handler`, walk back
                // to the nearest module-level handler — the brownfield
                // demo pattern is `instructions::initialize::handler`
                // forwarded from `pub fn initialize`. We approximate by
                // mapping `handler` to the first handler in
                // handler_set whose name appears in the file path or
                // surrounding context.
                let attributed_name = if name == "handler" {
                    // Look for the file name as a handler hint.
                    handler_set
                        .iter()
                        .find(|h| source.contains(&format!("instructions::{}::", h)))
                        .cloned()
                        .unwrap_or(name)
                } else {
                    name
                };
                out.push((attributed_name, site_line));
            }
        }
    }
    out
}

// ── File walker ───────────────────────────────────────────────────────

fn collect_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            matches!(
                n,
                "target" | ".git" | "node_modules" | "tests" | "fuzz" | "migrations"
            )
        }) {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn detects_account_info_field_as_type_tag_check() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use anchor_lang::prelude::*;

#[program]
pub mod x {
    use super::*;
    pub fn do_stuff(ctx: Context<DoStuff>) -> Result<()> { Ok(()) }
}

#[derive(Accounts)]
pub struct DoStuff<'info> {
    pub typed: Account<'info, MyData>,
    /// CHECK: skipped
    pub raw: AccountInfo<'info>,
    pub unchecked: UncheckedAccount<'info>,
}

#[account]
pub struct MyData { pub x: u64 }
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let typed_clauses: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::AccountTypeTagCheck)
            .collect();
        assert_eq!(
            typed_clauses.len(),
            2,
            "expected 2 AccountTypeTagCheck (raw + unchecked), got {:?}",
            typed_clauses
        );
        for p in &typed_clauses {
            assert_eq!(p.handler, "do_stuff");
        }
        Ok(())
    }

    #[test]
    fn detects_seeds_without_bump_as_pda_canonical() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use anchor_lang::prelude::*;

#[program]
pub mod x {
    pub fn make_pda(ctx: Context<MakePda>) -> Result<()> { Ok(()) }
}

#[derive(Accounts)]
pub struct MakePda<'info> {
    #[account(init, payer = owner, space = 8, seeds = [b"vault", owner.key().as_ref()])]
    pub vault: Account<'info, Vault>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[account]
pub struct Vault {}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let pda: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::PdaCanonicalDerivation)
            .collect();
        assert_eq!(
            pda.len(),
            1,
            "expected 1 PdaCanonicalDerivation, got {:?}",
            pda
        );
        Ok(())
    }

    #[test]
    fn seeds_with_bump_is_not_flagged() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use anchor_lang::prelude::*;
#[program]
pub mod x {
    pub fn make_pda(ctx: Context<MakePda>) -> Result<()> { Ok(()) }
}
#[derive(Accounts)]
pub struct MakePda<'info> {
    #[account(seeds = [b"vault"], bump)]
    pub vault: Account<'info, Vault>,
}
#[account]
pub struct Vault {}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let pda: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::PdaCanonicalDerivation)
            .collect();
        assert!(
            pda.is_empty(),
            "bump present should suppress PdaCanonicalDerivation; got {:?}",
            pda
        );
        Ok(())
    }

    #[test]
    fn detects_init_if_needed_as_lifecycle_one_shot() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use anchor_lang::prelude::*;
#[program] pub mod x {
    pub fn make_or_use(ctx: Context<MakeOrUse>) -> Result<()> { Ok(()) }
}
#[derive(Accounts)]
pub struct MakeOrUse<'info> {
    #[account(init_if_needed, payer = owner, space = 8)]
    pub state: Account<'info, S>,
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}
#[account] pub struct S {}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let lc: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::LifecycleOneShot)
            .collect();
        assert_eq!(lc.len(), 1, "expected 1 LifecycleOneShot, got {:?}", lc);
        Ok(())
    }

    #[test]
    fn detects_raw_arithmetic_in_handler_body() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use anchor_lang::prelude::*;
#[program] pub mod x {
    pub fn increment(ctx: Context<Increment>, delta: u64) -> Result<()> {
        let counter = &mut ctx.accounts.counter;
        counter.value = counter.value + delta;
        Ok(())
    }
    pub fn safe_inc(ctx: Context<Increment>, delta: u64) -> Result<()> {
        let counter = &mut ctx.accounts.counter;
        counter.value = counter.value.checked_add(delta).unwrap();
        Ok(())
    }
}
#[derive(Accounts)]
pub struct Increment<'info> {
    #[account(mut)] pub counter: Account<'info, C>,
}
#[account] pub struct C { pub value: u64 }
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let arith: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::ArithmeticNoOverflow)
            .collect();
        assert!(
            arith.iter().any(|p| p.handler == "increment"),
            "expected an ArithmeticNoOverflow clause attributed to `increment`. Got: {:?}",
            arith
        );
        // `safe_inc` uses checked_add and should NOT trigger.
        assert!(
            !arith.iter().any(|p| p.handler == "safe_inc"),
            "safe_inc uses checked_add — should not trigger an ArithmeticNoOverflow clause. Got: {:?}",
            arith
        );
        Ok(())
    }

    #[test]
    fn empty_project_yields_no_clauses() -> Result<()> {
        let dir = tempdir()?;
        let protos = extract_proto_clauses(dir.path())?;
        assert!(protos.is_empty());
        Ok(())
    }
}
