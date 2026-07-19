//! #260 gate: Severity::Error lints must be counted in the check summary
//! and fail the exit code — before the fix they printed with an `E`
//! prefix but were invisible to both the tally and the exit decision.

mod common;

use std::process::Command;

#[test]
fn error_severity_lint_fails_check_with_error_count() {
    common::ensure_qedgen_built();

    let temp = tempfile::tempdir().expect("tempdir");
    let spec = temp.path().join("demo.qedspec");
    std::fs::write(
        &spec,
        "spec Demo\n\n\
         type State\n  | Active of { counter : U64 }\n\n\
         invariant conservation \"total tokens preserved across all handlers\"\n",
    )
    .expect("write spec");

    let out = Command::new(common::qedgen_bin())
        .arg("check")
        .arg("--spec")
        .arg(&spec)
        .output()
        .expect("run qedgen check");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "a Severity::Error lint (invariant_no_body) must make `qedgen check` exit non-zero; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("1 error(s),"),
        "the summary tally must count the Error-severity lint; stderr:\n{stderr}"
    );
}
