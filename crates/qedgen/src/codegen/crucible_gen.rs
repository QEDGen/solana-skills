//! Crucible (https://github.com/asymmetric-research/crucible) fuzz harness
//! codegen — emits a `fuzz/<program>/` package (Cargo.toml, rust-toolchain,
//! .gitignore, idls/README.md, src/main.rs) mechanically from a `.qedspec`.
//! Mapping: mutable state fields → `Fixture` shadow fields; `handler x` →
//! `action_x`; invariants/properties with Rust-renderable bodies →
//! `fuzz_assert!` in `invariant_test`.
//! Anchor targets only — `generate()` errors early for sBPF specs.

use anyhow::{bail, Result};
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedInvariant, ParsedProperty, ParsedSpec};
use crate::rust_codegen_util;

/// Which invariant family the emitted harness asserts after each action.
///
/// `Spec` (default) — one `fuzz_assert!` per linked `invariant` / `property`.
///
/// `Protocol` — brownfield, no spec: `invariant_test` body is empty, but the
/// per-action protocol-invariant suite (`PROTOCOL_GUARDS`) fires as
/// `fuzz_assert!`s — wallet-lamport inflation, total-lamport conservation,
/// ownership takeover, discriminator/type change, close-scrub integrity,
/// rent-exemption loss, realloc data leak, and SPL token-balance
/// conservation. Plus any panic in the *harness/host* code.
///
/// IMPORTANT — what this does NOT catch: a fault *inside* the deployed `.so`
/// (arithmetic overflow, div-by-zero, `unwrap` on `None`, `require!` abort)
/// surfaces as a **transaction error**, not a host-process panic, so
/// Crucible's host-loop crash detector never sees it. See
/// `tests/fixtures/regressions/v2.21-crucible-crash-first/README.md`. The
/// suite only catches failure modes observable as a mechanical pre/post
/// state diff (lamports / owner / discriminator / data); arithmetic and
/// semantic bugs stay the Mollusk / spec lane.
///
/// `Both` — spec `fuzz_assert!`s AND the full protocol-invariant suite
/// (`PROTOCOL_GUARDS`). Kept distinct from `Spec` so protocol-invariant
/// codegen has a place to dispatch; the guard emission is shared with
/// `Protocol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantMode {
    Spec,
    Protocol,
    Both,
}

/// Write a `fuzz/<program>/` harness. `output_dir` may be the parent (e.g.
/// `fuzz/`) or the leaf itself — see `harness_dir_for`. `mode` selects the
/// invariant family asserted after each action; brownfield callers (no
/// spec) pass `InvariantMode::Protocol`.
pub fn generate(spec: &ParsedSpec, output_dir: &Path, mode: InvariantMode) -> Result<()> {
    generate_with_account_overlay(spec, output_dir, mode, None, None)
}

