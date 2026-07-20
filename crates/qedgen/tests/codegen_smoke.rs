mod common;

use common::{redirect_macros_to_path, repo_root};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(command: &mut Command) {
    let output = command.output().expect("failed to spawn command");
    if !output.status.success() {
        panic!(
            "command failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn run_capture(command: &mut Command) -> String {
    let output = command.output().expect("failed to spawn command");
    if !output.status.success() {
        panic!(
            "command failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

// The Anchor scaffold/proptest smokes moved to
// `tests/generated_artifact_gate.rs` (#294): the gate covers all bundled
// Anchor examples and raises the floor from "scaffold compiles" to "every
// generated Rust artifact compiles and its tests run".

/// Generate `<spec>` as a Pinocchio scaffold into a fresh tempdir and run
/// `cargo build` on it. The Pinocchio path is MIR-native (slice 6): the
/// scaffold emits lib + entrypoint + byte-dispatch, zeropod state, guards,
/// errors, checked effects, and SPL Token CPIs (`call Token.transfer(...)`
/// → `pinocchio_token::instructions::Transfer { … }.invoke()?;`).
///
/// `cargo build` (not `check`) so the `#![no_std]` + `entrypoint!` crate is
/// exercised through codegen. The spec carries an inline SPL `interface`,
/// so no `qed.toml` is needed. Regenerating from the committed spec (rather
/// than building a checked-in tree) keeps the gate testing *current*
/// codegen output — it can't silently drift.
fn smoke_pinocchio_scaffold(fixture: &str, spec_file: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_src = repo_root()
        .join("crates/qedgen/tests/fixtures/pinocchio-fixtures")
        .join(fixture)
        .join(spec_file);
    let spec_path = temp.path().join(spec_file);
    std::fs::copy(&spec_src, &spec_path).unwrap_or_else(|e| panic!("copy {fixture} spec: {e}"));
    std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");

    run(Command::new("git").arg("init").current_dir(temp.path()));

    let output_dir = temp.path().join("programs");
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("pinocchio")
        .arg("--output-dir")
        .arg(&output_dir)
        .current_dir(temp.path()));

    // Same unreleased-tag problem as the Anchor smoke: rewrite the
    // `qedgen-macros` git dep to the in-repo path dep before compiling.
    redirect_macros_to_path(&output_dir.join("Cargo.toml"));

    run(Command::new("cargo")
        .arg("build")
        .arg("--manifest-path")
        .arg(output_dir.join("Cargo.toml")));
}

/// Generate a Pinocchio program and its impl-targeted Kani proof, then run
/// real `cargo kani` against the generated `src/kani_impl.rs`. This is kept
/// ignored so normal CI does not need cargo-kani installed, but optional CI can
/// exercise the exact generated proof path instead of only snapshots.
fn smoke_pinocchio_generated_kani_impl(fixture: &str, spec_file: &str, harness: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_src = repo_root()
        .join("crates/qedgen/tests/fixtures/pinocchio-fixtures")
        .join(fixture)
        .join(spec_file);
    let spec_path = temp.path().join(spec_file);
    std::fs::copy(&spec_src, &spec_path).unwrap_or_else(|e| panic!("copy {fixture} spec: {e}"));
    std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");

    run(Command::new("git").arg("init").current_dir(temp.path()));

    let output_dir = temp.path().join("programs");
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("pinocchio")
        .arg("--output-dir")
        .arg(&output_dir)
        .current_dir(temp.path()));

    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("pinocchio")
        .arg("--kani-impl")
        .arg("--kani-impl-output")
        .arg(output_dir.join("tests/kani_impl.rs"))
        .current_dir(temp.path()));

    let kani_impl = output_dir.join("src/kani_impl.rs");
    let body = std::fs::read_to_string(&kani_impl).expect("read generated kani_impl.rs");
    assert!(
        !body.contains("TODO:"),
        "generated proof path should not present placeholder TODOs as green Kani evidence:\n{body}"
    );
    assert!(
        body.contains("Proof profile notes:"),
        "generated proof should include profile diagnostics:\n{body}"
    );

    redirect_macros_to_path(&output_dir.join("Cargo.toml"));

    run(Command::new("cargo")
        .arg("kani")
        .arg("--harness")
        .arg(harness)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(&output_dir));
}

fn copy_dir_contents(src: &Path, dst: &Path) {
    for file in walk_files(src) {
        let relative = file.strip_prefix(src).expect("fixture file under root");
        let target = dst.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::copy(&file, &target)
            .unwrap_or_else(|e| panic!("copy fixture file {}: {e}", file.display()));
    }
}

fn generated_harness_body<'a>(body: &'a str, harness: &str) -> &'a str {
    let start = body
        .find(&format!("fn {harness}()"))
        .unwrap_or_else(|| panic!("missing generated harness `{harness}`:\n{body}"));
    let rest = &body[start..];
    let next = rest
        .find("\n#[kani::proof]")
        .or_else(|| rest.find("\n}\n\n///"))
        .unwrap_or(rest.len());
    &rest[..next]
}

