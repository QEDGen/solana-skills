//! Probe regression corpus (#231 — in-repo half; recall/precision
//! *measurement* stays in the auditor-bench skill per
//! `feedback_bench_stays_a_skill`).
//!
//! Two fixture families under `tests/fixtures/probe-corpus/`:
//!
//! - `spec/<category>/{vulnerable,safe,hard_safe}.qedspec` — one triple per
//!   spec-aware predicate. The contract, asserted per fixture:
//!   vulnerable → emits its own category tag (candidate or finding);
//!   safe / hard_safe → does NOT emit its own category tag; and NO
//!   safe/hard_safe fixture emits a reproducer-backed `finding`
//!   (`findings[]`) of any category — a false *confirmed finding* is a
//!   release blocker (#231), whereas a stray cross-category *candidate* on a
//!   minimal multi-handler spec is legitimate probe output (the predicates
//!   compose), so cross-category candidates are not asserted against.
//!   hard_safe fixtures are specifically shaped to fool a naive
//!   token/name-matching scanner while staying safe under the real
//!   structural predicate.
//!
//! - `specless/<scenario>/` — brownfield project trees exercising the #235
//!   IDL-enrichment overlay end to end (enrich, framework-enforced
//!   narrowing, source/IDL drift, Pinocchio handler fill, derivable-IDL
//!   hint on a marker-without-IDL and on an unbuilt framework checkout,
//!   the #240 dead-guard / unwired-error-variant sweep, and the #241
//!   workspace-member ancestor walk to a repo-root `idl/`).
//!
//! These are pass/fail *regression* tests, not a metric: they pin that a
//! shipped predicate keeps firing on its vuln fixture and stays silent on
//! its safe/hard_safe fixtures across refactors.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe-corpus")
}

