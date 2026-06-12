//! `qedgen probe --runtime pinocchio` — Pinocchio-aware audit data layer.
//!
//! Walks every `.rs` under the project's `src/` (or `program/src/`),
//! enumerates `unsafe`-serde and arithmetic sites that Pinocchio
//! programs hand-write in place of Anchor's framework checks, parses
//! adjacent `// SAFETY:` comments, and emits the structured catalogue
//! the audit subagent consumes via rust-analyzer.
//!
//! Per `feedback_agent_lsp_substrate`: the enumerator is **deterministic
//! pattern matching only**. Control-flow analysis (does the SAFETY claim
//! hold on every reachable path?) is the agent's job. We emit sites
//! plus their author-written preconditions; the agent reads the impl and
//! decides.
//!
//! Site kinds enumerated (10 total):
//!
//! 1. `BorrowUnchecked` — `*.borrow_*_unchecked*()`
//! 2. `BytemuckCall` — `bytemuck::(from|try_from|cast)*<T>`
//! 3. `RawPtrCastFromAccount` — raw `as *const _` / `transmute` on account data
//! 4. `CustomLoadCall` — `load*` fn inside `unsafe { }` with a `borrow_*_unchecked` first arg
//! 5. `TryIntoUnwrapOnSlice` — `_[..].try_into().unwrap()`
//! 6. `SetLamportsArith` — `set_lamports(...)` / `*lamports {+/-}= _`
//! 7. `SetAmountArith` — `set_amount(amount() {+/-} _)`
//! 8. `IndexedAccountAccess` — `accounts[N]` literal
//! 9. `IndexedDataSlice` — `data[CONST..CONST{+/-}N]`
//! 10. `SafetyComment` — `// SAFETY:` blocks attached to the next `unsafe { }` scope
//!
//! Output: `PinocchioCatalogue { sites, summary }`. Consumers (the audit
//! subagent and `references/probes/pinocchio/*.md`) map sites → findings
//! via per-site predicates the agent applies.

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One detected site. `extra` carries site-kind-specific structured
/// data (load-type `T`, offset constant, etc.) that probe markdowns
/// use as `${...}` substitutions in the reproducer template.
#[derive(Debug, Clone, Serialize)]
pub struct PinocchioSite {
    pub kind: SiteKind,
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    /// The matched expression / call text, trimmed.
    pub expr: String,
    /// Containing fn name (best-effort — the most recent `fn` declaration
    /// seen above the site, ignoring nested closures).
    pub fn_name: Option<String>,
    /// True when the site lives inside an `unsafe { }` block scope. The
    /// scope detector is a brace-depth heuristic, not a full parser.
    pub in_unsafe_block: bool,
    /// Parsed `// SAFETY: ...` runs from the lines immediately above
    /// the enclosing `unsafe { }`. Concatenated into a single string
    /// for the agent to read verbatim.
    pub safety_comment: Option<String>,
    /// Kind-specific structured fields. Schema is documented in
    /// `references/probes/pinocchio/<probe>.md#substitutions`.
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteKind {
    BorrowUnchecked,
    BytemuckCall,
    RawPtrCastFromAccount,
    CustomLoadCall,
    TryIntoUnwrapOnSlice,
    SetLamportsArith,
    SetAmountArith,
    IndexedAccountAccess,
    IndexedDataSlice,
    SafetyComment,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PinocchioSummary {
    pub files_scanned: usize,
    pub sites_total: usize,
    pub safety_comments_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PinocchioCatalogue {
    pub schema_version: u32,
    pub project_root: PathBuf,
    pub sites: Vec<PinocchioSite>,
    pub summary: PinocchioSummary,
}

const SCHEMA_VERSION: u32 = 1;

/// Walk the project root and emit the full site catalogue. Reads every
/// `*.rs` under `src/`, `program/src/`, and `programs/*/src/`.
pub fn scan_program(project_root: &Path) -> Result<PinocchioCatalogue> {
    let rs_files = collect_rust_files(project_root)?;
    let mut sites = Vec::new();
    let mut files_scanned = 0;
    let mut safety_total = 0;

    let patterns = SitePatterns::new();

    for file in &rs_files {
        files_scanned += 1;
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel_file = file
            .strip_prefix(project_root)
            .unwrap_or(file)
            .to_path_buf();
        let mut file_sites = scan_file(&rel_file, &source, &patterns);
        for s in &file_sites {
            if matches!(s.kind, SiteKind::SafetyComment) {
                safety_total += 1;
            }
        }
        sites.append(&mut file_sites);
    }

    let summary = PinocchioSummary {
        files_scanned,
        sites_total: sites.len(),
        safety_comments_total: safety_total,
    };

    Ok(PinocchioCatalogue {
        schema_version: SCHEMA_VERSION,
        project_root: project_root.to_path_buf(),
        sites,
        summary,
    })
}

/// Recursively collect `.rs` files under conventional Solana program
/// source roots. Stops at `target/`, `.qed/`, and `node_modules/` to
/// avoid scanning build artifacts.
fn collect_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let candidates = ["src", "program/src", "programs"];
    for c in candidates {
        let dir = root.join(c);
        if !dir.exists() {
            continue;
        }
        walk_dir(&dir, &mut out)?;
    }
    // Fallback: also scan project_root for top-level .rs if nothing
    // matched (some Pinocchio fixtures put source straight under root).
    if out.is_empty() {
        walk_dir(root, &mut out)?;
    }
    Ok(out)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(
            name,
            "target" | ".qed" | "node_modules" | ".git" | "formal_verification"
        ) {
            continue;
        }
        if path.is_dir() {
            walk_dir(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

struct SitePatterns {
    borrow_unchecked: Regex,
    bytemuck_call: Regex,
    raw_ptr_cast: Regex,
    transmute_account: Regex,
    custom_load: Regex,
    try_into_unwrap: Regex,
    set_lamports_arith_call: Regex,
    set_lamports_arith_deref: Regex,
    set_amount_arith: Regex,
    indexed_account: Regex,
    indexed_data_slice: Regex,
    fn_decl: Regex,
}

impl SitePatterns {
    fn new() -> Self {
        // Each regex is intentionally permissive: false positives are
        // cheaper than misses because the agent re-reads context via
        // rust-analyzer before reporting. The patterns target what's
        // SYNTACTICALLY unsafe; the agent decides what's SEMANTICALLY a
        // bug.
        SitePatterns {
            borrow_unchecked: Regex::new(r"\.borrow_[a-z_]*unchecked[a-z_]*\s*\(\s*\)")
                .expect("regex"),
            bytemuck_call: Regex::new(
                r"bytemuck::\s*(?:from_bytes|try_from_bytes|cast|cast_ref|cast_slice|cast_slice_mut|from_bytes_mut|try_from_bytes_mut)\b",
            )
            .expect("regex"),
            raw_ptr_cast: Regex::new(r"\bas\s+\*(?:const|mut)\s+[A-Za-z_]").expect("regex"),
            transmute_account: Regex::new(r"\btransmute\b").expect("regex"),
            // `unsafe { load*::<T>(...)? }` or similar. We catch any
            // identifier-shaped fn name containing `load` whose first
            // arg references a `borrow_*_unchecked` call.
            custom_load: Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*load[A-Za-z0-9_]*)\s*(?:::\s*<[^>]+>\s*)?\(")
                .expect("regex"),
            try_into_unwrap: Regex::new(r"\.try_into\s*\(\s*\)\s*\.\s*unwrap\s*\(\s*\)")
                .expect("regex"),
            // p-token `set_lamports(self.lamports() + amount)` / similar.
            set_lamports_arith_call: Regex::new(
                r"\bset_lamports\s*\(\s*[^)]*?(?:[a-zA-Z_][a-zA-Z0-9_]*\.)?lamports\s*\(\s*\)\s*[+\-]\s*",
            )
            .expect("regex"),
            // `*source_lamports -= amount` shape.
            set_lamports_arith_deref: Regex::new(
                r"\*\s*[a-zA-Z_][a-zA-Z0-9_]*lamports[a-zA-Z0-9_]*\s*[+\-]=\s*",
            )
            .expect("regex"),
            // `set_amount(amount() + delta)` / similar.
            set_amount_arith: Regex::new(
                r"\bset_amount\s*\(\s*[^)]*?(?:[a-zA-Z_][a-zA-Z0-9_]*\.)?amount\s*\(\s*\)\s*[+\-]\s*",
            )
            .expect("regex"),
            indexed_account: Regex::new(r"\baccounts\s*\[\s*(\d+)\s*\]").expect("regex"),
            // `data[OFFSET..OFFSET+N]` or `data[A..B]` with const-y
            // looking identifiers.
            indexed_data_slice: Regex::new(
                r"\b(?:data|raw|buf|bytes)\s*\[\s*([A-Z0-9_]+)\s*\.\.\s*([A-Z0-9_+\- ]+)\s*\]",
            )
            .expect("regex"),
            // Note: tolerate optional `pub`, `async`, `unsafe`, and
            // `extern "C"` modifiers; the abi string is matched
            // permissively to avoid hitting raw-string quote-escape
            // issues.
            fn_decl: Regex::new(
                r#"^\s*(?:pub\s+(?:\([^)]*\)\s+)?)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+(?:"[^"]+"\s+)?)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"#,
            )
            .expect("regex"),
        }
    }
}

fn scan_file(rel_file: &Path, source: &str, p: &SitePatterns) -> Vec<PinocchioSite> {
    let mut sites = Vec::new();
    let source = strip_cfg_kani_regions(source);
    let lines: Vec<&str> = source.lines().collect();
    let unsafe_ranges = compute_unsafe_block_ranges(&lines);
    let safety_blocks = parse_safety_comments(&lines);

    // Track containing fn — most recent `fn NAME` declaration whose
    // brace depth still encloses the current line. Brace-depth
    // heuristic is good enough; we are not building an AST.
    let mut fn_stack: Vec<(String, usize)> = Vec::new();
    let mut depth_at_line_end: Vec<i32> = Vec::with_capacity(lines.len());
    let mut depth = 0_i32;

    for line in &lines {
        // Strip line comments but preserve strings as best-effort —
        // close enough for brace counting in source we control.
        let stripped = strip_line_comment(line);
        for ch in stripped.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        depth_at_line_end.push(depth);
    }

    for (idx, raw_line) in lines.iter().enumerate() {
        let line_no = (idx + 1) as u32;
        let stripped = strip_line_comment(raw_line);

        // Update fn_stack — pop frames whose depth is no longer active.
        let current_depth = depth_at_line_end.get(idx).copied().unwrap_or(0);
        while let Some((_, start_depth)) = fn_stack.last() {
            if current_depth <= *start_depth as i32 {
                fn_stack.pop();
            } else {
                break;
            }
        }
        // Push fn declared on this line, if any.
        if let Some(m) = p.fn_decl.captures(raw_line) {
            // depth_before_line: depth_at_line_end[idx] minus brace
            // events on this line.
            let mut brace_before = 0_i32;
            for ch in stripped.chars() {
                match ch {
                    '{' => brace_before += 1,
                    '}' => brace_before -= 1,
                    _ => {}
                }
            }
            let depth_before = current_depth - brace_before;
            fn_stack.push((m[1].to_string(), depth_before.max(0) as usize));
        }

        let fn_name = fn_stack.last().map(|(n, _)| n.clone());
        let in_unsafe = unsafe_ranges
            .iter()
            .any(|(s, e)| line_no >= *s && line_no <= *e);
        let safety = safety_for_line(&safety_blocks, line_no);

        // 1. BorrowUnchecked
        for m in p.borrow_unchecked.find_iter(&stripped) {
            sites.push(PinocchioSite {
                kind: SiteKind::BorrowUnchecked,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped[m.range()].to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({
                    "callee": stripped[m.range()].trim_start_matches('.').trim_end_matches("()"),
                }),
            });
        }

        // 2. BytemuckCall
        for m in p.bytemuck_call.find_iter(&stripped) {
            sites.push(PinocchioSite {
                kind: SiteKind::BytemuckCall,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped[m.range()].to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({
                    "call": stripped[m.range()],
                }),
            });
        }

        // 3. RawPtrCast / Transmute
        for m in p.raw_ptr_cast.find_iter(&stripped) {
            // Filter out trivial `as *const u8` casts of non-account
            // exprs by requiring the LHS to mention `.data` /
            // `.borrow` / `account` — keeps the signal high.
            let lhs_start = stripped[..m.start()].rfind([';', '{', '(', ',']);
            let lhs = match lhs_start {
                Some(p) => &stripped[p + 1..m.start()],
                None => &stripped[..m.start()],
            };
            let lhs_lc = lhs.to_lowercase();
            if !(lhs_lc.contains("data")
                || lhs_lc.contains("borrow")
                || lhs_lc.contains("account")
                || lhs_lc.contains("input"))
            {
                continue;
            }
            sites.push(PinocchioSite {
                kind: SiteKind::RawPtrCastFromAccount,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped.trim().to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({
                    "ptr_kind": "cast",
                }),
            });
        }
        for m in p.transmute_account.find_iter(&stripped) {
            // Skip `use core::mem::{transmute, ...};` imports — they
            // match the bare `transmute` token but aren't call sites.
            let trimmed = stripped.trim_start();
            if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
                continue;
            }
            // Same LHS-shape guard as the `raw_ptr_cast` branch above:
            // a real transmute on account data references `data` /
            // `borrow` / `account` / `input` in its surrounding
            // expression. Filters out non-data transmutes (size_of
            // arithmetic, type conversions on local arrays).
            let context_start = stripped[..m.start()].rfind([';', '{', '(', ',']);
            let context = match context_start {
                Some(p) => &stripped[p + 1..],
                None => &stripped[..],
            };
            let context_lc = context.to_lowercase();
            if !(context_lc.contains("data")
                || context_lc.contains("borrow")
                || context_lc.contains("account")
                || context_lc.contains("input"))
            {
                continue;
            }
            sites.push(PinocchioSite {
                kind: SiteKind::RawPtrCastFromAccount,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped.trim().to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({
                    "ptr_kind": "transmute",
                }),
            });
        }

        // 4. CustomLoadCall — only when in_unsafe AND a borrow_*_unchecked
        // sibling expression appears on this line or the previous line.
        if in_unsafe {
            for cap in p.custom_load.captures_iter(&stripped) {
                let callee = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                // Skip obvious noise — `payload`, `download`, etc. The
                // strict signal is a fn name containing `load` followed
                // by `(` whose first arg expression references
                // `borrow_*_unchecked`.
                if !callee.starts_with("load")
                    && callee != "loader"
                    && !callee.ends_with("_load")
                    && !callee.contains("_load_")
                {
                    continue;
                }
                let context = nearby_text(&lines, idx, 2);
                if !context.contains("borrow_") || !context.contains("unchecked") {
                    continue;
                }
                let m = cap.get(0).expect("group 0");
                sites.push(PinocchioSite {
                    kind: SiteKind::CustomLoadCall,
                    file: rel_file.to_path_buf(),
                    line: line_no,
                    col: m.start() as u32 + 1,
                    expr: stripped.trim().to_string(),
                    fn_name: fn_name.clone(),
                    in_unsafe_block: in_unsafe,
                    safety_comment: safety.clone(),
                    extra: serde_json::json!({
                        "callee": callee,
                        "unchecked_variant": callee.contains("unchecked"),
                    }),
                });
            }
        }

        // 5. TryIntoUnwrapOnSlice
        for m in p.try_into_unwrap.find_iter(&stripped) {
            sites.push(PinocchioSite {
                kind: SiteKind::TryIntoUnwrapOnSlice,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped.trim().to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({}),
            });
        }

        // 6. SetLamportsArith
        if let Some(m) = p.set_lamports_arith_call.find(&stripped) {
            sites.push(PinocchioSite {
                kind: SiteKind::SetLamportsArith,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped.trim().to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({"form": "set_lamports_call"}),
            });
        }
        if let Some(m) = p.set_lamports_arith_deref.find(&stripped) {
            sites.push(PinocchioSite {
                kind: SiteKind::SetLamportsArith,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped.trim().to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({"form": "deref_lamports"}),
            });
        }

        // 7. SetAmountArith
        if let Some(m) = p.set_amount_arith.find(&stripped) {
            sites.push(PinocchioSite {
                kind: SiteKind::SetAmountArith,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped.trim().to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({}),
            });
        }

        // 8. IndexedAccountAccess
        for cap in p.indexed_account.captures_iter(&stripped) {
            let m = cap.get(0).expect("g0");
            let idx_val: u32 = cap[1].parse().unwrap_or(0);
            sites.push(PinocchioSite {
                kind: SiteKind::IndexedAccountAccess,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped[m.range()].to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({"index": idx_val}),
            });
        }

        // 9. IndexedDataSlice
        for cap in p.indexed_data_slice.captures_iter(&stripped) {
            let m = cap.get(0).expect("g0");
            sites.push(PinocchioSite {
                kind: SiteKind::IndexedDataSlice,
                file: rel_file.to_path_buf(),
                line: line_no,
                col: m.start() as u32 + 1,
                expr: stripped[m.range()].to_string(),
                fn_name: fn_name.clone(),
                in_unsafe_block: in_unsafe,
                safety_comment: safety.clone(),
                extra: serde_json::json!({
                    "lo": cap.get(1).map(|x| x.as_str().to_string()),
                    "hi": cap.get(2).map(|x| x.as_str().trim().to_string()),
                }),
            });
        }
    }

    // 10. SafetyComment — emitted standalone so the catalogue is
    // queryable without re-walking the source.
    for block in &safety_blocks {
        sites.push(PinocchioSite {
            kind: SiteKind::SafetyComment,
            file: rel_file.to_path_buf(),
            line: block.first_line,
            col: 1,
            expr: block.text.clone(),
            fn_name: None,
            in_unsafe_block: false,
            safety_comment: Some(block.text.clone()),
            extra: serde_json::json!({
                "attached_unsafe_line": block.attached_unsafe_line,
                "lines": block.last_line - block.first_line + 1,
            }),
        });
    }

    sites
}

fn strip_cfg_kani_regions(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    if lines.iter().any(|line| is_inner_cfg_kani_attr(line)) {
        return vec![String::new(); lines.len()].join("\n");
    }

    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        if !is_cfg_kani_attr(lines[i]) {
            out.push(lines[i].to_string());
            i += 1;
            continue;
        }

        out.push(String::new());
        i += 1;

        while i < lines.len() && lines[i].trim().is_empty() {
            out.push(String::new());
            i += 1;
        }

        let mut depth = 0_i32;
        let mut saw_open = false;
        while i < lines.len() {
            let stripped = strip_line_comment(lines[i]);
            for ch in stripped.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        saw_open = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            let statement_done = !saw_open && stripped.contains(';');
            out.push(String::new());
            i += 1;

            if statement_done || (saw_open && depth <= 0) {
                break;
            }
        }
    }

    out.join("\n")
}

fn is_cfg_kani_attr(line: &str) -> bool {
    is_inner_cfg_kani_attr(line) || is_outer_cfg_kani_attr(line)
}

fn is_inner_cfg_kani_attr(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact == "#![cfg(kani)]"
        || compact.starts_with("#![cfg(all(kani")
        || compact.starts_with("#![cfg(any(kani")
}

fn is_outer_cfg_kani_attr(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact == "#[cfg(kani)]"
        || compact.starts_with("#[cfg(all(kani")
        || compact.starts_with("#[cfg(any(kani")
}

/// Best-effort detection of `unsafe { ... }` block extents. We scan
/// for the literal token `unsafe {` and walk forward counting braces
/// until depth balances. Strings / comments inside the block are
/// stripped per-line. Returns (start_line, end_line) inclusive in
/// 1-based numbering.
fn compute_unsafe_block_ranges(lines: &[&str]) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let unsafe_re = Regex::new(r"\bunsafe\s*\{").expect("regex");

    for (idx, line) in lines.iter().enumerate() {
        let stripped = strip_line_comment(line);
        for m in unsafe_re.find_iter(&stripped) {
            // Walk from the open brace position forward.
            let start_line = (idx + 1) as u32;
            // Find the `{` after the `unsafe` keyword.
            let after = &stripped[m.end() - 1..]; // includes `{`
            let mut depth: i32 = 0;
            let mut consumed_first = false;
            // Continue across subsequent lines if needed.
            let mut end_line = start_line;
            let mut line_offset = idx;
            // Local slice to process: start with `after`, then full lines.
            let mut buf = after.to_string();
            loop {
                for ch in buf.chars() {
                    match ch {
                        '{' => {
                            depth += 1;
                            consumed_first = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if consumed_first && depth == 0 {
                        end_line = (line_offset + 1) as u32;
                        break;
                    }
                }
                if consumed_first && depth == 0 {
                    break;
                }
                line_offset += 1;
                if line_offset >= lines.len() {
                    end_line = lines.len() as u32;
                    break;
                }
                buf = strip_line_comment(lines[line_offset]).to_string();
                end_line = (line_offset + 1) as u32;
            }
            ranges.push((start_line, end_line));
        }
    }

    ranges
}

#[derive(Debug, Clone)]
struct SafetyBlock {
    first_line: u32,
    last_line: u32,
    text: String,
    attached_unsafe_line: Option<u32>,
}

/// Find every `// SAFETY:` comment run. A run is consecutive
/// `// `-prefixed lines whose first line starts with `// SAFETY:`,
/// ending at the next non-`//`-prefix line. Attach to the next
/// `unsafe` block within 3 lines.
fn parse_safety_comments(lines: &[&str]) -> Vec<SafetyBlock> {
    let mut blocks = Vec::new();
    let safety_start = Regex::new(r"^\s*//\s*SAFETY\b").expect("regex");
    let comment_cont = Regex::new(r"^\s*//").expect("regex");
    let unsafe_kw = Regex::new(r"\bunsafe\b").expect("regex");

    let mut i = 0;
    while i < lines.len() {
        if safety_start.is_match(lines[i]) {
            let first = i;
            let mut last = i;
            let mut text = String::new();
            while last < lines.len() && comment_cont.is_match(lines[last]) {
                // Strip the leading `//` and optional whitespace.
                let raw = lines[last].trim_start();
                let after = raw.trim_start_matches('/').trim_start_matches('/').trim();
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(after);
                last += 1;
            }
            // Look ahead up to 3 lines for an `unsafe` token.
            let end = (last + 4).min(lines.len());
            let attached = lines[last..end]
                .iter()
                .enumerate()
                .find(|(_, l)| unsafe_kw.is_match(l))
                .map(|(off, _)| (last + off + 1) as u32);
            blocks.push(SafetyBlock {
                first_line: (first + 1) as u32,
                last_line: last as u32, // inclusive of last comment row (last is index of first non-comment, so -1+1)
                text,
                attached_unsafe_line: attached,
            });
            i = last;
            continue;
        }
        i += 1;
    }
    blocks
}

fn safety_for_line(blocks: &[SafetyBlock], line: u32) -> Option<String> {
    // Return the SAFETY block whose `attached_unsafe_line` matches
    // the current line (or is 1-2 lines earlier — the `unsafe {`
    // statement may span lines). Tight attachment avoids cascading
    // a single SAFETY block across unrelated sites further down.
    for b in blocks {
        if let Some(att) = b.attached_unsafe_line {
            if line >= att && line <= att + 4 {
                return Some(b.text.clone());
            }
        }
    }
    None
}

fn strip_line_comment(line: &str) -> String {
    // Cheap strip: drop everything after `//` not inside a string.
    let mut in_str = false;
    let mut prev = '\0';
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch == '"' && prev != '\\' {
            in_str = !in_str;
        }
        if !in_str && ch == '/' && i + 1 < bytes.len() && bytes[i + 1] as char == '/' {
            break;
        }
        out.push(ch);
        prev = ch;
        i += 1;
    }
    out
}

fn nearby_text(lines: &[&str], idx: usize, radius: usize) -> String {
    let lo = idx.saturating_sub(radius);
    let hi = (idx + radius + 1).min(lines.len());
    lines[lo..hi].join("\n")
}

/// Map each site in the catalogue to a candidate `Finding` carrying the
/// canonical `MolluskPrompt` + `MiriPrompt` reproducer pair the audit
/// subagent expands. Per `feedback_repros_agent_authored`: the prompt
/// is the artifact the agent acts on — not a generated test body.
///
/// Only the high-signal site kinds map directly to findings; the rest
/// (e.g. `SafetyComment`, `IndexedAccountAccess`) inform the agent's
/// CF analysis via the `pinocchio_catalogue` envelope. We never
/// duplicate a `SafetyComment` into a `findings[]` entry — those are
/// strictly informational.
pub fn findings_from_catalogue(cat: &PinocchioCatalogue) -> Vec<crate::probe::Finding> {
    use crate::probe::{Category, Finding, Reproducer, Severity};
    use sha2::{Digest, Sha256};

    // Cache per-file source so repeated lookups (multiple sites in one
    // file) don't re-read the same content. Maps absolute path →
    // contents. Sites carry their path relative to `cat.project_root`.
    let mut source_cache: std::collections::HashMap<PathBuf, String> =
        std::collections::HashMap::new();
    let mut load_source = |rel: &Path| -> Option<String> {
        if let Some(s) = source_cache.get(rel) {
            return Some(s.clone());
        }
        // Try absolute (catalogue may store absolute paths) then
        // relative to project_root.
        let abs = if rel.is_absolute() {
            rel.to_path_buf()
        } else {
            cat.project_root.join(rel)
        };
        let text = std::fs::read_to_string(&abs).ok()?;
        source_cache.insert(rel.to_path_buf(), text.clone());
        Some(text)
    };

    let mut findings = Vec::new();

    for site in &cat.sites {
        let (category, severity, probe_md) = match &site.kind {
            SiteKind::BorrowUnchecked | SiteKind::CustomLoadCall => (
                Category::PinocchioUncheckedAccountLoad,
                Severity::High,
                "unchecked_account_load",
            ),
            SiteKind::SetAmountArith => (
                Category::PinocchioUncheckedArith,
                Severity::High,
                "unchecked_amount_arith",
            ),
            SiteKind::SetLamportsArith => (
                Category::PinocchioUncheckedArith,
                Severity::High,
                "unchecked_lamport_arith",
            ),
            SiteKind::BytemuckCall | SiteKind::RawPtrCastFromAccount => (
                Category::PinocchioAccountTypeConfusion,
                Severity::Medium,
                "account_type_confusion",
            ),
            SiteKind::IndexedDataSlice => (
                Category::PinocchioOffsetOverrun,
                Severity::Medium,
                "offset_overrun",
            ),
            SiteKind::IndexedAccountAccess => (
                Category::PinocchioPositionWithoutTypeTag,
                Severity::Medium,
                "position_based_account_without_type_tag",
            ),
            SiteKind::TryIntoUnwrapOnSlice => (
                Category::PinocchioOffsetOverrun,
                Severity::Low,
                "offset_overrun",
            ),
            // SafetyComment: informational only — never a finding.
            SiteKind::SafetyComment => continue,
        };

        // v2.22 Slice 5: compute `gated_by` for the high-noise
        // categories. Walks the source preceding the site for length /
        // discriminator / owner gate signals; the auditor subagent
        // bulk-suppresses findings whose gate set covers the
        // expected triad. Computed once per (site, finding pair) and
        // shared between the Mollusk and Miri finding emissions
        // below.
        let gated_by = match site.kind {
            SiteKind::BytemuckCall
            | SiteKind::RawPtrCastFromAccount
            | SiteKind::CustomLoadCall
            | SiteKind::IndexedDataSlice
            | SiteKind::TryIntoUnwrapOnSlice => {
                let source = load_source(&site.file);
                source
                    .as_deref()
                    .map(|src| detect_gates(src, site.line))
                    // Collapse empty-gate to None so the JSON envelope
                    // omits the field for findings with no gates
                    // detected (the auditor-focus subset).
                    .filter(|gates| !gates.is_empty())
            }
            _ => None,
        };

        // Stable id: hash of (file, line, kind). Mirrors probe.rs's
        // `stable_id` shape so suppression files stay consistent
        // across runs.
        let mut hasher = Sha256::new();
        hasher.update(site.file.display().to_string().as_bytes());
        hasher.update(b":");
        hasher.update(site.line.to_string().as_bytes());
        hasher.update(b":");
        hasher.update(format!("{:?}", site.kind).as_bytes());
        let id = format!("{:x}", hasher.finalize());
        let id = id[..16].to_string();

        // Substitution map seeded with the universal keys probe
        // markdowns rely on. Per-probe markdowns may reference
        // additional keys via the `extra` blob.
        let mut subs = std::collections::BTreeMap::new();
        subs.insert("FILE".to_string(), site.file.display().to_string());
        subs.insert("LINE".to_string(), site.line.to_string());
        subs.insert("EXPR".to_string(), site.expr.clone());
        subs.insert(
            "FN".to_string(),
            site.fn_name.clone().unwrap_or_else(|| "<unknown>".into()),
        );
        if let Some(safety) = &site.safety_comment {
            subs.insert("SAFETY_CLAIM".to_string(), safety.clone());
        }
        // Flatten the `extra` JSON object into sub keys for convenience.
        if let Some(obj) = site.extra.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    subs.insert(k.to_uppercase(), s.to_string());
                } else if let Some(n) = v.as_i64() {
                    subs.insert(k.to_uppercase(), n.to_string());
                } else if let Some(n) = v.as_u64() {
                    subs.insert(k.to_uppercase(), n.to_string());
                } else if let Some(b) = v.as_bool() {
                    subs.insert(k.to_uppercase(), b.to_string());
                }
            }
        }

        // Adversarial inputs derived from the SAFETY comment. The
        // agent reads each clause and picks a negation strategy per
        // the table in references/probes/pinocchio/<probe>.md. We
        // seed with the canonical strategies the probe is most likely
        // to consume; the agent extends or prunes during repro
        // authoring.
        let adversarial = adversarial_for(site, &category);

        // Invariant asserts the Miri repro brackets the handler with.
        // Conservative default: lamport + token conservation. The
        // agent picks the relevant subset per probe.
        let invariants = invariants_for(&category);

        let mollusk = Reproducer::MolluskPrompt {
            template_path: format!(
                "references/probes/pinocchio/{}.md#mollusk-reproducer",
                probe_md
            ),
            substitutions: subs.clone(),
            repro_path: format!(".qed/probes/pinocchio/{}/repro_mollusk.rs", id),
        };

        // Stale SAFETY upgrade: when a finding carries a SAFETY claim
        // and the site is one of the high-leverage kinds, emit the
        // `stale_safety_comment` probe in parallel. Surfaces in the
        // catalogue as a second finding sharing the same site; the
        // agent merges or keeps separate per its CF read.
        if site.safety_comment.is_some()
            && matches!(
                category,
                Category::PinocchioUncheckedAccountLoad | Category::PinocchioUncheckedArith
            )
        {
            findings.push(Finding {
                id: format!("{}-safety", id),
                category: Category::PinocchioStaleSafetyComment,
                severity: Severity::Medium,
                handler: site.fn_name.clone().unwrap_or_else(|| "<unknown>".into()),
                spec_silent_on: format!(
                    "`// SAFETY:` claim at {}:{} asserts preconditions \
                     the agent cannot verify hold on every CF path",
                    site.file.display(),
                    site.line
                ),
                suppression_hint: "Verify each SAFETY clause holds on every reachable path. \
                     If not, replace the unchecked variant with a checked one \
                     or add the missing precondition assertion."
                    .to_string(),
                investigation_hint: format!(
                    "Read the SAFETY comment attached to the unsafe block at \
                     {}:{}. For each clause, identify where in the CF graph \
                     the precondition is enforced. Use the adversarial \
                     inputs from the MiriPrompt to drive a counterexample.",
                    site.file.display(),
                    site.line
                ),
                category_tag: "pinocchio_stale_safety_comment".to_string(),
                reproducer: Some(Reproducer::MiriPrompt {
                    template_path:
                        "references/probes/pinocchio/stale_safety_comment.md#miri-reproducer"
                            .to_string(),
                    substitutions: subs.clone(),
                    repro_path: format!(".qed/probes/pinocchio/{}-safety/repro_miri.rs", id),
                    adversarial_inputs: adversarial.clone(),
                    invariant_asserts: invariants.clone(),
                }),
                gated_by: gated_by.clone(),
            });
        }

        let miri = Reproducer::MiriPrompt {
            template_path: format!(
                "references/probes/pinocchio/{}.md#miri-reproducer",
                probe_md
            ),
            substitutions: subs,
            repro_path: format!(".qed/probes/pinocchio/{}/repro_miri.rs", id),
            adversarial_inputs: adversarial,
            invariant_asserts: invariants,
        };

        // Primary finding carries the Mollusk variant; a sibling
        // finding (id + "-miri") carries the Miri variant. The
        // verifier wires both into the dual-execution comparator.
        let handler = site.fn_name.clone().unwrap_or_else(|| "<unknown>".into());

        findings.push(Finding {
            id: id.clone(),
            category: category.clone(),
            severity: severity.clone(),
            handler: handler.clone(),
            spec_silent_on: format!(
                "Pinocchio site at {}:{} ({:?}): no spec / Anchor framework \
                 guarantee proves the obligation is upheld",
                site.file.display(),
                site.line,
                site.kind
            ),
            suppression_hint: "Confirm via CF read that the obligation is enforced. \
                If not, add the missing check or switch to a checked variant."
                .to_string(),
            investigation_hint: format!(
                "Read `{}` around line {}. Apply the matching probe at \
                 references/probes/pinocchio/{}.md and follow its \
                 `what_the_agent_should_check` section.",
                site.file.display(),
                site.line,
                probe_md
            ),
            category_tag: probe_md.to_string(),
            reproducer: Some(mollusk),
            gated_by: gated_by.clone(),
        });

        findings.push(Finding {
            id: format!("{}-miri", id),
            category,
            severity,
            handler,
            spec_silent_on: format!(
                "Pinocchio site at {}:{}: UB / aliasing under Miri not \
                 verified",
                site.file.display(),
                site.line
            ),
            suppression_hint: "Run the Miri repro under `cargo +nightly miri test`. \
                If the test passes (no UB), suppress with rationale; if it fires, \
                the unchecked variant is unsafe in practice."
                .to_string(),
            investigation_hint: format!(
                "Generate the Miri repro at .qed/probes/pinocchio/{}/repro_miri.rs \
                 from the template and run `qedgen verify --miri`.",
                id
            ),
            category_tag: probe_md.to_string(),
            reproducer: Some(miri),
            gated_by,
        });
    }