pub(crate) fn generate_with_account_overlay(
    spec: &ParsedSpec,
    output_dir: &Path,
    mode: InvariantMode,
    account_overlay: Option<&crate::probe::domain_account_overlay::AccountBindingOverlay>,
    // Absolute path to `<project>/target/deploy/<prog>.so`. When set, the
    // harness loads the program from this exact path so it works from any
    // location; when None it falls back to a layout-relative path.
    deploy_so: Option<&Path>,
) -> Result<()> {
    if spec.handlers.is_empty() {
        bail!("No handlers found in spec — nothing to fuzz");
    }
    if is_sbpf_target(spec) {
        bail!(
            "Crucible codegen targets Anchor programs only in v2.18. \
             sBPF/Pinocchio support is a planned follow-up."
        );
    }

    let dir = harness_dir_for(spec, output_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::create_dir_all(dir.join("idls"))?;

    std::fs::write(dir.join("Cargo.toml"), emit_cargo_toml(spec))?;
    std::fs::write(dir.join("rust-toolchain.toml"), RUST_TOOLCHAIN)?;
    std::fs::write(dir.join(".gitignore"), GITIGNORE)?;
    std::fs::write(dir.join("idls").join("README.md"), emit_idls_readme(spec))?;
    crate::codegen_shared::write_generated_file(
        &dir.join("src").join("main.rs"),
        &emit_harness_with_deploy(spec, mode, account_overlay, deploy_so)?,
    )?;

    let assertion_count = match mode {
        InvariantMode::Spec | InvariantMode::Both => executable_assertion_count(spec),
        InvariantMode::Protocol => 0,
    };
    let label = match mode {
        InvariantMode::Spec => "spec-driven",
        InvariantMode::Protocol => "protocol-only (crash-first)",
        InvariantMode::Both => "spec + protocol",
    };
    eprintln!(
        "Generated Crucible fuzz harness at {} ({} action(s), {} {} assertion(s))",
        dir.display(),
        spec.handlers.len(),
        assertion_count,
        label,
    );

    Ok(())
}

/// Pick the harness leaf directory. If `output_dir` already ends with the
/// spec's snake-case name, treat it as the leaf; otherwise append it. Lets
/// callers pass either `./fuzz/` (parent) or `./fuzz/my_program/` (leaf).
///
/// `pub(crate)`: the CLI must resolve the harness dir with THIS function
/// before threading it to the build / seed-corpus / replay consumers —
/// `generate` appends the leaf internally, so a caller that keeps the raw
/// `--harness-dir` would look one level too shallow (see run.rs).
pub(crate) fn harness_dir_for(spec: &ParsedSpec, output_dir: &Path) -> std::path::PathBuf {
    let leaf = spec_program_name(spec);
    if output_dir.file_name().and_then(|s| s.to_str()) == Some(leaf.as_str()) {
        output_dir.to_path_buf()
    } else {
        output_dir.join(leaf)
    }
}

/// Snake-case program name (same convention as the codegen crate name).
/// `pub(crate)`: the CLI dispatcher must use the same derivation or the
/// IDL ends up in the wrong subdirectory.
pub(crate) fn spec_program_name(spec: &ParsedSpec) -> String {
    let raw: &str = if spec.program_name.is_empty() {
        "program"
    } else {
        spec.program_name.as_str()
    };
    let mut out = String::new();
    let mut prev_lower = false;
    for c in raw.chars() {
        if c.is_uppercase() {
            if prev_lower {
                out.push('_');
            }
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_lower = false;
        } else if c == '-' || c == ' ' {
            out.push('_');
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

fn is_sbpf_target(spec: &ParsedSpec) -> bool {
    spec.is_assembly_target()
}

/// Assertions that Crucible can actually execute after each action. Domain
/// mode uses this as a coverage gate: a ratified dossier paired with a spec
/// containing no renderable assertions must not be reported as domain fuzzing.
pub(crate) fn executable_assertion_count(spec: &ParsedSpec) -> usize {
    let invariants = spec
        .invariants
        .iter()
        .filter(|i| {
            i.rust_expr
                .as_ref()
                .map(|r| !check::rust_expr_is_unsupported(r))
                .unwrap_or(false)
        })
        .filter(|i| {
            spec.handlers
                .iter()
                .any(|h| h.invariants.contains(&i.name) || h.establishes.contains(&i.name))
        })
        .count();
    let properties = spec
        .properties
        .iter()
        .filter(|property| {
            property
                .rust_expression
                .as_ref()
                .map(|expression| !check::rust_expr_is_unsupported(expression))
                .unwrap_or(false)
        })
        .count();
    invariants + properties
}

// ============================================================================
// Static file content
// ============================================================================

const RUST_TOOLCHAIN: &str = r#"[toolchain]
channel = "stable"
"#;

const GITIGNORE: &str = r#"target/
crashes/
corpus/
.fuzz-cache/
*.lcov
"#;

fn emit_idls_readme(spec: &ParsedSpec) -> String {
    let prog = spec_program_name(spec);
    format!(
        r#"# IDLs for Crucible fuzz harness

Drop the Anchor IDL JSON for `{prog}` here as `{prog}.json`. The simplest
path:

```
# from the program crate root:
anchor build
cp target/idl/{prog}.json ../path/to/fuzz/{prog}/idls/{prog}.json
```

`qedgen probe --fuzz` will look up `target/idl/{prog}.json` and symlink it
into this directory automatically if it exists. Manual copy is the
fallback when discovery doesn't apply (Codama / non-Anchor / hand-rolled).

The IDL must be in Anchor 0.30+ format. Anchor 0.29 IDLs need
`anchor idl convert` first.
"#
    )
}

// ============================================================================
// Cargo.toml
// ============================================================================

/// The Crucible deps are pinned by git ref (Crucible v0.x — no stable
/// crates.io release yet). Worth checking the resolved git ref into the
/// emitted `Cargo.lock` so the harness reproduces.
const CRUCIBLE_DEP: &str = r#"crucible-fuzzer = { git = "https://github.com/asymmetric-research/crucible", branch = "main" }
crucible-test-context = { git = "https://github.com/asymmetric-research/crucible", branch = "main" }
crucible-idl-gen = { git = "https://github.com/asymmetric-research/crucible", branch = "main" }"#;

fn emit_cargo_toml(spec: &ParsedSpec) -> String {
    let prog = spec_program_name(spec);
    format!(
        r#"# ---- GENERATED BY QEDGEN ----
# Crucible fuzz harness for `{prog}`.
#
# Workspace is empty on purpose — isolates the harness's Solana/Anchor
# version chain (Solana v3 + Anchor 1.0.1) from the parent program crate
# (which may pin earlier versions). See Crucible docs.
# ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----

[package]
name = "{prog}_fuzz"
version = "0.1.0"
edition = "2021"

[workspace]

[dependencies]
{CRUCIBLE_DEP}
anchor-lang = "1.0.1"
solana-pubkey = "3.0"
solana-keypair = "3.1"
solana-signer = "3.0"
solana-program = "3.0"
solana-message = "3.0"
solana-signature = "3.1"
solana-instruction = "3.1"
libafl = {{ version = "0.15.1", features = ["std", "cli", "prelude"] }}
libafl_bolts = {{ version = "0.15.1", features = ["std"] }}
arbitrary = {{ version = "1", features = ["derive"] }}
anyhow = "1.0"
bytemuck = "1.14"
ctor = "0.6"
ctrlc = "3.4"

[[bin]]
name = "invariant_test"
path = "src/main.rs"

[features]
invariant_test = []
"#
    )
}

// ============================================================================
// Harness body
// ============================================================================

/// Test-only convenience: emit a harness with the layout-relative `.so`
/// path. Production goes through `emit_harness_with_deploy` with an absolute
/// path from the CLI.
#[cfg(test)]
fn emit_harness(
    spec: &ParsedSpec,
    mode: InvariantMode,
    account_overlay: Option<&crate::probe::domain_account_overlay::AccountBindingOverlay>,
) -> Result<String> {
    emit_harness_with_deploy(spec, mode, account_overlay, None)
}

fn emit_harness_with_deploy(
    spec: &ParsedSpec,
    mode: InvariantMode,
    account_overlay: Option<&crate::probe::domain_account_overlay::AccountBindingOverlay>,
    deploy_so: Option<&Path>,
) -> Result<String> {
    let prog = spec_program_name(spec);
    let fixture = fixture_name(spec);

    let mut out = String::new();
    out.push_str(&header(spec, mode));
    out.push_str(&format!(
        r#"use crucible_fuzzer::*;
use anchor_lang::prelude::*;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_pubkey::Pubkey;
use anchor_lang::system_program;
use std::rc::Rc;

// IDL-generated types. Drop the IDL at idls/{prog}.json before building.
// `name = "path"` form pins the generated module to our snake_case program
// name regardless of the IDL's internal `program.name` casing (Codama IRs
// typically camelCase it, e.g. `multiDelegator`; Anchor IDLs vary). Without
// the override, the macro emits `pub mod multiDelegator` and the `use`
// statements below unresolve.
crucible_idl_gen::declare_fuzz_program!({prog} = "idls/{prog}.json");
use {prog}::instruction;
use {prog}::accounts;

"#
    ));

    let want_protocol = matches!(mode, InvariantMode::Protocol | InvariantMode::Both);
    if want_protocol {
        emit_protocol_invariants_helpers(&mut out);
    }

    let force_fixture_accounts =
        account_overlay.is_some_and(|overlay| !overlay.handlers.is_empty());
    emit_fixture_struct(&mut out, spec, &fixture, mode, force_fixture_accounts);
    out.push('\n');
    emit_fixture_impl(&mut out, spec, &fixture, mode, account_overlay, deploy_so)?;
    out.push('\n');
    emit_invariant_fn(&mut out, spec, &fixture, mode);

    out.push_str("\n// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");
    Ok(out)
}

/// Emit the protocol-invariant suite helpers inline into `main.rs` (Protocol
/// / Both only — keeps the harness single-file): the shared `AccountSnapshot`
/// infra plus every `PROTOCOL_GUARDS` assert fn, in registry order.
fn emit_protocol_invariants_helpers(out: &mut String) {
    out.push_str(SNAPSHOT_INFRA);
    for guard in PROTOCOL_GUARDS {
        (guard.emit_helper)(out);
    }
}

/// Which account set a guard snapshots.
#[derive(Clone, Copy)]
enum TrackedSet {
    /// Fuzzer-controlled wallets: every fixture keypair (brownfield: signers
    /// AND non-signer writables) EXCLUDING the program-owned PDA vault. A
    /// wallet gaining lamports is the drain-to-attacker shape, and the drain
    /// destination need not be a signer — a plain writable `recipient` is the
    /// classic missing-authority withdraw target. Spec mode has no non-signer
    /// keypairs, so this equals the signer set there.
    Wallet,
    /// Every created account: fixture keypairs + the program-owned vault.
    Account,
}

/// One entry in the crash-first protocol-invariant suite. To add a failure
/// mode: append a `ProtocolGuard` and its `emit_helper` — the helper block
/// and the per-action wiring both iterate `PROTOCOL_GUARDS`, so nothing else
/// needs threading. Every guard reads only post-state and fires on a
/// mechanical, spec-less anomaly; in-program faults are TX errors and stay
/// out of scope (Mollusk / spec lane).
struct ProtocolGuard {
    /// Stable catalog identity for the guard. Not emitted today; kept as the
    /// anchor for future per-guard config (e.g. a `--disable-guard <id>`
    /// flag or a categorize_crash mapping).
    #[allow(dead_code)]
    id: &'static str,
    /// Account set this guard snapshots + checks.
    set: TrackedSet,
    /// Emitted assert fn, called per action after `.send()`.
    assert_fn: &'static str,
    /// Emits the assert fn definition into the shared helpers block.
    emit_helper: fn(&mut String),
}

const PROTOCOL_GUARDS: &[ProtocolGuard] = &[
    ProtocolGuard {
        id: "wallet_lamport",
        set: TrackedSet::Wallet,
        assert_fn: "assert_no_wallet_inflation",
        emit_helper: emit_guard_wallet_inflation,
    },
    ProtocolGuard {
        id: "lamport_conservation",
        set: TrackedSet::Account,
        assert_fn: "assert_lamports_conserved",
        emit_helper: emit_guard_lamports_conserved,
    },
    ProtocolGuard {
        id: "ownership",
        set: TrackedSet::Account,
        assert_fn: "assert_no_ownership_takeover",
        emit_helper: emit_guard_ownership,
    },
    ProtocolGuard {
        id: "discriminator",
        set: TrackedSet::Account,
        assert_fn: "assert_no_discriminator_change",
        emit_helper: emit_guard_discriminator,
    },
    ProtocolGuard {
        id: "closure",
        set: TrackedSet::Account,
        assert_fn: "assert_closure_integrity",
        emit_helper: emit_guard_closure,
    },
    ProtocolGuard {
        id: "rent",
        set: TrackedSet::Account,
        assert_fn: "assert_rent_exemption_preserved",
        emit_helper: emit_guard_rent,
    },
    ProtocolGuard {
        id: "realloc_leak",
        set: TrackedSet::Account,
        assert_fn: "assert_no_realloc_data_leak",
        emit_helper: emit_guard_realloc_leak,
    },
    ProtocolGuard {
        id: "token_conservation",
        set: TrackedSet::Account,
        assert_fn: "assert_token_balance_conserved",
        emit_helper: emit_guard_token_conservation,
    },
];

/// Shared post-state snapshot: one `get_account` pass per tracked set per
/// action, captured as `AccountSnapshot` so every guard reads from a common
/// pre-image. Re-fetches current state at assert time to diff.
const SNAPSHOT_INFRA: &str = include_str!("../../templates/crucible-snapshot-infra.rs");

/// `missing_signer` / drain: a fuzzer-controlled wallet must not GAIN lamports
/// across a call — a gain means protocol funds flowed to an attacker account.
/// The wallet set spans EVERY fixture keypair (signers and non-signer
/// writables), so a drain that credits a plain `recipient: AccountInfo` (the
/// textbook missing-authority withdraw, which credits no signer) still fires;
/// the program-owned PDA vault is excluded because it legitimately receives
/// rent/deposits.
fn emit_guard_wallet_inflation(out: &mut String) {
    out.push_str(
        r#"/// A fuzzer wallet must not GAIN lamports across a call (drain → attacker).
/// Spans signers AND non-signer writables — a drain need not credit a signer.
fn assert_no_wallet_inflation(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    for b in before {
        if !b.exists {
            continue;
        }
        let after = account_state(ctx, &b.pk);
        if after.lamports > b.lamports {
            fuzz_assert!(
                false,
                "lamport inflation on wallet {} in {}: {} -> {}",
                b.pk, label, b.lamports, after.lamports
            );
        }
    }
}

"#,
    );
}

/// Lamport conservation: the pre-existing tracked accounts' total lamports
/// must not increase — a rise means value entered the set from an untracked
/// source (materialization / external-funded drain target).
fn emit_guard_lamports_conserved(out: &mut String) {
    out.push_str(
        r#"/// Total lamports across pre-existing tracked accounts must not increase.
fn assert_lamports_conserved(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    let sum_before: u128 = before.iter().filter(|b| b.exists).map(|b| b.lamports as u128).sum();
    let sum_after: u128 = before
        .iter()
        .filter(|b| b.exists)
        .map(|b| account_state(ctx, &b.pk).lamports as u128)
        .sum();
    if sum_after > sum_before {
        fuzz_assert!(
            false,
            "lamport inflation in {}: tracked total {} -> {}",
            label, sum_before, sum_after
        );
    }
}

"#,
    );
}

/// `missing_owner_check` / rug: a live account's `owner` must not flip
/// across a call (vault reassigned out, or a fuzzer account pulled in).
fn emit_guard_ownership(out: &mut String) {
    out.push_str(
        r#"/// A live tracked account's owner must not flip across a call.
fn assert_no_ownership_takeover(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    for b in before {
        if !b.exists || b.owner == Pubkey::default() {
            continue;
        }
        let after = account_state(ctx, &b.pk);
        if after.exists && after.owner != b.owner {
            fuzz_assert!(
                false,
                "ownership takeover on {} in {}: {} -> {}",
                b.pk, label, b.owner, after.owner
            );
        }
    }
}

"#,
    );
}

/// `account_type_confusion` / `discriminator_collision`: a live typed
/// account's 8-byte discriminator must not change type across a call.
fn emit_guard_discriminator(out: &mut String) {
    out.push_str(
        r#"/// A live typed account's discriminator must not change type.
fn assert_no_discriminator_change(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    for b in before {
        if !b.exists || b.data_len < 8 {
            continue;
        }
        let after = account_state(ctx, &b.pk);
        if after.exists && after.data_len >= 8 && after.disc != b.disc {
            fuzz_assert!(
                false,
                "discriminator change on {} in {}: {:?} -> {:?}",
                b.pk, label, b.disc, after.disc
            );
        }
    }
}

"#,
    );
}

/// `pda_lifecycle_reuse_after_close` / close-redirection: draining an
/// account to zero lamports must scrub it (data zeroed + owner reset to
/// system), else it stays reusable as a live typed account.
fn emit_guard_closure(out: &mut String) {
    out.push_str(
        r#"/// Closing (lamports -> 0) must scrub data + reset owner to system.
fn assert_closure_integrity(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    for b in before {
        if !b.exists || b.lamports == 0 {
            continue;
        }
        if let Some(a) = ctx.svm.get_account(&b.pk) {
            if a.lamports == 0 {
                let data_scrubbed = a.data.iter().all(|byte| *byte == 0);
                if !(data_scrubbed && a.owner == system_program::ID) {
                    fuzz_assert!(
                        false,
                        "unscrubbed close on {} in {}: owner={} data_zeroed={}",
                        b.pk, label, a.owner, data_scrubbed
                    );
                }
            }
        }
    }
}

"#,
    );
}

/// `missing_rent_exemption` / `lamport_write_demotion`: an account that was
/// rent-exempt must not drop below the rent-exempt minimum while still live.
fn emit_guard_rent(out: &mut String) {
    out.push_str(
        r#"/// A rent-exempt account must not drop below the minimum while live.
fn assert_rent_exemption_preserved(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    for b in before {
        if !b.exists || b.lamports < rent_exempt_minimum(b.data_len) {
            continue;
        }
        let after = account_state(ctx, &b.pk);
        if after.exists && after.lamports > 0 && after.lamports < rent_exempt_minimum(after.data_len) {
            fuzz_assert!(
                false,
                "rent-exemption lost on {} in {}: {} < min {}",
                b.pk, label, after.lamports, rent_exempt_minimum(after.data_len)
            );
        }
    }
}

"#,
    );
}

/// `realloc_zero_init_data_leak`: when an account grows, the new tail bytes
/// must be zero-initialized — non-zero tail leaks prior heap contents.
fn emit_guard_realloc_leak(out: &mut String) {
    out.push_str(
        r#"/// A grown account's new tail bytes must be zero-initialized.
fn assert_no_realloc_data_leak(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    for b in before {
        if !b.exists {
            continue;
        }
        if let Some(a) = ctx.svm.get_account(&b.pk) {
            if a.data.len() > b.data_len && a.data[b.data_len..].iter().any(|byte| *byte != 0) {
                fuzz_assert!(
                    false,
                    "realloc data leak on {} in {}: bytes [{}..{}] not zero-initialized",
                    b.pk, label, b.data_len, a.data.len()
                );
            }
        }
    }
}

"#,
    );
}

/// `arithmetic`/mint-authority manifestation: per-mint SPL token balances
/// summed across tracked accounts must not increase — tokens materializing
/// (mint without authority, or transfer-in from an untracked account).
fn emit_guard_token_conservation(out: &mut String) {
    out.push_str(
        r#"/// Per-mint token balance summed across tracked accounts must not increase.
fn assert_token_balance_conserved(ctx: &TestContext, before: &[AccountSnapshot], label: &str) {
    use std::collections::BTreeMap;
    let mut before_by_mint: BTreeMap<Pubkey, u128> = BTreeMap::new();
    let mut after_by_mint: BTreeMap<Pubkey, u128> = BTreeMap::new();
    for b in before {
        if let Some(mint) = b.token_mint {
            *before_by_mint.entry(mint).or_default() += b.token_amount as u128;
            let after = account_state(ctx, &b.pk);
            if let Some(after_mint) = after.token_mint {
                *after_by_mint.entry(after_mint).or_default() += after.token_amount as u128;
            }
        }
    }
    for (mint, before_sum) in &before_by_mint {
        let after_sum = after_by_mint.get(mint).copied().unwrap_or(0);
        if after_sum > *before_sum {
            fuzz_assert!(
                false,
                "token inflation for mint {} in {}: {} -> {}",
                mint, label, before_sum, after_sum
            );
        }
    }
}

"#,
    );
}

/// Wallet-set pubkey exprs (`self.<ident>.pubkey()`). The wallet-lamport
/// guard's tracked set: any fuzzer-controlled wallet gaining lamports is the
/// drain-to-attacker shape. Brownfield tracks EVERY fixture keypair (signers
/// and non-signer writables) so a drain to a plain `recipient` fires too; the
/// program-owned PDA vault is intentionally absent (it legitimately gains
/// rent, and `account_tracked_pubkey_exprs` is where the PDA is tracked).
/// Spec mode has no non-signer keypairs, so this collapses to the signer set.
fn wallet_tracked_pubkey_exprs(
    spec: &ParsedSpec,
    mode: InvariantMode,
    force_fixture_accounts: bool,
) -> Vec<String> {
    if uses_brownfield_accounts(spec, mode, force_fixture_accounts) {
        collect_brownfield_keypair_names(spec)
            .iter()
            .map(|name| format!("self.{}.pubkey()", brownfield_keypair_ident(name)))
            .collect()
    } else {
        collect_signer_idents(spec)
            .iter()
            .map(|sig| format!("self.{sig}.pubkey()"))
            .collect()
    }
}

/// Account-set pubkey exprs: every fixture keypair plus the program-owned
/// PDA vault (when setup created one). The `Account`-set guards' tracked set
/// — broader than signers because owner flips, discriminator changes, and
/// closures matter on non-signer writables too.
fn account_tracked_pubkey_exprs(
    spec: &ParsedSpec,
    op: &ParsedHandler,
    mode: InvariantMode,
    force_fixture_accounts: bool,
) -> Vec<String> {
    let mut exprs = Vec::new();
    if uses_brownfield_accounts(spec, mode, force_fixture_accounts) {
        for name in collect_brownfield_keypair_names(spec) {
            exprs.push(format!("self.{}.pubkey()", brownfield_keypair_ident(&name)));
        }
        for account in &op.accounts {
            if account.pda_seeds.is_some() {
                exprs.push(pda_local_ident(&account.name));
            }
        }
    } else {
        for sig in collect_signer_idents(spec) {
            exprs.push(format!("self.{sig}.pubkey()"));
        }
    }
    exprs
}

fn pda_local_ident(account_name: &str) -> String {
    format!("__pda_{}", brownfield_keypair_ident(account_name))
}

fn pda_seeds_for_account<'a>(
    spec: &'a ParsedSpec,
    account: &'a crate::check::ParsedHandlerAccount,
) -> Option<&'a [String]> {
    let inline = account.pda_seeds.as_deref()?;
    spec.pdas
        .iter()
        .find(|pda| pda.name == account.name)
        .or_else(|| {
            (inline.len() == 1)
                .then(|| spec.pdas.iter().find(|pda| pda.name == inline[0]))
                .flatten()
        })
        .map(|pda| pda.seeds.as_slice())
        .or(Some(inline))
}

fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128"
    )
}

