mod api;
mod aristotle;
mod asm2lean;
mod check;
mod ci;
mod codegen;
mod consolidate;
mod drift;
mod explain;
mod fingerprint;
mod idl2spec;
mod init;
mod integration_test;
mod kani;
mod lean_gen;
mod parser;
mod project;
mod spec;
mod proptest_gen;
mod rust_codegen_util;
mod unit_test;
mod validate;
mod verify;

use anyhow::{ensure, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// CLI tool for formal Lean 4 verification of Solana programs
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

    /// Verify sBPF proofs: check source hash, regenerate if stale, run lake build
    Verify {
        /// Path to the sBPF assembly source file
        #[arg(long)]
        asm: PathBuf,

        /// Path to the formal_verification directory containing proofs
        #[arg(long, default_value = "./formal_verification")]
        proofs: PathBuf,
    },

    /// Check spec coverage and drift detection across all verification layers
    Check {
        /// Path to the spec file (Spec.lean)
        #[arg(long)]
        spec: PathBuf,

        /// Path to the proofs directory
        #[arg(long, default_value = "./formal_verification/Proofs")]
        proofs: PathBuf,

        /// Path to generated Quasar program directory (enables code drift detection)
        #[arg(long)]
        code: Option<PathBuf>,

        /// Path to generated Kani harness file (enables Kani drift detection)
        #[arg(long)]
        kani: Option<PathBuf>,
    },

    /// Generate a Markdown verification report with intent descriptions
    Explain {
        /// Path to the spec file (Spec.lean)
        #[arg(long)]
        spec: PathBuf,

        /// Path to the proofs directory
        #[arg(long, default_value = "./formal_verification")]
        proofs: PathBuf,

        /// Output file (default: stdout)
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Lint a qedspec for completeness — structured warnings for agent consumption
    Lint {
        /// Path to the spec file (Spec.lean)
        #[arg(long)]
        spec: PathBuf,

        /// Output as JSON (default: human-readable)
        #[arg(long)]
        json: bool,
    },

    /// Show operation × property coverage matrix
    Coverage {
        /// Path to the spec file (.qedspec)
        #[arg(long)]
        spec: PathBuf,

        /// Output as JSON (default: human-readable table)
        #[arg(long)]
        json: bool,
    },

    /// Generate a Quasar program skeleton from a qedspec Lean file
    Codegen {
        /// Path to the spec file (Spec.lean)
        #[arg(long)]
        spec: PathBuf,

        /// Output directory for the generated Quasar project
        #[arg(long, default_value = "./programs")]
        output_dir: PathBuf,
    },

    /// Generate Kani proof harnesses from a qedspec Lean file
    Kani {
        /// Path to the spec file (Spec.lean)
        #[arg(long)]
        spec: PathBuf,

        /// Output path for the generated harness file
        #[arg(long, default_value = "./tests/kani.rs")]
        output: PathBuf,
    },

    /// Generate unit tests from a qedspec (plain Rust, cargo test)
    Test {
        /// Path to the spec file (Spec.lean or .qedspec)
        #[arg(long)]
        spec: PathBuf,

        /// Output path for the generated test file
        #[arg(long, default_value = "./src/tests.rs")]
        output: PathBuf,
    },

    /// Generate transient proptest harnesses from a qedspec (property-based testing)
    Proptest {
        /// Path to the spec file (.qedspec)
        #[arg(long)]
        spec: PathBuf,

        /// Output path for the generated proptest file (transient, not committed)
        #[arg(long, default_value = "/tmp/proptest_harness.rs")]
        output: PathBuf,
    },

    /// Generate QuasarSVM integration test scaffolds from a qedspec
    #[command(name = "integration-test")]
    IntegrationTest {
        /// Path to the spec file (.qedspec)
        #[arg(long)]
        spec: PathBuf,

        /// Output path for the generated test file
        #[arg(long, default_value = "./src/integration_tests.rs")]
        output: PathBuf,
    },

    /// Generate a Lean 4 file from a .qedspec spec
    #[command(name = "lean-gen")]
    LeanGen {
        /// Path to the .qedspec spec file
        #[arg(long)]
        spec: PathBuf,

        /// Output path for the generated Lean file
        #[arg(long, default_value = "./formal_verification/Spec.lean")]
        output: PathBuf,
    },

    /// Generate a GitHub Actions workflow for formal verification CI
    Ci {
        /// Output path for the workflow file
        #[arg(long, default_value = ".github/workflows/verify.yml")]
        output: PathBuf,

        /// sBPF assembly source file (adds verify step to CI)
        #[arg(long)]
        asm: Option<String>,
    },

    /// Detect code drift in #[qed(verified)] functions
    Drift {
        /// Path to Rust source file or directory to scan
        #[arg(long)]
        input: PathBuf,

        /// Exit 1 on any drift (CI gate)
        #[arg(long)]
        strict: bool,

        /// Auto-update hashes in source files
        #[arg(long)]
        update: bool,
    },

    /// Aristotle theorem prover (Harmonic) — sorry-filling via long-running agent
    #[command(subcommand)]
    Aristotle(AristotleCommands),
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

fn format_lint_warning(warning: &check::CompletenessWarning) -> String {
    let icon = match warning.severity {
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
        out.push_str(&format!("\n      Pre-state:  {}  →  {} ✓",
            cx.pre_state.iter().map(|(f, v)| format!("{} = {}", f, v)).collect::<Vec<_>>().join(", "),
            cx.pre_check,
        ));
        out.push_str(&format!("\n      Apply:      {} ({})",
            cx.handler,
            cx.effects.join(", "),
        ));
        out.push_str(&format!("\n      Post-state: {}  →  {} {}",
            cx.post_state.iter().map(|(f, v)| format!("{} = {}", f, v)).collect::<Vec<_>>().join(", "),
            cx.post_check,
            if cx.invariant_holds { "✓" } else { "✗" },
        ));
    }
    if !warning.fix_options.is_empty() {
        out.push_str("\n    Fix options:");
        for (i, opt) in warning.fix_options.iter().enumerate() {
            let label = (b'A' + i as u8) as char;
            out.push_str(&format!("\n      {}) {} — {}", label, opt.label, opt.rationale));
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
            validate::setup_workspace(workspace.as_deref(), mathlib).await?;
        }

        Commands::Init {
            name,
            asm,
            mathlib,
            quasar,
            output_dir,
        } => {
            init::init(&name, &output_dir, asm.as_deref(), mathlib, quasar)?;

            if quasar {
                let spec_path = output_dir.join("Spec.lean");
                let program_dir = output_dir
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(format!("programs/{}", name));
                let kani_path = output_dir
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join("tests/kani.rs");

                // Generate Quasar program skeleton
                codegen::generate(&spec_path, &program_dir)?;

                // Generate Kani proof harnesses
                kani::generate(&spec_path, &kani_path)?;

                // Generate unit tests
                let test_path = program_dir.join("src/tests.rs");
                unit_test::generate(&spec_path, &test_path)?;
            }
        }

        Commands::Verify { asm, proofs } => {
            verify::verify(&asm, &proofs)?;
        }

        Commands::Check {
            spec,
            proofs,
            code,
            kani,
        } => {
            let spec_name = spec
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Spec".to_string());

            if code.is_some() || kani.is_some() {
                // Unified drift detection mode
                let report =
                    check::check_unified(&spec, &proofs, code.as_deref(), kani.as_deref())?;
                check::print_unified_report(&spec_name, &report);
                if report.issue_count() > 0 {
                    std::process::exit(1);
                }
            } else {
                // Lean-only mode (backward compatible)
                let results = check::check(&spec, &proofs)?;
                check::print_report(&spec_name, &results);
                let all_proven = results.iter().all(|r| r.status == check::Status::Proven);
                if !all_proven {
                    std::process::exit(1);
                }
            }
        }

        Commands::Explain {
            spec,
            proofs,
            output,
        } => {
            let report = explain::explain(&spec, &proofs)?;
            if let Some(ref path) = output {
                std::fs::write(path, &report)?;
                eprintln!("Wrote verification report to {}", path.display());
            } else {
                print!("{}", report);
            }
        }

        Commands::Lint { spec, json } => {
            let warnings = check::lint(&spec)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&warnings)?);
            } else {
                if warnings.is_empty() {
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
                        std::process::exit(1);
                    }
                }
            }
        }

        Commands::Coverage { spec, json } => {
            let parsed = check::parse_spec_file(&spec)?;
            let matrix = check::coverage_matrix(&parsed);
            if json {
                println!("{}", serde_json::to_string_pretty(&matrix)?);
            } else {
                check::print_coverage_table(&matrix);
            }
        }

        Commands::Codegen { spec, output_dir } => {
            codegen::generate(&spec, &output_dir)?;
        }

        Commands::Kani { spec, output } => {
            kani::generate(&spec, &output)?;
        }

        Commands::Test { spec, output } => {
            unit_test::generate(&spec, &output)?;
        }

        Commands::Proptest { spec, output } => {
            proptest_gen::generate(&spec, &output)?;
        }

        Commands::IntegrationTest { spec, output } => {
            integration_test::generate(&spec, &output)?;
        }

        Commands::LeanGen { spec, output } => {
            let parsed = check::parse_spec_file(&spec)?;
            lean_gen::generate(&parsed, &output)?;
        }

        Commands::Ci { output, asm } => {
            ci::generate_ci(&output, asm.as_deref())?;
        }

        Commands::Drift {
            input,
            strict,
            update,
        } => {
            if update {
                let count = drift::update(&input)?;
                eprintln!("Updated {} hash(es).", count);
            } else {
                let entries = drift::check(&input)?;
                drift::print_report(&entries);
                if strict {
                    let has_drift = entries
                        .iter()
                        .any(|e| !matches!(e.status, drift::DriftStatus::Ok));
                    if has_drift {
                        std::process::exit(1);
                    }
                }
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
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::format_lint_warning;
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
}
