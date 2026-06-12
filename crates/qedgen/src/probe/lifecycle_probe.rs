//! Lifecycle external-state probe — v2.22 Slice 4.
//!
//! Detects close-handler / authority-grant asymmetry: a handler closes a
//! PDA that holds external authority (SPL Approve delegate, token mint
//! authority, ATA delegate, etc.) without issuing the corresponding
//! reverse CPI (`Revoke`, `SetAuthority::None`, `Assign`). The closed
//! PDA is still registered as an active delegate / authority on the
//! external account, visible to wallet UIs and downstream programs as
//! live permission.
//!
//! Closes the QED-HEAD-MED-3 finding the subscriptions HEAD bench
//! surfaced (close_subscription_authority closes the PDA but doesn't
//! `Revoke` the prior SPL Approve).
//!
//! ## Rule shape
//!
//! Two-stage:
//!
//! 1. **Stage A — authority grants.** Walk every `*.rs` for CPI shapes
//!    that confer authority on a named account:
//!    - `Approve { ..., delegate: <X>, ... }` / `Approve2022` /
//!      `ApproveSpl`
//!    - `SetAuthority { ..., new_authority: <X>, ... }` (or
//!      equivalent)
//!    - `system_program::Assign { ..., new_owner: <X> }` /
//!      `Assign { ..., owner: <X> }`
//!
//!    Record the named account that received the authority.
//!
//! 2. **Stage B — close handlers without revoke.** Walk every fn
//!    body whose enclosing file or fn name signals a close-shape
//!    handler (`close_*.rs`, fn name contains `close` / `revoke` /
//!    `terminate`). For each close handler:
//!    - Identify the closed PDA from `<X>::close(target, ...)`
//!      / `close_account(...)` / `ProgramAccount::close(...)`.
//!    - If the closed PDA was recorded as a Stage A authority and
//!      the close handler body does NOT contain a `Revoke` /
//!      `RevokeSpl` / `Revoke2022` / `SetAuthority` reverse CPI,
//!      emit a MEDIUM finding.
//!
//! ## False-positive guards
//!
//! - Close handlers that DO contain a reverse-CPI (`Revoke`,
//!   `RevokeSpl`, `Revoke2022`) are suppressed.
//! - Test fns are filtered (same predicate as the other source
//!   scanners).
//!
//! See PRD-v2.22 §S4.1.

use anyhow::Result;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::probe::{Category, Finding, Reproducer, Severity};

#[derive(Debug, Clone)]
struct AuthorityGrant {
    rel_file: PathBuf,
    line: u32,
    /// camelCase / snake_case ident receiving the authority (the
    /// `delegate: <X>` / `new_authority: <X>` RHS, stripped of
    /// `accounts.` and `.address()` ornamentation).
    target_account: String,
    /// The grant operator name for reproducer narrative
    /// (`Approve`, `SetAuthority`, ...).
    operator: String,
}

#[derive(Debug, Clone)]
struct CloseSite {
    rel_file: PathBuf,
    line: u32,
    fn_name: String,
    /// The account being closed (first arg of the close call,
    /// normalised same way as Stage A targets).
    closed_account: String,
    /// True when the enclosing fn body contains a revoke-shape CPI,
    /// indicating the close path properly tears down the authority.
    has_revoke: bool,
}

/// Entry point: walk `<root>/src/**/*.rs`, collect grants + close
/// sites, then cross-match.
pub fn scan_program(project_root: &Path) -> Result<Vec<Finding>> {
    let src_dir = project_root.join("src");
    // Some projects keep their program crate under `program/src/`
    // rather than the workspace root's `src/`. Walk whichever exists
    // — falling back to nothing is fine (the catalog auto-suppresses).
    let scan_root = if src_dir.exists() {
        src_dir
    } else if project_root.join("program").join("src").exists() {
        project_root.join("program").join("src")
    } else {
        return Ok(Vec::new());
    };
    let rs_files = collect_rust_files(&scan_root)?;
    let mut grants: Vec<AuthorityGrant> = Vec::new();
    let mut closes: Vec<CloseSite> = Vec::new();
    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(project_root)
            .unwrap_or(file)
            .to_path_buf();
        grants.extend(scan_authority_grants(&rel, &source));
        closes.extend(scan_close_sites(&rel, &source));
    }
    Ok(emit_findings(&grants, &closes))
}