fn render_pda_seed_expr(
    spec: &ParsedSpec,
    op: &ParsedHandler,
    seed: &str,
    account_overlay: Option<&crate::probe::domain_account_overlay::AccountBindingOverlay>,
) -> Result<String> {
    if let Some(hex) = seed.strip_prefix("bytes:") {
        if hex.len() % 2 != 0 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!(
                "Crucible PDA seed `{seed}` in handler `{}` has an invalid encoded byte literal",
                op.name
            );
        }
        let bytes = hex
            .as_bytes()
            .chunks(2)
            .map(|pair| format!("0x{}", std::str::from_utf8(pair).unwrap_or("00")))
            .collect::<Vec<_>>();
        return Ok(format!("&[{}]", bytes.join(", ")));
    }
    if (seed.starts_with('"') && seed.ends_with('"'))
        || (seed.starts_with('\'') && seed.ends_with('\''))
    {
        return Ok(format!("b{}", seed));
    }

    if let Some(account) = op.accounts.iter().find(|account| account.name == seed) {
        if account.pda_seeds.is_some() {
            return Ok(format!("{}.as_ref()", pda_local_ident(&account.name)));
        }
        if let Some(pubkey) = &account.default_pubkey {
            return Ok(format!("solana_pubkey::pubkey!(\"{pubkey}\").as_ref()"));
        }
        let target = account_overlay_target(account_overlay, &op.name, &account.name)?;
        return Ok(format!(
            "self.{}.pubkey().as_ref()",
            brownfield_keypair_ident(target)
        ));
    }

    if let Some((_, ty)) = op.takes_params.iter().find(|(name, _)| name == seed) {
        return Ok(if is_integer_type(ty) {
            format!("{seed}.to_le_bytes().as_ref()")
        } else if ty == "Bool" {
            format!("&[{seed} as u8]")
        } else if ty == "Pubkey" {
            // Pubkey action arguments are deliberately fixed because Crucible
            // cannot derive Arbitrary for Solana's Address wrapper. Match the
            // `instruction::X` literal emitted below.
            "Pubkey::default().as_ref()".to_string()
        } else {
            bail!(
                "Crucible PDA seed `{seed}` in handler `{}` has unsupported argument type `{ty}`",
                op.name
            )
        });
    }

    let state_fields = rust_codegen_util::resolve_state_fields(spec);
    if let Some((_, ty)) = state_fields.iter().find(|(name, _)| name == seed) {
        return Ok(if is_integer_type(ty) {
            format!("self.{seed}.to_le_bytes().as_ref()")
        } else if ty == "Bool" {
            format!("&[self.{seed} as u8]")
        } else if ty == "Pubkey" {
            format!("self.{seed}.as_ref()")
        } else {
            bail!(
                "Crucible PDA seed `{seed}` in handler `{}` has unsupported state-field type `{ty}`",
                op.name
            )
        });
    }

    bail!(
        "Crucible cannot resolve PDA seed `{seed}` in handler `{}`; bind it as an account, instruction argument, state field, or literal",
        op.name
    )
}

fn ordered_pda_accounts<'a>(
    spec: &ParsedSpec,
    op: &'a ParsedHandler,
) -> Result<Vec<&'a crate::check::ParsedHandlerAccount>> {
    fn visit<'a>(
        spec: &ParsedSpec,
        op: &'a ParsedHandler,
        account: &'a crate::check::ParsedHandlerAccount,
        visiting: &mut std::collections::BTreeSet<String>,
        emitted: &mut std::collections::BTreeSet<String>,
        ordered: &mut Vec<&'a crate::check::ParsedHandlerAccount>,
    ) -> Result<()> {
        if emitted.contains(&account.name) {
            return Ok(());
        }
        if !visiting.insert(account.name.clone()) {
            bail!(
                "Crucible PDA derivation cycle in handler `{}` at account `{}`",
                op.name,
                account.name
            );
        }
        for seed in pda_seeds_for_account(spec, account).unwrap_or_default() {
            if let Some(dependency) = op
                .accounts
                .iter()
                .find(|candidate| candidate.name == *seed && candidate.pda_seeds.is_some())
            {
                visit(spec, op, dependency, visiting, emitted, ordered)?;
            }
        }
        visiting.remove(&account.name);
        emitted.insert(account.name.clone());
        ordered.push(account);
        Ok(())
    }

    let mut visiting = std::collections::BTreeSet::new();
    let mut emitted = std::collections::BTreeSet::new();
    let mut ordered = Vec::new();
    for account in op
        .accounts
        .iter()
        .filter(|account| account.pda_seeds.is_some())
    {
        visit(spec, op, account, &mut visiting, &mut emitted, &mut ordered)?;
    }
    Ok(ordered)
}

fn header(spec: &ParsedSpec, mode: InvariantMode) -> String {
    let fp = crate::fingerprint::compute_fingerprint(spec);
    let mut s = crate::codegen_shared::marker_unlabeled(&fp, "fuzz/src/main.rs");
    s.push_str("//\n");
    s.push_str("// Crucible coverage-guided fuzz harness for the spec.\n");
    s.push_str("//\n");
    s.push_str("// Each `action_*` method is mutated as a typed Action variant by the\n");
    s.push_str("// fuzzer. `invariant_test` is checked after every dispatched action.\n");
    s.push_str("// fuzz_assert! sets a thread-local violation flag instead of panicking,\n");
    s.push_str("// so the loop breaks gracefully and the input is recorded as a crash.\n");
    match mode {
        InvariantMode::Spec => {}
        InvariantMode::Protocol => {
            s.push_str("//\n");
            s.push_str("// Mode: PROTOCOL (no spec). invariant_test() body is empty; the\n");
            s.push_str("// per-action protocol-invariant suite below is the live detector:\n");
            s.push_str("// wallet-lamport inflation, total-lamport conservation, ownership\n");
            s.push_str("// takeover, discriminator change, close-scrub, rent-exemption loss,\n");
            s.push_str("// realloc data leak, SPL token-balance conservation.\n");
            s.push_str("// NOTE: in-program faults (overflow, div-by-zero, unwrap, require!)\n");
            s.push_str("// surface as TX ERRORS, not host panics — the host-loop crash\n");
            s.push_str("// detector does NOT catch them. Only harness/host panics do.\n");
        }
        InvariantMode::Both => {
            s.push_str("//\n");
            s.push_str("// Mode: SPEC + PROTOCOL. Spec-invariant assertions fire as usual,\n");
            s.push_str("// plus the full protocol-invariant suite below. In-program faults\n");
            s.push_str("// still surface as TX errors, not host-loop crashes.\n");
        }
    }
    s.push_str("//\n");
    s.push_str("// To run:\n");
    s.push_str("//   cd fuzz/<program>/\n");
    s.push_str("//   crucible run <program> invariant_test\n");
    s.push_str("// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n");
    s.push_str("#![allow(unused_imports, unused_variables, dead_code)]\n\n");
    s
}

