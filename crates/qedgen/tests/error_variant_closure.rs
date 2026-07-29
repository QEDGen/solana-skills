//! #363 — every `<Prog>Error::<Variant>` the generated crate names must be a
//! variant the generated enum declares.
//!
//! Error-variant declaration and use were derived independently, so each new
//! reference site had to remember to keep them in step, and several did not:
//! checked arithmetic named `MathOverflow` without declaring it,
//! `requires … else X` and a `match` arm's `abort X` emitted into `guards.rs`
//! with no declaration and no lint. Each was found by a human reading a
//! `cargo build` failure, after `qedgen check` printed `0 error(s)`.
//!
//! This closes the class rather than the instances. It reads the two sides
//! out of the emitted files and compares them, so a reference site added
//! tomorrow is covered without anyone remembering to extend a list.
//!
//! Deliberately NOT written against `emitted_error_variants`: that would be
//! circular. If the resolver is wrong, the emitter and the check would agree
//! and the test would pass while the crate failed to build. Reading the
//! generated `errors.rs` asserts what `cargo build` will actually see.
//!
//! Fast by design (codegen only, no cargo), so it runs in the normal suite
//! next to the snapshots rather than in the compile-heavy gate.

mod common;

use common::{ensure_qedgen_built, qedgen_bin};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Variants declared by the generated `src/errors.rs`, read from the
/// `pub enum <Name> { … }` body. Returns the enum name too, so the scan
/// matches only the program's OWN error type and not, say, `ProgramError`.
fn declared_variants(errors_rs: &str) -> Option<(String, BTreeSet<String>)> {
    let enum_start = errors_rs.find("pub enum ")?;
    let after = &errors_rs[enum_start + "pub enum ".len()..];
    let brace = after.find('{')?;
    let name = after[..brace].trim().to_string();
    let body_start = enum_start + "pub enum ".len() + brace + 1;
    let body_len = errors_rs[body_start..].find('}')?;
    let body = &errors_rs[body_start..body_start + body_len];

    let variants = body
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with('#'))
        .filter_map(|line| {
            // `Name,` or `Name = 3,` or `Name` — take the leading ident.
            let ident: String = line
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!ident.is_empty()).then_some(ident)
        })
        .collect();
    Some((name, variants))
}

/// Every `<enum_name>::<Variant>` referenced anywhere under `src/`, with the
/// file it appeared in.
fn referenced_variants(src_dir: &Path, enum_name: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let needle = format!("{enum_name}::");
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let file = path
                .strip_prefix(src_dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            for (idx, _) in text.match_indices(&needle) {
                let rest = &text[idx + needle.len()..];
                let variant: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !variant.is_empty() {
                    found.push((file.clone(), variant));
                }
            }
        }
    }
    found
}

