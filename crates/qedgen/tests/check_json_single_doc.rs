//! #355 gate: `qedgen check --json` must write exactly one JSON document
//! to stdout. Before the fix, `--coverage --json` printed the coverage
//! object and the findings array as two concatenated documents, so
//! strict parsers (`serde_json::from_str`, `json.load`) failed with
//! "trailing characters" and no scripted consumer could reach
//! `backend_coverage`.

mod common;

use std::process::Command;

fn pool_spec() -> std::path::PathBuf {
    common::repo_root().join("crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec")
}

fn run_check(extra: &[&str]) -> String {
    common::ensure_qedgen_built();
    let out = Command::new(common::qedgen_bin())
        .arg("check")
        .arg("--spec")
        .arg(pool_spec())
        .args(extra)
        .output()
        .expect("run qedgen check");
    String::from_utf8(out.stdout).expect("stdout utf8")
}

#[test]
fn coverage_json_is_one_document_with_sections() {
    let stdout = run_check(&["--coverage", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("check --coverage --json stdout must parse as ONE JSON document: {e}\n{stdout}")
    });
    let obj = doc
        .as_object()
        .expect("with --coverage the single document is an object");
    let coverage = obj
        .get("coverage")
        .expect("document has a `coverage` section");
    assert!(
        coverage.get("backend_coverage").is_some(),
        "coverage section carries the backend_coverage rollup"
    );
    assert!(
        obj.get("findings").is_some_and(|f| f.is_array()),
        "document has the lint `findings` array"
    );
}

#[test]
fn plain_json_stays_a_bare_findings_array() {
    let stdout = run_check(&["--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("check --json stdout must parse as ONE JSON document: {e}\n{stdout}")
    });
    assert!(
        doc.is_array(),
        "plain `check --json` keeps the bare findings array for existing consumers"
    );
}
