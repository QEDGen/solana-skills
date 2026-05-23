mod anchor_adapt;
mod anchor_check;
mod anchor_extractor;
mod anchor_project;
mod anchor_resolver;
mod api;
mod aristotle;
mod arithmetic_symbol_probe;
mod asm2lean;
mod ast;
mod banner;
mod check;
mod chumsky_adapter;
mod chumsky_parser;
mod cluster;
mod codegen;
mod consolidate;
mod cpi_substitute;
mod crucible_brownfield;
mod crucible_gen;
mod crucible_probe;
mod deps;
mod drift;
mod feedback;
mod fill;
mod fingerprint;
mod handler_intent;
mod idl;
mod idl2spec;
mod import_resolver;
mod init;
mod integration_test;
mod interface_gen;
mod kani;
mod kani_impl;
mod lean_gen;
mod lifecycle_probe;
mod miri_verify;
mod native_extractor;
mod paired_validator_probe;
mod pinocchio_extractor;
mod pinocchio_probe;
mod pinocchio_to_spec;
mod probe;
mod probe_repro;
mod project;
mod prompts;
mod proofs_bootstrap;
mod proptest_gen;
mod qed_lock;
mod qed_manifest;
mod quantifier;
mod ratchet;
mod ratify;
mod reconcile;
mod regen_drift;
mod rust_codegen_util;
mod sbpf_verify;
mod shank_probe;
mod spec_hash;
mod unit_test;
mod upstream_check;
mod validate;
mod verify;
mod verify_counterexample;
mod verify_kani_parse;
mod verify_probe_repros;
mod verify_proptest_parse;

use anyhow::{ensure, Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

/// Find the bugs your tests miss — from one spec file
#[derive(Parser)]
#[command(name = "qedgen")]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Solana program framework target for greenfield codegen
/// (`qedgen init --target ...`). `anchor` and `quasar` are wired
/// end-to-end; `pinocchio` reserves the CLI surface but is not yet
/// implemented — selecting it errors at the init dispatcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Target {
    /// Anchor-compatible Rust program. `use anchor_lang::prelude::*`,
    /// `Context<X>`, `Result<()>`, `#[program] pub mod`, `'info`
    /// lifetimes on `#[derive(Accounts)]` structs. Auto-derived
    /// instruction discriminators.
    Anchor,
    /// Quasar (Blueshift) Rust program. `#![no_std]`,
    /// `use quasar_lang::prelude::*`, `Ctx<X>`, `Result<(),
    /// ProgramError>`, `#[program] mod`, explicit
    /// `#[instruction(discriminator = N)]` on each handler.
    Quasar,
    /// Pinocchio (no_std) Rust program. Reserved CLI surface; codegen
    /// is not yet implemented and selecting it errors.
    Pinocchio,
}

