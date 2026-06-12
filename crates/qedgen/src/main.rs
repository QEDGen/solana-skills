mod adapt;
mod check;
mod codegen;
mod dispatch;
mod mir;
mod probe;
mod project;
mod spec;
mod verify;

// Root re-exports: the v2.35 src/ reorg moved flat modules into directory
// groups; these keep every pre-existing `crate::<module>` path valid.
#[cfg(test)]
pub(crate) use adapt::pinocchio_to_spec;
pub(crate) use adapt::{
    anchor_adapt, anchor_check, anchor_extractor, anchor_project, anchor_resolver,
    native_extractor, pinocchio_extractor, pinocchio_profile, program_model,
};
pub(crate) use codegen::{
    asm2lean, banner, codegen_mir, codegen_shared, crucible_gen, fingerprint, integration_test,
    interface_gen, kani_impl, kani_mir, lean_gen_mir, lean_sidecars, proptest_gen_mir,
    rust_codegen_util, unit_test,
};
pub(crate) use dispatch::{api, aristotle};
pub(crate) use mir::cpi_substitute;
pub(crate) use probe::{
    arithmetic_symbol_probe, cluster, crucible_brownfield, crucible_probe, handler_intent,
    lifecycle_probe, paired_validator_probe, pinocchio_probe, probe_repro, prompts, ratify,
    shank_probe,
};
pub(crate) use project::{
    consolidate, deps, feedback, fill, init, proofs_bootstrap, qed_lock, qed_manifest, reconcile,
};
pub(crate) use spec::{
    ast, chumsky_adapter, chumsky_parser, idl, idl2spec, import_resolver, quantifier, spec_hash,
    validate,
};
pub(crate) use verify::{
    drift, miri_verify, ratchet, regen_drift, sbpf_verify, upstream_check, verify_counterexample,
    verify_kani_parse, verify_probe_repros, verify_proptest_parse,
};

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
    /// Pinocchio (no_std) Rust program. `#![no_std]`,
    /// `entrypoint!` + byte-discriminant dispatch, `&AccountInfo`
    /// account structs with `.handler()` methods, `zeropod` zero-copy
    /// state, `Result<(), ProgramError>`. MIR-native codegen.
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

    /// Brownfield adapter for existing Solana programs. Two modes:
    ///
    /// `--program <c>` (scaffold): detects the framework — Anchor (an
    /// `anchor-lang` dep or a `#[program]` mod), else Pinocchio
    /// (`pub fn process_*`), else native (any `pub fn`) — walks the
    /// program's handlers, and emits a `.qedspec` skeleton with TODO
    /// markers for state machine / requires / effects. The Anchor path
    /// resolves each instruction to its handler body and round-trips
    /// through the parser.
    ///
    /// `--program <c> --spec <s>` (attribute, Anchor-only): given an
    /// existing spec, emits one `#[qed(verified, spec = ..., handler = ...,
    /// hash = ..., spec_hash = ...)]` line per handler. Paste each above
    /// its handler `pub fn`; future body edits fire `compile_error!`
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
/// Redirect a `…/tests/kani_impl.rs` path to a sibling `…/src/kani_impl.rs`.
/// Pinocchio Kani harnesses must live in the lib (`src/`) because
/// `cargo kani` only discovers `#[kani::proof]` there, not in `tests/`
/// (M1 smoke-test finding, design doc §11a). Paths whose parent is not
/// `tests` pass through unchanged so an explicit `--kani-impl-output`
/// override is respected.
fn redirect_kani_impl_to_src(path: &std::path::Path) -> PathBuf {
    let file = path
        .file_name()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("kani_impl.rs"));
    match path.parent() {
        Some(parent) if parent.file_name().and_then(|s| s.to_str()) == Some("tests") => {
            // …/tests/kani_impl.rs → …/src/kani_impl.rs
            parent.parent().unwrap_or(parent).join("src").join(&file)
        }
        _ => path.to_path_buf(),
    }
}

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
        // Inlined snake-case normalization (mirrors crucible_gen).
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
    // IDL-aware enumerator; empty on non-standard layouts (don't fail —
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
        let md = prompts::render_interview(clusters_ref, &program_name, &now_iso);
        std::fs::write(dir.join("interview.md"), md)?;
        let cj = serde_json::to_string_pretty(clusters_ref)?;
        std::fs::write(dir.join("clusters.json"), cj)?;
        // skeleton.qedspec — the Anchor-compatible structural adapter feeds
        // the pre-interview skeleton; on failure fall back to a minimal stub
        // ratify still accepts.
        let anchor_overrides = std::collections::HashMap::new();
        let adapter_config = adapt::AdapterConfig::new(&program_name, &anchor_overrides);
        let skeleton = match adapt::render_skeleton_for_framework(
            program_model::ProgramFramework::Anchor,
            prog_root,
            adapter_config,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "warning: program adapter failed ({}); writing minimal skeleton",
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
        // Native skeleton accepts any `pub fn` (no `process_*`-style naming
        // convention to key on).
        let anchor_overrides = std::collections::HashMap::new();
        let adapter_config = adapt::AdapterConfig::new(&program_name, &anchor_overrides);
        let skeleton = adapt::render_skeleton_for_framework(
            program_model::ProgramFramework::Native,
            prog_root,
            adapter_config,
        )?;
        std::fs::write(dir.join("skeleton.qedspec"), skeleton)?;
        eprintln!("Wrote audit working set to {}", dir.display());
    }

    // Shank central-match dispatcher detection — the richer envelope
    // (handlers + dispatcher_kind) is purely additive. Each handler body is
    // also classified to narrow `applicable_categories`.
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

    // Persist the failing command's stderr for the next `qedgen feedback`.
    // Skip when `feedback` itself failed — don't overwrite the error it
    // would have reported on.
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
                    let program_name = adapt::default_program_name(&program);
                    let adapter_config = adapt::AdapterConfig::new(&program_name, &overrides);
                    if let Some(path) = out {
                        adapt::render_skeleton_to_file(&program, &path, adapter_config)?;
                    } else {
                        let rendered = adapt::render_skeleton(&program, adapter_config)?;
                        print!("{}", rendered);
                    }
                }
            }
        }

        Commands::Interface { idl, out, vendor } => {
            if vendor {
                // `.qed/interfaces/<idl-stem>.qedspec`, resolved via the
                // nearest `.qed/` ancestor of cwd.
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
            // --program routes through the Pinocchio site enumerator; the
            // envelope's `findings` are the site catalogue mapped 1:1. The
            // audit subagent writes the reproducers.
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

                // Anchor/Quasar route through anchor_extractor for
                // scaffold-to-spec interviews; no per-site findings yet
                // (the auditor handles those via Read+Grep).
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

                // Native (solana-program, no framework) routes through
                // native_extractor; pattern coverage is narrower — see its
                // module docs.
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
                // Arithmetic-symbol catalog: runtime-agnostic source-scan
                // findings merge into the same envelope.
                findings.extend(arithmetic_symbol_probe::scan_program(prog_root)?);
                // Paired-validator asymmetry across files.
                findings.extend(paired_validator_probe::scan_program(prog_root)?);
                // Lifecycle catalog: pairs authority-conferring CPI grants
                // with close handlers that don't tear them down.
                findings.extend(lifecycle_probe::scan_program(prog_root)?);
                // --emit-spec-candidates: lift findings into proto-clauses,
                // then cluster.
                let clusters = if emit_spec_candidates {
                    let protos = pinocchio_extractor::extract_proto_clauses(&findings);
                    Some(cluster::cluster_protos(protos))
                } else {
                    None
                };
                // --audit-dir: materialize the audit working set
                // (interview.md, clusters.json, skeleton.qedspec).
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
                    let md = prompts::render_interview(clusters_ref, &program_name, &now_iso);
                    std::fs::write(dir.join("interview.md"), md)?;
                    // clusters.json — ratify looks up cluster_id → suggested_syntax here.
                    let clusters_json = serde_json::to_string_pretty(clusters_ref)?;
                    std::fs::write(dir.join("clusters.json"), clusters_json)?;
                    // skeleton.qedspec — handler stubs only.
                    let anchor_overrides = std::collections::HashMap::new();
                    let adapter_config =
                        adapt::AdapterConfig::new(&program_name, &anchor_overrides);
                    let skeleton = adapt::render_skeleton_for_framework(
                        program_model::ProgramFramework::Pinocchio,
                        prog_root,
                        adapter_config,
                    )?;
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
                // Include the raw catalogue so the subagent has both
                // `findings[]` and the full site list to cross-reference.
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

            // --fuzz drives the Crucible engine — separate from the
            // pattern-match predicates (run probe twice and merge JSON for
            // both). Accepts EITHER --spec (spec-driven) OR --root
            // (brownfield protocol-mode); both share the build → smoke →
            // run → triage pipeline and differ only in which `.qedspec` is
            // loaded and which invariant family is emitted.
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
                // Same name normalization as crucible_gen so the computed
                // harness path matches the directory generate creates —
                // kebab-case names must become snake_case or the IDL lands
                // in a sibling directory of the real harness.
                let prog = crucible_gen::spec_program_name(&synthesised_spec);
                let harness_parent = if matches!(mode, crucible_gen::InvariantMode::Protocol) {
                    crucible_brownfield::brownfield_harness_parent(&project_root_for_idl)
                } else {
                    project_root_for_idl.join("fuzz")
                };
                let harness = harness_dir
                    .clone()
                    .unwrap_or_else(|| harness_parent.join(&prog));
                // Brownfield: emit the harness under `.qed/fuzz/<prog>/` if
                // absent. Spec mode expects a prior `codegen --crucible`;
                // never auto-regen.
                if matches!(mode, crucible_gen::InvariantMode::Protocol) && !harness.exists() {
                    std::fs::create_dir_all(&harness_parent)?;
                    crucible_gen::generate(&synthesised_spec, &harness_parent, mode)?;
                }
                // Brownfield ships its own IDL (no `anchor build` to feed
                // `discover_idl`): write the synthesised JSON to
                // `<harness>/idls/<prog>.json`, overwriting each run so
                // scanner improvements propagate.
                if let Some(idl_json) = synthesised_idl.as_deref() {
                    crucible_brownfield::write_synthesized_idl(&harness, &prog, idl_json)
                        .context("writing synthesised IDL")?;
                }
                // Budget 0 = emit the harness and exit (dry-run preview
                // without the Crucible build cost).
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
            // Program scaffolding parses the `.qedspec` directly (init's
            // Spec.lean skeleton isn't enough) — refuse `--target` without
            // `--spec`.
            let scaffold_target = target;
            if scaffold_target.is_some() && spec.is_none() {
                anyhow::bail!(
                    "`--target` requires `--spec <path.qedspec>` — the \
                     program codegen runs against the spec directly."
                );
            }

            // .qed/ lives at the program root — see init::resolve_program_root.
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
                // Tests live INSIDE the program package so cargo-kani /
                // cargo-test resolve the governing Cargo.toml.
                let kani_path = program_dir.join("tests/kani.rs");

                // Rust skeleton via the MIR path (codegen_mir) — same as the
                // `codegen` command.
                let parsed = check::parse_spec_file(qedspec_path)?;
                let mir = mir::lower(&parsed);
                codegen_mir::generate(&mir, &parsed, qedspec_path, &program_dir, target)?;

                // Kani harnesses are framework-neutral (pure spec-derived
                // state model).
                kani_mir::generate(&mir, &parsed, &kani_path)?;

                // Unit tests are framework-neutral too.
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

            // --frozen elevates qed.lock drift to a hard error (CI); Auto
            // writes the lock on drift (right for local dev).
            let lock_mode = if frozen {
                qed_lock::LockMode::Frozen
            } else {
                qed_lock::LockMode::Auto
            };

            // --no-cache forces a fresh github fetch for every imported dep
            // (skips the TTL window). Path sources unaffected.
            let cache_opts = import_resolver::CacheOpts {
                force_refresh: no_cache,
            };

            let mut has_issues = false;

            // `check --frozen` runs the upstream binary-hash diff
            // opportunistically: mismatches are P2 warnings (exit 0);
            // `--strict` escalates to CRIT and gates exit (verify parity).
            // Fetch errors (no `solana` CLI / network) never gate either
            // mode — a sandbox without the toolchain mustn't fail CI.
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
                            // Couldn't open the lock — note but never gate
                            // exit (parity with verify's Error routing).
                            eprintln!("note: --frozen upstream check skipped: {}", e);
                        }
                    }
                }

                // proof_hash drift routing (sibling of the binary_hash
                // dispatch): parse under Frozen so handle_lock populates
                // `proof_hash_findings`. P2 under `--frozen`, CRIT with
                // `--strict`. Parse errors re-raise at the main parse
                // below — don't double-report here.
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

            // Anchor cross-check (--anchor-project) — spec handler list vs
            // the existing program; catches stale specs and uncovered
            // handlers as a CI gate.
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

            // Orphan / missing preservation theorems in Proofs.lean — runs
            // whenever the proofs dir exists; no-op without obligations.
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

            // Fall back to .qed/config.json's `spec` like check/codegen so
            // flag-less `qedgen verify` works.
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;

            // Parse once if either gate needs the ParsedSpec; both are
            // pre-checks that may exit before backends dispatch.
            let parsed_for_gates = if require_verified || recursive {
                Some(check::parse_spec_file(&spec)?)
            } else {
                None
            };

            // --require-verified: fail fast before backends — results
            // against a not-fully-proven dep graph still rest on Stance-1
            // axiom discharge.
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

            // --recursive: `lake build` every imported proof package
            // (resolver returns DFS-pre-order = bottom-up, cycle-detected).
            // Keep walking on failure so every breakage shows; aggregate
            // exit at the end. Empty list = no-op success.
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
                                // First ~10 lines of each stream — the head
                                // usually identifies the failure.
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

            // --check-upstream diffs each pinned binary hash against the
            // on-chain `.so` (`solana program dump`); runs independently of
            // the harnesses; --offline refuses network. Auto-on when
            // qed.lock has any pinned hash — skipping the flag no longer
            // bypasses the gate; `--upstream-stale-ok` suppresses it for
            // offline dev.
            let spec_dir = spec.parent().unwrap_or_else(|| Path::new("."));
            let run_upstream = if upstream_stale_ok {
                // Honored even when --check-upstream is explicit — the
                // local-dev escape hatch, not a "render warnings anyway" knob.
                false
            } else if check_upstream {
                true
            } else {
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
                // Allowed combination — breadcrumb that the suppression
                // flag won.
                eprintln!(
                    "note: --upstream-stale-ok suppressed --check-upstream (offline-dev mode)"
                );
                let any_backend_flag = proptest || kani || lean || miri || probe_repros;
                if !any_backend_flag {
                    return Ok(());
                }
            }

            // --probe-repros runs the per-probe Mollusk reproducers — a
            // separate stage with its own report shape (not folded into the
            // BackendReport rollup), run first so the auditor has the
            // gating data.
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
            // is present on disk.
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

            // --crucible is a thin alias over the probe engine; findings
            // wrap into one BackendReport so they render alongside
            // Kani/proptest.
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
        // Exit codes match ratchet: findings map to 1/2 via
        // `ratchet::exit_code`; caller-side failures (missing IDL, bad
        // JSON) exit 3 so CI can tell breakage from misconfiguration.
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
            // `is_pinocchio` gates the post-regen stamped-drift scan below
            // (the Pinocchio scaffold carries no `#[qed(verified)]` stamps).
            let is_pinocchio = matches!(target, Target::Pinocchio);
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            // sBPF specs model assembly, not a Rust state machine — every
            // Rust-shaped artifact is meaningless for them. Decide up front
            // so the scaffold's handlers gate can't fire before the Lean
            // branch (#88): assembly targets emit only `--lean` and `--ci`.
            let is_assembly = check::parse_spec_file(&spec)?.is_assembly_target();
            if is_assembly {
                eprintln!(
                    "note: sBPF spec — skipping Rust scaffold (assembly programs \
                     generate Lean proofs via --lean; runtime checks belong in \
                     client-side tests)."
                );
            } else {
                let parsed = check::parse_spec_file(&spec)?;
                let mir = mir::lower(&parsed);
                codegen_mir::generate(&mir, &parsed, &spec, &output_dir, target)?;
            }

            if kani || all {
                // sBPF is verified by Lean proofs over the assembly; the
                // harness generator has no sBPF awareness and would emit
                // meaningless Anchor-shaped harnesses. Skip.
                if is_assembly {
                    if kani {
                        eprintln!(
                            "note: skipping Kani codegen for sBPF spec — assembly \
                             programs are verified via Lean proofs; runtime checks \
                             belong in client-side tests."
                        );
                    }
                } else {
                    // Codegen is pure text generation — the hard cargo-kani
                    // gate lives in `qedgen verify --kani`. Warn so the
                    // install hint surfaces; don't block.
                    if let Err(e) = deps::require_kani() {
                        eprintln!("warning: {e}");
                    }
                    let parsed = check::parse_spec_file(&spec)?;
                    let mir = mir::lower(&parsed);
                    kani_mir::generate(&mir, &parsed, &kani_output)?;
                }
            }

            // Impl-targeted Kani harness emits when `--kani-impl` is
            // explicit, or `--all` + at least one handler auto-triggers
            // (modifies ⊋ effect.lhs — the LP-shape signal;
            // `kani_impl::spec_triggers_impl_harness`). Plain `--kani`
            // stays model-only so the spec-transition and implementation
            // gates remain separable. sBPF never emits Kani — suppress the
            // auto-trigger too.
            let auto_impl_trigger = if is_assembly {
                false
            } else {
                let parsed = check::parse_spec_file(&spec)?;
                kani_impl::spec_triggers_impl_harness(&parsed)
            };
            let want_kani_impl = !is_assembly && (kani_impl || (all && auto_impl_trigger));
            if want_kani_impl {
                if let Err(e) = deps::require_kani() {
                    eprintln!("warning: {e}");
                }
                // Pinocchio/Quasar harnesses must live in `src/` — `cargo
                // kani` only discovers `#[kani::proof]` in the lib, and the
                // harness's `crate::<Pascal>` refs only resolve there.
                // Anchor keeps its `tests/` default.
                let kani_impl_path = if matches!(target, Target::Pinocchio | Target::Quasar) {
                    redirect_kani_impl_to_src(&kani_impl_output)
                } else {
                    kani_impl_output.clone()
                };
                kani_impl::generate(
                    &spec,
                    &kani_impl_path,
                    /*explicit_flag=*/ kani_impl,
                    target,
                )?;
            }

            if test || all {
                // Meaningless for assembly targets (same rationale as Kani).
                if is_assembly {
                    if test {
                        eprintln!(
                            "note: skipping unit-test codegen for sBPF spec — assembly \
                             programs are verified via Lean proofs; runtime checks \
                             belong in client-side tests."
                        );
                    }
                } else {
                    unit_test::generate(&spec, &test_output)?;
                }
            }
            if proptest || all {
                // Meaningless for assembly targets (same rationale as Kani).
                if is_assembly {
                    if proptest {
                        eprintln!(
                            "note: skipping proptest codegen for sBPF spec — assembly \
                             programs are verified via Lean proofs; runtime checks \
                             belong in client-side tests."
                        );
                    }
                } else {
                    let parsed = check::parse_spec_file(&spec)?;
                    let mir = mir::lower(&parsed);
                    proptest_gen_mir::generate(&mir, &parsed, &proptest_output)?;
                }
            }
            if crucible || all {
                // Crucible fuzzes the Rust handler surface — likewise
                // meaningless for assembly targets.
                if is_assembly {
                    if crucible {
                        eprintln!(
                            "note: skipping Crucible codegen for sBPF spec — assembly \
                             programs are verified via Lean proofs; runtime checks \
                             belong in client-side tests."
                        );
                    }
                } else {
                    let parsed = check::parse_spec_file(&spec)?;
                    crucible_gen::generate(
                        &parsed,
                        &crucible_output,
                        crucible_gen::InvariantMode::Spec,
                    )?;
                }
            }
            if integration || all {
                if is_assembly {
                    if integration {
                        eprintln!(
                            "note: skipping integration-test codegen for sBPF spec — \
                             assembly programs are verified via Lean proofs; runtime \
                             checks belong in client-side tests."
                        );
                    }
                } else {
                    integration_test::generate(&spec, &integration_output)?;
                }
            }
            if lean || all {
                // Pure text writers; `lake` is only needed to *build*, which
                // `qedgen verify --lean` gates. Warn without blocking.
                if let Err(e) = deps::require_lean() {
                    eprintln!("warning: {e}");
                }
                let parsed = check::parse_spec_file(&spec)?;
                // `lean_gen_mir` handles every spec shape — single/multi-
                // account, indexed records, ADTs, and sBPF (via the MIR
                // `is_assembly` flag).
                let mir = mir::lower(&parsed);
                lean_gen_mir::generate(&mir, &parsed, &lean_output)?;
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

            // Surface stale `#[qed(verified)]` stamps right after regen so
            // users get the re-stamp command before the proc-macro's
            // `compile_error!` fires on the next build. Skipped for
            // Pinocchio (no stamps), assembly targets (no scaffold), and a
            // missing output_dir.
            if !is_pinocchio && !is_assembly && output_dir.exists() {
                match drift::check_stamped_drift(&output_dir) {
                    Ok(stamped) if !stamped.is_empty() => {
                        eprintln!(
                            "cargo:warning={} verified handler(s) have stale stamps after regen:",
                            stamped.len()
                        );
                        for entry in &stamped {
                            eprintln!(
                                "cargo:warning=  {}::{}",
                                entry.file.display(),
                                entry.fn_name
                            );
                        }
                        // All stamped fns share the same `--drift` root, so
                        // one invocation re-stamps the whole tree.
                        eprintln!(
                            "cargo:warning=hint: run `qedgen check --drift {} --update-hashes` \
                             to re-stamp",
                            output_dir.display()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("warning: stamped-drift scan failed: {}", e);
                    }
                }
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
    use super::{expand_ci_template, format_lint_warning, redirect_kani_impl_to_src};
    use crate::check::{CompletenessWarning, Severity};
    use std::path::PathBuf;

    #[test]
    fn kani_impl_path_redirects_tests_to_src_for_pinocchio() {
        // Default Anchor-shaped path → sibling src/.
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("./programs/tests/kani_impl.rs")),
            PathBuf::from("./programs/src/kani_impl.rs"),
        );
        // Bare tests/ root → src/.
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("tests/kani_impl.rs")),
            PathBuf::from("src/kani_impl.rs"),
        );
        // Non-tests parent passes through (explicit override respected).
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("./custom/kani_impl.rs")),
            PathBuf::from("./custom/kani_impl.rs"),
        );
        assert_eq!(
            redirect_kani_impl_to_src(&PathBuf::from("./programs/src/kani_impl.rs")),
            PathBuf::from("./programs/src/kani_impl.rs"),
        );
    }

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

    // verify.yml carries {{VERIFY_STEP}} and {{RATCHET_STEP}} placeholders;
    // a refactor that drops or mangles either would be invisible elsewhere —
    // these three tests catch that cheaply.
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
