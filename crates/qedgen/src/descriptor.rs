//! `qedgen descriptor` — emit a name-level refinement descriptor from a `.qedspec`.
//!
//! This is the PRODUCER half of the qedgen <-> qedsvm discharge seam. It lowers a single
//! constant-increment handler to the JSON obligation qedsvm's `qedlift` consumes via
//! `--descriptor` (schema: qedsvm `docs/REFINEMENT_DESCRIPTOR.md`).
//!
//! The descriptor is NAME-LEVEL: it carries which named field a handler mutates and by how
//! much, never byte offsets. Offsets are *shape*, owned by the IDL and resolved on the qedsvm
//! side. So qedgen never computes a layout here; it emits pure semantics derived from the spec.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::check::ParsedSpec;
use std::collections::BTreeSet;

mod lean_check;
mod outcome;
mod transition;

use lean_check::{check_modules, find_lake_project, LeanResult};
use outcome::{classify_lift, parse_outcome, DischargeReport, LeanCheck, LiftVerdict, Verdict};

/// Descriptor schema versions, kept in lockstep with qedsvm's `DESCRIPTOR_SCHEMA_MAX`.
/// A constant delta (`add_const`) is v1. A parameter delta (`add_param`) is v3: qedsvm binds
/// the parameter to its serialized instruction-data address, which needs the `input_layout`
/// (#404). qedsvm returns `unsupported / missing_parameter_binding` for a v2 parameter
/// descriptor, so qedgen no longer emits v2.
///
/// Scope note (#124): the whole-transition mode (qedsvm v0.9.0 `--transition`) consumes this
/// same shape. Paths, guards, and abort codes come from the discovered `.pcs` traces, not the
/// descriptor. Field offsets stay out of the producer: they are shape, owned by the IDL (inline
/// `layout` remains a hand-authored escape hatch for fixtures). The `input_layout` is
/// different: it is an explicit assumption about the serialized input (account data lengths
/// and the tracked account's index), supplied by the caller and printed in the descriptor.
const SCHEMA_VERSION_CONST: u32 = 1;
const SCHEMA_VERSION_PARAM: u32 = 3;
/// `--transition` without an input layout: the parameter only names a binder in the bundle,
/// not a bound instruction-data address, which is the form qedsvm's transition mode reads.
const SCHEMA_VERSION_PARAM_TRANSITION: u32 = 2;

/// `--account-data-lengths` / `--account-index`: the schema v3 input layout a parameter delta
/// needs. Both are explicit assumptions, never inferred from source.
#[derive(Debug, Clone, Default)]
pub(crate) struct InputLayoutFlags {
    pub account_data_lengths: Option<Vec<u64>>,
    pub account_index: Option<usize>,
}

/// Read a Codama IDL for name and account-index resolution.
pub(crate) fn load_idl(path: &Path) -> Result<serde_json::Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading IDL {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing IDL {}", path.display()))
}

impl<'a> DescriptorInputs<'a> {
    pub(crate) fn new(
        account: Option<String>,
        idl: Option<&'a serde_json::Value>,
        layout: &InputLayoutFlags,
    ) -> Self {
        DescriptorInputs {
            account,
            idl,
            account_data_lengths: layout.account_data_lengths.clone(),
            account_index: layout.account_index,
            transition: false,
        }
    }
}

/// Inputs to [`build_descriptor`] beyond the spec and handler name.
#[derive(Default)]
pub(crate) struct DescriptorInputs<'a> {
    /// Account name override (default: the spec's first account type, else the program name).
    pub account: Option<String>,
    /// The Codama IDL. For a parameter delta it resolves the IDL instruction and argument
    /// names (qedsvm matches them exactly) and the tracked account's index.
    pub idl: Option<&'a serde_json::Value>,
    /// Data length of every non-duplicate account the instruction receives, in order
    /// (`--account-data-lengths`). Required for a parameter delta.
    pub account_data_lengths: Option<Vec<u64>>,
    /// Index of the tracked account among those accounts (`--account-index`). Resolved from
    /// the IDL when omitted.
    pub account_index: Option<usize>,
    /// `--transition` mode: a parameter delta without layout flags keeps the unbound schema v2
    /// form, because the transition bundle uses the parameter only as a binder name.
    pub transition: bool,
}