fn assert_green_harness_contract(
    body: &str,
    harness: &str,
    required_postcondition_snippets: &[&str],
) {
    let harness_body = generated_harness_body(body, harness);
    let compact_body = compact_rust(harness_body);
    assert!(
        harness_body.contains("let _result = crate::process_instruction("),
        "{harness} must call the real Pinocchio dispatcher:\n{harness_body}"
    );
    assert!(
        compact_body.contains("kani::cover!(_result.is_ok()"),
        "{harness} should retain supplemental success-path cover evidence:\n{harness_body}"
    );
    assert!(
        compact_body.contains("assert!(_result.is_ok()"),
        "{harness} must assert success; cover-only evidence is not proof completion:\n{harness_body}"
    );
    for snippet in required_postcondition_snippets {
        assert!(
            harness_body.contains(snippet),
            "{harness} is missing required concrete post-state assertion `{snippet}`:\n{harness_body}"
        );
    }
}

fn compact_rust(body: &str) -> String {
    body.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn smoke_pinocchio_kani_profile_diversity() {
    let body = render_pinocchio_kani_profile_diversity_impl();

    assert_pinocchio_kani_profile_diversity_contract(&body);
}

fn render_pinocchio_kani_profile_diversity_impl() -> String {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_root = repo_root()
        .join("crates/qedgen/tests/fixtures/pinocchio-fixtures")
        .join("kani-profile-diversity");
    copy_dir_contents(&fixture_root, temp.path());
    std::fs::create_dir(temp.path().join(".qed")).expect("create root .qed");
    std::fs::create_dir(temp.path().join("verification/.qed")).expect("create spec .qed");

    run(Command::new("git").arg("init").current_dir(temp.path()));

    let spec_path = temp.path().join("verification/profile.qedspec");
    let kani_impl = temp.path().join("src/kani_impl.rs");
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("pinocchio")
        .arg("--kani-impl")
        .arg("--kani-impl-output")
        .arg(&kani_impl)
        .current_dir(temp.path()));

    std::fs::read_to_string(&kani_impl).expect("read generated kani_impl.rs")
}

