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
use ratchet_anchor::{normalize, AnchorIdl};
use ratchet_core::{
    check, default_preflight_rules, default_rules, preflight, CheckContext, Report, Severity,
};
use std::path::{Path, PathBuf};

/// Options accepted by the `qedgen readiness` subcommand.
pub struct ReadinessOpts {
    pub idl: PathBuf,
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
}

/// Run the preflight rule set against a single Anchor IDL and return
/// the resulting [`Report`].
pub fn run_readiness(opts: &ReadinessOpts) -> Result<Report> {
    let idl = load_idl(&opts.idl)?;
    let surface =
        normalize(&idl).with_context(|| format!("normalizing IDL at {}", opts.idl.display()))?;
    let ctx = CheckContext::new();
    let rules = default_preflight_rules();
    Ok(preflight(&surface, &ctx, &rules))
}

/// Diff two Anchor IDLs under the default rule set. Allow-flags flow
/// through to the engine so intentional unsafe changes can be
/// acknowledged at the CLI boundary.
pub fn run_check_upgrade(opts: &CheckUpgradeOpts) -> Result<Report> {
    let old = load_idl(&opts.old)?;
    let new = load_idl(&opts.new)?;
    let old_surface = normalize(&old)
        .with_context(|| format!("normalizing old IDL at {}", opts.old.display()))?;
    let new_surface = normalize(&new)
        .with_context(|| format!("normalizing new IDL at {}", opts.new.display()))?;

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

fn load_idl(path: &Path) -> Result<AnchorIdl> {
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
        let report = run_readiness(&ReadinessOpts { idl }).unwrap();
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
        let report = run_readiness(&ReadinessOpts { idl }).unwrap();
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
        })
        .unwrap();
        assert!(report.findings.iter().any(|f| f.rule_id == "R007"));
        assert_eq!(exit_code(&report), 1);
    }

    #[test]
    fn missing_idl_is_surfaced_as_io_error() {
        let err = run_readiness(&ReadinessOpts {
            idl: PathBuf::from("/does/not/exist.json"),
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("reading"));
    }

    #[test]
    fn malformed_json_is_surfaced_as_parse_error() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", "not json");
        let err = run_readiness(&ReadinessOpts { idl }).unwrap_err();
        assert!(format!("{err:#}").contains("parsing"));
    }
}
