//! End-to-end smoke for v2.21 Slice 1 — `qedgen probe --fuzz <budget> --root <path>`.
//!
//! The test exercises the CLI gate-lift, brownfield runtime detection,
//! handler enumeration, and harness emission. Crucible binary + cargo
//! build are deliberately skipped via `--fuzz 0` (budget-0 emit-and-
//! exit). Validates the headline PRD-v2.21 exit criterion (S1.1 + S1.3
//! + S1.4 — minus the live fuzz run which requires Crucible on PATH).
//!
//! Sanity:
//! - `--fuzz` without `--spec` or `--root` errors clearly.
//! - `--fuzz 0 --root <anchor-crate>` emits `.qed/fuzz/<prog>/`.
//! - The emitted harness contains the PROTOCOL banner and an empty
//!   `invariant_test()` body.
//! - The emitted JSON envelope has `mode: spec_less` and 0 findings.

mod common;

use common::{ensure_qedgen_built, qedgen_bin, repo_root};
use std::path::Path;
use std::process::Command;

fn write_brownfield_anchor(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
anchor-lang = "0.30"
"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("src").join("lib.rs"),
        r#"use anchor_lang::prelude::*;
declare_id!("11111111111111111111111111111111");

#[program]
pub mod p {
    use super::*;
    pub fn run(ctx: Context<Empty>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }
    pub fn another(ctx: Context<Empty>, n: u64) -> Result<()> {
        let _ = (ctx, n);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Empty<'info> {
    x: AccountInfo<'info>,
}
"#,
    )
    .unwrap();
}

#[test]
fn fuzz_without_spec_or_root_errors_clearly() {
    ensure_qedgen_built();
    let out = Command::new(qedgen_bin())
        .args(["probe", "--fuzz", "0"])
        .output()
        .expect("spawn qedgen");
    assert!(
        !out.status.success(),
        "expected non-zero exit when neither --spec nor --root is given"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires either --spec") && stderr.contains("--root"),
        "error should name both flags; got: {stderr}"
    );
}

#[test]
fn fuzz_zero_brownfield_emits_protocol_harness() {
    ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("tmpdir");
    write_brownfield_anchor(tmp.path(), "buggy_anchor");

    let out = Command::new(qedgen_bin())
        .args(["probe", "--fuzz", "0", "--root"])
        .arg(tmp.path())
        .output()
        .expect("spawn qedgen");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "qedgen probe failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // 1. JSON envelope is spec_less mode with no findings.
    assert!(
        stdout.contains("\"mode\": \"spec_less\""),
        "expected spec_less envelope, got: {stdout}"
    );
    assert!(
        stdout.contains("\"findings\": []"),
        "expected empty findings, got: {stdout}"
    );

    // 2. Harness directory was emitted at .qed/fuzz/<prog>/.
    let harness = tmp.path().join(".qed/fuzz/buggy_anchor");
    assert!(
        harness.join("Cargo.toml").exists(),
        "Cargo.toml missing at {}",
        harness.display()
    );
    let main_rs =
        std::fs::read_to_string(harness.join("src").join("main.rs")).expect("read main.rs");
    assert!(
        main_rs.contains("Mode: PROTOCOL (no spec)"),
        "PROTOCOL banner missing from emitted harness"
    );
    assert!(
        main_rs.contains("fn invariant_test(_fixture:"),
        "protocol-mode emits empty invariant_test body with _fixture; got:\n{main_rs}"
    );
    // Action stubs for both discovered handlers.
    assert!(
        main_rs.contains("pub fn action_run"),
        "action_run stub missing"
    );
    assert!(
        main_rs.contains("pub fn action_another"),
        "action_another stub missing"
    );

    // 3. Budget-0 short-circuit message on stderr.
    assert!(
        stderr.contains("Budget = 0:"),
        "expected budget-0 message; got: {stderr}"
    );
}