fn assert_pinocchio_kani_profile_diversity_contract(body: &str) {
    // Generated Rust is rustfmt-formatted at the write seam, so code
    // snippets are matched whitespace-insensitively; comment/prose needles
    // survive compaction too (rustfmt does not rewrap comments).
    let compact_body = compact_rust(body);
    let has = |snippet: &str| compact_body.contains(&compact_rust(snippet));

    assert!(
        has("fn verify_move_tokens_impl"),
        "missing move_tokens impl harness:\n{body}"
    );
    assert!(
        has("let pre_transfer_0_from = read_token_amount(&source);")
            && has("let pre_transfer_0_to = read_token_amount(&destination);")
            && has("kani::assume(pre_transfer_0_from >= (amount as u64));")
            && has("kani::assume(pre_transfer_0_to <= u64::MAX - (amount as u64));")
            && has(
                "assert_eq!(read_token_amount(&source), pre_transfer_0_from - (amount as u64));"
            )
            && has(
                "assert_eq!(read_token_amount(&destination), pre_transfer_0_to + (amount as u64));"
            ),
        "simple SPL token transfer should emit concrete token delta assertions:\n{body}"
    );
    assert!(
        has("token owner/mint projections:")
            && has("source mint=mint owner=authority")
            && has("destination mint=mint owner=authority"),
        "token owner/mint profile notes should explain inferred token account projections:\n{body}"
    );

    assert!(
        has("fn verify_move_batch_impl")
            && has("kani::assume(transfer_count <= 2);")
            && has("instruction_data[1] = 2u8;")
            && has("instruction_data[2] = (from_lane_id_0 as u8) as u8;")
            && has("let generated_instruction_data_4_bytes = (amount_0 as u64).to_le_bytes();")
            && has("instruction_data[12] = (from_lane_id_1 as u8) as u8;")
            && has("let generated_instruction_data_14_bytes = (amount_1 as u64).to_le_bytes();"),
        "repeated record batch should pack indexed fields from the ABI schema:\n{body}"
    );

    assert!(
        has("fn verify_touch_config_impl")
            && has("// ABI account layout `config_account`: 237 byte data region.")
            && has("let mut config_data: [u8; 237] = [0u8; 237];")
            && has("config_data[0] = 67u8;")
            && has("config_data[7] = 67u8;"),
        "ABI data account should allocate schema length and stamp CFGMAGIC bytes:\n{body}"
    );

    assert!(
        has("fn verify_set_fee_impl")
            && has("- source account order: config, admin")
            && has("- ABI/dispatcher tag: 6")
            && has("assert!(_result.is_ok()")
            && has("assert_eq!(read_state_u16(&config, 104),"),
        "stateful config write proof should use source/ABI profile and assert post-state:\n{body}"
    );

    assert!(
        has("fn verify_route_ata_impl")
            && has("let vault_key = crate::derive_token_vault(&authority_key, &[2u8; 32]).0;")
            && has("let mut vault = build_minimal_account(vault_key, false, true);"),
        "non-program_id PDA should use the source tuple helper for a reachable Kani witness:\n{body}"
    );

    assert!(
        has("fn verify_router_swap_impl")
            && has("let mut input_mint = build_mint_account([7u8; 32], false, false, (input_decimals as u8));")
            && has("let mut output_mint = build_mint_account([8u8; 32], false, false, (output_decimals as u8));")
            && has("kani::assume(amount_out >= amount_in);"),
        "router swap proof should bind mint decimals and stable no-loss fee facts to real inputs:\n{body}"
    );

    assert!(
        has("fn verify_router_rebalance_pair_impl")
            && has("read_token_amount(&source_inventory_0)")
            && has("read_token_amount(&destination_inventory_1)")
            && has("assert_eq!(read_token_amount(&source_inventory_0), pre_transfer_0_from - (amount_0 as u64));")
            && has("assert_eq!(read_token_amount(&destination_inventory_1), pre_transfer_1_to + (amount_1 as u64));"),
        "two-transfer router rebalance proof should assert indexed token deltas:\n{body}"
    );

    assert!(
        has("fn verify_missing_profile_impl")
            && has("- source account order: profile unavailable; using spec order")
            && has("- ABI/dispatcher tag: profile unavailable"),
        "missing source/ABI fallback should be reported in proof profile notes:\n{body}"
    );
    assert!(
        !has("TODO: concrete account layout from source/ABI profile"),
        "profile-backed green proof paths should not carry placeholder TODOs:\n{body}"
    );
}

fn assert_or_update_codegen_smoke_snapshot(snapshot_name: &str, rendered: &str) {
    common::assert_or_update_snapshot_content(snapshot_name, "Pinocchio Kani impl", rendered);
}

