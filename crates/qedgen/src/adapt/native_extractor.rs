//! Native Rust proto-clause extractor (v2.19 M4.1, preview-quality).
//!
//! Native Solana programs (solana-program / pinocchio-free /
//! anchor-free) have no framework conventions — every check is the
//! author's responsibility. Detection patterns are looser than
//! Anchor's because there's no `#[derive(Accounts)]` to scan and no
//! `_unchecked` suffix to look for.
//!
//! v1 detectors:
//!
//! 1. **`invoke_signed` / `invoke` calls without an adjacent program-ID
//!    check** → `CpiProgramPin`. Scans for `invoke_signed(` /
//!    `invoke(` and asserts that within ~10 lines above the call,
//!    a `program_id ==` or `check_<program>_program` pattern appears.
//!    Conservative: false-negative biased (won't surface every issue),
//!    not false-positive biased.
//!
//! 2. **`create_program_address` usage** → `PdaCanonicalDerivation`.
//!    `create_program_address` accepts a user-supplied bump;
//!    `find_program_address` derives the canonical one. The
//!    canonical-bump pattern is `find_program_address` followed by
//!    storing the bump on the account.
//!
//! 3. **Raw arithmetic on numeric fields** → `ArithmeticNoOverflow`.
//!    Reuses the Anchor extractor's per-line heuristic (skips
//!    `checked_*` / `saturating_*` / `wrapping_*` family + Anchor-
//!    attribute false-positives).
//!
//! 4. **Direct lamport mutation** (`**account.try_borrow_mut_lamports()
//!    -= x`) → `ArithmeticNoOverflow` (raw arith subtype) +
//!    `CpiAccountDirection` (because the mutation pattern can
//!    redirect rent if `destination` is signer-controlled). v1 only
//!    flags as `ArithmeticNoOverflow`; close-redirection is a
//!    SKILL.md/Read+Grep predicate deferred to the agent layer.
//!
//! Out of scope for v1 (Native is "preview" per the release notes):
//! - Manual signer-check absence detection (requires data-flow
//!   analysis to know which account is "authority").
//! - Manual owner-check absence detection (same — needs to know
//!   which account is "token-shaped").
//! - Discriminator collision detection (cross-handler analysis).
//!
//! These are covered by the auditor SKILL.md's per-runtime predicate
//! set at the agent layer (Read+Grep on the impl); the extractor
//! complements it for the patterns where syntactic detection is
//! reliable.

use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};

use crate::cluster::{ClusterKind, ProtoClause};