#[test]
fn budget_zero_distinguishes_protocol_and_domain_assertion_surfaces() {
    ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("tmpdir");
    let program = tmp.path().join("program");
    write_brownfield_anchor(&program, "buggy_anchor");

    let protocol_harness = tmp.path().join("protocol").join("buggy_anchor");
    let protocol = Command::new(qedgen_bin())
        .args([
            "probe",
            "--fuzz",
            "0",
            "--crucible-mode",
            "protocol",
            "--root",
        ])
        .arg(&program)
        .arg("--harness-dir")
        .arg(&protocol_harness)
        .output()
        .expect("emit protocol harness");
    assert!(
        protocol.status.success(),
        "protocol emit failed: {}",
        String::from_utf8_lossy(&protocol.stderr)
    );

    let spec = tmp.path().join("domain.qedspec");
    std::fs::write(
        &spec,
        r#"spec DomainMode
program_id "11111111111111111111111111111111"

type State
  | Active of { count : U64 }

type Error | E

invariant domain_count_bounded :
  state.count <= 10

handler bump (delta : U64) : State.Active -> State.Active {
  invariant domain_count_bounded
  effect { count := count + delta }
}

"#,
    )
    .unwrap();
    let dossier = tmp.path().join("domain-dossier.json");
    std::fs::write(
        &dossier,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "schema_uri": "https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json",
            "asset_flows": [{
                "id": "domain_count_flow",
                "metadata": {
                    "ratification": "user",
                    "verification_lanes": ["crucible"]
                }
            }],
            "quantities": [],
            "paired_operations": [],
            "lifecycle_edges": [],
            "authority_capabilities": [],
            "economic_equations": [],
            "external_assumptions": []
        }))
        .unwrap(),
    )
    .unwrap();
    let domain_harness = tmp.path().join("domain").join("domain_mode");
    let domain = Command::new(qedgen_bin())
        .args([
            "probe",
            "--fuzz",
            "0",
            "--crucible-mode",
            "domain",
            "--spec",
        ])
        .arg(&spec)
        .arg("--domain-dossier")
        .arg(&dossier)
        .arg("--harness-dir")
        .arg(&domain_harness)
        .output()
        .expect("emit domain harness");
    assert!(
        domain.status.success(),
        "domain emit failed: {}",
        String::from_utf8_lossy(&domain.stderr)
    );

    let protocol_body = std::fs::read_to_string(protocol_harness.join("src/main.rs")).unwrap();
    let domain_body = std::fs::read_to_string(domain_harness.join("src/main.rs")).unwrap();
    assert!(protocol_body.contains("Mode: PROTOCOL (no spec)"));
    assert!(protocol_body.contains("fn invariant_test(_fixture:"));
    assert!(protocol_body.contains("fn assert_no_wallet_inflation"));
    assert!(!protocol_body.contains("domain_count_bounded"));
    assert!(domain_body.contains("Mode: SPEC + PROTOCOL"));
    assert!(domain_body.contains("fn invariant_test(fixture:"));
    assert!(domain_body.contains("Invariant: domain_count_bounded"));
    assert!(domain_body.contains("invariant domain_count_bounded violated"));
    assert!(domain_body.contains("fn assert_no_wallet_inflation"));
    assert!(String::from_utf8_lossy(&protocol.stdout).contains("\"mode\": \"spec_less\""));
    assert!(String::from_utf8_lossy(&domain.stdout).contains("\"mode\": \"spec_aware\""));
}

