// `readiness` / `check-upgrade`: on-chain shape safety via ratchet
// (https://github.com/saicharanpogul/ratchet), embedded as a library (not
// shelled out) for single-binary install, lockfile-pinned versioning, and
// no subprocess/PATH surprises.
//
// readiness = preflight P-rules on one IDL; check-upgrade = diff R-rules on
// two. Exit codes mirror ratchet's CLI so CI scripts port unchanged:
//   0 = additive/safe, 1 = breaking, 2 = unsafe,
//   3 = qedgen-level error before the engine ran.

use anyhow::{Context, Result};
use ratchet_anchor::{normalize as normalize_anchor, AnchorIdl};
use ratchet_core::{
    check, default_preflight_rules, default_rules, preflight, CheckContext, ProgramSurface, Report,
    Severity,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// IDL framework. Picks the loader + normaliser; the rule engine is identical
/// (all rules operate on the framework-agnostic `ProgramSurface` IR).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    #[default]
    Anchor,
    Quasar,
}

impl Framework {
    /// Walk cwd ancestors for `Anchor.toml` / `Quasar.toml` (Cargo-style
    /// workspace discovery). Both markers at the same level → Anchor;
    /// no marker or `current_dir()` failure → Anchor. Default only —
    /// explicit `--quasar` always wins.
    pub fn detect_from_cwd() -> Self {
        let Ok(start) = std::env::current_dir() else {
            return Framework::Anchor;
        };
        for dir in start.ancestors() {
            let has_quasar = dir.join("Quasar.toml").exists();
            let has_anchor = dir.join("Anchor.toml").exists();
            match (has_quasar, has_anchor) {
                (true, false) => return Framework::Quasar,
                (_, true) => return Framework::Anchor,
                _ => continue,
            }
        }
        Framework::Anchor
    }
}

/// Options accepted by the `qedgen readiness` subcommand.
pub struct ReadinessOpts {
    pub idl: PathBuf,
    pub framework: Framework,
}

/// Options accepted by the `qedgen check-upgrade` subcommand.
pub struct CheckUpgradeOpts {
    pub old: PathBuf,
    pub new: PathBuf,
    /// `--unsafe <flag>` acknowledgements (see `ratchet list-rules`).
    pub unsafes: Vec<String>,
    /// `--migrated-account <Name>` declarations for R003/R004 demotion.
    pub migrated_accounts: Vec<String>,
    /// `--realloc-account <Name>` declarations for R005 demotion.
    pub realloc_accounts: Vec<String>,
    /// Framework for both `old` and `new`; mixed-framework diffs unsupported.
    pub framework: Framework,
}

/// Run the preflight rule set against a single IDL.
pub fn run_readiness(opts: &ReadinessOpts) -> Result<Report> {
    let surface = load_surface(&opts.idl, opts.framework)?;
    let ctx = CheckContext::new();
    let rules = default_preflight_rules();
    Ok(preflight(&surface, &ctx, &rules))
}

/// Diff two IDLs under the default rule set; allow-flags flow through so
/// intentional unsafe changes can be acknowledged at the CLI boundary.
pub fn run_check_upgrade(opts: &CheckUpgradeOpts) -> Result<Report> {
    let old_surface = load_surface(&opts.old, opts.framework)?;
    let new_surface = load_surface(&opts.new, opts.framework)?;

    let mut ctx = CheckContext::new();
    for flag in &opts.unsafes {
        ctx = ctx.with_allow(flag);
    }
    for name in &opts.migrated_accounts {
        ctx = ctx.with_migration(name);
    }
    for name in &opts.realloc_accounts {
        ctx = ctx.with_realloc(name);
    }

    let rules = default_rules();
    Ok(check(&old_surface, &new_surface, &ctx, &rules))
}

