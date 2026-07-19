//! Run-to-run determinism regression test — PRD-v2.21 §"Slice 6".
//!
//! Asserts that `qedgen codegen --all --spec <path>` is **byte-identical**
//! across two consecutive invocations. The process-level `RandomState`
//! seed differs between runs; any code path that lets a `HashMap` or
//! `HashSet` drive output ordering surfaces as a non-zero `diff` here.
//!
//! Each bundled spec under `examples/rust/` runs twice; failure prints
//! the unified diff so the offending file is obvious. Single-spec and
//! multi-spec (directory-of-fragments) shapes both exercised.

mod common;

use common::{ensure_qedgen_built, qedgen_bin, repo_root};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Collect file contents under `dir` keyed by their relative path, so two
/// directory trees can be compared as ordered maps.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string();
            // .lake/ and target/ build outputs aren't part of codegen.
            if rel.starts_with(".lake/")
                || rel.contains("/.lake/")
                || rel.starts_with("target/")
                || rel.contains("/target/")
            {
                continue;
            }
            let bytes = fs::read(&p).unwrap_or_default();
            out.push((rel, bytes));
        }
    }
}

fn run_codegen(spec: &Path, out_dir: &Path) {
    // Canonicalize so the scaffold's spec-path relativization works on
    // macOS (`/var` tempdirs canonicalize to `/private/var`; a mixed
    // form makes strip_prefix fail and the stamp fall back to the
    // absolute — run-unique — tempdir path).
    let out_dir = &out_dir.canonicalize().expect("canonicalize out_dir");
    common::git_init(out_dir);
    let staged = common::stage_spec_surface(spec, out_dir);
    // No --output-dir override: the defaults resolve against the staged
    // spec's project root (#279), so everything lands under out_dir with
    // the standard layout and the `#[qed(spec = …)]` stamps relativize
    // to stable run-independent paths.
    let output = Command::new(qedgen_bin())
        .args(["codegen", "--all", "--spec"])
        .arg(&staged)
        .current_dir(out_dir)
        .output()
        .expect("spawn qedgen codegen");
    assert!(
        output.status.success(),
        "qedgen codegen failed on {}\nstdout: {}\nstderr: {}",
        spec.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_deterministic(spec: &Path) {
    let a = tempfile::tempdir().expect("tmpdir");
    let b = tempfile::tempdir().expect("tmpdir");
    run_codegen(spec, a.path());
    run_codegen(spec, b.path());
    let snap_a = snapshot(a.path());
    let snap_b = snapshot(b.path());
    if snap_a != snap_b {
        let mut diff_summary = String::new();
        let map_a: std::collections::BTreeMap<&String, &Vec<u8>> =
            snap_a.iter().map(|(k, v)| (k, v)).collect();
        let map_b: std::collections::BTreeMap<&String, &Vec<u8>> =
            snap_b.iter().map(|(k, v)| (k, v)).collect();
        for k in map_a.keys() {
            if map_a.get(k) != map_b.get(k) {
                diff_summary.push_str(&format!("differs: {k}\n"));
                let bytes_a = map_a.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
                let bytes_b = map_b.get(k).map(|v| v.as_slice()).unwrap_or(&[]);
                let str_a = String::from_utf8_lossy(bytes_a);
                let str_b = String::from_utf8_lossy(bytes_b);
                for (i, (la, lb)) in str_a.lines().zip(str_b.lines()).enumerate() {
                    if la != lb {
                        diff_summary
                            .push_str(&format!("  line {}:\n    A: {la}\n    B: {lb}\n", i + 1));
                        if diff_summary.len() > 4000 {
                            diff_summary.push_str("  …(truncated)\n");
                            break;
                        }
                    }
                }
            }
        }
        for k in map_b.keys() {
            if !map_a.contains_key(k) {
                diff_summary.push_str(&format!("only in B: {k}\n"));
            }
        }
        panic!(
            "codegen non-deterministic for {}:\n{diff_summary}",
            spec.display()
        );
    }
}

#[test]
fn escrow_codegen_is_deterministic() {
    ensure_qedgen_built();
    assert_deterministic(&repo_root().join("examples/rust/escrow/escrow.qedspec"));
}

#[test]
fn escrow_split_multi_spec_is_deterministic() {
    ensure_qedgen_built();
    assert_deterministic(&repo_root().join("examples/rust/escrow-split"));
}

#[test]
fn lending_codegen_is_deterministic() {
    ensure_qedgen_built();
    assert_deterministic(&repo_root().join("examples/rust/lending/lending.qedspec"));
}

#[test]
fn multisig_codegen_is_deterministic() {
    ensure_qedgen_built();
    assert_deterministic(&repo_root().join("examples/rust/multisig/multisig.qedspec"));
}
