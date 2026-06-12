//! Crucible fuzz harness codegen — v2.18.
//!
//! Produces a `fuzz/<program>/` directory containing a Crucible
//! (https://github.com/asymmetric-research/crucible) fuzz harness derived
//! mechanically from a `.qedspec`. Same backend-codegen pattern as
//! `kani.rs` and `proptest_gen.rs`: `pub fn generate(spec, output_dir)`
//! is the entry point.
//!
//! ## What gets emitted
//!
//! ```text
//! fuzz/<program>/
//! ├── Cargo.toml             # workspace=[], Crucible deps pinned, [features] invariant_test
//! ├── rust-toolchain.toml    # channel = "stable"
//! ├── .gitignore             # target/, crashes/, .fuzz-cache/
//! ├── idls/
//! │   └── README.md          # "drop your IDL here as <name>.json"
//! └── src/
//!     └── main.rs            # the harness — fixture + actions + invariants
//! ```
//!
//! ## Spec → harness mapping
//!
//! - `state { ... }` (mutable fields)         → `Fixture` struct fields (shadowed from LiteSVM after each action)
//! - `program_id "..."`                       → `ctx.add_program(...)` in `setup()`
//! - `handler X { ... }`                      → `pub fn action_x(&mut self, ...) -> bool { ... }`
//! - handler args + bounds                    → `#[range(lo..hi)]` annotations
//! - `invariant Foo : <expr>` (rust_expr)     → `fn invariant_foo(fixture: &mut F)` with `fuzz_assert!`
//! - `property X { ... preserved_by ... }`    → same as invariant (one `invariant_x` fn)
//!
//! ## Scope cap (Anchor only in v2.18)
//!
//! v2.18 ships Anchor-target emission only. sBPF / Pinocchio / Quasar are
//! deferred. `generate()` errors early for sBPF specs.

use anyhow::{bail, Result};
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedInvariant, ParsedProperty, ParsedSpec};
use crate::rust_codegen_util;

/// Which invariant family the emitted harness asserts after each action.
///
/// `Spec` — the v2.18+ default. Emits one `fuzz_assert!` per linked
/// `invariant` / `property` in the parsed spec. Used when a real
/// `.qedspec` drives the harness.
///
/// `Protocol` — v2.21 brownfield mode. Emits an empty `invariant_test`
/// body so the only checks that fire are Crucible's intrinsic
/// crash-detectors (panic, `unwrap` on `None`, `BorrowMutError`,
/// arithmetic overflow / div-by-zero in debug). Used when there's no
/// spec — the bear-hug entry point for an audit. See
/// [[feedback_crucible_crash_first]] / PRD-v2.21 §"Slice 1".
///
/// `Both` — emit spec assertions AND keep protocol-level crashes
/// firing. Identical codegen output to `Spec` today because the
/// protocol crashes don't need any harness instrumentation to fire
/// (they're caught by the LibAFL host loop). Kept as a distinct
/// variant so future protocol-invariant codegen (e.g. lamport-
/// conservation companion module) has a place to dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvariantMode {
    Spec,
    Protocol,
    Both,
}