    findings
}

/// v2.22 Slice 5: detect upstream guards preceding a zero-copy /
/// account-data load. Walks backward from `target_line` for ~30 lines
/// looking for canonical Pinocchio gate signals:
///
/// - **`length_check`** — any `<...>.len() < N` / `!= LEN` / `== LEN`
///   / `>= N` comparison (the prerequisite for safe transmute /
///   bytemuck on a byte slice).
/// - **`discriminator_check`** — any reference to an
///   `AccountDiscriminator::*` constant or a `discriminator` /
///   `DISCRIMINATOR` ident compared.
/// - **`owner_check`** — `ProgramAccount::check(<acc>, ...)` /
///   `<acc>.owner()` reference / `&crate::ID` comparison.
///
/// When all three fire upstream, the load is defensively fenced and
/// the auditor subagent can bulk-suppress the finding via the
/// `gated_by` list. When only the length gate fires (the
/// instruction-data parsing case), the finding stays but the
/// auditor knows the buffer-bound check is in place.
pub(crate) fn detect_gates(source: &str, target_line: u32) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    if target_line == 0 || (target_line as usize) > lines.len() {
        return Vec::new();
    }
    let target_idx = (target_line as usize) - 1;
    let lo = target_idx.saturating_sub(30);
    let window = lines[lo..=target_idx].join("\n");
    let mut gates: Vec<String> = Vec::new();
    let has_len = regex::Regex::new(r"\.len\(\)\s*(?:[<>!=]=?|<|>)")
        .ok()
        .map(|re| re.is_match(&window))
        .unwrap_or(false);
    if has_len {
        gates.push("length_check".to_string());
    }
    let has_disc = window.contains("Discriminator")
        || window.contains("discriminator")
        || window.contains("DISCRIMINATOR");
    if has_disc {
        gates.push("discriminator_check".to_string());
    }
    let has_owner = window.contains("ProgramAccount::check")
        || window.contains(".owner()")
        || window.contains("check_owner")
        || window.contains("&crate::ID");
    if has_owner {
        gates.push("owner_check".to_string());
    }
    gates
}