/// Pins that the committed v2.21 fixture under
/// `crates/qedgen/tests/fixtures/regressions/v2.21-crucible-crash-first/buggy_anchor/`
/// drives the brownfield path end-to-end. Catches regressions that
/// would otherwise only surface when a user manually copies the
/// fixture out of the repo.
#[test]
fn fixture_buggy_anchor_drives_brownfield_emit() {
    ensure_qedgen_built();
    // Copy the fixture out of the repo into a tempdir so emitted
    // `.qed/fuzz/` doesn't pollute the working tree.
    let src = repo_root()
        .join("crates/qedgen/tests/fixtures/regressions/v2.21-crucible-crash-first/buggy_anchor");
    assert!(
        src.exists(),
        "fixture missing at {} — did the v2.21 commit land?",
        src.display()
    );
    let tmp = tempfile::tempdir().expect("tmpdir");
    let dst = tmp.path().join("buggy_anchor");
    copy_dir_recursive(&src, &dst);

    let out = Command::new(qedgen_bin())
        .args(["probe", "--fuzz", "0", "--root"])
        .arg(&dst)
        .output()
        .expect("spawn qedgen");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "fixture brownfield emit failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let main_rs = dst.join(".qed/fuzz/buggy_anchor/src/main.rs");
    let body = std::fs::read_to_string(&main_rs).expect("read emitted main.rs");
    assert!(body.contains("pub fn action_run"));
    assert!(body.contains("pub fn action_maybe"));
    assert!(body.contains("pub fn action_drain"));
    assert!(body.contains("Mode: PROTOCOL"));
    // Protocol-mode harness carries the shared snapshot infra AND, now that
    // buggy_anchor ships a committed idl.json, the IDL-driven path fills the
    // `accounts::*` literals and wires the per-action guard suite. `drain`'s
    // `authority` is a signer (part of the wallet set) and `vault` is a
    // program-owned PDA the harness stages so the drain can actually debit it.
    assert!(
        body.contains("fn snapshot_account_state") && body.contains("struct AccountSnapshot"),
        "protocol-mode brownfield harness must emit the shared snapshot infra"
    );
    // IDL-driven account discovery: `drain` auto-fills its accounts literal
    // (no `todo!()`) and the guard suite wraps the send — this is what makes
    // the fixture fire a real finding under `--fuzz <budget>`.
    assert!(
        body.contains("accounts(accounts::Drain {"),
        "drain's accounts literal should be auto-filled from the IDL, not todo!()"
    );
    // Full protocol suite emitted + the wallet-lamport and ownership guards
    // wired around .send(). The account-set guards track the program-owned
    // vault PDA, so a handler reassigning it out of program ownership trips
    // the ownership guard even though no wallet gains lamports.
    for assert_fn in [
        "fn assert_no_wallet_inflation",
        "fn assert_lamports_conserved",
        "fn assert_no_ownership_takeover",
        "fn assert_no_discriminator_change",
        "fn assert_closure_integrity",
        "fn assert_rent_exemption_preserved",
        "fn assert_no_realloc_data_leak",
        "fn assert_token_balance_conserved",
    ] {
        assert!(
            body.contains(assert_fn),
            "protocol suite must emit `{assert_fn}`"
        );
    }
    assert!(
        body.contains("assert_no_wallet_inflation(&self.ctx, &__wallet_snap,"),
        "drain should wire the wallet-lamport guard around .send()"
    );
    assert!(
        body.contains("assert_no_ownership_takeover(&self.ctx, &__account_snap,"),
        "drain should wire the ownership-takeover guard around .send()"
    );
    assert!(
        !body.contains("todo!("),
        "IDL-driven brownfield emit should leave no todo!() in the action bodies"
    );
    // The `vault` PDA must be staged program-owned + funded so the program
    // can debit it — without this, drain errors and nothing fires.
    assert!(
        body.contains(".owner(program_id)"),
        "vault PDA should be created program-owned in setup()"
    );
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// v2.22 Slice 3 — Pinocchio brownfield Crucible fuzz
// ─────────────────────────────────────────────────────────────────────

/// Pins that the committed v2.22 fixture under
/// `crates/qedgen/tests/fixtures/regressions/v2.22-pinocchio-brownfield-fuzz/buggy_pinocchio/`
/// drives the Pinocchio brownfield path end-to-end.
///
/// Validates:
/// - `--fuzz 0 --root <pinocchio-crate>` succeeds (no runtime gate).
/// - The harness is emitted at `.qed/fuzz/buggy_pinocchio/`.
/// - The Codama IDL is copied to `<harness>/idls/buggy_pinocchio.json`.
/// - The harness Carries action stubs for the IDL's three instructions
///   (`run`, `maybe`, `drain`) — handler names are derived from the
///   IDL's `instructions[].name`, prefixed with `process_` to match
///   the Pinocchio source convention.
#[test]
fn fixture_buggy_pinocchio_drives_brownfield_emit() {
    ensure_qedgen_built();
    let src = repo_root().join(
        "crates/qedgen/tests/fixtures/regressions/v2.22-pinocchio-brownfield-fuzz/buggy_pinocchio",
    );
    assert!(
        src.exists(),
        "fixture missing at {} — did the v2.22 commit land?",
        src.display()
    );
    let tmp = tempfile::tempdir().expect("tmpdir");
    let dst = tmp.path().join("buggy_pinocchio");
    copy_dir_recursive(&src, &dst);

    let out = Command::new(qedgen_bin())
        .args(["probe", "--fuzz", "0", "--root"])
        .arg(&dst)
        .output()
        .expect("spawn qedgen");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "Pinocchio brownfield emit failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let harness = dst.join(".qed/fuzz/buggy_pinocchio");
    assert!(
        harness.join("Cargo.toml").exists(),
        "harness Cargo.toml missing at {}",
        harness.display()
    );

    // IDL was copied through to the harness.
    let harness_idl = harness.join("idls/buggy_pinocchio.json");
    assert!(
        harness_idl.exists(),
        "synthesised IDL not written at {}",
        harness_idl.display()
    );
    let idl_text = std::fs::read_to_string(&harness_idl).expect("read harness IDL");
    assert!(
        idl_text.contains("BgyP1n"),
        "harness IDL should be the maintainer-authored Codama IDL (address prefix BgyP1n); got: {idl_text}"
    );

    // Action stubs derived from the IDL's three instructions. Handler
    // names come from the IDL `instructions[].name` field (no
    // `process_` prefix — the harness emitter PascalCases the handler
    // name to build `instruction::Foo` literals that must match the
    // declare_fuzz_program! macro's type names).
    let main_rs = std::fs::read_to_string(harness.join("src/main.rs")).expect("read main.rs");

    // Macro line uses the explicit `name = "path"` form so the generated
    // module is named `buggy_pinocchio` regardless of the IDL's internal
    // `program.name` casing (Codama IR camelCases program names; without
    // the override the `use buggy_pinocchio::*` lines below the macro
    // would unresolve when the IDL declares `buggyPinocchio`).
    assert!(
        main_rs.contains(
            "crucible_idl_gen::declare_fuzz_program!(buggy_pinocchio = \"idls/buggy_pinocchio.json\")"
        ),
        "expected explicit-name declare_fuzz_program! invocation; got:\n{main_rs}"
    );
    for action in ["action_run", "action_maybe", "action_drain"] {
        assert!(
            main_rs.contains(action),
            "expected `{action}` in emitted harness; got:\n{main_rs}"
        );
    }
    assert!(
        main_rs.contains("Mode: PROTOCOL"),
        "Pinocchio brownfield should emit PROTOCOL-mode harness"
    );
    // Its accounts (`source`, `target`) carry no default address or PDA
    // seed, so the old `spec_is_brownfield_with_idl_accounts` proxy
    // mis-classified this program as spec-mode and emitted NO keypairs —
    // both protocol guards then had an empty tracked set and fired nothing
    // (a green-but-vacuous test). Since Protocol mode is now treated as
    // brownfield unconditionally, `source`/`target` get fixture keypairs and
    // the ownership-takeover guard wires a real tracked set + per-action
    // check. Assert the guard is WIRED, not merely that a helper is present.
    assert!(
        main_rs.contains("source: Rc<Keypair>,") && main_rs.contains("target: Rc<Keypair>,"),
        "plain writable accounts must get fixture keypairs in Protocol mode:\n{main_rs}"
    );
    assert!(
        main_rs.contains("let __account_snap = snapshot_account_state(&self.ctx, &__account_set);"),
        "account-set guards must snapshot a non-empty tracked set per action:\n{main_rs}"
    );
    assert!(
        main_rs.contains("assert_no_ownership_takeover(&self.ctx, &__account_snap, \"drain\")"),
        "drain must wire the ownership-takeover check (not vacuous):\n{main_rs}"
    );
}

#[test]
fn pinocchio_brownfield_bails_without_idl_on_disk() {
    ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\npinocchio = \"0.6\"\n",
    )
    .unwrap();
    // Source has no `process_*` handlers AND no idl.json — gate should
    // bail clearly pointing at Codama.
    std::fs::write(tmp.path().join("src/lib.rs"), "// no IDL\n").unwrap();
    let out = Command::new(qedgen_bin())
        .args(["probe", "--fuzz", "0", "--root"])
        .arg(tmp.path())
        .output()
        .expect("spawn qedgen");
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Codama") && stderr.contains("codama"),
        "Pinocchio bail should cite Codama and the codama CLI; got: {stderr}"
    );
}

#[test]
fn brownfield_errors_when_no_anchor_handlers_found() {
    ensure_qedgen_built();
    let tmp = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"empty\"\nversion = \"0.1.0\"\n\n[dependencies]\nanchor-lang = \"0.30\"\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("src").join("lib.rs"), "// no handlers\n").unwrap();
    let out = Command::new(qedgen_bin())
        .args(["probe", "--fuzz", "0", "--root"])
        .arg(tmp.path())
        .output()
        .expect("spawn qedgen");
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("No `pub fn"),
        "expected handler-discovery error; got: {stderr}"
    );
}