/// Entry point — walks the project root and emits Native proto-clauses.
pub fn extract_proto_clauses(project_root: &Path) -> Result<Vec<ProtoClause>> {
    let rs_files = collect_rust_files(project_root)?;
    let mut out = Vec::new();
    let pat = NativePatterns::new();

    // Map fn name → file location for handler attribution. Native
    // doesn't have a canonical entry naming convention; we collect
    // every `pub fn` and assume any non-helper is a handler.
    let handler_set: std::collections::BTreeSet<String> = rs_files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|s| {
            pat.pub_fn
                .captures_iter(&s)
                .map(|c| c.get(1).unwrap().as_str().to_string())
                .collect::<Vec<_>>()
        })
        .collect();

    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };

        // Pass 1: CPI calls without nearby program-ID checks
        for (line_idx, line) in source.lines().enumerate() {
            if !pat.invoke_call.is_match(line) {
                continue;
            }
            // Look back up to 10 lines for a program-ID validation
            // pattern.
            let window_start = line_idx.saturating_sub(10);
            let window: Vec<&str> = source
                .lines()
                .skip(window_start)
                .take(line_idx - window_start + 1)
                .collect();
            let window_str = window.join("\n");
            let validated = pat.program_id_check.is_match(&window_str)
                || pat.check_program_helper.is_match(&window_str);
            if validated {
                continue;
            }
            let handler = nearest_handler(&source, line_idx + 1, &pat, &handler_set);
            out.push(ProtoClause {
                kind: ClusterKind::CpiProgramPin,
                handler,
                finding_id: format!(
                    "native:{}:{}:cpi_no_program_pin",
                    file.display(),
                    line_idx + 1
                ),
                evidence_text: format!(
                    "`invoke_signed` / `invoke` at {}:{} has no `program_id ==` or `check_<program>_program(...)` validation within 10 lines.",
                    file.display(),
                    line_idx + 1
                ),
            });
        }

        // Pass 2: create_program_address (non-canonical PDA)
        for (line_idx, line) in source.lines().enumerate() {
            if !pat.create_program_address.is_match(line) {
                continue;
            }
            let handler = nearest_handler(&source, line_idx + 1, &pat, &handler_set);
            out.push(ProtoClause {
                kind: ClusterKind::PdaCanonicalDerivation,
                handler,
                finding_id: format!(
                    "native:{}:{}:create_program_address",
                    file.display(),
                    line_idx + 1
                ),
                evidence_text: format!(
                    "`create_program_address` at {}:{} accepts a user-supplied bump; canonical derivation uses `find_program_address`.",
                    file.display(),
                    line_idx + 1
                ),
            });
        }

        // Pass 3: raw arithmetic (reuses Anchor's heuristic with
        // Native-aware filters: no Anchor attributes to skip).
        let arith_lines = scan_arith_sites(&source, &pat);
        for line in arith_lines {
            let handler = nearest_handler(&source, line, &pat, &handler_set);
            out.push(ProtoClause {
                kind: ClusterKind::ArithmeticNoOverflow,
                handler,
                finding_id: format!("native:{}:{}:raw_arith", file.display(), line),
                evidence_text: format!(
                    "Raw arithmetic at {}:{} — no checked_* / saturating_* / wrapping_* wrapper.",
                    file.display(),
                    line
                ),
            });
        }

        // Pass 4: direct lamport mutation
        for (line_idx, line) in source.lines().enumerate() {
            if !pat.lamport_mut.is_match(line) {
                continue;
            }
            let handler = nearest_handler(&source, line_idx + 1, &pat, &handler_set);
            out.push(ProtoClause {
                kind: ClusterKind::ArithmeticNoOverflow,
                handler,
                finding_id: format!(
                    "native:{}:{}:lamport_demotion",
                    file.display(),
                    line_idx + 1
                ),
                evidence_text: format!(
                    "Direct lamport mutation at {}:{} via `**account.try_borrow_mut_lamports()? OP x` — also reachable as a close-account redirection if the destination is signer-controlled (see auditor SKILL.md).",
                    file.display(),
                    line_idx + 1
                ),
            });
        }
    }

    Ok(out)
}

struct NativePatterns {
    pub_fn: Regex,
    invoke_call: Regex,
    program_id_check: Regex,
    check_program_helper: Regex,
    create_program_address: Regex,
    lamport_mut: Regex,
    checked_call: Regex,
}

impl NativePatterns {
    fn new() -> Self {
        Self {
            pub_fn: Regex::new(r"(?m)^\s*pub\s+fn\s+(\w+)\s*\(").unwrap(),
            // `invoke_signed(` or `invoke(` — direct or via solana_program prefix.
            invoke_call: Regex::new(r"\b(?:solana_program::program::)?invoke(?:_signed)?\s*\(")
                .unwrap(),
            // `program_id == &X` or `program_id != &X` or
            // `account.key == &spl_token::id()` or
            // `account.key() == &spl_token::id()` — any explicit pubkey
            // comparison against a program-ID-shaped target. Tolerates
            // both field access (older SDK) and method call (newer SDK).
            program_id_check: Regex::new(
                r"\bprogram_id\s*(?:==|!=)|\b(?:key|owner)(?:\s*\(\s*\))?\s*(?:==|!=)\s*&?\w+::id\(\)",
            )
            .unwrap(),
            // Recognizes helper-named patterns like `check_*_program(...)`.
            check_program_helper: Regex::new(r"\bcheck_\w+_program\s*\(").unwrap(),
            create_program_address: Regex::new(
                r"\bPubkey::create_program_address\b|\bcreate_program_address\s*\(",
            )
            .unwrap(),
            // `**account.try_borrow_mut_lamports()? -= x` and family.
            lamport_mut: Regex::new(
                r"\*\*[\w\.]+\s*\.\s*(?:try_)?borrow_mut_lamports\s*\(\s*\)",
            )
            .unwrap(),
            checked_call: Regex::new(
                r"\b(?:checked|saturating|wrapping|overflowing)_(?:add|sub|mul|div|rem)\b",
            )
            .unwrap(),
        }
    }
}

/// Walk the source and find the most-recently-declared `pub fn` at or
/// before the given line. Best-effort attribution.
fn nearest_handler(
    source: &str,
    site_line: usize,
    pat: &NativePatterns,
    handler_set: &std::collections::BTreeSet<String>,
) -> String {
    let mut best: Option<(usize, String)> = None;
    for caps in pat.pub_fn.captures_iter(source) {
        let line = source[..caps.get(0).unwrap().start()].matches('\n').count() + 1;
        if line <= site_line {
            best = Some((line, caps.get(1).unwrap().as_str().to_string()));
        } else {
            break;
        }
    }
    match best {
        Some((_, name)) if handler_set.contains(&name) => name,
        Some((_, name)) => name,
        None => "<unknown>".to_string(),
    }
}