fn fixture_name(spec: &ParsedSpec) -> String {
    // Normalise to snake_case first — `to_pascal_case` only splits on `_`,
    // so a raw kebab name would yield `Multi-delegator` (invalid Rust).
    let normalised = spec_program_name(spec);
    let pascal = crate::codegen_shared::to_pascal_case(&normalised);
    let head: &str = if pascal.is_empty() {
        "Program"
    } else {
        &pascal
    };
    format!("{head}Fixture")
}

fn emit_fixture_struct(
    out: &mut String,
    spec: &ParsedSpec,
    fixture: &str,
    mode: InvariantMode,
    force_fixture_accounts: bool,
) {
    out.push_str("/// Fixture state. Includes Crucible test infrastructure plus shadow\n");
    out.push_str("/// fields mirroring spec state — invariants read from these instead of\n");
    out.push_str("/// from LiteSVM accounts directly so the body translation matches the\n");
    out.push_str("/// proptest / Kani backends.\n");
    out.push_str("#[derive(Clone)]\n");
    out.push_str(&format!("struct {fixture} {{\n"));
    out.push_str("    ctx: TestContext,\n");
    out.push_str("    program_id: Pubkey,\n");

    if uses_brownfield_accounts(spec, mode, force_fixture_accounts) {
        // Brownfield: one Keypair per non-default, non-PDA account (signers
        // AND user-provided writables). Field idents are snake_cased; the
        // IDL's camelCase name stays on `ParsedHandlerAccount` to match the
        // macro-generated struct field, converted at use sites.
        for name in collect_brownfield_keypair_names(spec) {
            let ident = brownfield_keypair_ident(&name);
            out.push_str(&format!("    {ident}: Rc<Keypair>,\n"));
        }
    } else {
        // Spec mode: signing identities from auth-cited handlers + named PDAs.
        let signers = collect_signer_idents(spec);
        for sig in &signers {
            out.push_str(&format!("    {sig}: Rc<Keypair>,\n"));
        }
    }

    // Shadow fields for mutable state — read after each action.
    let state_fields = rust_codegen_util::resolve_state_fields(spec);
    let mutable_fields = rust_codegen_util::field_refs(state_fields);
    for (fname, ftype) in &mutable_fields {
        let rust_ty = map_simple_type(ftype);
        out.push_str(&format!("    {fname}: {rust_ty},\n"));
    }

    out.push_str("}\n");
}

/// True when any handler carries IDL-derived account metadata — switches
/// the fixture/setup emitters between brownfield (Codama IDL) and spec mode
/// (accounts from the `.qedspec` accounts block).
fn spec_is_brownfield_with_idl_accounts(spec: &ParsedSpec) -> bool {
    spec.handlers.iter().any(|h| {
        h.accounts
            .iter()
            .any(|a| a.default_pubkey.is_some() || a.pda_seeds.is_some())
    })
}

/// Whether to emit fixture accounts the brownfield way (one Keypair per
/// non-default, non-PDA account — signers AND writables) rather than the
/// spec-mode signers-only way.
///
/// The `spec_is_brownfield_with_idl_accounts` proxy alone under-fires: a
/// brownfield program whose handlers take only plain writable accounts (no
/// default address, no PDA seed — e.g. a raw `drain(source, target)`) looks
/// spec-shaped and emits no keypairs, leaving both protocol guards with an
/// empty tracked set. `InvariantMode::Protocol` is unconditionally brownfield
/// by construction (the `--root` path with no spec passes it), so treat it as
/// brownfield too — but only when there ARE account entries to derive
/// keypairs from. An auth-only handler (`auth X`, no accounts block) has no
/// account list; it must keep the signer path so the `auth X` signer still
/// gets a keypair (the brownfield collector reads the account list, not
/// `who`). `Both` keeps the proxy — it carries a real spec whose accounts
/// block drives account handling.
fn uses_brownfield_accounts(
    spec: &ParsedSpec,
    mode: InvariantMode,
    force_fixture_accounts: bool,
) -> bool {
    force_fixture_accounts
        || spec_is_brownfield_with_idl_accounts(spec)
        || (matches!(mode, InvariantMode::Protocol) && spec_has_account_entries(spec))
}

/// Any handler declares a non-empty account list.
fn spec_has_account_entries(spec: &ParsedSpec) -> bool {
    spec.handlers.iter().any(|h| !h.accounts.is_empty())
}

/// DSL type → Rust shadow type. Primitives + Pubkey only; compound types
/// fall back to `()` with a TODO marking the field unshadowed.
fn map_simple_type(dsl_type: &str) -> &'static str {
    match dsl_type {
        "U8" => "u8",
        "U16" => "u16",
        "U32" => "u32",
        "U64" => "u64",
        "U128" => "u128",
        "I8" => "i8",
        "I16" => "i16",
        "I32" => "i32",
        "I64" => "i64",
        "I128" => "i128",
        "Pubkey" => "Pubkey",
        "Bool" => "bool",
        _ => "() /* TODO: compound type shadow not yet supported */",
    }
}

fn collect_signer_idents(spec: &ParsedSpec) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for h in &spec.handlers {
        if let Some(who) = &h.who {
            seen.insert(who.clone());
        }
        for acc in &h.accounts {
            if acc.is_signer {
                seen.insert(acc.name.clone());
            }
        }
    }
    seen.into_iter().collect()
}

/// Collect every account name needing a fixture-owned `Rc<Keypair>`:
/// neither a hardcoded `default_pubkey` (those become `pubkey!` literals)
/// nor `pda_seeds`. Non-signers qualify too — Crucible tracks the pubkey
/// for snapshot/restore even when the harness doesn't sign. Returns names
/// in the IDL's original casing; see `brownfield_keypair_ident`.
fn collect_brownfield_keypair_names(spec: &ParsedSpec) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for h in &spec.handlers {
        for acc in &h.accounts {
            if acc.default_pubkey.is_none() && acc.pda_seeds.is_none() {
                seen.insert(acc.name.clone());
            }
        }
    }
    seen.into_iter().collect()
}

