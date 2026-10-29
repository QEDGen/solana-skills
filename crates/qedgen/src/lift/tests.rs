use super::qedsvm::{check_checkout, lift_tree, resolve_checkout, QedsvmTools};
use super::report::{axiom_allowed, parse_axioms, InstructionReport, LiftReport};
use super::*;
use std::path::Path;
use std::process::Command;

const BINDINGS: &str = include_str!("../../tests/fixtures/lift/label_vault.bindings.json");
const COVERAGE: &str = include_str!("../../tests/fixtures/lift/label_vault.coverage.json");

#[test]
fn s2_axiom_allowed() {
    let checks = vec![
        "Examples.LabelVault.decodes".to_string(),
        "Examples.LabelVault.check53".to_string(),
    ];
    assert!(axiom_allowed("propext", &checks));
    assert!(axiom_allowed("Classical.choice", &checks));
    assert!(axiom_allowed("Quot.sound", &checks));
    assert!(axiom_allowed(
        "Examples.LabelVault.check53._native.native_decide.ax_1_1",
        &checks
    ));
    assert!(axiom_allowed(
        "Examples.LabelVault.decodes._native.native_decide.ax_1_1",
        &checks
    ));
    assert!(!axiom_allowed("sorryAx", &checks));
    assert!(!axiom_allowed("Lean.ofReduceBool", &checks));
    assert!(!axiom_allowed("Examples.LabelVault.my_axiom", &checks));
    // A hand-written axiom named like a native_decide one, outside the generated checks.
    assert!(!axiom_allowed(
        "Examples.LabelVault.forged._native.native_decide.ax_1",
        &checks
    ));
}

#[test]
fn s2_native_check_decls_from_bindings() {
    let b: serde_json::Value = serde_json::from_str(BINDINGS).unwrap();
    let d = report::native_check_decls("LabelVault", &b);
    for n in ["decodes", "noRetWitness_ok", "check53", "absOk53", "pins53"] {
        assert!(d.contains(&format!("Examples.LabelVault.{n}")), "{n}");
    }
}

#[test]
fn s2_authority_comes_from_proven_facts() {
    let ws = built_workspace();
    let r = run_stub(ws.path(), GOOD_AXIOMS).unwrap();
    let ix1 = &r.instructions[1];
    assert_eq!(ix1.signers, vec![1]);
    assert_eq!(
        ix1.key_compares,
        vec!["account 1 key == account 0 data[8..40]".to_string()]
    );
    assert!(r.instructions[0].key_compares.is_empty());
}

#[test]
fn s2_gaps_list_unpinned_guards_and_every_length() {
    let ws = built_workspace();
    let r = run_stub(ws.path(), GOOD_AXIOMS).unwrap();
    let g = r.gaps.join("\n");
    assert!(g.contains("at 75") && g.contains("at 84"), "{g}");
    assert!(g.contains("account 0") && g.contains("48"), "{g}");
    assert!(g.contains("satisfiable"), "{g}");
}

#[test]
fn s2_parse_axioms_multiline() {
    let out = "'Examples.LabelVault.readable53' depends on axioms: [propext,\n Classical.choice,\n Quot.sound,\n Examples.LabelVault.pins53._native.native_decide.ax_1_1]\n";
    let parsed = parse_axioms(out);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].0, "Examples.LabelVault.readable53");
    assert_eq!(parsed[0].1.len(), 4);
    assert_eq!(
        parsed[0].1[3],
        "Examples.LabelVault.pins53._native.native_decide.ax_1_1"
    );
}

#[test]
fn s2_module_prefix() {
    assert_eq!(module_prefix("label_vault").unwrap(), "LabelVault");
    assert_eq!(module_prefix("p_token").unwrap(), "PToken");
    assert!(module_prefix("9bad").is_err());
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("a"), "a").unwrap();
    git(dir.path(), &["add", "a"]);
    git(dir.path(), &["commit", "-qm", "a"]);
    dir
}

#[test]
fn s2_wrong_qedsvm_rev_is_refused() {
    let dir = repo();
    let err = check_checkout(dir.path(), Some("0123456789abcdef"), false).unwrap_err();
    assert!(err.to_string().contains("0123456789abcdef"), "{err}");
}

#[test]
fn s2_unpinned_checkout_needs_dev_mode() {
    let dir = repo();
    let err = check_checkout(dir.path(), None, false).unwrap_err();
    assert!(err.to_string().contains("--qedsvm-dev"), "{err}");
    let rev = check_checkout(dir.path(), None, true).unwrap();
    assert_eq!(rev.len(), 40);
    std::fs::write(dir.path().join("b"), "new").unwrap();
    assert!(check_checkout(dir.path(), None, true)
        .unwrap()
        .ends_with("-dirty"));
}