fn proof_completion_pinocchio_kani_profile_diversity_with_cargo_kani() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture_root = repo_root()
        .join("crates/qedgen/tests/fixtures/pinocchio-fixtures")
        .join("kani-profile-diversity");
    copy_dir_contents(&fixture_root, temp.path());
    std::fs::create_dir(temp.path().join(".qed")).expect("create root .qed");
    std::fs::create_dir(temp.path().join("verification/.qed")).expect("create spec .qed");

    run(Command::new("git").arg("init").current_dir(temp.path()));

    let spec_path = temp.path().join("verification/profile.qedspec");
    let kani_impl = temp.path().join("src/kani_impl.rs");
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("pinocchio")
        .arg("--kani-impl")
        .arg("--kani-impl-output")
        .arg(&kani_impl)
        .current_dir(temp.path()));

    let body = std::fs::read_to_string(&kani_impl).expect("read generated kani_impl.rs");
    assert!(
        !body.contains("TODO:"),
        "proof-lab generated harness should not carry placeholder TODOs:\n{body}"
    );
    assert!(
        compact_rust(&body).contains("kani::cover!(_result.is_ok()"),
        "proof-lab generated harness should include success-path cover evidence:\n{body}"
    );

    for (harness, postconditions) in [
        (
            "verify_move_tokens_impl",
            &[
                "assert_eq!(read_token_amount(&source), pre_transfer_0_from - (amount as u64));",
                "assert_eq!(read_token_amount(&destination), pre_transfer_0_to + (amount as u64));",
            ][..],
        ),
        (
            "verify_set_fee_impl",
            &["assert_eq!(read_state_u16(&config, 104), (new_max_fee_bps as u16));"][..],
        ),
        (
            "verify_set_paused_impl",
            &["assert_eq!(read_state_bool(&config, 106), (paused_value != 0));"][..],
        ),
        (
            "verify_router_swap_impl",
            &[
                "assert_eq!(read_token_amount(&user_input), pre_transfer_0_from - (amount_in as u64));",
                "assert_eq!(read_token_amount(&vault_input), pre_transfer_0_to + (amount_in as u64));",
                "assert_eq!(read_token_amount(&vault_output), pre_transfer_1_from - (amount_out as u64));",
                "assert_eq!(read_token_amount(&user_output), pre_transfer_1_to + (amount_out as u64));",
                "assert_eq!(read_state_u16(&config, 104), pre_state_config_max_fee_bps);",
                "assert_eq!(read_state_bool(&config, 106), pre_state_config_paused);",
            ][..],
        ),
        (
            "verify_router_withdraw_impl",
            &[
                "assert_eq!(read_token_amount(&vault_source), pre_transfer_0_from - (amount as u64));",
                "assert_eq!(read_token_amount(&destination), pre_transfer_0_to + (amount as u64));",
                "assert_eq!(read_state_u16(&config, 104), pre_state_config_max_fee_bps);",
            ][..],
        ),
        (
            "verify_router_rebalance_impl",
            &[
                "assert_eq!(read_token_amount(&source_inventory), pre_transfer_0_from - (amount as u64));",
                "assert_eq!(read_token_amount(&destination_inventory), pre_transfer_0_to + (amount as u64));",
                "assert_eq!(read_state_u16(&config, 104), pre_state_config_max_fee_bps);",
            ][..],
        ),
        (
            "verify_router_rebalance_pair_impl",
            &[
                "assert_eq!(read_token_amount(&source_inventory_0), pre_transfer_0_from - (amount_0 as u64));",
                "assert_eq!(read_token_amount(&destination_inventory_0), pre_transfer_0_to + (amount_0 as u64));",
                "assert_eq!(read_token_amount(&source_inventory_1), pre_transfer_1_from - (amount_1 as u64));",
                "assert_eq!(read_token_amount(&destination_inventory_1), pre_transfer_1_to + (amount_1 as u64));",
                "assert_eq!(read_state_u16(&config, 104), pre_state_config_max_fee_bps);",
            ][..],
        ),
    ] {
        assert_green_harness_contract(&body, harness, postconditions);
        eprintln!("running generated Pinocchio proof-lab harness: {harness}");
        let output = run_capture(
            Command::new("cargo")
                .arg("kani")
                .arg("--harness")
                .arg(harness)
                .arg("-Z")
                .arg("unstable-options")
                .arg("--harness-timeout")
                .arg(
                    std::env::var("QEDGEN_KANI_HARNESS_TIMEOUT")
                        .unwrap_or_else(|_| "180".to_string()),
                )
                .arg("--manifest-path")
                .arg(temp.path().join("Cargo.toml"))
                .env("CARGO_NET_OFFLINE", "true"),
        );
        assert!(
            output.contains("** 1 of 1 cover properties satisfied"),
            "{harness} should prove a reachable success path, got:\n{output}"
        );
    }

    for regression_only_harness in [
        "verify_move_batch_impl",
        "verify_touch_config_impl",
        "verify_route_ata_impl",
    ] {
        let harness_body = generated_harness_body(&body, regression_only_harness);
        let compact_body = compact_rust(harness_body);
        assert!(
            compact_body.contains("assert!(_result.is_ok()"),
            "{regression_only_harness} should assert generated witness success even though it is not proof-completion evidence:\n{harness_body}"
        );
    }
}

