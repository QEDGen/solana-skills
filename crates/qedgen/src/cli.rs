//! CLI surface: the clap argument definitions for every `qedgen`
//! subcommand. Split out of `main.rs` (v3.0 prep) — pure type/derive
//! definitions, no dispatch logic. `Target` is re-exported from the crate
//! root so existing `crate::Target` paths (verify::regen_drift,
//! codegen_shared::guards) keep resolving.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
/// Find the bugs your tests miss — from one spec file
#[derive(Parser)]
#[command(name = "qedgen")]
#[command(version, about, long_about = None)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

/// Solana program framework target for greenfield codegen
/// (`qedgen init --target ...`). All three targets are wired
/// end-to-end (`codegen_mir` dispatch): full scaffold + spec-model
/// Kani/proptest + per-target impl-Kani shape.
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

/// CLI override → probe runtime, 1:1 by name. Keeps the two dispatch
/// sites (`--program` and `--fuzz --root`) from hand-mapping the enum.
impl From<RuntimeOverride> for crate::probe::Runtime {
    fn from(r: RuntimeOverride) -> Self {
        match r {
            RuntimeOverride::Pinocchio => Self::Pinocchio,
            RuntimeOverride::Anchor => Self::Anchor,
            RuntimeOverride::Quasar => Self::Quasar,
            RuntimeOverride::Native => Self::Native,
            RuntimeOverride::Sbpf => Self::Sbpf,
        }
    }
}