/// Run `qedgen probe <args...>` and parse stdout as the schema-v3 envelope.
fn probe(args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("probe")
        .args(args)
        .output()
        .expect("spawn qedgen probe");
    assert!(
        out.status.success(),
        "probe {args:?} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "probe {args:?} stdout was not valid JSON: {e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn tags_of<'a>(env: &'a Value, key: &str) -> Vec<&'a str> {
    env.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("category_tag").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

/// The eight spec-aware predicate categories, each with a fixture triple.
const SPEC_CATEGORIES: &[&str] = &[
    "missing_signer",
    "arithmetic_overflow_wrapping",
    "unbounded_amount_param",
    "permissionless_state_writer",
    "lifecycle_one_shot_violation",
    "init_without_pda",
    "stored_field_never_written",
    "arbitrary_cpi",
];

fn spec_path(category: &str, variant: &str) -> String {
    corpus_dir()
        .join("spec")
        .join(category)
        .join(format!("{variant}.qedspec"))
        .display()
        .to_string()
}

#[test]
fn vulnerable_fixtures_emit_their_own_category() {
    for &cat in SPEC_CATEGORIES {
        let env = probe(&["--spec", &spec_path(cat, "vulnerable")]);
        let mut emitted = tags_of(&env, "candidates");
        emitted.extend(tags_of(&env, "findings"));
        assert!(
            emitted.contains(&cat),
            "vulnerable fixture for `{cat}` must emit its own category tag; \
             got candidates+findings = {emitted:?}"
        );
    }
}

#[test]
fn safe_fixtures_do_not_emit_their_own_category() {
    for &cat in SPEC_CATEGORIES {
        for variant in ["safe", "hard_safe"] {
            let env = probe(&["--spec", &spec_path(cat, variant)]);
            let mut emitted = tags_of(&env, "candidates");
            emitted.extend(tags_of(&env, "findings"));
            assert!(
                !emitted.contains(&cat),
                "`{variant}` fixture for `{cat}` must NOT emit its own category tag; \
                 got candidates+findings = {emitted:?}"
            );
        }
    }
}

/// The hard release bar (#231): no safe/hard_safe fixture may produce a
/// reproducer-backed `finding` of ANY category. A false *confirmed finding*
/// is a release blocker; a stray cross-category *candidate* is not.
#[test]
fn no_safe_fixture_emits_a_confirmed_finding() {
    for &cat in SPEC_CATEGORIES {
        for variant in ["safe", "hard_safe"] {
            let env = probe(&["--spec", &spec_path(cat, variant)]);
            let findings = tags_of(&env, "findings");
            assert!(
                findings.is_empty(),
                "`{variant}` fixture for `{cat}` emitted confirmed finding(s) {findings:?} — \
                 a false confirmed finding is a release blocker"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Spec-less IDL-enrichment overlay (#235) — end-to-end scenarios.
// ---------------------------------------------------------------------------

fn specless_root(scenario: &str) -> String {
    corpus_dir()
        .join("specless")
        .join(scenario)
        .display()
        .to_string()
}

fn handlers_of(env: &Value) -> &Vec<Value> {
    static EMPTY: Vec<Value> = Vec::new();
    env.get("handlers")
        .and_then(Value::as_array)
        .unwrap_or(&EMPTY)
}

fn handler<'a>(env: &'a Value, name: &str) -> &'a Value {
    handlers_of(env)
        .iter()
        .find(|h| h.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("handler `{name}` not in envelope"))
}

/// Anchor + on-disk IDL: `idl_path` reported; enforced signer flags narrow
/// untagged handlers (admin → authority_gated, no-signer crank →
/// permissionless); a source handler absent from the IDL and an IDL
/// instruction absent from source both surface as drift.
#[test]
fn anchor_idl_overlay_enriches_narrows_and_reports_drift() {
    let env = probe(&["--bootstrap", "--root", &specless_root("anchor-idl")]);

    assert_eq!(
        env.get("idl_path").and_then(Value::as_str),
        Some("target/idl/vault.json")
    );

    let init = handler(&env, "initialize");
    assert_eq!(
        init.get("intent_tag").and_then(Value::as_str),
        Some("authority_gated"),
        "signer `admin` should narrow initialize to authority_gated"
    );
    assert!(
        init.get("applicable_categories").is_some(),
        "authority_gated handler should carry a narrowed category list"
    );
    let signers: Vec<&str> = init
        .get("idl_accounts")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|a| a.get("signer").and_then(Value::as_bool) == Some(true))
        .filter_map(|a| a.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(signers, vec!["admin"]);

    let crank = handler(&env, "crank");
    assert_eq!(
        crank.get("intent_tag").and_then(Value::as_str),
        Some("permissionless"),
        "no declared signer should narrow crank to permissionless"
    );

    // Drift: `emergency_withdraw` (source-only) and `reconcile` (IDL-only).
    let drift: Vec<&str> = env
        .get("candidates")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|c| c.get("category_tag").and_then(Value::as_str) == Some("idl_source_drift"))
        .filter_map(|c| c.get("handler").and_then(Value::as_str))
        .collect();
    assert!(
        drift.contains(&"emergency_withdraw"),
        "source-only drift missing: {drift:?}"
    );
    assert!(
        drift.contains(&"reconcile"),
        "idl-only drift missing: {drift:?}"
    );
}

/// Spec elicitation (PRD Phases 0–1): every spec-less envelope carries a
/// `run_id` + `spec_readiness`, and the hypothesizer fires only on
/// evidence-anchored claims — `initialize` gets an authorization
/// hypothesis (single authority-named enforced signer) and a
/// lifecycle_init_once hypothesis (Anchor `init` constraint), while the
/// permissionless `crank` and the evidence-free `emergency_withdraw`
/// yield nothing (precision rule: no anchor → no hypothesis).
#[test]
fn specless_envelope_carries_evidence_anchored_hypotheses() {
    let env = probe(&["--bootstrap", "--root", &specless_root("anchor-idl")]);

    let run_id = env.get("run_id").and_then(Value::as_str).unwrap();
    assert!(run_id.starts_with("run-"), "run_id: {run_id}");

    let readiness = env.get("spec_readiness").expect("spec_readiness present");
    assert_eq!(readiness["hypotheses_total"], 3, "{readiness:#}");
    assert_eq!(readiness["lowerable"], 3);
    assert_eq!(readiness["by_class"]["authorization"], 1);
    assert_eq!(readiness["by_class"]["lifecycle_init_once"], 1);
    assert_eq!(readiness["by_class"]["arithmetic_bound"], 1);

    let hypotheses = env
        .get("hypotheses")
        .and_then(Value::as_array)
        .expect("hypotheses present");
    let auth = hypotheses
        .iter()
        .find(|h| h["class"] == "authorization")
        .expect("authorization hypothesis");
    assert_eq!(auth["handler"], "initialize");
    assert_eq!(auth["lowering"]["kind"], "auth_clause");
    assert_eq!(auth["lowering"]["signer_account"], "admin");
    assert!(auth["id"].as_str().unwrap().starts_with("h-"));
    // Phase 4: the IDL `has_one` relation (vault → admin) is a
    // stored-authority binding, lifting confidence to high.
    assert!(auth["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "idl_has_one_binding"));
    assert_eq!(auth["confidence"], "high");

    let lifecycle = hypotheses
        .iter()
        .find(|h| h["class"] == "lifecycle_init_once")
        .expect("lifecycle hypothesis");
    assert_eq!(lifecycle["handler"], "initialize");
    assert!(lifecycle["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "anchor_init_constraint"));

    // Phase 5: the held `require!(cap <= 1_000_000, …)` bound lifts into
    // an executable arithmetic_bound hypothesis.
    let arith = hypotheses
        .iter()
        .find(|h| h["class"] == "arithmetic_bound")
        .expect("arithmetic_bound hypothesis");
    assert_eq!(arith["handler"], "initialize");
    assert_eq!(arith["lowering"]["kind"], "requires_bound");
    assert_eq!(arith["lowering"]["param"], "cap");
    assert_eq!(arith["lowering"]["bound"], "1000000");
    assert_eq!(arith["lowering"]["error"], "CapTooHigh");

    // Precision: no hypothesis for the permissionless or evidence-free
    // handlers.
    assert!(hypotheses
        .iter()
        .all(|h| h["handler"] != "crank" && h["handler"] != "emergency_withdraw"));
}

/// Pinocchio: source discovery yields no handlers, so the Codama IDL fills
/// `handlers[]` (each `discovered_via: "idl"`, `source_file` = the IDL).
#[test]
fn pinocchio_codama_overlay_fills_handlers_from_idl() {
    let env = probe(&["--program", &specless_root("pinocchio-codama")]);

    assert_eq!(
        env.get("runtime").and_then(Value::as_str),
        Some("pinocchio")
    );
    assert_eq!(
        env.get("idl_path").and_then(Value::as_str),
        Some("idl.json")
    );

    let names: Vec<&str> = handlers_of(&env)
        .iter()
        .filter_map(|h| h.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"increment"), "handlers: {names:?}");
    assert!(names.contains(&"reset"), "handlers: {names:?}");

    for h in handlers_of(&env) {
        assert_eq!(
            h.get("discovered_via").and_then(Value::as_str),
            Some("idl"),
            "Pinocchio handler `{:?}` should be IDL-discovered",
            h.get("name")
        );
    }
}

/// #241: a workspace member probed at program-dir granularity
/// (`--root programs/foo`) with its IDL at the *repo* root (`idl/foo.json`)
/// still resolves `idl_path` via the ancestor walk and applies the overlay —
/// previously this cwd reported `derivable_idl: "anchor"` with no `idl_path`
/// while probing the repo root enriched fine.
#[test]
fn workspace_member_program_root_finds_repo_root_idl() {
    let program_root = PathBuf::from(specless_root("anchor-workspace-member"))
        .join("programs")
        .join("foo")
        .display()
        .to_string();
    let env = probe(&["--bootstrap", "--root", &program_root]);

    assert_eq!(env.get("runtime").and_then(Value::as_str), Some("anchor"));
    let idl_path = env
        .get("idl_path")
        .and_then(Value::as_str)
        .expect("idl_path must resolve from the workspace root");
    assert!(
        idl_path.ends_with("idl/foo.json"),
        "unexpected idl_path: {idl_path}"
    );
    assert!(
        env.get("derivable_idl").is_none(),
        "a consumed IDL must suppress the derivable hint"
    );

    // The overlay applies as if the IDL were local: enrichment + narrowing.
    let init = handler(&env, "initialize");
    assert_eq!(
        init.get("intent_tag").and_then(Value::as_str),
        Some("authority_gated")
    );
    assert!(init.get("idl_accounts").is_some());
    assert_eq!(
        handler(&env, "crank")
            .get("intent_tag")
            .and_then(Value::as_str),
        Some("permissionless")
    );
}

/// A `shank` dep with no on-disk IDL reports `derivable_idl: "shank"` and no
/// `idl_path` — the overlay does not shell out, it hints.
#[test]
fn shank_marker_without_idl_reports_derivable() {
    let env = probe(&[
        "--bootstrap",
        "--root",
        &specless_root("native-shank-marker"),
    ]);
    assert_eq!(env.get("runtime").and_then(Value::as_str), Some("native"));
    assert!(env.get("idl_path").is_none(), "no IDL is on disk");
    assert_eq!(
        env.get("derivable_idl").and_then(Value::as_str),
        Some("shank")
    );
}

/// An unbuilt Anchor checkout (no `target/idl`, no shank/codama markers)
/// reports `derivable_idl: "anchor"` — the runtime itself says an IDL is one
/// `anchor build` away (#238).
#[test]
fn unbuilt_anchor_without_idl_reports_derivable_anchor() {
    let env = probe(&["--bootstrap", "--root", &specless_root("anchor-unbuilt")]);
    assert_eq!(env.get("runtime").and_then(Value::as_str), Some("anchor"));
    assert!(env.get("idl_path").is_none(), "no IDL is on disk");
    assert_eq!(
        env.get("derivable_idl").and_then(Value::as_str),
        Some("anchor")
    );
}

/// #240 dead-guard sweep: an `#[error_code]` variant defined but wired into
/// no enforcement call-site surfaces as an `unwired_error_variant` candidate;
/// variants that ARE enforced (require! / return Err) stay silent. The sweep
/// runs on the spec-less envelope (a source lens the bootstrap previously
/// lacked) and emits candidates, never reproducer-backed findings.
#[test]
fn unwired_error_variant_surfaces_as_candidate() {
    let env = probe(&[
        "--bootstrap",
        "--root",
        &specless_root("anchor-unwired-guard"),
    ]);

    let unwired: Vec<&str> = env
        .get("candidates")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|c| c.get("category_tag").and_then(Value::as_str) == Some("unwired_error_variant"))
        .filter_map(|c| c.get("handler").and_then(Value::as_str))
        .collect();

    // Exactly the defined-but-unenforced variant; the two wired ones are silent.
    assert_eq!(
        unwired,
        vec!["HookAuthorityCannotBePartOfHookAccounts"],
        "got {unwired:?}"
    );

    // A dead guard is an absence — it must be a candidate, never a finding.
    let findings = tags_of(&env, "findings");
    assert!(
        !findings.contains(&"unwired_error_variant"),
        "unwired variant must not be a reproducer-backed finding: {findings:?}"
    );
}
