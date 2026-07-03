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
// qedgen shells out to qedsvm's `qedlift` binary; no meaning crosses the boundary (qedgen
// parses none of qedlift's internals, only its exit status and whether it emitted a proof).
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

/// Build the descriptor for `handler`, then discharge it against `so` via `qedlift`. Prints a
/// verdict (proven against the bytes / not discharged) and returns an error on failure.
///
/// `out_dir`: when `Some`, the discharged artifacts (`<Module>TracedLifted.lean` +
/// `<Module>Refinement.lean`) are persisted there instead of being discarded with the temp
/// workdir — so the byte-level proof lives in the project (A2a). When `None`, the legacy
/// verdict-only behaviour is preserved.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_discharge(
    parsed: &ParsedSpec,
    handler: &str,
    account: Option<String>,
    so: &Path,
    idl: Option<&Path>,
    qedlift: &Path,
    module: Option<String>,
    out_dir: Option<&Path>,
) -> Result<()> {
    let descriptor = build_descriptor(parsed, handler, account)?;
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
    let module = module.unwrap_or_else(|| format!("{}{}", pascal(&account_name), pascal(handler)));

    let work = tempfile::tempdir().context("create temp workdir for discharge")?;
    let desc_path = work.path().join("descriptor.json");
    std::fs::write(&desc_path, serde_json::to_string_pretty(&descriptor)?)
        .context("write temp descriptor")?;
    let out = work.path().join(format!("{}TracedLifted.lean", module));
    let refinement = work.path().join(format!("{}Refinement.lean", module));

    println!("=== qedgen discharge ===");
    println!("  spec handler : {}", handler);
    println!(
        "  obligation   : {}.{} += {}",
        account_name, mutated, delta_str
    );
    println!("  program      : {}", so.display());
    println!("  qedlift      : {}", qedlift.display());

    let output = qedlift_command(qedlift, &desc_path, so, idl, &module, &out)
        .output()
        .map_err(|e| {
            anyhow!(
                "could not run qedlift at {}: {} (build it with `cargo build \
                 --features qedrecover --bin qedlift` in the qedsvm repo)",
                qedlift.display(),
                e
            )
        })?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() && refinement.exists() {
        let proof = std::fs::read_to_string(&refinement).unwrap_or_default();
        // sanity: a discharged proof is sorry-free.
        if proof.contains("sorry") {
            bail!(
                "qedlift emitted a refinement containing `sorry` for `{}`",
                handler
            );
        }
        println!(
            "  ✔ DISCHARGED : `{}` is proven against the bytes (offsets resolved from the IDL).",
            handler
        );
        println!(
            "    qedlift emitted a sorry-free AsmRefinesFieldUpdate refinement + a \
             qedsvm_discharge'd `ensures`."
        );
        // A2a: persist the proof into the project instead of discarding it with the temp
        // workdir, so a later `lake build` type-checks it (and A2b can consume it from the
        // bridge). Without --out-dir, keep the legacy verdict-only behaviour.
        match out_dir {
            Some(dest) => {
                let (lifted_dst, refinement_dst) =
                    persist_discharge_artifacts(dest, &out, &refinement)?;
                println!("    persisted    : {}", refinement_dst.display());
                println!("                   {}", lifted_dst.display());
                println!(
                    "    wire it in   : `import {}Refinement` (add both modules to your lake lib",
                    module
                );
                println!(
                    "                   roots; the project must `require qedsvm` — lean_solana \
                     projects already do)."
                );
            }
            None => {
                println!(
                    "    Type-check it with `lake build` in the qedsvm project (the emitted module"
                );
                println!(
                    "    is identical in shape to the committed, lake-green Generated proofs)."
                );
                println!(
                    "    Pass `--out-dir <project>` to persist it into the project instead of a \
                     temp dir."
                );
            }
        }
        Ok(())
    } else if output.status.success() {
        bail!(
            "NOT DISCHARGED: qedlift ran but emitted no refinement for `{}` (the bytes likely \
             do not realise the claimed obligation).\n{}",
            handler,
            stderr_tail(&stderr)
        )
    } else {
        bail!(
            "qedlift failed ({}):\n{}",
            output.status,
            stderr_tail(&stderr)
        )
    }
}

