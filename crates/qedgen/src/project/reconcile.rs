//! `qedgen reconcile` — unified drift report for coding agents. Report-only.
//!
//! Two signals: (1) Rust-side — rescans `#[qed(verified, ...)]` attributes
//! and recomputes the spec fragment hash (same algorithm as the compile-time
//! proc-macro check, but reports instead of failing the build); (2)
//! Lean-side — `proofs_bootstrap::check_orphans` for orphan / missing
//! theorems.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::check;
use crate::drift;
use crate::proofs_bootstrap::{self, OrphanFinding};
use crate::spec_hash;

/// A single Rust-side drift entry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RustDriftEntry {
    pub file: String,
    pub line: usize,
    pub handler: String,
    pub expected_spec_hash: String,
    pub actual_spec_hash: String,
}

/// A single Lean orphan theorem entry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LeanOrphanEntry {
    pub theorem: String,
    pub reason: String,
}

/// A single Lean missing-theorem entry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LeanMissingEntry {
    pub theorem: String,
    pub snippet: String,
}

/// Full reconcile report. Serialized to JSON with `--json`.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub spec: String,
    pub rust_drift: Vec<RustDriftEntry>,
    pub lean_orphans: Vec<LeanOrphanEntry>,
    pub lean_missing: Vec<LeanMissingEntry>,
    /// Non-fatal warnings (e.g. attribute points at a different spec).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Report {
    pub fn has_drift(&self) -> bool {
        !self.rust_drift.is_empty()
            || !self.lean_orphans.is_empty()
            || !self.lean_missing.is_empty()
    }
}

/// Walk up from a file's parent directory until a `Cargo.toml` is found.
/// Returns the directory that contains that manifest.
fn nearest_manifest_dir(file: &Path) -> Option<PathBuf> {
    let mut cur = file.parent()?.to_path_buf();
    loop {
        if cur.join("Cargo.toml").exists() {
            return Some(cur);
        }
        cur = cur.parent()?.to_path_buf();
    }
}