/// Stage A: extract authority-conferring CPI invocations. Matches the
/// canonical Pinocchio / Anchor struct-literal shapes:
///
/// - `Approve { ..., delegate: <X>, ... }.invoke(...)`
/// - `Approve2022 { ..., delegate: <X>, ... }.invoke(...)`
/// - `ApproveSpl { ..., delegate: <X>, ... }.invoke(...)`
/// - `SetAuthority { ..., new_authority: <X>, ... }.invoke(...)`
/// - `Assign { ..., new_owner: <X>, ... }.invoke(...)` /
///   `Assign { ..., owner: <X>, ... }`
fn scan_authority_grants(rel_file: &Path, source: &str) -> Vec<AuthorityGrant> {
    // The regex captures `<Approve...>` / `<SetAuthority>` / `<Assign>`
    // struct-literal blocks. We allow up to ~600 chars between the
    // opening `{` and the field we care about — Approve calls in
    // subscriptions take 5-6 fields including `token_program`,
    // `source`, etc.
    let grant_re = Regex::new(
        r"(?s)\b(?P<op>Approve(?:2022|Spl)?|SetAuthority|Assign)\s*\{(?P<body>[^}]{0,800})\}",
    )
    .expect("static regex compiles");
    let delegate_re = Regex::new(
        r"(?:delegate|new_authority|new_owner|authority)\s*:\s*(?P<target>[A-Za-z_][\w\.\(\)]{0,80})",
    )
    .expect("static regex compiles");
    let new_authority_re = Regex::new(r"new_authority\s*:\s*(?P<target>[A-Za-z_][\w\.\(\)]{0,80})")
        .expect("static regex compiles");
    let owner_re = Regex::new(r"(?:new_owner|owner)\s*:\s*(?P<target>[A-Za-z_][\w\.\(\)]{0,80})")
        .expect("static regex compiles");

    let mut out = Vec::new();
    for caps in grant_re.captures_iter(source) {
        let block_start = caps.get(0).unwrap().start();
        if line_is_commented(source, block_start) {
            continue;
        }
        let op = caps.name("op").unwrap().as_str();
        // SetAuthority's `authority` field is the *current* authority,
        // not the new one. Only match `new_authority` for SetAuthority;
        // for the others, `authority` is fine.
        let body = caps.name("body").unwrap().as_str();
        let target_field = if op == "SetAuthority" {
            new_authority_re.captures(body).map(|c| {
                let raw = c.name("target").unwrap().as_str();
                normalize_target(raw)
            })
        } else if op == "Assign" {
            owner_re.captures(body).map(|c| {
                let raw = c.name("target").unwrap().as_str();
                normalize_target(raw)
            })
        } else {
            // Approve family: `delegate` field is the receiver.
            delegate_re.captures(body).and_then(|c| {
                // The combined regex matches `authority` too; for
                // Approve we specifically want delegate.
                let full = c.get(0).unwrap().as_str();
                if !full.starts_with("delegate") {
                    return None;
                }
                let raw = c.name("target").unwrap().as_str();
                Some(normalize_target(raw))
            })
        };
        let Some(target) = target_field else {
            continue;
        };
        if target.is_empty() {
            continue;
        }
        let line = byte_offset_to_line(source, block_start);
        out.push(AuthorityGrant {
            rel_file: rel_file.to_path_buf(),
            line,
            target_account: target,
            operator: op.to_string(),
        });
    }
    out
}

