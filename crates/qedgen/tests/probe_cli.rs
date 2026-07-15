//! CLI-surface tests for `qedgen probe` mode combinations (#225 Phase 0).
//!
//! Two invariants pinned here:
//! 1. Invalid / ambiguous flag combinations fail loudly through clap —
//!    no engine is ever silently skipped (`--program` used to win over
//!    `--fuzz` by dispatch order, dropping the requested fuzz run).
//! 2. Every probe envelope carries the same canonical schema version,
//!    whichever engine produced it (fuzz-mode outputs shipped a
//!    hardcoded `version: 1` against the v2 schema).

use std::path::PathBuf;
use std::process::{Command, Output};

fn qedgen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .args(args)
        .output()
        .expect("spawn qedgen")
}

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

/// `--program` + `--fuzz` used to silently skip the fuzz engine
/// (`--program` dispatches first and returns). Now a clap conflict.
#[test]
fn probe_program_plus_fuzz_is_rejected() {
    let out = qedgen(&["probe", "--program", "some/dir", "--fuzz", "60"]);
    assert!(!out.status.success(), "conflicting flags must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--fuzz") && stderr.contains("cannot be used with"),
        "expected clap conflict naming --fuzz, got:\n{stderr}"
    );
}

/// `--program` ignores `--root`; reject the pair instead of dropping it.
#[test]
fn probe_program_plus_root_is_rejected() {
    let out = qedgen(&["probe", "--program", "some/dir", "--root", "other/dir"]);
    assert!(!out.status.success(), "conflicting flags must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected clap conflict, got:\n{stderr}"
    );
}

#[test]
fn probe_program_plus_spec_is_rejected() {
    let out = qedgen(&["probe", "--program", "some/dir", "--spec", "x.qedspec"]);
    assert!(!out.status.success(), "conflicting flags must fail");
}

/// `--fuzz` without a target has a dedicated (non-clap) error that names
/// both valid pairings.
#[test]
fn probe_fuzz_without_spec_or_root_names_both_options() {
    let out = qedgen(&["probe", "--fuzz", "60"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--spec") && stderr.contains("--root"),
        "error must name both valid pairings, got:\n{stderr}"
    );
}

#[test]
fn probe_bootstrap_requires_root() {
    let out = qedgen(&["probe", "--bootstrap"]);
    assert!(!out.status.success());
}

/// Acceptance criterion (#225): fuzz and non-fuzz outputs use the same
/// canonical schema version. Budget-0 exercises the fuzz-mode envelope
/// without paying the Crucible build cost.
#[test]
fn fuzz_and_spec_probe_agree_on_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = tmp.path().join("counter.qedspec");
    std::fs::copy(fixture("descriptor/counter.qedspec"), &spec).unwrap();
    let spec_str = spec.to_str().unwrap();

    let spec_out = qedgen(&["probe", "--spec", spec_str]);
    assert!(
        spec_out.status.success(),
        "spec-aware probe failed:\n{}",
        String::from_utf8_lossy(&spec_out.stderr)
    );
    let fuzz_out = qedgen(&["probe", "--fuzz", "0", "--spec", spec_str]);
    assert!(
        fuzz_out.status.success(),
        "budget-0 fuzz probe failed:\n{}",
        String::from_utf8_lossy(&fuzz_out.stderr)
    );

    let spec_json: serde_json::Value = serde_json::from_slice(&spec_out.stdout).unwrap();
    let fuzz_json: serde_json::Value = serde_json::from_slice(&fuzz_out.stdout).unwrap();
    assert_eq!(
        spec_json["version"], fuzz_json["version"],
        "fuzz-mode envelope drifted from the canonical probe schema version"
    );
    // Pin the canonical value so both paths can't drift in lockstep by
    // accident; bump alongside probe::SCHEMA_VERSION on a conscious change.
    assert_eq!(spec_json["version"], serde_json::json!(3));
    // v3 (#227): budget-0 fuzz is a dry run, not a clean pass.
    assert_eq!(fuzz_json["outcome"], serde_json::json!("dry_run"));
    assert_eq!(
        fuzz_json["engine_runs"][0]["status"],
        serde_json::json!("blocked"),
        "budget-0 must report the fuzz engine as blocked, not passed"
    );
}

