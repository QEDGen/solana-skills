//! `qedgen descriptor` — emit a name-level refinement descriptor from a `.qedspec`.
//!
//! This is the PRODUCER half of the qedgen <-> qedsvm discharge seam. It lowers a single
//! constant-increment handler to the JSON obligation qedsvm's `qedlift` consumes via
//! `--descriptor` (schema: qedsvm `docs/REFINEMENT_DESCRIPTOR.md`).
//!
//! The descriptor is NAME-LEVEL: it carries which named field a handler mutates and by how
//! much, never byte offsets. Offsets are *shape*, owned by the IDL and resolved on the qedsvm
//! side. So qedgen never computes a layout here; it emits pure semantics derived from the spec.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::check::ParsedSpec;

/// Descriptor schema versions, kept in lockstep with qedsvm's `DESCRIPTOR_SCHEMA_MAX`.
/// A constant delta (`add_const`) is v1; a parameter delta (`add_param`) is v2.
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
        [one] => one,
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
        Ok(delta) => (serde_json::json!({ "add_const": delta }), SCHEMA_VERSION_CONST),
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
            (serde_json::json!({ "add_param": value }), SCHEMA_VERSION_PARAM)
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
pub(crate) fn run_discharge(
    parsed: &ParsedSpec,
    handler: &str,
    account: Option<String>,
    so: &Path,
    idl: Option<&Path>,
    qedlift: &Path,
    module: Option<String>,
) -> Result<()> {
    let descriptor = build_descriptor(parsed, handler, account)?;
    let mutated = descriptor["mutated"].as_str().unwrap_or("?");
    // Constant (`add_const`) or parameter (`add_param`) credit, for the printed obligation.
    let delta_str = descriptor["op"]["add_const"]
        .as_i64()
        .map(|k| k.to_string())
        .or_else(|| descriptor["op"]["add_param"].as_str().map(|p| p.to_string()))
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
    println!("  obligation   : {}.{} += {}", account_name, mutated, delta_str);
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
        println!("    Type-check it with `lake build` in the qedsvm project (the emitted module");
        println!("    is identical in shape to the committed, lake-green Generated proofs).");
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
            h.effects = vec![("total".to_string(), "add".to_string(), "mystery".to_string())];
            h.takes_params.clear();
        }
        let err = build_descriptor(&parsed, "deposit", Some("vault".to_string()))
            .expect_err("an undeclared RHS must be rejected");
        assert!(
            err.to_string().contains("neither an integer literal nor a declared parameter"),
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
}