/// Runtime override for `qedgen probe --runtime <X>`. v2.19 adds the
/// Pinocchio surface; other entries are reserved for parity with the
/// detector but route through the generic bootstrap envelope today.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum RuntimeOverride {
    Pinocchio,
    Anchor,
    Quasar,
    Native,
    Sbpf,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate Lean 4 proofs using Leanstral API
    Generate {
        /// Path to prompt file
        #[arg(long)]
        prompt_file: PathBuf,

        /// Directory to write generated Lean project
        #[arg(long)]
        output_dir: PathBuf,

        /// Number of independent completions (pass@N)
        #[arg(long, default_value = "4")]
        passes: usize,

        /// Sampling temperature
        #[arg(long, default_value = "0.6")]
        temperature: f64,

        /// Max tokens per completion
        #[arg(long, default_value = "16384")]
        max_tokens: usize,

        /// Validate completions with 'lake build Best'
        #[arg(long)]
        validate: bool,

        /// Include Mathlib dependency (enables u128 arithmetic helpers)
        #[arg(long)]
        mathlib: bool,
    },

    /// Fill sorry markers in a Lean file using Leanstral
    FillSorry {
        /// Path to Lean file containing sorry markers
        #[arg(long)]
        file: PathBuf,

        /// Output path (default: overwrite input file)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Number of independent attempts per sorry
        #[arg(long, default_value = "3")]
        passes: usize,

        /// Sampling temperature
        #[arg(long, default_value = "0.3")]
        temperature: f64,

        /// Max tokens per completion
        #[arg(long, default_value = "16384")]
        max_tokens: usize,

        /// Validate filled file with 'lake build'
        #[arg(long)]
        validate: bool,

        /// Auto-escalate to Aristotle if sorry markers remain after Leanstral
        #[arg(long)]
        escalate: bool,
    },

    /// Brownfield adapter for existing Anchor programs. Two modes:
    ///
    /// `--program <c>` (scaffold): parses `<c>/src/lib.rs`, finds the
    /// `#[program]` mod, walks each instruction to its handler body,
    /// and emits a `.qedspec` skeleton with TODO markers for state
    /// machine / requires / effects. Round-trips through the parser.
    ///
    /// `--program <c> --spec <s>` (attribute): given an existing spec,
    /// emits one `#[qed(verified, spec = ..., handler = ..., hash = ...,
    /// spec_hash = ...)]` line per handler. Paste each above its
    /// handler `pub fn`; future body edits fire `compile_error!`
    /// until you re-run this command.
    Adapt {
        /// Path to the program crate (the directory containing the
        /// program's own `Cargo.toml`, with `src/lib.rs` inside).
        #[arg(long)]
        program: PathBuf,

        /// Path to an existing .qedspec. Switches to attribute-emit
        /// mode: prints one `#[qed(verified, ...)]` line per handler.
        /// Without this flag, scaffold mode emits a starter `.qedspec`.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Path to write output. Without this flag, prints to stdout.
        /// In scaffold mode, writes a `.qedspec`; in attribute mode,
        /// writes a `// === handler … ===` report.
        #[arg(long)]
        out: Option<PathBuf>,

        /// Manually point an unrecognized handler at its actual
        /// implementation. Format: `<handler>=<rust_path>` where the
        /// path is `module::sub::function` (or just `function`).
        /// Repeatable: pass once per handler. Drift's custom
        /// dispatcher is the canonical use case.
        #[arg(long = "handler", value_name = "NAME=PATH")]
        handler_overrides: Vec<String>,
    },

    /// Generate a Tier-0 .qedspec interface block from an Anchor IDL.
    ///
    /// Shape only — program ID, discriminators, accounts, argument types.
    /// No requires/ensures (effects need semantic understanding the IDL does
    /// not carry). Upgrade to Tier 1 by declaring what the callee does; see
    /// docs/design/spec-composition.md §2.
    Interface {
        /// Path to the Anchor IDL JSON file.
        #[arg(long)]
        idl: PathBuf,

        /// Path to write the generated .qedspec. If omitted, the rendered
        /// source is printed to stdout so the caller can redirect.
        #[arg(long, conflicts_with = "vendor")]
        out: Option<PathBuf>,

        /// Drop the interface into `.qed/interfaces/<program>.qedspec` (the
        /// vendored-library convention). Resolved via the nearest `.qed/`.
        /// Overrides `--out`; errors if no `.qed/` ancestor is found.
        #[arg(long)]
        vendor: bool,
    },

    /// Probe a `.qedspec` for category-coverage gaps. Emits JSON consumed
    /// by the auditor subagent (or readable directly).
    ///
    /// Modes:
    /// - **Spec-aware** (`--spec <path>`): runs runtime-agnostic predicates
    ///   against the parsed `.qedspec`, emits per-handler findings.
    /// - **Spec-less** (`--bootstrap --root <path>`): walks a brownfield
    ///   project, detects runtime, discovers handlers, emits the work-list
    ///   envelope (handlers + applicable categories) for the auditor to
    ///   investigate via Read/Grep on the impl source.
    /// - **Fuzz, spec-driven** (`--fuzz <budget> --spec <path>`): builds
    ///   the spec-driven Crucible harness and surfaces crashes as Findings.
    /// - **Fuzz, brownfield** (`--fuzz <budget> --root <path>`, v2.21):
    ///   synthesises a minimal handler list from the project, emits a
    ///   protocol-only Crucible harness under `<root>/.qed/fuzz/`, and
    ///   surfaces panics / unwrap-on-None / BorrowMutError / overflow
    ///   as crashes. No `.qedspec` required.
    Probe {
        /// Path to `.qedspec` file (spec-aware mode)
        #[arg(long, conflicts_with = "bootstrap")]
        spec: Option<PathBuf>,

        /// Spec-less mode — walk a project root and emit the auditor work list
        #[arg(long, requires = "root")]
        bootstrap: bool,

        /// Project root for spec-less mode. Used by:
        /// - `--bootstrap` (emits auditor work list)
        /// - `--fuzz` without `--spec` (v2.21 brownfield protocol-mode
        ///   Crucible — emits a harness at `<root>/.qed/fuzz/<prog>/`
        ///   and surfaces panic / unwrap / overflow crashes).
        ///
        /// Typically the program crate dir, e.g. `programs/lending`.
        #[arg(long)]
        root: Option<PathBuf>,

        /// Pinocchio audit mode (v2.19). Walks `<path>` and emits the
        /// site catalogue + SAFETY-comment metadata the audit subagent
        /// consumes. Detection auto-routes via `Cargo.toml` (`pinocchio`
        /// dep), so `--program <path>` is the same as `--bootstrap
        /// --root <path>` when the runtime is Pinocchio — `--program`
        /// is the user-facing alias documented in the PRD.
        #[arg(long, conflicts_with_all = ["spec", "bootstrap"])]
        program: Option<PathBuf>,

        /// Override runtime detection (`pinocchio`, `anchor`, `quasar`,
        /// `native`, `sbpf`). Only `pinocchio` has dedicated probe
        /// output today; the others fall back to the generic bootstrap
        /// envelope.
        #[arg(long, value_enum)]
        runtime: Option<RuntimeOverride>,

        /// Coverage-guided fuzz probe engine (v2.18). Drives a generated
        /// Crucible harness for the given budget and converts each crash
        /// into a Finding with `Reproducer::Crucible`. Different engine
        /// from the pattern-match predicates above — both can run; both
        /// emit into the same `findings[]`.
        ///
        /// Pair with either `--spec <path>` (spec-driven harness,
        /// asserts spec invariants) or `--root <project-path>` (v2.21
        /// brownfield protocol-mode — emits a harness with an empty
        /// `invariant_test()` body and surfaces only intrinsic
        /// Crucible crashes: panic / unwrap-on-None / BorrowMutError /
        /// arithmetic overflow). Passing both layers spec invariants on
        /// top of protocol crashes.
        ///
        /// Budget is wall-clock seconds (e.g. `300` for 5 min). Pass `0`
        /// to disable.
        #[arg(long)]
        fuzz: Option<u64>,

        /// Crucible harness directory. Defaults to `./fuzz/<spec_program>`,
        /// matching `qedgen codegen --crucible` output.
        #[arg(long)]
        harness_dir: Option<PathBuf>,

        /// Skip the 30s smoke pre-flight that surfaces same-class bugs
        /// before burning the full budget on duplicates.
        #[arg(long)]
        no_smoke: bool,

        /// Use Crucible's stateful mode (action-chain pool, ~10× throughput).
        /// Stateless default keeps repros short and reads cleanly; opt
        /// into stateful once shallow findings are cleared.
        #[arg(long)]
        stateful: bool,

        /// v2.19 M1: lift findings into candidate spec clauses (clusters)
        /// the auditor subagent uses to drive the scaffold-to-spec
        /// interview. Schema v3 — adds `clusters[]` to the probe envelope.
        /// Off by default; v2-shape consumers see no change.
        #[arg(long)]
        emit_spec_candidates: bool,

        /// v2.19 M1.5/M1.7: when `--emit-spec-candidates` is also set,
        /// materialize the full audit working set into this directory:
        /// `interview.md` (user-editable prompts), `clusters.json` (the
        /// full cluster envelope), and `skeleton.qedspec` (the
        /// pre-interview structural skeleton). The companion
        /// `qedgen ratify --audit-dir <path>` consumes all three to
        /// produce the final spec. Conventionally
        /// `.qed/audit/<timestamp>/`.
        #[arg(long, requires = "emit_spec_candidates")]
        audit_dir: Option<PathBuf>,
    },

    /// Ratify a scaffold-to-spec interview into a `.qedspec` + side-files.
    ///
    /// Inverse of `qedgen probe --emit-spec-candidates --audit-dir <X>`.
    /// Reads the audit working set (`interview.md`, `clusters.json`,
    /// `skeleton.qedspec`) the user has answered, and emits:
    ///
    /// - `<program>.qedspec` — skeleton with the user's accepted clauses
    ///   merged into handler bodies / top-level invariants.
    /// - `.qed/plan/scoping.md` — rejected clusters with rationale.
    /// - `.qed/findings/scaffold-to-spec-<id>.md` — bug-flagged clusters.
    Ratify {
        /// Audit working-set directory (the one passed to `probe
        /// --audit-dir`). Must contain `interview.md`, `clusters.json`,
        /// and `skeleton.qedspec`.
        #[arg(long)]
        audit_dir: PathBuf,

        /// Output path for the generated `.qedspec`. Defaults to
        /// `<project_root>/<project_name>.qedspec`, derived from the
        /// audit-dir grandparent.
        #[arg(long)]
        out: Option<PathBuf>,

        /// Override the rejected-cluster scoping notes path. Defaults
        /// to `<project_root>/.qed/plan/scoping.md` (append-on-write).
        #[arg(long)]
        scoping_out: Option<PathBuf>,

        /// Override the bug-flagged findings directory. Defaults to
        /// `<project_root>/.qed/findings/`.
        #[arg(long)]
        findings_dir: Option<PathBuf>,
    },

    /// Scaffold a .qedspec from an Anchor IDL JSON file.
    ///
    /// v2.10 cleanup: this subcommand previously also generated SPEC.md
    /// (via `--from-spec` and the default `--format md` path). The
    /// SPEC.md generators have been removed — `.qedspec` is QEDGen's
    /// front-door human-readable artifact (`feedback_spec_design.md`),
    /// and parallel Markdown duplicates drifted from spec without a
    /// real consumer. `qedgen spec` is now exclusively IDL → `.qedspec`.
    Spec {
        /// Path to Anchor IDL JSON file
        #[arg(long)]
        idl: PathBuf,

        /// Directory to write the scaffolded `.qedspec` (default:
        /// `./formal_verification`). The file is named
        /// `<idl-stem>.qedspec`.
        #[arg(long, default_value = "./formal_verification")]
        output_dir: PathBuf,
    },

    /// Consolidate multiple proof projects into a single Lean project
    Consolidate {
        /// Directory containing proof subdirectories (each with Best.lean)
        #[arg(long)]
        input_dir: PathBuf,

        /// Directory to write consolidated Lean project
        #[arg(long)]
        output_dir: PathBuf,
    },

    /// Transpile an sBPF assembly file (.s) to a Lean 4 program module
    #[command(name = "asm2lean")]
    Asm2Lean {
        /// Path to the sBPF assembly source file
        #[arg(long)]
        input: PathBuf,

        /// Path for the generated Lean 4 file
        #[arg(long)]
        output: PathBuf,

        /// Lean namespace (default: derived from output filename)
        #[arg(long)]
        namespace: Option<String>,
    },

    /// Set up the global validation workspace
    Setup {
        /// Directory for the validation workspace (default: platform cache dir)
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Include Mathlib dependency (fetches ~8GB pre-built cache)
        #[arg(long)]
        mathlib: bool,
    },

    /// Initialize a new formal verification project
    Init {
        /// Project name (alphanumeric + underscores)
        #[arg(long)]
        name: String,

        /// Path to the authored `.qedspec` (file or directory). Written
        /// into `.qed/config.json` so `qedgen check`/`codegen` can resolve
        /// it without an explicit `--spec`. Relative to the program root.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// sBPF assembly source file (runs asm2lean automatically)
        #[arg(long)]
        asm: Option<PathBuf>,

        /// Include Mathlib dependency
        #[arg(long)]
        mathlib: bool,

        /// Also generate the program crate + Kani harnesses for the
        /// named framework target. `anchor` and `quasar` are fully
        /// implemented; `pinocchio` reserves the CLI surface but its
        /// codegen branch is not yet implemented and errors cleanly
        /// when selected. Omit to skip program scaffolding entirely.
        #[arg(long, value_enum)]
        target: Option<Target>,

        /// Output directory (default: ./formal_verification)
        #[arg(long, default_value = "./formal_verification")]
        output_dir: PathBuf,
    },

    /// Validate a spec — lint, coverage, drift, and verification report
    ///
    /// Default (no flags): runs lint + coverage.
    /// With --explain: generates a Markdown verification report.
    /// With --drift: detects code drift in #[qed(verified)] functions.
    Check {
        /// Path to the spec file (.qedspec or a directory of fragments).
        /// Optional — falls back to the `spec` field in the nearest
        /// `.qed/config.json` discovered by walking up from cwd.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Path to the proofs directory
        #[arg(long, default_value = "./formal_verification")]
        proofs: PathBuf,

        /// Show operation × property coverage matrix
        #[arg(long)]
        coverage: bool,

        /// Generate a Markdown verification report with intent descriptions
        #[arg(long)]
        explain: bool,

        /// Output file for --explain report (default: stdout)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Path to the generated Rust program directory (enables code drift detection)
        #[arg(long)]
        code: Option<PathBuf>,

        /// Path to an existing Anchor program crate (the directory holding
        /// `Cargo.toml`, with `src/lib.rs` inside). Cross-checks the spec's
        /// handler list against the program's `#[program]` mod and reports
        /// any spec/program drift. Pure read; useful as a CI gate.
        #[arg(long)]
        anchor_project: Option<PathBuf>,

        /// Path to Rust source for #[qed(verified)] drift detection
        #[arg(long)]
        drift: Option<PathBuf>,

        /// Auto-update drift hashes in source files
        #[arg(long)]
        update_hashes: bool,

        /// Enable transitive drift detection (check if callees have changed)
        #[arg(long)]
        deep: bool,

        /// Path to generated Kani harness file (enables Kani drift detection)
        #[arg(long)]
        kani: Option<PathBuf>,

        /// Path to sBPF assembly source (hash check + lake build)
        #[arg(long)]
        asm: Option<PathBuf>,

        /// Output as JSON (for agent consumption)
        #[arg(long)]
        json: bool,

        /// Refuse to update `qed.lock`; error if the on-disk lock is stale
        /// or missing. Used in CI to detect un-bumped imports.
        #[arg(long)]
        frozen: bool,

        /// v2.26 Slice 4c — escalate `--check-upstream`-style pin
        /// mismatches surfaced by `--frozen` to CRIT severity, so a
        /// stale `upstream { binary_hash }` pin fails the check instead
        /// of just warning. Use in release-blocking CI; default `--frozen`
        /// stays warning-only (P2) for everyday local runs.
        #[arg(long, requires = "frozen")]
        strict: bool,

        /// Force-refresh the github source cache for every imported dep.
        /// Wipes `~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/` and
        /// re-clones. Use after a force-pushed tag or when the
        /// QEDGEN_CACHE_TTL window (default 7 days) hasn't expired but
        /// you know the upstream changed.
        #[arg(long)]
        no_cache: bool,

        /// Regenerate bundled examples into temporary directories and fail
        /// if committed generated artifacts have drifted.
        #[arg(long)]
        regen_drift: bool,

        /// Root containing bundled Rust examples for --regen-drift.
        #[arg(long, default_value = "examples/rust", requires = "regen_drift")]
        examples_root: PathBuf,

        /// v2.21 §"Slice 5": with `--regen-drift`, also write the
        /// regenerated content into the repo so the committed example
        /// outputs match current codegen. Useful for rebasing PRs across
        /// codegen-touching releases. Does NOT touch user-owned files
        /// (handler bodies, Spec.lean proofs) — only the codegen-owned
        /// set that `--regen-drift` already compares.
        #[arg(long, requires = "regen_drift")]
        write: bool,
    },

    /// Run the generated harnesses against the generated implementation.
    ///
    /// `check` validates the spec; `verify` validates the code the spec
    /// produced. Default (no flags) runs every backend whose artifact is
    /// present on disk. Use --proptest/--kani/--lean to target one backend.
    Verify {
        /// Path to the spec file (.qedspec). Optional — falls back to the
        /// `spec` field in the nearest `.qed/config.json` discovered by
        /// walking up from cwd, mirroring `check` and `codegen`.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Run proptest harnesses (cargo test --release)
        #[arg(long)]
        proptest: bool,

        /// Path to the proptest harness file (matches codegen default)
        #[arg(long, default_value = "./programs/tests/proptest.rs")]
        proptest_path: PathBuf,

        /// Run Kani BMC harnesses (cargo kani)
        #[arg(long)]
        kani: bool,

        /// Path to the Kani harness file (matches codegen default)
        #[arg(long, default_value = "./programs/tests/kani.rs")]
        kani_path: PathBuf,

        /// Run Lean proofs (lake build)
        #[arg(long)]
        lean: bool,

        /// Path to the Lean project directory
        #[arg(long, default_value = "./formal_verification")]
        lean_dir: PathBuf,

        /// v2.19: run Pinocchio Miri reproducers under
        /// `.qed/probes/pinocchio/*/repro_miri.rs` via
        /// `cargo +nightly miri test`. UB / aliasing / overflow
        /// diagnostics surface as findings; dual-execution divergence
        /// against Mollusk repros surfaces as Critical.
        #[arg(long)]
        miri: bool,

        /// Stop on the first failing backend
        #[arg(long)]
        fail_fast: bool,

        /// Output as JSON (for agent consumption)
        #[arg(long)]
        json: bool,

        /// Diff every imported library interface's pinned
        /// `upstream_binary_hash` against the on-chain `.so`. Shells out to
        /// `solana program dump` per `feedback_dispatch_over_reimplement.md`
        /// — requires the Solana CLI in PATH. Skips dependencies without a
        /// pinned hash. Non-zero exit on any mismatch.
        #[arg(long)]
        check_upstream: bool,

        /// Override the RPC endpoint passed through to `solana program dump
        /// --url <rpc>`. If omitted, the Solana CLI uses whatever cluster is
        /// configured in `~/.config/solana/cli/config.yml`.
        #[arg(long)]
        rpc_url: Option<String>,

        /// Refuse to reach the network. Any dependency that would require
        /// an on-chain fetch reports as Error instead. Skipped entries (no
        /// pinned hash / no program_id) still skip cleanly. CI gate friendly.
        #[arg(long)]
        offline: bool,

        /// v2.26 Slice 4c — suppress the upstream binary-hash check
        /// even when the lock declares pinned hashes. Mismatches demote
        /// to `Info` and the verify run stays green. Intended for
        /// offline development; **do not** use in CI — a real stale pin
        /// is silently masked. Pairs with the auto-on behavior of
        /// `--check-upstream`: when any `upstream { binary_hash }` is
        /// pinned, verify runs the check by default unless this flag is
        /// set.
        #[arg(long)]
        upstream_stale_ok: bool,

        /// Run probe reproducers under `<project>/target/qedgen-repros/`
        /// (PLAN-v2.16 D4). Each repro is a Mollusk-driven Rust test
        /// asserting a specific probe finding's bug fires; the verb
        /// captures pass/fail per finding so the auditor / next probe
        /// invocation can drop findings whose repros didn't reproduce.
        /// Pre-D3 (no repros generated yet) this is a no-op that emits
        /// a `note: no repros found` placeholder.
        #[arg(long)]
        probe_repros: bool,

        /// Run the Crucible coverage-guided fuzz engine (v2.18). Thin
        /// alias over `qedgen probe --fuzz <budget>` — wraps the
        /// findings as a BackendReport so they render through the same
        /// `format_human` named-counterexample surface as Kani /
        /// proptest. Value is wall-clock seconds (e.g. 300 = 5 min).
        #[arg(long)]
        crucible: Option<u64>,

        /// Harness directory for `--crucible`. Defaults to
        /// `./fuzz/<spec_program>/`, matching `qedgen codegen --crucible`.
        #[arg(long)]
        crucible_harness_dir: Option<PathBuf>,

        /// Skip Crucible's 30s smoke pre-flight before the full run.
        #[arg(long)]
        crucible_no_smoke: bool,

        /// Use Crucible's stateful mode (action-chain pool).
        #[arg(long)]
        crucible_stateful: bool,

        /// v2.27 Track D2 — exit non-zero if any imported interface
        /// declares `ensures` clauses (Tier-1+) but the provider did NOT
        /// ship a Lake-buildable proof package alongside its qedspec
        /// (`<source>/.qed/proofs/<Iface>.lean` + `lakefile.lean`).
        /// Tier-0 shape-only imports (no ensures) and sentinel-pinned
        /// native programs (System) are exempt — the former are
        /// flagged by the `cpi_no_callee_ensures` P1 lint instead, and
        /// the latter are runtime trust boundaries that no proof
        /// package can express.
        ///
        /// Default-off in v2.27: the bundled stdlib still ships as
        /// Stance-1 (binary_hash axiom discharge), so default-on would
        /// always fail on `from "spl"` / `from "metaplex"` imports.
        /// Re-evaluate in v2.28 after Track C2 ships bundled proofs.
        #[arg(long)]
        require_verified: bool,

        /// v2.27 Track D3 — walk the transitive dep graph and run
        /// `lake build` against every imported proof package, not just
        /// the consumer's own Lean tree. The resolver returns deps in
        /// DFS-pre-order so iteration is naturally bottom-up. Each
        /// layer's pass/fail is reported individually; exits non-zero
        /// if any layer fails. Cycle detection is reused from
        /// `import_resolver::resolve_recursive`.
        ///
        /// Implied by `--lean` when imports ship verified proofs but
        /// not auto-enabled — operators may want to verify only the
        /// consumer's own tree (the v2.26 behavior) before paying the
        /// per-layer Lake build cost.
        #[arg(long)]
        recursive: bool,
    },

    /// Lint one Anchor IDL for mainnet-readiness before first deploy.
    ///
    /// Runs the ratchet P-rule preflight on the IDL and reports every
    /// future-upgrade landmine it finds — missing `version: u8` prefix,
    /// no `_reserved` trailing padding, unpinned discriminators, name
    /// collisions, writable accounts with no signer. Complements
    /// `qedgen check` / `qedgen verify` (which prove semantics) by
    /// proving the on-chain shape is safe to evolve.
    ///
    /// Exit codes: 0 = additive/safe, 1 = breaking, 2 = unsafe.
    Readiness {
        /// Path to the IDL JSON (typically target/idl/<program>.json
        /// from `anchor build` or `quasar build`).
        #[arg(long, required_unless_present = "list_rules")]
        idl: Option<PathBuf>,

        /// Print the catalog of P-rules applied by `readiness` and exit.
        /// Replaces the pre-embed `ratchet list-rules` step: users who
        /// installed qedgen via `install.sh` / `npx skills add` don't
        /// have the standalone `ratchet` CLI on PATH, but the rule set
        /// is linked in as a library, so surface it here.
        #[arg(long)]
        list_rules: bool,

        /// Treat `--idl` as a Quasar-emitted IDL rather than an Anchor
        /// IDL. Auto-detected when a `Quasar.toml` (and no shadowing
        /// `Anchor.toml`) lives in the current working directory; pass
        /// explicitly to force Quasar mode from elsewhere.
        #[arg(long)]
        quasar: bool,

        /// Output as JSON (for agent / CI consumption)
        #[arg(long)]
        json: bool,
    },

    /// Diff an old vs new Anchor IDL and flag every upgrade-unsafe change.
    ///
    /// Runs the ratchet R-rule engine over the pair. Catches the
    /// failure modes `solana program upgrade` won't — field reorders,
    /// discriminator changes, orphaned accounts, PDA seed drift,
    /// signer/writable tightening.
    ///
    /// Exit codes: 0 = additive/safe, 1 = breaking, 2 = unsafe.
    CheckUpgrade {
        /// Path to the baseline IDL (the one on-chain today).
        #[arg(long, required_unless_present = "list_rules")]
        old: Option<PathBuf>,

        /// Path to the candidate IDL (the one the upgrade would ship).
        #[arg(long, required_unless_present = "list_rules")]
        new: Option<PathBuf>,

        /// Acknowledge a specific unsafe finding so it reports as
        /// additive instead (repeatable). Pass `--list-rules` to see the
        /// full flag catalog.
        #[arg(long = "unsafe")]
        unsafes: Vec<String>,

        /// Declare an account as having a migration in source; demotes
        /// R003/R004 findings for that account to Additive (repeatable).
        #[arg(long = "migrated-account")]
        migrated_accounts: Vec<String>,

        /// Declare an account as having `realloc = ...` in source;
        /// demotes R005 for that account to Additive (repeatable).
        #[arg(long = "realloc-account")]
        realloc_accounts: Vec<String>,

        /// Print the catalog of R-rules applied by `check-upgrade` and
        /// exit. Same motivation as on `readiness`: the rule set is
        /// linked in as a library so there's no `ratchet list-rules`
        /// binary on PATH — this flag fills the gap.
        #[arg(long)]
        list_rules: bool,

        /// Treat both IDLs as Quasar-emitted rather than Anchor.
        /// Auto-detected from `Quasar.toml`; the flag forces Quasar
        /// mode when running from elsewhere. Mixed-framework diffs
        /// aren't supported — Anchor IDLs and Quasar IDLs both lower
        /// into the same IR, but the loaders differ and a "rename a
        /// program from Anchor to Quasar" diff is out of scope.
        #[arg(long)]
        quasar: bool,

        /// Output as JSON (for agent / CI consumption)
        #[arg(long)]
        json: bool,
    },

    /// Generate committed artifacts from a qedspec
    ///
    /// Default (no flags): generates the Rust program skeleton for the
    /// chosen `--target` (default: `anchor`). Use flags to generate
    /// additional artifacts, or `--all` for everything.
    Codegen {
        /// Path to the spec file (.qedspec or a directory of fragments).
        /// Optional — falls back to the `spec` field in the nearest
        /// `.qed/config.json` discovered by walking up from cwd.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Framework target for the Rust program crate. `anchor` is
        /// fully implemented (default); `quasar` is fully implemented
        /// (Blueshift's `quasar_lang`); `pinocchio` reserves the CLI
        /// surface but its codegen branch is not yet implemented.
        #[arg(long, value_enum, default_value_t = Target::Anchor)]
        target: Target,

        /// Output directory for the generated Rust program crate
        #[arg(long, default_value = "./programs")]
        output_dir: PathBuf,

        /// Generate Kani proof harnesses
        #[arg(long)]
        kani: bool,

        /// Output path for Kani harnesses (default: ./programs/tests/kani.rs —
        /// sits INSIDE the program package so `cargo kani --tests` finds it
        /// via `programs/Cargo.toml`. Before v2.6 the default was
        /// `./tests/kani.rs`, which landed without a governing Cargo.toml;
        /// that layout silently broke `qedgen verify`.)
        #[arg(long, default_value = "./programs/tests/kani.rs")]
        kani_output: PathBuf,

        /// Generate impl-targeted Kani harnesses (v2.26): call the user's
        /// real Anchor handler against a symbolic `Accounts` context and
        /// assert the spec's `ensures` clauses. Pairs with `--kani` (which
        /// produces the spec-model harnesses). Even without this flag,
        /// emission is auto-triggered when any handler has `modifies`
        /// listing fields absent from its `effect` block (the v2.25 LP-
        /// shape signal indicating the impl is expected to fill those
        /// fields). Anchor target only in v2.26.
        #[arg(long)]
        kani_impl: bool,

        /// Output path for impl-targeted Kani harnesses (default:
        /// `./programs/tests/kani_impl.rs`). Separate file from the
        /// spec-model `kani.rs` so `cargo kani --harness` can target
        /// either set without ambiguity.
        #[arg(long, default_value = "./programs/tests/kani_impl.rs")]
        kani_impl_output: PathBuf,

        /// Generate unit tests (plain Rust, cargo test)
        #[arg(long)]
        test: bool,

        /// Output path for unit tests (default: ./programs/src/tests.rs)
        #[arg(long, default_value = "./programs/src/tests.rs")]
        test_output: PathBuf,

        /// Generate proptest harnesses (property-based testing)
        #[arg(long)]
        proptest: bool,

        /// Output path for proptest harnesses
        /// (default: ./programs/tests/proptest.rs — see --kani-output for why).
        #[arg(long, default_value = "./programs/tests/proptest.rs")]
        proptest_output: PathBuf,

        /// Generate a Crucible coverage-guided fuzz harness (v2.18).
        /// Anchor target only; sBPF / Pinocchio specs error early.
        #[arg(long)]
        crucible: bool,

        /// Parent directory for the generated Crucible harness. The harness
        /// lives at `<dir>/<program_name>/` (or `<dir>/` when `<dir>` already
        /// ends with the program name). Default: `./fuzz`.
        #[arg(long, default_value = "./fuzz")]
        crucible_output: PathBuf,

        /// Generate in-process SVM integration test scaffolds
        #[arg(long)]
        integration: bool,

        /// Output path for integration tests (default: ./src/integration_tests.rs)
        #[arg(long, default_value = "./src/integration_tests.rs")]
        integration_output: PathBuf,

        /// Generate Lean 4 proofs from qedspec
        #[arg(long)]
        lean: bool,

        /// Output path for Lean file (default: ./formal_verification/Spec.lean)
        #[arg(long, default_value = "./formal_verification/Spec.lean")]
        lean_output: PathBuf,

        /// Generate GitHub Actions CI workflow
        #[arg(long)]
        ci: bool,

        /// Output path for CI workflow (default: .github/workflows/verify.yml)
        #[arg(long, default_value = ".github/workflows/verify.yml")]
        ci_output: PathBuf,

        /// sBPF assembly source file (for CI workflow)
        #[arg(long)]
        ci_asm: Option<String>,

        /// Path to the Anchor IDL the generated CI should lint with
        /// `qedgen readiness`. When set, the emitted verify.yml runs
        /// ratchet after the verification jobs — any breaking /
        /// unsafe finding fails the build. Value is the path relative
        /// to the repo root, e.g. `target/idl/escrow.json`.
        #[arg(long)]
        ci_ratchet: Option<String>,

        /// Generate all artifacts
        #[arg(long)]
        all: bool,

        /// DEPRECATED (slated for v3.0 removal): emit one stdout prompt
        /// block per handler whose body still contains a `todo!()`. The
        /// agent can already do this directly — grep for `todo!()` in
        /// programs/, read the spec's handler block, edit each body in
        /// place. The prompt-emission layer is redundant with the
        /// agent's own file tools. Flag remains functional in v2.x to
        /// avoid breaking existing scripts.
        #[arg(long)]
        fill: bool,

        /// Restrict --fill to one handler by name (deprecated with --fill).
        #[arg(long)]
        handler: Option<String>,

        /// DEPRECATED (slated for v3.0 removal): emit prompt blocks for
        /// every `todo!()` site in the generated integration test file.
        /// Same direct-edit guidance applies — the agent reads the spec
        /// and the test file, edits in place.
        #[arg(long)]
        fill_tests: bool,
    },

    /// Aristotle theorem prover (Harmonic) — sorry-filling via long-running agent
    #[command(subcommand)]
    Aristotle(AristotleCommands),

    /// Emit a unified drift report (Rust handlers + Lean proofs vs .qedspec)
    ///
    /// Report-only; never modifies files. Exits 0 on no drift, 1 on drift.
    /// Pair with `--json` for machine-readable output consumable by agents.
    Reconcile {
        /// Path to the spec file (.qedspec). Optional — falls back to the
        /// `spec` field in the nearest `.qed/config.json` discovered by
        /// walking up from cwd, mirroring `check`, `codegen`, and `verify`.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Root directory to scan for Rust handlers (recursive)
        #[arg(long, default_value = "programs/")]
        code: PathBuf,

        /// Directory containing Proofs.lean
        #[arg(long, default_value = "formal_verification/")]
        proofs: PathBuf,

        /// Emit JSON instead of the human-readable report
        #[arg(long)]
        json: bool,
    },

    /// File a GitHub issue with the last failure's context.
    ///
    /// Bundles qedgen version, OS/arch, detected runtime, the most recent
    /// command's stderr (from `.qed/last-error.log`), and the relevant
    /// `.qedspec` excerpt into a Markdown body. Writes a local copy to
    /// `.qed/feedback/<timestamp>.md`, previews the issue, asks for
    /// confirmation, then files via `gh issue create` (falling back to a
    /// pre-filled GitHub URL if `gh` is unavailable). Override the target
    /// repo with `QEDGEN_FEEDBACK_REPO=owner/repo`.
    Feedback {
        /// Free-form description of what happened. Appears at the top of
        /// the issue body. Helpful but not required — defaults to a
        /// "describe what happened" placeholder when omitted.
        #[arg(long)]
        note: Option<String>,

        /// Override the auto-derived issue title (default: "[qedgen
        /// <version>] <command> failed: <first-stderr-line>").
        #[arg(long)]
        title: Option<String>,

        /// Path to the `.qedspec` to excerpt. Default: parse the spec
        /// path out of the last error's stderr, or fall back to the
        /// single `.qedspec` in the current directory.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Render the title and body to stdout and exit. No local
        /// artifact, no remote submission. Useful for piping into other
        /// tools.
        #[arg(long)]
        dry_run: bool,

        /// Skip the interactive confirmation prompt and submit straight
        /// away. Required in non-interactive shells (CI) — without it the
        /// submit defaults to no.
        #[arg(long)]
        yes: bool,

        /// Suppress the post-submit browser open when falling back to the
        /// pre-filled URL. The URL is still printed to stdout.
        #[arg(long)]
        no_open: bool,
    },
}