/// Per-category catalogue of adversarial inputs the agent should
/// include in the Miri reproducer.
fn adversarial_for(
    site: &PinocchioSite,
    category: &crate::probe::Category,
) -> Vec<crate::probe::AdversarialInput> {
    use crate::probe::{AdversarialInput, Category};
    let mut out = Vec::new();
    let safety = site.safety_comment.clone().unwrap_or_default();

    // Generic by-claim mapping. The agent re-reads the SAFETY text
    // verbatim and refines.
    if safety.to_lowercase().contains("init") {
        out.push(AdversarialInput {
            claim_text: "account is initialized".to_string(),
            negation_strategy: "uninit_init_flag".to_string(),
            expected_outcome: "handler_err".to_string(),
        });
    }
    if safety.to_lowercase().contains("different") || safety.to_lowercase().contains("distinct") {
        out.push(AdversarialInput {
            claim_text: "two accounts are distinct".to_string(),
            negation_strategy: "swap_position".to_string(),
            expected_outcome: "miri_ub".to_string(),
        });
    }
    if safety.to_lowercase().contains("owner") {
        out.push(AdversarialInput {
            claim_text: "owner matches program_id".to_string(),
            negation_strategy: "foreign_owner".to_string(),
            expected_outcome: "handler_err".to_string(),
        });
    }
    if safety.to_lowercase().contains("len") || safety.to_lowercase().contains("size") {
        out.push(AdversarialInput {
            claim_text: "data buffer is at least N bytes".to_string(),
            negation_strategy: "short_buffer".to_string(),
            expected_outcome: "either".to_string(),
        });
    }

    // Category-specific defaults — fire even with no SAFETY comment.
    match category {
        Category::PinocchioUncheckedArith => {
            out.push(AdversarialInput {
                claim_text: "arithmetic does not overflow".to_string(),
                negation_strategy: "oversized_amount".to_string(),
                expected_outcome: "miri_ub".to_string(),
            });
        }
        Category::PinocchioUncheckedAccountLoad if out.is_empty() => {
            out.push(AdversarialInput {
                claim_text: "implicit precondition: account ownership".to_string(),
                negation_strategy: "foreign_owner".to_string(),
                expected_outcome: "handler_err".to_string(),
            });
        }
        _ => {}
    }

    out
}