/// Convert a brownfield account name (kept in the IDL's case so it
/// matches the macro-generated struct field name) into a snake_case Rust
/// ident for the fixture struct. `sourceAccountInfo` → `source_account_info`.
pub(crate) fn brownfield_keypair_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Render `s` as a double-quoted Rust string literal, escaping backslashes
/// and quotes (filesystem paths on Windows, or with spaces/quotes, would
/// otherwise produce invalid source).
fn rust_string_literal(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn emit_fixture_impl(
    out: &mut String,
    spec: &ParsedSpec,
    fixture: &str,
    mode: InvariantMode,
    account_overlay: Option<&crate::probe::domain_account_overlay::AccountBindingOverlay>,
    deploy_so: Option<&Path>,
) -> Result<()> {
    let prog = spec_program_name(spec);
    let force_fixture_accounts =
        account_overlay.is_some_and(|overlay| !overlay.handlers.is_empty());
    let is_brownfield = uses_brownfield_accounts(spec, mode, force_fixture_accounts);
    let signers = collect_signer_idents(spec);
    let brownfield_names = if is_brownfield {
        collect_brownfield_keypair_names(spec)
    } else {
        Vec::new()
    };

    out.push_str("#[fuzz_fixture]\n");
    out.push_str(&format!("impl {fixture} {{\n"));

    // ── setup() ──────────────────────────────────────────────────────────
    out.push_str("    pub fn setup() -> Self {\n");
    out.push_str("        let mut ctx = TestContext::new();\n");
    out.push_str(&format!("        let program_id = {prog}::ID;\n"));
    // The `.so` path. When the CLI knows the project root it passes an
    // ABSOLUTE path (`deploy_so`) so the harness resolves the program from
    // any location — the fix for a `--harness-dir` outside the project.
    // Otherwise (`qedgen codegen --crucible`, unit tests) fall back to a
    // RELATIVE path from the standard harness depth: spec-mode harness lives
    // at <root>/fuzz/<prog>/, brownfield (protocol) at <root>/.qed/fuzz/<prog>/
    // — one extra level. Wrong depth panics at `ctx.add_program` on startup.
    let so_literal = match deploy_so {
        Some(path) => rust_string_literal(&path.display().to_string()),
        None => {
            let prefix = match mode {
                InvariantMode::Protocol => "../../..",
                InvariantMode::Spec | InvariantMode::Both => "../..",
            };
            rust_string_literal(&format!("{prefix}/target/deploy/{prog}.so"))
        }
    };
    out.push_str(&format!(
        "        ctx.add_program(&program_id, {so_literal})\n"
    ));
    out.push_str("            .unwrap();\n");
    if is_brownfield {
        for name in &brownfield_names {
            let ident = brownfield_keypair_ident(name);
            out.push_str(&format!("        let {ident} = Rc::new(Keypair::new());\n"));
            out.push_str(&format!(
                "        ctx.create_account()\n            .pubkey({ident}.pubkey())\n            .lamports(100_000_000_000)\n            .owner(system_program::ID)\n            .create()\n            .unwrap();\n"
            ));
        }
    } else {
        for sig in &signers {
            out.push_str(&format!("        let {sig} = Rc::new(Keypair::new());\n"));
            out.push_str(&format!(
                "        ctx.create_account()\n            .pubkey({sig}.pubkey())\n            .lamports(100_000_000_000)\n            .owner(system_program::ID)\n            .create()\n            .unwrap();\n"
            ));
        }
    }
    let state_fields = rust_codegen_util::resolve_state_fields(spec);
    let mutable_fields = rust_codegen_util::field_refs(state_fields);

    out.push_str("        Self {\n");
    out.push_str("            ctx,\n");
    out.push_str("            program_id,\n");
    if is_brownfield {
        for name in &brownfield_names {
            let ident = brownfield_keypair_ident(name);
            out.push_str(&format!("            {ident},\n"));
        }
    } else {
        for sig in &signers {
            out.push_str(&format!("            {sig},\n"));
        }
    }
    for (fname, ftype) in &mutable_fields {
        let init = match map_simple_type(ftype) {
            "Pubkey" => "Pubkey::default()".to_string(),
            "bool" => "false".to_string(),
            t if t.starts_with("()") => "()".to_string(),
            _ => "0".to_string(),
        };
        out.push_str(&format!("            {fname}: {init},\n"));
    }
    out.push_str("        }\n    }\n\n");

    // ── action_* per handler ─────────────────────────────────────────────
    for h in &spec.handlers {
        emit_action_fn(out, spec, h, mode, account_overlay)?;
        out.push('\n');
    }

    out.push_str("}\n");
    Ok(())
}

fn emit_action_fn(
    out: &mut String,
    spec: &ParsedSpec,
    op: &ParsedHandler,
    mode: InvariantMode,
    account_overlay: Option<&crate::probe::domain_account_overlay::AccountBindingOverlay>,
) -> Result<()> {
    out.push_str(&format!("    /// {} → action variant.\n", op.name));
    if op.accounts.is_empty() {
        // No IDL/spec accounts to render → the `.accounts(...)` literal is
        // left as a `todo!()` for the agent to fill.
        out.push_str("    /// No account list resolved — the `accounts::X { ... }` literal\n");
        out.push_str("    /// falls through as todo!() for the agent to fill (drop an IDL\n");
        out.push_str("    /// at the project root to auto-fill it).\n");
    }

    // Pubkey-typed args are NOT exposed as action params: #[fuzz_fixture]
    // can't derive Arbitrary for Pubkey (no Arbitrary on its `Address`
    // wrapper), so one in the signature breaks the build. The call literal
    // inlines `Pubkey::default()`; real PDAs / signer pubkeys belong in the
    // agent-filled `.accounts(...)` literal, not the fuzzed payload.
    let mut params = String::new();
    for (pname, ptype) in &op.takes_params {
        if ptype == "Pubkey" {
            continue;
        }
        let rust_ty = map_simple_type(ptype);
        let range_attr = infer_range_attr(spec, ptype);
        if !range_attr.is_empty() {
            params.push_str(&format!("        {range_attr}\n"));
        }
        params.push_str(&format!("        {pname}: {rust_ty},\n"));
    }

    out.push_str(&format!(
        "    pub fn action_{}(\n        &mut self,\n{}    ) -> bool {{\n",
        op.name, params
    ));

    // Derive every PDA from its declared seed tuple. Dependencies between
    // PDAs are topologically ordered, and non-init PDAs are materialized on
    // demand so argument-dependent addresses can vary across fuzz actions.
    // Init transitions must leave the target absent for the program/system
    // instruction to create it.
    let pda_accounts = ordered_pda_accounts(spec, op)?;
    // A handler is an init transition — its PDAs must be left absent for the
    // program/system instruction to create them — when its pre-state cannot
    // belong to an existing account: the state name denotes nonexistence, or
    // no handler in the spec produces it (unreachable except by creation).
    // A name list alone is wrong in both directions: the minimal skeleton's
    // `Init` is a creation state, while a produced `Inactive` is a paused
    // EXISTING account that must be materialized on demand. Per-account init
    // constraints (Anchor `init`) are not modeled, so this stays per-handler.
    let produced_states: std::collections::BTreeSet<&str> = spec
        .handlers
        .iter()
        .filter_map(|h| h.post_status.as_deref())
        .collect();
    let is_init = op
        .pre_status
        .as_deref()
        .is_some_and(|pre| check::state_name_is_nonexistent(pre) || !produced_states.contains(pre));
    for account in &pda_accounts {
        let seeds = pda_seeds_for_account(spec, account).unwrap_or_default();
        let local = pda_local_ident(&account.name);
        let rendered = seeds
            .iter()
            .map(|seed| render_pda_seed_expr(spec, op, seed, account_overlay))
            .collect::<Result<Vec<_>>>();
        let seed_exprs = match rendered {
            Ok(exprs) => exprs,
            // Domain replay pins byte-exact addresses, so an overlay run must
            // fail loudly rather than derive a placeholder the resolved plan
            // never referenced.
            Err(err) if account_overlay.is_some() => return Err(err),
            Err(err) => {
                out.push_str(&format!(
                    "        // qedgen: {err}.\n        // Placeholder empty-seed derivation — fuzzing proceeds, but this\n        // PDA's real address is NOT exercised; agent-fill the seed tuple\n        // if `{}` matters to the properties under test.\n",
                    account.name
                ));
                Vec::new()
            }
        };
        out.push_str(&format!(
            "        let {local} = Pubkey::find_program_address(&[{}], &self.program_id).0;\n",
            seed_exprs.join(", ")
        ));
        if !is_init {
            out.push_str(&format!(
                "        if self.ctx.svm.get_account(&{local}).is_none() {{\n            self.ctx.create_account()\n                .pubkey({local})\n                .lamports(100_000_000_000)\n                .owner(self.program_id)\n                .create()\n                .unwrap();\n        }}\n"
            ));
        }
    }

    // The Anchor `.call(...).accounts(...).send()` chain. `.send()` returns
    // `Result<TxOutcome, ...>`; both layers collapse into one bool —
    // transport errors count as failed actions, same as program errors.
    let ix_name = pascal_case(&op.name);
    // Pubkey fields get `Pubkey::default()` (see param-list note above);
    // other params pass through via field shorthand.
    let arg_inits: String = op
        .takes_params
        .iter()
        .map(|(n, t)| {
            if t == "Pubkey" {
                format!("{n}: Pubkey::default(), ")
            } else {
                format!("{n}, ")
            }
        })
        .collect();
    // Snapshot wallet lamports before .send() (Protocol / Both only). Tracked
    // set = every fuzzer-controlled fixture keypair (signers AND non-signer
    // writables), so a drain to a plain `recipient` is caught, not just a
    // drain that credits a signer. The program-owned PDA vault is NOT in this
    // set (it legitimately gains rent) — the account-set guards track it.
    // Protocol-invariant suite (Protocol / Both). Snapshot each used tracked
    // set once before .send(); the registry drives which guards assert after.
    let want_protocol = matches!(mode, InvariantMode::Protocol | InvariantMode::Both);
    let force_fixture_accounts =
        account_overlay.is_some_and(|overlay| !overlay.handlers.is_empty());
    let wallet_exprs = wallet_tracked_pubkey_exprs(spec, mode, force_fixture_accounts);
    let account_exprs = account_tracked_pubkey_exprs(spec, op, mode, force_fixture_accounts);
    let use_wallet = want_protocol && !wallet_exprs.is_empty();
    let use_account = want_protocol && !account_exprs.is_empty();
    if use_wallet {
        out.push_str("        let __wallet_set: Vec<Pubkey> = vec![\n");
        for expr in &wallet_exprs {
            out.push_str(&format!("            {expr},\n"));
        }
        out.push_str("        ];\n");
        out.push_str(
            "        let __wallet_snap = snapshot_account_state(&self.ctx, &__wallet_set);\n",
        );
    }
    if use_account {
        out.push_str("        let __account_set: Vec<Pubkey> = vec![\n");
        for expr in &account_exprs {
            out.push_str(&format!("            {expr},\n"));
        }
        out.push_str("        ];\n");
        out.push_str(
            "        let __account_snap = snapshot_account_state(&self.ctx, &__account_set);\n",
        );
    }

    out.push_str("        let outcome = self.ctx.program(self.program_id)\n");
    out.push_str(&format!(
        "            .call(instruction::{ix_name} {{ {arg_inits}}})\n"
    ));
    // Populated `accounts` list → real `accounts::Foo { ... }` literal.
    // Crucible's generated struct OMITS fixed-pubkey accounts (auto-filled
    // by the macro) and snake-cases the remaining field names. So:
    //   - `default_pubkey` → SKIP (Crucible auto-fills).
    //   - PDA (`pda_seeds`) → the seed-derived local emitted above.
    //   - Otherwise → `self.<keypair_ident>.pubkey()`.
    // Empty list (no IDL, no spec accounts block) → `todo!()` agent-fill.
    if op.accounts.is_empty() {
        out.push_str(&format!(
            "            .accounts::<accounts::{ix_name}>(todo!(\"agent-fill: accounts::{ix_name} {{{{ ... }}}} from spec accounts block\"))\n"
        ));
    } else {
        out.push_str(&format!("            .accounts(accounts::{ix_name} {{\n"));
        for acc in &op.accounts {
            if acc.default_pubkey.is_some() {
                // Crucible auto-fills fixed-address accounts (no struct field).
                continue;
            }
            let value = if acc.pda_seeds.is_some() {
                pda_local_ident(&acc.name)
            } else {
                let target = account_overlay_target(account_overlay, &op.name, &acc.name)?;
                format!("self.{}.pubkey()", brownfield_keypair_ident(target))
            };
            let field = brownfield_keypair_ident(&acc.name);
            out.push_str(&format!("                {field}: {value},\n"));
        }
        out.push_str("            })\n");
    }

    // Signers: prefer IDL per-account `isSigner` flags; fall back to the
    // spec's `auth X` lift.
    let brownfield_signers: Vec<String> = op
        .accounts
        .iter()
        .filter(|a| a.is_signer && a.default_pubkey.is_none() && a.pda_seeds.is_none())
        .map(|a| account_overlay_target(account_overlay, &op.name, &a.name).map(str::to_string))
        .collect::<Result<Vec<_>>>()?;
    if !brownfield_signers.is_empty() {
        let refs: Vec<String> = brownfield_signers
            .iter()
            .map(|n| format!("&*self.{}", brownfield_keypair_ident(n)))
            .collect();
        out.push_str(&format!("            .signers(&[{}])\n", refs.join(", ")));
    } else if let Some(who) = &op.who {
        out.push_str(&format!("            .signers(&[&self.{who}])\n"));
    }
    out.push_str("            .send();\n");
    out.push_str(
        "        let success = outcome.as_ref().map(|o| o.is_success()).unwrap_or(false);\n",
    );

    // Registry-driven asserts after .send() (Protocol / Both). Run even when
    // .send() failed — a reverted tx can still leave partial mutations under
    // some error shapes. Each guard reads the snapshot for its tracked set.
    if want_protocol {
        for guard in PROTOCOL_GUARDS {
            let (set_used, snap_var) = match guard.set {
                TrackedSet::Wallet => (use_wallet, "__wallet_snap"),
                TrackedSet::Account => (use_account, "__account_snap"),
            };
            if set_used {
                out.push_str(&format!(
                    "        {}(&self.ctx, &{}, \"{}\");\n",
                    guard.assert_fn, snap_var, op.name,
                ));
            }
        }
    }

    // A direct scalar assignment from a same-typed handler argument can be
    // mirrored without knowing the on-chain account layout. This records the
    // domain-relevant value that the deployed program actually accepted; it
    // does not pretend to deserialize arbitrary program state. Richer effects
    // still need the account-specific sync below.
    if !op.effects.is_empty() {
        out.push_str("        if success {\n");
        if !matches!(mode, InvariantMode::Protocol) {
            for eff in &op.effects {
                if let Some(rhs) = direct_scalar_shadow_assignment(spec, op, eff) {
                    out.push_str(&format!("            self.{} = {rhs};\n", eff.field));
                }
            }
        }
        out.push_str("            // TODO: sync shadow state from the on-chain account here.\n");
        out.push_str("            // For each effect declared in the spec, copy the post-state\n");
        out.push_str("            // field into the matching self.<field>. Example pattern:\n");
        out.push_str(
            "            //   let acc = self.ctx.read_anchor_account::<StateAcct>(&pda).unwrap();\n",
        );
        for eff in &op.effects {
            let field = &eff.field;
            out.push_str(&format!("            //   self.{field} = acc.{field};\n"));
        }
        out.push_str("        }\n");
    }

    out.push_str("        success\n");
    out.push_str("    }\n");
    Ok(())
}

/// Return a safe shadow-state RHS for the one layout-independent effect shape:
/// `field := argument`, where both sides have the same scalar type. The action
/// has succeeded at this point, so the mirror lets domain invariants test the
/// input envelope accepted by the deployed program. Arithmetic, field reads,
/// nested paths, and type conversions remain account-specific agent work.
fn direct_scalar_shadow_assignment(
    spec: &ParsedSpec,
    op: &ParsedHandler,
    effect: &crate::check::ParsedEffect,
) -> Option<String> {
    if effect.op != "set"
        || effect.field.is_empty()
        || !effect
            .field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || effect.field.as_bytes()[0].is_ascii_digit()
    {
        return None;
    }
    let rhs = if effect.value_rust.is_empty() {
        effect.value.trim()
    } else {
        effect.value_rust.trim()
    };
    let (_, argument_type) = op.takes_params.iter().find(|(name, _)| name == rhs)?;
    let (_, field_type) = spec
        .state_fields
        .iter()
        .find(|(name, _)| name == &effect.field)?;
    if argument_type != field_type || !is_crucible_scalar_type(field_type) {
        return None;
    }
    Some(rhs.to_string())
}

fn is_crucible_scalar_type(qedspec_type: &str) -> bool {
    matches!(
        qedspec_type,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" | "Bool"
    )
}

fn account_overlay_target<'a>(
    overlay: Option<&'a crate::probe::domain_account_overlay::AccountBindingOverlay>,
    handler: &str,
    account: &'a str,
) -> Result<&'a str> {
    let Some(value) = overlay
        .and_then(|overlay| overlay.handlers.get(handler))
        .and_then(|handler| handler.accounts.get(account))
    else {
        return Ok(account);
    };
    value.strip_prefix("fixture:").ok_or_else(|| {
        anyhow::anyhow!(
            "account overlay target `{value}` for `{handler}.{account}` is not a fixture identifier"
        )
    })
}