#[derive(Subcommand)]
enum AristotleCommands {
    /// Submit a Lean project to Aristotle for sorry-filling
    Submit {
        /// Path to the Lean project directory (must contain lakefile.lean)
        #[arg(long)]
        project_dir: PathBuf,

        /// Custom prompt for Aristotle (default: "Fill in all sorry placeholders with valid proofs")
        #[arg(long)]
        prompt: Option<String>,

        /// Output directory for the solved project (default: project_dir)
        #[arg(long)]
        output_dir: Option<PathBuf>,

        /// Wait for completion (may take minutes to hours)
        #[arg(long)]
        wait: bool,

        /// Polling interval in seconds (default: 30)
        #[arg(long)]
        poll_interval: Option<u64>,
    },

    /// Check the status of an Aristotle project (use --wait to poll until done)
    Status {
        /// Project ID returned by 'aristotle submit'
        project_id: String,

        /// Poll until the project reaches a terminal status, then download the result
        #[arg(long)]
        wait: bool,

        /// Polling interval in seconds (default: 30, requires --wait)
        #[arg(long)]
        poll_interval: Option<u64>,

        /// Output directory for the solved project (default: current dir, requires --wait)
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
    },

    /// Download the result of a completed Aristotle project
    Result {
        /// Project ID
        project_id: String,

        /// Output directory for the solved project
        #[arg(long, default_value = ".")]
        output_dir: PathBuf,
    },

    /// Cancel a running Aristotle project
    Cancel {
        /// Project ID
        project_id: String,
    },

    /// List recent Aristotle projects
    List {
        /// Maximum number of projects to show
        #[arg(long, default_value = "10")]
        limit: u32,

        /// Filter by status (e.g. IN_PROGRESS, COMPLETE, FAILED)
        #[arg(long)]
        status: Option<String>,
    },
}

/// Walk up from `start` looking for a `.git` directory. Returns true if one
/// is found before hitting the filesystem root. qedgen refuses to write
/// scaffolding unless the user has a git repo — the safety net for
/// regeneration is a clean working tree.
fn has_git_repo(start: &std::path::Path) -> bool {
    let mut cur = match start.canonicalize() {
        Ok(p) => p,
        Err(_) => start.to_path_buf(),
    };
    loop {
        if cur.join(".git").exists() {
            return true;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return false,
        }
    }
}

fn require_git_repo() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    if !has_git_repo(&cwd) {
        eprintln!("qedgen requires a git repo — run `git init` first");
        std::process::exit(1);
    }
    Ok(())
}

/// v2.18 P3 alias: wrap a Crucible fuzz-probe run into a single
/// BackendReport so `qedgen verify --crucible <budget>` renders through
/// the v2.17 named-counterexample human surface alongside the other
/// backends. Each finding's action sequence becomes a counterexample;
/// the harness path lives in BackendReport.detail for context.
fn crucible_backend_report(
    spec: &Path,
    harness_dir: Option<PathBuf>,
    budget_secs: u64,
    no_smoke: bool,
    stateful: bool,
) -> verify::BackendReport {
    use std::time::Instant;
    let start = Instant::now();

    let project_root = spec
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let parsed = match check::parse_spec_file(spec) {
        Ok(p) => p,
        Err(e) => {
            return verify::BackendReport {
                name: "crucible",
                status: verify::BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!("failed to parse spec: {e}")),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            }
        }
    };
    let prog = if parsed.program_name.is_empty() {
        "program".to_string()
    } else {
        // Re-use the snake-case logic via crucible_gen's path: walk chars,
        // insert `_` before each capital. Lighter version inlined here.
        let mut out = String::new();
        let mut prev_lower = false;
        for c in parsed.program_name.chars() {
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
    };
    let harness = harness_dir.unwrap_or_else(|| project_root.join("fuzz").join(&prog));

    let mut ctx = crucible_probe::FuzzProbeContext::new(spec, project_root, harness.clone());
    ctx.fuzz_budget = std::time::Duration::from_secs(budget_secs);
    if no_smoke {
        ctx.smoke_budget = std::time::Duration::ZERO;
    }
    ctx.stateful = stateful;

    let findings = match crucible_probe::run_fuzz_probe(&ctx) {
        Ok(f) => f,
        Err(e) => {
            return verify::BackendReport {
                name: "crucible",
                status: verify::BackendStatus::Failed,
                duration_ms: start.elapsed().as_millis(),
                detail: Some(format!("crucible run failed: {e:#}")),
                log_path: None,
                counterexamples: Vec::new(),
                axioms: Vec::new(),
            }
        }
    };

    let duration_ms = start.elapsed().as_millis();
    let status = if findings.is_empty() {
        verify::BackendStatus::Passed
    } else {
        verify::BackendStatus::Failed
    };

    let counterexamples = findings
        .iter()
        .map(crucible_finding_to_counterexample)
        .collect::<Vec<_>>();

    let detail = if findings.is_empty() {
        Some(format!(
            "no findings in {}s ({} budget). \
             Pass `--crucible <larger>` to go deeper, or `--crucible-stateful` for chain coverage.",
            budget_secs, budget_secs
        ))
    } else {
        Some(format!(
            "{} distinct finding(s). \
             Replay via `crucible show {} <crash> --replay`.",
            findings.len(),
            harness.display(),
        ))
    };

    verify::BackendReport {
        name: "crucible",
        status,
        duration_ms,
        detail,
        log_path: None,
        counterexamples,
        axioms: Vec::new(),
    }
}

/// Map a Crucible Finding into the structured Counterexample shape the
/// v2.17 human renderer consumes. Action sequence flattens to one
/// (name, value) row per action, plus a leading row for the violation
/// category.
fn crucible_finding_to_counterexample(f: &probe::Finding) -> verify_counterexample::Counterexample {
    use verify_counterexample::{Counterexample, CounterexampleVar};
    let mut assignments = Vec::new();
    assignments.push(CounterexampleVar {
        name: "category".to_string(),
        value: f.category_tag.clone(),
        line: None,
    });
    if let Some(probe::Reproducer::Crucible {
        action_sequence,
        crucible_version,
        ..
    }) = &f.reproducer
    {
        for (i, action) in action_sequence.iter().enumerate() {
            assignments.push(CounterexampleVar {
                name: format!("action[{}]", i),
                value: format!(
                    "{}({}){}",
                    action.name,
                    serde_json::to_string(&action.params).unwrap_or_default(),
                    action
                        .error_code
                        .map(|c| format!(" → Custom({})", c))
                        .unwrap_or_else(|| if action.success {
                            " → ok".into()
                        } else {
                            " → fail".into()
                        }),
                ),
                line: None,
            });
        }
        assignments.push(CounterexampleVar {
            name: "crucible_version".to_string(),
            value: crucible_version.clone(),
            line: None,
        });
    }
    Counterexample {
        harness: format!("{} ({})", f.handler, f.category_tag),
        status: "failed".to_string(),
        assignments,
        seed: None,
        failure_message: Some(f.spec_silent_on.clone()),
        source_location: f.reproducer.as_ref().and_then(|r| match r {
            probe::Reproducer::Crucible { crash_path, .. } => Some(crash_path.clone()),
            _ => None,
        }),
    }
}

