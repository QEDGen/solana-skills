mod api;
mod aristotle;
mod asm2lean;
mod consolidate;
mod project;
mod spec;
mod validate;

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
    },

    /// Generate a draft SPEC.md from an Anchor IDL
    Spec {
        /// Path to Anchor IDL JSON file
        #[arg(long)]
        idl: PathBuf,

        /// Directory to write SPEC.md (default: ./formal_verification)
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

    /// Set up the global validation workspace (scaffold + Mathlib cache)
    Setup {
        /// Directory for the validation workspace (default: platform cache dir)
        #[arg(long)]
        workspace: Option<PathBuf>,
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
        }

        Commands::Spec { idl, output_dir } => {
            spec::generate_spec(&idl, &output_dir)?;
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

        Commands::Setup { workspace } => {
            validate::setup_workspace(workspace.as_deref()).await?;
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