/// `#[range(lo..hi)]` inference — currently none ("" → Crucible defaults
/// to the type's full range with boundary-biased mutation).
fn infer_range_attr(_spec: &ParsedSpec, _ptype: &str) -> String {
    // TODO: lift bounds from `requires p <= MAX` / `where` clauses.
    String::new()
}

/// snake_case → PascalCase (used for Anchor `instruction::Foo` / `accounts::Foo`).
/// Alias of the shared `codegen_shared::to_pascal_case`.
fn pascal_case(s: &str) -> String {
    crate::codegen_shared::to_pascal_case(s)
}

fn emit_invariant_fn(out: &mut String, spec: &ParsedSpec, fixture: &str, mode: InvariantMode) {
    // Protocol-only: empty body. The live detector for this mode is the
    // wallet-lamport-inflation guard (plus the rest of the protocol suite)
    // emitted per action, plus any harness/host panic. In-program faults are
    // TX errors, not caught here.
    if matches!(mode, InvariantMode::Protocol) {
        out.push_str("#[invariant_test]\n");
        out.push_str(&format!("fn invariant_test(_fixture: &mut {fixture}) {{\n"));
        out.push_str("    // Protocol mode — no spec assertions. The per-action protocol-\n");
        out.push_str("    // invariant suite (see top of file) is the live check; in-program\n");
        out.push_str("    // faults (overflow, unwrap, require!) surface as TX errors, not\n");
        out.push_str("    // host-loop crashes.\n");
        out.push_str("}\n");
        return;
    }

    let linked: Vec<&ParsedInvariant> = spec
        .invariants
        .iter()
        .filter(|i| {
            i.rust_expr
                .as_ref()
                .map(|r| !check::rust_expr_is_unsupported(r))
                .unwrap_or(false)
        })
        .filter(|i| {
            spec.handlers
                .iter()
                .any(|h| h.invariants.contains(&i.name) || h.establishes.contains(&i.name))
        })
        .collect();

    let props_with_expr: Vec<&ParsedProperty> = spec
        .properties
        .iter()
        .filter(|p| {
            p.rust_expression
                .as_ref()
                .map(|r| !check::rust_expr_is_unsupported(r))
                .unwrap_or(false)
        })
        .collect();

    out.push_str("#[invariant_test]\n");
    out.push_str(&format!("fn invariant_test(fixture: &mut {fixture}) {{\n"));

    if linked.is_empty() && props_with_expr.is_empty() {
        out.push_str("    // No spec invariants or properties with a Rust-renderable body.\n");
        out.push_str(
            "    // Add `invariant <name> : <expr>` or `property <name> { expr ... preserved_by ... }`\n",
        );
        out.push_str(
            "    // to the spec, then regenerate — Crucible will check it after every action.\n",
        );
    }

    for inv in &linked {
        let Some(rust) = inv.rust_expr.as_deref() else {
            continue;
        };
        let body = sanitize_body_for_fixture(rust);
        out.push_str(&format!(
            "    // Invariant: {}{}\n",
            inv.name,
            inv.lean_expr
                .as_deref()
                .map(|le| format!(" — {le}"))
                .unwrap_or_default()
        ));
        out.push_str(&format!(
            "    fuzz_assert!({body}, \"invariant {} violated\");\n",
            inv.name
        ));
    }

    for prop in &props_with_expr {
        let Some(rust) = prop.rust_expression.as_deref() else {
            continue;
        };
        let body = sanitize_body_for_fixture(rust);
        out.push_str(&format!(
            "    // Property: {}{}\n",
            prop.name,
            prop.expression
                .as_deref()
                .map(|e| format!(" — {e}"))
                .unwrap_or_default()
        ));
        out.push_str(&format!(
            "    fuzz_assert!({body}, \"property {} violated\");\n",
            prop.name
        ));
    }

    out.push_str("}\n");
}

/// The body strings produced by `translate_property_to_rust` (invariants)
/// and AST-rendered Rust (properties) reference `s.<field>`. The Crucible
/// invariant_test gets a `fixture` binding instead — rewrite the prefix.
fn sanitize_body_for_fixture(rust: &str) -> String {
    // Replace `s.` with `fixture.` only at token boundaries — i.e. when
    // the `s` starts a new identifier. Prior char must not be alphanumeric
    // or `_` (otherwise it's a longer ident ending in `s`) AND not `.`
    // (otherwise it's a nested field access like `foo.s.bar`).
    let mut out = String::new();
    let bytes = rust.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let at_start = i == 0;
        let after_break = i > 0
            && !(bytes[i - 1].is_ascii_alphanumeric()
                || bytes[i - 1] == b'_'
                || bytes[i - 1] == b'.');
        if (at_start || after_break)
            && i + 1 < bytes.len()
            && bytes[i] == b's'
            && bytes[i + 1] == b'.'
        {
            out.push_str("fixture.");
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter::parse_str;

    const MINIMAL_SPEC: &str = r#"spec Counter
program_id "11111111111111111111111111111111"

const MAX_COUNT = 100

type State
  | Active of { count : U64 }

type Error
  | E

invariant count_bounded :
  state.count <= MAX_COUNT

handler increment : State.Active -> State.Active {
  permissionless
  invariant count_bounded
  requires state.count < MAX_COUNT
  effect { count := count + 1 }
}
"#;

    /// A multi-handler, multi-account spec exercising the full account-set
    /// guard wiring (signers + writables + a PDA vault) — the shape the
    /// parse gate below must keep valid.
    const MULTI_ACCOUNT_SPEC: &str = r#"spec Vault
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, admin : Pubkey }

type Error
  | E

handler deposit (amount : U64) : State.Active -> State.Active {
  accounts { user : signer, writable
             vault : writable }
  effect { balance := balance + amount }
}

handler withdraw (amount : U64) : State.Active -> State.Active {
  accounts { admin : signer, writable
             vault : writable }
  requires state.balance >= amount
  effect { balance := balance - amount }
}
"#;

    #[test]
    fn deploy_so_emits_absolute_path_else_relative_fallback() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse spec");

        // With an absolute deploy path the harness loads it verbatim, so a
        // `--harness-dir` outside the project still resolves the program.
        let abs = Path::new("/tmp/proj/target/deploy/counter.so");
        let with_abs =
            emit_harness_with_deploy(&spec, InvariantMode::Both, None, Some(abs)).unwrap();
        assert!(
            with_abs
                .contains("ctx.add_program(&program_id, \"/tmp/proj/target/deploy/counter.so\")"),
            "{with_abs}"
        );
        assert!(
            !with_abs.contains("../.."),
            "no relative prefix when absolute"
        );
        syn::parse_file(&with_abs).expect("absolute harness is valid Rust");

        // Without one, fall back to the layout-relative path.
        let with_rel = emit_harness_with_deploy(&spec, InvariantMode::Both, None, None).unwrap();
        assert!(
            with_rel.contains("../../target/deploy/counter.so"),
            "{with_rel}"
        );
    }

    /// Compile/parse gate: the emitted harness must be syntactically valid
    /// Rust in every mode. `syn::parse_file` validates syntax without the
    /// crucible git deps or the Solana toolchain (a full `crucible run` build
    /// stays a manual step — see the fixture READMEs), so this runs in CI as
    /// the regression guard against codegen that emits non-compiling Rust.
    #[test]
    fn emitted_harness_parses_as_valid_rust() {
        let cases = [
            (MINIMAL_SPEC, InvariantMode::Spec, "minimal/spec"),
            (MINIMAL_SPEC, InvariantMode::Protocol, "minimal/protocol"),
            (
                MULTI_ACCOUNT_SPEC,
                InvariantMode::Protocol,
                "multi-account/protocol",
            ),
            (
                MULTI_ACCOUNT_SPEC,
                InvariantMode::Both,
                "multi-account/both",
            ),
        ];
        for (src, mode, label) in cases {
            let spec = parse_str(src).expect("parse spec");
            let harness = emit_harness(&spec, mode, None).expect("emit harness");
            if let Err(e) = syn::parse_file(&harness) {
                panic!("emitted harness ({label}) is not valid Rust: {e}\n---\n{harness}");
            }
        }
    }

    /// A `liquidate`-shaped handler whose PDA seed tuple references a name
    /// (`borrower`) that is neither an account, an argument, nor a flat
    /// state field — the shape that must degrade to the placeholder
    /// derivation, never abort generation (regression: lending example).
    const UNRESOLVED_SEED_SPEC: &str = r#"spec Lending
