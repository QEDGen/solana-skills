mod api;
mod consolidate;
mod ir;
mod project;
mod prompt;
mod proof_plan;
mod validate;
mod workflow;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// CLI tool for generating formal Lean 4 proofs for Solana programs using Mistral's Leanstral model
#[derive(Parser)]
#[command(name = "leanstral")]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze a Solana/Anchor project and generate property candidates
    Analyze {
        /// Path to Anchor IDL JSON file
        #[arg(long)]
        idl: Option<PathBuf>,

        /// Path to Anchor program Rust source file
        #[arg(long)]
        input: Option<PathBuf>,

        /// Path to test files (can be specified multiple times)
        #[arg(long)]
        tests: Vec<PathBuf>,

        /// Directory to write analysis artifacts
        #[arg(long, default_value = "/tmp/leanstral-analysis")]
        output_dir: PathBuf,
    },

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

    /// Full pipeline: analyze + generate + validate (recommended)
    Verify {
        /// Path to Anchor IDL JSON file
        #[arg(long)]
        idl: Option<PathBuf>,

        /// Path to Anchor program Rust source file
        #[arg(long)]
        input: Option<PathBuf>,

        /// Path to test files (can be specified multiple times)
        #[arg(long)]
        tests: Vec<PathBuf>,

        /// Directory to write analysis artifacts
        #[arg(long, default_value = "/tmp/leanstral-analysis")]
        analysis_dir: PathBuf,

        /// Directory to write generated Lean projects
        #[arg(long, default_value = "/tmp/leanstral-solana-proofs")]
        output_dir: PathBuf,

        /// Number of independent completions per property
        #[arg(long, default_value = "3")]
        passes: usize,

        /// Sampling temperature
        #[arg(long, default_value = "0.2")]
        temperature: f64,

        /// Max tokens per completion
        #[arg(long)]
        max_tokens: Option<usize>,

        /// Number of top property candidates to generate proofs for
        #[arg(long, default_value = "3")]
        top_k: usize,

        /// Validate completions with 'lake build Best'
        #[arg(long)]
        validate: bool,

        /// Compiler-guided repair attempts after failed validation
        #[arg(long, default_value = "1")]
        repair_rounds: usize,

        /// Stop after analysis and ranking (no proof generation)
        #[arg(long)]
        analysis_only: bool,
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

    /// Set up the global validation workspace (scaffold + Mathlib cache)
    Setup {
        /// Directory for the validation workspace (default: platform cache dir)
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Analyze {
            idl,
            input,
            tests,
            output_dir,
        } => {
            if idl.is_none() && input.is_none() {
                anyhow::bail!("At least one of --idl or --input must be provided");
            }
            anchor_ir::analyze_project(
                idl.as_deref(),
                input.as_deref(),
                &tests,
                Some(&output_dir),
            ).map_err(|e| anyhow::anyhow!(e))?;
            println!("Analysis complete. Results written to {}", output_dir.display());
        }

        Commands::Generate {
            prompt_file,
            output_dir,
            passes,
            temperature,
            max_tokens,
            validate,
        } => {
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

        Commands::Verify {
            idl,
            input,
            tests,
            analysis_dir,
            output_dir,
            passes,
            temperature,
            max_tokens,
            top_k,
            validate,
            repair_rounds,
            analysis_only,
        } => {
            if idl.is_none() && input.is_none() {
                anyhow::bail!("At least one of --idl or --input must be provided");
            }
            workflow::run_full_pipeline(
                idl,
                input,
                tests,
                analysis_dir,
                output_dir,
                passes,
                temperature,
                max_tokens,
                top_k,
                validate,
                repair_rounds,
                analysis_only,
            )
            .await?;
        }

        Commands::Consolidate {
            input_dir,
            output_dir,
        } => {
            consolidate::consolidate_proofs(&input_dir, &output_dir)?;
        }

        Commands::Setup { workspace } => {
            validate::setup_workspace(workspace.as_deref()).await?;
        }
    }

    Ok(())
}