/// Stage B: find close handlers. Two signals: (a) file name starts
/// with `close_` / `revoke_` / `terminate_` (the per-instruction file
/// convention in subscriptions / escrow); (b) fn name contains
/// `close` / `revoke` / `terminate` for codebases that consolidate
/// multiple lifecycle handlers in one file.
fn scan_close_sites(rel_file: &Path, source: &str) -> Vec<CloseSite> {
    // The fn signature we care about: `pub fn <name>(...)` where the
    // body contains a close-shape call. Per-file scan: match every
    // such fn, then per-fn check whether the body closes a known
    // account and whether it carries a Revoke.
    let fn_re =
        Regex::new(r"(?m)^(?:\s*pub(?:\([^)]*\))?\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
            .expect("static regex compiles");
    let close_re = Regex::new(
        r"(?:[A-Za-z_]\w*::close\s*\(\s*(?P<a>[^,)]+)|close_account\s*\(\s*(?P<b>[^,)]+))",
    )
    .expect("static regex compiles");

    let mut out = Vec::new();
    // Iterate by enclosing fn span: for each fn-decl start, find the
    // body span via brace-matching, scan that body for close + revoke
    // signals.
    for caps in fn_re.captures_iter(source) {
        let name = caps.name("name").unwrap().as_str();
        let m = caps.get(0).unwrap();
        if is_test_fn_name(name) {
            continue;
        }
        let in_close_file = file_name_is_close(rel_file);
        let fn_is_close = name_is_close_shape(name);
        if !in_close_file && !fn_is_close {
            continue;
        }
        let Some(body) = body_after(source, m.end()) else {
            continue;
        };
        // Find the close target. Patterns:
        //   ProgramAccount::close(<target>, ...)
        //   <Type>::close(<target>, ...)
        //   close_account(<target>, ...)
        //   <target>.close(...)
        for cc in close_re.captures_iter(&body) {
            let target_raw = cc
                .name("a")
                .or_else(|| cc.name("b"))
                .map(|m| m.as_str())
                .unwrap_or("");
            let target = normalize_target(target_raw);
            if target.is_empty() {
                continue;
            }
            // body-relative byte offset → absolute → line.
            let body_offset_in_src = source.find(&body[..]).unwrap_or(0);
            let abs = body_offset_in_src + cc.get(0).unwrap().start();
            let line = byte_offset_to_line(source, abs);
            let has_revoke = body_signals_revoke(&body);
            out.push(CloseSite {
                rel_file: rel_file.to_path_buf(),
                line,
                fn_name: name.to_string(),
                closed_account: target,
                has_revoke,
            });
        }
    }
    out
}

/// True when the body contains a revoke-shape CPI: `Revoke` /
/// `RevokeSpl` / `Revoke2022` / `revoke(`, OR a `SetAuthority` that
/// reads `new_authority: None`. These signal the close handler
/// properly tears down the authority.
fn body_signals_revoke(body: &str) -> bool {
    if body.contains("Revoke ")
        || body.contains("Revoke{")
        || body.contains("Revoke {")
        || body.contains("RevokeSpl")
        || body.contains("Revoke2022")
        || body.contains(".revoke(")
        || body.contains("::revoke(")
        || body.contains("revoke(")
    {
        return true;
    }
    if body.contains("SetAuthority") && body.contains("new_authority: None") {
        return true;
    }
    false
}

fn file_name_is_close(rel_file: &Path) -> bool {
    let Some(name) = rel_file.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    name.starts_with("close_") || name.starts_with("revoke_") || name.starts_with("terminate_")
}

fn name_is_close_shape(fn_name: &str) -> bool {
    let lower = fn_name.to_ascii_lowercase();
    lower.contains("close") || lower.contains("revoke") || lower.contains("terminate")
}

/// Normalise an account expression for cross-stage matching. Strips
/// common ornamentation (`accounts.`, `.address()`, `.key`, `&`,
/// trailing whitespace) so `accounts.subscription_authority` and
/// `subscription_authority` and `accounts.subscription_authority.address()`
/// all canonicalise to `subscription_authority`.
fn normalize_target(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    s = s.trim_start_matches('&').trim().to_string();
    s = s.trim_start_matches("accounts.").to_string();
    s = s.trim_start_matches("ctx.accounts.").to_string();
    // Drop trailing method calls / accessors: `.address()`, `.key`,
    // `.to_account_info()`.
    for suffix in [".address()", ".key()", ".key", ".to_account_info()", "()"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_string();
        }
    }
    s.trim().to_string()
}