/// Run the full reconcile check. Report-only — never modifies files.
pub fn reconcile(spec_path: &Path, code_dir: &Path, proofs_dir: &Path) -> Result<Report> {
    let spec_display = spec_path.display().to_string();
    let spec_src = std::fs::read_to_string(spec_path)
        .with_context(|| format!("reading spec file {}", spec_path.display()))?;
    let spec_path_canonical = spec_path
        .canonicalize()
        .unwrap_or_else(|_| spec_path.to_path_buf());

    let mut rust_drift = Vec::new();
    let mut warnings = Vec::new();

    // Rust side: scan .rs files for #[qed(verified, ...)] attributes.
    // Attribute parsing is shared with `qedgen verify --drift`
    // (`drift::scan_verified_fns`) so the two commands agree on the
    // attribute grammar by construction.
    if code_dir.exists() {
        let files = crate::fs_walk::collect_rs_files(code_dir, crate::fs_walk::DEFAULT_SKIP_DIRS);
        for file in &files {
            let source = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let entries = match drift::scan_verified_fns(&source) {
                Ok(e) => e,
                Err(_) => continue, // not parseable Rust — skip
            };
            for entry in entries {
                let attr = &entry.attr;
                let (attr_spec, handler, declared_hash) =
                    match (&attr.spec, &attr.handler, &attr.spec_hash) {
                        (Some(s), Some(h), Some(sh)) if !sh.is_empty() => {
                            (s.clone(), h.clone(), sh.clone())
                        }
                        _ => continue,
                    };

                // Resolve the spec path relative to the nearest Cargo.toml.
                let manifest_dir = nearest_manifest_dir(file)
                    .unwrap_or_else(|| file.parent().unwrap_or(Path::new(".")).to_path_buf());
                let attr_spec_path = manifest_dir.join(&attr_spec);
                let attr_spec_canonical = attr_spec_path
                    .canonicalize()
                    .unwrap_or_else(|_| attr_spec_path.clone());

                if attr_spec_canonical != spec_path_canonical {
                    warnings.push(format!(
                        "{}:{}: attribute references spec `{}` (resolved to `{}`) but --spec is `{}` — skipping",
                        file.display(),
                        entry.attr_line,
                        attr_spec,
                        attr_spec_canonical.display(),
                        spec_path_canonical.display(),
                    ));
                    continue;
                }

                let actual_hash = match spec_hash::spec_hash_for_handler(&spec_src, &handler) {
                    Some(h) => h,
                    None => {
                        warnings.push(format!(
                            "{}:{}: handler `{}` not found in spec — skipping",
                            file.display(),
                            entry.attr_line,
                            handler,
                        ));
                        continue;
                    }
                };

                if actual_hash != declared_hash {
                    rust_drift.push(RustDriftEntry {
                        file: file.display().to_string(),
                        line: entry.attr_line,
                        handler,
                        expected_spec_hash: declared_hash,
                        actual_spec_hash: actual_hash,
                    });
                }
            }
        }
    }

    // Lean side: reuse check_orphans so the logic stays in one place.
    let (lean_orphans, lean_missing) = if proofs_dir.exists() {
        let parsed = check::parse_spec_file(spec_path)?;
        let findings = proofs_bootstrap::check_orphans(&parsed, proofs_dir)?;
        let mut orphans = Vec::new();
        let mut missing = Vec::new();
        for f in findings {
            match f {
                OrphanFinding::Orphan(name) => {
                    let reason = orphan_reason(&name, &parsed);
                    orphans.push(LeanOrphanEntry {
                        theorem: name,
                        reason,
                    });
                }
                OrphanFinding::Missing(name) => {
                    let snippet = format!("theorem {} ... := by sorry", name);
                    missing.push(LeanMissingEntry {
                        theorem: name,
                        snippet,
                    });
                }
                // #166: a Proofs.lean from a DIFFERENT spec is a workspace-
                // hygiene note, not per-theorem drift — surface as a warning.
                f @ OrphanFinding::ForeignProofs { .. } => {
                    warnings.push(f.to_string());
                }
            }
        }
        (orphans, missing)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(Report {
        spec: spec_display,
        rust_drift,
        lean_orphans,
        lean_missing,
        warnings,
    })
}

/// Best-effort explanation for why a theorem is orphaned. Parses the
/// conventional `<property>_preserved_by_<handler>` shape and reports
/// whichever side the spec no longer has.
fn orphan_reason(theorem: &str, spec: &check::ParsedSpec) -> String {
    let Some(idx) = theorem.find("_preserved_by_") else {
        return format!(
            "Theorem `{}` does not match an obligation declared in spec",
            theorem
        );
    };
    let prop = &theorem[..idx];
    let handler = &theorem[idx + "_preserved_by_".len()..];
    let handler_declared = spec.handlers.iter().any(|h| h.name == handler);
    let prop_declared = spec.properties.iter().any(|p| p.name == prop);
    if !handler_declared {
        format!("Handler `{}` no longer declared in spec", handler)
    } else if !prop_declared {
        format!("Property `{}` no longer declared in spec", prop)
    } else {
        format!(
            "Property `{}` no longer marked `preserved_by {}` in spec",
            prop, handler
        )
    }
}

/// Human-readable rendering of the reconcile report. Modelled after
/// `qedgen check`'s lint output: one issue per stanza, a `Fix:` line the
/// agent can act on.
pub fn print_report(report: &Report) {
    for w in &report.warnings {
        eprintln!("warning: {}", w);
    }

    let total = report.rust_drift.len() + report.lean_orphans.len() + report.lean_missing.len();
    if total == 0 {
        eprintln!("Spec, code, and proofs are in sync — no drift detected.");
        return;
    }

    eprintln!("Drift detected against {}:\n", report.spec);

    if !report.rust_drift.is_empty() {
        eprintln!("Rust handlers ({}):", report.rust_drift.len());
        for d in &report.rust_drift {
            eprintln!(
                "  {}:{}  handler `{}`  SPEC HASH DRIFT",
                d.file, d.line, d.handler
            );
            eprintln!("    Expected: {}", d.expected_spec_hash);
            eprintln!("    Actual:   {}", d.actual_spec_hash);
            eprintln!(
                "    Fix: update the handler body to match the spec, or update `spec_hash = \"{}\"` if the spec change is intentional.",
                d.actual_spec_hash
            );
        }
        eprintln!();
    }

    if !report.lean_orphans.is_empty() {
        eprintln!("Lean orphan theorems ({}):", report.lean_orphans.len());
        for o in &report.lean_orphans {
            eprintln!("  theorem `{}`  ORPHAN", o.theorem);
            eprintln!("    Reason: {}", o.reason);
            eprintln!(
                "    Fix: delete `{}` from Proofs.lean, or restore the spec declaration.",
                o.theorem
            );
        }
        eprintln!();
    }

    if !report.lean_missing.is_empty() {
        eprintln!("Lean missing theorems ({}):", report.lean_missing.len());
        for m in &report.lean_missing {
            eprintln!("  theorem `{}`  MISSING", m.theorem);
            eprintln!("    Fix: add to Proofs.lean:");
            eprintln!("      {}", m.snippet);
        }
        eprintln!();
    }

    eprintln!(
        "{} drift issue(s): {} Rust, {} orphan, {} missing",
        total,
        report.rust_drift.len(),
        report.lean_orphans.len(),
        report.lean_missing.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    const SPEC: &str = r#"spec Demo

type State
  | Active of { count : U64 }

type Error
  | Overflow
  | Underflow

handler deposit (amount : U64) : State.Active -> State.Active {
  requires state.count + amount <= 100 else Overflow
  effect { count += amount }
}

handler withdraw (amount : U64) : State.Active -> State.Active {
  requires state.count >= amount else Underflow
  effect { count -= amount }
}

property count_bounded :
  state.count <= 100
  preserved_by [deposit, withdraw]
"#;

    fn fake_project(spec: &str) -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Project layout: spec + Cargo.toml at root, handlers under src/.
        let spec_path = root.join("demo.qedspec");
        std::fs::write(&spec_path, spec).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"demo\"\nversion=\"0.0.1\"\nedition=\"2021\"\n",
        )
        .unwrap();
        let code_dir = root.join("src");
        std::fs::create_dir_all(&code_dir).unwrap();
        let proofs_dir = root.join("formal_verification");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        (dir, spec_path, code_dir, proofs_dir)
    }

    fn write_handler(code_dir: &Path, name: &str, spec_name: &str, spec_hash: &str) -> PathBuf {
        let path = code_dir.join(format!("{}.rs", name));
        let body = format!(
            r#"
#[qed(verified, spec = "{}", handler = "{}", hash = "aaaaaaaaaaaaaaaa", spec_hash = "{}")]
pub fn {}(amount: u64) -> u64 {{
    amount + 1
}}
"#,
            spec_name, name, spec_hash, name,
        );
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn clean_state_no_drift() {
        let (_dir, spec_path, code_dir, proofs_dir) = fake_project(SPEC);

        let spec_src = std::fs::read_to_string(&spec_path).unwrap();
        let deposit_hash = spec_hash::spec_hash_for_handler(&spec_src, "deposit").unwrap();
        let withdraw_hash = spec_hash::spec_hash_for_handler(&spec_src, "withdraw").unwrap();
        write_handler(&code_dir, "deposit", "demo.qedspec", &deposit_hash);
        write_handler(&code_dir, "withdraw", "demo.qedspec", &withdraw_hash);

        std::fs::write(
            proofs_dir.join("Proofs.lean"),
            "theorem count_bounded_preserved_by_deposit : True := trivial\n\
             theorem count_bounded_preserved_by_withdraw : True := trivial\n",
        )
        .unwrap();

        let report = reconcile(&spec_path, &code_dir, &proofs_dir).unwrap();
        assert!(
            report.rust_drift.is_empty(),
            "expected no rust drift, got {:?}",
            report.rust_drift
        );
        assert!(report.lean_orphans.is_empty());
        assert!(report.lean_missing.is_empty());
        assert!(!report.has_drift());
    }

    #[test]
    fn detects_planted_rust_drift() {
        let (_dir, spec_path, code_dir, _proofs_dir) = fake_project(SPEC);
        let planted = write_handler(&code_dir, "deposit", "demo.qedspec", "deadbeefdeadbeef");

        let report = reconcile(&spec_path, &code_dir, &code_dir /* no proofs */).unwrap();
        assert_eq!(report.rust_drift.len(), 1);
        let d = &report.rust_drift[0];
        assert_eq!(d.handler, "deposit");
        assert_eq!(d.expected_spec_hash, "deadbeefdeadbeef");
        assert!(d.file.ends_with("deposit.rs"));
        assert!(d.line >= 1);
        let _ = planted;
    }

    #[test]
    fn detects_missing_theorem() {
        let (_dir, spec_path, code_dir, proofs_dir) = fake_project(SPEC);
        // Proofs.lean only covers deposit — withdraw is missing.
        std::fs::write(
            proofs_dir.join("Proofs.lean"),
            "theorem count_bounded_preserved_by_deposit : True := trivial\n",
        )
        .unwrap();

        let report = reconcile(&spec_path, &code_dir, &proofs_dir).unwrap();
        assert_eq!(report.lean_missing.len(), 1);
        assert_eq!(
            report.lean_missing[0].theorem,
            "count_bounded_preserved_by_withdraw"
        );
        assert!(report.lean_missing[0].snippet.contains("sorry"));
    }

    #[test]
    fn detects_orphan_theorem() {
        let (_dir, spec_path, code_dir, proofs_dir) = fake_project(SPEC);
        std::fs::write(
            proofs_dir.join("Proofs.lean"),
            "theorem count_bounded_preserved_by_deposit : True := trivial\n\
             theorem count_bounded_preserved_by_withdraw : True := trivial\n\
             theorem count_bounded_preserved_by_ghost : True := trivial\n",
        )
        .unwrap();

        let report = reconcile(&spec_path, &code_dir, &proofs_dir).unwrap();
        assert_eq!(report.lean_orphans.len(), 1);
        let o = &report.lean_orphans[0];
        assert_eq!(o.theorem, "count_bounded_preserved_by_ghost");
        assert!(o.reason.contains("ghost"));
    }

    #[test]
    fn json_output_is_valid() {
        let (_dir, spec_path, code_dir, proofs_dir) = fake_project(SPEC);
        write_handler(&code_dir, "deposit", "demo.qedspec", "deadbeefdeadbeef");
        std::fs::write(
            proofs_dir.join("Proofs.lean"),
            "theorem count_bounded_preserved_by_deposit : True := trivial\n",
        )
        .unwrap();

        let report = reconcile(&spec_path, &code_dir, &proofs_dir).unwrap();
        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("rust_drift").is_some());
        assert!(parsed.get("lean_orphans").is_some());
        assert!(parsed.get("lean_missing").is_some());
        assert_eq!(parsed["rust_drift"][0]["handler"], "deposit");
    }

    // Attribute-grammar coverage for the shared scanner reconcile now
    // rides on (`drift::scan_verified_fns`), exercised through reconcile's
    // needs: multi-line attrs, line numbers, non-qed attrs, legacy shapes.
    #[test]
    fn shared_scanner_multiline_attr_with_line_number() {
        let src = r#"
#[qed(verified,
      spec = "foo.qedspec",
      handler = "deposit",
      hash = "aaaa",
      spec_hash = "bbbb")]
