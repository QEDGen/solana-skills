//! Native (framework-free) proto-clause extractor — no conventions to lean
//! on, so detection is looser than Anchor's. Detectors: `invoke`/
//! `invoke_signed` with no program-ID check within ~10 lines above →
//! CpiProgramPin (false-negative biased); `create_program_address` (accepts a
//! user-supplied bump; `find_program_address` is canonical) →
//! PdaCanonicalDerivation; raw arithmetic and direct lamport mutation →
//! ArithmeticNoOverflow (close-redirection stays an agent-layer predicate).
//! Signer/owner-check absence and discriminator collisions need data-flow
//! analysis and stay with the agent-layer predicate set.

use anyhow::Result;
use regex::Regex;
use std::path::Path;

use crate::adapt::arith_heuristics;
use crate::cluster::{ClusterKind, ProtoClause};
use crate::fs_walk;

/// Entry point — walks the project root and emits Native proto-clauses.
pub fn extract_proto_clauses(project_root: &Path) -> Result<Vec<ProtoClause>> {
    let rs_files = fs_walk::collect_rs_files(project_root, fs_walk::DEFAULT_SKIP_DIRS);
    let mut out = Vec::new();
    let pat = NativePatterns::new();

    // Native has no canonical entry naming; collect every `pub fn` and
    // assume any non-helper is a handler.
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
            // Look back up to 10 lines for a program-ID validation.
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

        // Pass 3: raw arithmetic.
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
            // Explicit pubkey comparison against a program-ID-shaped target;
            // tolerates field access (older SDK) and method call (newer SDK).
            program_id_check: Regex::new(
                r"\bprogram_id\s*(?:==|!=)|\b(?:key|owner)(?:\s*\(\s*\))?\s*(?:==|!=)\s*&?\w+::id\(\)",
            )
            .unwrap(),
            // Helper-named checks: `check_*_program(...)`.
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

/// Most-recently-declared `pub fn` at or before `site_line` — best-effort
/// attribution.
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

/// Anchor extractor's arith heuristic minus the attribute filter (no
/// `#[account(...)]` macros in Native source).
fn scan_arith_sites(source: &str, pat: &NativePatterns) -> Vec<usize> {
    arith_heuristics::scan_arith_sites(source, &pat.checked_call, |_, _| false)
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