fn emit_findings(grants: &[AuthorityGrant], closes: &[CloseSite]) -> Vec<Finding> {
    let granted_accounts: BTreeSet<String> =
        grants.iter().map(|g| g.target_account.clone()).collect();
    let mut out = Vec::new();
    for close in closes {
        if close.has_revoke {
            continue;
        }
        if !granted_accounts.contains(&close.closed_account) {
            continue;
        }
        // Resolve the grant site(s) that match for the narrative.
        let matching_grants: Vec<&AuthorityGrant> = grants
            .iter()
            .filter(|g| g.target_account == close.closed_account)
            .collect();
        let grant_narrative = matching_grants
            .iter()
            .map(|g| format!("{} at {}:{}", g.operator, g.rel_file.display(), g.line))
            .collect::<Vec<_>>()
            .join("; ");
        let finding_id = make_id(&close.rel_file, close.line, &close.closed_account);

        let mut subs = std::collections::BTreeMap::new();
        subs.insert("CLOSED_ACCOUNT".to_string(), close.closed_account.clone());
        subs.insert(
            "CLOSE_FILE".to_string(),
            close.rel_file.display().to_string(),
        );
        subs.insert("CLOSE_LINE".to_string(), close.line.to_string());
        subs.insert("CLOSE_FN".to_string(), close.fn_name.clone());
        subs.insert("GRANT_SITES".to_string(), grant_narrative.clone());

        out.push(Finding {
            id: finding_id.clone(),
            category: Category::ExternalAuthorityNotRevokedOnClose,
            severity: Severity::Medium,
            handler: close.fn_name.clone(),
            spec_silent_on: format!(
                "Handler `{}` at {}:{} closes `{}` but the program previously \
                 conferred external authority on it ({}). The closed PDA is \
                 still registered as an active delegate / authority on the \
                 external account, visible to wallets and downstream \
                 programs as live permission.",
                close.fn_name,
                close.rel_file.display(),
                close.line,
                close.closed_account,
                grant_narrative
            ),
            suppression_hint: format!(
                "Issue the reverse CPI alongside the close: `Revoke` / \
                 `Revoke2022` for an SPL Approve delegate, `SetAuthority {{ \
                 new_authority: None, ... }}` for a mint / freeze authority, \
                 or `Assign {{ new_owner: SYSTEM_PROGRAM_ID, ... }}` for an \
                 ownership grant. The reverse CPI must succeed BEFORE the \
                 close primitive so the external account no longer points \
                 at the now-defunct PDA. (Alternative: re-init the closed \
                 PDA on the same seeds — applicable when `{}` is paired \
                 with an init handler that reuses the address.)",
                close.closed_account
            ),
            investigation_hint: format!(
                "Walk every transaction that the close handler `{}` is \
                 reachable through. Confirm whether the external authority \
                 is preserved across the close (a re-init path or a \
                 same-seeds replay) or left dangling. Wallet UIs query \
                 SPL Token's delegate field directly — a dangling delegate \
                 is visible as 'this address can still spend my tokens' even \
                 after the program-owned PDA is closed.",
                close.fn_name
            ),
            category_tag: "external_authority_not_revoked_on_close".to_string(),
            reproducer: Some(Reproducer::MolluskPrompt {
                template_path:
                    "references/probes/lifecycle/external_authority_not_revoked_on_close.md#reproducer"
                        .to_string(),
                substitutions: subs,
                repro_path: format!(".qed/probes/lifecycle/{finding_id}/repro.rs"),
            }),
            gated_by: None,
        });
    }
    out
}

// ─────────────────────────────────────────────────────────────────────
// Shared helpers (duplicated from arithmetic_symbol_probe /
// paired_validator_probe; v2.23 may factor them out).
// ─────────────────────────────────────────────────────────────────────

fn byte_offset_to_line(source: &str, offset: usize) -> u32 {
    let prefix = &source[..offset.min(source.len())];
    1 + prefix.chars().filter(|c| *c == '\n').count() as u32
}