/// sBPF programs are verified by Lean proofs over the assembly; their
/// runtime behavior is exercised by client-side tests, not generated
/// Kani/proptest harnesses (which have no sBPF awareness and would emit
/// meaningless Anchor-shaped output). `codegen --kani` / `--proptest`
/// must skip harness emission for assembly targets and print a note.
/// Fast test — no cargo check, just inspects the generated tree.
fn assert_sbpf_skips(flag: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_src = repo_root().join("examples/sbpf/counter/counter.qedspec");
    let spec_path = temp.path().join("counter.qedspec");
    std::fs::copy(&spec_src, &spec_path).expect("copy counter spec");
    std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");
    run(Command::new("git").arg("init").current_dir(temp.path()));

    let output_dir = temp.path().join("programs");
    let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg(flag)
        .arg("--output-dir")
        .arg(&output_dir)
        .current_dir(temp.path())
        .output()
        .expect("spawn codegen");
    assert!(
        out.status.success(),
        "codegen {flag} on sBPF spec should succeed, got {}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("skipping") && stderr.contains("sBPF"),
        "expected a skip note for {flag} on an sBPF spec, got stderr:\n{stderr}"
    );
    // No Kani / proptest harness file may be emitted anywhere in the tree.
    let banned = if flag == "--kani" {
        "kani.rs"
    } else {
        "proptest.rs"
    };
    let found: Vec<PathBuf> = walk_files(temp.path())
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(banned))
        .collect();
    assert!(
        found.is_empty(),
        "expected no {banned} for sBPF {flag}, found: {found:?}"
    );
}

/// Minimal recursive file walk (avoids a dev-dep on `walkdir`).
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn sbpf_codegen_skips_kani_harness() {
    assert_sbpf_skips("--kani");
}

#[test]
fn sbpf_codegen_skips_proptest_harness() {
    assert_sbpf_skips("--proptest");
}

#[test]
#[ignore = "runs qedgen codegen + cargo build on a generated Pinocchio crate"]
fn vault_pinocchio_scaffold_compiles() {
    smoke_pinocchio_scaffold("vault-greenfield", "vault.qedspec");
}

/// Issue #71 regression: a spec with a `Pubkey` state field (raw `[u8; 32]`,
/// no Pod wrapper) + an account-key effect/guard must `cargo build`. Pre-fix
/// this emitted `__state.<pubkey>.get()` (no such method) and `ctx.<acct>` in
/// the `self`-bound effect body — both compile errors that passed `check` +
/// proptest. Locking it into the compile gate keeps the Pubkey path covered.
#[test]
#[ignore = "runs qedgen codegen + cargo build on a generated Pinocchio crate"]
fn config_pubkey_pinocchio_scaffold_compiles() {
    smoke_pinocchio_scaffold("config-pubkey", "config.qedspec");
}

#[test]
#[ignore = "runs qedgen codegen + cargo kani on a generated Pinocchio impl harness"]
fn generated_pinocchio_kani_impl_proves_with_cargo_kani() {
    if std::env::var_os("QEDGEN_RUN_CARGO_KANI_SMOKE").is_none() {
        eprintln!(
            "skipping generated Pinocchio cargo-kani smoke; set QEDGEN_RUN_CARGO_KANI_SMOKE=1"
        );
        return;
    }
    smoke_pinocchio_generated_kani_impl("kani-generated-ping", "ping.qedspec", "verify_ping_impl");
}

