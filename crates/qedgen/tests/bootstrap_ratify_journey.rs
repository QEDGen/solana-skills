//! Journey test for the documented spec-less elicitation handoff (#248,
//! #249): `probe --bootstrap --emit-spec-candidates --audit-dir` →
//! author `answers.json` → `ratify --audit-dir`, exactly as the auditor
//! guidance prescribes. Before #248 the bootstrap branch silently
//! dropped the audit dir (empty dir, ratify hard-error); before #249
//! ratify wrote the spec to `<root>/.qed/.qed.qedspec`.

mod common;

use std::process::Command;

#[test]
fn bootstrap_probe_to_ratify_journey() {
    common::ensure_qedgen_built();

    let tmp = common::stage_fixture(
        "crates/qedgen/tests/fixtures/probe-corpus/specless/native-shank-marker",
    );
    let root = tmp.path();
    let audit = root.join(".qed/audit/journey-1");

    // Step 1: bootstrap probe with working-set materialization.
    let out = Command::new(common::qedgen_bin())
        .args(["probe", "--bootstrap", "--root"])
        .arg(root)
        .args(["--emit-spec-candidates", "--audit-dir"])
        .arg(&audit)
        .output()
        .expect("run qedgen probe --bootstrap");
    assert!(
        out.status.success(),
        "bootstrap probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for artifact in [
        "clusters.json",
        "skeleton.qedspec",
        "hypotheses.json",
        "run-manifest.json",
    ] {
        assert!(
            audit.join(artifact).exists(),
            "bootstrap --emit-spec-candidates --audit-dir must materialize {artifact} (#248); \
             audit dir contents: {:?}",
            std::fs::read_dir(&audit)
                .map(|d| d
                    .filter_map(|e| e.ok().map(|e| e.file_name()))
                    .collect::<Vec<_>>())
                .unwrap_or_default()
        );
    }

    // Step 2: author the (empty) structured answer set — the issue repro.
    std::fs::write(audit.join("answers.json"), "{\"answers\":[]}\n").expect("write answers");

    // Step 3: ratify consumes the working set with default output paths.
    let out = Command::new(common::qedgen_bin())
        .args(["ratify", "--audit-dir"])
        .arg(&audit)
        .current_dir(root)
        .output()
        .expect("run qedgen ratify");
    assert!(
        out.status.success(),
        "ratify failed on the bootstrap working set: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // #249: default spec path is <root>/<name>.qedspec derived from the
    // manifest's recorded program root — never <root>/.qed/.qed.qedspec.
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .expect("root name");
    assert!(
        root.join(format!("{name}.qedspec")).exists(),
        "ratified spec must land at <root>/{name}.qedspec; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !root.join(".qed/.qed.qedspec").exists(),
        "doubled .qed/.qed.qedspec path must not reappear (#249)"
    );
}

/// #289: `probe --program .` from inside the program root — the natural
/// invocation — must canonicalize before anything derives a name. The
/// skeleton spec name, the manifest's recorded `target.program_root`,
/// and ratify's default spec path all carry the directory's real name,
/// never the `program`/`Program` placeholder.
#[test]
fn probe_program_dot_uses_real_directory_name() {
    common::ensure_qedgen_built();

    let (_tmp, root) = common::stage_fixture_named(
        "crates/qedgen/tests/fixtures/probe-corpus/specless/pinocchio-codama",
        "myvault",
    );
    let audit_rel = ".qed/audit/dot-journey";

    let out = Command::new(common::qedgen_bin())
        .args([
            "probe",
            "--program",
            ".",
            "--emit-spec-candidates",
            "--audit-dir",
            audit_rel,
        ])
        .current_dir(&root)
        .output()
        .expect("run qedgen probe --program .");
    assert!(
        out.status.success(),
        "probe --program . failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let audit = root.join(audit_rel);
    let skeleton =
        std::fs::read_to_string(audit.join("skeleton.qedspec")).expect("read skeleton.qedspec");
    let spec_line = skeleton
        .lines()
        .find(|l| l.starts_with("spec "))
        .unwrap_or_default()
        .to_string();
    assert!(
        spec_line.to_lowercase().contains("myvault"),
        "skeleton spec name must derive from the real directory name, got: {spec_line}"
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(audit.join("run-manifest.json")).expect("read run-manifest.json"),
    )
    .expect("parse run-manifest.json");
    let recorded = manifest["target"]["program_root"]
        .as_str()
        .expect("target.program_root");
    assert!(
        std::path::Path::new(recorded).is_absolute() && recorded.ends_with("myvault"),
        "manifest must record the canonicalized program root, got: {recorded}"
    );

    // Ratify's default output derives from the recorded root.
    std::fs::write(audit.join("answers.json"), "{\"answers\":[]}\n").expect("write answers");
    let out = Command::new(common::qedgen_bin())
        .args(["ratify", "--audit-dir", audit_rel])
        .current_dir(&root)
        .output()
        .expect("run qedgen ratify");
    assert!(
        out.status.success(),
        "ratify failed on the --program . working set: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        root.join("myvault.qedspec").exists(),
        "ratified spec must land at myvault.qedspec, not program.qedspec; dir: {:?}",
        std::fs::read_dir(&root)
            .map(|d| d
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    assert!(
        !root.join("program.qedspec").exists(),
        "placeholder program.qedspec must not be written (#289)"
    );
}
