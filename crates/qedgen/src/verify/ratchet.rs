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
    check, default_preflight_rules, default_rules, preflight, CheckContext, Finding,
    ProgramSurface, Report, Severity,
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
    /// IDL for the P-rule preflight. Optional when `so` is set, so a program
    /// without an IDL (Pinocchio, native, sBPF assembly) can still be checked.
    pub idl: Option<PathBuf>,
    /// Built program; its ELF header must say sBPF v3 (#426).
    pub so: Option<PathBuf>,
    pub framework: Framework,
    pub root: Option<PathBuf>,
    /// `--unsafe <flag>` acknowledgements from Ratchet or QEDGen findings.
    pub unsafes: Vec<String>,
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
    /// Optional candidate-project source root for source/IDL reconciliation.
    pub root: Option<PathBuf>,
    /// Candidate built program; its ELF header must say sBPF v3 (#426).
    pub new_so: Option<PathBuf>,
}

/// Run the preflight rule set against a single IDL, plus the sBPF version
/// rule when a built program is given.
pub fn run_readiness(opts: &ReadinessOpts) -> Result<Report> {
    let mut report = match &opts.idl {
        Some(idl) => {
            let surface = load_surface(idl, opts.framework)?;
            let mut ctx = CheckContext::new();
            for flag in &opts.unsafes {
                ctx = ctx.with_allow(flag);
            }
            let rules = default_preflight_rules();
            let mut report = preflight(&surface, &ctx, &rules);
            if let Some(root) = &opts.root {
                apply_source_drift(&mut report, root, idl, opts.framework, &opts.unsafes)?;
            }
            report
        }
        None => {
            if opts.root.is_some() {
                anyhow::bail!("`--root` reconciles source against an IDL; pass `--idl` too");
            }
            if opts.so.is_none() {
                anyhow::bail!("readiness needs `--idl`, `--so`, or both");
            }
            Report::new()
        }
    };
    if let Some(so) = &opts.so {
        apply_sbpf_version(&mut report, so, Acknowledge::Allowed(&opts.unsafes))?;
    }
    Ok(report)
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
    let mut report = check(&old_surface, &new_surface, &ctx, &rules);
    if let Some(root) = &opts.root {
        apply_source_drift(&mut report, root, &opts.new, opts.framework, &opts.unsafes)?;
    }
    if let Some(so) = &opts.new_so {
        // The candidate IS the upgrade, and SIMD-0500 rejects it. There is
        // nothing to acknowledge.
        apply_sbpf_version(&mut report, so, Acknowledge::Never)?;
    }
    Ok(report)
}

/// `readiness --unsafe` value that acknowledges a pre-v3 program, for
/// example a V0 program that is already deployed and will never be upgraded.
/// `check-upgrade` does not accept it: its candidate is the upgrade.
pub const ALLOW_PRE_V3_SBPF: &str = "allow-pre-v3-sbpf";

/// Whether a QED002 finding may be acknowledged.
enum Acknowledge<'a> {
    /// `readiness`: the program may stay deployed as it is.
    Allowed(&'a [String]),
    /// `check-upgrade`: the cluster rejects the upgrade whatever the flags say.
    Never,
}

