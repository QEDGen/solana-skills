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