/// Build the name-level descriptor for `handler` in `parsed`.
///
/// Requires the handler to have exactly one increment effect `<field> += <rhs>`, where `<rhs>`
/// is either an integer literal (constant delta, schema v1) or a declared parameter of the
/// handler (parameter delta, schema v3 with an `input_layout`). A non-`+=` op, multiple
/// effects, a missing handler, an RHS that is neither a literal nor a declared parameter, and a
/// parameter delta without a complete input layout are rejected with clear errors.
pub(crate) fn build_descriptor(
    parsed: &ParsedSpec,
    handler: &str,
    inputs: &DescriptorInputs,
) -> Result<serde_json::Value> {
    let h = parsed
        .handlers
        .iter()
        .find(|h| h.name == handler)
        .ok_or_else(|| {
            anyhow!(
                "handler `{}` not found (handlers: {})",
                handler,
                parsed
                    .handlers
                    .iter()
                    .map(|h| h.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;

    // Single-field increment: exactly one effect, op `add` (checked `+=`). The RHS is either
    // an integer literal (constant delta, v1) or a declared parameter (parameter delta, v2).
    let (field, op, value) = match h.effects.as_slice() {
        [one] => (&one.field, &one.op, &one.value),
        effects => bail!(
            "handler `{}` has {} effects; the descriptor seam supports exactly one \
             increment effect (`<field> += <int literal | parameter>`)",
            handler,
            effects.len()
        ),
    };
    if op != "add" {
        bail!(
            "handler `{}` effect on `{}` is `{}`, not a checked `+=`; the descriptor seam \
             supports only `<field> += <int literal | parameter>`",
            handler,
            field,
            op
        );
    }

    // `account` resolution: explicit override, else the spec's first account type, else the
    // program name. Use the IDL account name (the override) so qedsvm resolves the offsets.
    let account = inputs
        .account
        .clone()
        .or_else(|| parsed.account_types.first().map(|a| a.name.clone()))
        .unwrap_or_else(|| parsed.program_name.clone());

    // Constant delta (`+= k`) vs parameter delta (`+= amount`): an integer-literal RHS is a
    // constant (schema v1); otherwise the RHS must be a declared parameter of the handler
    // (schema v3). An RHS that is neither is rejected (the soundness boundary).
    match value.parse::<i64>() {
        Ok(delta) => {
            if inputs.account_data_lengths.is_some() || inputs.account_index.is_some() {
                eprintln!(
                    "note: --account-data-lengths / --account-index are ignored for `{handler}`: \
                     a constant delta needs no input layout"
                );
            }
            Ok(serde_json::json!({
                "schema_version": SCHEMA_VERSION_CONST,
                "account": account,
                "handler": handler,
                "mutated": field,
                "op": { "add_const": delta },
            }))
        }
        Err(_) => {
            if !h.takes_params.iter().any(|(p, _)| p == value) {
                bail!(
                    "handler `{}` increments `{}` by `{}`, which is neither an integer literal \
                     nor a declared parameter of `{}` (params: {})",
                    handler,
                    field,
                    value,
                    handler,
                    h.takes_params
                        .iter()
                        .map(|(p, _)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            parameter_descriptor(handler, value, field, &account, inputs)
        }
    }
}

/// Schema v3 parameter descriptor (#404). qedsvm binds the parameter to its serialized
/// instruction-data address, so the descriptor must name the IDL instruction and argument
/// exactly and carry the input layout. Everything is checked here, before qedlift runs.
fn parameter_descriptor(
    handler: &str,
    param: &str,
    field: &str,
    account: &str,
    inputs: &DescriptorInputs,
) -> Result<serde_json::Value> {
    if inputs.transition && inputs.account_data_lengths.is_none() && inputs.account_index.is_none()
    {
        return Ok(serde_json::json!({
            "schema_version": SCHEMA_VERSION_PARAM_TRANSITION,
            "account": account,
            "handler": handler,
            "mutated": field,
            "op": { "add_param": param },
        }));
    }
    let lengths = inputs.account_data_lengths.clone().ok_or_else(|| {
        anyhow!(
            "handler `{handler}` credits `{field}` by the parameter `{param}`, which needs a \
             schema v3 input layout: pass --account-data-lengths <n,...> (the data length of \
             every non-duplicate account the instruction receives, in order) and, without \
             --idl, --account-index <i>"
        )
    })?;
    if lengths.is_empty() {
        bail!("--account-data-lengths is empty; list one length per account");
    }

    let (account, ix_name, arg_name, idl_index) = match inputs.idl {
        Some(idl) => {
            // qedsvm resolves field offsets by the IDL account-type name, so emit its exact
            // spelling (`vault_account` in the spec, `vaultAccount` in Codama).
            let account = idl_account_type(idl, account)?;
            let ix = idl_instruction(idl, handler)?;
            let ix_name = ix["name"].as_str().unwrap_or(handler).to_string();
            let arg_name = idl_u64_argument(ix, &ix_name, param)?;
            let accounts = ix["accounts"].as_array().cloned().unwrap_or_default();
            if !accounts.is_empty() && accounts.len() != lengths.len() {
                bail!(
                    "--account-data-lengths lists {} account(s), but IDL instruction `{ix_name}` \
                     takes {}; give one length per account, in order",
                    lengths.len(),
                    accounts.len()
                );
            }
            let idl_index = unique_match(
                accounts
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| a["name"].as_str().is_some_and(|n| same_name(n, &account)))
                    .map(|(i, _)| i),
            );
            (account, ix_name, arg_name, idl_index)
        }
        None => {
            eprintln!(
                "note: no --idl; the descriptor uses the spec names `{handler}` / `{param}`, and \
                 qedsvm matches them exactly against the IDL"
            );
            (
                account.to_string(),
                handler.to_string(),
                param.to_string(),
                Err(0),
            )
        }
    };

    let index = match (inputs.account_index, idl_index) {
        // An explicit index must not contradict the IDL: the descriptor would name one
        // account and lay out another.
        (Some(i), Ok(from_idl)) if i != from_idl => bail!(
            "--account-index {i} contradicts the IDL, where `{account}` is instruction account \
             {from_idl}"
        ),
        (Some(i), _) => i,
        (None, Ok(i)) => i,
        (None, Err(count)) => bail!(
            "could not pick the tracked account `{account}`: {} (pass --account-index <i>)",
            if inputs.idl.is_none() {
                "no --idl to resolve it from".to_string()
            } else if count == 0 {
                "no instruction account has that name".to_string()
            } else {
                format!("{count} instruction accounts have that name")
            }
        ),
    };
    if index >= lengths.len() {
        bail!(
            "--account-index {index} is outside --account-data-lengths ({} account(s))",
            lengths.len()
        );
    }

    Ok(serde_json::json!({
        "schema_version": SCHEMA_VERSION_PARAM,
        "account": account,
        "handler": ix_name,
        "mutated": field,
        "op": { "add_param": arg_name },
        "input_layout": {
            "account_data_lengths": lengths,
            "account_index": index,
        },
    }))
}

/// Spec names are snake_case; Codama names are usually camelCase. Compare them without case
/// or underscores. A match must still be unique.
fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| *c != '_')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    norm(a) == norm(b)
}

/// The single element of `it`, else how many there were.
fn unique_match<T>(mut it: impl Iterator<Item = T>) -> std::result::Result<T, usize> {
    match (it.next(), it.next()) {
        (Some(one), None) => Ok(one),
        (None, _) => Err(0),
        (Some(_), Some(_)) => Err(2 + it.count()),
    }
}

/// The IDL spelling of the account type that matches `account`.
fn idl_account_type(idl: &serde_json::Value, account: &str) -> Result<String> {
    let program = idl.get("program").unwrap_or(idl);
    let names: Vec<&str> = program["accounts"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();
    let matches: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| same_name(n, account))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.to_string()),
        [] => bail!(
            "no IDL account type matches `{account}` (IDL accounts: {}); qedsvm resolves the \
             field offsets from it",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        ),
        many => bail!(
            "{} IDL account types match `{account}`: {}",
            many.len(),
            many.join(", ")
        ),
    }
}

/// The one Codama instruction that matches the spec handler.
fn idl_instruction<'v>(idl: &'v serde_json::Value, handler: &str) -> Result<&'v serde_json::Value> {
    let program = idl.get("program").unwrap_or(idl);
    let instructions = program["instructions"]
        .as_array()
        .ok_or_else(|| anyhow!("the IDL has no `instructions` (expected a Codama IDL)"))?;
    let matches: Vec<_> = instructions
        .iter()
        .filter(|i| i["name"].as_str().is_some_and(|n| same_name(n, handler)))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => bail!("no IDL instruction matches handler `{handler}`"),
        many => bail!(
            "{} IDL instructions match handler `{handler}`: {}",
            many.len(),
            many.iter()
                .filter_map(|i| i["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The IDL name of the argument that matches `param`. It must be a direct little-endian u64,
/// the only parameter shape qedsvm binds.
fn idl_u64_argument(ix: &serde_json::Value, ix_name: &str, param: &str) -> Result<String> {
    let args = ix["arguments"].as_array().cloned().unwrap_or_default();
    let matches: Vec<_> = args
        .iter()
        .filter(|a| a["name"].as_str().is_some_and(|n| same_name(n, param)))
        .collect();
    let arg = match matches.as_slice() {
        [one] => *one,
        [] => bail!("IDL instruction `{ix_name}` has no argument matching `{param}`"),
        many => bail!(
            "{} arguments of IDL instruction `{ix_name}` match `{param}`",
            many.len()
        ),
    };
    let ty = &arg["type"];
    if ty["kind"] != "numberTypeNode" || ty["format"] != "u64" || ty["endian"] != "le" {
        bail!(
            "argument `{}` of IDL instruction `{ix_name}` is not a little-endian u64; qedsvm \
             binds only u64 parameters",
            arg["name"].as_str().unwrap_or(param)
        );
    }
    Ok(arg["name"].as_str().unwrap_or(param).to_string())
}

// ════════════════════════════════════════════════════════════════
// Discharge driver: spec -> descriptor -> qedlift -> verdict (the one-command chain).
//
// qedgen shells out to qedsvm's `qedlift` binary. It reads qedlift's structured
// `refinement outcome` line, its exit status, and the emitted modules, then runs Lean on those
// modules before it reports `verified` (#406). It parses none of qedlift's internals.
// ════════════════════════════════════════════════════════════════

/// Assemble the `qedlift --descriptor ...` invocation. Factored out so the argument wiring is
/// unit-testable without a built qedlift on the path.
pub(crate) fn qedlift_command(
    qedlift: &Path,
    descriptor_json: &Path,
    so: &Path,
    idl: Option<&Path>,
    module: &str,
    output: &Path,
) -> Command {
    let mut c = Command::new(qedlift);
    c.arg("--so")
        .arg(so)
        .arg("--descriptor")
        .arg(descriptor_json)
        .arg("--module")
        .arg(module)
        .arg("--output")
        .arg(output);
    if let Some(idl) = idl {
        c.arg("--idl").arg(idl);
    }
    c
}

/// Assemble the `qedlift --transition ...` invocation (qedsvm #40 whole-
/// transition mode, v0.9.0). Same seam discipline as [`qedlift_command`]:
/// qedgen passes the name-level descriptor and where to write; trace
/// discovery (`<stem>_<path>.pcs` beside the `.so`), path lifting, and the
/// bundle theorem are qedlift's business.
pub(crate) fn qedlift_transition_command(
    qedlift: &Path,
    descriptor_json: &Path,
    so: &Path,
    idl: Option<&Path>,
    output_dir: &Path,
) -> Command {
    let mut c = Command::new(qedlift);
    c.arg("--so")
        .arg(so)
        .arg("--descriptor")
        .arg(descriptor_json)
        .arg("--transition")
        .arg("--output-dir")
        .arg(output_dir);
    if let Some(idl) = idl {
        c.arg("--idl").arg(idl);
    }
    c
}

/// PascalCase a name for the default Lean module (`vault` -> `Vault`, `increment` -> `Increment`).
fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut up = true;
    for ch in s.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            up = true;
        } else if up {
            out.extend(ch.to_uppercase());
            up = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Inputs for [`run_discharge`] beyond the parsed spec.
pub(crate) struct DischargeRequest<'a> {
    pub handler: &'a str,
    pub account: Option<String>,
    pub so: &'a Path,
    pub idl: Option<&'a Path>,
    pub qedlift: &'a Path,
    /// Lean module name (default `<Account><Handler>`).
    pub module: Option<String>,
    /// Persist the proof modules here when the verdict passes (A2a). `None` keeps the
    /// verdict-only behaviour.
    pub out_dir: Option<&'a Path>,
    /// Lake project for the Lean check. Default: the nearest Lake project at or above
    /// `out_dir`. With neither, no Lean check runs and the verdict is at most `emitted`.
    pub lean_project: Option<&'a Path>,
    /// Print the JSON report instead of the human report.
    pub json: bool,
    /// Input layout for a parameter delta (schema v3).
    pub layout: InputLayoutFlags,
}

/// Build the descriptor for the handler, discharge it against the `.so` via `qedlift`, and
/// run Lean on the emitted modules (#406). Prints one report (human or JSON) and returns an
/// error unless the verdict is `verified` or `emitted`.
///
/// qedlift writes into a fresh temp dir, so files left in `out_dir` by an earlier run never
/// count as this run's proof. Artifacts are copied into `out_dir` only when the verdict passes.
pub(crate) fn run_discharge(parsed: &ParsedSpec, req: &DischargeRequest) -> Result<()> {
    let handler = req.handler;
    let idl_json = req.idl.map(load_idl).transpose()?;
    let descriptor = build_descriptor(
        parsed,
        handler,
        &DescriptorInputs::new(req.account.clone(), idl_json.as_ref(), &req.layout),
    )?;
    let mutated = descriptor["mutated"].as_str().unwrap_or("?");
    // Constant (`add_const`) or parameter (`add_param`) credit, for the printed obligation.
    let delta_str = descriptor["op"]["add_const"]
        .as_i64()
        .map(|k| k.to_string())
        .or_else(|| {
            descriptor["op"]["add_param"]
                .as_str()
                .map(|p| p.to_string())
        })
        .unwrap_or_else(|| "?".to_string());
    let account_name = descriptor["account"].as_str().unwrap_or("?").to_string();
    let module = req
        .module
        .clone()
        .unwrap_or_else(|| format!("{}{}", pascal(&account_name), pascal(handler)));

    let work = tempfile::tempdir().context("create temp workdir for discharge")?;
    let desc_path = work.path().join("descriptor.json");
    std::fs::write(&desc_path, serde_json::to_string_pretty(&descriptor)?)
        .context("write temp descriptor")?;
    let lifted = work.path().join(format!("{}TracedLifted.lean", module));
    let refinement = work.path().join(format!("{}Refinement.lean", module));

    // A qedlift that cannot be launched is a `failed` report, not an early return, so
    // `--json` consumers always get a verdict.
    let launched = qedlift_command(req.qedlift, &desc_path, req.so, req.idl, &module, &lifted)
        .output()
        .map_err(|e| {
            format!(
                "could not run qedlift at {}: {} (build it with `cargo build \
                 -p qedlift --bin qedlift` in qedsvm-rs/)",
                req.qedlift.display(),
                e
            )
        });
    let stderr = match &launched {
        Ok(output) => String::from_utf8_lossy(&output.stderr).into_owned(),
        Err(_) => String::new(),
    };

    let artifacts_present = lifted.is_file() && refinement.is_file();
    let mut lift = match &launched {
        Err(message) => LiftVerdict::with(Verdict::Failed, "qedlift_not_runnable", message.clone()),
        Ok(output) => match parse_outcome(&stderr) {
            Ok(outcome) => {
                classify_lift(output.status.success(), outcome.as_ref(), artifacts_present)
            }
            Err(e) => LiftVerdict::with(Verdict::Failed, "malformed_outcome", format!("{e:#}")),
        },
    };
    if lift.verdict == Verdict::Failed
        && !matches!(
            lift.reason.as_deref(),
            Some("malformed_outcome" | "qedlift_not_runnable")
        )
    {
        // qedlift's own diagnostics explain a crash or a missing refinement.
        let detail = stderr_tail(&stderr);
        if !detail.is_empty() {
            let base = lift.message.take().unwrap_or_default();
            lift.message = Some(format!("{base}\n{detail}"));
        }
    }
    if lift.verdict == Verdict::Emitted {
        // Cheap guard for when no Lean check runs; Lean also reports `sorry`.
        let proof = std::fs::read_to_string(&refinement).unwrap_or_default();
        if proof.contains("sorry") {
            lift = LiftVerdict::with(
                Verdict::Failed,
                "sorry_in_refinement",
                format!("qedlift emitted a refinement containing `sorry` for `{handler}`"),
            );
        }
    }

    let mut report = DischargeReport::new(
        handler,
        format!("{}.{} += {}", account_name, mutated, delta_str),
        req.so.display().to_string(),
        req.qedlift.display().to_string(),
        lift,
    );

    if report.verdict == Verdict::Emitted {
        report.lean_check = match lean_project_for(req)? {
            None => LeanCheck::NotRun {
                why: "no Lake project; pass --lean-project, or put --out-dir inside a Lake \
                      project that requires qedsvm"
                    .to_string(),
            },
            Some(project) => {
                let shown = project.display().to_string();
                match check_modules(&project, &[lifted.clone(), refinement.clone()]) {
                    Ok(LeanResult::Passed) => {
                        report.verdict = Verdict::Verified;
                        LeanCheck::Passed { project: shown }
                    }
                    Ok(LeanResult::Failed(output)) => {
                        report.verdict = Verdict::Failed;
                        report.reason = Some("lean_check_failed".to_string());
                        LeanCheck::Failed {
                            project: shown,
                            output,
                        }
                    }
                    Err(e) => {
                        report.verdict = Verdict::Failed;
                        report.reason = Some("lean_check_failed".to_string());
                        LeanCheck::Failed {
                            project: shown,
                            output: format!("{e:#}"),
                        }
                    }
                }
            }
        };
    }

    if report.verdict.passes() {
        if let Some(dest) = req.out_dir {
            // The refinement imports `Generated.<Module>TracedLifted`, so both modules go
            // under `<out-dir>/Generated/`, the same layout the Lean check compiled.
            match persist_discharge_artifacts(&dest.join("Generated"), &lifted, &refinement) {
                Ok((lifted_dst, refinement_dst)) => {
                    report.artifacts = vec![
                        refinement_dst.display().to_string(),
                        lifted_dst.display().to_string(),
                    ];
                }
                Err(e) => {
                    report.verdict = Verdict::Failed;
                    report.reason = Some("persist_failed".to_string());
                    report.message = Some(format!("{e:#}"));
                }
            }
        }
    }

    if req.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_human());
        if report.verdict.passes() && !report.artifacts.is_empty() {
            println!(
                "    wire it in   : `import Generated.{module}Refinement`. Add \
                 `Generated.{module}TracedLifted` and"
            );
            println!(
                "                   `Generated.{module}Refinement` to a lean_lib whose source \
                 root is --out-dir; the"
            );
            println!(
                "                   project must `require qedsvm` (lean_solana projects already \
                 do)."
            );
        }
    }

    if report.verdict.passes() {
        Ok(())
    } else {
        bail!(
            "discharge verdict for `{}`: {}",
            handler,
            report.verdict.label()
        )
    }
}

/// The Lake project for the Lean check: `--lean-project`, else the nearest Lake project at or
/// above `--out-dir`.
fn lean_project_for(req: &DischargeRequest) -> Result<Option<PathBuf>> {
    if let Some(p) = req.lean_project {
        return Ok(Some(p.to_path_buf()));
    }
    let Some(out_dir) = req.out_dir else {
        return Ok(None);
    };
    let abs = if out_dir.is_absolute() {
        out_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .context("reading the current directory")?
            .join(out_dir)
    };
    Ok(find_lake_project(&abs))
}

/// Whole-transition discharge (qedsvm #40; hardened in #405): build the descriptor, drive
/// `qedlift --transition` into a fresh temp dir, build one row per path (spec-expected paths
/// reconciled against the discovered `<stem>_<label>.pcs` traces), and run Lean on every
/// emitted module before anything is `verified`. See [`transition`] for the verdict rules.
///
/// Stale modules in `--out-dir` never count: qedlift writes into the temp dir, and the modules
/// are copied into `<out-dir>/Generated/` only when the verdict passes.
pub(crate) fn run_discharge_transition(parsed: &ParsedSpec, req: &DischargeRequest) -> Result<()> {
    let handler = req.handler;
    let idl_json = req.idl.map(load_idl).transpose()?;
    let descriptor = build_descriptor(
        parsed,
        handler,
        &DescriptorInputs {
            transition: true,
            ..DescriptorInputs::new(req.account.clone(), idl_json.as_ref(), &req.layout)
        },
    )?;
    let spec_handler = parsed
        .handlers
        .iter()
        .find(|h| h.name == handler)
        .ok_or_else(|| anyhow!("handler `{handler}` not found"))?;
    let tracked = format!(
        "{}.{}",
        descriptor["account"].as_str().unwrap_or("?"),
        descriptor["mutated"].as_str().unwrap_or("?")
    );
    let mut report = transition::new_report(handler, tracked, req.so, req.qedlift)?;
    let expected = transition::expected_labels(spec_handler);
    let traced = transition::traced_labels(req.so);

    let work = tempfile::tempdir().context("create temp workdir for discharge")?;
    let desc_path = work.path().join("descriptor.json");
    std::fs::write(&desc_path, serde_json::to_string_pretty(&descriptor)?)
        .context("write temp descriptor")?;
    let out = work.path().join("out");
    std::fs::create_dir_all(&out)?;

    let launched = qedlift_transition_command(req.qedlift, &desc_path, req.so, req.idl, &out)
        .output()
        .map_err(|e| {
            format!(
                "could not run qedlift at {}: {} (build it with `cargo build \
                 -p qedlift --bin qedlift` in qedsvm-rs/)",
                req.qedlift.display(),
                e
            )
        });

    match launched {
        Err(message) => {
            report.paths = transition::path_rows(&expected, &traced, None, &BTreeSet::new())?;
            report.reason = Some("qedlift_not_runnable".to_string());
            report.message = Some(message);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            discharge_transition_run(&mut report, req, &expected, &traced, &output, &stderr, &out);
        }
    }

    // An unbound parameter (schema v2, no input layout) names the binder but was never tied to
    // the instruction's serialized argument, so the transition cannot be `verified` (or pass
    // as `emitted`), and no path counts as verified. The bound v3 form
    // (`--account-data-lengths`) can verify.
    let unbound_param =
        descriptor["op"].get("add_param").is_some() && descriptor.get("input_layout").is_none();
    if unbound_param {
        report.all_discovered_paths_verified = false;
        report.all_expected_paths_verified = false;
    }
    if unbound_param && matches!(report.verdict, Verdict::Verified | Verdict::Emitted) {
        report.verdict = Verdict::Incomplete;
        report.reason = Some("parameter_unbound".to_string());
        report.message = Some(format!(
            "`{}` is not bound to its serialized instruction-data address; pass \
             --account-data-lengths (and --idl or --account-index) to bind it",
            descriptor["op"]["add_param"].as_str().unwrap_or("?")
        ));
    }

    if report.verdict.passes() {
        if let (Some(dest), Some(bundle)) = (req.out_dir, report.bundle.clone()) {
            let modules = transition::emitted_modules(&out);
            match transition::publish_modules(&dest.join("Generated"), &bundle, &modules) {
                Ok(paths) => {
                    report.artifacts = paths.iter().map(|p| p.display().to_string()).collect()
                }
                Err(e) => {
                    report.verdict = Verdict::Failed;
                    report.reason = Some("persist_failed".to_string());
                    report.message = Some(format!("{e:#}"));
                }
            }
        }
    }

    if req.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_human());
    }
    if report.verdict.passes() {
        Ok(())
    } else {
        bail!(
            "discharge --transition verdict for `{}`: {}",
            handler,
            report.verdict.label()
        )
    }
}