/// The headline #227 fix: a spec whose predicates fire but whose
/// reproducers aren't built yet must expose those hits as `candidates[]` —
/// NOT return an empty result indistinguishable from a clean spec. And a
/// candidate must never masquerade as a finding (no severity, no
/// reproducer, findings[] stays empty under the all-stubs constructors).
#[test]
fn spec_probe_preserves_predicate_hits_as_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = tmp.path().join("unbounded.qedspec");
    // `permissionless` + unbounded `amount` in a transfer → multiple
    // predicates fire (unbounded_amount_param, permissionless_state_writer).
    std::fs::write(
        &spec,
        r#"spec Drain

type State
  | Active

handler withdraw (amount : U64) : State.Active -> State.Active {
  permissionless
  effect { balance -= amount }
}
"#,
    )
    .unwrap();

    let out = qedgen(&["probe", "--spec", spec.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "probe failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let candidates = json["candidates"].as_array().expect("candidates[] present");
    assert!(
        !candidates.is_empty(),
        "predicate hits must surface as candidates, got empty:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    // Contract: candidates carry NO severity and NO reproducer.
    for c in candidates {
        assert!(
            c.get("severity").is_none(),
            "candidate must not carry severity"
        );
        assert!(
            c.get("reproducer").is_none(),
            "candidate must not carry a reproducer"
        );
        assert!(
            c["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "candidate must explain why it isn't a finding"
        );
    }
    // findings[] keeps its reproducer-only contract — empty while all
    // constructors are stubs.
    assert_eq!(
        json["findings"].as_array().map(Vec::len),
        Some(0),
        "no reproducer constructors exist yet, so findings must be empty"
    );
    // The predicate engine ran to completion and recorded the demotions.
    let engine = &json["engine_runs"][0];
    assert_eq!(engine["engine"], serde_json::json!("spec_predicates"));
    assert_eq!(engine["status"], serde_json::json!("passed"));
    assert!(
        engine["candidates_dropped"].as_u64().unwrap() >= 1,
        "engine run must account for the demoted candidates"
    );
    assert!(json["coverage"]["handlers_discovered"].as_u64().unwrap() >= 1);
}

// ---- #228: ArithmeticOverflowWrapping reproducer slice ----

/// Copy a fixture spec into a fresh tempdir so the generated
/// `target/qedgen-repros/` lands there, not in the source tree.
fn staged_spec(tmp: &std::path::Path, fixture_rel: &str, name: &str) -> PathBuf {
    let dst = tmp.join(name);
    std::fs::copy(fixture(fixture_rel), &dst).unwrap();
    dst
}

fn arith_hit<'a>(json: &'a serde_json::Value, key: &str) -> Vec<&'a serde_json::Value> {
    json[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|x| x["category_tag"] == "arithmetic_overflow_wrapping")
                .collect()
        })
        .unwrap_or_default()
}

/// Default path (`probe --spec`): the wrapping hit is a CANDIDATE carrying a
/// generated `repro_harness` pointer — the harness source is written, but
/// nothing is built or run (no compiled binary on disk) and no finding is
/// emitted. This is the agent-authored-repros default.
#[test]
fn arith_overflow_default_generates_harness_without_executing() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = staged_spec(
        tmp.path(),
        "repro-228/vulnerable.qedspec",
        "vulnerable.qedspec",
    );

    let out = qedgen(&["probe", "--spec", spec.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    // No finding (nothing was executed to confirm); the hit is a candidate.
    assert!(
        arith_hit(&json, "findings").is_empty(),
        "default must not emit a finding"
    );
    let candidates = arith_hit(&json, "candidates");
    assert_eq!(candidates.len(), 1, "wrapping hit must be a candidate");
    let harness = &candidates[0]["repro_harness"];
    assert_eq!(harness["kind"], "boundary_value");
    assert!(harness["path"].as_str().unwrap().ends_with("repro.rs"));
    assert!(harness["invocation"].as_str().unwrap().contains("rustc"));

    // The harness source was written; the compiled binary was NOT (no exec).
    let harness_rs = tmp.path().join(harness["path"].as_str().unwrap());
    assert!(harness_rs.exists(), "harness source must be generated");
    assert!(
        !harness_rs.with_file_name("repro").exists(),
        "default path must not build the harness"
    );

    // The reproducers engine reports it generated but did not run.
    let repro_engine = json["engine_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["engine"] == "reproducers")
        .expect("reproducers engine run present");
    assert_eq!(repro_engine["status"], "blocked");
}

/// `--execute-repros`: the CLI builds + runs the generated harness with
/// `rustc`; because the wrap reproduces (exit 0), the candidate is promoted to
/// a finding carrying a `BoundaryValue` reproducer.
#[test]
fn arith_overflow_execute_promotes_to_finding() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = staged_spec(
        tmp.path(),
        "repro-228/vulnerable.qedspec",
        "vulnerable.qedspec",
    );

    let out = qedgen(&[
        "probe",
        "--spec",
        spec.to_str().unwrap(),
        "--execute-repros",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let findings = arith_hit(&json, "findings");
    assert_eq!(
        findings.len(),
        1,
        "execution must promote the wrap to a finding"
    );
    let repro = &findings[0]["reproducer"];
    assert_eq!(repro["kind"], "boundary_value");
    assert!(repro["failing_input"]
        .as_str()
        .unwrap()
        .contains("u64::MAX"));
    // No arith candidate remains (it was promoted).
    assert!(arith_hit(&json, "candidates").is_empty());

    let repro_engine = json["engine_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["engine"] == "reproducers")
        .unwrap();
    assert_eq!(repro_engine["status"], "passed");
    assert!(repro_engine["detail"]
        .as_str()
        .unwrap()
        .contains("1 reproduced"));
}

/// The safe fixture uses the CHECKED operator (`+=`), so the wrapping
/// predicate never fires — no arith candidate and no finding, whether or not
/// execution is requested.
#[test]
fn arith_overflow_safe_fixture_produces_no_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = staged_spec(tmp.path(), "repro-228/safe.qedspec", "safe.qedspec");

    let out = qedgen(&[
        "probe",
        "--spec",
        spec.to_str().unwrap(),
        "--execute-repros",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    assert!(
        arith_hit(&json, "findings").is_empty(),
        "safe spec must emit no finding"
    );
    assert!(
        arith_hit(&json, "candidates").is_empty(),
        "safe spec must emit no candidate"
    );
}