program_id "11111111111111111111111111111111"

type State
  | Active of { total : U64 }

type Error
  | E

handler liquidate (amount : U64) : State.Active -> State.Active {
  accounts { liquidator : signer, writable
             loan : writable, pda ["loan", borrower] }
  requires state.total >= amount
  effect { total := total - amount }
}
"#;

    #[test]
    fn unresolved_pda_seed_degrades_to_placeholder_without_overlay() {
        let spec = parse_str(UNRESOLVED_SEED_SPEC).expect("parse spec");
        let harness = emit_harness(&spec, InvariantMode::Spec, None)
            .expect("unresolved seed must not abort generation");
        assert!(
            harness.contains("Pubkey::find_program_address(&[], &self.program_id)"),
            "{harness}"
        );
        assert!(
            harness.contains("Placeholder empty-seed derivation"),
            "{harness}"
        );
        assert!(
            harness.contains("cannot resolve PDA seed `borrower`"),
            "{harness}"
        );
        syn::parse_file(&harness).expect("degraded harness must still be valid Rust");
    }

    #[test]
    fn unresolved_pda_seed_stays_fatal_under_account_overlay() {
        // Domain replay pins byte-exact addresses: with an overlay present a
        // placeholder would silently diverge from the resolved plan.
        let spec = parse_str(UNRESOLVED_SEED_SPEC).expect("parse spec");
        let overlay = crate::probe::domain_account_overlay::AccountBindingOverlay {
            schema_version: 1,
            schema_uri: String::new(),
            source_resolved_sequence_schema_uri: String::new(),
            audit_id: None,
            handlers: Default::default(),
        };
        let err = emit_harness(&spec, InvariantMode::Spec, Some(&overlay))
            .expect_err("overlay generation must reject unresolved seeds");
        assert!(err
            .to_string()
            .contains("cannot resolve PDA seed `borrower`"));
    }

    /// Lifecycle spec exercising both directions of the init gate: `Init` is
    /// produced by no handler (creation state → PDA left absent), while
    /// `Inactive` is produced by `pause` (existing paused account → PDA
    /// materialized on demand).
    const LIFECYCLE_PDA_SPEC: &str = r#"spec Lifecycle
program_id "11111111111111111111111111111111"

type State
  | Init
  | Active of { balance : U64 }
  | Inactive

type Error
  | E

handler initialize : State.Init -> State.Active {
  accounts { payer : signer, writable
             vault : writable, pda ["vault"] }
  effect { balance := 0 }
}

handler pause : State.Active -> State.Inactive {
  accounts { admin : signer, writable
             vault : writable, pda ["vault"] }
}