/// Top-level entry point. Writes a `fuzz/<program>/` directory at
/// `output_dir`. `output_dir` is expected to be the *parent* directory
/// containing the harness — e.g. `fuzz/` — and the function appends the
/// spec name as the subdirectory. Caller may also pass the leaf directly
/// (the function tolerates both — see logic in `harness_dir_for`).
///
/// `mode` controls which invariant family the emitted `invariant_test`
/// asserts after each action. The default `InvariantMode::Spec`
/// preserves v2.20 behavior; brownfield callers (no spec) pass
/// `InvariantMode::Protocol`.
pub fn generate(spec: &ParsedSpec, output_dir: &Path, mode: InvariantMode) -> Result<()> {
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
    std::fs::write(dir.join("src").join("main.rs"), emit_harness(spec, mode)?)?;

    let assertion_count = match mode {
        InvariantMode::Spec | InvariantMode::Both => linked_invariant_count(spec),
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
fn harness_dir_for(spec: &ParsedSpec, output_dir: &Path) -> std::path::PathBuf {
    let leaf = spec_program_name(spec);
    if output_dir.file_name().and_then(|s| s.to_str()) == Some(leaf.as_str()) {
        output_dir.to_path_buf()
    } else {
        output_dir.join(leaf)
    }
}

/// Snake-case program name derived from the spec. Mirrors the convention
/// used by codegen.rs for the program crate name. `pub(crate)` so the
/// CLI dispatcher in `main.rs` can use the same conversion when
/// computing the brownfield harness path (the two derivations have to
/// agree or the IDL ends up in the wrong subdirectory).
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

fn linked_invariant_count(spec: &ParsedSpec) -> usize {
    spec.invariants
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
        .count()
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

fn emit_harness(spec: &ParsedSpec, mode: InvariantMode) -> Result<String> {
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

    emit_fixture_struct(&mut out, spec, &fixture);
    out.push('\n');
    emit_fixture_impl(&mut out, spec, &fixture, mode)?;
    out.push('\n');
    emit_invariant_fn(&mut out, spec, &fixture, mode);

    out.push_str("\n// ---- GENERATED BY QEDGEN — DO NOT EDIT BELOW THIS LINE ----\n");
    Ok(out)
}

/// Emit the v2.21 §S1.2 protocol-invariants helpers: a signer-lamport
/// snapshot/check pair. Inlined into `main.rs` (no separate companion file
/// — keeps the harness layout single-file). Only emitted for
/// `InvariantMode::Protocol` / `Both`.
///
/// The check is asymmetric: signers may LOSE lamports (fees, rent
/// reserves moving to PDAs they create) but must not GAIN lamports
/// during a handler call. A gain means lamports flowed in from outside
/// the tracked set (drain → signer), which under a sealed fuzz harness
/// is a strong bug signal. The check tolerates the standard signer fee
/// (5000 lamports) on first lift; v2.22 can drop that once we wire a
/// per-account "expected delta" annotation from the spec.
///
/// Per [[feedback_crucible_crash_first]] — lamport conservation is the
/// canonical "protocol invariant" Crucible should fuzz against the
/// brownfield surface. Discriminator/size checks deferred to v2.22.
fn emit_protocol_invariants_helpers(out: &mut String) {
    out.push_str("// ── Protocol invariants (v2.21 §S1.2 — lamport conservation) ────────\n");
    out.push_str("// Per-signer asymmetric check: signers may LOSE lamports (fees, rent)\n");
    out.push_str("// but must not GAIN lamports across a handler call. A gain implies\n");
    out.push_str("// lamports flowed in from outside the tracked set — a drain shape.\n");
    out.push_str("// ────────────────────────────────────────────────────────────────────\n");
    out.push_str("fn lamports_of(ctx: &TestContext, pk: &Pubkey) -> u64 {\n");
    out.push_str("    ctx.svm.get_account(pk).map(|a| a.lamports).unwrap_or(0)\n");
    out.push_str("}\n\n");
    out.push_str("/// Snapshot a vector of (pubkey, lamports) tuples for the tracked set.\n");
    out.push_str(
        "fn snapshot_lamports(ctx: &TestContext, tracked: &[Pubkey]) -> Vec<(Pubkey, u64)> {\n",
    );
    out.push_str("    tracked.iter().map(|pk| (*pk, lamports_of(ctx, pk))).collect()\n");
    out.push_str("}\n\n");
    out.push_str("/// Fire fuzz_assert! if any tracked pubkey gained lamports across the\n");
    out.push_str("/// call. Names the pubkey + deltas in the message so crash dumps point\n");
    out.push_str("/// at the offending account.\n");
    out.push_str("fn assert_no_signer_inflation(\n");
    out.push_str("    ctx: &TestContext,\n");
    out.push_str("    before: &[(Pubkey, u64)],\n");
    out.push_str("    label: &str,\n");
    out.push_str(") {\n");
    out.push_str("    for (pk, before_lamports) in before {\n");
    out.push_str("        let after = lamports_of(ctx, pk);\n");
    out.push_str("        if after > *before_lamports {\n");
    out.push_str("            fuzz_assert!(\n");
    out.push_str("                false,\n");
    out.push_str("                \"lamport inflation on signer {} in {}: {} → {} (Δ +{})\",\n");
    out.push_str("                pk,\n");
    out.push_str("                label,\n");
    out.push_str("                before_lamports,\n");
    out.push_str("                after,\n");
    out.push_str("                after - before_lamports,\n");
    out.push_str("            );\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
}

fn header(spec: &ParsedSpec, mode: InvariantMode) -> String {
    let fp = crate::fingerprint::compute_fingerprint(spec);
    let hash = fp
        .file_hashes
        .get("fuzz/src/main.rs")
        .cloned()
        .unwrap_or_default();
    let mut s = crate::banner::banner(None, &hash);
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
            s.push_str("// Mode: PROTOCOL (no spec). invariant_test() body is intentionally\n");
            s.push_str("// empty — Crucible still surfaces panics, unwrap-on-None,\n");
            s.push_str("// BorrowMutError, and arithmetic overflow as crashes via its\n");
            s.push_str("// host-loop crash detector. Spec-invariant assertions are not\n");
            s.push_str("// emitted in this mode.\n");
        }
        InvariantMode::Both => {
            s.push_str("//\n");
            s.push_str("// Mode: SPEC + PROTOCOL. Spec-invariant assertions fire as usual;\n");
            s.push_str("// protocol-level crashes (panic, unwrap-on-None, BorrowMutError,\n");
            s.push_str("// overflow) are caught by Crucible's host-loop crash detector.\n");
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
    // Run the name through spec_program_name first so kebab-case Cargo
    // names (`multi-delegator`) get normalised to snake_case before the
    // PascalCase converter — `to_pascal_case` only splits on `_`, so a
    // raw kebab name would yield `Multi-delegator` (invalid Rust).
    let normalised = spec_program_name(spec);
    let pascal = crate::codegen_shared::to_pascal_case(&normalised);
    let head: &str = if pascal.is_empty() {
        "Program"
    } else {
        &pascal
    };
    format!("{head}Fixture")
}

fn emit_fixture_struct(out: &mut String, spec: &ParsedSpec, fixture: &str) {
    out.push_str("/// Fixture state. Includes Crucible test infrastructure plus shadow\n");
    out.push_str("/// fields mirroring spec state — invariants read from these instead of\n");
    out.push_str("/// from LiteSVM accounts directly so the body translation matches the\n");
    out.push_str("/// proptest / Kani backends.\n");
    out.push_str("#[derive(Clone)]\n");
    out.push_str(&format!("struct {fixture} {{\n"));
    out.push_str("    ctx: TestContext,\n");
    out.push_str("    program_id: Pubkey,\n");

    if spec_is_brownfield_with_idl_accounts(spec) {
        // Brownfield path: one Keypair per non-default, non-PDA account
        // across all handlers (signers AND user-provided writable
        // accounts). The field ident is snake_cased to satisfy Rust
        // naming-convention lints — the original camelCase name is kept
        // on `ParsedHandlerAccount` to match the macro-generated struct
        // field name; the emitter does the case conversion at use sites.
        for name in collect_brownfield_keypair_names(spec) {
            let ident = brownfield_keypair_ident(&name);
            out.push_str(&format!("    {ident}: Rc<Keypair>,\n"));
        }
    } else {
        // Spec-mode path: signing identities derive from auth-cited
        // handlers + named PDAs (v2.21 behavior).
        let signers = collect_signer_idents(spec);
        for sig in &signers {
            out.push_str(&format!("    {sig}: Rc<Keypair>,\n"));
        }
    }

    // Shadow fields for mutable state — read after each action.
    let state_fields = rust_codegen_util::resolve_state_fields(spec);
    let mutable_fields = rust_codegen_util::mutable_fields(state_fields);
    for (fname, ftype) in &mutable_fields {
        let rust_ty = map_simple_type(ftype);
        out.push_str(&format!("    {fname}: {rust_ty},\n"));
    }

    out.push_str("}\n");
}

/// True when at least one handler carries the IDL-derived account
/// metadata that distinguishes brownfield Pinocchio (Codama IDL) from
/// spec-mode (where accounts come from the `.qedspec`'s accounts
/// block). Used to switch the fixture/setup emitters between the two
/// generation paths.
fn spec_is_brownfield_with_idl_accounts(spec: &ParsedSpec) -> bool {
    spec.handlers.iter().any(|h| {
        h.accounts
            .iter()
            .any(|a| a.default_pubkey.is_some() || a.pda_seeds.is_some())
    })
}

/// Map a DSL type to its Rust shadow type. v0 handles only primitives + Pubkey;
/// compound types (records, sum types, Map) fall back to `()` with a TODO so
/// the user knows the field is unshadowed.
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

/// v2.22 S3.3: collect every account name across all handlers that
/// needs a fixture-owned `Rc<Keypair>` field. An account qualifies when
/// it has neither a hardcoded `default_pubkey` (those become
/// `pubkey!("...")` literals) nor a `pda_seeds` derivation. Both signer
/// and non-signer "user" accounts pass — Crucible needs the pubkey
/// tracked for snapshot/restore even when the harness doesn't sign for
/// it.
///
/// Returns names in the IDL's original (camelCase) form. The emitter
/// builds the fixture field ident via `brownfield_keypair_ident` to
/// avoid Rust naming convention warnings.
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

fn emit_fixture_impl(
    out: &mut String,
    spec: &ParsedSpec,
    fixture: &str,
    mode: InvariantMode,
) -> Result<()> {
    let prog = spec_program_name(spec);
    let is_brownfield = spec_is_brownfield_with_idl_accounts(spec);
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
    // Spec-mode harness lives at <root>/fuzz/<prog>/ — `../../target/`
    // walks two levels back to the project root. Brownfield (protocol)
    // mode harness lives at <root>/.qed/fuzz/<prog>/ — needs one extra
    // level. Without this, the harness panics at `ctx.add_program`
    // immediately on startup (No such file or directory).
    let so_path_prefix = match mode {
        InvariantMode::Protocol => "../../..",
        InvariantMode::Spec | InvariantMode::Both => "../..",
    };
    out.push_str(&format!(
        "        ctx.add_program(&program_id, \"{so_path_prefix}/target/deploy/{prog}.so\")\n"
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
    let mutable_fields = rust_codegen_util::mutable_fields(state_fields);

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
        emit_action_fn(out, spec, h, mode)?;
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
) -> Result<()> {
    out.push_str(&format!(
        "    /// {} → action variant. Bodies for non-trivial\n",
        op.name
    ));
    out.push_str("    /// `accounts::X { ... }` literals fall through as todo!() — fill\n");
    out.push_str("    /// via `qedgen codegen --fill` once that hook lands for Crucible.\n");

    // Build param list with #[range] hints when bounds are inferable.
    // Pubkey-typed args are deliberately *not* exposed as Arbitrary-derived
    // action params: Crucible's #[fuzz_fixture] macro can't derive
    // Arbitrary for Solana's Pubkey (its internal `Address` wrapper has no
    // Arbitrary impl), so passing one through the action signature breaks
    // the build. The call-literal logic below inlines `Pubkey::default()`
    // for those fields. Real PDAs / signer pubkeys belong in the
    // `.accounts(...)` literal which the agent fills, not in the call
    // payload that gets fuzzed.
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

    // Body — the Anchor `.call(...).accounts(...).send()` chain. Account
    // context comes from the spec's `accounts` block when present; v0
    // emits `todo!()` for the accounts struct (agent-fill) because the
    // Anchor accounts struct field naming is richer than we carry.
    //
    // `.send()` returns `Result<TxOutcome, ...>`. TxOutcome is Crucible's
    // per-tx status (Success / ProgramError / panic). We collapse both
    // layers into a single bool: transport error from `.send()` itself
    // counts as a failed action for fuzz purposes, same as a program-
    // side error.
    let ix_name = pascal_case(&op.name);
    // Pubkey-typed fields are *not* declared as action params (Crucible
    // can't fuzz Pubkey), so inline `Pubkey::default()` for them in the
    // struct literal. Other params are passed through via field
    // shorthand — they're declared as fn parameters above.
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
    // ── v2.21 §S1.2 — snapshot signer lamports before .send() ───────────
    // Emitted only in Protocol / Both modes. Tracked set = every signer
    // keypair the fixture knows about; signers may lose lamports (fees,
    // rent) but must not gain them across a handler call. PDAs are NOT
    // tracked yet (their pubkeys are derived dynamically at runtime
    // inside the agent-filled `.accounts(...)` literal — wiring spec
    // PDAs into the tracked set is v2.22 scope).
    let want_protocol = matches!(mode, InvariantMode::Protocol | InvariantMode::Both);
    let is_brownfield = spec_is_brownfield_with_idl_accounts(spec);
    let signers = collect_signer_idents(spec);
    if want_protocol && !signers.is_empty() {
        out.push_str("        let __tracked_pubkeys: Vec<Pubkey> = vec![\n");
        for sig in &signers {
            // Brownfield fixture fields go through `brownfield_keypair_ident`
            // for snake-case Rust idents (the original camelCase name is
            // kept on `ParsedHandlerAccount`); the tracker must reference
            // the same ident form or the emit produces `self.escrowSeed`
            // while the fixture has `self.escrow_seed`.
            let ident = if is_brownfield {
                brownfield_keypair_ident(sig)
            } else {
                sig.clone()
            };
            out.push_str(&format!("            self.{ident}.pubkey(),\n"));
        }
        out.push_str("        ];\n");
        out.push_str(
            "        let __lamports_before = snapshot_lamports(&self.ctx, &__tracked_pubkeys);\n",
        );
    }

    out.push_str("        let outcome = self.ctx.program(self.program_id)\n");
    out.push_str(&format!(
        "            .call(instruction::{ix_name} {{ {arg_inits}}})\n"
    ));
    // v2.22 S3.3: when the handler's `accounts` list is populated (set
    // from a Codama IR / Anchor 0.30 IDL by the brownfield path), emit a
    // real `accounts::Foo { ... }` literal. Crucible's generated
    // `accounts::Foo` struct OMITS accounts with fixed-pubkey defaults
    // (publicKeyValueNode → auto-filled by the macro) and snake-cases
    // every remaining field name (`escrowSeed` → `escrow_seed`). So:
    //
    //   - Account carries `default_pubkey` → SKIP. Crucible auto-fills it.
    //   - Account is a PDA (`pda_seeds: Some(...)`) → emit a placeholder
    //     `Pubkey::find_program_address(&[], &self.program_id).0`. Real
    //     seed-aware derivation is v2.22.x; the placeholder compiles
    //     and the program's PDA check rejects the call, which is fine
    //     signal for a crash-first fuzzer.
    //   - Otherwise → `self.<keypair_ident>.pubkey()`.
    //
    // Spec-mode handlers (accounts populated but typically lacking
    // `default_pubkey` / `pda_seeds`) fall through the same path. When
    // the list is empty (no IDL, no spec accounts block) fall back to
    // the v2.21 `todo!()` agent-fill.
    if op.accounts.is_empty() {
        out.push_str(&format!(
            "            .accounts::<accounts::{ix_name}>(todo!(\"agent-fill: accounts::{ix_name} {{{{ ... }}}} from spec accounts block\"))\n"
        ));
    } else {
        out.push_str(&format!("            .accounts(accounts::{ix_name} {{\n"));
        for acc in &op.accounts {
            if acc.default_pubkey.is_some() {
                // Crucible auto-fills fixed-address accounts; the
                // generated struct doesn't have a field for them.
                continue;
            }
            let value = if acc.pda_seeds.is_some() {
                "Pubkey::find_program_address(&[], &self.program_id).0".to_string()
            } else {
                format!("self.{}.pubkey()", brownfield_keypair_ident(&acc.name))
            };
            // Field name on the macro-generated struct is the
            // snake_case form of the IDL's camelCase name.
            let field = brownfield_keypair_ident(&acc.name);
            out.push_str(&format!("                {field}: {value},\n"));
        }
        out.push_str("            })\n");
    }

    // Signers: prefer the IDL's per-account `isSigner` flags when the
    // brownfield path populated them; otherwise fall back to the v2.21
    // `auth X` lift from the spec.
    let brownfield_signers: Vec<&str> = op
        .accounts
        .iter()
        .filter(|a| a.is_signer && a.default_pubkey.is_none() && a.pda_seeds.is_none())
        .map(|a| a.name.as_str())
        .collect();
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

    // ── v2.21 §S1.2 — assert no signer inflation after .send() ──────────
    // Only emitted in Protocol / Both modes and only when the handler had
    // a non-empty tracked set. The check runs whether or not .send()
    // succeeded — a failed tx can still mutate state (CPI rollback isn't
    // guaranteed for every error shape).
    if want_protocol && !signers.is_empty() {
        out.push_str(&format!(
            "        assert_no_signer_inflation(&self.ctx, &__lamports_before, \"{}\");\n",
            op.name,
        ));
    }

    // Post-call: shadow state sync. v0 emits a structured comment because
    // syncing requires knowing the on-chain account struct shape — also
    // agent-fill once the hook lands.
    if !op.effects.is_empty() {
        out.push_str("        if success {\n");
        out.push_str("            // TODO: sync shadow state from the on-chain account here.\n");
        out.push_str("            // For each effect declared in the spec, copy the post-state\n");
        out.push_str("            // field into the matching self.<field>. Example pattern:\n");
        out.push_str(
            "            //   let acc = self.ctx.read_anchor_account::<StateAcct>(&pda).unwrap();\n",
        );
        for (field, _kind, _rhs) in &op.effects {
            out.push_str(&format!("            //   self.{field} = acc.{field};\n"));
        }
        out.push_str("        }\n");
    }

    out.push_str("        success\n");
    out.push_str("    }\n");
    Ok(())
}

/// Infer a `#[range(lo..hi)]` annotation for a handler param when the
/// spec carries a constant upper bound for the param's type. v0
/// heuristic: scan declared constants for one whose name matches
/// `MAX_<TYPE_TAG>` or `<TYPE_TAG>_MAX` and use it as the upper bound.
/// Falls back to "" (no annotation) — Crucible defaults to the type's
/// full range with boundary-biased mutation.
fn infer_range_attr(_spec: &ParsedSpec, _ptype: &str) -> String {
    // v0: no inference; the user can add #[range(..)] manually after gen.
    // v0.1+ can lift bounds from `requires p <= MAX` or `where` clauses.
    String::new()
}

/// snake_case → PascalCase (used for Anchor `instruction::Foo` / `accounts::Foo`).
fn pascal_case(s: &str) -> String {
    let mut out = String::new();
    let mut upper_next = true;
    for c in s.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn emit_invariant_fn(out: &mut String, spec: &ParsedSpec, fixture: &str, mode: InvariantMode) {
    // Protocol-only mode: empty body, with a one-line comment naming the
    // intrinsic detectors. v2.21 ships no companion-module instrumentation
    // for lamport-conservation / discriminator checks — that's a v2.21.1
    // polish (PRD-v2.21 §S1.2 sBPF carve-out also applies to the lamport
    // diff that needs schema info). Today's "protocol" surface is whatever
    // Crucible's host loop already catches.
    if matches!(mode, InvariantMode::Protocol) {
        out.push_str("#[invariant_test]\n");
        out.push_str(&format!("fn invariant_test(_fixture: &mut {fixture}) {{\n"));
        out.push_str("    // Protocol mode — no spec assertions. Crucible surfaces panics,\n");
        out.push_str("    // unwrap-on-None, BorrowMutError, and overflow as crashes via\n");
        out.push_str("    // its host-loop detector.\n");
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
        emit_fixture_struct(&mut out, &spec, "CounterFixture");
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
        emit_fixture_impl(&mut out, &spec, "CounterFixture", InvariantMode::Spec).expect("emit");
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
        // Body is rewritten to fixture-relative — the comment line above
        // retains the original Lean form for human readability, but the
        // assertion line itself must reference fixture.
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
        // Any spec with `pragma sbpf` should be refused — Crucible v2.18
        // ships Anchor-only. We synthesise a minimal ParsedSpec rather
        // than rely on the parser accepting a stub sBPF spec.
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

    // v2.21 §S1.2 — Protocol/Both modes wrap every `.send()` with a
    // before/after signer-lamport snapshot and inflation assertion. Spec
    // mode (the v2.20 default) must NOT emit the wrap so its output stays
    // byte-identical to v2.20.
    #[test]
    fn protocol_mode_wraps_send_with_lamport_check() {
        // MINIMAL_SPEC's `increment` has `permissionless` (no auth, no
        // signers). For the wrap to actually fire we need a handler that
        // produces a non-empty signer set. Use a spec with `auth`.
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
        let harness = emit_harness(&spec, InvariantMode::Protocol).expect("emit protocol harness");
        // Helpers emitted once at top.
        assert!(
            harness.contains("fn snapshot_lamports"),
            "protocol mode must emit the snapshot_lamports helper"
        );
        assert!(
            harness.contains("fn assert_no_signer_inflation"),
            "protocol mode must emit the inflation check"
        );
        // Action body snapshots before AND asserts after.
        assert!(
            harness.contains("let __lamports_before = snapshot_lamports("),
            "protocol-mode action must snapshot before .send()"
        );
        assert!(
            harness.contains("assert_no_signer_inflation(&self.ctx, &__lamports_before,"),
            "protocol-mode action must check inflation after .send()"
        );
        // Per-handler label threads through.
        assert!(
            harness.contains("assert_no_signer_inflation(&self.ctx, &__lamports_before, \"bump\")"),
            "inflation check must label by handler name"
        );
    }

    #[test]
    fn spec_mode_does_not_emit_lamport_check() {
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
        let harness = emit_harness(&spec, InvariantMode::Spec).expect("emit spec harness");
        assert!(
            !harness.contains("snapshot_lamports"),
            "Spec mode must NOT emit the protocol helpers — preserves v2.20 output"
        );
        assert!(
            !harness.contains("assert_no_signer_inflation"),
            "Spec mode must NOT emit the inflation check"
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
        let harness = emit_harness(&spec, InvariantMode::Both).expect("emit both harness");
        // Spec invariant flows in.
        assert!(harness.contains("fuzz_assert!"));
        // Plus the protocol-invariant lamport check.
        assert!(harness.contains("assert_no_signer_inflation"));
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
            doc: None,
            who: None,
            on_account: None,
            pre_status: None,
            post_status: None,
            takes_params: vec![],
            guard_str: None,
            guard_str_rust: None,
            aborts_if: vec![],
            requires: vec![],
            ensures: vec![],
            modifies: None,
            let_bindings: vec![],
            aborts_total: false,
            permissionless: false,
            effects: vec![],
            effect_on_error: vec![],
            accounts: vec![],
            transfers: vec![],
            emits: vec![],
            invariants: vec![],
            establishes: vec![],
            properties: vec![],
            schema_includes: vec![],
            calls: vec![],
            effect_branches: None,
            abstract_binders: vec![],
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
            program_name: "PercolatorRiskEngine".into(),
            ..Default::default()
        };
        assert_eq!(spec_program_name(&spec), "percolator_risk_engine");
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
