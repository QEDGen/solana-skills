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

/// #288 rename→recover lane (the #253 dogfooding session, mechanized):
/// scaffold → fill → commit → spec-level account rename → `codegen`
/// warns on the stale user-owned skip → `--merge-accounts` surgically
/// updates the `#[derive(Accounts)]` structs (fills preserved) →
/// `--force` refuses while user-owned files are dirty and regenerates
/// wholesale once committed.
#[test]
fn rename_recover_journey() {
    common::ensure_qedgen_built();

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let spec = |account: &str| {
        format!(
            r#"spec Renamer
program_id "11111111111111111111111111111111"
type State | Active of {{ total : U64 }}
handler poke : State.Active -> State.Active {{
  accounts {{ {account} : writable }}
  effect {{ Active.total += 1 }}
}}
"#
        )
    };
    std::fs::write(root.join("renamer.qedspec"), spec("vault")).expect("write spec");
    common::git_init(root);
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.email=j@t", "-c", "user.name=journey"])
            .args(args)
            .current_dir(root)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    assert_ok(
        "qedgen init",
        &qedgen(
            root,
            &["init", "--name", "renamer", "--spec", "renamer.qedspec"],
        ),
    );
    assert_ok("qedgen codegen", &qedgen(root, &["codegen"]));

    // Simulate the agent fill, then commit — the recovery baseline.
    let instr_path = root.join("programs/src/instructions/poke.rs");
    let instr = std::fs::read_to_string(&instr_path).expect("poke.rs");
    std::fs::write(&instr_path, format!("{instr}// FILL-MARKER\n")).expect("fill");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "scaffold + fill"]);

    // Spec-level rename: account vault → treasury.
    std::fs::write(root.join("renamer.qedspec"), spec("treasury")).expect("rename spec");

    // Plain codegen: skips the user-owned set but must WARN it's stale
    // and name the recovery flags (#253 option 1 + #288).
    let out = qedgen(root, &["codegen"]);
    assert_ok("qedgen codegen (post-rename)", &out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("DIFFERENT spec revision") && stderr.contains("--merge-accounts"),
        "post-rename codegen must warn stale and name the recovery flags; stderr:\n{stderr}"
    );
    let lib_path = root.join("programs/src/lib.rs");
    let lib = std::fs::read_to_string(&lib_path).expect("lib.rs");
    assert!(
        lib.contains("pub vault:"),
        "plain codegen must not touch the user-owned lib.rs"
    );

    // Surgical recovery: structs update, fills survive.
    assert_ok(
        "qedgen codegen --merge-accounts",
        &qedgen(root, &["codegen", "--merge-accounts"]),
    );
    let lib = std::fs::read_to_string(&lib_path).expect("lib.rs");
    assert!(
        lib.contains("pub treasury:") && !lib.contains("pub vault:"),
        "--merge-accounts must regenerate the Accounts struct fields; got:\n{lib}"
    );
    assert!(
        std::fs::read_to_string(&instr_path)
            .expect("poke.rs")
            .contains("// FILL-MARKER"),
        "--merge-accounts must not touch instruction fills"
    );

    // --force refuses while user-owned files have uncommitted changes
    // (the merged lib.rs is dirty; dirty the fill too).
    let instr = std::fs::read_to_string(&instr_path).expect("poke.rs");
    std::fs::write(&instr_path, format!("{instr}// UNCOMMITTED\n")).expect("dirty fill");
    let out = qedgen(root, &["codegen", "--force"]);
    assert!(
        !out.status.success(),
        "--force must refuse dirty user-owned files; stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Commit or stash"),
        "refusal must point at the git recovery path; stderr:\n{stderr}"
    );

    // Committed baseline → --force regenerates the user-owned set.
    git(&["add", "-A"]);
    git(&["commit", "-qm", "pre-force baseline"]);
    assert_ok(
        "qedgen codegen --force",
        &qedgen(root, &["codegen", "--force"]),
    );
    assert!(
        !std::fs::read_to_string(&instr_path)
            .expect("poke.rs")
            .contains("FILL-MARKER"),
        "--force must regenerate the instruction scaffold wholesale"
    );
    let lib = std::fs::read_to_string(&lib_path).expect("lib.rs");
    assert!(
        lib.contains("pub treasury:"),
        "--force regen must carry the renamed account; got:\n{lib}"
    );
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