/// Classify one `qedlift --transition` run into `report` (verdict, rows, Lean check).
fn discharge_transition_run(
    report: &mut transition::TransitionReport,
    req: &DischargeRequest,
    expected: &BTreeSet<String>,
    traced: &BTreeSet<String>,
    output: &std::process::Output,
    stderr: &str,
    out: &Path,
) {
    let fail = |report: &mut transition::TransitionReport, verdict, reason: &str, msg: String| {
        report.verdict = verdict;
        report.reason = Some(reason.to_string());
        report.message = Some(msg);
    };
    let outcome = match transition::parse_transition_outcome(stderr) {
        Ok(o) => o,
        Err(e) => {
            return fail(
                report,
                Verdict::Failed,
                "malformed_outcome",
                format!("{e:#}"),
            )
        }
    };
    let modules = transition::emitted_modules(out);
    let stems = transition::module_stems(&modules);
    report.paths = match transition::path_rows(expected, traced, outcome.as_ref(), &stems) {
        Ok(rows) => rows,
        Err(e) => {
            return fail(
                report,
                Verdict::Failed,
                "malformed_outcome",
                format!("{e:#}"),
            )
        }
    };
    if let Some(o) = outcome.as_ref().filter(|o| o.status != "emitted") {
        let verdict = match o.status.as_str() {
            "rejected" => Verdict::Rejected,
            "unsupported" => Verdict::Unsupported,
            _ => Verdict::Failed,
        };
        let reason = o.reason.clone().unwrap_or_else(|| o.status.clone());
        return fail(
            report,
            verdict,
            &reason,
            o.message.clone().unwrap_or_default(),
        );
    }
    if !output.status.success() {
        return fail(
            report,
            Verdict::Failed,
            "qedlift_failed",
            format!(
                "qedlift --transition failed ({})\n{}",
                output.status,
                stderr_tail(stderr)
            ),
        );
    }
    let stem = req
        .so
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("program");
    let bundle = outcome
        .as_ref()
        .and_then(|o| o.bundle.clone())
        .unwrap_or_else(|| format!("{}Transition", pascal(stem)));
    if !out.join(format!("{bundle}.lean")).is_file() {
        return fail(
            report,
            Verdict::Failed,
            "no_bundle",
            format!(
                "qedlift ran but wrote no `{bundle}.lean` (are there >= 2 `{stem}_<label>.pcs` \
                 traces beside the .so?)\n{}",
                stderr_tail(stderr)
            ),
        );
    }
    report.bundle = Some(bundle.clone());
    if let Some(m) = modules.iter().find(|m| {
        std::fs::read_to_string(m)
            .map(|t| t.contains("sorry"))
            .unwrap_or(false)
    }) {
        return fail(
            report,
            Verdict::Failed,
            "sorry_in_module",
            format!("qedlift emitted {} containing `sorry`", m.display()),
        );
    }

    let project = match lean_project_for(req) {
        Ok(p) => p,
        Err(e) => {
            return fail(
                report,
                Verdict::Failed,
                "lean_check_failed",
                format!("{e:#}"),
            )
        }
    };
    report.lean_check = transition::lean_check(project, &modules);
    let (verdict, reason) = transition::overall_verdict(&report.paths, &report.lean_check);
    report.verdict = verdict;
    report.reason = reason;
    let lean_ok = matches!(report.lean_check, LeanCheck::Passed { .. });
    let lifted = |r: &transition::PathRow| r.status == transition::PathStatus::Lifted;
    report.all_discovered_paths_verified =
        lean_ok && report.paths.iter().filter(|r| r.traced).all(lifted);
    report.all_expected_paths_verified =
        lean_ok && report.paths.iter().filter(|r| r.expected).all(lifted);
}