/// User-facing Crucible verification layer for `qedgen probe --fuzz`.
/// Kept distinct from codegen's internal invariant-family enum so the CLI
/// can enforce the artifacts each audit layer needs before fuzzing starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum CrucibleMode {
    /// Behavioral/protocol guards derived mechanically from account state.
    Protocol,
    /// Structural assertions compiled from a `.qedspec` skeleton.
    Skeleton,
    /// Ratified domain intent from a dossier, compiled through its `.qedspec`.
    Domain,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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

    /// DEPRECATED (slated for v3.0 removal): "adapt" bundled two
    /// unrelated jobs and both now have honest homes — scaffold mode is
    /// subsumed by `qedgen probe --emit-spec-candidates --audit-dir`
    /// (elicitation-first: confirmable hypotheses instead of TODO
    /// stubs), and attribute mode is `qedgen stamp` (same emission plus
    /// the recorded-verification gate). Both modes remain functional in
    /// v2.x to avoid breaking existing scripts.
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

    /// Stamp `#[qed(verified, …)]` drift attributes for an
    /// already-verified spec (Anchor-only; the post-verification half of
    /// the old `adapt`). Emits one attribute per handler — body hash +
    /// spec-block hash (+ Accounts-struct seal) — to paste above each
    /// `pub fn`; the proc macro recomputes both at compile time and fires
    /// `compile_error!` on drift.
    ///
    /// Runs AFTER verification and proves nothing itself: it requires
    /// recorded implementation-verified evidence (written by
    /// `qedgen verify` to `.qed/verify-evidence.json`, with a passing
    /// implementation-bound backend — miri or a `kani_impl` harness) whose
    /// spec and program-source hashes match what is being stamped. Probe
    /// reproducers confirm findings and are not conformance evidence.
    /// Checking or model-tested results are not eligible.
    Stamp {
        /// Path to the program crate (the directory containing the
        /// program's own `Cargo.toml`, with `src/lib.rs` inside).
        #[arg(long)]
        program: PathBuf,

        /// The verified `.qedspec` to stamp against.
        #[arg(long)]
        spec: PathBuf,

        /// Path to write the attribute report. Without this flag,
        /// prints to stdout.
        #[arg(long)]
        out: Option<PathBuf>,

        /// Manually point an unrecognized handler at its actual
        /// implementation. Format: `<handler>=<rust_path>`. Repeatable.
        #[arg(long = "handler", value_name = "NAME=PATH")]
        handler_overrides: Vec<String>,

        /// Override the verification-evidence path (default:
        /// `<spec_dir>/.qed/verify-evidence.json`).
        #[arg(long)]
        evidence: Option<PathBuf>,
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
    ///   against the parsed `.qedspec`; unconfirmed hits are candidates and
    ///   only reproducer-backed results are findings.
    /// - **Spec-less** (`--bootstrap --root <path>`): walks a brownfield
    ///   project, detects runtime, discovers handlers, emits the work-list
    ///   envelope (handlers + applicable categories) for the auditor to
    ///   investigate via Read/Grep on the impl source.
    /// - **Fuzz, spec-driven** (`--fuzz <budget> --spec <path>`): builds
    ///   the spec-driven Crucible harness and surfaces crashes as Findings.
    /// - **Fuzz, brownfield** (`--fuzz <budget> --root <path>`, v2.21):
    ///   synthesises a minimal handler list from the project, emits a
    ///   protocol-only Crucible harness under `<root>/.qed/fuzz/` and checks
    ///   observable post-state guards (lamports, ownership/type, close/realloc,
    ///   rent, and token conservation). Program-internal faults remain
    ///   transaction errors and need a spec assertion or separate reproducer.
    ///   No `.qedspec` required.
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
        ///   with mechanical post-state guards).
        ///
        /// Typically the program crate dir, e.g. `programs/lending`.
        #[arg(long)]
        root: Option<PathBuf>,

        /// Program audit mode. Walks `<path>` and routes through the
        /// runtime's dedicated extractor: Pinocchio emits the site
        /// catalogue + SAFETY-comment metadata (v2.19); Anchor/Quasar
        /// route through the anchor extractor (scaffold-to-spec
        /// interview); native/qedgen-codegen route through the native
        /// extractor. Runtimes without an extractor fall back to the
        /// bootstrap envelope. Detection auto-routes via `Cargo.toml`.
        ///
        /// This is a static engine only — it conflicts with `--fuzz`
        /// (use `--fuzz <budget> --root <path>` for brownfield fuzzing
        /// and merge the two JSON outputs yourself).
        #[arg(long, conflicts_with_all = ["spec", "bootstrap", "fuzz", "root"])]
        program: Option<PathBuf>,

        /// Override runtime detection (`pinocchio`, `anchor`, `quasar`,
        /// `native`, `sbpf`). Pinocchio, Anchor/Quasar, and native each
        /// have a dedicated extractor under `--program`; runtimes
        /// without one fall back to the generic bootstrap envelope. `sbpf`
        /// identifies the target but provides metadata only; the source
        /// auditor does not audit assembly.
        #[arg(long, value_enum)]
        runtime: Option<RuntimeOverride>,

        /// Coverage-guided fuzz probe engine (v2.18). Drives a generated
        /// Crucible harness for the given budget and converts each crash
        /// into a Finding with `Reproducer::Crucible`. Different engine
        /// from the pattern-match predicates above — a `--fuzz` run
        /// REPLACES the predicate pass in a single invocation. To get
        /// both, run `probe --spec` and `probe --fuzz --spec` separately
        /// and merge the JSON.
        ///
        /// Pair with either `--spec <path>` (spec-driven harness,
        /// asserts spec invariants) or `--root <project-path>` (v2.21
        /// brownfield protocol-mode — emits a harness with mechanical
        /// post-state guards). Passing both layers spec invariants on top
        /// of protocol guards.
        ///
        /// Budget is wall-clock seconds (e.g. `300` for 5 min). Pass `0`
        /// for a dry run that emits the harness but does not build or fuzz.
        #[arg(long)]
        fuzz: Option<u64>,

        /// Select the Crucible verification layer explicitly:
        /// - `protocol`: mechanical behavioral guards; requires `--root`.
        /// - `skeleton`: structural `.qedspec` assertions; requires `--spec`.
        /// - `domain`: ratified domain facts plus protocol guards; requires
        ///   `--spec` and `--domain-dossier`.
        ///
        /// When omitted, the legacy argument-based inference remains:
        /// root-only = protocol, spec-only = skeleton, spec + root = both.
        #[arg(long, value_enum, requires = "fuzz")]
        crucible_mode: Option<CrucibleMode>,

        /// Canonical `domain-dossier.json` consumed by
        /// `--crucible-mode domain`. Facts assigned to the Crucible lane
        /// must all be ratified (`auto` or `user`) before fuzzing starts.
        #[arg(long, requires = "fuzz")]
        domain_dossier: Option<PathBuf>,

        /// Deterministic action targets emitted by `qedgen ratify`. With
        /// `--domain-sequence-bindings`, every target must resolve before
        /// domain-mode replay starts.
        #[arg(long, requires = "fuzz", requires = "domain_sequence_bindings")]
        domain_sequences: Option<PathBuf>,

        /// Explicit user values for every unresolved account, argument, and
        /// lifecycle association in `--domain-sequences`. Values are never
        /// inferred from names or nearby source.
        #[arg(long, requires = "fuzz", requires = "domain_sequences")]
        domain_sequence_bindings: Option<PathBuf>,

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

        /// Lift probe evidence into candidate spec clauses (`clusters[]`)
        /// for the scaffold-to-spec interview. Clusters are proto-spec
        /// clauses and are distinct from unconfirmed security leads in
        /// `candidates[]`. Off by default within the schema-v3 envelope.
        #[arg(long)]
        emit_spec_candidates: bool,

        /// v2.19 M1.5/M1.7: when `--emit-spec-candidates` is also set,
        /// materialize the full audit working set into this directory:
        /// `interview.md` (user-editable prompts), `clusters.json` (the
        /// full cluster envelope), `skeleton.qedspec` (the pre-interview
        /// structural skeleton), `domain-dossier.json` plus its Markdown
        /// rendering, `domain-interview.json` plus its Markdown rendering,
        /// and `run-manifest.json` (resumable lane status). The
        /// companion `qedgen ratify --audit-dir <path>` consumes the interview,
        /// clusters, and skeleton; the domain artifacts remain the audit/spec
        /// provenance record. Conventionally
        /// `.qed/audit/<timestamp>/`.
        #[arg(long, requires = "emit_spec_candidates")]
        audit_dir: Option<PathBuf>,

        /// Build and run generated reproducer harnesses (#228), promoting a
        /// candidate to a finding only when its harness actually reproduces.
        /// Off by default: the default path only *generates* harnesses under
        /// `target/qedgen-repros/` and leaves the candidate carrying a
        /// `repro_harness` pointer for the agent/CI to run — so a plain
        /// `probe --spec` performs no builds and no execution. Requires
        /// `rustc` on PATH (soft dependency).
        #[arg(long)]
        execute_repros: bool,

        /// Accepted for CLI consistency with sibling subcommands
        /// (`verify --json`, `readiness --json`); probe output is
        /// always the JSON envelope, so this flag is a no-op (#251).
        #[arg(long)]
        json: bool,
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
    /// - `domain-dossier.json` — structural candidate ratification states,
    ///   when the audit working set carries schema-v1 domain artifacts.
    /// - `spec-handoff.json` — structural/domain/regression layer status,
    ///   provenance IDs, and explicit language gaps.
    /// - `domain-sequences.json` — stateful setup/forward/reverse/teardown
    ///   coverage targets with unresolved accounts and arguments made explicit.
    Ratify {
        /// Audit working-set directory (the one passed to `probe
        /// --audit-dir`). Must contain `clusters.json` and
        /// `skeleton.qedspec`, plus either a structured `answers.json`
        /// (the in-harness interview's answer set) or the legacy
        /// `interview.md`.
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

        /// Structured answer set (`{id → accept|reject|bug + note}`,
        /// hypothesis and cluster IDs alike). Defaults to
        /// `<audit_dir>/answers.json` when present; when resolved, the
        /// legacy `interview.md` is not consulted.
        #[arg(long)]
        answers: Option<PathBuf>,

        /// Also generate the spec-model proptest harness
        /// (`<audit_dir>/model-proptest.rs`) from the ratified spec.
        /// Generation is `checking`-level evidence; *running* the harness
        /// earns `model-tested`.
        #[arg(long, default_value_t = false)]
        proptest: bool,
    },

    /// DEPRECATED (slated for v3.0 removal): the IDL is now an evidence
    /// source for `qedgen probe` (the spec-elicitation front door) rather
    /// than a standalone shell emitter — probe's hypothesizer consumes
    /// IDL signer flags and `has_one` relations directly and offers
    /// confirmable clauses instead of a TODO shell. Remains functional in
    /// v2.x to avoid breaking existing scripts.
    ///
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

    /// Emit a name-level refinement descriptor (JSON, to stdout) for a single
    /// constant-increment handler: the producer half of the qedgen <-> qedsvm
    /// discharge seam. qedsvm's qedlift consumes it via `--descriptor`
    /// (schema: qedsvm docs/REFINEMENT_DESCRIPTOR.md). The descriptor carries
    /// only semantics (which named field a handler mutates, by how much); the
    /// field offsets are resolved from the IDL on the qedsvm side.
    Descriptor {
        /// Path to the `.qedspec`
        #[arg(long)]
        spec: PathBuf,

        /// Handler to inspect (must have a single-field `+= <int literal>` effect)
        #[arg(long)]
        handler: String,

        /// Account name for the descriptor (default: the spec's first account
        /// type, else the program name). Use the IDL account name so qedsvm can
        /// resolve the field offsets from the IDL.
        #[arg(long)]
        account: Option<String>,
    },

    /// Run the full spec -> byte-level proof chain for one handler: build the
    /// name-level descriptor from the `.qedspec`, then discharge it against the
    /// compiled `.so` via qedsvm's `qedlift`. Reports whether the handler's
    /// effect is proven against the bytes. This is the one-command driver over
    /// the qedgen <-> qedsvm seam (`descriptor` + qedlift `--descriptor`).
    Discharge {
        /// Path to the `.qedspec`
        #[arg(long)]
        spec: PathBuf,

        /// Handler to discharge (single-field `+= <int literal>` effect)
        #[arg(long)]
        handler: String,

        /// Account name (default: the spec's first account type / program name).
        /// Use the IDL account name so qedlift resolves the offsets from the IDL.
        #[arg(long)]
        account: Option<String>,

        /// Compiled program to discharge against
        #[arg(long)]
        so: PathBuf,

        /// Codama IDL (.json) supplying the account shape (offsets)
        #[arg(long)]
        idl: Option<PathBuf>,

        /// Path to a built qedsvm `qedlift` binary (built with
        /// `--features qedrecover`)
        #[arg(long)]
        qedlift: PathBuf,

        /// Lean module name for the emitted proof (default: `<Account><Handler>`)
        #[arg(long)]
        module: Option<String>,

        /// Persist the discharged proof (`<Module>TracedLifted.lean` +
        /// `<Module>Refinement.lean`) into this directory instead of a temp
        /// dir. Omit to keep the verdict-only (artifact-discarded) behaviour.
        #[arg(long)]
        out_dir: Option<PathBuf>,

        /// Whole-transition mode (qedsvm #40, v0.9.0): lift EVERY path of
        /// the program from discovered `<stem>_<path>.pcs` traces beside the
        /// `.so` and emit per-path `*_transition_path` /
        /// `*_transition_fault` corollaries plus the one bundle theorem
        /// covering success and abort paths under their branch guards.
        /// Requires `--out-dir` (qedlift writes the modules directly) and
        /// >= 2 traces beside the binary.
        #[arg(long)]
        transition: bool,
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
        /// named framework target (`anchor`, `quasar`, or `pinocchio` —
        /// all fully implemented). Omit to skip program scaffolding
        /// entirely.
        #[arg(long, value_enum)]
        target: Option<Target>,

        /// Output directory (default: ./formal_verification)
        #[arg(long, default_value = "./formal_verification")]
        output_dir: PathBuf,
    },

    /// Validate a spec — lint, coverage, drift, and verification report
    ///
    /// Default (no flags): runs lint + coverage.
    /// With --explain: generates a Markdown verification report (add --json
    /// for the structured verification-status payload the agent renders from).
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

        /// Program crate whose implementation is exercised by an
        /// implementation-bound backend. Required for verification evidence
        /// that can authorize `qedgen stamp`: the source-tree hash recorded
        /// here must still match the crate passed to `stamp`.
        #[arg(long)]
        program: Option<PathBuf>,

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
        /// Emits `note: no repros found` only when the directory contains
        /// no runnable reproducers.
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

        /// #332 — fail unless every backend obligation is `emitted`.
        /// Recomputes the reconciled backend-obligation manifest in
        /// memory (kani / lean / proptest) and exits 1 on any
        /// `unsupported` or `failed` entry: a passing strict verify
        /// means no requested obligation was silently dropped by a
        /// backend. Off by default because known capability gaps
        /// (multi-account file-level features, ADT Kani parity,
        /// pubkey-guard Lean clauses) would fail every affected spec.
        #[arg(long)]
        strict: bool,
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
    /// Default (no artifact flags): generates the Rust program skeleton for
    /// the chosen `--target` (default: `anchor`). Explicit artifact flags
    /// generate only those artifacts; use `--all` for the scaffold and every
    /// artifact.
    Codegen {
        /// Path to the spec file (.qedspec or a directory of fragments).
        /// Optional — falls back to the `spec` field in the nearest
        /// `.qed/config.json` discovered by walking up from cwd.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Framework target for the Rust program crate: `anchor`
        /// (default), `quasar` (Blueshift's `quasar_lang`), or
        /// `pinocchio` — all fully implemented. Known per-target gaps
        /// (generic CPI on Quasar/Pinocchio, imported account mirrors
        /// on Pinocchio) surface as `todo!()` breadcrumbs or a clean
        /// error, never silent wrong output.
        #[arg(long, value_enum, default_value_t = Target::Anchor)]
        target: Target,

        /// Output directory for the generated Rust program crate
        #[arg(long, default_value = "./programs")]
        output_dir: PathBuf,

        /// Regenerate the USER-OWNED files too (`src/lib.rs`,
        /// `src/instructions/*.rs`) — the rename workflow where regen +
        /// re-fill beats hand-merging (#288). Destructive: handler fills
        /// are overwritten, so every affected file must have a committed,
        /// unmodified git baseline (the recovery path); dirty or untracked
        /// files abort before anything is written.
        #[arg(long, conflicts_with = "merge_accounts")]
        force: bool,

        /// Surgical alternative to --force for spec-level renames (#288):
        /// regenerate only the `#[derive(Accounts)]` structs inside the
        /// user-owned `lib.rs`, preserving handler fills and everything
        /// else (the Cargo.toml section-merge doctrine applied to Rust
        /// items). Hand-tuned constraints inside replaced structs are
        /// overwritten, so the same git-baseline guard applies. Structs
        /// with no matching handler (e.g. pre-rename leftovers) are left
        /// in place and reported. Anchor target only.
        #[arg(long)]
        merge_accounts: bool,

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
        /// fields). Per-target shapes: Anchor/Quasar drive the accounts
        /// struct; Pinocchio uses its `#[repr(C)]` stack-`AccountInfo`
        /// shape. The brownfield/context modes below are Anchor-only.
        #[arg(long)]
        kani_impl: bool,

        /// Output path for impl-targeted Kani harnesses (default:
        /// `./programs/tests/kani_impl.rs`). Separate file from the
        /// spec-model `kani.rs` so `cargo kani --harness` can target
        /// either set without ambiguity.
        #[arg(long, default_value = "./programs/tests/kani_impl.rs")]
        kani_impl_output: PathBuf,

        /// Emit the BROWNFIELD Anchor impl-Kani shape (#162): a state-struct
        /// harness (symbolic state → apply the real effect + validity gate →
        /// assert `ensures`) instead of the greenfield `Accounts` context +
        /// `accounts.handler(...)` shape, which does not resolve against a
        /// pre-existing Anchor program (shared Accounts structs, `Context<T>`
        /// + `Args`, associated-fn handlers). Anchor target only.
        #[arg(long)]
        kani_impl_brownfield: bool,

        /// Emit the CONTEXT/instruction impl-Kani shape (#169): drive the real
        /// `#[derive(Accounts)]` constraint gate (`try_accounts`) with symbolic
        /// `AccountInfo`s + the real instruction fn through a `Context`
        /// (agent-fill), and assert the instruction-level authorization
        /// property (signer / has_one / owner / seeds) that the state-struct
        /// shape cannot reach. Anchor target only.
        #[arg(long)]
        kani_impl_context: bool,

        /// Generate unit tests (plain Rust, cargo test)
        #[arg(long)]
        test: bool,

        /// Output path for unit tests (default: ./programs/tests/unit.rs).
        /// Lives in `tests/` so cargo auto-discovers it as a test target —
        /// a `src/` location needs a `mod` hook the scaffold never emits
        /// (pre-v2.47 default `./programs/src/tests.rs` was dead code).
        #[arg(long, default_value = "./programs/tests/unit.rs")]
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

        /// Output path for integration tests (default: ./programs/tests/integration_tests.rs)
        #[arg(long, default_value = "./programs/tests/integration_tests.rs")]
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
pub(crate) enum AristotleCommands {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_bootstrap_accepts_json_flag() {
        // #251: probe output is always JSON, but the flag learned on
        // sibling subcommands must parse instead of hard-erroring.
        let cli = Cli::try_parse_from(["qedgen", "probe", "--bootstrap", "--root", "p", "--json"])
            .expect("--json must parse on probe");
        let Commands::Probe {
            bootstrap, json, ..
        } = cli.command
        else {
            panic!("expected probe command");
        };
        assert!(bootstrap);
        assert!(json);
    }

    #[test]
    fn parses_explicit_domain_crucible_mode() {
        let cli = Cli::try_parse_from([
            "qedgen",
            "probe",
            "--fuzz",
            "0",
            "--crucible-mode",
            "domain",
            "--spec",
            "vault.qedspec",
            "--domain-dossier",
            "domain-dossier.json",
        ])
        .unwrap();
        let Commands::Probe {
            crucible_mode,
            domain_dossier,
            ..
        } = cli.command
        else {
            panic!("expected probe command");
        };
        assert_eq!(crucible_mode, Some(CrucibleMode::Domain));
        assert_eq!(domain_dossier, Some(PathBuf::from("domain-dossier.json")));
    }

    #[test]
    fn domain_sequence_replay_requires_and_parses_both_artifacts() {
        let cli = Cli::try_parse_from([
            "qedgen",
            "probe",
            "--fuzz",
            "30",
            "--crucible-mode",
            "domain",
            "--spec",
            "vault.qedspec",
            "--domain-dossier",
            "domain-dossier.json",
            "--domain-sequences",
            "domain-sequences.json",
            "--domain-sequence-bindings",
            "domain-sequence-bindings.json",
        ])
        .unwrap();
        let Commands::Probe {
            domain_sequences,
            domain_sequence_bindings,
            ..
        } = cli.command
        else {
            panic!("expected probe command");
        };
        assert_eq!(
            domain_sequences,
            Some(PathBuf::from("domain-sequences.json"))
        );
        assert_eq!(
            domain_sequence_bindings,
            Some(PathBuf::from("domain-sequence-bindings.json"))
        );

        let missing_bindings = Cli::try_parse_from([
            "qedgen",
            "probe",
            "--fuzz",
            "30",
            "--domain-sequences",
            "domain-sequences.json",
        ]);
        assert!(missing_bindings.is_err());
    }

    #[test]
    fn crucible_mode_requires_fuzz_flag() {
        let parsed = Cli::try_parse_from([
            "qedgen",
            "probe",
            "--crucible-mode",
            "protocol",
            "--root",
            ".",
        ]);
        let error = match parsed {
            Ok(_) => panic!("mode without --fuzz should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("--fuzz"));
    }
}
