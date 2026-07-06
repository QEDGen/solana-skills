//! Lifecycle external-state probe.
//!
//! Detects close-handler / authority-grant asymmetry: a handler closes a
//! PDA holding external authority (SPL Approve delegate, mint authority,
//! Assign owner) without the reverse CPI (`Revoke`, `SetAuthority: None`,
//! re-`Assign`), leaving the closed PDA registered as live permission on
//! the external account. Two stages: A — record accounts granted authority
//! via `Approve*` / `SetAuthority` / `Assign` CPIs; B — emit MEDIUM when a
//! close-shape handler closes a Stage-A account without a revoke-shape CPI
//! in its body. False-positive guards: close handlers containing a reverse
//! CPI are suppressed; test fns are filtered.

use anyhow::Result;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::probe::scan_util::{
    body_after, byte_offset_to_line, is_test_fn_name, line_is_commented, make_id,
};
use crate::probe::{Category, Finding, Reproducer, Severity};

#[derive(Debug, Clone)]
struct AuthorityGrant {
    rel_file: PathBuf,
    line: u32,
    /// Ident receiving the authority (`delegate:` / `new_authority:` RHS,
    /// normalized via `normalize_target`).
    target_account: String,
    /// Grant operator name (`Approve`, `SetAuthority`, ...) for the
    /// reproducer narrative.
    operator: String,
}

#[derive(Debug, Clone)]
struct CloseSite {
    rel_file: PathBuf,
    line: u32,
    fn_name: String,
    /// Account being closed (first arg of the close call, normalized
    /// same way as Stage A targets).
    closed_account: String,
    /// Enclosing fn body contains a revoke-shape CPI — the close path
    /// properly tears down the authority.
    has_revoke: bool,
}

/// Entry point: walk `<root>/src/**/*.rs`, collect grants + close
/// sites, then cross-match.
pub fn scan_program(project_root: &Path) -> Result<Vec<Finding>> {
    let src_dir = project_root.join("src");
    // Program crates may live under `program/src/` instead of the root
    // `src/`; neither existing means nothing to scan.
    let scan_root = if src_dir.exists() {
        src_dir
    } else if project_root.join("program").join("src").exists() {
        project_root.join("program").join("src")
    } else {
        return Ok(Vec::new());
    };
    let rs_files = crate::fs_walk::collect_rs_files(&scan_root, crate::fs_walk::DEFAULT_SKIP_DIRS);
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

/// Stage A: extract authority-conferring CPI struct literals —
/// `Approve(2022|Spl)? { delegate: <X> }`, `SetAuthority { new_authority:
/// <X> }`, `Assign { (new_)owner: <X> }` — recording the account that
/// received the authority.
fn scan_authority_grants(rel_file: &Path, source: &str) -> Vec<AuthorityGrant> {
    // Struct-literal bodies can span 5-6 fields; allow up to ~800 chars
    // between the opening `{` and the field we care about.
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
        // Footgun: SetAuthority's `authority` field is the *current*
        // authority, not the new one — only `new_authority` is the grant.
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
            // Approve family: only `delegate` is the receiver (the
            // combined regex also matches `authority`).
            delegate_re.captures(body).and_then(|c| {
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

/// Stage B: find close handlers. Two signals: (a) file name starts with
/// `close_` / `revoke_` / `terminate_` (per-instruction file convention);
/// (b) fn name contains `close` / `revoke` / `terminate` (consolidated
/// lifecycle handlers in one file).
fn scan_close_sites(rel_file: &Path, source: &str) -> Vec<CloseSite> {
    let fn_re =
        Regex::new(r"(?m)^(?:\s*pub(?:\([^)]*\))?\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
            .expect("static regex compiles");
    let close_re = Regex::new(
        r"(?:[A-Za-z_]\w*::close\s*\(\s*(?P<a>[^,)]+)|close_account\s*\(\s*(?P<b>[^,)]+))",
    )
    .expect("static regex compiles");

    let mut out = Vec::new();
    // Per fn decl: brace-match the body, scan it for close + revoke
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
        // Close target = first arg of `<Type>::close(...)` /
        // `close_account(...)`.
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

/// Revoke-shape CPI (`Revoke*` / `revoke(`) or `SetAuthority` with
/// `new_authority: None` — the close handler tears down the authority.
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

/// Normalise an account expression for cross-stage matching: strip
/// `&` / `accounts.` prefixes and accessor suffixes so e.g.
/// `accounts.vault.address()` and `vault` canonicalise the same.
fn normalize_target(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    s = s.trim_start_matches('&').trim().to_string();
    s = s.trim_start_matches("accounts.").to_string();
    s = s.trim_start_matches("ctx.accounts.").to_string();
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
        // Salt frozen at the pre-scan_util literal ("lifecycle_close:<acct>")
        // so existing suppression ids stay valid.
        let finding_id = make_id(
            &close.rel_file,
            close.line,
            &format!("lifecycle_close:{}", close.closed_account),
        );

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
            category_tag: Category::ExternalAuthorityNotRevokedOnClose.tag().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn fires_on_canonical_subscriptions_qed_head_med_3_shape() {
        // Stage A: Approve2022 confers authority on
        // `subscription_authority`.
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
        // Stage B: close handler with no Revoke.
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