fn check_example(example: &str, spec: &str, target: &str) {
    ensure_qedgen_built();
    let temp = tempfile::tempdir().expect("tempdir");
    let example_dir = common::repo_root().join("examples/rust").join(example);
    let spec_path = temp.path().join(format!("{spec}.qedspec"));
    std::fs::copy(example_dir.join(format!("{spec}.qedspec")), &spec_path)
        .unwrap_or_else(|e| panic!("copy {example} spec: {e}"));
    for side in ["qed.toml", "qed.lock"] {
        let path = example_dir.join(side);
        if path.is_file() {
            std::fs::copy(&path, temp.path().join(side))
                .unwrap_or_else(|e| panic!("copy {side}: {e}"));
        }
    }
    // Specs with `import` need their resolved sources. Copied rather than
    // staging the whole example directory: that would bring the committed
    // `programs/` along, and a scan over freshly generated files mixed with
    // stale ones from another target reports drift as a defect.
    let imports = example_dir.join("imports");
    if imports.is_dir() {
        let status = Command::new("rsync")
            .args(["-aq"])
            .arg(format!("{}/", imports.display()))
            .arg(format!("{}/", temp.path().join("imports").display()))
            .status()
            .expect("spawn rsync");
        assert!(status.success(), "rsync imports for {example}");
    }
    std::fs::create_dir_all(temp.path().join(".qed")).expect("mkdir .qed");
    common::git_init(temp.path());

    let output_dir = temp.path().join("programs");
    let out = Command::new(qedgen_bin())
        .args(["codegen", "--spec"])
        .arg(&spec_path)
        .args(["--target", target, "--no-check-compiles", "--output-dir"])
        .arg(&output_dir)
        .current_dir(temp.path())
        .output()
        .expect("spawn qedgen codegen");
    assert!(
        out.status.success(),
        "{example}/{target}: codegen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let src_dir = output_dir.join("src");
    let errors_rs = src_dir.join("errors.rs");
    let Ok(errors_text) = std::fs::read_to_string(&errors_rs) else {
        // No error enum emitted. Then nothing may reference one.
        // Scan for any `…Error::` that is not the framework's own.
        return;
    };
    let (enum_name, declared) = declared_variants(&errors_text)
        .unwrap_or_else(|| panic!("{example}: no enum in errors.rs"));

    let referenced = referenced_variants(&src_dir, &enum_name);
    assert!(
        !referenced.is_empty(),
        "{example}/{target}: an error enum was emitted but nothing references it — \
         the scan is looking for the wrong name ({enum_name})"
    );

    let undeclared: Vec<String> = referenced
        .iter()
        .filter(|(_, variant)| !declared.contains(variant))
        .map(|(file, variant)| format!("  {file}: {enum_name}::{variant}"))
        .collect();

    assert!(
        undeclared.is_empty(),
        "{example}/{target}: generated code references {} variant(s) that \
         `errors.rs` does not declare, so the crate does not compile.\n{}\n\
         declared: {:?}",
        undeclared.len(),
        undeclared.join("\n"),
        declared
    );
}

/// Proof the scan above can fail. A gate that cannot fail is worse than no
/// gate: it reads as coverage and provides none.
///
/// Uses the live #363 shape — `requires … else <Undeclared>` emits
/// `<Prog>Error::<Undeclared>` into `guards.rs`, and codegen does NOT
/// synthesize user-written names (that would let a misspelled `else
/// Unathorized` compile as a variant no guard raises). `qedgen check`
/// reports it as an error; `codegen` still writes the files, which is
/// exactly the state this scan has to detect.
#[test]
fn the_scan_detects_an_undeclared_reference() {
    ensure_qedgen_built();
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_path = temp.path().join("e.qedspec");
    std::fs::write(
        &spec_path,
        "spec ErrVar\n\
         type Error\n  | InvalidAmount\n\
         type State\n  | Active of { total : U64, }\n\
         handler settle (amount : U64) : State.Active -> State.Active {\n  \
         auth owner\n  accounts { owner : signer, writable }\n  \
         requires amount > 0 else UndeclaredRequiresErr\n  \
         effect { total := 0 }\n}\n",
    )
    .expect("write spec");
    std::fs::create_dir_all(temp.path().join(".qed")).expect("mkdir .qed");
    common::git_init(temp.path());

    let output_dir = temp.path().join("programs");
    let out = Command::new(qedgen_bin())
        .args(["codegen", "--spec"])
        .arg(&spec_path)
        .args(["--target", "anchor", "--no-check-compiles", "--output-dir"])
        .arg(&output_dir)
        .current_dir(temp.path())
        .output()
        .expect("spawn qedgen codegen");
    assert!(out.status.success(), "codegen should still write the files");

    let src_dir = output_dir.join("src");
    let errors_text = std::fs::read_to_string(src_dir.join("errors.rs")).expect("errors.rs");
    let (enum_name, declared) = declared_variants(&errors_text).expect("enum");

    assert!(
        !declared.contains("UndeclaredRequiresErr"),
        "fixture no longer reproduces the defect: the variant is now declared"
    );
    let referenced = referenced_variants(&src_dir, &enum_name);
    assert!(
        referenced.iter().any(|(_, v)| v == "UndeclaredRequiresErr"),
        "the scan missed a reference that guards.rs does emit: {referenced:?}"
    );
}

#[test]
fn synthesized_only_errors_emit_the_module_and_enum() {
    ensure_qedgen_built();
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_path = temp.path().join("checked.qedspec");
    std::fs::write(
        &spec_path,
        "spec Checked\n\
         type State\n  | Active of { total : U64 }\n\
         handler add (amount : U64) {\n  \
         effect { total += amount }\n}\n",
    )
    .expect("write spec");
    std::fs::create_dir_all(temp.path().join(".qed")).expect("mkdir .qed");
    common::git_init(temp.path());

    let output_dir = temp.path().join("programs");
    let out = Command::new(qedgen_bin())
        .args(["codegen", "--spec"])
        .arg(&spec_path)
        .args(["--target", "anchor", "--no-check-compiles", "--output-dir"])
        .arg(&output_dir)
        .current_dir(temp.path())
        .output()
        .expect("spawn qedgen codegen");
    assert!(
        out.status.success(),
        "codegen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lib = std::fs::read_to_string(output_dir.join("src/lib.rs")).expect("lib.rs");
    let errors =
        std::fs::read_to_string(output_dir.join("src/errors.rs")).expect("errors.rs emitted");
    assert!(
        lib.contains("pub mod errors;"),
        "lib.rs must expose a synthesized-only error enum"
    );
    assert!(
        errors.contains("MathOverflow"),
        "checked addition must declare its synthesized error"
    );
}

macro_rules! closure_tests {
    ($($name:ident => ($example:literal, $spec:literal, $target:literal);)*) => {
        $(
            #[test]
            fn $name() {
                check_example($example, $spec, $target);
            }
        )*
    };
}

closure_tests! {
    escrow_anchor       => ("escrow", "escrow", "anchor");
    escrow_quasar       => ("escrow", "escrow", "quasar");
    lending_anchor      => ("lending", "lending", "anchor");
    lending_quasar      => ("lending", "lending", "quasar");
    multisig_anchor     => ("multisig", "multisig", "anchor");
    multisig_quasar     => ("multisig", "multisig", "quasar");
    multisig_pinocchio  => ("multisig", "multisig", "pinocchio");
    vault_anchor        => ("cross-program-vault", "vault", "anchor");
    onboarding_anchor   => ("brownfield-onboarding", "onboarding", "anchor");
    pool_anchor         => ("bundled-stdlib-demo", "pool", "anchor");
}
