//! `qedgen reconcile` — unified drift report for coding agents.
//!
//! Consolidates two independent drift signals into a single report:
//!
//! 1. **Rust-side drift** — scans user-owned handler files for
//!    `#[qed(verified, spec = "...", handler = "...", spec_hash = "...")]`
//!    attributes, recomputes the spec fragment hash, and reports mismatches.
//!    This is the CLI-side complement to the compile-time proc-macro check:
//!    same hashing algorithm, but reports instead of failing the build.
//!
//! 2. **Lean-side drift** — delegates to
//!    `proofs_bootstrap::check_orphans` to find orphan theorems (handler
//!    dropped from spec) and missing theorems (new obligation added).
//!
//! Report-only. Never modifies files.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::check;
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

/// A parsed `#[qed(verified, ...)]` attribute with source position.
#[derive(Debug, Clone)]
struct QedAttr {
    line: usize,
    spec: Option<String>,
    handler: Option<String>,
    spec_hash: Option<String>,
}

/// Recursively collect `.rs` files under a directory. Mirrors the approach
/// used by `drift::walkdir` — no new dependency.
fn collect_rs_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if dir.is_file() {
        if dir.extension().is_some_and(|e| e == "rs") {
            out.push(dir.to_path_buf());
        }
        return Ok(out);
    }
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_rs_files(&path)?);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Walk up from a file's parent directory until a `Cargo.toml` is found.
/// Returns the directory that contains that manifest.
fn nearest_manifest_dir(file: &Path) -> Option<PathBuf> {
    let mut cur = file.parent()?.to_path_buf();
    loop {
        if cur.join("Cargo.toml").exists() {
            return Some(cur);
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return None,
        }
    }
}