handler resume : State.Inactive -> State.Active {
  accounts { admin : signer, writable
             vault : writable, pda ["vault"] }
}
"#;

    #[test]
    fn init_gate_follows_state_machine_structure_not_name_list() {
        let spec = parse_str(LIFECYCLE_PDA_SPEC).expect("parse spec");
        let mut out = String::new();
        emit_fixture_impl(
            &mut out,
            &spec,
            "LifecycleFixture",
            InvariantMode::Spec,
            None,
            None,
        )
        .expect("emit");
        let action_of = |name: &str| {
            let start = out.find(&format!("pub fn action_{name}")).expect(name);
            let end = out[start..]
                .find("\n    /// ")
                .map(|o| start + o)
                .unwrap_or(out.len());
            &out[start..end]
        };
        // `Init` is unproduced → initialize is a creation transition: the
        // PDA must be left absent (no on-demand create_account block).
        assert!(
            !action_of("initialize").contains("create_account"),
            "initialize must leave its PDA absent:\n{}",
            action_of("initialize")
        );
        // `Inactive` is produced by `pause` → resume acts on an EXISTING
        // paused account: the PDA must be materialized on demand.
        assert!(
            action_of("resume").contains("create_account"),
            "resume must materialize its PDA on demand:\n{}",
            action_of("resume")
        );
        // `Active` is produced by initialize/resume → pause also materializes.
        assert!(action_of("pause").contains("create_account"));
    }

    #[test]
    fn emits_cargo_toml_with_pinned_crucible_deps() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let toml = emit_cargo_toml(&spec);
        assert!(toml.contains("name = \"counter_fuzz\""));
        assert!(toml.contains("crucible-fuzzer"));
        assert!(toml.contains("crucible-test-context"));
        assert!(toml.contains("[features]\ninvariant_test = []"));
        assert!(toml.contains("[workspace]"));
        // Workspace must be empty (isolates Solana/Anchor version chain).
        assert!(toml.contains("[workspace]\n\n[dependencies]"));
    }

    #[test]
    fn emits_fixture_with_state_shadow_fields() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let mut out = String::new();
        emit_fixture_struct(
            &mut out,
            &spec,
            "CounterFixture",
            InvariantMode::Spec,
            false,
        );
        assert!(out.contains("#[derive(Clone)]"));
        assert!(out.contains("struct CounterFixture {"));
        assert!(out.contains("ctx: TestContext,"));
        assert!(out.contains("program_id: Pubkey,"));
        assert!(out.contains("count: u64,"));
    }

    #[test]
    fn emits_action_fn_per_handler() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let mut out = String::new();
        emit_fixture_impl(
            &mut out,
            &spec,
            "CounterFixture",
            InvariantMode::Spec,
            None,
            None,
        )
        .expect("emit");
        assert!(out.contains("#[fuzz_fixture]"));
        assert!(out.contains("impl CounterFixture {"));
        assert!(out.contains("pub fn setup() -> Self"));
        assert!(out.contains("pub fn action_increment(\n        &mut self,\n    ) -> bool"));
        // todo!() agent-fill site for the accounts struct literal
        assert!(out.contains("todo!(\"agent-fill: accounts::Increment"));
    }

    #[test]
    fn emits_invariant_test_with_fuzz_assert() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let mut out = String::new();
        emit_invariant_fn(&mut out, &spec, "CounterFixture", InvariantMode::Spec);
        assert!(out.contains("#[invariant_test]"));
        assert!(out.contains("fn invariant_test(fixture: &mut CounterFixture)"));
        assert!(out.contains("fuzz_assert!"));
        // The assertion line must be fixture-relative (the comment line
        // keeps the original Lean form).
        let fuzz_assert_line = out
            .lines()
            .find(|l| l.contains("fuzz_assert!"))
            .expect("fuzz_assert! line present");
        assert!(
            fuzz_assert_line.contains("fixture.count"),
            "fuzz_assert! must reference fixture.count, got: {fuzz_assert_line}"
        );
        assert!(
            !fuzz_assert_line.contains(" s."),
            "raw `s.` must not survive into the Crucible assertion body: {fuzz_assert_line}"
        );
    }

    #[test]
    fn sbpf_spec_errors_early() {
        // `pragma sbpf` specs are refused (Anchor-only). Synthesise a
        // ParsedSpec directly rather than parse a stub sBPF spec.
        let mut spec = ParsedSpec {
            program_name: "SbpfProg".into(),
            ..Default::default()
        };
        spec.pragmas.push("sbpf".into());
        spec.handlers.push(crate::check::ParsedHandler {
            name: "transfer".into(),
            ..synthetic_handler()
        });
        let tmp = std::env::temp_dir().join(format!("qedgen-crucible-{}", std::process::id()));
        let err = generate(&spec, &tmp, InvariantMode::Spec).expect_err("sBPF spec should error");
        assert!(format!("{err:#}").contains("Anchor"));
    }

    #[test]
    fn protocol_mode_emits_empty_invariant_body() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let mut out = String::new();
        emit_invariant_fn(&mut out, &spec, "CounterFixture", InvariantMode::Protocol);
        assert!(out.contains("#[invariant_test]"));
        assert!(
            out.contains("Protocol mode — no spec assertions"),
            "protocol-mode comment missing: {out}"
        );
        // No spec-derived fuzz_assert! in protocol mode.
        assert!(
            !out.contains("fuzz_assert!"),
            "protocol mode must not emit fuzz_assert!: {out}"
        );
        // Fixture param is unused → underscore-prefixed.
        assert!(out.contains("_fixture: &mut CounterFixture"));
    }

    #[test]
    fn both_mode_keeps_spec_assertions() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let mut out = String::new();
        emit_invariant_fn(&mut out, &spec, "CounterFixture", InvariantMode::Both);
        assert!(out.contains("fuzz_assert!"));
        assert!(out.contains("fixture.count"));
    }

    #[test]
    fn domain_mode_mirrors_same_typed_accepted_scalar_arguments() {
        let spec = parse_str(
            r#"spec AcceptedLimit
program_id "11111111111111111111111111111111"

type State | Active of { last_accepted : U64 }
type Error | E

invariant accepted_limit : state.last_accepted <= 10

handler accept (amount : U64) : State.Active -> State.Active {
  permissionless
  invariant accepted_limit
  effect { last_accepted := amount }
}
"#,
        )
        .expect("parse");

        let domain = emit_harness(&spec, InvariantMode::Both, None).expect("domain harness");
        let protocol =
            emit_harness(&spec, InvariantMode::Protocol, None).expect("protocol harness");

        assert!(domain.contains("if success {\n            self.last_accepted = amount;"));
        assert!(domain.contains("fixture.last_accepted <= 10"));
        assert!(!protocol.contains("self.last_accepted = amount;"));
        assert!(!protocol.contains("invariant accepted_limit violated"));
    }

    // Protocol/Both wrap every `.send()` with a wallet-lamport snapshot +
    // inflation assertion; Spec mode must NOT emit the wrap.
    #[test]
    fn protocol_mode_wraps_send_with_lamport_check() {
        // MINIMAL_SPEC's handler is permissionless (empty signer set); the
        // wrap only fires for a handler with `auth`.
        let src = r#"spec Counter
program_id "11111111111111111111111111111111"

type State
  | Active of { count : U64, authority : Pubkey }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  auth authority
  effect { count := count + delta }
}
"#;
        let spec = parse_str(src).expect("parse");
        let harness =
            emit_harness(&spec, InvariantMode::Protocol, None).expect("emit protocol harness");
        // Shared snapshot infra emitted once at top.
        assert!(
            harness.contains("fn snapshot_account_state")
                && harness.contains("struct AccountSnapshot"),
            "protocol mode must emit the shared AccountSnapshot infra"
        );
        // Every guard's assert fn is emitted, in registry order.
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
                harness.contains(assert_fn),
                "protocol suite must emit `{assert_fn}`:\n{harness}"
            );
        }
        // Both tracked sets snapshotted before .send() (Counter's `auth
        // authority` gives a non-empty wallet set; the account set falls
        // back to signers when there's no accounts block).
        assert!(
            harness.contains("let __wallet_snap = snapshot_account_state(")
                && harness.contains("let __account_snap = snapshot_account_state("),
            "protocol-mode action must snapshot both tracked sets before .send()"
        );
        // Wallet-set guard reads __wallet_snap; account-set guards read
        // __account_snap; per-handler label threads through.
        assert!(
            harness.contains("assert_no_wallet_inflation(&self.ctx, &__wallet_snap, \"bump\")"),
            "wallet-lamport guard must read the wallet snapshot, labelled by handler"
        );
        assert!(
            harness.contains("assert_no_ownership_takeover(&self.ctx, &__account_snap, \"bump\")"),
            "account-set guard must read the account snapshot, labelled by handler"
        );
    }

    // The broadened wallet-lamport guard tracks NON-signer writables too: a
    // drain that credits a plain `recipient` (the textbook missing-authority
    // withdraw — no signer gains) must still land in the tracked set. The
    // signer-only predecessor missed this shape entirely (recipient credited,
    // no signer inflates, and the account-set total is conserved because the
    // recipient is inside it).
    #[test]
    fn wallet_guard_tracks_non_signer_recipient() {
        let authority = crate::check::ParsedHandlerAccount {
            name: "authority".into(),
            is_signer: true,
            is_writable: true,
            ..Default::default()
        };
        let recipient = crate::check::ParsedHandlerAccount {
            name: "recipient".into(),
            is_signer: false, // plain writable — NOT a signer
            is_writable: true,
            ..Default::default()
        };
        let mut handler = synthetic_handler();
        handler.name = "drain".into();
        handler.accounts = vec![authority, recipient];
        let spec = ParsedSpec {
            program_name: "vault".into(),
            handlers: vec![handler],
            ..Default::default()
        };
        let harness =
            emit_harness(&spec, InvariantMode::Protocol, None).expect("emit protocol harness");
        // The non-signer writable still gets a fixture keypair …
        assert!(
            harness.contains("recipient: Rc<Keypair>,"),
            "non-signer writable must get a fixture keypair:\n{harness}"
        );
        // … and joins the wallet snapshot set (both accounts, not just the signer).
        assert!(
            harness.contains("let __wallet_set: Vec<Pubkey> = vec![")
                && harness.contains("self.authority.pubkey(),")
                && harness.contains("self.recipient.pubkey(),"),
            "wallet set must include the non-signer recipient:\n{harness}"
        );
        // … and the broadened guard is wired around the drain's .send().
        assert!(
            harness.contains("assert_no_wallet_inflation(&self.ctx, &__wallet_snap, \"drain\")"),
            "drain must wire the broadened wallet-lamport guard:\n{harness}"
        );
    }

    #[test]
    fn account_overlay_materializes_fixture_identities_in_action_and_signers() {
        let authority = crate::check::ParsedHandlerAccount {
            name: "authority".into(),
            is_signer: true,
            is_writable: true,
            ..Default::default()
        };
        let recipient = crate::check::ParsedHandlerAccount {
            name: "recipient".into(),
            is_writable: true,
            ..Default::default()
        };
        let mut handler = synthetic_handler();
        handler.name = "drain".into();
        handler.accounts = vec![authority, recipient];
        let spec = ParsedSpec {
            program_name: "vault".into(),
            handlers: vec![handler],
            ..Default::default()
        };
        let overlay = crate::probe::domain_account_overlay::AccountBindingOverlay {
            schema_version: 1,
            schema_uri: crate::probe::domain_account_overlay::ACCOUNT_BINDING_OVERLAY_SCHEMA_URI
                .into(),
            source_resolved_sequence_schema_uri: "resolved".into(),
            audit_id: Some("audit".into()),
            handlers: [(
                "drain".to_string(),
                crate::probe::domain_account_overlay::HandlerAccountOverlay {
                    accounts: [
                        ("authority".to_string(), "fixture:recipient".to_string()),
                        ("recipient".to_string(), "fixture:authority".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                    provenance: Default::default(),
                },
            )]
            .into_iter()
            .collect(),
        };

        let harness = emit_harness(&spec, InvariantMode::Both, Some(&overlay)).unwrap();
        assert!(harness.contains("recipient: Rc<Keypair>,"));
        assert!(harness.contains("authority: self.recipient.pubkey(),"));
        assert!(harness.contains("recipient: self.authority.pubkey(),"));
        assert!(harness.contains(".signers(&[&*self.recipient])"));
    }

    #[test]
    fn derives_nested_pdas_from_literal_account_and_numeric_argument_seeds() {
        let src = r#"spec SeededVault
program_id "11111111111111111111111111111111"

pda pool ["pool", authority]
pda ticket ["ticket", pool, lane]

type State
  | Active of { balance : U64 }

type Error
  | E

handler withdraw (lane : U64) : State.Active -> State.Active {
  accounts { ticket : writable, pda [ticket]
             pool : writable, pda [pool]
             authority : signer, writable }
  effect { balance := balance }
}
"#;
        let spec = parse_str(src).expect("parse");
        let harness =
            emit_harness(&spec, InvariantMode::Protocol, None).expect("emit protocol harness");
        let pool = harness
            .find("let __pda_pool = Pubkey::find_program_address")
            .expect("pool derivation");
        let ticket = harness
            .find("let __pda_ticket = Pubkey::find_program_address")
            .expect("ticket derivation");
        assert!(
            pool < ticket,
            "dependency PDA must be derived first:\n{harness}"
        );
        assert!(harness.contains(
            "let __pda_pool = Pubkey::find_program_address(&[b\"pool\", self.authority.pubkey().as_ref()], &self.program_id).0;"
        ));
        assert!(harness.contains(
            "let __pda_ticket = Pubkey::find_program_address(&[b\"ticket\", __pda_pool.as_ref(), lane.to_le_bytes().as_ref()], &self.program_id).0;"
        ));
        assert!(harness.contains("ticket: __pda_ticket,"));
        assert!(harness.contains("pool: __pda_pool,"));
        assert!(harness.contains("__pda_ticket,") && harness.contains("__pda_pool,"));
        assert!(!harness.contains("find_program_address(&[],"));
    }

    #[test]
    fn init_handler_derives_pda_without_precreating_it() {
        let src = r#"spec Initializer
program_id "11111111111111111111111111111111"

pda vault ["vault", payer]

type State
  | Uninitialized
  | Active of { value : U64 }

type Error
  | E

handler initialize : State.Uninitialized -> State.Active {
  accounts { payer : signer, writable
             vault : writable, pda [vault] }
  effect { value := 0 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let harness =
            emit_harness(&spec, InvariantMode::Protocol, None).expect("emit protocol harness");
        assert!(harness.contains(
            "let __pda_vault = Pubkey::find_program_address(&[b\"vault\", self.payer.pubkey().as_ref()], &self.program_id).0;"
        ));
        assert!(harness.contains("vault: __pda_vault,"));
        assert!(
            !harness.contains("get_account(&__pda_vault).is_none()"),
            "init target must remain absent before dispatch:\n{harness}"
        );
    }

    #[test]
    fn spec_mode_does_not_emit_protocol_suite() {
        let src = r#"spec Counter
program_id "11111111111111111111111111111111"

type State
  | Active of { count : U64, authority : Pubkey }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  auth authority
  effect { count := count + delta }
}
"#;
        let spec = parse_str(src).expect("parse");
        let harness = emit_harness(&spec, InvariantMode::Spec, None).expect("emit spec harness");
        assert!(
            !harness.contains("snapshot_account_state")
                && !harness.contains("assert_no_wallet_inflation")
                && !harness.contains("assert_no_ownership_takeover")
                && !harness.contains("assert_closure_integrity"),
            "Spec mode must NOT emit the protocol-invariant suite — protocol-only"
        );
    }

    #[test]
    fn both_mode_emits_lamport_check_and_spec_invariants() {
        let src = r#"spec Counter
program_id "11111111111111111111111111111111"

type State
  | Active of { count : U64, authority : Pubkey }

type Error
  | E

invariant count_bounded :
  state.count <= 1000

handler bump (delta : U64) : State.Active -> State.Active {
  auth authority
  invariant count_bounded
  requires state.count < 1000
  effect { count := count + delta }
}
"#;
        let spec = parse_str(src).expect("parse");
        let harness = emit_harness(&spec, InvariantMode::Both, None).expect("emit both harness");
        // Spec invariant flows in.
        assert!(harness.contains("fuzz_assert!"));
        // Plus the protocol-invariant lamport check.
        assert!(harness.contains("assert_no_wallet_inflation"));
    }

    #[test]
    fn header_mentions_protocol_mode_in_brownfield() {
        let spec = parse_str(MINIMAL_SPEC).expect("parse");
        let s = header(&spec, InvariantMode::Protocol);
        assert!(s.contains("Mode: PROTOCOL"));
        let s2 = header(&spec, InvariantMode::Spec);
        assert!(!s2.contains("Mode: PROTOCOL"));
        let s3 = header(&spec, InvariantMode::Both);
        assert!(s3.contains("Mode: SPEC + PROTOCOL"));
    }

    fn synthetic_handler() -> crate::check::ParsedHandler {
        crate::check::ParsedHandler {
            name: "noop".into(),
            ..Default::default()
        }
    }

    #[test]
    fn pascal_case_converts_snake() {
        assert_eq!(pascal_case("initialize"), "Initialize");
        assert_eq!(pascal_case("update_pool"), "UpdatePool");
        assert_eq!(pascal_case("a_b_c"), "ABC");
    }

    #[test]
    fn spec_name_snake_cases() {
        let mut spec = ParsedSpec {
            program_name: "LiquidityRiskEngine".into(),
            ..Default::default()
        };
        assert_eq!(spec_program_name(&spec), "liquidity_risk_engine");
        spec.program_name = "escrow-split".into();
        assert_eq!(spec_program_name(&spec), "escrow_split");
    }

    #[test]
    fn sanitize_body_rewrites_s_prefix_at_token_boundary() {
        assert_eq!(
            sanitize_body_for_fixture("s.count <= 100"),
            "fixture.count <= 100"
        );
        assert_eq!(
            sanitize_body_for_fixture("s.a + s.b < s.c"),
            "fixture.a + fixture.b < fixture.c"
        );
        // Should not touch identifiers that happen to start with 's.'
        assert_eq!(
            sanitize_body_for_fixture("fixture.s.nested"),
            "fixture.s.nested"
        );
    }
}
