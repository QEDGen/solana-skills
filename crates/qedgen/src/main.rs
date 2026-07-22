mod adapt;
mod check;
mod cli;
mod codegen;
mod descriptor;
mod dispatch;
mod fs_walk;
mod mir;
mod obligations;
mod probe;
mod project;
mod run;
mod run_helpers;
mod spec;
mod verify;

// Root re-exports: the v2.35 src/ reorg moved flat modules into directory
// groups; these keep every pre-existing `crate::<module>` path valid. The
// `cli` types are re-exported here too so `crate::Target`
// (verify::regen_drift, codegen_shared::guards) still resolves after the
// v3.0 split that moved them into `cli.rs`.
#[cfg(test)]
pub(crate) use adapt::pinocchio_to_spec;
pub(crate) use adapt::{
    anchor_adapt, anchor_check, anchor_extractor, anchor_project, anchor_resolver,
    native_extractor, pinocchio_extractor, pinocchio_profile, program_model,
};
pub(crate) use cli::{AristotleCommands, Cli, Commands, CrucibleMode, Target};
pub(crate) use codegen::{
    asm2lean, banner, codegen_mir, codegen_shared, crucible_gen, fingerprint, integration_test,
    interface_gen, kani_impl, kani_mir, lean_gen_mir, lean_names, lean_sidecars, proptest_gen_mir,
    repro_gen, rust_codegen_util, unit_test,
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
    validate,
};
pub(crate) use spec::{
    ast, chumsky_adapter, chumsky_parser, idl, idl2spec, import_resolver, quantifier, spec_hash,
};
pub(crate) use verify::{
    drift, miri_verify, ratchet, regen_drift, sbpf_verify, upstream_check, verify_counterexample,
    verify_kani_parse, verify_probe_repros, verify_proptest_parse,
};

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let command_name = run::command_name_of(&cli.command).to_string();
    let cwd_for_capture = std::env::current_dir().ok();

    let result = run::dispatch(cli.command).await;

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
