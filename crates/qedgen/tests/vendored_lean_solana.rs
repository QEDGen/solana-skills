mod common;

use common::repo_root;
use std::path::Path;

/// Collect every `.lean` file under `dir`, as paths relative to `dir`.
/// Skips Lake build output.
fn lean_files(dir: &Path, rel: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read vendored lean_solana dir") {
        let entry = entry.expect("read entry");
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name == ".lake" || name == "build" {
                continue;
            }
            lean_files(&path, &rel.join(&name), out);
        } else if path.extension().is_some_and(|e| e == "lean") {
            out.push(rel.join(&name));
        }
    }
}

/// A bundled example that vendors `lean_solana/` must carry the same Lean
/// sources as the root library. A stale copy silently changes the trust
/// surface of that example (for example, keeping axioms the root library
/// has since proved).
#[test]
fn vendored_lean_solana_copies_match_the_root_library() {
    let root = repo_root();
    let source = root.join("lean_solana");
    let examples_root = root.join("examples/rust");
    let mut stale = Vec::new();
    let mut checked = 0;

    for entry in std::fs::read_dir(&examples_root).expect("read examples/rust") {
        let vendored = entry
            .expect("read example entry")
            .path()
            .join("formal_verification/lean_solana");
        if !vendored.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        lean_files(&vendored, Path::new(""), &mut files);
        for rel in files {
            checked += 1;
            let expected = std::fs::read_to_string(source.join(&rel));
            let actual = std::fs::read_to_string(vendored.join(&rel)).expect("read vendored file");
            match expected {
                Ok(expected) if expected == actual => {}
                Ok(_) => stale.push(format!("{} differs", vendored.join(&rel).display())),
                Err(_) => stale.push(format!(
                    "{} has no source in lean_solana/",
                    vendored.join(&rel).display()
                )),
            }
        }
    }

    assert!(
        checked > 0,
        "expected at least one vendored lean_solana copy"
    );
    assert!(
        stale.is_empty(),
        "vendored lean_solana copies are out of date; copy the files from lean_solana/:\n{}",
        stale.join("\n")
    );
}