fn is_test_fn_name(fn_name: &str) -> bool {
    let lower = fn_name.to_ascii_lowercase();
    lower.starts_with("test_")
        || lower.starts_with("it_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
}

fn line_is_commented(source: &str, offset: usize) -> bool {
    let bytes = source.as_bytes();
    let mut i = offset.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    let line_prefix = &source[i..offset.min(source.len())];
    if let Some(idx) = line_prefix.find("//") {
        let before = &line_prefix[..idx];
        let quote_count = before.chars().filter(|c| *c == '"').count();
        quote_count % 2 == 0
    } else {
        false
    }
}

fn make_id(rel_file: &Path, line: u32, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(rel_file.display().to_string().as_bytes());
    h.update(b":");
    h.update(line.to_string().as_bytes());
    h.update(b":lifecycle_close:");
    h.update(key.as_bytes());
    let id = format!("{:x}", h.finalize());
    id[..16].to_string()
}

/// Body-after-fn-decl extraction. Walks forward to the first `{`,
/// then brace-tracks to the matching `}`.
fn body_after(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let body_start = i + 1;
    let mut depth: i32 = 1;
    let mut j = body_start;
    while j < bytes.len() && depth > 0 {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(source[body_start..j].to_string());
        }
        j += 1;
    }
    Some(source[body_start..].to_string())
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
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| matches!(n, "target" | ".git" | "node_modules" | "tests" | ".qed"))
        {
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

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn fires_on_canonical_subscriptions_qed_head_med_3_shape() {
        // Stage A signal: Approve2022 / ApproveSpl conferring authority
        // on `subscription_authority`.
        let init_src = r#"
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    Approve2022 {
        token_program: accounts.token_program.address(),
        source: accounts.user_ata,
        delegate: accounts.subscription_authority,
        authority: accounts.user,
        amount: u64::MAX,
    }
    .invoke()?;
    Ok(())
}
"#;
        // Stage B signal: close handler with no Revoke.
        let close_src = r#"
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    ProgramAccount::close(accounts.subscription_authority, accounts.user)
}
"#;
        let grants = scan_authority_grants(&p("initialize_subscription_authority.rs"), init_src);
        let closes = scan_close_sites(&p("close_subscription_authority.rs"), close_src);
        assert!(!grants.is_empty(), "Stage A should detect Approve2022");
        assert!(
            !closes.is_empty(),
            "Stage B should detect ProgramAccount::close"
        );
        assert_eq!(grants[0].target_account, "subscription_authority");
        assert_eq!(closes[0].closed_account, "subscription_authority");
        assert!(!closes[0].has_revoke);
        let findings = emit_findings(&grants, &closes);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.category_tag, "external_authority_not_revoked_on_close");
        assert!(matches!(f.severity, Severity::Medium));
    }

    #[test]
    fn suppresses_close_handler_with_revoke_cpi() {
        let init_src = r#"
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    Approve {
        source: accounts.user_ata,
        delegate: accounts.subscription_authority,
        authority: accounts.user,
        amount: 100,
    }
    .invoke()?;
    Ok(())
}
"#;
        let close_src = r#"
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    Revoke {
        source: accounts.user_ata,
        authority: accounts.user,
    }
    .invoke()?;
    ProgramAccount::close(accounts.subscription_authority, accounts.user)
}
"#;
        let grants = scan_authority_grants(&p("initialize.rs"), init_src);
        let closes = scan_close_sites(&p("close_subscription_authority.rs"), close_src);
        assert!(closes[0].has_revoke);
        let findings = emit_findings(&grants, &closes);
        assert!(
            findings.is_empty(),
            "close handler with Revoke should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_close_handler_without_matching_grant() {
        // Close handler is fine but no upstream Approve targeted the
        // closed account.
        let close_src = r#"
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    ProgramAccount::close(accounts.escrow_pda, accounts.user)
}
"#;
        let grants: Vec<AuthorityGrant> = Vec::new();
        let closes = scan_close_sites(&p("close_escrow.rs"), close_src);
        let findings = emit_findings(&grants, &closes);
        assert!(
            findings.is_empty(),
            "no grant = no finding, got {findings:#?}"
        );
    }

    #[test]
    fn set_authority_records_new_authority_not_current() {
        // SetAuthority's `authority` field is the *current* owner; the
        // grant target is `new_authority`. The probe must pick the
        // right field.
        let src = r#"
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    SetAuthority {
        mint: accounts.mint,
        authority: accounts.user,
        new_authority: accounts.escrow_pda,
        authority_type: AuthorityType::MintTokens,
    }
    .invoke()?;
    Ok(())
}
"#;
        let grants = scan_authority_grants(&p("init.rs"), src);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].target_account, "escrow_pda");
        assert_eq!(grants[0].operator, "SetAuthority");
    }

    #[test]
    fn normalize_target_strips_accessors() {
        assert_eq!(
            normalize_target("accounts.subscription_authority"),
            "subscription_authority"
        );
        assert_eq!(normalize_target("&accounts.escrow_pda"), "escrow_pda");
        assert_eq!(
            normalize_target("accounts.subscription_authority.address()"),
            "subscription_authority"
        );
        assert_eq!(normalize_target("ctx.accounts.vault"), "vault");
    }
}
