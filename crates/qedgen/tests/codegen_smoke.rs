use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qedgen crate should live under <repo>/crates/qedgen")
        .to_path_buf()
}

fn run(command: &mut Command) {
    let output = command.output().expect("failed to spawn command");
    if !output.status.success() {
        panic!(
            "command failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Generate `<example>` as an Anchor scaffold into a fresh tempdir, write
/// a `[patch]` config pointing `qedgen-macros` at the in-repo crate, and
/// run `cargo check` on the result. Don't pass `--offline` — CI runners
/// start cold, so the smoke needs to be allowed to fetch anchor-lang /
/// anchor-spl / solana-program on first run; locally cargo's registry
/// cache makes the second run fast.
fn smoke_anchor_scaffold(example: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_src = repo_root()
        .join("examples/rust")
        .join(example)
        .join(format!("{example}.qedspec"));
    let spec_path = temp.path().join(format!("{example}.qedspec"));
    std::fs::copy(&spec_src, &spec_path).unwrap_or_else(|e| panic!("copy {} spec: {e}", example));

    std::fs::copy(
        repo_root()
            .join("examples/rust")
            .join(example)
            .join("qed.toml"),
        temp.path().join("qed.toml"),
    )
    .unwrap_or_else(|e| panic!("copy {} manifest: {e}", example));
    std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");

    run(Command::new("git").arg("init").current_dir(temp.path()));

    let output_dir = temp.path().join("programs");
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("anchor")
        .arg("--output-dir")
        .arg(&output_dir)
        .current_dir(temp.path()));

    let cargo_config_dir = output_dir.join(".cargo");
    std::fs::create_dir_all(&cargo_config_dir).expect("create cargo config dir");
    let macros_path = repo_root().join("crates/qedgen-macros");
    std::fs::write(
        cargo_config_dir.join("config.toml"),
        format!(
            "[patch.\"https://github.com/qedgen/solana-skills\"]\nqedgen-macros = {{ path = {:?} }}\n",
            macros_path
        ),
    )
    .expect("write cargo patch config");

    run(Command::new("cargo")
        .arg("check")
        .arg("--manifest-path")
        .arg(output_dir.join("Cargo.toml")));
}

/// Anchor scaffold smoke + run the generated proptest harness against
/// it. Raises the floor from "compiles" to "tests pass" — catches
/// regressions in the predicate / transition rendering that pure
/// `cargo check` would miss (e.g., the Pubkey-effect-filter bug Day 1
/// surfaced on token-fundraiser).
fn smoke_anchor_scaffold_with_proptest(example: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let spec_src = repo_root()
        .join("examples/rust")
        .join(example)
        .join(format!("{example}.qedspec"));
    let spec_path = temp.path().join(format!("{example}.qedspec"));
    std::fs::copy(&spec_src, &spec_path).unwrap_or_else(|e| panic!("copy {} spec: {e}", example));

    std::fs::copy(
        repo_root()
            .join("examples/rust")
            .join(example)
            .join("qed.toml"),
        temp.path().join("qed.toml"),
    )
    .unwrap_or_else(|e| panic!("copy {} manifest: {e}", example));
    std::fs::create_dir(temp.path().join(".qed")).expect("create .qed");

    run(Command::new("git").arg("init").current_dir(temp.path()));

    let output_dir = temp.path().join("programs");
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--target")
        .arg("anchor")
        .arg("--output-dir")
        .arg(&output_dir)
        .current_dir(temp.path()));
    run(Command::new(env!("CARGO_BIN_EXE_qedgen"))
        .arg("codegen")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--proptest")
        .arg("--proptest-output")
        .arg(output_dir.join("tests/proptest.rs"))
        .current_dir(temp.path()));

    let cargo_config_dir = output_dir.join(".cargo");
    std::fs::create_dir_all(&cargo_config_dir).expect("create cargo config dir");
    let macros_path = repo_root().join("crates/qedgen-macros");
    std::fs::write(
        cargo_config_dir.join("config.toml"),
        format!(
            "[patch.\"https://github.com/qedgen/solana-skills\"]\nqedgen-macros = {{ path = {:?} }}\n",
            macros_path
        ),
    )
    .expect("write cargo patch config");

    // proptest is a dev-dependency on the test crate; the generator
    // emits Cargo.toml without dev-deps because production Anchor
    // builds don't need it. Append it for the smoke run.
    let cargo_toml = output_dir.join("Cargo.toml");
    let mut manifest = std::fs::read_to_string(&cargo_toml).expect("read Cargo.toml");
    manifest.push_str("\n[dev-dependencies]\nproptest = \"1\"\n");
    std::fs::write(&cargo_toml, manifest).expect("rewrite Cargo.toml");

    run(Command::new("cargo")
        .arg("test")
        .arg("--manifest-path")
        .arg(&cargo_toml)
        .arg("--test")
        .arg("proptest"));
}

#[test]
#[ignore = "runs qedgen codegen and cargo check on a generated Anchor crate"]
fn escrow_anchor_scaffold_compiles() {
    smoke_anchor_scaffold("escrow");
}

#[test]
#[ignore = "runs qedgen codegen and cargo check on a generated Anchor crate"]
fn multisig_anchor_scaffold_compiles() {
    smoke_anchor_scaffold("multisig");
}

#[test]
#[ignore = "runs qedgen codegen and cargo check on a generated Anchor crate"]
fn percolator_anchor_scaffold_compiles() {
    smoke_anchor_scaffold("percolator");
}

#[test]
#[ignore = "runs qedgen codegen + cargo test --test proptest on a generated Anchor crate"]
fn escrow_anchor_proptest_runs() {
    smoke_anchor_scaffold_with_proptest("escrow");
}
