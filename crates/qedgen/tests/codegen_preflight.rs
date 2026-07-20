//! Gates for the codegen dispatch seam: the combined prerequisite
//! preflight (#262) and spec-relative output resolution (#279).

mod common;

use std::path::Path;
use std::process::Command;

fn qedgen_from(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(common::qedgen_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn qedgen")
}

/// #262: a scratch dir missing BOTH prerequisites (git repo, .qed/) gets
/// one error naming both fixes — not one prerequisite per round-trip.
#[test]
fn preflight_reports_all_missing_prerequisites_at_once() {
    common::ensure_qedgen_built();

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::copy(
        common::repo_root().join("examples/rust/escrow/escrow.qedspec"),
        root.join("escrow.qedspec"),
    )
    .expect("copy spec");

    let out = qedgen_from(root, &["codegen", "--proptest", "--spec", "escrow.qedspec"]);
    assert!(!out.status.success(), "preflight must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("git init") && stderr.contains("qedgen init"),
        "one message must name every missing prerequisite; stderr:\n{stderr}"
    );
}

/// #279: relative output paths (including the clap defaults) resolve
/// against the spec's directory, not the invoker's cwd — no scattered
/// artifact trees when codegen is driven from outside the project.
#[test]
fn relative_outputs_resolve_against_spec_dir_not_cwd() {
    common::ensure_qedgen_built();

    // A real project: spec + git + qedgen init.
    let proj = tempfile::tempdir().expect("tempdir");
    let root = proj.path();
    std::fs::copy(
        common::repo_root().join("examples/rust/escrow/escrow.qedspec"),
        root.join("escrow.qedspec"),
    )
    .expect("copy spec");
    common::git_init(root);
    let out = qedgen_from(
        root,
        &["init", "--name", "escrow", "--spec", "escrow.qedspec"],
    );
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Drive codegen from a DIFFERENT, non-git cwd with an absolute --spec.
    // The spec project owns every relative output and is the repository
    // whose recovery baseline matters.
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let spec_abs = root.join("escrow.qedspec");
    let out = qedgen_from(
        elsewhere.path(),
        &[
            "codegen",
            "--proptest",
            "--spec",
            spec_abs.to_str().expect("utf8"),
        ],
    );
    assert!(
        out.status.success(),
        "codegen failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        root.join("programs/tests/proptest.rs").exists(),
        "artifacts must land under the spec's project root"
    );
    let scattered: Vec<_> = std::fs::read_dir(elsewhere.path())
        .expect("read cwd")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert!(
        scattered.is_empty(),
        "nothing may be created under the invoking cwd; found {scattered:?}"
    );
}
