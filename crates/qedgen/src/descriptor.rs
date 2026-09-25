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

mod lean_check;
mod outcome;
mod transition;

use lean_check::{check_modules, find_lake_project, LeanResult};
use outcome::{classify_lift, parse_outcome, DischargeReport, LeanCheck, LiftVerdict, Verdict};

/// Descriptor schema versions, kept in lockstep with qedsvm's `DESCRIPTOR_SCHEMA_MAX`.
/// A constant delta (`add_const`) is v1; a parameter delta (`add_param`) is v2.
///
/// v2.40 scope note (#124): the whole-transition mode (qedsvm v0.9.0
/// `--transition`) consumes this SAME v1/v2 shape — paths, guards, and abort
/// codes come from the discovered `.pcs` traces, not the descriptor. A richer
/// descriptor (guard cascade, multi-field effects, per-abort codes — built
/// from the #151 `ExprTree`) is a schema v3 the current consumer would refuse
/// fail-closed (`DESCRIPTOR_SCHEMA_MAX = 2`); it lands in lockstep with a
/// qedsvm-side bump. Layout stays out of the producer entirely: offsets are
/// shape, owned by the IDL (inline `layout` remains a hand-authored escape
/// hatch for fixtures).
const SCHEMA_VERSION_CONST: u32 = 1;
const SCHEMA_VERSION_PARAM: u32 = 2;

/// Build the name-level descriptor for `handler` in `parsed`.
///
/// Requires the handler to have exactly one increment effect `<field> += <rhs>`, where `<rhs>`
/// is either an integer literal (constant delta, schema v1) or a declared parameter of the
/// handler (parameter delta, schema v2). A non-`+=` op, multiple effects, a missing handler,
/// or an RHS that is neither a literal nor a declared parameter are rejected with clear errors.
pub(crate) fn build_descriptor(
    parsed: &ParsedSpec,
    handler: &str,
    account: Option<String>,
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

    // Constant delta (`+= k`) vs parameter delta (`+= amount`): an integer-literal RHS is a
    // constant (schema v1); otherwise the RHS must be a declared parameter of the handler
    // (schema v2). An RHS that is neither is rejected (the soundness boundary).
    let (op_json, schema_version) = match value.parse::<i64>() {
        Ok(delta) => (
            serde_json::json!({ "add_const": delta }),
            SCHEMA_VERSION_CONST,
        ),
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
            (
                serde_json::json!({ "add_param": value }),
                SCHEMA_VERSION_PARAM,
            )
        }
    };

    // `account` resolution: explicit override, else the spec's first account type, else the
    // program name. Use the IDL account name (the override) so qedsvm resolves the offsets.
    let account = account
        .or_else(|| parsed.account_types.first().map(|a| a.name.clone()))
        .unwrap_or_else(|| parsed.program_name.clone());

    Ok(serde_json::json!({
        "schema_version": schema_version,
        "account": account,
        "handler": handler,
        "mutated": field,
        "op": op_json,
    }))
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
}

/// Build the descriptor for the handler, discharge it against the `.so` via `qedlift`, and
/// run Lean on the emitted modules (#406). Prints one report (human or JSON) and returns an
/// error unless the verdict is `verified` or `emitted`.
///
/// qedlift writes into a fresh temp dir, so files left in `out_dir` by an earlier run never
/// count as this run's proof. Artifacts are copied into `out_dir` only when the verdict passes.
pub(crate) fn run_discharge(parsed: &ParsedSpec, req: &DischargeRequest) -> Result<()> {
    let handler = req.handler;
    let descriptor = build_descriptor(parsed, handler, req.account.clone())?;
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
    let descriptor = build_descriptor(parsed, handler, req.account.clone())?;
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
            report.paths = transition::path_rows(&expected, &traced, None)?;
            report.reason = Some("qedlift_not_runnable".to_string());
            report.message = Some(message);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            discharge_transition_run(&mut report, req, &expected, &traced, &output, &stderr, &out);
        }
    }

    if report.verdict.passes() {
        if let Some(dest) = req.out_dir {
            let modules = transition::emitted_modules(&out);
            match persist_modules(&dest.join("Generated"), &modules) {
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
    expected: &std::collections::BTreeSet<String>,
    traced: &std::collections::BTreeSet<String>,
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
    report.paths = match transition::path_rows(expected, traced, outcome.as_ref()) {
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
    let modules = transition::emitted_modules(out);
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

/// Copy `modules` into `dest` (created if needed). Returns the persisted paths.
fn persist_modules(dest: &Path, modules: &[PathBuf]) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dest)
        .with_context(|| format!("creating discharge out-dir {}", dest.display()))?;
    modules
        .iter()
        .map(|src| {
            let name = src
                .file_name()
                .ok_or_else(|| anyhow!("module path has no file name: {}", src.display()))?;
            let dst = dest.join(name);
            std::fs::copy(src, &dst)
                .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
            Ok(dst)
        })
        .collect()
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
        let d = build_descriptor(&parsed, "increment", Some("vault".to_string()))
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
        let d = build_descriptor(&parsed, "increment", Some("Counter".to_string()))
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

    /// A parameter delta (`total += amount`) emits an `add_param` descriptor (schema v2):
    /// the RHS is a declared handler parameter, so it is a runtime credit, not a constant.
    /// (Real vaults deposit `+= amount`, not `+= 1`.)
    #[test]
    fn parameter_delta_emits_add_param() {
        let parsed = parse("tests/fixtures/descriptor/vault.qedspec");
        let d = build_descriptor(&parsed, "deposit", Some("vault".to_string()))
            .expect("build deposit (parameter) descriptor");
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
        let err = build_descriptor(&parsed, "deposit", Some("vault".to_string()))
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
        let err = build_descriptor(&parsed, "nope", None).expect_err("unknown handler");
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
        }
    }

    const BOTH_PATHS: &str = r#"{"status":"emitted","bundle":"GuardedCounterTransition","paths":[{"label":"success","kind":"return","exit_code":0,"tracked_written":true},{"label":"zero_amount","kind":"return","exit_code":1,"tracked_written":false}]}"#;

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
        run_discharge_transition(&parsed, &transition_request(&so, &fake, Some(&out)))
            .expect("both paths lifted");
        assert!(out.join("Generated/GuardedCounterTransition.lean").exists());
        assert!(out
            .join("Generated/GuardedCounterZeroAmountLifted.lean")
            .exists());
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
