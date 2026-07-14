//! Formatting gate: every checked-in generated-Rust artifact must be
//! rustfmt-clean. Codegen formats at the `write_generated_file` seam
//! (`codegen_shared::format_rust_source`), so a diff here means either a
//! new emitter bypassed the seam or a template file was edited without
//! re-running rustfmt.

use std::path::PathBuf;
use std::process::Command;

fn rustfmt_available() -> bool {
    Command::new("rustfmt")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn assert_fmt_clean(files: &[PathBuf]) {
    assert!(!files.is_empty(), "no .rs files found to fmt-check");
    let mut dirty = Vec::new();
    for file in files {
        let output = Command::new("rustfmt")
            .args(["--edition", "2021", "--check"])
            .arg(file)
            .output()
            .expect("failed to spawn rustfmt");
        if !output.status.success() {
            dirty.push(format!(
                "{}\n{}",
                file.display(),
                String::from_utf8_lossy(&output.stdout)
            ));
        }
    }
    assert!(
        dirty.is_empty(),
        "generated Rust is not rustfmt-clean:\n{}",
        dirty.join("\n")
    );
}

fn rs_files_in(dir: &str) -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", root.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    files
}

#[test]
fn snapshot_rust_outputs_are_rustfmt_clean() {
    if !rustfmt_available() {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }
    assert_fmt_clean(&rs_files_in("tests/snapshots"));
}

#[test]
fn rust_templates_are_rustfmt_clean() {
    if !rustfmt_available() {
        eprintln!("skipping: rustfmt not on PATH");
        return;
    }
    assert_fmt_clean(&rs_files_in("templates"));
}