/// A struct-mirror spec (`pragma state_struct = <RealStruct>`) describes a
/// program that ALREADY EXISTS, so `codegen` must not synthesize a greenfield
/// Anchor scaffold for it — under ANY invocation, not just
/// `--kani-impl-brownfield`. Dropping the pragma re-enables the scaffold, which
/// proves the pragma (not some incidental spec shape) is the deciding signal.
/// Fast: inspects the generated tree, no cargo.
#[test]
fn struct_mirror_codegen_skips_greenfield_scaffold() {
    const MIRROR_SPEC: &str = "spec MirrorSmoke
pragma state_struct = Settings
type Error | Bad
state {
  threshold : U16,
  time_lock : U32,
}
handler bump (new_threshold : U16) {
  requires new_threshold > 0 else Bad
  modifies [threshold]
  effect { threshold := new_threshold }
}
";

    fn codegen_anchor(spec_body: &str) -> (bool, String, tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let spec_path = temp.path().join("spec.qedspec");
        std::fs::write(&spec_path, spec_body).expect("write spec");
        std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");
        run(Command::new("git").arg("init").current_dir(temp.path()));
        let output_dir = temp.path().join("programs");
        let out = Command::new(env!("CARGO_BIN_EXE_qedgen"))
            .arg("codegen")
            .arg("--spec")
            .arg(&spec_path)
            .arg("--target")
            .arg("anchor")
            .arg("--output-dir")
            .arg(&output_dir)
            .current_dir(temp.path())
            .output()
            .expect("spawn codegen");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            out.status.success(),
            "codegen should succeed, got {}\nstderr:\n{stderr}",
            out.status
        );
        (out.status.success(), stderr, temp, output_dir)
    }

    // (1) Mirror spec -> scaffold skipped, note printed, no crate files anywhere.
    let (_, stderr, temp, _out_dir) = codegen_anchor(MIRROR_SPEC);
    assert!(
        stderr.contains("skipping greenfield Rust scaffold"),
        "expected a scaffold-skip note for a struct-mirror spec, got stderr:\n{stderr}"
    );
    let scaffold_files: Vec<PathBuf> = walk_files(temp.path())
        .into_iter()
        .filter(|p| {
            matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("Cargo.toml") | Some("lib.rs")
            )
        })
        .collect();
    assert!(
        scaffold_files.is_empty(),
        "struct-mirror spec must not emit a greenfield crate, found: {scaffold_files:?}"
    );

    // (2) Same spec WITHOUT the mirror pragma -> the scaffold IS emitted.
    let plain = MIRROR_SPEC.replace("pragma state_struct = Settings\n", "");
    let (_, _stderr, _temp2, out_dir2) = codegen_anchor(&plain);
    assert!(
        out_dir2.join("Cargo.toml").exists(),
        "a non-mirror Anchor spec should still emit the greenfield scaffold Cargo.toml"
    );
}

#[test]
fn pinocchio_kani_profile_diversity_fixture_generates_expected_proofs() {
    smoke_pinocchio_kani_profile_diversity();
}

#[test]
fn pinocchio_kani_profile_diversity_impl_snapshot_matches() {
    let body = render_pinocchio_kani_profile_diversity_impl();
    assert_pinocchio_kani_profile_diversity_contract(&body);
    assert_or_update_codegen_smoke_snapshot("kani-profile-diversity.kani_impl.rs", &body);
}

#[test]
#[ignore = "runs qedgen codegen + cargo kani on a richer generic Pinocchio proof-lab fixture"]
fn generated_pinocchio_profile_diversity_kani_impl_proves_with_cargo_kani() {
    if std::env::var_os("QEDGEN_RUN_CARGO_KANI_SMOKE").is_none() {
        eprintln!(
            "skipping generated Pinocchio proof-lab cargo-kani smoke; set QEDGEN_RUN_CARGO_KANI_SMOKE=1"
        );
        return;
    }
    proof_completion_pinocchio_kani_profile_diversity_with_cargo_kani();
}