/// QED002: SIMD-0500 (planned for Agave 4.4) rejects deploys and upgrades of
/// programs older than sBPF v3. A version newer than v3 passes.
fn apply_sbpf_version(report: &mut Report, so: &Path, ack: Acknowledge<'_>) -> Result<()> {
    let version = crate::sbpf_elf::read_sbpf_version(so)?;
    if version >= crate::sbpf_elf::SBPF_V3 {
        return Ok(());
    }
    let acknowledged = match ack {
        Acknowledge::Allowed(flags) => flags.iter().any(|flag| flag == ALLOW_PRE_V3_SBPF),
        Acknowledge::Never => false,
    };
    let severity = if acknowledged {
        Severity::Additive
    } else {
        Severity::Unsafe
    };
    let name = so
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| so.display().to_string());
    let finding = Finding::new(severity, "QED002", "sbpf-version-below-v3")
        .at([format!("so:{name}")])
        .message(format!(
            "program is sBPF v{version} (ELF e_flags = {version}), not v3: once SIMD-0500 \
                 is active, the cluster rejects deploying or upgrading it"
        ))
        .suggestion(
            "Rebuild with `cargo build-sbf --arch v3` (cargo-build-sbf 4.2.0+, \
                 platform-tools v1.56+) or `sbpf build -a v3`.",
        );
    report.push(match ack {
        Acknowledge::Allowed(_) => finding.allow_flag(ALLOW_PRE_V3_SBPF),
        Acknowledge::Never => finding,
    });
    Ok(())
}