#[test]
fn s2_dirty_checkout_is_refused() {
    let dir = repo();
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::fs::write(dir.path().join("a"), "changed").unwrap();
    let err = check_checkout(dir.path(), Some(head.trim()), false).unwrap_err();
    assert!(err.to_string().contains("uncommitted"), "{err}");
}

struct Stub {
    axioms: String,
    fail_discover: bool,
}

impl QedsvmTools for Stub {
    fn concept_map(&self, _so: &Path, _debug: &Path, out: &Path) -> anyhow::Result<()> {
        std::fs::write(out, "{}")?;
        Ok(())
    }
    fn discover(
        &self,
        _so: &Path,
        _debug: &Path,
        _concepts: &Path,
        out: &Path,
    ) -> anyhow::Result<()> {
        if self.fail_discover {
            anyhow::bail!("discovery failed");
        }
        std::fs::write(out, BINDINGS)?;
        Ok(())
    }
    fn success_proofs(
        &self,
        _b: &Path,
        _prefix: &str,
        _so: &Path,
        out_dir: &Path,
    ) -> anyhow::Result<()> {
        std::fs::create_dir_all(out_dir)?;
        std::fs::write(out_dir.join("Text.lean"), "-- text")?;
        std::fs::write(out_dir.join("Success.lean"), "-- success")?;
        std::fs::write(out_dir.join("coverage.json"), COVERAGE)?;
        Ok(())
    }
    fn check_lean(
        &self,
        _prefix: &str,
        _dir: &Path,
        _theorems: &[String],
    ) -> anyhow::Result<String> {
        Ok(self.axioms.clone())
    }
}

fn built_workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join(".qed/build/label_vault");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("label_vault.so"), b"so").unwrap();
    std::fs::write(dir.join("label_vault.debug.so"), b"dbg").unwrap();
    let record = include_str!("../../tests/fixtures/lift/label_vault.build.json").replace(
        "914f0f0e58ec5c19b7eaba9a0f7d9eb978f5053fcd2383db91985c63368dfc8d",
        &crate::verifiable_build::selfcheck::file_sha256(b"so"),
    );
    std::fs::write(dir.join("build.json"), record).unwrap();
    ws
}

const GOOD_AXIOMS: &str = "'Examples.LabelVault.readable53' depends on axioms: [propext, Classical.choice, Quot.sound, Examples.LabelVault.check53._native.native_decide.ax_1_1]\n";

fn run_stub(ws: &Path, axioms: &str) -> anyhow::Result<LiftReport> {
    lift_with(
        ws,
        "label_vault",
        &Stub {
            axioms: axioms.into(),
            fail_discover: false,
        },
        "deadbeef".into(),
    )
}

#[test]
fn s2_report_lists_gaps_and_coverage() {
    let ws = built_workspace();
    let r = run_stub(ws.path(), GOOD_AXIOMS).unwrap();
    assert!(r.passed);
    assert_eq!(r.coverage["instructions"], 3);
    assert_eq!(r.instructions.len(), 3);
    assert!(r
        .gaps
        .iter()
        .any(|g| g.contains("native_decide") && g.contains("qedsvm#98")));
    assert!(r
        .gaps
        .iter()
        .any(|g| g.contains("account 1") && g.contains("not check")));
    assert_eq!(r.axioms.native_decide, 1);
    let dir = ws.path().join(".qed/lift/label_vault");
    for f in [
        "report.json",
        "report.txt",
        "bindings.json",
        "Text.lean",
        "Success.lean",
    ] {
        assert!(dir.join(f).exists(), "{f}");
    }
}

#[test]
fn s2_disallowed_axiom_fails_the_lift() {
    let ws = built_workspace();
    let bad = "'Examples.LabelVault.readable53' depends on axioms: [propext, sorryAx]\n";
    let r = run_stub(ws.path(), bad).unwrap();
    assert!(!r.passed);
    assert_eq!(r.axioms.disallowed, vec!["sorryAx".to_string()]);
}

#[test]
fn s2_missing_theorem_axioms_fail_the_lift() {
    let ws = built_workspace();
    let r = run_stub(ws.path(), "").unwrap();
    assert!(!r.passed);
}

#[test]
fn s2_lift_never_writes_verify_evidence() {
    let ws = built_workspace();
    run_stub(ws.path(), GOOD_AXIOMS).unwrap();
    assert!(!ws.path().join(".qed/verify-evidence.json").exists());
}