/// Load + normalise an IDL JSON; both frameworks lower into `ProgramSurface`.
fn load_surface(path: &Path, framework: Framework) -> Result<ProgramSurface> {
    match framework {
        Framework::Anchor => {
            let idl = load_anchor_idl(path)?;
            normalize_anchor(&idl)
                .with_context(|| format!("normalizing Anchor IDL at {}", path.display()))
        }
        Framework::Quasar => {
            let idl = ratchet_quasar::load_quasar_idl(path)
                .with_context(|| format!("loading Quasar IDL at {}", path.display()))?;
            ratchet_quasar::normalize(&idl)
                .with_context(|| format!("normalizing Quasar IDL at {}", path.display()))
        }
    }
}

fn load_anchor_idl(path: &Path) -> Result<AnchorIdl> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice::<AnchorIdl>(&bytes)
        .with_context(|| format!("parsing Anchor IDL at {}", path.display()))
}

/// Map a report's highest severity to ratchet's CLI exit-code convention.
/// A report with no findings is treated as additive/safe.
pub fn exit_code(report: &Report) -> i32 {
    match report.max_severity() {
        None | Some(Severity::Additive) => 0,
        Some(Severity::Breaking) => 1,
        Some(Severity::Unsafe) => 2,
    }
}

// Human report → stderr, JSON → stdout (mirrors upstream ratchet), so
// `>report.json` captures machine output without swallowing the banner.
pub fn print_human(report: &Report) {
    if report.findings.is_empty() {
        eprintln!("READY — no findings.");
        return;
    }
    for f in &report.findings {
        let sev = match f.severity {
            Severity::Breaking => "BREAKING",
            Severity::Unsafe => "UNSAFE  ",
            Severity::Additive => "additive",
        };
        eprintln!(
            "{}  {}  {}  {}",
            sev,
            f.rule_id,
            f.rule_name,
            f.path.join("/")
        );
        eprintln!("          {}", f.message);
        if let Some(hint) = &f.suggestion {
            eprintln!("          hint: {}", hint);
        }
        if let Some(flag) = &f.allow_flag {
            eprintln!("          (acknowledge with --unsafe {})", flag);
        }
    }
    eprintln!();
    match report.max_severity() {
        None | Some(Severity::Additive) => eprintln!("verdict: READY"),
        Some(Severity::Breaking) => eprintln!("verdict: BREAKING"),
        Some(Severity::Unsafe) => eprintln!("verdict: UNSAFE — review each finding"),
    }
}

pub fn print_json(report: &Report) -> Result<()> {
    let s = serde_json::to_string_pretty(report).context("serializing ratchet report")?;
    println!("{}", s);
    Ok(())
}

/// One `--list-rules` row. id / name / description are the only fields stable
/// across rule catalog revisions.
#[derive(Serialize)]
struct RuleEntry {
    id: &'static str,
    name: &'static str,
    description: &'static str,
}

/// Print the embedded preflight (P-rule) catalog; `--json` emits a
/// machine-parseable payload on stdout.
pub fn print_rules_preflight(json: bool) -> Result<()> {
    let entries: Vec<RuleEntry> = default_preflight_rules()
        .iter()
        .map(|r| RuleEntry {
            id: r.id(),
            name: r.name(),
            description: r.description(),
        })
        .collect();
    render_rule_catalog("readiness (preflight, P-rules)", &entries, json)
}

/// Print the embedded diff (R-rule) catalog; same JSON / human split as
/// [`print_rules_preflight`].
pub fn print_rules_diff(json: bool) -> Result<()> {
    let entries: Vec<RuleEntry> = default_rules()
        .iter()
        .map(|r| RuleEntry {
            id: r.id(),
            name: r.name(),
            description: r.description(),
        })
        .collect();
    render_rule_catalog("check-upgrade (diff, R-rules)", &entries, json)
}

