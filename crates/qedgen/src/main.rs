mod api;
mod aristotle;
mod asm2lean;
mod ast;
mod banner;
mod check;
mod chumsky_adapter;
mod chumsky_parser;
mod codegen;
mod consolidate;
mod deps;
mod drift;
mod fill;
mod fingerprint;
mod idl2spec;
mod init;
mod integration_test;
mod interface_gen;
mod kani;
mod lean_gen;
mod project;
mod proofs_bootstrap;
mod proptest_gen;
mod ratchet;
mod reconcile;
mod rust_codegen_util;
mod sbpf_verify;
mod spec;
mod spec_hash;
mod unit_test;
mod validate;
mod verify;

use anyhow::{ensure, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Find the bugs your tests miss — from one spec file
#[derive(Parser)]
#[command(name = "qedgen")]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

    /// Generate SPEC.md or .qedspec from an Anchor IDL or a .qedspec file
    Spec {
        /// Path to Anchor IDL JSON file
        #[arg(long, required_unless_present = "from_spec")]
        idl: Option<PathBuf>,

        /// Path to .qedspec file (alternative to --idl)
        #[arg(long, conflicts_with = "idl")]
        from_spec: Option<PathBuf>,

        /// Path to proofs directory (for --from-spec status checking)
        #[arg(long)]
        proofs: Option<PathBuf>,

        /// Directory to write output (default: ./formal_verification)
        #[arg(long, default_value = "./formal_verification")]
        output_dir: PathBuf,

        /// Output format: "md" (default) or "qedspec"
        #[arg(long, default_value = "md")]
        format: String,
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

        /// Also generate a Quasar program skeleton and Kani harnesses
        #[arg(long)]
        quasar: bool,

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

        /// Path to generated Quasar program directory (enables code drift detection)
        #[arg(long)]
        code: Option<PathBuf>,

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
    },

    /// Run the generated harnesses against the generated implementation.
    ///
    /// `check` validates the spec; `verify` validates the code the spec
    /// produced. Default (no flags) runs every backend whose artifact is
    /// present on disk. Use --proptest/--kani/--lean to target one backend.
    Verify {
        /// Path to the spec file (.qedspec)
        #[arg(long)]
        spec: PathBuf,

        /// Run proptest harnesses (cargo test --release)
        #[arg(long)]
        proptest: bool,

        /// Path to the proptest harness file (matches codegen default)
        #[arg(long, default_value = "./programs/tests/proptest.rs")]
        proptest_path: PathBuf,

        /// Run Kani BMC harnesses (cargo kani) — lands in v2.4-M2
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

        /// Stop on the first failing backend
        #[arg(long)]
        fail_fast: bool,

        /// Output as JSON (for agent consumption)
        #[arg(long)]
        json: bool,
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
        /// Path to the Anchor IDL JSON (typically target/idl/<program>.json)
        #[arg(long)]
        idl: PathBuf,

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
        #[arg(long)]
        old: PathBuf,

        /// Path to the candidate IDL (the one the upgrade would ship).
        #[arg(long)]
        new: PathBuf,

        /// Acknowledge a specific unsafe finding so it reports as
        /// additive instead (repeatable). See `ratchet list-rules` for
        /// the full flag catalog.
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

        /// Output as JSON (for agent / CI consumption)
        #[arg(long)]
        json: bool,
    },

    /// Generate committed artifacts from a qedspec
    ///
    /// Default (no flags): generates Quasar Rust skeleton only.
    /// Use flags to generate additional artifacts, or --all for everything.
    Codegen {
        /// Path to the spec file (.qedspec or a directory of fragments).
        /// Optional — falls back to the `spec` field in the nearest
        /// `.qed/config.json` discovered by walking up from cwd.
        #[arg(long)]
        spec: Option<PathBuf>,

        /// Output directory for the generated Quasar project
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

        /// Generate QuasarSVM integration test scaffolds
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

        /// After scaffolding, emit one stdout prompt block per handler
        /// whose generated body still contains a `todo!()`. The in-session
        /// agent (Claude / Codex) reads the prompts and edits the files.
        #[arg(long)]
        fill: bool,

        /// Restrict --fill to one handler by name (default: all that need filling)
        #[arg(long)]
        handler: Option<String>,

        /// After scaffolding, emit prompt blocks for every `todo!()` site in
        /// the generated integration test file. Same stdout-for-agent flow
        /// as --fill, but for `tests/integration_tests.rs` rather than
        /// per-handler files.
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
        /// Path to the spec file (.qedspec)
        #[arg(long)]
        spec: PathBuf,

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

/// Expand the committed CI template by substituting `{{VERIFY_STEP}}`
/// and `{{RATCHET_STEP}}` with the caller-provided snippets, then
/// normalise trailing whitespace so the workflow file ends with
/// exactly one newline regardless of whether either step was set.
///
/// Factored out of the `Codegen` match arm so the substitution is
/// unit-testable without spawning a process — the template bytes are
/// `include_str!`'d at compile time, so the test wires them in the
/// same way.
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
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

        Commands::Spec {
            idl,
            from_spec,
            proofs,
            output_dir,
            format,
        } => {
            if let Some(spec_path) = from_spec {
                spec::generate_spec_from_qedspec(&spec_path, proofs.as_deref(), &output_dir)?;
            } else if let Some(idl_path) = idl {
                if format == "qedspec" {
                    let stem = idl_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let output_file = output_dir.join(format!("{}.qedspec", stem));
                    idl2spec::generate_qedspec(&idl_path, &output_file)?;
                } else {
                    spec::generate_spec(&idl_path, &output_dir)?;
                }
            } else {
                anyhow::bail!("Either --idl or --from-spec must be specified");
            }
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
            quasar,
            output_dir,
        } => {
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

            init::init(&name, &output_dir, asm.as_deref(), mathlib, quasar)?;

            if quasar {
                let spec_path = output_dir.join("Spec.lean");
                let program_dir = program_root.join(format!("programs/{}", name));
                // v2.6: tests live INSIDE the program package so cargo-kani
                // and cargo-test can resolve the governing Cargo.toml via the
                // usual `tests/` convention. Previously at `tests/kani.rs` at
                // program_root, which had no Cargo.toml above it.
                let kani_path = program_dir.join("tests/kani.rs");

                // Generate Quasar program skeleton
                codegen::generate(&spec_path, &program_dir)?;

                // Generate Kani proof harnesses
                kani::generate(&spec_path, &kani_path)?;

                // Generate unit tests
                let test_path = program_dir.join("src/tests.rs");
                unit_test::generate(&spec_path, &test_path)?;
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
            drift,
            update_hashes,
            deep,
            kani,
            asm,
            json,
        } => {
            require_git_repo()?;
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            let spec_name = spec
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Spec".to_string());

            let mut has_issues = false;

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
                let parsed = check::parse_spec_file(&spec)?;
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
                let parsed = check::parse_spec_file(&spec)?;
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
                let warnings = check::lint(&spec)?;
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
            fail_fast,
            json,
        } => {
            require_git_repo()?;

            // No explicit backend flags -> run every backend whose artifact
            // is present on disk. This matches the agent-friendly "just do
            // the right thing" default from the PRD.
            let any_flag = proptest || kani || lean;
            let opts = if any_flag {
                verify::VerifyOpts {
                    spec: spec.clone(),
                    proptest,
                    proptest_path,
                    kani,
                    kani_path,
                    lean,
                    lean_dir,
                    fail_fast,
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
                    fail_fast,
                }
            };

            let report = verify::run(&opts)?;

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
        Commands::Readiness { idl, json } => {
            let report = match ratchet::run_readiness(&ratchet::ReadinessOpts { idl }) {
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
            json,
        } => {
            let report = match ratchet::run_check_upgrade(&ratchet::CheckUpgradeOpts {
                old,
                new,
                unsafes,
                migrated_accounts,
                realloc_accounts,
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
            output_dir,
            kani,
            kani_output,
            test,
            test_output,
            proptest,
            proptest_output,
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
            let cwd = std::env::current_dir()?;
            let spec = init::resolve_spec_path(spec.as_deref(), &cwd)?;
            // Rust skeleton (always)
            codegen::generate(&spec, &output_dir)?;

            if kani || all {
                deps::require_kani()?;
                kani::generate(&spec, &kani_output)?;
            }
            if test || all {
                unit_test::generate(&spec, &test_output)?;
            }
            if proptest || all {
                proptest_gen::generate(&spec, &proptest_output)?;
            }
            if integration || all {
                integration_test::generate(&spec, &integration_output)?;
            }
            if lean || all {
                deps::require_lean()?;
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
