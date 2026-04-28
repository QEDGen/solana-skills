use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qedgen crate should live under <repo>/crates/qedgen")
        .to_path_buf()
}

#[test]
fn rust_examples_with_qed_state_have_qed_toml() {
    let examples_root = repo_root().join("examples/rust");
    let mut missing = Vec::new();

    for entry in std::fs::read_dir(&examples_root).expect("read examples/rust") {
        let entry = entry.expect("read example entry");
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let has_qed_state = path.join(".qed").is_dir();
        let has_generated_artifacts = path.join("programs/src").is_dir()
            || path.join("src/instructions").is_dir()
            || path.join("formal_verification/Spec.lean").is_file()
            || path.join("tests/kani.rs").is_file()
            || path.join("tests/proptest.rs").is_file();

        if (has_qed_state || has_generated_artifacts) && !path.join("qed.toml").is_file() {
            missing.push(
                path.strip_prefix(&examples_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        missing.is_empty(),
        "example roots missing qed.toml: {}",
        missing.join(", ")
    );
}