fn render_rule_catalog(header: &str, entries: &[RuleEntry], json: bool) -> Result<()> {
    if json {
        let s = serde_json::to_string_pretty(entries).context("serializing rule catalog")?;
        println!("{}", s);
        return Ok(());
    }
    eprintln!("qedgen {} — {} rule(s):", header, entries.len());
    for entry in entries {
        eprintln!("  {}  {:<40}  {}", entry.id, entry.name, entry.description);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const BARE_V1_IDL: &str = r#"{
        "metadata": { "name": "t" },
        "instructions": [],
        "accounts": [
            { "name": "State", "discriminator": [1,2,3,4,5,6,7,8] }
        ],
        "types": [
            {
                "name": "State",
                "type": {
                    "kind": "struct",
                    "fields": [{ "name": "balance", "type": "u64" }]
                }
            }
        ]
    }"#;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn readiness_flags_bare_v1_surface() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", BARE_V1_IDL);
        let report = run_readiness(&ReadinessOpts {
            idl,
            framework: Framework::Anchor,
        })
        .unwrap();
        let ids: Vec<&str> = report.findings.iter().map(|f| f.rule_id.as_str()).collect();
        // Known bare-surface traps: no `version` prefix, no `_reserved`
        // padding, struct name collides with the account name.
        assert!(ids.contains(&"P001"));
        assert!(ids.contains(&"P002"));
        assert!(ids.contains(&"P005"));
    }

    #[test]
    fn readiness_exit_code_matches_severity() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", BARE_V1_IDL);
        let report = run_readiness(&ReadinessOpts {
            idl,
            framework: Framework::Anchor,
        })
        .unwrap();
        // Bare v1 surface fires P001 (unsafe) and P002 (unsafe); exit 2.
        assert_eq!(exit_code(&report), 2);
    }

    #[test]
    fn check_upgrade_identical_idls_produce_no_findings() {
        let tmp = TempDir::new().unwrap();
        let old = write(tmp.path(), "old.json", BARE_V1_IDL);
        let new = write(tmp.path(), "new.json", BARE_V1_IDL);
        let report = run_check_upgrade(&CheckUpgradeOpts {
            old,
            new,
            unsafes: vec![],
            migrated_accounts: vec![],
            realloc_accounts: vec![],
            framework: Framework::Anchor,
        })
        .unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(exit_code(&report), 0);
    }

    #[test]
    fn check_upgrade_breaking_change_fires_rule() {
        let tmp = TempDir::new().unwrap();
        let old = write(
            tmp.path(),
            "old.json",
            r#"{
                "metadata": { "name": "t" },
                "instructions": [
                    { "name": "old_ix", "discriminator": [1,2,3,4,5,6,7,8], "accounts": [], "args": [] }
                ],
                "accounts": [],
                "types": []
            }"#,
        );
        let new = write(
            tmp.path(),
            "new.json",
            r#"{
                "metadata": { "name": "t" },
                "instructions": [],
                "accounts": [],
                "types": []
            }"#,
        );
        let report = run_check_upgrade(&CheckUpgradeOpts {
            old,
            new,
            unsafes: vec![],
            migrated_accounts: vec![],
            realloc_accounts: vec![],
            framework: Framework::Anchor,
        })
        .unwrap();
        assert!(report.findings.iter().any(|f| f.rule_id == "R007"));
        assert_eq!(exit_code(&report), 1);
    }

    #[test]
    fn missing_idl_is_surfaced_as_io_error() {
        let err = run_readiness(&ReadinessOpts {
            idl: PathBuf::from("/does/not/exist.json"),
            framework: Framework::Anchor,
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("reading"));
    }

    #[test]
    fn malformed_json_is_surfaced_as_parse_error() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", "not json");
        let err = run_readiness(&ReadinessOpts {
            idl,
            framework: Framework::Anchor,
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("parsing"));
    }

    // Catalog-shape guards for `--list-rules`: counts pinned at the v0.3.1
    // catalog (6 P + 16 R); bump when the upstream catalog grows.
    #[test]
    fn list_rules_preflight_covers_full_p_catalog() {
        let entries: Vec<_> = default_preflight_rules()
            .iter()
            .map(|r| (r.id(), r.name()))
            .collect();
        assert_eq!(entries.len(), 6);
        assert!(entries.iter().all(|(id, _)| id.starts_with('P')));
        let ids: Vec<&str> = entries.iter().map(|(id, _)| *id).collect();
        for expected in &["P001", "P002", "P003", "P004", "P005", "P006"] {
            assert!(ids.contains(expected), "missing {expected} in catalog");
        }
    }

    #[test]
    fn list_rules_diff_covers_full_r_catalog() {
        let entries: Vec<_> = default_rules().iter().map(|r| (r.id(), r.name())).collect();
        assert_eq!(entries.len(), 16);
        assert!(entries.iter().all(|(id, _)| id.starts_with('R')));
        // Spot-check rule ids referenced by number in docs.
        let ids: Vec<&str> = entries.iter().map(|(id, _)| *id).collect();
        for expected in &["R001", "R006", "R007", "R013", "R016"] {
            assert!(ids.contains(expected), "missing {expected} in catalog");
        }
    }

    // --- Quasar dispatch -------------------------------------------------
    //
    // Quasar IDL JSON differs from Anchor's (1-byte variable-length
    // discriminators, untagged `IdlType` union, struct-only typedefs).
    // These tests prove `Framework::Quasar` reaches ratchet-quasar's loader
    // and the same rule engine fires on the resulting surface.

    const QUASAR_BARE_V1_IDL: &str = r#"{
        "address": "11111111111111111111111111111111",
        "metadata": { "name": "t", "version": "0.1.0", "spec": "0.1.0" },
        "instructions": [],
        "accounts": [
            { "name": "State", "discriminator": [42] }
        ],
        "types": [
            {
                "name": "State",
                "type": {
                    "kind": "struct",
                    "fields": [{ "name": "balance", "type": "u64" }]
                }
            }
        ]
    }"#;

    #[test]
    fn quasar_readiness_flags_bare_v1_surface() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", QUASAR_BARE_V1_IDL);
        let report = run_readiness(&ReadinessOpts {
            idl,
            framework: Framework::Quasar,
        })
        .unwrap();
        let ids: Vec<&str> = report.findings.iter().map(|f| f.rule_id.as_str()).collect();
        // Same gaps as Anchor; P003/P004 are intentionally silenced for Quasar
        // (discriminators are always source-pinned, so the sha256-default
        // rules are a category error).
        assert!(ids.contains(&"P001"));
        assert!(ids.contains(&"P002"));
        assert!(!ids.contains(&"P003"));
        assert!(!ids.contains(&"P004"));
    }

    #[test]
    fn quasar_check_upgrade_catches_discriminator_change() {
        let tmp = TempDir::new().unwrap();
        let old = write(tmp.path(), "old.json", QUASAR_BARE_V1_IDL);
        let new = write(
            tmp.path(),
            "new.json",
            // Same shape but discriminator flipped 42 → 99.
            &QUASAR_BARE_V1_IDL.replace("\"discriminator\": [42]", "\"discriminator\": [99]"),
        );
        let report = run_check_upgrade(&CheckUpgradeOpts {
            old,
            new,
            unsafes: vec![],
            migrated_accounts: vec![],
            realloc_accounts: vec![],
            framework: Framework::Quasar,
        })
        .unwrap();
        assert!(
            report.findings.iter().any(|f| f.rule_id == "R006"),
            "expected R006 account-discriminator-change, got {:?}",
            report
                .findings
                .iter()
                .map(|f| &f.rule_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(exit_code(&report), 1);
    }

    #[test]
    fn quasar_anchor_idl_under_quasar_mode_is_a_parse_error() {
        // Quasar requires a top-level `address` field Anchor IDLs lack, so an
        // Anchor-shaped IDL fails serde under Framework::Quasar — proving the
        // dispatch doesn't silently fall back to the Anchor parser.
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", BARE_V1_IDL);
        let err = run_readiness(&ReadinessOpts {
            idl,
            framework: Framework::Quasar,
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("parsing"));
    }
}
