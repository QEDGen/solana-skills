//! Axiom gate: `verify --strict` fails when a Lean theorem depends on an
//! axiom outside the spec-derived permitted set.
//!
//! A fake `lake` on PATH stands in for the Lean toolchain. `lake build`
//! succeeds, and `lake env lean _QedgenAxiomReport.lean` prints the
//! `#print axioms` lines from `QEDGEN_FAKE_AXIOMS`. That is all `run_lean`
//! reads, so the gate is exercised without a Mathlib build.

#![cfg(unix)]

mod common;

use common::{ensure_qedgen_built, git_init, qedgen_bin};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const SPEC: &str = "\
spec AxiomGate

type State
  | Active of {
      balance : U64,
    }

handler deposit (amount : U64) : State.Active -> State.Active {
  effect { balance += amount }
}
";

const FAKE_LAKE: &str = "#!/bin/sh
if [ \"$1\" = \"env\" ]; then
  printf '%s\\n' \"$QEDGEN_FAKE_AXIOMS\"
fi
exit 0
";

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("v.qedspec"), SPEC).expect("write spec");
    std::fs::create_dir_all(root.join(".qed")).expect("mkdir .qed");
    let lean = root.join("formal_verification");
    std::fs::create_dir_all(&lean).expect("mkdir lean dir");
    std::fs::write(lean.join("lakefile.lean"), "-- fake\n").expect("write lakefile");
    std::fs::write(
        lean.join("Proofs.lean"),
        "namespace AxiomGate\ntheorem t : True := trivial\nend AxiomGate\n",
    )
    .expect("write proofs");
    let bin = root.join("fakebin");
    std::fs::create_dir_all(&bin).expect("mkdir fakebin");
    let lake = bin.join("lake");
    std::fs::write(&lake, FAKE_LAKE).expect("write fake lake");
    std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    git_init(root);
    tmp
}

fn verify(root: &Path, axioms_line: &str, strict: bool) -> (String, bool) {
    ensure_qedgen_built();
    let path = format!(
        "{}:{}",
        root.join("fakebin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new(qedgen_bin());
    cmd.args([
        "verify",
        "--spec",
        "v.qedspec",
        "--lean",
        "--lean-dir",
        "formal_verification",
    ])
    .env("PATH", path)
    .env("QEDGEN_FAKE_AXIOMS", axioms_line)
    .current_dir(root);
    if strict {
        cmd.arg("--strict");
    }
    let out = cmd.output().expect("spawn qedgen verify");
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        out.status.success(),
    )
}

#[test]
fn strict_passes_when_only_classical_axioms_are_used() {
    let tmp = project();
    let (out, ok) = verify(
        tmp.path(),
        "'AxiomGate.t' depends on axioms: [propext, Classical.choice]",
        true,
    );
    assert!(ok, "classical axioms must not gate:\n{out}");
    assert!(
        out.contains("non-classical axiom use(s) are permitted"),
        "{out}"
    );
}

#[test]
fn strict_fails_on_sorry_and_compiler_trust() {
    let tmp = project();
    let line = "'AxiomGate.t' depends on axioms: [propext, sorryAx, Lean.ofReduceBool]";

    let (out, ok) = verify(tmp.path(), line, true);
    assert!(!ok, "forbidden axioms must fail --strict:\n{out}");
    assert!(
        out.contains("- sorryAx  [CRIT] incomplete proof (sorry)"),
        "{out}"
    );
    assert!(
        out.contains("- Lean.ofReduceBool  [CRIT] trusts the Lean compiler"),
        "{out}"
    );
    assert!(
        out.contains("2 [CRIT] axiom use(s) outside the permitted set"),
        "{out}"
    );

    // Without --strict the findings print and the run passes.
    let (out, ok) = verify(tmp.path(), line, false);
    assert!(ok, "non-strict verify must not gate on axioms:\n{out}");
    assert!(out.contains("[CRIT]"), "{out}");
}

#[test]
fn strict_fails_on_undeclared_ensures_axiom() {
    // No interface is pinned by this spec, so no ensures axiom is permitted.
    let tmp = project();
    let (out, ok) = verify(
        tmp.path(),
        "'AxiomGate.t' depends on axioms: [Token.transfer.ensures_axiom_0]",
        true,
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("[CRIT] not declared by a pinned interface"),
        "{out}"
    );
}