/// Extract all `#[qed(verified, ...)]` attributes from Rust source text,
/// keyed by the 1-based line number the attribute starts on. Deliberately
/// regex-free: we scan for the `#[qed(` prefix, match the enclosing
/// `(...)` with depth tracking, and parse `key = "value"` pairs from the
/// interior. Handles multi-line attributes.
fn scan_qed_attrs(source: &str) -> Vec<QedAttr> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `#[qed(` (allow whitespace: `# [ qed (` — rare, but cheap to handle).
        if bytes[i] == b'#' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'[' {
                j += 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                // Match literal "qed"
                if j + 3 <= bytes.len() && &bytes[j..j + 3] == b"qed" {
                    let after_qed = j + 3;
                    let mut k = after_qed;
                    while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if k < bytes.len() && bytes[k] == b'(' {
                        // Find matching `)` respecting strings.
                        let inner_start = k + 1;
                        let mut cursor = inner_start;
                        let mut depth = 1i32;
                        let mut in_str = false;
                        let mut in_line_comment = false;
                        let mut in_block_comment = false;
                        while cursor < bytes.len() {
                            let b = bytes[cursor];
                            if in_line_comment {
                                if b == b'\n' {
                                    in_line_comment = false;
                                }
                                cursor += 1;
                                continue;
                            }
                            if in_block_comment {
                                if b == b'*'
                                    && cursor + 1 < bytes.len()
                                    && bytes[cursor + 1] == b'/'
                                {
                                    in_block_comment = false;
                                    cursor += 2;
                                    continue;
                                }
                                cursor += 1;
                                continue;
                            }
                            if in_str {
                                if b == b'\\' && cursor + 1 < bytes.len() {
                                    cursor += 2;
                                    continue;
                                }
                                if b == b'"' {
                                    in_str = false;
                                }
                                cursor += 1;
                                continue;
                            }
                            if b == b'/' && cursor + 1 < bytes.len() {
                                if bytes[cursor + 1] == b'/' {
                                    in_line_comment = true;
                                    cursor += 2;
                                    continue;
                                }
                                if bytes[cursor + 1] == b'*' {
                                    in_block_comment = true;
                                    cursor += 2;
                                    continue;
                                }
                            }
                            if b == b'"' {
                                in_str = true;
                                cursor += 1;
                                continue;
                            }
                            if b == b'(' {
                                depth += 1;
                            } else if b == b')' {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            cursor += 1;
                        }
                        if depth == 0 && cursor < bytes.len() {
                            let inner = &source[inner_start..cursor];
                            // The `verified` keyword is required before key=value pairs.
                            let first_word = inner
                                .trim_start()
                                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                                .next()
                                .unwrap_or("");
                            if first_word == "verified" {
                                let line = 1 + source[..i].bytes().filter(|&b| b == b'\n').count();
                                let (spec, handler, spec_hash) = parse_kv_pairs(inner);
                                out.push(QedAttr {
                                    line,
                                    spec,
                                    handler,
                                    spec_hash,
                                });
                            }
                            i = cursor + 1;
                            continue;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// Parse `key = "value"` pairs from the attribute interior.
/// Returns `(spec, handler, spec_hash)` — other keys (`hash`, etc.) are ignored.
fn parse_kv_pairs(interior: &str) -> (Option<String>, Option<String>, Option<String>) {
    let bytes = interior.as_bytes();
    let mut spec = None;
    let mut handler = None;
    let mut spec_hash = None;
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace + commas.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        // Read an identifier.
        let ident_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i == ident_start {
            i += 1;
            continue;
        }
        let ident = &interior[ident_start..i];
        // Look for `=` (with optional whitespace).
        let save = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            i = save;
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'"' {
            continue;
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let value = interior[val_start..i].to_string();
        i += 1;
        match ident {
            "spec" => spec = Some(value),
            "handler" => handler = Some(value),
            "spec_hash" => spec_hash = Some(value),
            _ => {}
        }
    }
    (spec, handler, spec_hash)
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
    if code_dir.exists() {
        let files = collect_rs_files(code_dir)?;
        for file in &files {
            let source = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let attrs = scan_qed_attrs(&source);
            for attr in attrs {
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
                        attr.line,
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
                            attr.line,
                            handler,
                        ));
                        continue;
                    }
                };

                if actual_hash != declared_hash {
                    rust_drift.push(RustDriftEntry {
                        file: file.display().to_string(),
                        line: attr.line,
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

        // Compute the real spec hash for `deposit` and stamp it.
        let spec_src = std::fs::read_to_string(&spec_path).unwrap();
        let deposit_hash = spec_hash::spec_hash_for_handler(&spec_src, "deposit").unwrap();
        let withdraw_hash = spec_hash::spec_hash_for_handler(&spec_src, "withdraw").unwrap();
        write_handler(&code_dir, "deposit", "demo.qedspec", &deposit_hash);
        write_handler(&code_dir, "withdraw", "demo.qedspec", &withdraw_hash);

        // Also write a Proofs.lean that has both expected theorems.
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
        // Round-trip.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("rust_drift").is_some());
        assert!(parsed.get("lean_orphans").is_some());
        assert!(parsed.get("lean_missing").is_some());
        assert_eq!(parsed["rust_drift"][0]["handler"], "deposit");
    }

    #[test]
    fn scan_qed_attrs_multiline() {
        let src = r#"
#[qed(verified,
      spec = "foo.qedspec",
      handler = "deposit",
      hash = "aaaa",
      spec_hash = "bbbb")]
pub fn deposit() {}
"#;
        let attrs = scan_qed_attrs(src);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].spec.as_deref(), Some("foo.qedspec"));
        assert_eq!(attrs[0].handler.as_deref(), Some("deposit"));
        assert_eq!(attrs[0].spec_hash.as_deref(), Some("bbbb"));
    }

    #[test]
    fn scan_ignores_non_qed_attrs() {
        let src = r#"
#[derive(Debug)]
#[allow(dead_code)]
#[qed(verified, spec = "a.qedspec", handler = "h", spec_hash = "cc")]
pub fn h() {}
"#;
        let attrs = scan_qed_attrs(src);
        assert_eq!(attrs.len(), 1);
    }

    #[test]
    fn scan_ignores_legacy_qed_without_verified() {
        // `#[qed(something_else)]` should not be parsed as a spec-bound attr.
        let src = r#"
#[qed(experimental)]
pub fn h() {}
"#;
        let attrs = scan_qed_attrs(src);
        assert_eq!(attrs.len(), 0);
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