/// Expand the committed CI template by substituting `{{VERIFY_STEP}}`
/// and `{{RATCHET_STEP}}` with the caller-provided snippets, then
/// normalise trailing whitespace so the workflow file ends with
/// exactly one newline regardless of whether either step was set.
///
/// Factored out of the `Codegen` match arm so the substitution is
/// unit-testable without spawning a process — the template bytes are
/// `include_str!`'d at compile time, so the test wires them in the
/// same way.
/// Pick the Anchor / Quasar loader for `qedgen readiness` and
/// `qedgen check-upgrade`. Explicit `--quasar` always wins; otherwise
/// the framework is inferred from the project marker in the current
/// working directory (`Quasar.toml` → Quasar; default → Anchor). A
/// short stderr banner lights up the first time autodetect picks
/// Quasar so the dev sees which loader fired without re-reading
/// `--help`. Suppressed under `--json` to keep machine consumers'
/// output clean.
fn resolve_framework(explicit_quasar: bool, as_json: bool) -> ratchet::Framework {
    if explicit_quasar {
        return ratchet::Framework::Quasar;
    }
    let detected = ratchet::Framework::detect_from_cwd();
    if detected == ratchet::Framework::Quasar && !as_json {
        eprintln!(
            "qedgen: Quasar project detected (Quasar.toml in cwd) — using ratchet's Quasar IDL parser"
        );
    }
    detected
}