/// Reused-shape arith scanner. Same heuristic as the Anchor extractor's
/// minus the Anchor-attribute filter (Native source doesn't have
/// `#[account(...)]` macros).
fn scan_arith_sites(source: &str, pat: &NativePatterns) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }
        if pat.checked_call.is_match(line) {
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

fn contains_assignment(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b' '
            };
            if next == b'=' || matches!(prev, b'!' | b'<' | b'>' | b'=') {
                continue;
            }
            return true;
        }
    }
    false
}

fn has_arith_operator(s: &str) -> bool {
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
                return false;
            }
            if b == b'-' && next == b'>' {
                continue;
            }
            return true;
        }
    }
    false
}

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
    fn invoke_signed_without_program_id_check_yields_cpi_pin() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use solana_program::*;
pub fn handler(accounts: &[AccountInfo]) -> ProgramResult {
    let token_program = &accounts[0];
    invoke_signed(
        &transfer_ix,
        accounts,
        &[&[b"vault", &[bump]]],
    )?;
    Ok(())
}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let cpi: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::CpiProgramPin)
            .collect();
        assert_eq!(cpi.len(), 1, "expected 1 CpiProgramPin, got {:?}", cpi);
        Ok(())
    }

    #[test]
    fn invoke_signed_with_program_id_check_is_suppressed() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
use solana_program::*;
pub fn handler(accounts: &[AccountInfo]) -> ProgramResult {
    let token_program = &accounts[0];
    if token_program.key != &spl_token::id() {
        return Err(ProgramError::IncorrectProgramId);
    }
    invoke_signed(&transfer_ix, accounts, &[&[b"vault", &[bump]]])?;
    Ok(())
}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let cpi: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::CpiProgramPin)
            .collect();
        assert!(
            cpi.is_empty(),
            "program_id check should suppress CpiProgramPin; got {:?}",
            cpi
        );
        Ok(())
    }

    #[test]
    fn check_program_helper_suppresses_cpi_pin() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
pub fn handler(accounts: &[AccountInfo]) -> ProgramResult {
    check_token_program(&accounts[0])?;
    invoke_signed(&ix, accounts, &[&[b"v", &[bump]]])?;
    Ok(())
}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let cpi: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::CpiProgramPin)
            .collect();
        assert!(
            cpi.is_empty(),
            "check_<program>_program helper should suppress"
        );
        Ok(())
    }

    #[test]
    fn create_program_address_yields_pda_canonical() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
pub fn handler() -> ProgramResult {
    let pda = Pubkey::create_program_address(&[b"vault", &[bump]], &id())?;
    Ok(())
}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let pda: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::PdaCanonicalDerivation)
            .collect();
        assert_eq!(pda.len(), 1);
        Ok(())
    }

    #[test]
    fn lamport_mutation_yields_arith_proto_clause() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
pub fn close_account(accounts: &[AccountInfo]) -> ProgramResult {
    let acct = &accounts[0];
    let dest = &accounts[1];
    **dest.try_borrow_mut_lamports()? += **acct.try_borrow_lamports()?;
    **acct.try_borrow_mut_lamports()? = 0;
    Ok(())
}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let demot: Vec<_> = protos
            .iter()
            .filter(|p| {
                p.kind == ClusterKind::ArithmeticNoOverflow
                    && p.finding_id.contains("lamport_demotion")
            })
            .collect();
        assert_eq!(
            demot.len(),
            2,
            "expected 2 lamport-demotion clauses, got {:?}",
            demot
        );
        Ok(())
    }

    #[test]
    fn raw_arith_in_native_handler_yields_clause() -> Result<()> {
        let dir = tempdir()?;
        write_file(
            dir.path(),
            "src/lib.rs",
            r#"
pub fn increment(accounts: &[AccountInfo], delta: u64) -> ProgramResult {
    let mut counter: u64 = 0;
    counter = counter + delta;
    Ok(())
}
"#,
        );
        let protos = extract_proto_clauses(dir.path())?;
        let arith: Vec<_> = protos
            .iter()
            .filter(|p| p.kind == ClusterKind::ArithmeticNoOverflow)
            .collect();
        assert!(
            !arith.is_empty(),
            "expected raw-arith clause; got {:?}",
            arith
        );
        Ok(())
    }
}