#[test]
fn s2_missing_build_is_refused() {
    let ws = tempfile::tempdir().unwrap();
    let err = run_stub(ws.path(), GOOD_AXIOMS).unwrap_err();
    assert!(err.to_string().contains("qedgen build"), "{err}");
}

#[test]
fn s2_v3_build_is_refused() {
    let ws = built_workspace();
    let p = ws.path().join(".qed/build/label_vault/build.json");
    let text = std::fs::read_to_string(&p)
        .unwrap()
        .replace("\"arch\": \"v0\"", "\"arch\": \"v3\"");
    std::fs::write(&p, text).unwrap();
    let err = run_stub(ws.path(), GOOD_AXIOMS).unwrap_err();
    assert!(err.to_string().contains("V0"), "{err}");
}

#[test]
fn s2_replaced_deploy_binary_is_refused() {
    let ws = built_workspace();
    std::fs::write(
        ws.path().join(".qed/build/label_vault/label_vault.so"),
        b"another program",
    )
    .unwrap();
    let err = run_stub(ws.path(), GOOD_AXIOMS).unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");
}

#[test]
fn s2_relative_checkout_is_made_absolute() {
    // The test runs inside this crate, which is a git checkout.
    let (dir, _) = resolve_checkout(Some(Path::new(".")), true).unwrap();
    assert!(dir.is_absolute(), "{}", dir.display());
}

#[test]
fn s2_each_lean_check_gets_its_own_tree() {
    let root = tempfile::tempdir().unwrap();
    let a = lift_tree(root.path()).unwrap();
    let b = lift_tree(root.path()).unwrap();
    assert_ne!(a.path(), b.path());
    assert!(a.path().starts_with(root.path().join(".lake/qedgen-lift")));
}

#[test]
fn s2_failed_rerun_removes_the_old_report() {
    let ws = built_workspace();
    run_stub(ws.path(), GOOD_AXIOMS).unwrap();
    let dir = ws.path().join(".qed/lift/label_vault");
    assert!(dir.join("report.json").exists());
    let failing = Stub {
        axioms: GOOD_AXIOMS.into(),
        fail_discover: true,
    };
    lift_with(ws.path(), "label_vault", &failing, "deadbeef".into()).unwrap_err();
    assert!(!dir.join("report.json").exists());
    assert!(!dir.join("report.txt").exists());
}

#[test]
fn s2_instruction_facts_hold_on_every_route() {
    let bindings = serde_json::json!({
        "instructions": [{"discriminator": 1, "handler": "h", "success_edges": [53]}]
    });
    let coverage = serde_json::json!({"route_facts": [
        {"edge": 53, "route": 10, "discriminator": 1, "proven_signers": [1, 2],
         "proven_key_compares": ["k"]},
        {"edge": 53, "route": 20, "discriminator": 1, "proven_signers": [2],
         "proven_key_compares": []}
    ]});
    let r = InstructionReport::of(&bindings, &coverage);
    assert_eq!(r[0].signers, vec![2]);
    assert!(r[0].key_compares.is_empty());
    assert_eq!(r[0].routes.len(), 2);
    assert_eq!(r[0].routes[0].route, 10);
    assert_eq!(r[0].routes[0].signers, vec![1, 2]);
    assert_eq!(r[0].routes[0].key_compares, vec!["k".to_string()]);
}

#[test]
fn s2_gate_workflow_checks_out_the_pinned_qedsvm() {
    let pin = super::qedsvm::QEDSVM_LIFT_REV.expect("a qedsvm revision is pinned");
    let wf = include_str!("../../../../.github/workflows/label-lift.yml");
    assert!(
        wf.contains(&format!("ref: {pin}")),
        "label-lift.yml must check out qedsvm at {pin}"
    );
}

#[test]
fn s2_failed_clone_leaves_no_cached_checkout() {
    let upstream = repo();
    let url = format!("file://{}", upstream.path().display());
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(upstream.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let head = head.trim();
    let cache = tempfile::tempdir().unwrap();
    let dir = cache.path().join(head);
    // The fetch fails: the revision does not exist upstream.
    super::qedsvm::ensure_clone(&dir, &url, "0123456789abcdef0123456789abcdef01234567")
        .unwrap_err();
    assert!(!dir.exists(), "a failed clone must not look finished");
    // A later run recovers once the fetch can succeed.
    super::qedsvm::ensure_clone(&dir, &url, head).unwrap();
    assert_eq!(check_checkout(&dir, Some(head), false).unwrap(), head);
}