fn invariants_for(category: &crate::probe::Category) -> Vec<String> {
    use crate::probe::Category;
    match category {
        Category::PinocchioUncheckedArith => vec![
            "assert_lamport_conservation".to_string(),
            "assert_token_conservation_per_mint".to_string(),
        ],
        Category::PinocchioUncheckedAccountLoad => vec![
            "assert_no_unowned_writes".to_string(),
            "assert_distinct_data_buffers".to_string(),
        ],
        Category::PinocchioMutableBorrowAliasing => {
            vec!["assert_distinct_data_buffers".to_string()]
        }
        Category::PinocchioMissingPdaVerification => {
            vec!["assert_no_unowned_writes".to_string()]
        }
        _ => vec!["assert_lamport_conservation".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_borrow_unchecked() {
        let src = r#"
fn foo(account: &Account) {
    let data = unsafe { account.borrow_mut_data_unchecked() };
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("foo.rs"), src, &p);
        assert!(sites
            .iter()
            .any(|s| matches!(s.kind, SiteKind::BorrowUnchecked)));
    }

    #[test]
    fn ignores_cfg_kani_file() {
        let src = r#"
#![cfg(kani)]

fn proof_only(account: &Account) {
    let data = unsafe { account.borrow_mut_data_unchecked() };
}

fn also_proof_only(accounts: &[AccountInfo]) {
    let account = unsafe { core::slice::from_raw_parts(&accounts as *const _ as *const AccountInfo, 2) };
    let amount = u64::from_le_bytes(data[AMOUNT_OFFSET..AMOUNT_OFFSET + 8].try_into().unwrap());
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("kani_impl.rs"), src, &p);
        assert!(
            sites.is_empty(),
            "cfg(kani)-only files should not produce production probe sites: {sites:?}"
        );
    }

    #[test]
    fn ignores_cfg_kani_function_but_keeps_production_lines() {
        let src = r#"
#[cfg(kani)]
fn proof_only(account: &Account) {
    let data = unsafe { account.borrow_mut_data_unchecked() };
}

fn production(account: &Account) {
    let data = unsafe { account.borrow_mut_data_unchecked() };
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("processor.rs"), src, &p);
        let borrow_sites = sites
            .iter()
            .filter(|s| matches!(s.kind, SiteKind::BorrowUnchecked))
            .collect::<Vec<_>>();
        assert_eq!(borrow_sites.len(), 1, "got sites: {sites:?}");
        assert_eq!(borrow_sites[0].line, 8);
    }

    #[test]
    fn ignores_cfg_kani_inner_block() {
        let src = r#"
fn invoke(account: &Account) {
    #[cfg(kani)]
    {
        let data = unsafe { account.borrow_mut_data_unchecked() };
        return;
    }

    let data = unsafe { account.borrow_mut_data_unchecked() };
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("processor.rs"), src, &p);
        let borrow_sites = sites
            .iter()
            .filter(|s| matches!(s.kind, SiteKind::BorrowUnchecked))
            .collect::<Vec<_>>();
        assert_eq!(borrow_sites.len(), 1, "got sites: {sites:?}");
        assert_eq!(borrow_sites[0].line, 9);
    }

    #[test]
    fn detect_set_amount_arith() {
        let src = r#"
fn transfer(dst: &mut Account, amount: u64) {
    dst.set_amount(dst.amount() + amount);
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("foo.rs"), src, &p);
        assert!(
            sites
                .iter()
                .any(|s| matches!(s.kind, SiteKind::SetAmountArith)),
            "expected SetAmountArith in {:?}",
            sites
                .iter()
                .map(|s| format!("{:?}", s.kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn detect_set_lamports_deref() {
        let src = r#"
fn pay(src: &mut Account, amount: u64) {
    let source_lamports = unsafe { src.borrow_mut_lamports_unchecked() };
    *source_lamports -= amount;
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("foo.rs"), src, &p);
        assert!(sites
            .iter()
            .any(|s| matches!(s.kind, SiteKind::SetLamportsArith)));
    }

    #[test]
    fn detect_indexed_account() {
        let src = r#"
fn dispatch(accounts: &[AccountInfo]) {
    let x = &accounts[0];
    let y = &accounts[2];
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("foo.rs"), src, &p);
        assert_eq!(
            sites
                .iter()
                .filter(|s| matches!(s.kind, SiteKind::IndexedAccountAccess))
                .count(),
            2
        );
    }

    #[test]
    fn detect_safety_comment_and_attach() {
        let src = r#"
fn foo() {
    // SAFETY: the account is initialized and distinct from src
    // and has been validated to be a token account.
    let acct = unsafe { load_mut::<Account>(info.borrow_mut_data_unchecked()) };
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("foo.rs"), src, &p);
        let has_safety = sites
            .iter()
            .any(|s| matches!(s.kind, SiteKind::SafetyComment));
        assert!(has_safety);
        let load = sites
            .iter()
            .find(|s| matches!(s.kind, SiteKind::BorrowUnchecked));
        assert!(load.is_some());
        let load = load.unwrap();
        assert!(load.safety_comment.is_some());
        let txt = load.safety_comment.clone().unwrap();
        assert!(txt.contains("initialized"));
    }

    #[test]
    fn detect_gates_fires_on_canonical_triad() {
        let src = r#"
pub fn load(account: &AccountView, data: &[u8]) -> Result<&Self, ProgramError> {
    if data.len() != Self::LEN {
        return Err(InvalidData);
    }
    if data[DISCRIMINATOR_OFFSET] != AccountDiscriminator::Plan as u8 {
        return Err(InvalidDiscriminator);
    }
    ProgramAccount::check(account, &crate::ID)?;
    Ok(unsafe { &*transmute::<*const u8, *const Self>(data.as_ptr()) })
}
"#;
        // target_line points at the transmute (last non-blank line ≈ 9).
        let gates = super::detect_gates(src, 9);
        assert!(gates.contains(&"length_check".to_string()));
        assert!(gates.contains(&"discriminator_check".to_string()));
        assert!(gates.contains(&"owner_check".to_string()));
    }

    #[test]
    fn detect_gates_returns_empty_when_no_gates() {
        let src = r#"
pub fn risky_load(data: &[u8]) -> &Self {
    unsafe { &*transmute::<*const u8, *const Self>(data.as_ptr()) }
}
"#;
        let gates = super::detect_gates(src, 3);
        assert!(gates.is_empty());
    }

    #[test]
    fn detect_gates_partial_length_only() {
        // Instruction-data parse — has length gate but no
        // discriminator / owner (those don't apply to instruction
        // data).
        let src = r#"
pub fn load(data: &[u8]) -> Result<&Self, ProgramError> {
    if data.len() != Self::LEN {
        return Err(InvalidData);
    }
    Ok(unsafe { &*transmute::<*const u8, *const Self>(data.as_ptr()) })
}
"#;
        let gates = super::detect_gates(src, 5);
        assert_eq!(gates, vec!["length_check".to_string()]);
    }

    #[test]
    fn ignore_load_in_safe_context() {
        let src = r#"
fn downloader() {
    let result = download_archive(url);
}
"#;
        let p = SitePatterns::new();
        let sites = scan_file(Path::new("foo.rs"), src, &p);
        assert!(!sites
            .iter()
            .any(|s| matches!(s.kind, SiteKind::CustomLoadCall)));
    }
}