/// Copy the qedlift artifacts out of the throwaway workdir into `dest` (created if needed), so
/// the discharge proof lives in the project rather than vanishing with the temp dir. Returns the
/// persisted `(lifted, refinement)` paths. Files are overwritten — re-discharging the same
/// handler refreshes the proof in place.
fn persist_discharge_artifacts(
    dest: &Path,
    lifted: &Path,
    refinement: &Path,
) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dest)
        .with_context(|| format!("creating discharge out-dir {}", dest.display()))?;
    let copy_into = |src: &Path| -> Result<PathBuf> {
        let name = src
            .file_name()
            .ok_or_else(|| anyhow!("artifact path has no file name: {}", src.display()))?;
        let dst = dest.join(name);
        std::fs::copy(src, &dst)
            .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
        Ok(dst)
    };
    let lifted_dst = copy_into(lifted)?;
    let refinement_dst = copy_into(refinement)?;
    Ok((lifted_dst, refinement_dst))
}

/// Last few lines of qedlift stderr (it dumps the decoded instruction list, which is noise here).
fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| !l.trim_start().starts_with("pc=") && !l.contains("decoded insns"))
        .collect();
    let n = lines.len().min(12);
    lines[lines.len() - n..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(path: &str) -> ParsedSpec {
        crate::check::parse_spec_file(std::path::Path::new(path))
            .unwrap_or_else(|e| panic!("parse {}: {}", path, e))
    }

    /// The canonical name-level case: a vault `increment` handler doing `total += 1` lowers to
    /// the exact descriptor qedsvm's `vault.descriptor.json` carries (account `vault`, mutated
    /// `total`, op add_const 1) — the one that discharges to a sorry-free proof byte-identical
    /// to the registry-driven VaultRefinement.lean.
    #[test]
    fn vault_increment_emits_name_level_descriptor() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let d = build_descriptor(
            &parsed,
            "increment",
            &DescriptorInputs {
                account: Some("vault".to_string()),
                ..Default::default()
            },
        )
        .expect("build vault descriptor");
        assert_eq!(
            d,
            serde_json::json!({
                "schema_version": 1,
                "account": "vault",
                "handler": "increment",
                "mutated": "total",
                "op": { "add_const": 1 }
            })
        );
    }

    /// An in-repo-style counter spec: `counter += 1` inside the `Active` variant lowers the
    /// same way. (counter.so has no IDL, so qedsvm uses the inline-layout fallback; the
    /// producer still emits the same name-level semantics.)
    #[test]
    fn counter_increment_emits_descriptor() {
        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        let d = build_descriptor(
            &parsed,
            "increment",
            &DescriptorInputs {
                account: Some("Counter".to_string()),
                ..Default::default()
            },
        )
        .expect("build counter descriptor");
        assert_eq!(
            d,
            serde_json::json!({
                "schema_version": 1,
                "account": "Counter",
                "handler": "increment",
                "mutated": "counter",
                "op": { "add_const": 1 }
            })
        );
    }

    fn vault_idl() -> serde_json::Value {
        load_idl(Path::new("tests/fixtures/descriptor/vault.codama.json")).expect("vault IDL")
    }

    fn param_inputs<'a>(
        idl: Option<&'a serde_json::Value>,
        lengths: Option<Vec<u64>>,
        index: Option<usize>,
    ) -> DescriptorInputs<'a> {
        DescriptorInputs {
            account: Some("vault".to_string()),
            idl,
            account_data_lengths: lengths,
            account_index: index,
            transition: false,
        }
    }

    /// A parameter delta (`total += amount`) emits a schema v3 `add_param` descriptor with the
    /// input layout. With the IDL, the account index is resolved from the instruction's account
    /// list. This is exactly qedsvm's `vault_deposit.descriptor.json` (#404).
    #[test]
    fn parameter_delta_emits_schema_v3_with_input_layout() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let idl = vault_idl();
        let d = build_descriptor(
            &parsed,
            "deposit",
            &param_inputs(Some(&idl), Some(vec![41]), None),
        )
        .expect("build deposit (parameter) descriptor");
        assert_eq!(
            d,
            serde_json::json!({
                "schema_version": 3,
                "account": "vault",
                "handler": "deposit",
                "mutated": "total",
                "op": { "add_param": "amount" },
                "input_layout": { "account_data_lengths": [41], "account_index": 0 }
            })
        );
    }

    /// A parameter delta without a complete input layout fails before qedlift runs.
    #[test]
    fn parameter_delta_needs_an_input_layout() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let idl = vault_idl();
        let cases: [(DescriptorInputs, &str); 5] = [
            (
                param_inputs(Some(&idl), None, None),
                "--account-data-lengths",
            ),
            (param_inputs(Some(&idl), Some(vec![]), None), "empty"),
            (param_inputs(None, Some(vec![41]), None), "--account-index"),
            (param_inputs(Some(&idl), Some(vec![41, 0]), None), "takes 1"),
            (param_inputs(None, Some(vec![41]), Some(1)), "outside"),
        ];
        for (inputs, want) in cases {
            let err = build_descriptor(&parsed, "deposit", &inputs).expect_err(want);
            assert!(err.to_string().contains(want), "want `{want}`, got: {err}");
        }
    }

    /// Without an IDL, an explicit index is enough and the spec names are used as is.
    #[test]
    fn parameter_delta_without_idl_uses_spec_names() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let d = build_descriptor(
            &parsed,
            "deposit",
            &param_inputs(None, Some(vec![41]), Some(0)),
        )
        .expect("explicit layout without IDL");
        assert_eq!(d["schema_version"], 3);
        assert_eq!(d["handler"], "deposit");
        assert_eq!(d["input_layout"]["account_index"], 0);
    }

    /// Codama names are camelCase. qedsvm matches names exactly, so the descriptor carries the
    /// IDL's names. A missing, ambiguous, or non-u64 match fails clearly.
    #[test]
    fn parameter_delta_resolves_idl_names() {
        let mut parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        if let Some(h) = parsed.handlers.iter_mut().find(|h| h.name == "deposit") {
            h.name = "deposit_funds".to_string();
            h.effects = vec![crate::check::ParsedEffect::from_triple(
                "total",
                "add",
                "min_amount",
            )];
            h.takes_params = vec![("min_amount".to_string(), "U64".to_string())];
        }
        let idl = |arg_ty: &str, extra_ix: bool| {
            let mut ixs = vec![serde_json::json!({
                "name": "depositFunds",
                "accounts": [{ "name": "owner" }, { "name": "vaultAccount" }],
                "arguments": [{ "name": "minAmount", "type": serde_json::from_str::<serde_json::Value>(arg_ty).unwrap() }]
            })];
            if extra_ix {
                ixs.push(serde_json::json!({ "name": "deposit_funds", "arguments": [] }));
            }
            serde_json::json!({
                "program": { "accounts": [{ "name": "vaultAccount" }], "instructions": ixs }
            })
        };
        let u64_le = r#"{"kind":"numberTypeNode","format":"u64","endian":"le"}"#;
        // The spec spells the account `vault_account`; Codama spells it `vaultAccount`.
        let inputs = |idl, lengths, index| DescriptorInputs {
            account: Some("vault_account".to_string()),
            ..param_inputs(idl, lengths, index)
        };

        let ok = idl(u64_le, false);
        let d = build_descriptor(
            &parsed,
            "deposit_funds",
            &inputs(Some(&ok), Some(vec![0, 41]), None),
        )
        .expect("camelCase IDL resolves");
        assert_eq!(
            d["account"], "vaultAccount",
            "account uses the IDL spelling"
        );
        assert_eq!(d["handler"], "depositFunds");
        assert_eq!(d["op"]["add_param"], "minAmount");
        assert_eq!(d["input_layout"]["account_index"], 1);

        // An explicit index that agrees with the IDL is fine; one that contradicts it is not.
        build_descriptor(
            &parsed,
            "deposit_funds",
            &inputs(Some(&ok), Some(vec![0, 41]), Some(1)),
        )
        .expect("agreeing index");
        let err = build_descriptor(
            &parsed,
            "deposit_funds",
            &inputs(Some(&ok), Some(vec![0, 41]), Some(0)),
        )
        .expect_err("conflicting index");
        assert!(err.to_string().contains("contradicts the IDL"), "{err}");

        // No IDL account type with that name: qedsvm could not resolve the offsets.
        let err = build_descriptor(
            &parsed,
            "deposit_funds",
            &DescriptorInputs {
                account: Some("treasury".to_string()),
                ..param_inputs(Some(&ok), Some(vec![0, 41]), Some(1))
            },
        )
        .expect_err("unknown account type");
        assert!(
            err.to_string().contains("no IDL account type matches"),
            "{err}"
        );

        let ambiguous = idl(u64_le, true);
        let err = build_descriptor(
            &parsed,
            "deposit_funds",
            &inputs(Some(&ambiguous), Some(vec![0, 41]), None),
        )
        .expect_err("two instructions match");
        assert!(
            err.to_string().contains("2 IDL instructions match"),
            "{err}"
        );

        let u32_arg = idl(
            r#"{"kind":"numberTypeNode","format":"u32","endian":"le"}"#,
            false,
        );
        let err = build_descriptor(
            &parsed,
            "deposit_funds",
            &inputs(Some(&u32_arg), Some(vec![0, 41]), None),
        )
        .expect_err("non-u64 argument");
        assert!(err.to_string().contains("not a little-endian u64"), "{err}");
    }

    /// `--transition` keeps the unbound v2 parameter form without layout flags (the bundle uses
    /// the parameter as a binder only), and emits v3 when flags are given.
    #[test]
    fn transition_parameter_delta_is_v2_without_layout() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let d = build_descriptor(
            &parsed,
            "deposit",
            &DescriptorInputs {
                transition: true,
                ..param_inputs(None, None, None)
            },
        )
        .expect("transition without layout");
        assert_eq!(
            d,
            serde_json::json!({
                "schema_version": 2,
                "account": "vault",
                "handler": "deposit",
                "mutated": "total",
                "op": { "add_param": "amount" }
            })
        );
        let idl = vault_idl();
        let d = build_descriptor(
            &parsed,
            "deposit",
            &DescriptorInputs {
                transition: true,
                ..param_inputs(Some(&idl), Some(vec![41]), None)
            },
        )
        .expect("transition with layout");
        assert_eq!(d["schema_version"], 3);
    }

    /// Layout flags on a constant delta are ignored, and the descriptor stays byte-compatible.
    #[test]
    fn constant_delta_ignores_layout_flags() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let d = build_descriptor(
            &parsed,
            "increment",
            &param_inputs(None, Some(vec![41]), Some(0)),
        )
        .expect("constant delta");
        assert_eq!(
            d,
            serde_json::json!({
                "schema_version": 1,
                "account": "vault",
                "handler": "increment",
                "mutated": "total",
                "op": { "add_const": 1 }
            })
        );
    }

    /// An RHS that is neither an integer literal nor a declared parameter is rejected (the
    /// soundness boundary): the producer must not emit a credit it cannot name.
    #[test]
    fn unknown_rhs_is_rejected() {
        let mut parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        // Rewrite deposit's effect to credit by an undeclared symbol.
        if let Some(h) = parsed.handlers.iter_mut().find(|h| h.name == "deposit") {
            h.effects = vec![crate::check::ParsedEffect::from_triple(
                "total", "add", "mystery",
            )];
            h.takes_params.clear();
        }
        let err = build_descriptor(
            &parsed,
            "deposit",
            &DescriptorInputs {
                account: Some("vault".to_string()),
                ..Default::default()
            },
        )
        .expect_err("an undeclared RHS must be rejected");
        assert!(
            err.to_string()
                .contains("neither an integer literal nor a declared parameter"),
            "error should explain the unknown RHS, got: {err}"
        );
    }

    /// A missing handler is a clear error listing the available handlers.
    #[test]
    fn unknown_handler_is_rejected() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let err = build_descriptor(&parsed, "nope", &DescriptorInputs::default())
            .expect_err("unknown handler");
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    /// The qedlift invocation is assembled with the expected flags (with and without `--idl`).
    #[test]
    fn qedlift_command_is_assembled_correctly() {
        let c = qedlift_command(
            Path::new("/bin/qedlift"),
            Path::new("/tmp/d.json"),
            Path::new("/p/vault.so"),
            Some(Path::new("/p/vault.codama.json")),
            "VaultDescriptor",
            Path::new("/tmp/VaultDescriptorTracedLifted.lean"),
        );
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(c.get_program().to_string_lossy(), "/bin/qedlift");
        assert_eq!(
            args,
            vec![
                "--so",
                "/p/vault.so",
                "--descriptor",
                "/tmp/d.json",
                "--module",
                "VaultDescriptor",
                "--output",
                "/tmp/VaultDescriptorTracedLifted.lean",
                "--idl",
                "/p/vault.codama.json",
            ]
        );

        // No IDL -> no --idl flag (inline-layout / no-IDL programs).
        let c2 = qedlift_command(
            Path::new("/bin/qedlift"),
            Path::new("/tmp/d.json"),
            Path::new("/p/counter.so"),
            None,
            "CounterDescriptor",
            Path::new("/tmp/CounterDescriptorTracedLifted.lean"),
        );
        assert!(
            !c2.get_args().any(|a| a == "--idl"),
            "no IDL should omit the --idl flag"
        );
    }

    // ------------------------------------------------------------------------
    // A2a — persist discharged artifacts into the project
    // ------------------------------------------------------------------------

    /// The persist helper copies both qedlift artifacts into `dest`, creating it
    /// (including missing parents) and preserving contents.
    #[test]
    fn persist_copies_both_artifacts_into_dest() {
        let src = tempfile::tempdir().expect("src tempdir");
        let lifted = src.path().join("FooTracedLifted.lean");
        let refinement = src.path().join("FooRefinement.lean");
        std::fs::write(&lifted, "lifted-body").unwrap();
        std::fs::write(&refinement, "refinement-body").unwrap();
        let dest = tempfile::tempdir().expect("dest tempdir");
        // A nested, not-yet-created subdir exercises create_dir_all.
        let nested = dest.path().join("Generated");

        let (l, r) = persist_discharge_artifacts(&nested, &lifted, &refinement).expect("persist");
        assert_eq!(l, nested.join("FooTracedLifted.lean"));
        assert_eq!(r, nested.join("FooRefinement.lean"));
        assert_eq!(std::fs::read_to_string(&l).unwrap(), "lifted-body");
        assert_eq!(std::fs::read_to_string(&r).unwrap(), "refinement-body");
    }

    /// `--out-dir` end-to-end through `run_discharge` with a fake qedlift: the
    /// persisted proof lands in the out-dir (not a temp dir that vanishes).
    #[cfg(unix)]
    #[test]
    fn discharge_persists_artifacts_to_out_dir() {
        use std::os::unix::fs::PermissionsExt;
        // Fake qedlift: emit a sorry-free refinement + lifted module so the
        // success branch fires, parsing only the args run_discharge passes.
        const FAKE: &str = "#!/bin/sh\nout=\"\"; mod=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output) out=\"$2\"; shift 2 ;;\n    --module) mod=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ndir=$(dirname \"$out\")\nprintf 'theorem lifted : True := trivial\\n' > \"$out\"\nprintf 'theorem refines_asm : True := trivial\\n' > \"$dir/${mod}Refinement.lean\"\n";

        let tmp = tempfile::tempdir().expect("tempdir");
        let fake = tmp.path().join("fake-qedlift.sh");
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let so = tmp.path().join("counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        let out = tmp.path().join("project");

        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        run_discharge(&parsed, &request(&so, &fake, Some(&out), None)).expect("discharge persists");

        // Module defaults to `<Account><Handler>` = CounterIncrement, persisted under
        // `Generated/` so `import Generated.CounterIncrementRefinement` resolves.
        assert!(
            out.join("Generated/CounterIncrementRefinement.lean")
                .exists(),
            "refinement persisted into out-dir/Generated",
        );
        assert!(
            out.join("Generated/CounterIncrementTracedLifted.lean")
                .exists(),
            "lifted module persisted into out-dir/Generated",
        );
    }

    fn request<'a>(
        so: &'a Path,
        qedlift: &'a Path,
        out_dir: Option<&'a Path>,
        lean_project: Option<&'a Path>,
    ) -> DischargeRequest<'a> {
        DischargeRequest {
            handler: "increment",
            account: Some("Counter".to_string()),
            so,
            idl: None,
            qedlift,
            module: None,
            out_dir,
            lean_project,
            json: false,
            layout: InputLayoutFlags::default(),
        }
    }

    /// Write an executable fake qedlift and a dummy `.so` into `dir`.
    #[cfg(unix)]
    fn fake_qedlift(dir: &Path, script: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let fake = dir.join("fake-qedlift.sh");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let so = dir.join("counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        (fake, so)
    }

    /// A structured `rejected` or `unsupported` outcome fails the command, keeps the typed
    /// reason in the error path, and persists nothing.
    #[cfg(unix)]
    #[test]
    fn rejected_and_unsupported_outcomes_fail_without_persisting() {
        for (status, reason) in [
            ("rejected", "mutation_mismatch"),
            ("unsupported", "missing_parameter_binding"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let script = format!(
                "#!/bin/sh\necho 'refinement outcome: {{\"status\":\"{status}\",\"reason\":\"{reason}\",\"message\":\"m\"}}' >&2\nexit 1\n"
            );
            let (fake, so) = fake_qedlift(tmp.path(), &script);
            let out = tmp.path().join("project");
            let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
            let err = run_discharge(&parsed, &request(&so, &fake, Some(&out), None))
                .expect_err("non-emitted outcome must fail");
            assert!(err.to_string().contains(status), "{status}: {err}");
            assert!(!out.exists(), "{status}: nothing persisted");
        }
    }

    /// A proof left in `--out-dir` by an earlier run cannot make a failed run pass: qedlift
    /// writes to a fresh temp dir, and the old file is neither read nor replaced.
    #[cfg(unix)]
    #[test]
    fn stale_out_dir_artifacts_cannot_pass_a_failed_run() {
        let tmp = tempfile::tempdir().unwrap();
        let (fake, so) = fake_qedlift(tmp.path(), "#!/bin/sh\necho boom >&2\nexit 3\n");
        let out = tmp.path().join("project");
        std::fs::create_dir_all(out.join("Generated")).unwrap();
        let stale = out.join("Generated/CounterIncrementRefinement.lean");
        std::fs::write(&stale, "theorem old : True := trivial\n").unwrap();
        std::fs::write(
            out.join("Generated/CounterIncrementTracedLifted.lean"),
            "theorem old_lift : True := trivial\n",
        )
        .unwrap();

        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        let err = run_discharge(&parsed, &request(&so, &fake, Some(&out), None))
            .expect_err("failed qedlift must fail even with stale proofs present");
        assert!(err.to_string().contains("failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&stale).unwrap(),
            "theorem old : True := trivial\n",
            "stale proof untouched"
        );
    }

    /// A qedlift that cannot be launched produces a `failed` verdict (so `--json` still prints a
    /// report), not an early error.
    #[test]
    fn unlaunchable_qedlift_is_a_failed_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let so = tmp.path().join("counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        let missing = tmp.path().join("no-such-qedlift");
        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        let err = run_discharge(&parsed, &request(&so, &missing, None, None))
            .expect_err("a missing qedlift must fail");
        assert!(
            err.to_string().contains("discharge verdict") && err.to_string().contains("failed"),
            "{err}"
        );
    }

    /// A copy into `--out-dir` that fails is a `failed` verdict, not an early error.
    #[cfg(unix)]
    #[test]
    fn persist_failure_is_a_failed_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\nout=\"\"; mod=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output) out=\"$2\"; shift 2 ;;\n    --module) mod=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ndir=$(dirname \"$out\")\nprintf 'theorem lifted : True := trivial\\n' > \"$out\"\nprintf 'theorem refines_asm : True := trivial\\n' > \"$dir/${mod}Refinement.lean\"\necho 'refinement outcome: {\"status\":\"emitted\"}' >&2\n";
        let (fake, so) = fake_qedlift(tmp.path(), script);
        // A regular file where the out-dir should be: creating `Generated/` under it fails.
        let out = tmp.path().join("not-a-dir");
        std::fs::write(&out, "x").unwrap();
        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        let err = run_discharge(&parsed, &request(&so, &fake, Some(&out), None))
            .expect_err("a failed copy must fail the discharge");
        assert!(
            err.to_string().contains("discharge verdict") && err.to_string().contains("failed"),
            "{err}"
        );
    }

    /// An emitted refinement whose Lean check cannot pass is `failed`, never `verified`, and is
    /// not persisted. A directory with no lakefile stands in for a broken Lean project.
    #[cfg(unix)]
    #[test]
    fn failed_lean_check_is_not_verified() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\nout=\"\"; mod=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output) out=\"$2\"; shift 2 ;;\n    --module) mod=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\ndir=$(dirname \"$out\")\nprintf 'theorem lifted : True := trivial\\n' > \"$out\"\nprintf 'theorem refines_asm : True := trivial\\n' > \"$dir/${mod}Refinement.lean\"\necho 'refinement outcome: {\"status\":\"emitted\"}' >&2\n";
        let (fake, so) = fake_qedlift(tmp.path(), script);
        let not_lake = tmp.path().join("not-a-lake-project");
        std::fs::create_dir_all(&not_lake).unwrap();
        let out = tmp.path().join("project");

        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        let err = run_discharge(&parsed, &request(&so, &fake, Some(&out), Some(&not_lake)))
            .expect_err("a failed Lean check must fail the discharge");
        assert!(err.to_string().contains("failed"), "{err}");
        assert!(!out.exists(), "nothing persisted on a failed Lean check");
    }

    /// Transition-command wiring: `--transition` + `--output-dir` (no
    /// `--module`/`--output` — qedlift names per-path modules itself).
    #[test]
    fn transition_command_wires_args() {
        let c = qedlift_transition_command(
            Path::new("/bin/qedlift"),
            Path::new("/tmp/d.json"),
            Path::new("/tmp/counter.so"),
            Some(Path::new("/tmp/idl.json")),
            Path::new("/tmp/out"),
        );
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--so",
                "/tmp/counter.so",
                "--descriptor",
                "/tmp/d.json",
                "--transition",
                "--output-dir",
                "/tmp/out",
                "--idl",
                "/tmp/idl.json",
            ]
        );
    }

    /// A guarded spec: `credit` expects a `success` path and a `zero_amount` rejection path.
    fn guarded_spec(dir: &Path) -> ParsedSpec {
        let path = dir.join("guarded.qedspec");
        std::fs::write(
            &path,
            "spec GuardedCounter\n\nstate {\n  counter : U64\n}\n\ntype Error\n  | ZeroAmount\n\n\
             handler credit (amount : U64) {\n  requires amount > 0 else ZeroAmount\n  \
             effect { counter += amount }\n}\n",
        )
        .unwrap();
        crate::check::parse_spec_file(&path).expect("guarded spec parses")
    }

    /// A fake `qedlift --transition`: writes the path modules and bundle into --output-dir,
    /// then prints `outcome` (a `transition outcome` JSON body) when it is non-empty.
    #[cfg(unix)]
    fn fake_transition(dir: &Path, outcome: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let line = if outcome.is_empty() {
            String::new()
        } else {
            format!("echo 'transition outcome: {outcome}' >&2\n")
        };
        let script = format!(
            "#!/bin/sh\nod=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output-dir) od=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\n\
             mkdir -p \"$od\"\nprintf '{body}\\n' > \"$od/GuardedCounterSuccessLifted.lean\"\n\
             printf '{body}\\n' > \"$od/GuardedCounterZeroAmountLifted.lean\"\n\
             printf '{body}\\n' > \"$od/GuardedCounterTransition.lean\"\n{line}"
        );
        let fake = dir.join(format!("fake-{}.sh", outcome.len() + body.len()));
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    fn transition_request<'a>(
        so: &'a Path,
        qedlift: &'a Path,
        out_dir: Option<&'a Path>,
    ) -> DischargeRequest<'a> {
        DischargeRequest {
            handler: "credit",
            account: Some("GuardedCounter".to_string()),
            so,
            idl: None,
            qedlift,
            module: None,
            out_dir,
            lean_project: None,
            json: false,
            layout: InputLayoutFlags::default(),
        }
    }

    const BOTH_PATHS: &str = r#"{"status":"emitted","bundle":"GuardedCounterTransition","paths":[{"label":"success","module":"GuardedCounterSuccess","kind":"return","exit_code":0,"tracked_written":true},{"label":"zero_amount","module":"GuardedCounterZeroAmount","kind":"return","exit_code":1,"tracked_written":false}]}"#;

    /// Both spec-expected paths traced and reported: `emitted` without a Lake project, and
    /// the modules persist under `<out-dir>/Generated/`.
    #[cfg(unix)]
    #[test]
    fn transition_with_expected_paths_passes_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = guarded_spec(tmp.path());
        let so = tmp.path().join("guarded_counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        for t in ["success", "zero_amount"] {
            std::fs::write(tmp.path().join(format!("guarded_counter_{t}.pcs")), "").unwrap();
        }
        let fake = fake_transition(tmp.path(), BOTH_PATHS, "theorem t : True := trivial");
        let out = tmp.path().join("project");
        // Bound parameter (schema v3), so the verdict can pass.
        let mut req = transition_request(&so, &fake, Some(&out));
        req.layout = InputLayoutFlags {
            account_data_lengths: Some(vec![16]),
            account_index: Some(0),
        };
        run_discharge_transition(&parsed, &req).expect("both paths lifted");
        assert!(out.join("Generated/GuardedCounterTransition.lean").exists());
        assert!(out
            .join("Generated/GuardedCounterZeroAmountLifted.lean")
            .exists());
    }

    /// An unbound parameter (no layout flags) caps the transition at `incomplete`, even when
    /// every path lifts: the parameter was never tied to its serialized argument.
    #[cfg(unix)]
    #[test]
    fn transition_unbound_parameter_is_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = guarded_spec(tmp.path());
        let so = tmp.path().join("guarded_counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        for t in ["success", "zero_amount"] {
            std::fs::write(tmp.path().join(format!("guarded_counter_{t}.pcs")), "").unwrap();
        }
        let fake = fake_transition(tmp.path(), BOTH_PATHS, "theorem t : True := trivial");
        let out = tmp.path().join("project");
        let err = run_discharge_transition(&parsed, &transition_request(&so, &fake, Some(&out)))
            .expect_err("unbound parameter cannot pass");
        assert!(err.to_string().contains("incomplete"), "{err}");
        assert!(!out.exists(), "nothing persisted");
    }

    /// A spec-expected path with no trace is `incomplete`, and nothing is persisted.
    #[cfg(unix)]
    #[test]
    fn transition_missing_expected_trace_is_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = guarded_spec(tmp.path());
        let so = tmp.path().join("guarded_counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        std::fs::write(tmp.path().join("guarded_counter_success.pcs"), "").unwrap();
        std::fs::write(tmp.path().join("guarded_counter_other.pcs"), "").unwrap();
        let fake = fake_transition(tmp.path(), BOTH_PATHS, "theorem t : True := trivial");
        let out = tmp.path().join("project");
        let err = run_discharge_transition(&parsed, &transition_request(&so, &fake, Some(&out)))
            .expect_err("zero_amount has no trace");
        assert!(err.to_string().contains("incomplete"), "{err}");
        assert!(!out.exists(), "nothing persisted");
    }

    /// Without qedlift's outcome line the kinds are unknown: at most `incomplete`.
    #[cfg(unix)]
    #[test]
    fn transition_without_outcome_line_is_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = guarded_spec(tmp.path());
        let so = tmp.path().join("guarded_counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        for t in ["success", "zero_amount"] {
            std::fs::write(tmp.path().join(format!("guarded_counter_{t}.pcs")), "").unwrap();
        }
        let fake = fake_transition(tmp.path(), "", "theorem t : True := trivial");
        let err = run_discharge_transition(&parsed, &transition_request(&so, &fake, None))
            .expect_err("unconfirmed kinds cannot pass");
        assert!(err.to_string().contains("incomplete"), "{err}");
    }

    /// A stale bundle in `--out-dir` cannot make a failed run pass: qedlift writes into a
    /// fresh temp dir, and the old files are neither read nor replaced.
    #[cfg(unix)]
    #[test]
    fn transition_stale_bundle_cannot_pass_a_failed_run() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let parsed = guarded_spec(tmp.path());
        let so = tmp.path().join("guarded_counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        let out = tmp.path().join("project");
        std::fs::create_dir_all(out.join("Generated")).unwrap();
        let stale = out.join("Generated/GuardedCounterTransition.lean");
        std::fs::write(&stale, "theorem old : True := trivial\n").unwrap();
        let fake = tmp.path().join("fake-fail.sh");
        std::fs::write(&fake, "#!/bin/sh\necho boom >&2\nexit 2\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = run_discharge_transition(&parsed, &transition_request(&so, &fake, Some(&out)))
            .expect_err("a failed qedlift fails even with a stale bundle present");
        assert!(err.to_string().contains("failed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&stale).unwrap(),
            "theorem old : True := trivial\n"
        );
    }

    /// A module carrying `sorry` fails the discharge.
    #[cfg(unix)]
    #[test]
    fn transition_sorry_module_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = guarded_spec(tmp.path());
        let so = tmp.path().join("guarded_counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        for t in ["success", "zero_amount"] {
            std::fs::write(tmp.path().join(format!("guarded_counter_{t}.pcs")), "").unwrap();
        }
        let fake = fake_transition(tmp.path(), BOTH_PATHS, "theorem t : True := by sorry");
        let err = run_discharge_transition(&parsed, &transition_request(&so, &fake, None))
            .expect_err("sorry must fail");
        assert!(err.to_string().contains("failed"), "{err}");
    }
}
