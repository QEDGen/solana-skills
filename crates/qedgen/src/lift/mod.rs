//! `qedgen lift` (#450): success-path theorems for a built program's binary, with no
//! `.qedspec`. It takes the V0 `debug = 2` pair from `qedgen build`, asks a pinned qedsvm
//! checkout to read the concept map, discover the bindings, generate the Lean modules and
//! check them, then writes a report under `.qed/lift/<library>/`. It never writes
//! `.qed/verify-evidence.json`: the checks still use `native_decide` (qedsvm#98), which
//! `verify --strict` rejects.

pub(crate) mod qedsvm;
pub(crate) mod report;

use crate::verifiable_build;
use anyhow::{bail, Context, Result};
use qedsvm::QedsvmTools;
use report::{parse_axioms, AxiomSummary, InstructionReport, LiftReport};
use std::path::{Path, PathBuf};

pub const LIFT_DIR: &str = ".qed/lift";

pub struct LiftOptions {
    pub program: PathBuf,
    pub library: Option<String>,
    pub qedsvm: Option<PathBuf>,
    pub qedsvm_dev: bool,
    pub skip_build: bool,
    pub json: bool,
}

/// `label_vault` -> `LabelVault`: the Lean module directory and namespace.
pub fn module_prefix(library: &str) -> Result<String> {
    let prefix: String = library
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    if !prefix
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        || !prefix.chars().all(|c| c.is_ascii_alphanumeric())
    {
        bail!("library name {library:?} does not give a Lean module name");
    }
    Ok(prefix)
}

/// Run the lift on an existing build. Separate from [`run_lift`] so tests can stub qedsvm.
pub(crate) fn lift_with(
    workspace: &Path,
    library: &str,
    tools: &dyn QedsvmTools,
    qedsvm_rev: String,
) -> Result<LiftReport> {
    let record = verifiable_build::load_record(workspace, library)?.with_context(|| {
        format!("no build of {library} under .qed/build; run `qedgen build --arch v0` first")
    })?;
    if record.arch != "v0" {
        bail!(
            "the binary lift supports sBPF V0 only; this build is {} (the v3 port of the success-proof stack is pending)",
            record.arch
        );
    }
    let build = verifiable_build::build_dir(workspace, library);
    let so = build.join(format!("{library}.so"));
    let debug = build.join(format!("{library}.debug.so"));
    let out = workspace.join(LIFT_DIR).join(library);
    std::fs::create_dir_all(&out)?;
    let prefix = module_prefix(library)?;
    let concepts = out.join("concepts.json");
    let bindings_path = out.join("bindings.json");
    tools.concept_map(&so, &debug, &concepts)?;
    tools.discover(&so, &debug, &concepts, &bindings_path)?;
    tools.success_proofs(&bindings_path, &prefix, &so, &out)?;
    let bindings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&bindings_path)?)
            .context("parsing bindings.json")?;
    let coverage: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("coverage.json"))?)
            .context("parsing coverage.json")?;
    let edges = bindings["edges"].as_array().cloned().unwrap_or_default();
    let theorems: Vec<String> = edges
        .iter()
        .filter_map(|e| e["pc"].as_u64())
        .map(|pc| format!("Examples.{prefix}.readable{pc}"))
        .collect();
    let printed = tools.check_lean(&prefix, &out, &theorems)?;
    let checks = report::native_check_decls(&prefix, &bindings);
    let axioms = AxiomSummary::of(&theorems, &parse_axioms(&printed), &checks);
    let report = LiftReport {
        schema_version: 1,
        library: library.to_string(),
        program_hash: record.program_hash.clone(),
        commit: record.commit.clone(),
        qedsvm_rev,
        passed: !theorems.is_empty() && axioms.disallowed.is_empty() && axioms.missing.is_empty(),
        instructions: InstructionReport::of(&bindings, &coverage),
        gaps: report::gaps(&bindings, &coverage),
        theorems,
        coverage,
        axioms,
    };
    std::fs::write(
        out.join("report.json"),
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    std::fs::write(out.join("report.txt"), report.text())?;
    Ok(report)
}

pub fn run_lift(opts: &LiftOptions) -> Result<()> {
    let workspace = verifiable_build::preflight::workspace_root(&opts.program)?;
    let library = match &opts.library {
        Some(l) => l.clone(),
        None => verifiable_build::solana_verify::library_name(&opts.program)?,
    };
    if !opts.skip_build {
        verifiable_build::run_build(&verifiable_build::BuildOptions {
            program: opts.program.clone(),
            library: Some(library.clone()),
            arch: "v0".into(),
            tools_version: verifiable_build::DEFAULT_TOOLS_VERSION.into(),
        })?;
    }
    let (root, rev) = qedsvm::resolve_checkout(opts.qedsvm.as_deref(), opts.qedsvm_dev)?;
    let tools = qedsvm::RealTools { root };
    let report = lift_with(&workspace, &library, &tools, rev)?;
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.text());
    }
    if !report.passed {
        bail!(
            "the lift did not pass; see {}/{library}/report.txt",
            LIFT_DIR
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