fn apply_source_drift(
    report: &mut Report,
    root: &Path,
    idl: &Path,
    framework: Framework,
    acknowledged: &[String],
) -> Result<()> {
    let runtime = match framework {
        Framework::Anchor => crate::probe::Runtime::Anchor,
        Framework::Quasar => crate::probe::Runtime::Quasar,
    };
    let drift = crate::probe::idl_source_drift(root, idl, runtime)?;

    for finding in &mut report.findings {
        let is_idl_only = finding
            .path
            .iter()
            .any(|part| drift.is_idl_only_path_segment(part));
        if is_idl_only {
            finding.severity = Severity::Additive;
            finding.message = format!(
                "stale IDL surface: {}; source discovery found no matching handler",
                finding.message
            );
            finding.suggestion = Some(
                "Rebuild the IDL from current source or restore the missing source handler."
                    .to_string(),
            );
        }
    }

    // A source-only handler has no IDL instruction, so it has no raw IDL name
    // to key on. Its `ix:` segment is therefore the Rust symbol, always
    // snake_case, while sibling Ratchet findings carry whatever casing the IDL
    // used. That asymmetry is intended: the segment names the only thing that
    // exists. The message says so, so a mixed-casing report stays readable.
    for handler in drift.source_only {
        let allow_flag = format!("allow-source-only-{handler}");
        let severity = if acknowledged.iter().any(|flag| flag == &allow_flag) {
            Severity::Additive
        } else {
            Severity::Unsafe
        };
        report.push(
            Finding::new(severity, "QED001", "source-handler-missing-from-idl")
                .at([format!("ix:{handler}")])
                .message(format!(
                    "undeclared surface reaching mainnet: source handler `{handler}` is not declared by the IDL"
                ))
                .suggestion(
                    "Rebuild and review the IDL, or acknowledge an intentional private handler.",
                )
                .allow_flag(allow_flag),
        );
    }

    Ok(())
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

/// QEDGen's own rules, which run beside ratchet's and appear in both
/// catalogs. Each one fires only when its input flag is given.
const QED_RULES: &[RuleEntry] = &[
    RuleEntry {
        id: "QED001",
        name: "source-handler-missing-from-idl",
        description: "With --root: a source handler the IDL does not declare \
                      (acknowledge with allow-source-only-<handler>).",
    },
    RuleEntry {
        id: "QED002",
        name: "sbpf-version-below-v3",
        description: "With readiness --so or check-upgrade --new-so: a built program \
                      older than sBPF v3, which SIMD-0500 blocks from deploys and upgrades \
                      (readiness only: acknowledge with allow-pre-v3-sbpf).",
    },
];

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
    let all: Vec<&RuleEntry> = entries.iter().chain(QED_RULES).collect();
    if json {
        let s = serde_json::to_string_pretty(&all).context("serializing rule catalog")?;
        println!("{}", s);
        return Ok(());
    }
    eprintln!("qedgen {} + QED rules — {} rule(s):", header, all.len());
    for entry in all {
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

    fn anchor_idl_drift_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/probe-corpus/specless/anchor-idl")
    }

    /// The drift fixture's own source surface, re-declared with a camelCase
    /// IDL-only instruction. Anchor 0.29 and earlier, Codama, and Shank all
    /// emit camelCase instruction names.
    const CAMEL_CASE_IDL: &str = r#"{
        "address": "Vault1111111111111111111111111111111111111",
        "metadata": { "name": "vault", "version": "0.1.0", "spec": "0.1.0" },
        "instructions": [
            {
                "name": "initialize",
                "accounts": [
                    { "name": "admin", "signer": true, "writable": true },
                    { "name": "vault", "writable": true }
                ],
                "args": [ { "name": "cap", "type": "u64" } ]
            },
            {
                "name": "staleReconcile",
                "accounts": [ { "name": "vault", "writable": true } ],
                "args": []
            }
        ]
    }"#;

    #[test]
    fn readiness_demotes_stale_idl_findings_for_camel_case_instruction_names() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "vault.json", CAMEL_CASE_IDL);
        let report = run_readiness(&ReadinessOpts {
            idl: Some(idl),
            so: None,
            framework: Framework::Anchor,
            root: Some(anchor_idl_drift_fixture()),
            unsafes: vec![],
        })
        .unwrap();

        // `staleReconcile` normalizes to `stale_reconcile`, which no source
        // handler provides. Ratchet paths carry the raw IDL name, so the
        // comparison has to normalize before it can match.
        assert!(
            report.findings.iter().any(|finding| {
                finding.rule_id == "P006"
                    && finding.path.iter().any(|part| part == "ix:staleReconcile")
                    && finding.severity == Severity::Additive
                    && finding.message.contains("stale IDL")
            }),
            "camelCase IDL-only instruction must still be demoted: {:?}",
            report.findings
        );
        // `crank` is source-only under this IDL and must stay unsafe.
        assert!(
            report.findings.iter().any(|finding| {
                finding.rule_id == "QED001"
                    && finding.path == ["ix:crank"]
                    && finding.severity == Severity::Unsafe
            }),
            "source-only handler must stay unsafe: {:?}",
            report.findings
        );
    }

    #[test]
    fn readiness_with_root_reports_source_only_handler_and_demotes_stale_idl_finding() {
        let root = anchor_idl_drift_fixture();
        let idl = root.join("target/idl/vault.json");
        let report = run_readiness(&ReadinessOpts {
            idl: Some(idl),
            so: None,
            framework: Framework::Anchor,
            root: Some(root),
            unsafes: vec![],
        })
        .unwrap();

        assert!(
            report.findings.iter().any(|finding| {
                finding.rule_id == "QED001"
                    && finding.path == ["ix:emergency_withdraw"]
                    && finding.severity == Severity::Unsafe
            }),
            "source-only handler must be an unsafe deployment finding: {:?}",
            report.findings
        );
        assert!(
            report.findings.iter().any(|finding| {
                finding.rule_id == "P006"
                    && finding.path.iter().any(|part| part == "ix:reconcile")
                    && finding.severity == Severity::Additive
                    && finding.message.contains("stale IDL")
            }),
            "IDL-only instruction findings must be labeled stale: {:?}",
            report.findings
        );
    }

    #[test]
    fn readiness_without_root_preserves_idl_only_report() {
        let root = anchor_idl_drift_fixture();
        let idl = root.join("target/idl/vault.json");
        let report = run_readiness(&ReadinessOpts {
            idl: Some(idl),
            so: None,
            framework: Framework::Anchor,
            root: None,
            unsafes: vec![],
        })
        .unwrap();

        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "QED001"));
        assert!(
            report.findings.iter().any(|finding| {
                finding.rule_id == "P006"
                    && finding.path.iter().any(|part| part == "ix:reconcile")
                    && finding.severity == Severity::Unsafe
            }),
            "without source reconciliation the IDL-only P006 remains unchanged"
        );
    }

    #[test]
    fn readiness_flags_bare_v1_surface() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", BARE_V1_IDL);
        let report = run_readiness(&ReadinessOpts {
            idl: Some(idl),
            so: None,
            framework: Framework::Anchor,
            root: None,
            unsafes: vec![],
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
            idl: Some(idl),
            so: None,
            framework: Framework::Anchor,
            root: None,
            unsafes: vec![],
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
            root: None,
            new_so: None,
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
            root: None,
            new_so: None,
        })
        .unwrap();
        assert!(report.findings.iter().any(|f| f.rule_id == "R007"));
        assert_eq!(exit_code(&report), 1);
    }

    #[test]
    fn missing_idl_is_surfaced_as_io_error() {
        let err = run_readiness(&ReadinessOpts {
            idl: Some(PathBuf::from("/does/not/exist.json")),
            so: None,
            framework: Framework::Anchor,
            root: None,
            unsafes: vec![],
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("reading"));
    }

    #[test]
    fn malformed_json_is_surfaced_as_parse_error() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", "not json");
        let err = run_readiness(&ReadinessOpts {
            idl: Some(idl),
            so: None,
            framework: Framework::Anchor,
            root: None,
            unsafes: vec![],
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("parsing"));
    }

    // Catalog-shape guards for `--list-rules`: counts pinned at the v0.4.0
    // catalog (7 P + 20 R); bump when the upstream catalog grows.
    #[test]
    fn list_rules_preflight_covers_full_p_catalog() {
        let entries: Vec<_> = default_preflight_rules()
            .iter()
            .map(|r| (r.id(), r.name()))
            .collect();
        assert_eq!(entries.len(), 7);
        assert!(entries.iter().all(|(id, _)| id.starts_with('P')));
        let ids: Vec<&str> = entries.iter().map(|(id, _)| *id).collect();
        for expected in &["P001", "P002", "P003", "P004", "P005", "P006", "P007"] {
            assert!(ids.contains(expected), "missing {expected} in catalog");
        }
    }

    #[test]
    fn list_rules_diff_covers_full_r_catalog() {
        let entries: Vec<_> = default_rules().iter().map(|r| (r.id(), r.name())).collect();
        assert_eq!(entries.len(), 20);
        assert!(entries.iter().all(|(id, _)| id.starts_with('R')));
        // Spot-check rule ids referenced by number in docs.
        let ids: Vec<&str> = entries.iter().map(|(id, _)| *id).collect();
        for expected in &[
            "R001", "R006", "R007", "R013", "R016", "R017", "R018", "R019", "R020",
        ] {
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
            idl: Some(idl),
            so: None,
            framework: Framework::Quasar,
            root: None,
            unsafes: vec![],
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
            root: None,
            new_so: None,
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
            idl: Some(idl),
            so: None,
            framework: Framework::Quasar,
            root: None,
            unsafes: vec![],
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("parsing"));
    }

    // --- sBPF version (QED002, #426) -------------------------------------

    fn sbpf_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sbpf-elf")
            .join(name)
    }

    fn readiness_so(so: PathBuf, unsafes: Vec<String>) -> Result<Report> {
        run_readiness(&ReadinessOpts {
            idl: None,
            so: Some(so),
            framework: Framework::Anchor,
            root: None,
            unsafes,
        })
    }

    #[test]
    fn readiness_flags_v0_program_as_unsafe() {
        let report = readiness_so(sbpf_fixture("counter-v0.so"), vec![]).unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.rule_id == "QED002")
            .expect("V0 program must fire QED002");
        assert_eq!(finding.severity, Severity::Unsafe);
        assert_eq!(finding.path, ["so:counter-v0.so"]);
        assert!(finding.message.contains("SIMD-0500"), "{}", finding.message);
        assert_eq!(exit_code(&report), 2);
    }

    #[test]
    fn readiness_passes_v3_program() {
        let report = readiness_so(sbpf_fixture("counter-v3.so"), vec![]).unwrap();
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(exit_code(&report), 0);
    }

    #[test]
    fn readiness_pre_v3_can_be_acknowledged() {
        let report = readiness_so(
            sbpf_fixture("counter-v0.so"),
            vec![ALLOW_PRE_V3_SBPF.to_string()],
        )
        .unwrap();
        assert!(report
            .findings
            .iter()
            .any(|f| f.rule_id == "QED002" && f.severity == Severity::Additive));
        assert_eq!(exit_code(&report), 0);
    }

    #[test]
    fn readiness_non_elf_so_is_an_error_not_a_pass() {
        let tmp = TempDir::new().unwrap();
        let not_elf = write(tmp.path(), "p.so", "not an elf file at all");
        assert!(readiness_so(not_elf, vec![]).is_err());
        assert!(readiness_so(tmp.path().join("missing.so"), vec![]).is_err());
    }

    #[test]
    fn readiness_combines_idl_rules_and_sbpf_version() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", BARE_V1_IDL);
        let report = run_readiness(&ReadinessOpts {
            idl: Some(idl),
            so: Some(sbpf_fixture("counter-v0.so")),
            framework: Framework::Anchor,
            root: None,
            unsafes: vec![],
        })
        .unwrap();
        let ids: Vec<&str> = report.findings.iter().map(|f| f.rule_id.as_str()).collect();
        assert!(ids.contains(&"P001") && ids.contains(&"QED002"), "{ids:?}");
    }

    #[test]
    fn readiness_root_without_idl_is_an_error() {
        let err = run_readiness(&ReadinessOpts {
            idl: None,
            so: Some(sbpf_fixture("counter-v3.so")),
            framework: Framework::Anchor,
            root: Some(anchor_idl_drift_fixture()),
            unsafes: vec![],
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("--idl"), "{err:#}");
    }

    #[test]
    fn check_upgrade_flags_pre_v3_candidate_binary() {
        let tmp = TempDir::new().unwrap();
        let old = write(tmp.path(), "old.json", BARE_V1_IDL);
        let new = write(tmp.path(), "new.json", BARE_V1_IDL);
        let run = |so: &str| {
            run_check_upgrade(&CheckUpgradeOpts {
                old: old.clone(),
                new: new.clone(),
                unsafes: vec![],
                migrated_accounts: vec![],
                realloc_accounts: vec![],
                framework: Framework::Anchor,
                root: None,
                new_so: Some(sbpf_fixture(so)),
            })
            .unwrap()
        };
        let v0 = run("counter-v0.so");
        assert!(v0.findings.iter().any(|f| f.rule_id == "QED002"));
        assert_eq!(exit_code(&v0), 2);
        let v3 = run("counter-v3.so");
        assert!(v3.findings.is_empty(), "{:?}", v3.findings);
    }

    /// The candidate of `check-upgrade` is the upgrade itself. SIMD-0500
    /// rejects it, so the readiness acknowledgement must not pass it.
    #[test]
    fn check_upgrade_pre_v3_candidate_cannot_be_acknowledged() {
        let tmp = TempDir::new().unwrap();
        let idl = write(tmp.path(), "t.json", BARE_V1_IDL);
        let report = run_check_upgrade(&CheckUpgradeOpts {
            old: idl.clone(),
            new: idl,
            unsafes: vec![ALLOW_PRE_V3_SBPF.to_string()],
            migrated_accounts: vec![],
            realloc_accounts: vec![],
            framework: Framework::Anchor,
            root: None,
            new_so: Some(sbpf_fixture("counter-v0.so")),
        })
        .unwrap();
        let finding = report
            .findings
            .iter()
            .find(|f| f.rule_id == "QED002")
            .expect("QED002");
        assert_eq!(finding.severity, Severity::Unsafe);
        assert!(finding.allow_flag.is_none(), "no acknowledgement offered");
        assert_eq!(exit_code(&report), 2);
    }
}
