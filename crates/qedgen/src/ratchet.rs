// The `readiness` and `check-upgrade` subcommands close the last gap in
// the qedgen pipeline: proofs prove the handlers are semantically
// correct, but they don't say anything about whether the program's
// on-chain shape is safe to deploy — or safe to evolve after deploy.
// That's what ratchet (https://github.com/saicharanpogul/ratchet) does.
//
// qedgen embeds ratchet as a library rather than shelling out to
// `solana-ratchet-cli`:
//   - single binary for end-users (no extra install step),
//   - stable version coupling (lockfile pins the ratchet version),
//   - panic-free happy path (no subprocess / PATH surprises).
//
// Scope split — readiness runs the preflight P-rules on one IDL;
// check-upgrade runs the diff R-rules on two IDLs. Exit codes mirror
// ratchet's CLI conventions so CI scripts can switch between the two
// entry points without rewriting their signal handling:
//   0 = only additive findings (safe), 1 = breaking, 2 = unsafe,
//   3 = qedgen-level error before the engine ran (caller-side).

use anyhow::{Context, Result};
use ratchet_anchor::{normalize as normalize_anchor, AnchorIdl};
use ratchet_core::{
    check, default_preflight_rules, default_rules, preflight, CheckContext, ProgramSurface, Report,
    Severity,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Which framework's IDL the caller is handing us. Picks the loader +
/// normaliser path; the rule engine downstream is identical either
/// way (every R-rule and P-rule operates on the framework-agnostic
/// `ProgramSurface` IR).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Framework {
    #[default]
    Anchor,
    Quasar,
}

impl Framework {
    /// Walk the current working directory and its ancestors looking for
    /// a project marker — `Anchor.toml` or `Quasar.toml` — and pick the
    /// framework matching the first level that has either. Mirrors the
    /// way Cargo locates a workspace root, so devs running `qedgen` from
    /// a nested subdirectory still get the right autodetect. Mixed
    /// directories (both markers at the same level) pick Anchor to
    /// preserve historical behaviour.
    ///
    /// Falls back to Anchor when no marker is found anywhere up the
    /// chain (or when `current_dir()` itself fails). Used only as a
    /// default — explicit `--quasar` always wins.
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
    /// Framework for both `old` and `new`. Mixed-framework diffs
    /// aren't supported — Anchor + Quasar IDLs lower into the same
    /// IR but the loaders differ, and a "what does it mean to
    /// rename a program from Anchor to Quasar" diff is out of scope.
    pub framework: Framework,
}

/// Run the preflight rule set against a single IDL (Anchor or
/// Quasar) and return the resulting [`Report`].
pub fn run_readiness(opts: &ReadinessOpts) -> Result<Report> {
    let surface = load_surface(&opts.idl, opts.framework)?;
    let ctx = CheckContext::new();
    let rules = default_preflight_rules();
    Ok(preflight(&surface, &ctx, &rules))
}

/// Diff two IDLs under the default rule set. Allow-flags flow
/// through to the engine so intentional unsafe changes can be
/// acknowledged at the CLI boundary.
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

/// Load + normalise an IDL JSON for either framework. Both lower
/// into the same `ProgramSurface` so every rule downstream is
/// identical.
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

// Human report → stderr, JSON report → stdout. Mirrors upstream
// ratchet's CLI so agents / shell consumers can redirect `>report.json`
// for machine parsing without swallowing the human-readable banner, and
// CI logs show the verdict inline while an optional `--json` capture
// stays separable.
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

/// One row in a `--list-rules` dump. Kept minimal — id / name /
/// description are the only fields the upstream ratchet CLI prints
/// and the only fields stable across rule catalog revisions.
#[derive(Serialize)]
struct RuleEntry {
    id: &'static str,
    name: &'static str,
    description: &'static str,
}

/// Print every preflight (`P001`–`P006`) rule shipped with the
/// embedded ratchet. `--json` switches to a machine-parseable payload
/// (stdout) so agents can consume the catalog without regex-matching
/// the human table.
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

/// Print every diff (`R001`–`R016`) rule shipped with the embedded
/// ratchet. Same JSON / human split as
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

    // Catalog-shape guards for `--list-rules`. These tests snapshot the
    // `id` / `name` / `description` tuples the embedded ratchet exposes;
    // if an upstream rule is renamed without updating the P / R ID
    // prefixes, the contains-check catches it. The counts are pinned at
    // the v0.3.1 catalog (6 P-rules + 16 R-rules = 22); bump here when
    // the upstream catalog grows.
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
        // Spot-check a few key rule ids — these are the ones the PR body
        // and README reference by number, so they must stay discoverable.
        let ids: Vec<&str> = entries.iter().map(|(id, _)| *id).collect();
        for expected in &["R001", "R006", "R007", "R013", "R016"] {
            assert!(ids.contains(expected), "missing {expected} in catalog");
        }
    }

    // --- Quasar dispatch -------------------------------------------------
    //
    // Quasar's IDL JSON is a different shape from Anchor's: 1-byte
    // variable-length account discriminators, an untagged `IdlType`
    // union, and struct-only typedefs joined to accounts by name.
    // ratchet-quasar handles the lowering; these tests just prove that
    // dispatching through `Framework::Quasar` reaches that codepath and
    // that the same R/P rule engine fires on the resulting surface.

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
        // Same readiness gaps as the Anchor case — no `version` prefix,
        // no `_reserved` padding. P003/P004 are intentionally silenced
        // for Quasar (devs always pin discriminators in source, so the
        // sha256-default rules are a category error).
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
        // Anchor and Quasar both wrap struct typedefs in
        // `type: { kind: "struct", fields: ... }`, but the top-level
        // shape diverges: Quasar requires a top-level `address` field
        // (Anchor's program id lives under `metadata` instead). Feeding
        // `BARE_V1_IDL` (Anchor-shaped, no top-level `address`) through
        // `Framework::Quasar` therefore fails serde with `missing field
        // \`address\``. Confirms the dispatch is wired to the Quasar
        // parser rather than silently falling back to Anchor.
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