fn expand_ci_template(template: &str, verify_step: &str, ratchet_step: &str) -> String {
    let mut out = template
        .replace("{{VERIFY_STEP}}", verify_step)
        .replace("{{RATCHET_STEP}}", ratchet_step);
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

fn format_lint_warning(warning: &check::CompletenessWarning) -> String {
    let icon = match warning.severity {
        check::Severity::Error => "E",
        check::Severity::Warning => "!",
        check::Severity::Info => "i",
    };
    let mut out = format!(
        "  {} [P{}] [{}] {}\n    Fix: {}",
        icon, warning.priority, warning.rule, warning.message, warning.fix
    );
    if let Some(ref example) = warning.example {
        out.push_str("\n    Example:");
        for line in example.lines() {
            out.push_str("\n      ");
            out.push_str(line);
        }
    }
    if let Some(ref cx) = warning.counterexample {
        out.push_str("\n    Counterexample:");
        out.push_str(&format!(
            "\n      Pre-state:  {}  →  {} ✓",
            cx.pre_state
                .iter()
                .map(|(f, v)| format!("{} = {}", f, v))
                .collect::<Vec<_>>()
                .join(", "),
            cx.pre_check,
        ));
        out.push_str(&format!(
            "\n      Apply:      {} ({})",
            cx.handler,
            cx.effects.join(", "),
        ));
        out.push_str(&format!(
            "\n      Post-state: {}  →  {} {}",
            cx.post_state
                .iter()
                .map(|(f, v)| format!("{} = {}", f, v))
                .collect::<Vec<_>>()
                .join(", "),
            cx.post_check,
            if cx.invariant_holds { "✓" } else { "✗" },
        ));
    }
    if !warning.fix_options.is_empty() {
        out.push_str("\n    Fix options:");
        for (i, opt) in warning.fix_options.iter().enumerate() {
            let label = (b'A' + i as u8) as char;
            out.push_str(&format!(
                "\n      {}) {} — {}",
                label, opt.label, opt.rationale
            ));
            for line in opt.snippet.lines() {
                out.push_str(&format!("\n         {}", line));
            }
        }
    }
    out
}

/// Anchor (and Quasar) probe path used by `qedgen probe --program <root>`.
/// Mirrors the Pinocchio branch's shape: runs the runtime-specific
/// extractor, clusters proto-clauses, optionally materializes the audit
/// working set, and prints the schema-v3 envelope. Anchor doesn't emit
/// per-site findings yet — the auditor SKILL.md handles them at the
/// agent layer via Read+Grep, while the scaffold-to-spec interview
/// works off the extractor's clusters directly.
fn run_anchor_probe(
    prog_root: &Path,
    runtime_final: probe::Runtime,
    emit_spec_candidates: bool,
    audit_dir: Option<&Path>,
) -> Result<()> {
    let applicable = probe::applicable_categories_public(&runtime_final);
    // Anchor handlers come from the existing IDL-aware enumerator;
    // empty if the project layout isn't standard (we don't fail —
    // ratify continues with what it has).
    let handlers_opt = match probe::run_bootstrap(prog_root) {
        Ok(bs) => bs.handlers,
        Err(_) => None,
    };
    let clusters = if emit_spec_candidates {
        let protos = anchor_extractor::extract_proto_clauses(prog_root)?;
        Some(cluster::cluster_protos(protos))
    } else {
        None
    };

    if let (Some(dir), Some(clusters_ref)) = (audit_dir, clusters.as_ref()) {
        std::fs::create_dir_all(dir)?;
        let program_name = prog_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("program")
            .to_string();
        let now_iso = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap_or_else(|_| "unknown".to_string());
        // 1. interview.md
        let md = prompts::render_interview(clusters_ref, &program_name, &now_iso);
        std::fs::write(dir.join("interview.md"), md)?;
        // 2. clusters.json
        let cj = serde_json::to_string_pretty(clusters_ref)?;
        std::fs::write(dir.join("clusters.json"), cj)?;
        // 3. skeleton.qedspec — reuse anchor_adapt::adapt as the
        // structural skeleton. If it fails (e.g., non-standard project
        // layout), fall back to a minimal stub that ratify still
        // accepts.
        let skeleton = match anchor_adapt::adapt(prog_root, &std::collections::HashMap::new()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "warning: anchor_adapt::adapt failed ({}); writing minimal skeleton",
                    e
                );
                format!(
                    "spec {}\n\ntype State | Init | Active\ntype Error | InvalidArgument\n",
                    program_name
                )
            }
        };
        std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
        eprintln!("Wrote audit working set to {}", dir.display());
    }

    let output = probe::ProbeOutput {
        version: probe::schema_version(),
        mode: probe::Mode::SpecLess,
        spec_path: None,
        project_root: Some(prog_root.display().to_string()),
        runtime: Some(runtime_final),
        handlers: handlers_opt,
        applicable_categories: Some(applicable),
        findings: Vec::new(),
        clusters,
        dispatcher_kind: None,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Native (solana-program) probe path. Same envelope shape as Anchor;
/// reuses the Pinocchio-style source-walk for the skeleton because
/// Native has no IDL to drive a richer emitter.
fn run_native_probe(
    prog_root: &Path,
    runtime_final: probe::Runtime,
    emit_spec_candidates: bool,
    audit_dir: Option<&Path>,
) -> Result<()> {
    let applicable = probe::applicable_categories_public(&runtime_final);
    let clusters = if emit_spec_candidates {
        let protos = native_extractor::extract_proto_clauses(prog_root)?;
        Some(cluster::cluster_protos(protos))
    } else {
        None
    };

    if let (Some(dir), Some(clusters_ref)) = (audit_dir, clusters.as_ref()) {
        std::fs::create_dir_all(dir)?;
        let program_name = prog_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("program")
            .to_string();
        let now_iso = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap_or_else(|_| "unknown".to_string());
        let md = prompts::render_interview(clusters_ref, &program_name, &now_iso);
        std::fs::write(dir.join("interview.md"), md)?;
        let cj = serde_json::to_string_pretty(clusters_ref)?;
        std::fs::write(dir.join("clusters.json"), cj)?;
        // Native skeleton: pinocchio_to_spec::render_skeleton_native
        // accepts any `pub fn` (Native has no naming convention vs
        // Pinocchio's `process_*` prefix).
        let skeleton = pinocchio_to_spec::render_skeleton_native(prog_root, &program_name)?;
        std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
        eprintln!("Wrote audit working set to {}", dir.display());
    }

    // v2.20 §S2.1: also try Shank-shape central-match dispatcher on
    // the native `--program` path. The richer envelope (handlers +
    // dispatcher_kind) is purely additive; absent detection leaves the
    // path unchanged.
    // v2.20 §S2.2: also classify each handler body and emit narrowed
    // `applicable_categories` per entry.
    let (handlers, dispatcher_kind) = match shank_probe::detect_shank_dispatcher(prog_root)
        .ok()
        .flatten()
    {
        Some(cat) => {
            let hs: Vec<probe::BootstrapHandler> = cat
                .handlers
                .into_iter()
                .map(|sh| {
                    let (intent_tag, narrowed) =
                        narrow_shank_handler(&sh.name, &sh.entry_fn, prog_root, &applicable);
                    probe::BootstrapHandler {
                        name: sh.name,
                        source_file: sh.file,
                        enum_variant: Some(sh.enum_variant),
                        entry_fn: Some(sh.entry_fn),
                        line: Some(sh.line),
                        applicable_categories: narrowed,
                        intent_tag,
                    }
                })
                .collect();
            (Some(hs), Some("shank_central_match".to_string()))
        }
        None => (None, None),
    };

    let output = probe::ProbeOutput {
        version: probe::schema_version(),
        mode: probe::Mode::SpecLess,
        spec_path: None,
        project_root: Some(prog_root.display().to_string()),
        runtime: Some(runtime_final),
        handlers,
        applicable_categories: Some(applicable),
        findings: Vec::new(),
        clusters,
        dispatcher_kind,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// v2.20 §S2.2 helper for the `--program` native flow. Mirrors the
/// `probe::run_bootstrap` path: resolves the handler body, classifies
/// intent, and returns `(intent_tag_str, narrowed_categories)`. The
/// narrowed list is only emitted when the classifier actually drops
/// at least one category; an unchanged list is reported as `None`
/// so the global `applicable_categories` field stays authoritative.
fn narrow_shank_handler(
    handler_name: &str,
    entry_fn: &str,
    project_root: &Path,
    global: &[String],
) -> (Option<String>, Option<Vec<String>>) {
    let Some((_path, body)) = handler_intent::resolve_handler_body(entry_fn, project_root) else {
        return (None, None);
    };
    let tag = handler_intent::classify_handler_body(handler_name, &body);
    let tag_str = tag.map(|t| t.as_str().to_string());
    let narrowed = handler_intent::filter_categories(global, tag);
    if narrowed.len() == global.len() {
        return (tag_str, None);
    }
    (tag_str, Some(narrowed))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let command_name = command_name_of(&cli.command).to_string();
    let cwd_for_capture = std::env::current_dir().ok();

    let result = dispatch(cli.command).await;

    // Persist the failing command's stderr so the next `qedgen feedback`
    // invocation has real context to bundle. Skip when `feedback` itself
    // is what failed — overwriting the error it would have reported on
    // is exactly the wrong outcome.
    if command_name != "feedback" {
        if let (Err(e), Some(cwd)) = (result.as_ref(), cwd_for_capture.as_ref()) {
            let _ = feedback::capture_last_error(cwd, &command_name, e);
        }
    }

    result
}

/// Top-level subcommand name for telemetry and the last-error log
/// header. Aristotle's sub-verbs collapse to the single `"aristotle"`
/// label — that's the user-facing surface they invoked.
fn command_name_of(c: &Commands) -> &'static str {
    match c {
        Commands::Generate { .. } => "generate",
        Commands::FillSorry { .. } => "fill-sorry",
        Commands::Adapt { .. } => "adapt",
        Commands::Interface { .. } => "interface",
        Commands::Probe { .. } => "probe",
        Commands::Ratify { .. } => "ratify",
        Commands::Spec { .. } => "spec",
        Commands::Consolidate { .. } => "consolidate",
        Commands::Asm2Lean { .. } => "asm2lean",
        Commands::Setup { .. } => "setup",
        Commands::Init { .. } => "init",
        Commands::Check { .. } => "check",
        Commands::Verify { .. } => "verify",
        Commands::Readiness { .. } => "readiness",
        Commands::CheckUpgrade { .. } => "check-upgrade",
        Commands::Codegen { .. } => "codegen",
        Commands::Aristotle(_) => "aristotle",
        Commands::Reconcile { .. } => "reconcile",
        Commands::Feedback { .. } => "feedback",
    }
}

async fn dispatch(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::Generate {
            prompt_file,
            output_dir,
            passes,
            temperature,
            max_tokens,
            validate,
            mathlib,
        } => {
            ensure!(passes > 0, "passes must be greater than 0");
            ensure!(
                (0.0..=2.0).contains(&temperature),
                "temperature must be between 0.0 and 2.0"
            );
            ensure!(max_tokens > 0, "max_tokens must be greater than 0");
            if validate {
                deps::require_lean()?;
            }
            let prompt = std::fs::read_to_string(&prompt_file)?;
            api::generate_proofs(
                &prompt,
                &output_dir,
                passes,
                temperature,
                max_tokens,
                validate,
                None,
                mathlib,
            )
            .await?;
        }

        Commands::FillSorry {
            file,
            output,
            passes,
            temperature,
            max_tokens,
            validate,
            escalate,
        } => {
            ensure!(passes > 0, "passes must be greater than 0");
            ensure!(
                (0.0..=2.0).contains(&temperature),
                "temperature must be between 0.0 and 2.0"
            );
            ensure!(max_tokens > 0, "max_tokens must be greater than 0");
            if validate {
                deps::require_lean()?;
            }
            api::fill_sorry(
                &file,
                output.as_deref(),
                passes,
                temperature,
                max_tokens,
                validate,
            )
            .await?;

            // If --escalate: check for remaining sorry markers, submit to Aristotle
            if escalate {
                let result_path = output.as_deref().unwrap_or(&file);
                let content = std::fs::read_to_string(result_path)?;
                if content.contains("sorry") {
                    eprintln!("\nSorry markers remain after Leanstral. Escalating to Aristotle...");
                    // Derive project dir from the file path (go up to lakefile.lean)
                    let project_dir = result_path
                        .parent()
                        .and_then(|p| {
                            if p.join("lakefile.lean").exists() {
                                Some(p.to_path_buf())
                            } else {
                                p.parent().and_then(|pp| {
                                    if pp.join("lakefile.lean").exists() {
                                        Some(pp.to_path_buf())
                                    } else {
                                        None
                                    }
                                })
                            }
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Could not find lakefile.lean above {}. \
                                 Run `qedgen aristotle submit` manually with --project-dir.",
                                result_path.display()
                            )
                        })?;
                    let prompt = "Fill in all sorry placeholders with valid proofs".to_string();
                    aristotle::fill_sorry(&project_dir, &project_dir, &prompt, true, None).await?;
                } else {
                    eprintln!("All sorry markers filled by Leanstral.");
                }
            }
        }

        Commands::Adapt {
            program,
            spec,
            out,
            handler_overrides,
        } => {
            let mut overrides = std::collections::HashMap::new();
            for raw in &handler_overrides {
                let (name, parsed) = anchor_adapt::parse_handler_override(raw)?;
                overrides.insert(name, parsed);
            }
            match spec {
                Some(spec_path) => {
                    let entries =
                        anchor_adapt::compute_attributes(&program, &spec_path, &overrides)?;
                    let rendered = anchor_adapt::render_attributes(&entries);
                    if let Some(path) = out {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&path, &rendered)?;
                        eprintln!("Wrote {} ({} bytes)", path.display(), rendered.len());
                    } else {
                        print!("{}", rendered);
                    }
                }
                None => {
                    if let Some(path) = out {
                        anchor_adapt::adapt_to_file(&program, &path, &overrides)?;
                    } else {
                        let rendered = anchor_adapt::adapt(&program, &overrides)?;
                        print!("{}", rendered);
                    }
                }
            }
        }

        Commands::Interface { idl, out, vendor } => {
            if vendor {
                // Drop into `.qed/interfaces/<program>.qedspec`. The program
                // name is derived from the IDL metadata; the directory is
                // resolved via the nearest `.qed/` ancestor of cwd.
                let cwd = std::env::current_dir()?;
                let (qed_dir, config) = init::discover_qed_config(&cwd).ok_or_else(|| {
                    anyhow::anyhow!(
                        "--vendor requires a `.qed/` ancestor of {} — run `qedgen init` first or pass `--out`",
                        cwd.display()
                    )
                })?;
                let project_root = qed_dir.parent().unwrap_or(std::path::Path::new("."));
                let interfaces_dir = project_root.join(
                    config
                        .interfaces_dir
                        .as_deref()
                        .unwrap_or(".qed/interfaces"),
                );
                let stem = idl
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("interface");
                let target = interfaces_dir.join(format!("{}.qedspec", stem));
                interface_gen::generate_to_file(&idl, &target)?;
                eprintln!("Vendored interface to {}", target.display());
            } else if let Some(path) = out {
                interface_gen::generate_to_file(&idl, &path)?;
                eprintln!("Wrote Tier-0 interface to {}", path.display());
            } else {
                let rendered = interface_gen::generate(&idl)?;
                print!("{}", rendered);
            }
        }

        Commands::Probe {
            spec,
            bootstrap,
            root,
            program,
            runtime,
            fuzz,
            harness_dir,
            no_smoke,
            stateful,
            emit_spec_candidates,
            audit_dir,
        } => {
            // v2.19: --program <path> (with optional --runtime pinocchio)
            // routes through the Pinocchio site enumerator and emits a
            // probe-shaped JSON envelope whose `findings` are the
            // site catalogue mapped 1:1 to candidate findings. The
            // audit subagent picks the relevant probe markdown per
            // site kind and writes the reproducer.
            if let Some(prog_root) = &program {
                let detected = probe::detect_runtime_public(prog_root);
                let runtime_final = match runtime {
                    Some(RuntimeOverride::Pinocchio) => probe::Runtime::Pinocchio,
                    Some(RuntimeOverride::Anchor) => probe::Runtime::Anchor,
                    Some(RuntimeOverride::Quasar) => probe::Runtime::Quasar,
                    Some(RuntimeOverride::Native) => probe::Runtime::Native,
                    Some(RuntimeOverride::Sbpf) => probe::Runtime::Sbpf,
                    None => detected.clone(),
                };

                // M3: Anchor (and Quasar) route through anchor_extractor
                // for scaffold-to-spec interviews. Doesn't emit per-site
                // findings yet — the auditor SKILL.md handles that via
                // Read+Grep at the agent layer. Clusters come directly
                // from source patterns.
                if matches!(
                    runtime_final,
                    probe::Runtime::Anchor | probe::Runtime::Quasar
                ) {
                    return run_anchor_probe(
                        prog_root,
                        runtime_final,
                        emit_spec_candidates,
                        audit_dir.as_deref(),
                    );
                }

                // M4 (preview): Native Rust programs (solana-program
                // dep, no anchor / pinocchio) route through
                // native_extractor. Pattern coverage is narrower than
                // Anchor's because Native has no framework conventions
                // — see native_extractor.rs docs for the v1 detector
                // set.
                if matches!(runtime_final, probe::Runtime::Native) {
                    return run_native_probe(
                        prog_root,
                        runtime_final,
                        emit_spec_candidates,
                        audit_dir.as_deref(),
                    );
                }

                if !matches!(runtime_final, probe::Runtime::Pinocchio) {
                    eprintln!(
                        "warning: --program targets {} (detected: {:?}); \
                         emitting bootstrap envelope only. Pass --runtime <name> to force a specific extractor.",
                        prog_root.display(),
                        detected,
                    );
                    let output = probe::run_bootstrap(prog_root)?;
                    println!("{}", serde_json::to_string_pretty(&output)?);
                    return Ok(());
                }
                let catalogue = pinocchio_probe::scan_program(prog_root)?;
                let mut findings = pinocchio_probe::findings_from_catalogue(&catalogue);
                // v2.22 Slice 1 — arithmetic-symbol catalog. Runtime-
                // agnostic source-scanner findings (currently:
                // silent_success_arithmetic) merge into the same
                // envelope as the Pinocchio-specific probes. Both run
                // on the `--program <root>` path; the
                // arithmetic-symbol rules fire on any Rust source
                // regardless of detected runtime.
                findings.extend(arithmetic_symbol_probe::scan_program(prog_root)?);
                // v2.22 Slice 2 — paired-validator asymmetry across
                // files. Runs alongside the per-file scanners; merges
                // into the same envelope.
                findings.extend(paired_validator_probe::scan_program(prog_root)?);
                // v2.22 Slice 4 — lifecycle external-state catalog.
                // Cross-file: pairs authority-conferring CPI grants
                // with close-handler bodies that don't tear them down.
                findings.extend(lifecycle_probe::scan_program(prog_root)?);
                // M1.3+M1.4: when --emit-spec-candidates is set, lift
                // findings into proto-clauses via the Pinocchio extractor,
                // then cluster them via the runtime-agnostic algorithm.
                // Other runtimes (Anchor, Native, Quasar) gain their own
                // extractors in M3/M4.
                let clusters = if emit_spec_candidates {
                    let protos = pinocchio_extractor::extract_proto_clauses(&findings);
                    Some(cluster::cluster_protos(protos))
                } else {
                    None
                };
                // M1.5+M1.7+M1.8: when --audit-dir is set, materialize
                // the full audit working set: interview.md (user prompts),
                // clusters.json (full envelope for `qedgen ratify`),
                // skeleton.qedspec (pre-interview structural skeleton).
                if let (Some(dir), Some(clusters_ref)) = (audit_dir.as_ref(), clusters.as_ref()) {
                    let program_name = prog_root
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("program")
                        .to_string();
                    let now_iso = time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Iso8601::DEFAULT)
                        .unwrap_or_else(|_| "unknown".to_string());
                    std::fs::create_dir_all(dir)?;
                    // 1. interview.md
                    let md = prompts::render_interview(clusters_ref, &program_name, &now_iso);
                    std::fs::write(dir.join("interview.md"), md)?;
                    // 2. clusters.json — full envelope; ratify consumes
                    // this to look up cluster_id → suggested_syntax.
                    let clusters_json = serde_json::to_string_pretty(clusters_ref)?;
                    std::fs::write(dir.join("clusters.json"), clusters_json)?;
                    // 3. skeleton.qedspec — pre-interview structural
                    // skeleton (handler stubs only).
                    let skeleton = pinocchio_to_spec::render_skeleton(prog_root, &program_name)?;
                    std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
                    eprintln!("Wrote audit working set to {}", dir.display());
                }
                let output = probe::ProbeOutput {
                    version: probe::schema_version(),
                    mode: probe::Mode::SpecLess,
                    spec_path: None,
                    project_root: Some(prog_root.display().to_string()),
                    runtime: Some(probe::Runtime::Pinocchio),
                    handlers: None,
                    applicable_categories: Some(probe::applicable_categories_public(
                        &probe::Runtime::Pinocchio,
                    )),
                    findings,
                    clusters,
                    dispatcher_kind: None,
                };
                // Top-level envelope: include the raw catalogue so the
                // subagent has both `findings[]` (per-site mapped) and
                // the full site list (other kinds the agent may want
                // to cross-reference).
                let mut value = serde_json::to_value(&output)?;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "pinocchio_catalogue".to_string(),
                        serde_json::to_value(&catalogue)?,
                    );
                }
                println!("{}", serde_json::to_string_pretty(&value)?);
                return Ok(());
            }

            // v2.18: --fuzz drives the Crucible engine. Different engine
            // from the pattern-match predicates that run when --fuzz is
            // absent — both produce Findings into the same surface, so
            // a user wanting both should run probe twice (once with,
            // once without) and merge JSON. v2.18.1 may merge them in a
            // single invocation if real eval data warrants it.
            //
            // v2.21 Slice 1: --fuzz now accepts EITHER --spec (existing
            // spec-driven path) OR --root (brownfield protocol-mode).
            // The two modes share the build → smoke → run → triage
            // pipeline in `crucible_probe::run_fuzz_probe`; they differ
            // only in (a) which `.qedspec` is loaded (real vs.
            // synthesised) and (b) which invariant family
            // `crucible_gen::generate` emits. See PRD-v2.21 §"Slice 1".
            if let Some(budget_secs) = fuzz {
                let (
                    synthesised_spec,
                    synthesised_idl,
                    spec_path_for_ctx,
                    project_root_for_idl,
                    mode,
                ) = match (spec.clone(), root.clone()) {
                    (Some(spec_path), maybe_root) => {
                        let parsed = check::parse_spec_file(&spec_path)?;
                        let spec_parent = spec_path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        // --spec + --root layers spec invariants on
                        // top of protocol-mode crash detection.
                        let mode = if maybe_root.is_some() {
                            crucible_gen::InvariantMode::Both
                        } else {
                            crucible_gen::InvariantMode::Spec
                        };
                        (parsed, None, spec_path, spec_parent, mode)
                    }
                    (None, Some(root_path)) => {
                        let resolved = crucible_brownfield::resolve_program_root(&root_path)?;
                        let detected = probe::detect_runtime_public(&resolved);
                        let runtime_final = match runtime {
                            Some(RuntimeOverride::Anchor) => probe::Runtime::Anchor,
                            Some(RuntimeOverride::Quasar) => probe::Runtime::Quasar,
                            Some(RuntimeOverride::Pinocchio) => probe::Runtime::Pinocchio,
                            Some(RuntimeOverride::Native) => probe::Runtime::Native,
                            Some(RuntimeOverride::Sbpf) => probe::Runtime::Sbpf,
                            None => detected,
                        };
                        let synth = crucible_brownfield::synthesize_spec(&resolved, runtime_final)?;
                        (
                            synth.spec,
                            synth.idl_json,
                            resolved.clone(),
                            resolved,
                            crucible_gen::InvariantMode::Protocol,
                        )
                    }
                    (None, None) => {
                        return Err(anyhow::anyhow!(
                            "--fuzz requires either --spec <path> (spec-driven) \
                                 or --root <project-path> (brownfield protocol-mode). \
                                 See `qedgen probe --help` for details."
                        ));
                    }
                };
                // Use the same name normalization as crucible_gen so
                // the harness path the dispatcher computes here lines
                // up with the directory crucible_gen::generate actually
                // creates. Kebab-case Cargo names like `multi-delegator`
                // must become `multi_delegator`; otherwise the dispatcher
                // writes the IDL to a sibling directory of the real
                // harness.
                let prog = crucible_gen::spec_program_name(&synthesised_spec);
                let harness_parent = if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                    crucible_brownfield::brownfield_harness_parent(&project_root_for_idl)
                } else {
                    project_root_for_idl.join("fuzz")
                };
                let harness = harness_dir
                    .clone()
                    .unwrap_or_else(|| harness_parent.join(&prog));
                // Brownfield mode: emit the harness scaffold under
                // `.qed/fuzz/<prog>/` if it isn't there yet. Spec mode
                // expects the user has already run
                // `qedgen codegen --crucible`; we don't auto-regen.
                if matches!(mode, crucible_gen::InvariantMode::Protocol) && !harness.exists() {
                    std::fs::create_dir_all(&harness_parent)?;
                    crucible_gen::generate(&synthesised_spec, &harness_parent, mode)?;
                }
                // v2.22 Slice 3: Pinocchio brownfield ships its own
                // IDL — there's no `anchor build` step to feed
                // `discover_idl`. Write the synthesised JSON straight
                // into `<harness>/idls/<prog>.json` so the existing
                // emitter path picks it up unchanged. Overwrite on every
                // run so scanner improvements propagate without manual
                // cleanup.
                if let Some(idl_json) = synthesised_idl.as_deref() {
                    crucible_brownfield::write_synthesized_idl(&harness, &prog, idl_json)
                        .context("writing synthesised IDL")?;
                }
                // Budget-0 emit-and-exit: lets users preview the
                // harness without paying the Crucible build cost. The
                // existing spec-mode UX implicitly did smoke + a 0-len
                // full run; v2.21 short-circuits to skip both. Same
                // shape as a "dry-run" without adding a new flag.
                if budget_secs == 0 {
                    let output = probe::ProbeOutput {
                        version: 1,
                        mode: if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                            probe::Mode::SpecLess
                        } else {
                            probe::Mode::SpecAware
                        },
                        spec_path: spec.as_ref().map(|p| p.display().to_string()),
                        project_root: root.as_ref().map(|p| p.display().to_string()),
                        runtime: None,
                        handlers: None,
                        applicable_categories: None,
                        findings: Vec::new(),
                        clusters: None,
                        dispatcher_kind: None,
                    };
                    eprintln!(
                        "Budget = 0: harness ready at {}; skipping build + fuzz run.",
                        harness.display()
                    );
                    println!("{}", serde_json::to_string_pretty(&output)?);
                    return Ok(());
                }
                let mut ctx = crucible_probe::FuzzProbeContext::new(
                    &spec_path_for_ctx,
                    project_root_for_idl,
                    harness,
                );
                ctx.fuzz_budget = std::time::Duration::from_secs(budget_secs);
                if no_smoke {
                    ctx.smoke_budget = std::time::Duration::ZERO;
                }
                ctx.stateful = stateful;
                ctx.invariant_mode = mode;
                let findings = crucible_probe::run_fuzz_probe(&ctx)?;
                let output = probe::ProbeOutput {
                    version: 1,
                    mode: if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                        probe::Mode::SpecLess
                    } else {
                        probe::Mode::SpecAware
                    },
                    spec_path: spec.as_ref().map(|p| p.display().to_string()),
                    project_root: root.as_ref().map(|p| p.display().to_string()),
                    runtime: None,
                    handlers: None,
                    applicable_categories: None,
                    findings,
                    clusters: None,
                    dispatcher_kind: None,
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
                return Ok(());
            }

            let _ = (harness_dir, no_smoke, stateful);
            let output = if bootstrap {
                let root = root
                    .ok_or_else(|| anyhow::anyhow!("--bootstrap requires --root <project-path>"))?;
                probe::run_bootstrap(&root)?
            } else {
                let spec = spec.ok_or_else(|| {
                    anyhow::anyhow!("provide --spec <path> for spec-aware mode, or --bootstrap --root <path> for spec-less")
                })?;
                probe::run_probe(&spec)?
            };
            let rendered = serde_json::to_string_pretty(&output)?;
            println!("{}", rendered);
        }

        Commands::Ratify {
            audit_dir,
            out,
            scoping_out,
            findings_dir,
        } => {
            let opts = ratify::RatifyOpts {
                audit_dir,
                spec_out: out,
                scoping_out,
                findings_dir,
            };
            let report = ratify::run(&opts)?;
            eprintln!(
                "Ratification complete: {} accepted, {} narrowed, {} rejected, {} flagged-as-bug, {} deferred",
                report.accepted,
                report.narrowed,
                report.rejected,
                report.flagged_as_bug,
                report.deferred,
            );
            eprintln!("Wrote spec to {}", report.spec_path.display());
            if report.rejected > 0 {
                eprintln!("Wrote scoping notes to {}", report.scoping_path.display());
            }
            for p in &report.findings_paths {
                eprintln!("Wrote bug-flagged finding to {}", p.display());
            }
        }

        Commands::Spec { idl, output_dir } => {
            let stem = idl
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            std::fs::create_dir_all(&output_dir)?;
            let output_file = output_dir.join(format!("{}.qedspec", stem));
            idl2spec::generate_qedspec(&idl, &output_file)?;
        }

        Commands::Consolidate {
            input_dir,
            output_dir,
        } => {
            consolidate::consolidate_proofs(&input_dir, &output_dir)?;
        }

        Commands::Asm2Lean {
            input,
            output,
            namespace,
        } => {
            asm2lean::asm2lean(&input, &output, namespace.as_deref())?;
        }

        Commands::Setup { workspace, mathlib } => {
            deps::require_lean()?;
            validate::setup_workspace(workspace.as_deref(), mathlib).await?;
        }

        Commands::Init {
            name,
            spec,
            asm,
            mathlib,
            target,
            output_dir,
        } => {
            // Pinocchio reserves the CLI surface but is not yet
            // implemented. Anchor and Quasar branches are wired
            // end-to-end below.
            if matches!(target, Some(Target::Pinocchio)) {
                anyhow::bail!(
                    "`--target pinocchio` is not yet implemented. \
                     `--target anchor` and `--target quasar` are \
                     supported; omit `--target` to skip program \
                     scaffolding entirely."
                );
            }

            // Program scaffolding (codegen + kani harnesses + unit tests)
            // requires the original `.qedspec` — `init` writes a
            // separate `Spec.lean` skeleton, but the codegen path parses
            // the qedspec directly. Refuse cleanly when `--target` is
            // set without `--spec`.
            let scaffold_target = target;
            if scaffold_target.is_some() && spec.is_none() {
                anyhow::bail!(
                    "`--target` requires `--spec <path.qedspec>` — the \
                     program codegen runs against the spec directly."
                );
            }

            // .qed/ lives at the program root. If the user passed --spec, anchor
            // to the spec's parent directory (what they expect); otherwise fall
            // back to the output_dir's parent. See init::resolve_program_root.
            let cwd = std::env::current_dir()?;
            let program_root = init::resolve_program_root(spec.as_deref(), &output_dir, &cwd);
            // The spec pointer is stored relative to program_root so
            // `qedgen check` from anywhere under the project resolves it
            // via .qed/config.json → project_root / <spec>.
            let spec_rel = spec.as_ref().map(|p| {
                p.strip_prefix(&program_root)
                    .unwrap_or(p.as_path())
                    .to_string_lossy()
                    .to_string()
            });
            init::init_qed_dir(&program_root, &name, spec_rel.as_deref())?;

            init::init(
                &name,
                &output_dir,
                asm.as_deref(),
                mathlib,
                scaffold_target.is_some(),
            )?;

            if let (Some(target), Some(qedspec_path)) = (scaffold_target, spec.as_ref()) {
                let program_dir = program_root.join(format!("programs/{}", name));
                // v2.6: tests live INSIDE the program package so cargo-kani
                // and cargo-test can resolve the governing Cargo.toml via the
                // usual `tests/` convention. Previously at `tests/kani.rs` at
                // program_root, which had no Cargo.toml above it.
                let kani_path = program_dir.join("tests/kani.rs");

                // Generate the framework-flavored Rust program skeleton.
                codegen::generate(qedspec_path, &program_dir, target)?;

                // Kani harnesses are framework-neutral (no Anchor/Quasar
                // types — pure spec-derived state model).
                kani::generate(qedspec_path, &kani_path)?;

                // Unit tests are framework-neutral too — plain `cargo
                // test` over the spec-derived state struct.
                let test_path = program_dir.join("src/tests.rs");
                unit_test::generate(qedspec_path, &test_path)?;
            }
        }

        // ==================================================================
        // check — unified spec validation
        // ==================================================================
        Commands::Check {
            spec,
            proofs,
            coverage,
            explain,
            output,
            code,
            anchor_project,
            drift,
            update_hashes,
            deep,
            kani,
            asm,
            json,
            frozen,
            strict,
            no_cache,
            regen_drift,
            examples_root,
            write,
        } => {
            require_git_repo()?;
            let cwd = std::env::current_dir()?;

            if regen_drift {
                let examples_root = if examples_root.is_absolute() {
                    examples_root
                } else {
                    cwd.join(examples_root)
                };
                let mode = if write {
                    regen_drift::WriteMode::Write
                } else {
                    regen_drift::WriteMode::Check
                };
                let report = regen_drift::check_examples_with(&examples_root, mode)?;
                regen_drift::print_report(&report);
                // In Write mode, drift entries are expected (the writer
                // resolved them). Only error if anything's still unresolved:
                // missing manifests always, MissingGeneratedCounterpart
                // always (writer can't synthesize a file the regen
                // pipeline didn't produce), and Changed entries when
                // running in Check mode.
                let unresolved = !report.missing_manifests.is_empty()
                    || report.drift.iter().any(|d| match d.kind {
                        regen_drift::DriftKind::MissingGeneratedCounterpart => true,
                        regen_drift::DriftKind::Changed => {
                            !matches!(mode, regen_drift::WriteMode::Write)
                        }
                    });
                if unresolved {
                    std::process::exit(1);
                }
                return Ok(());
            }

            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            let spec_name = spec
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Spec".to_string());

            // v2.8 G2: --frozen elevates qed.lock drift to a hard error
            // (CI usage). Default Auto mode auto-writes the lock on drift,
            // which is the right behavior for local development.
            let lock_mode = if frozen {
                qed_lock::LockMode::Frozen
            } else {
                qed_lock::LockMode::Auto
            };

            // F7 fold-in: --no-cache forces a fresh github fetch for every
            // imported dep (skips the TTL window). Path sources unaffected.
            let cache_opts = import_resolver::CacheOpts {
                force_refresh: no_cache,
            };

            let mut has_issues = false;

            // v2.26 Slice 4c — `check --frozen` runs the upstream
            // binary-hash diff opportunistically. Mismatches surface as
            // P2 warnings (`has_issues` stays false; exit zero) so a
            // routine CI run that misses a redeploy still draws
            // attention without blocking. `--strict` escalates mismatch
            // to CRIT and gates exit, matching the verify behavior.
            //
            // Fetch errors (missing `solana` CLI, no network) never
            // gate either mode — they always surface as P2 so a sandbox
            // without the Solana toolchain doesn't false-positive CI.
            if frozen {
                let spec_dir = spec.parent().unwrap_or_else(|| Path::new("."));
                let pinned = qed_lock::read(spec_dir)
                    .ok()
                    .flatten()
                    .as_ref()
                    .map(upstream_check::lock_has_pinned_hash)
                    .unwrap_or(false);
                if pinned {
                    match upstream_check::check_lock(spec_dir, None, false) {
                        Ok(results) => {
                            let gate = if strict {
                                upstream_check::Gate::CheckFrozenStrict
                            } else {
                                upstream_check::Gate::CheckFrozen
                            };
                            let routed = upstream_check::route_findings(results, gate);
                            let blocking = upstream_check::print_routed_report(&routed);
                            if blocking {
                                has_issues = true;
                            }
                        }
                        Err(e) => {
                            // Couldn't open the lock to dispatch — surface
                            // a P2-equivalent note but never gate exit
                            // (parity with the verify path's Error
                            // routing). `--strict` users see the message;
                            // they decide whether to investigate.
                            eprintln!("note: --frozen upstream check skipped: {}", e);
                        }
                    }
                }

                // v2.27 Track D1 — proof_hash drift routing. Sibling to
                // the binary_hash dispatch above; parses the spec under
                // Frozen mode so qed_lock::handle_lock populates
                // `ParsedSpec.proof_hash_findings` with any Stance-2
                // proof package whose content drifted from the on-disk
                // lock. P2 under plain `--frozen` (warn, exit 0), CRIT
                // under `--frozen --strict`. Parse errors here surface
                // identically to the main parse below (they would have
                // fired there too); we let the downstream parse re-raise
                // them rather than double-reporting.
                if let Ok(parsed) = check::parse_spec_file_with_opts(&spec, lock_mode, cache_opts) {
                    if !parsed.proof_hash_findings.is_empty() {
                        let gate = if strict {
                            upstream_check::Gate::CheckFrozenStrict
                        } else {
                            upstream_check::Gate::CheckFrozen
                        };
                        let routed = upstream_check::route_findings(
                            parsed.proof_hash_findings.clone(),
                            gate,
                        );
                        let blocking = upstream_check::print_routed_report(&routed);
                        if blocking {
                            has_issues = true;
                        }
                    }
                }
            }

            // sBPF verification (--asm)
            if let Some(ref asm_path) = asm {
                sbpf_verify::verify(asm_path, &proofs)?;
            }

            // Drift detection (--drift)
            if let Some(ref drift_path) = drift {
                if update_hashes {
                    let count = drift::update(drift_path)?;
                    eprintln!("Updated {} hash(es).", count);
                } else {
                    let entries = drift::check(drift_path)?;
                    drift::print_report(&entries);
                    if entries
                        .iter()
                        .any(|e| !matches!(e.status, drift::DriftStatus::Ok))
                    {
                        has_issues = true;
                    }
                    if deep {
                        let deep_entries = drift::check_deep(drift_path)?;
                        drift::print_deep_report(&deep_entries);
                        if !deep_entries.is_empty() {
                            has_issues = true;
                        }
                    }
                }
            }

            // Unified code/kani drift (--code, --kani)
            if code.is_some() || kani.is_some() {
                let report =
                    check::check_unified(&spec, &proofs, code.as_deref(), kani.as_deref())?;
                check::print_unified_report(&spec_name, &report);
                if report.issue_count() > 0 {
                    has_issues = true;
                }
            }

            // Anchor cross-check (--anchor-project) — verify that the spec's
            // handler list matches the user's existing Anchor program. M5
            // catches stale specs and uncovered handlers as a CI gate.
            if let Some(ref project_path) = anchor_project {
                let parsed = check::parse_spec_file(&spec)?;
                let findings = anchor_check::check_anchor_coverage(&parsed, project_path)?;
                let effect_findings = anchor_check::check_effect_coverage(&parsed, project_path)?;
                if json {
                    let payload = serde_json::json!({
                        "handler_coverage": findings
                            .iter()
                            .map(|f| serde_json::json!({
                                "kind": format!("{:?}", f.kind),
                                "handler": f.handler_name,
                                "message": f.message(),
                            }))
                            .collect::<Vec<_>>(),
                        "effect_coverage": effect_findings
                            .iter()
                            .map(|f| serde_json::json!({
                                "handler": f.handler,
                                "field": f.field,
                                "message": f.message(),
                            }))
                            .collect::<Vec<_>>(),
                    });
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                } else {
                    if findings.is_empty() {
                        eprintln!(
                            "Anchor cross-check (`{}`) — spec and program handler sets agree.",
                            project_path.display()
                        );
                    } else {
                        eprintln!(
                            "Anchor cross-check (`{}`) — {} handler-set disagreement(s):",
                            project_path.display(),
                            findings.len()
                        );
                        for f in &findings {
                            eprintln!("  ! {}", f.message());
                        }
                    }
                    if effect_findings.is_empty() {
                        eprintln!(
                            "Effect coverage — every spec effect has a matching mutation in the Rust body."
                        );
                    } else {
                        eprintln!(
                            "Effect coverage — {} unimplemented effect(s):",
                            effect_findings.len()
                        );
                        for f in &effect_findings {
                            eprintln!("  ! {}", f.message());
                        }
                    }
                }
                if !findings.is_empty() || !effect_findings.is_empty() {
                    has_issues = true;
                }
            }

            // Explain report (--explain) — inline markdown generation
            if explain {
                let results = check::check(&spec, &proofs)?;
                let proven = results
                    .iter()
                    .filter(|r| r.status == check::Status::Proven)
                    .count();
                let sorry = results
                    .iter()
                    .filter(|r| r.status == check::Status::Sorry)
                    .count();
                let missing = results
                    .iter()
                    .filter(|r| r.status == check::Status::Missing)
                    .count();
                let total = results.len();

                let mut md = format!("# {} Verification Report\n\n", spec_name);
                md.push_str(&format!(
                    "**{}/{} properties verified** ({} sorry, {} missing)\n\n",
                    proven, total, sorry, missing
                ));
                if proven == total {
                    md.push_str("> All properties verified (sorry-free).\n\n");
                }
                md.push_str("## Properties\n\n");
                for r in &results {
                    let (icon, label) = match r.status {
                        check::Status::Proven => ("✓", "PROVEN"),
                        check::Status::Sorry => ("✗", "SORRY"),
                        check::Status::Missing => ("✗", "MISSING"),
                    };
                    md.push_str(&format!("### {} {} — {}\n\n", icon, r.name, label));
                    if let Some(ref intent) = r.intent {
                        md.push_str(&format!("**Intent:** {}\n\n", intent));
                    }
                    if r.status != check::Status::Proven {
                        if let Some(ref suggestion) = r.suggestion {
                            md.push_str(&format!("**Suggestion:** {}\n\n", suggestion));
                        }
                    }
                }

                if let Some(ref path) = output {
                    std::fs::write(path, &md)?;
                    eprintln!("Wrote verification report to {}", path.display());
                } else {
                    print!("{}", md);
                }
            }

            // Coverage matrix (--coverage)
            if coverage {
                let parsed = check::parse_spec_file_with_opts(&spec, lock_mode, cache_opts)?;
                let matrix = check::coverage_matrix(&parsed);
                if json {
                    println!("{}", serde_json::to_string_pretty(&matrix)?);
                } else {
                    check::print_coverage_table(&matrix);
                }
            }

            // Orphan / missing preservation theorems in Proofs.lean. This
            // runs whenever the proofs dir exists and is a no-op on specs
            // without preservation obligations.
            if proofs.exists() {
                let parsed = check::parse_spec_file_with_opts(&spec, lock_mode, cache_opts)?;
                let findings = proofs_bootstrap::check_orphans(&parsed, &proofs)?;
                if !findings.is_empty() {
                    if json {
                        let as_json: Vec<serde_json::Value> = findings
                            .iter()
                            .map(|f| match f {
                                proofs_bootstrap::OrphanFinding::Orphan(n) => {
                                    serde_json::json!({"kind": "orphan", "theorem": n})
                                }
                                proofs_bootstrap::OrphanFinding::Missing(n) => {
                                    serde_json::json!({"kind": "missing", "theorem": n})
                                }
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&as_json)?);
                    } else {
                        eprintln!("Proofs.lean drift:");
                        for f in &findings {
                            eprintln!("  {}", f);
                        }
                    }
                    has_issues = true;
                }
            }

            // Lint — always runs (core of spec validation)
            {
                let mut warnings = check::lint_with_opts(&spec, lock_mode, cache_opts)?;
                // Code-aware lints (residual `todo!()` placeholders in
                // user-owned handler bodies) only fire when --code is set.
                // Merge them in here so JSON consumers see one combined list.
                if let Some(ref code_dir) = code {
                    let parsed = check::parse_spec_file_with_opts(&spec, lock_mode, cache_opts)?;
                    warnings.extend(check::check_handler_todos(&parsed, code_dir)?);
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&warnings)?);
                } else if warnings.is_empty() {
                    eprintln!("Spec is complete — no issues found.");
                } else {
                    let warns = warnings
                        .iter()
                        .filter(|w| w.severity == check::Severity::Warning)
                        .count();
                    let infos = warnings
                        .iter()
                        .filter(|w| w.severity == check::Severity::Info)
                        .count();
                    for w in &warnings {
                        eprintln!("{}\n", format_lint_warning(w));
                    }
                    eprintln!("{} warning(s), {} info", warns, infos);
                    if warns > 0 {
                        has_issues = true;
                    }
                }
            }

            if has_issues {
                std::process::exit(1);
            }
        }

        // ==================================================================
        // verify — run generated harnesses against generated code
        // ==================================================================
        Commands::Verify {
            spec,
            proptest,
            proptest_path,
            kani,
            kani_path,
            lean,
            lean_dir,
            miri,
            fail_fast,
            json,
            check_upstream,
            rpc_url,
            offline,
            upstream_stale_ok,
            probe_repros,
            crucible,
            crucible_harness_dir,
            crucible_no_smoke,
            crucible_stateful,
            require_verified,
            recursive,
        } => {
            require_git_repo()?;

            // Resolve --spec the same way `check` and `codegen` do: fall
            // back to .qed/config.json's `spec` field when omitted, so the
            // README's quick-start `qedgen verify` (no flags after init)
            // works as documented.
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;

            // v2.27 Track D2 / D3 — parse the spec once if either flag
            // needs the ParsedSpec (verified_callees / verified_proof_pkgs).
            // Both gates are pre-checks that may exit before backends
            // dispatch; bundling the parse avoids paying for it twice.
            let parsed_for_gates = if require_verified || recursive {
                Some(check::parse_spec_file(&spec)?)
            } else {
                None
            };

            // v2.27 Track D2 — short-circuit on unverified imports
            // before any backend dispatches. Rationale: if the dep
            // graph isn't fully Stance-2 proven, running proptest /
            // Kani / Lean against it still produces results that
            // depend on Stance-1 axiom discharge. Failing fast lets CI
            // gate on "all imports verified" cleanly without
            // surrounding the verify call with shell glue.
            if require_verified {
                let parsed = parsed_for_gates.as_ref().expect("parsed under gate guard");
                let findings = check::collect_require_verified_findings(parsed);
                if !findings.is_empty() {
                    eprintln!(
                        "--require-verified: {} unverified import(s) — every imported interface \
                         with `ensures` clauses must ship a Lake-buildable proof package.",
                        findings.len(),
                    );
                    for f in &findings {
                        eprintln!("  [CRIT] {}: unverified callee", f.interface_name);
                        eprintln!("         {}", f.fix_hint);
                    }
                    std::process::exit(1);
                }
            }

            // v2.27 Track D3 — `--recursive` walks the transitive
            // resolution closure (cycle-detected by the resolver) and
            // runs `lake build` against every imported proof package.
            // Per-layer pass/fail is reported up-front so a downstream
            // backend failure doesn't mask a transitive proof failure.
            // Each layer's failure is independent: keep walking so
            // operators see every breakage, then aggregate exit at the
            // end.
            //
            // Empty `verified_proof_pkgs` is a no-op success — this
            // spec doesn't import any verified providers, so there's
            // nothing transitive to build.
            if recursive {
                let parsed = parsed_for_gates.as_ref().expect("parsed under gate guard");
                if parsed.verified_proof_pkgs.is_empty() {
                    eprintln!(
                        "--recursive: no imported proof packages in this spec's dep graph; \
                         nothing to walk."
                    );
                } else {
                    eprintln!(
                        "--recursive: walking {} verified provider proof package(s) bottom-up.",
                        parsed.verified_proof_pkgs.len(),
                    );
                    let mut any_failed = false;
                    for (idx, pkg_root) in parsed.verified_proof_pkgs.iter().enumerate() {
                        eprintln!(
                            "  [{}/{}] lake build — {}",
                            idx + 1,
                            parsed.verified_proof_pkgs.len(),
                            pkg_root.display(),
                        );
                        match std::process::Command::new("lake")
                            .arg("build")
                            .current_dir(pkg_root)
                            .output()
                        {
                            Ok(out) if out.status.success() => {
                                eprintln!("       PASS");
                            }
                            Ok(out) => {
                                any_failed = true;
                                let stderr = String::from_utf8_lossy(&out.stderr);
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                eprintln!("       FAIL");
                                // Show the first ~10 lines of each
                                // stream — `lake build` output gets
                                // very long; the head is usually
                                // enough to identify the failure.
                                for line in stderr.lines().take(10) {
                                    eprintln!("         | {}", line);
                                }
                                for line in stdout.lines().take(10) {
                                    eprintln!("         | {}", line);
                                }
                            }
                            Err(e) => {
                                any_failed = true;
                                eprintln!("       ERROR: failed to spawn `lake build`: {}", e);
                            }
                        }
                    }
                    if any_failed {
                        eprintln!(
                            "--recursive: at least one provider's Lake build failed; the dep \
                             graph is NOT fully proven. Fix the provider(s) above before \
                             trusting this consumer's Stance-2 axioms."
                        );
                        std::process::exit(1);
                    }
                    eprintln!("--recursive: every imported proof package built clean.");
                }
            }

            // v2.8 G5 / v2.26 Slice 4c: --check-upstream diffs each
            // imported library's pinned binary hash against the on-chain
            // `.so` via `solana program dump`. Runs independently so
            // users can `--check-upstream` without re-running the harnesses.
            // F6 fold-in: --offline refuses any network fetch.
            //
            // v2.26 Slice 4c — auto-on when `qed.lock` declares any
            // pinned `upstream_binary_hash`. Explicit `--check-upstream`
            // still works (and is the right flag in scripts / CI), but
            // skipping it no longer silently bypasses the gate. Pair
            // with `--upstream-stale-ok` to suppress the check for
            // offline dev runs.
            let spec_dir = spec.parent().unwrap_or_else(|| Path::new("."));
            let run_upstream = if upstream_stale_ok {
                // Honor the suppression flag even when --check-upstream
                // is explicit — `upstream-stale-ok` is the local-dev
                // escape hatch, not a "render warnings anyway" knob.
                false
            } else if check_upstream {
                true
            } else {
                // Auto-on detection: only when a qed.lock exists and
                // at least one entry has a populated binary_hash pin.
                qed_lock::read(spec_dir)
                    .ok()
                    .flatten()
                    .as_ref()
                    .map(upstream_check::lock_has_pinned_hash)
                    .unwrap_or(false)
            };
            if run_upstream {
                let results = upstream_check::check_lock(spec_dir, rpc_url.as_deref(), offline)?;
                let gate = upstream_check::Gate::Verify;
                let routed = upstream_check::route_findings(results, gate);
                let blocking = upstream_check::print_routed_report(&routed);
                if blocking {
                    std::process::exit(1);
                }
                // When --check-upstream is the only verb, exit cleanly
                // without firing the backend runners. Combine with
                // --proptest etc. to do both in one invocation.
                let any_backend_flag = proptest || kani || lean || miri || probe_repros;
                if check_upstream && !any_backend_flag {
                    return Ok(());
                }
            } else if check_upstream && upstream_stale_ok {
                // The combination is explicitly allowed — emit a single
                // breadcrumb so the operator knows the gate was honored
                // but the suppression flag won.
                eprintln!(
                    "note: --upstream-stale-ok suppressed --check-upstream (offline-dev mode)"
                );
                let any_backend_flag = proptest || kani || lean || miri || probe_repros;
                if !any_backend_flag {
                    return Ok(());
                }
            }

            // PLAN-v2.16 D4: --probe-repros runs the per-probe Mollusk
            // reproducers under `target/qedgen-repros/`. Like
            // --check-upstream, it's a separate verification stage with
            // its own report shape — not folded into the backend
            // BackendReport rollup. Runs before the proptest/kani/lean
            // backends so the auditor has the gating data first.
            if probe_repros {
                let project_root = spec.parent().map(Path::to_path_buf).unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                let report = verify_probe_repros::run(&project_root)?;
                if json {
                    verify_probe_repros::print_json(&report)?;
                } else {
                    verify_probe_repros::print_human(&report);
                }
                if !report.all_fired_or_inconclusive() {
                    std::process::exit(1);
                }
                let any_backend_flag = proptest || kani || lean || miri;
                if !any_backend_flag {
                    return Ok(());
                }
            }

            // No explicit backend flags -> run every backend whose artifact
            // is present on disk. This matches the agent-friendly "just do
            // the right thing" default from the PRD.
            let any_flag = proptest || kani || lean || miri;
            // Project root used by Miri repro discovery — spec parent dir.
            let project_root = spec
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let miri_default = !project_root
                .join(".qed")
                .join("probes")
                .join("pinocchio")
                .read_dir()
                .map(|mut it| it.next().is_none())
                .unwrap_or(true);
            let opts = if any_flag {
                verify::VerifyOpts {
                    spec: spec.clone(),
                    proptest,
                    proptest_path,
                    kani,
                    kani_path,
                    lean,
                    lean_dir,
                    miri,
                    fail_fast,
                    project_root: project_root.clone(),
                }
            } else {
                verify::VerifyOpts {
                    spec: spec.clone(),
                    proptest: proptest_path.exists(),
                    proptest_path,
                    kani: kani_path.exists(),
                    kani_path,
                    lean: lean_dir.join("lakefile.lean").exists()
                        || lean_dir.join("lakefile.toml").exists(),
                    lean_dir,
                    miri: miri_default,
                    fail_fast,
                    project_root: project_root.clone(),
                }
            };

            let mut report = verify::run(&opts)?;

            // v2.18 P3: --crucible is a thin alias over the probe engine.
            // Findings come back as a Vec<Finding>; we wrap them as a
            // single BackendReport so they render through the v2.17
            // format_human named-trace surface alongside Kani/proptest.
            if let Some(budget_secs) = crucible {
                let backend = crucible_backend_report(
                    &spec,
                    crucible_harness_dir.clone(),
                    budget_secs,
                    crucible_no_smoke,
                    crucible_stateful,
                );
                report.backends.push(backend);
            }
            let _ = (crucible_harness_dir, crucible_no_smoke, crucible_stateful);

            if json {
                verify::print_json(&report)?;
            } else {
                verify::print_human(&report);
            }

            if !report.ok() {
                std::process::exit(1);
            }
        }

        // ==================================================================
        // readiness — preflight lint for first-deploy mainnet-readiness
        // ==================================================================
        //
        // Exit-code discipline matches ratchet's CLI: rule-engine findings
        // map to 1/2 via `ratchet::exit_code`, but caller-side failures
        // (missing IDL, unparseable JSON) exit 3 so CI scripts can
        // distinguish "your program has a breaking change" from "your
        // pipeline is misconfigured."
        Commands::Readiness {
            idl,
            list_rules,
            quasar,
            json,
        } => {
            if list_rules {
                ratchet::print_rules_preflight(json)?;
                return Ok(());
            }
            // clap's `required_unless_present = "list_rules"` guarantees
            // `idl` is Some here — unwrap is safe in shape.
            let idl = idl.expect("--idl is required unless --list-rules");
            let framework = resolve_framework(quasar, json);
            let report = match ratchet::run_readiness(&ratchet::ReadinessOpts { idl, framework }) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {:#}", e);
                    std::process::exit(3);
                }
            };
            if json {
                ratchet::print_json(&report)?;
            } else {
                ratchet::print_human(&report);
            }
            let code = ratchet::exit_code(&report);
            if code != 0 {
                std::process::exit(code);
            }
        }

        // ==================================================================
        // check-upgrade — diff two IDLs under ratchet's R-rules
        // ==================================================================
        Commands::CheckUpgrade {
            old,
            new,
            unsafes,
            migrated_accounts,
            realloc_accounts,
            list_rules,
            quasar,
            json,
        } => {
            if list_rules {
                ratchet::print_rules_diff(json)?;
                return Ok(());
            }
            let old = old.expect("--old is required unless --list-rules");
            let new = new.expect("--new is required unless --list-rules");
            let framework = resolve_framework(quasar, json);
            let report = match ratchet::run_check_upgrade(&ratchet::CheckUpgradeOpts {
                old,
                new,
                unsafes,
                migrated_accounts,
                realloc_accounts,
                framework,
            }) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {:#}", e);
                    std::process::exit(3);
                }
            };
            if json {
                ratchet::print_json(&report)?;
            } else {
                ratchet::print_human(&report);
            }
            let code = ratchet::exit_code(&report);
            if code != 0 {
                std::process::exit(code);
            }
        }

        // ==================================================================
        // codegen — generate committed artifacts
        // ==================================================================
        Commands::Codegen {
            spec,
            target,
            output_dir,
            kani,
            kani_output,
            kani_impl,
            kani_impl_output,
            test,
            test_output,
            proptest,
            proptest_output,
            crucible,
            crucible_output,
            integration,
            integration_output,
            lean,
            lean_output,
            ci,
            ci_output,
            ci_asm,
            ci_ratchet,
            all,
            fill,
            handler,
            fill_tests,
        } => {
            require_git_repo()?;
            // v2.24.x: Pinocchio reserves the Rust-scaffold CLI surface
            // but has no scaffold implementation yet. Verification
            // backends (Kani / proptest / Lean / integration_test /
            // CI / crucible) are spec-driven and target-agnostic —
            // they reason about spec semantics, not the runtime
            // representation — so they run cleanly for Pinocchio
            // specs even without a Rust scaffold. Skip the
            // `codegen::generate` step on Pinocchio when the user
            // asked for at least one backend flag (or `--all`).
            // Bail loudly only when the user requested the scaffold
            // alone (no backend flags) and Pinocchio is the chosen
            // target — that's the unambiguous "I want a Pinocchio
            // Rust program" case the scaffold can't satisfy.
            let any_backend =
                kani || kani_impl || proptest || lean || test || integration || ci || crucible;
            let pinocchio_no_scaffold = matches!(target, Target::Pinocchio);
            if pinocchio_no_scaffold && !any_backend && !all {
                anyhow::bail!(
                    "`--target pinocchio` is not yet implemented for Rust scaffold codegen. \
                     `--target anchor` and `--target quasar` are supported for scaffold. \
                     To generate just the verification backends (Kani / proptest / Lean / etc.) \
                     for a Pinocchio spec, pass `--kani`, `--proptest`, `--lean`, or `--all` \
                     alongside `--target pinocchio` — the scaffold step will be skipped and \
                     the backends will run against the spec directly."
                );
            }
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            // Rust skeleton — Anchor and Quasar emit; Pinocchio skips
            // (handled above + here so `--all` on Pinocchio still
            // works for the backend artifacts).
            if !pinocchio_no_scaffold {
                codegen::generate(&spec, &output_dir, target)?;
            }

            if kani || all {
                // Codegen is pure text generation; missing cargo-kani only
                // matters when the harness is actually executed. The hard gate
                // lives in `qedgen verify --kani` (see verify.rs). Warn here
                // so the install hint surfaces, but don't block codegen.
                if let Err(e) = deps::require_kani() {
                    eprintln!("warning: {e}");
                }
                kani::generate(&spec, &kani_output)?;
            }

            // v2.26 Batch 2 Track H — impl-targeted Kani harness. Emits
            // when:
            //   1. `--kani-impl` was passed explicitly, OR
            //   2. `--all` was passed AND at least one handler auto-triggers
            //      (modifies ⊋ effect.lhs — the LP-shape signal), OR
            //   3. `--kani` was passed AND at least one handler auto-triggers
            //      (so users on `--kani` get the impl-side coverage when
            //      their spec declares modifies-driven fill sites).
            //
            // `kani_impl::spec_triggers_impl_harness` is the auto-trigger
            // predicate. Per-handler heuristic lives in one place
            // (mirrors `codegen.rs` Phase A's modifies-vs-effect diff).
            let auto_impl_trigger = {
                let parsed = check::parse_spec_file(&spec)?;
                kani_impl::spec_triggers_impl_harness(&parsed)
            };
            let want_kani_impl = kani_impl || ((kani || all) && auto_impl_trigger);
            if want_kani_impl {
                if let Err(e) = deps::require_kani() {
                    eprintln!("warning: {e}");
                }
                kani_impl::generate(&spec, &kani_impl_output, /*explicit_flag=*/ kani_impl)?;
            }

            if test || all {
                unit_test::generate(&spec, &test_output)?;
            }
            if proptest || all {
                proptest_gen::generate(&spec, &proptest_output)?;
            }
            if crucible || all {
                let parsed = check::parse_spec_file(&spec)?;
                crucible_gen::generate(
                    &parsed,
                    &crucible_output,
                    crucible_gen::InvariantMode::Spec,
                )?;
            }
            if integration || all {
                integration_test::generate(&spec, &integration_output)?;
            }
            if lean || all {
                // Same rationale as the Kani branch: `lean_gen::generate` and
                // `proofs_bootstrap::bootstrap_if_missing` are pure text
                // writers. `lake` is only required to *build* the generated
                // proofs, which `qedgen verify --lean` (and Aristotle) gate
                // separately. Warn here without blocking codegen.
                if let Err(e) = deps::require_lean() {
                    eprintln!("warning: {e}");
                }
                let parsed = check::parse_spec_file(&spec)?;
                lean_gen::generate(&parsed, &lean_output)?;
                // Bootstrap Proofs.lean alongside Spec.lean. Never overwrites
                // an existing file — the user-owned theorems survive regen.
                if let Some(proofs_dir) = lean_output.parent() {
                    proofs_bootstrap::bootstrap_if_missing(&parsed, proofs_dir)?;
                }
            }
            if ci || all {
                const CI_TEMPLATE: &str = include_str!("../../../templates/verify.yml");
                let verify_step = if let Some(ref asm) = ci_asm {
                    format!("\n      - name: Verify sBPF binary\n        run: qedgen check --spec program.qedspec --asm {}\n", asm)
                } else {
                    String::new()
                };
                let ratchet_step = if let Some(ref idl) = ci_ratchet {
                    format!(
                        "\n      - name: Ratchet readiness lint\n        run: qedgen readiness --idl {}\n",
                        idl
                    )
                } else {
                    String::new()
                };
                let workflow = expand_ci_template(CI_TEMPLATE, &verify_step, &ratchet_step);
                if let Some(parent) = ci_output.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&ci_output, workflow)?;
                eprintln!("Generated CI workflow: {}", ci_output.display());
            }

            if fill {
                eprintln!("warning: `qedgen codegen --fill` is deprecated.");
                eprintln!("         The agent can fill `todo!()` sites directly via Read / Edit.");
                eprintln!("         Pattern: grep for `todo!()` in programs/, read the spec's");
                eprintln!("         handler/accounts blocks, edit each body in place. The");
                eprintln!("         prompt-emission layer is redundant with the agent's own");
                eprintln!("         file tools. Slated for hard-removal in v3.0; flag remains");
                eprintln!("         functional for now to avoid breaking existing scripts.");
                let parsed = check::parse_spec_file(&spec)?;
                let opts = fill::FillOpts {
                    spec: &parsed,
                    spec_path: &spec,
                    programs_dir: &output_dir,
                    only_handler: handler.as_deref(),
                };
                fill::emit_prompts(&opts)?;
            }

            if fill_tests {
                eprintln!("warning: `qedgen codegen --fill-tests` is deprecated.");
                eprintln!("         The agent can fill integration-test `todo!()` sites directly.");
                eprintln!("         Slated for hard-removal in v3.0; flag remains functional.");
                let parsed = check::parse_spec_file(&spec)?;
                let opts = fill::FillTestsOpts {
                    spec: &parsed,
                    spec_path: &spec,
                    tests_path: &integration_output,
                };
                fill::emit_test_prompts(&opts)?;
            }
        }

        Commands::Aristotle(cmd) => match cmd {
            AristotleCommands::Submit {
                project_dir,
                prompt,
                output_dir,
                wait,
                poll_interval,
            } => {
                deps::require_lean()?;
                if let Some(interval) = poll_interval {
                    ensure!(interval >= 5, "poll_interval must be at least 5 seconds");
                    ensure!(
                        interval <= 3600,
                        "poll_interval must be at most 3600 seconds"
                    );
                }
                let prompt = prompt.unwrap_or_else(|| {
                    "Fill in all sorry placeholders with valid proofs".to_string()
                });
                let output = output_dir.unwrap_or_else(|| project_dir.clone());
                aristotle::fill_sorry(&project_dir, &output, &prompt, wait, poll_interval).await?;
            }

            AristotleCommands::Status {
                project_id,
                wait,
                poll_interval,
                output_dir,
            } => {
                if let Some(interval) = poll_interval {
                    ensure!(interval >= 5, "poll_interval must be at least 5 seconds");
                    ensure!(
                        interval <= 3600,
                        "poll_interval must be at most 3600 seconds"
                    );
                }
                let project = aristotle::status(&project_id).await?;
                println!("Project:  {}", project.project_id);
                println!("Status:   {}", project.status);
                println!("Progress: {}%", project.percent_complete.unwrap_or(0));
                println!("Created:  {}", project.created_at);
                println!("Updated:  {}", project.last_updated_at);
                if let Some(summary) = &project.output_summary {
                    println!("Summary:  {}", summary);
                }

                if wait {
                    match project.status.as_str() {
                        "QUEUED" | "IN_PROGRESS" | "NOT_STARTED" => {
                            eprintln!("\nPolling until completion...");
                            let final_project = aristotle::poll(&project_id, poll_interval).await?;
                            match final_project.status.as_str() {
                                "COMPLETE" | "COMPLETE_WITH_ERRORS" => {
                                    if final_project.status == "COMPLETE_WITH_ERRORS" {
                                        eprintln!("Warning: Aristotle completed with some errors.");
                                    }
                                    aristotle::download_result(
                                        &final_project.project_id,
                                        &output_dir,
                                    )
                                    .await?;
                                    if let Some(summary) = &final_project.output_summary {
                                        eprintln!("\nSummary: {}", summary);
                                    }
                                }
                                status => {
                                    eprintln!("Project ended with status: {}", status);
                                    if let Some(summary) = &final_project.output_summary {
                                        eprintln!("Summary: {}", summary);
                                    }
                                }
                            }
                        }
                        _ => {
                            eprintln!("Project already in terminal state, nothing to poll.");
                        }
                    }
                }
            }

            AristotleCommands::Result {
                project_id,
                output_dir,
            } => {
                aristotle::download_result(&project_id, &output_dir).await?;
            }

            AristotleCommands::Cancel { project_id } => {
                let project = aristotle::cancel(&project_id).await?;
                eprintln!(
                    "Project {} cancelled (status: {})",
                    project.project_id, project.status
                );
            }

            AristotleCommands::List { limit, status } => {
                let projects = aristotle::list(limit, status.as_deref()).await?;
                if projects.is_empty() {
                    println!("No projects found.");
                } else {
                    println!("{:<38} {:<22} {:>5}  CREATED", "ID", "STATUS", "%");
                    for p in &projects {
                        println!(
                            "{:<38} {:<22} {:>4}%  {}",
                            p.project_id,
                            p.status,
                            p.percent_complete.unwrap_or(0),
                            p.created_at
                        );
                    }
                }
            }
        },

        // ==================================================================
        // reconcile — unified drift report (Rust handlers + Lean proofs)
        // ==================================================================
        Commands::Reconcile {
            spec,
            code,
            proofs,
            json,
        } => {
            require_git_repo()?;
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            let report = reconcile::reconcile(&spec, &code, &proofs)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                reconcile::print_report(&report);
            }
            if report.has_drift() {
                std::process::exit(1);
            }
        }

        // ==================================================================
        // feedback — bundle last-error context into a GitHub issue
        // ==================================================================
        Commands::Feedback {
            note,
            title,
            spec,
            dry_run,
            yes,
            no_open,
        } => {
            feedback::run(
                spec.as_deref(),
                note.as_deref(),
                title.as_deref(),
                dry_run,
                yes,
                no_open,
            )?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{expand_ci_template, format_lint_warning};
    use crate::check::{CompletenessWarning, Severity};

    #[test]
    fn plain_text_lint_output_includes_priority() {
        let warning = CompletenessWarning {
            rule: "missing_effect".to_string(),
            severity: Severity::Warning,
            priority: 2,
            message: "operation 'borrow' takes params and transitions state but has no effect"
                .to_string(),
            subject: Some("borrow".to_string()),
            fix: "Add an effect block to describe state changes".to_string(),
            example: Some(
                "  operation borrow\n    effect: loan_amount add loan_amount".to_string(),
            ),
            counterexample: None,
            fix_options: vec![],
        };

        let rendered = format_lint_warning(&warning);
        assert!(rendered.contains("[P2] [missing_effect]"));
        assert!(rendered.contains("Fix: Add an effect block to describe state changes"));
        assert!(rendered.contains("Example:"));
    }

    // The committed verify.yml template carries two extension placeholders
    // — {{VERIFY_STEP}} for the optional sBPF source-hash check and
    // {{RATCHET_STEP}} for the optional deploy-safety lint. A refactor
    // that silently drops or mangles either one would be invisible in the
    // rest of the test suite; these three snapshots catch that class of
    // regression cheaply.
    const CI_TEMPLATE: &str = include_str!("../../../templates/verify.yml");

    #[test]
    fn ci_template_unset_placeholders_produce_clean_workflow() {
        let out = expand_ci_template(CI_TEMPLATE, "", "");
        // Both placeholders fully consumed.
        assert!(!out.contains("{{VERIFY_STEP}}"));
        assert!(!out.contains("{{RATCHET_STEP}}"));
        // Neither optional step present when unset.
        assert!(!out.contains("Verify sBPF binary"));
        assert!(!out.contains("Ratchet readiness lint"));
        // Core workflow still intact.
        assert!(out.contains("Check spec coverage"));
        assert!(out.contains("Build proofs"));
        // Exactly one trailing newline — no blank line at EOF.
        assert!(out.ends_with('\n'));
        assert!(!out.ends_with("\n\n"));
    }

    #[test]
    fn ci_template_ratchet_step_injects_readiness_job() {
        let ratchet = "\n      - name: Ratchet readiness lint\n        run: qedgen readiness --idl target/idl/escrow.json\n";
        let out = expand_ci_template(CI_TEMPLATE, "", ratchet);
        assert!(out.contains("Ratchet readiness lint"));
        assert!(out.contains("qedgen readiness --idl target/idl/escrow.json"));
        assert!(!out.contains("{{RATCHET_STEP}}"));
        assert!(out.ends_with('\n'));
        assert!(!out.ends_with("\n\n"));
    }

    #[test]
    fn ci_template_both_steps_coexist_without_collision() {
        let verify = "\n      - name: Verify sBPF binary\n        run: qedgen check --spec program.qedspec --asm src/program.s\n";
        let ratchet = "\n      - name: Ratchet readiness lint\n        run: qedgen readiness --idl target/idl/x.json\n";
        let out = expand_ci_template(CI_TEMPLATE, verify, ratchet);
        assert!(out.contains("Verify sBPF binary"));
        assert!(out.contains("Ratchet readiness lint"));
        // sBPF step precedes proof build; ratchet step follows spec coverage.
        let verify_pos = out.find("Verify sBPF binary").unwrap();
        let proofs_pos = out.find("Build proofs").unwrap();
        let coverage_pos = out.find("Check spec coverage").unwrap();
        let ratchet_pos = out.find("Ratchet readiness lint").unwrap();
        assert!(verify_pos < proofs_pos);
        assert!(coverage_pos < ratchet_pos);
    }
}
