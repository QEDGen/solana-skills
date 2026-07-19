//! Journey tests (#269): execute the exact command sequences the docs
//! prescribe, end to end, against staged fixtures. Per-phase unit gates
//! don't catch a lane whose steps never composed (#248/#249 shipped that
//! way); these do. Companion: `bootstrap_ratify_journey.rs`.

mod common;

use std::path::Path;
use std::process::Command;

fn qedgen(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(common::qedgen_bin())
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn qedgen")
}

fn assert_ok(step: &str, out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{step} failed (exit {:?}):\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SKILL.md core-loop quickstart: authored spec → `git init` →
/// `qedgen init` → `qedgen check` → `qedgen codegen --all`, all with the
/// spec resolved from `.qed/config.json` (no --spec after init). #262's
/// friction report came from deviating from this lane; this pins that
/// the documented sequence itself works from a bare directory.
#[test]
fn quickstart_init_check_codegen_journey() {
    common::ensure_qedgen_built();

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::copy(
        common::repo_root().join("examples/rust/escrow/escrow.qedspec"),
        root.join("escrow.qedspec"),
    )
    .expect("copy spec");
    common::git_init(root);

    assert_ok(
        "qedgen init",
        &qedgen(
            root,
            &["init", "--name", "escrow", "--spec", "escrow.qedspec"],
        ),
    );

    let check = qedgen(root, &["check"]);
    assert_ok("qedgen check", &check);
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(
        stderr.contains("0 error(s), 0 warning(s)"),
        "bundled escrow spec must check clean in the quickstart; stderr:\n{stderr}"
    );

    assert_ok("qedgen codegen --all", &qedgen(root, &["codegen", "--all"]));
    for artifact in [
        "programs/src/lib.rs",
        "programs/tests/proptest.rs",
        "formal_verification/Spec.lean",
        "formal_verification/Proofs.lean",
        ".github/workflows/verify.yml",
    ] {
        assert!(
            root.join(artifact).exists(),
            "codegen --all must produce {artifact} in the project root"
        );
    }
}

/// Scaffold-to-spec lane: `probe --program <anchor-root>
/// --emit-spec-candidates --audit-dir` → author `answers.json` →
/// `ratify --audit-dir`, per the auditor guidance — the Anchor-extractor
/// sibling of the bootstrap lane (#248/#249 hit only the latter because
/// only this lane had ever been driven).
#[test]
fn scaffold_probe_to_ratify_journey() {
    common::ensure_qedgen_built();

    let tmp =
        common::stage_fixture("crates/qedgen/tests/fixtures/probe-corpus/specless/anchor-idl");
    let root = tmp.path();
    let audit = root.join(".qed/audit/journey-1");

    let mut probe_args = vec!["probe", "--program"];
    let root_str = root.to_str().expect("utf8 root");
    probe_args.extend([root_str, "--emit-spec-candidates", "--audit-dir"]);
    let audit_str = audit.to_str().expect("utf8 audit");
    probe_args.push(audit_str);
    assert_ok("probe --program", &qedgen(root, &probe_args));

    for artifact in [
        "clusters.json",
        "skeleton.qedspec",
        "hypotheses.json",
        "run-manifest.json",
    ] {
        assert!(
            audit.join(artifact).exists(),
            "scaffold-lane probe must materialize {artifact}"
        );
    }

    std::fs::write(audit.join("answers.json"), "{\"answers\":[]}\n").expect("write answers");
    assert_ok(
        "ratify",
        &qedgen(root, &["ratify", "--audit-dir", audit_str]),
    );

    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .expect("root name");
    assert!(
        root.join(format!("{name}.qedspec")).exists(),
        "ratified spec must land at <root>/{name}.qedspec"
    );
}
