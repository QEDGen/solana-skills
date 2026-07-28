//! Execution gate for the Parallax reproducer lane.
//!
//! The lane's whole value is telling a real bug from a guard, and that
//! distinction is invisible to every cheaper check: the generated repro
//! compiles either way, and the spec predicate fires either way. Only
//! RUNNING it against a real program separates them.
//!
//! One fixture program covers all three outcomes, so a single `build-sbf`
//! gates the full partition:
//!
//! | handler      | predicate fires | verdict                              |
//! |--------------|-----------------|--------------------------------------|
//! | `set_fee`    | both            | both CONFIRM — genuinely unguarded   |
//! | `bump_total` | both            | authority DROPS, replay CONFIRMS     |
//! | `open`       | neither         | no candidate at all                  |
//!
//! `bump_total` is the case that matters most. `missing_signer` keys off a
//! missing `auth` clause, but an `accounts` block marking an account
//! `signer` enforces a signature anyway, and the PDA seed binds the vault to
//! its caller — so the predicate fires on a handler that is not exploitable.
//! Confirming it without running the reproducer would surface a false
//! CRITICAL. Its replay claim is separately TRUE, which is why the assertion
//! below pins per-claim verdicts rather than a per-handler count.
//!
//! `#[ignore]`: builds an SBF program and compiles LiteSVM. CI runs it with
//! `-- --ignored`.

mod common;

use common::{ensure_qedgen_built, qedgen_bin, run_capture_ok, run_ok, stage_fixture};
use std::process::Command;

const PROGRAM_ID: &str = "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS";

/// Which `(handler, category)` pairs must be confirmed, and which must be
/// dropped after their reproducer runs.
const MUST_CONFIRM: &[&str] = &["missing_signer", "lifecycle_one_shot_violation"];

#[test]
#[ignore = "compile-heavy: cargo build-sbf + LiteSVM, then runs the reproducers"]
fn parallax_reproducers_confirm_bugs_and_drop_guarded_handlers() {
    ensure_qedgen_built();
    let tmp = stage_fixture("crates/qedgen/tests/fixtures/parallax-repro-gate");
    common::git_init(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".qed")).expect("create .qed");

    run_ok(
        Command::new(qedgen_bin())
            .arg("codegen")
            .arg("--spec")
            .arg("vulnerable.qedspec")
            .arg("--target")
            .arg("anchor")
            .arg("--output-dir")
            .arg("program")
            .current_dir(tmp.path()),
    );

    // #368 — this used to rewrite `declare_id!` after generation, because a
    // spec without `program_id` gets the System Program's address stamped as
    // its own. The fixture spec now declares the id, so codegen emits it and
    // the reproducer lane (which refuses the placeholder outright) has a real
    // target. Asserted rather than assumed: if the spec loses its
    // `program_id`, this fails here instead of the lane silently reporting
    // "no bug" from a transaction aimed at the System Program.
    let lib = tmp.path().join("program/src/lib.rs");
    let source = std::fs::read_to_string(&lib).expect("read lib.rs");
    assert!(
        source.contains(&format!("declare_id!(\"{PROGRAM_ID}\")")),
        "the fixture spec must declare `program_id \"{PROGRAM_ID}\"`; got:\n{source}"
    );

    // The generated manifest pins `qedgen-macros` to an unreleased tag.
    let manifest_path = tmp.path().join("program/Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let macros = common::repo_root().join("crates/qedgen-macros");
    let patched: String = manifest
        .lines()
        .map(|line| {
            if line.starts_with("qedgen-macros = {") {
                format!("qedgen-macros = {{ path = {macros:?} }}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&manifest_path, patched).expect("write manifest");

    run_ok(
        Command::new("cargo")
            .arg("build-sbf")
            .current_dir(tmp.path().join("program")),
    );

    let json = run_capture_ok(
        Command::new(qedgen_bin())
            .arg("probe")
            .arg("--spec")
            .arg("vulnerable.qedspec")
            .arg("--execute-repros")
            .arg("--json")
            .current_dir(tmp.path()),
    );
    let report: serde_json::Value = serde_json::from_str(json.trim())
        .unwrap_or_else(|e| panic!("probe --json is not valid JSON: {e}\n{json}"));

    let confirmed: Vec<(String, String)> = report["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|f| {
            (
                f["handler"].as_str().unwrap_or_default().to_string(),
                f["category_tag"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();

    // `set_fee` is genuinely unguarded: both claims must be confirmed, each
    // by a reproducer that actually ran.
    for category in MUST_CONFIRM {
        assert!(
            confirmed
                .iter()
                .any(|(h, c)| h == "set_fee" && c == category),
            "set_fee/{category} must be CONFIRMED — the program accepts the attack.\n\
             confirmed: {confirmed:?}"
        );
    }

    // The regression this gate exists for: a guarded handler the predicate
    // still flags must NOT be confirmed as an authority bug.
    assert!(
        !confirmed
            .iter()
            .any(|(h, c)| h == "bump_total" && c == "missing_signer"),
        "bump_total/missing_signer must be DROPPED — `owner : signer` enforces a \
         signature and the PDA seed binds the vault to its caller, so the program \
         refuses the attack. Confirming it is a false CRITICAL.\nconfirmed: {confirmed:?}"
    );

    // ...while its replay claim, about the same handler, is genuinely true.
    assert!(
        confirmed
            .iter()
            .any(|(h, c)| h == "bump_total" && c == "lifecycle_one_shot_violation"),
        "bump_total/lifecycle_one_shot_violation must be CONFIRMED — the handler \
         really can be replayed.\nconfirmed: {confirmed:?}"
    );

    // Every confirmed finding must carry a Parallax reproducer, never a bare
    // claim: a finding without executed evidence is what this lane forbids.
    for finding in report["findings"].as_array().expect("findings array") {
        assert_eq!(
            finding["reproducer"]["kind"].as_str(),
            Some("parallax"),
            "confirmed finding lacks an executed Parallax reproducer: {finding}"
        );
    }
}

/// Without `--execute-repros` nothing may be confirmed: the reproducer was
/// written but never run, and a generated test is not evidence.
#[test]
#[ignore = "compile-heavy: shares the SBF build above"]
fn generation_alone_never_confirms() {
    ensure_qedgen_built();
    let tmp = stage_fixture("crates/qedgen/tests/fixtures/parallax-repro-gate");
    common::git_init(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".qed")).expect("create .qed");

    let json = run_capture_ok(
        Command::new(qedgen_bin())
            .arg("probe")
            .arg("--spec")
            .arg("vulnerable.qedspec")
            .arg("--json")
            .current_dir(tmp.path()),
    );
    let report: serde_json::Value = serde_json::from_str(json.trim()).expect("probe --json");

    let parallax_confirmed = report["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter(|f| f["reproducer"]["kind"].as_str() == Some("parallax"))
        .count();
    assert_eq!(
        parallax_confirmed, 0,
        "no Parallax finding may be confirmed without --execute-repros:\n{}",
        report["findings"]
    );
}