pub fn deposit() {}
"#;
        let entries = drift::scan_verified_fns(src).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attr.spec.as_deref(), Some("foo.qedspec"));
        assert_eq!(entries[0].attr.handler.as_deref(), Some("deposit"));
        assert_eq!(entries[0].attr.spec_hash.as_deref(), Some("bbbb"));
        // The attribute starts on line 2 (1-based; leading newline).
        assert_eq!(entries[0].attr_line, 2);
    }

    #[test]
    fn shared_scanner_ignores_non_qed_attrs() {
        let src = r#"
#[derive(Debug)]
#[qed(verified, spec = "a.qedspec", handler = "h", spec_hash = "cc")]
pub fn h() {}
"#;
        let entries = drift::scan_verified_fns(src).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attr_line, 3);
    }

    #[test]
    fn shared_scanner_ignores_legacy_qed_without_verified() {
        let src = r#"
#[qed(experimental)]
pub fn h() {}
"#;
        let entries = drift::scan_verified_fns(src).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn warning_on_cross_spec_attribute() {
        let (_dir, spec_path, code_dir, _proofs_dir) = fake_project(SPEC);
        // Planted attribute points at a spec that does not exist. The path
        // won't canonicalize to the real spec, so reconcile should warn
        // and skip, not error.
        write_handler(&code_dir, "deposit", "../wrong.qedspec", "deadbeefdeadbeef");
        let report = reconcile(&spec_path, &code_dir, &code_dir).unwrap();
        assert!(report.rust_drift.is_empty());
        assert!(!report.warnings.is_empty());
        assert!(report.warnings[0].contains("wrong.qedspec"));
    }
}