/// Whole-transition discharge (qedsvm #40, v0.9.0): build the descriptor, then drive
/// `qedlift --transition` — every path of the program is lifted from its discovered
/// `<stem>_<path>.pcs` trace, each carrying an `AsmRefinesTransitionPath` (success: exit code +
/// tracked fields pre→post) or `AsmRefinesTransitionFault` (typed abort/panic/OOB, no post)
/// corollary, plus the ONE bundle theorem covering all paths under their branch guards.
///
/// The bundle module is `<StemPascal>Transition.lean` in `out_dir` (qedlift writes directly —
/// no temp-dir copy dance). Verdict = qedlift succeeded ∧ bundle exists ∧ sorry-free.
pub(crate) fn run_discharge_transition(
    parsed: &ParsedSpec,
    handler: &str,
    account: Option<String>,
    so: &Path,
    idl: Option<&Path>,
    qedlift: &Path,
    out_dir: Option<&Path>,
) -> Result<()> {
    let descriptor = build_descriptor(parsed, handler, account)?;
    let account_name = descriptor["account"].as_str().unwrap_or("?").to_string();
    let mutated = descriptor["mutated"].as_str().unwrap_or("?");

    let out_dir = out_dir.ok_or_else(|| {
        anyhow!(
            "--transition requires --out-dir: qedlift writes one module per path plus the \
             bundle theorem directly into the project"
        )
    })?;

    let work = tempfile::tempdir().context("create temp workdir for discharge")?;
    let desc_path = work.path().join("descriptor.json");
    std::fs::write(&desc_path, serde_json::to_string_pretty(&descriptor)?)
        .context("write temp descriptor")?;

    let stem_snake = so.file_stem().and_then(|s| s.to_str()).unwrap_or("program");
    let bundle_module = format!("{}Transition", pascal(stem_snake));
    let bundle_path = out_dir.join(format!("{}.lean", bundle_module));

    println!("=== qedgen discharge (whole-transition) ===");
    println!("  spec handler : {}", handler);
    println!("  tracked      : {}.{}", account_name, mutated);
    println!("  program      : {}", so.display());
    println!("  qedlift      : {}", qedlift.display());

    let output = qedlift_transition_command(qedlift, &desc_path, so, idl, out_dir)
        .output()
        .map_err(|e| {
            anyhow!(
                "could not run qedlift at {}: {} (build it with `cargo build \
                 --features qedrecover --bin qedlift` in the qedsvm repo)",
                qedlift.display(),
                e
            )
        })?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        bail!(
            "qedlift --transition failed ({}):\n{}",
            output.status,
            stderr_tail(&stderr)
        );
    }
    if !bundle_path.exists() {
        bail!(
            "NOT DISCHARGED: qedlift ran but emitted no transition bundle for `{}` (expected \
             {}; are there >= 2 `<stem>_<path>.pcs` traces beside the .so?).\n{}",
            handler,
            bundle_path.display(),
            stderr_tail(&stderr)
        );
    }
    let bundle = std::fs::read_to_string(&bundle_path).unwrap_or_default();
    if bundle.contains("sorry") {
        bail!(
            "qedlift emitted a transition bundle containing `sorry` for `{}`",
            handler
        );
    }
    println!(
        "  ✔ DISCHARGED : `{}` whole-transition bundle proven against the bytes.",
        handler
    );
    println!("    bundle       : {}", bundle_path.display());
    println!(
        "    Every discovered path carries its own *_transition_path / *_transition_fault \
         corollary"
    );
    println!(
        "    (success: exit code + tracked field pre→post; fault: typed error, no tracked \
         writes)."
    );
    println!(
        "    wire it in   : `import {}` (add the emitted modules to your lake lib roots; the",
        bundle_module
    );
    println!(
        "                   project must `require qedsvm` — lean_solana projects already do)."
    );
    Ok(())
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
        run_discharge(
            &parsed,
            "increment",
            Some("Counter".to_string()),
            &so,
            None,
            &fake,
            None,
            Some(&out),
        )
        .expect("discharge persists");

        // Module defaults to `<Account><Handler>` = CounterIncrement.
        assert!(
            out.join("CounterIncrementRefinement.lean").exists(),
            "refinement persisted into out-dir",
        );
        assert!(
            out.join("CounterIncrementTracedLifted.lean").exists(),
            "lifted module persisted into out-dir",
        );
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

    /// Whole-transition end-to-end with a fake qedlift: verdict requires the
    /// `<StemPascal>Transition.lean` bundle in the out-dir, sorry-free.
    #[cfg(unix)]
    #[test]
    fn discharge_transition_requires_sorry_free_bundle() {
        use std::os::unix::fs::PermissionsExt;
        // Fake qedlift --transition: writes one path module + the bundle
        // into --output-dir, parsing only the args the driver passes.
        const FAKE: &str = "#!/bin/sh\nod=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output-dir) od=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\nmkdir -p \"$od\"\nprintf 'theorem counter_success_transition_path : True := trivial\\n' > \"$od/CounterSuccessLifted.lean\"\nprintf 'theorem counter_transition_bundle : True := trivial\\n' > \"$od/CounterTransition.lean\"\n";

        let tmp = tempfile::tempdir().expect("tempdir");
        let fake = tmp.path().join("fake-qedlift.sh");
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let so = tmp.path().join("counter.so");
        std::fs::write(&so, b"\x7fELF").unwrap();
        let out = tmp.path().join("project");

        let parsed = parse("tests/fixtures/descriptor/counter.qedspec");
        run_discharge_transition(
            &parsed,
            "increment",
            Some("Counter".to_string()),
            &so,
            None,
            &fake,
            Some(&out),
        )
        .expect("transition discharge succeeds with bundle present");
        assert!(out.join("CounterTransition.lean").exists());

        // Without --out-dir the driver refuses up front.
        let err = run_discharge_transition(
            &parsed,
            "increment",
            Some("Counter".to_string()),
            &so,
            None,
            &fake,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--out-dir"), "{err}");

        // A bundle carrying `sorry` is refused.
        let sorry_out = tmp.path().join("sorry-project");
        std::fs::create_dir_all(&sorry_out).unwrap();
        const FAKE_SORRY: &str = "#!/bin/sh\nod=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --output-dir) od=\"$2\"; shift 2 ;;\n    *) shift ;;\n  esac\ndone\nmkdir -p \"$od\"\nprintf 'theorem b : True := by sorry\\n' > \"$od/CounterTransition.lean\"\n";
        let fake_sorry = tmp.path().join("fake-qedlift-sorry.sh");
        std::fs::write(&fake_sorry, FAKE_SORRY).unwrap();
        std::fs::set_permissions(&fake_sorry, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = run_discharge_transition(
            &parsed,
            "increment",
            Some("Counter".to_string()),
            &so,
            None,
            &fake_sorry,
            Some(&sorry_out),
        )
        .unwrap_err();
        assert!(err.to_string().contains("sorry"), "{err}");
    }
}
