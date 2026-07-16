//! Dead-guard / unwired-error-variant sweep (#240).
//!
//! An error-enum variant that is *defined* but wired into no guard is a
//! named intention the code never enforces: the maintainer named the check
//! (the variant name often spells out the invariant) but no call site ever
//! fires it, so the path it was meant to protect proceeds unchecked. This
//! class is invisible to the per-category catalog and to the comparison-
//! direction / store-without-validate passes — there IS no guard to find a
//! coverage gap against; the guard exists in name only.
//!
//! The sweep was a prose manual-review pass in the auditor skill (§3f),
//! which a strong model under-executes (a benchmark run missed the class
//! entirely in one worker and mis-rated it in another). It is fully
//! deterministic, so it belongs in the tool: enumerate the program's
//! `#[error_code]` enum variants, grep each for an enforcement call-site in
//! the crate's `src/`, and emit one [`Candidate`] per zero-call-site
//! variant for the model to triage and severity-rate. "qedgen greps; the
//! model judges."
//!
//! Emitted as a [`Candidate`], never a [`Finding`]: a guard that is never
//! called is an *absence*, and an absence has no runnable reproducer
//! (`probes = reproducible only`). The candidate carries the severity rule
//! in its `investigation_hint` — a dead guard inherits the impact ceiling
//! of the path it fails to protect, not a dead-variant floor.
//!
//! Scope: `#[error_code]` enums (Anchor / Quasar), the runtimes that carry
//! a first-class error enum with named variants. Native `thiserror` enums
//! are a future extension; absent an `#[error_code]` enum the sweep is a
//! clean no-op (empty vec), never a false positive.

use anyhow::Result;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;

use crate::probe::scan_util::{byte_offset_to_line, line_is_commented};
use crate::probe::{Candidate, Category};

/// A variant declaration site: the enum it belongs to and where it is
/// defined, so a candidate can name the exact `file:line` of the dead guard.
struct VariantDecl {
    name: String,
    enum_name: String,
    rel_file: String,
    line: u32,
}

/// The byte span of an enum body in a specific file, so occurrences of a
/// variant name inside its own declaration are excluded from the call-site
/// grep (the declaration is not enforcement).
struct EnumBodySpan {
    file: std::path::PathBuf,
    start: usize,
    end: usize,
}

/// Entry point: enumerate `#[error_code]` variants under `<root>/src`, then
/// flag every variant with no enforcement call-site. No `src/`, no error
/// enum, or no dead variant each yield an empty vec (never an error).
pub fn scan_program(project_root: &Path) -> Result<Vec<Candidate>> {
    let src_dir = project_root.join("src");
    if !src_dir.exists() {
        return Ok(Vec::new());
    }
    let rs_files = crate::fs_walk::collect_rs_files(&src_dir, crate::fs_walk::DEFAULT_SKIP_DIRS);

    // Pass 1: enumerate error-enum variants + record each enum body span.
    let mut decls: Vec<VariantDecl> = Vec::new();
    let mut enum_spans: Vec<EnumBodySpan> = Vec::new();
    let mut sources: BTreeMap<std::path::PathBuf, String> = BTreeMap::new();
    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(project_root)
            .unwrap_or(file)
            .display()
            .to_string();
        for e in find_error_enums(&source) {
            enum_spans.push(EnumBodySpan {
                file: file.clone(),
                start: e.body_start,
                end: e.body_end,
            });
            for (variant, offset) in e.variants {
                decls.push(VariantDecl {
                    name: variant,
                    enum_name: e.enum_name.clone(),
                    rel_file: rel.clone(),
                    line: byte_offset_to_line(&source, offset),
                });
            }
        }
        sources.insert(file.clone(), source);
    }
    if decls.is_empty() {
        return Ok(Vec::new());
    }

    // Pass 2: for each variant, does any non-comment occurrence exist in
    // `src/` OUTSIDE its own enum declaration? Any such occurrence is
    // treated as wired (conservative — a bare-name match biases toward
    // not-flagging, so we only surface variants that are provably dead).
    let mut out: Vec<Candidate> = Vec::new();
    for decl in &decls {
        if !has_enforcement_call_site(&decl.name, &sources, &enum_spans) {
            out.push(unwired_candidate(decl));
        }
    }
    Ok(out)
}

/// A parsed `#[error_code]` enum: its name, its body byte-span, and each
/// variant with the byte offset of its declaration.
struct ErrorEnum {
    enum_name: String,
    body_start: usize,
    body_end: usize,
    variants: Vec<(String, usize)>,
}

/// Locate every `#[error_code]`-attributed `enum <Name> { ... }` in a file
/// and extract its variants. Brace-matched from the `{` after the enum
/// name, so nested braces (variant discriminants, doc-comment braces) don't
/// truncate the body early.
fn find_error_enums(source: &str) -> Vec<ErrorEnum> {
    // `#[error_code]` (optionally `#[error_code(offset = N)]`) followed,
    // possibly across other derives/attrs, by `... enum <Name> {`.
    let attr_re = Regex::new(r"#\[\s*error_code").expect("static regex compiles");
    let enum_re =
        Regex::new(r"enum\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*\{").expect("static regex compiles");
    let mut out = Vec::new();
    for attr in attr_re.find_iter(source) {
        // Find the first `enum <Name> {` at or after the attribute.
        let Some(caps) = enum_re.captures_at(source, attr.end()) else {
            continue;
        };
        let name = caps.name("name").unwrap().as_str().to_string();
        let brace_open = caps.get(0).unwrap().end() - 1; // index of `{`
        let Some(body_end) = match_brace(source, brace_open) else {
            continue;
        };
        let body = &source[brace_open + 1..body_end];
        let variants = extract_variants(body, brace_open + 1);
        out.push(ErrorEnum {
            enum_name: name,
            body_start: brace_open,
            body_end,
            variants,
        });
    }
    out
}

/// Return the byte index of the `}` matching the `{` at `open`, or `None`
/// if unbalanced. Skips braces inside `//` line comments and string
/// literals so a `{` in a `#[msg("...")]` string or comment can't unbalance
/// the count.
fn match_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    let mut in_str = false;
    let mut in_line_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
        } else if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            in_line_comment = true;
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Extract variant identifiers from an enum body. A variant is an
/// identifier at the start of a statement (after skipping attributes like
/// `#[msg("...")]` and doc comments). `body_offset` is the byte offset of
/// the body start in the original source, so returned offsets are absolute.
fn extract_variants(body: &str, body_offset: usize) -> Vec<(String, usize)> {
    // A variant declaration line: leading whitespace, a CamelCase ident,
    // then `,`, `{`, `(`, `=`, or end-of-line — never `::` (a path) or `(`
    // preceded by lowercase (a fn call). Attributes (`#[...]`) and doc
    // comments (`///`, `//`) are skipped by requiring the ident to be the
    // first token on the line.
    let re = Regex::new(r"(?m)^[ \t]*(?P<name>[A-Z][A-Za-z0-9_]*)\s*(?:,|\{|\(|=|$)")
        .expect("static regex compiles");
    let mut out = Vec::new();
    for caps in re.captures_iter(body) {
        let m = caps.name("name").unwrap();
        out.push((m.as_str().to_string(), body_offset + m.start()));
    }
    out
}

/// Does `variant` appear as an enforcement reference anywhere in `src/`
/// outside its own enum declaration and outside comments? Bare word-
/// boundary match: a reference to the variant name in Rust code is almost
/// always error construction (`err!` / `require_*!` / `return Err(..)` / a
/// match arm). Matching bare (not `Enum::Variant`) biases toward
/// not-flagging on name collisions — the safe direction, since a false
/// "unwired" claim is noise.
fn has_enforcement_call_site(
    variant: &str,
    sources: &BTreeMap<std::path::PathBuf, String>,
    enum_spans: &[EnumBodySpan],
) -> bool {
    let re =
        Regex::new(&format!(r"\b{}\b", regex::escape(variant))).expect("escaped regex compiles");
    for (file, source) in sources {
        for m in re.find_iter(source) {
            // Skip the occurrence inside the variant's own enum body.
            let in_own_decl = enum_spans
                .iter()
                .any(|span| &span.file == file && m.start() >= span.start && m.start() < span.end);
            if in_own_decl {
                continue;
            }
            if line_is_commented(source, m.start()) {
                continue;
            }
            return true;
        }
    }
    false
}

fn unwired_candidate(decl: &VariantDecl) -> Candidate {
    Candidate {
        category: Category::UnwiredErrorVariant,
        category_tag: Category::UnwiredErrorVariant.tag().to_string(),
        handler: decl.name.clone(),
        spec_silent_on: format!(
            "error variant `{}::{}` is defined at {}:{} but has no enforcement \
             call-site anywhere in `src/` — the named check is wired into no guard",
            decl.enum_name, decl.name, decl.rel_file, decl.line
        ),
        suppression_hint: "wire the variant into the guard it names (a \
                           `require!`/`require_*!`/`err!`/`return Err(..)` on the path it \
                           should protect), or delete the dead variant if the invariant is \
                           genuinely covered elsewhere"
            .to_string(),
        investigation_hint: "read the variant name and the path/handler it evidently guards, \
                             then ask whether the missing enforcement is exploitable. If its \
                             invariant is load-bearing (a signer / authority / limit the path \
                             assumes), the absent guard is a REAL finding — grade it at the \
                             impact ceiling of the path it fails to protect, NOT at a \
                             dead-variant floor (an unwired global-authority guard is HIGH, not \
                             LOW/INFO). If a different guard already covers the invariant \
                             redundantly, or the variant is deprecated/placeholder, it is INFO."
            .to_string(),
        reason: "deterministic dead-guard sweep — the error enum was enumerated and each \
                 variant grepped for an enforcement call-site in `src/`; an unwired guard is an \
                 absence, which has no runnable reproducer"
            .to_string(),
        repro_harness: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("qedgen-dead-guard-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    /// One enforced variant (`Unauthorized`, fired via `require!`) and one
    /// unenforced variant (`AdminGuardMissing`, defined only) → exactly the
    /// latter is emitted.
    #[test]
    fn flags_only_the_unenforced_variant() {
        let root = tmp_root("basic");
        std::fs::write(
            root.join("src/errors.rs"),
            r#"
use anchor_lang::prelude::*;

#[error_code]
pub enum MyError {
    #[msg("caller is not authorized")]
    Unauthorized,
    #[msg("admin guard was never wired")]
    AdminGuardMissing,
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            r#"
use crate::errors::MyError;
pub fn handler(is_admin: bool) -> Result<()> {
    require!(is_admin, MyError::Unauthorized);
    Ok(())
}
"#,
        )
        .unwrap();

        let cands = scan_program(&root).unwrap();
        let names: Vec<&str> = cands.iter().map(|c| c.handler.as_str()).collect();
        assert_eq!(names, vec!["AdminGuardMissing"], "got {names:?}");
        let c = &cands[0];
        assert_eq!(c.category_tag, "unwired_error_variant");
        assert!(c.spec_silent_on.contains("MyError::AdminGuardMissing"));
        assert!(c.spec_silent_on.contains("src/errors.rs:"));
        assert!(c.repro_harness.is_none());
        // Severity rule must ride along so the model does not mis-rate it low.
        assert!(c.investigation_hint.contains("impact ceiling"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A variant referenced only inside a `//` comment is still unwired.
    #[test]
    fn commented_reference_does_not_count_as_enforcement() {
        let root = tmp_root("commented");
        std::fs::write(
            root.join("src/errors.rs"),
            "#[error_code]\npub enum E {\n    NeverFired,\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "// TODO: return Err(E::NeverFired) when the guard is added\npub fn f() {}\n",
        )
        .unwrap();
        let cands = scan_program(&root).unwrap();
        assert_eq!(
            cands.iter().map(|c| c.handler.as_str()).collect::<Vec<_>>(),
            vec!["NeverFired"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `return Err(E::X.into())` and a bare match-arm reference both count
    /// as enforcement.
    #[test]
    fn return_err_and_match_arm_count_as_enforcement() {
        let root = tmp_root("wired-forms");
        std::fs::write(
            root.join("src/errors.rs"),
            "#[error_code]\npub enum E {\n    ViaReturn,\n    ViaMatch,\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            r#"
fn a() -> Result<()> { return Err(E::ViaReturn.into()); }
fn b(x: u8) -> Result<()> {
    match x {
        0 => Err(E::ViaMatch.into()),
        _ => Ok(()),
    }
}
"#,
        )
        .unwrap();
        let cands = scan_program(&root).unwrap();
        assert!(
            cands.is_empty(),
            "both wired; got {:?}",
            cands.iter().map(|c| &c.handler).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No `#[error_code]` enum → clean no-op, not a false positive.
    #[test]
    fn no_error_enum_is_a_silent_noop() {
        let root = tmp_root("no-enum");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub enum Plain { A, B }\npub fn f() {}\n",
        )
        .unwrap();
        assert!(scan_program(&root).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `{` inside a `#[msg("...")]` string must not truncate the enum body.
    #[test]
    fn brace_in_msg_string_does_not_truncate_body() {
        let root = tmp_root("brace-in-msg");
        std::fs::write(
            root.join("src/errors.rs"),
            "#[error_code]\npub enum E {\n    #[msg(\"unbalanced { brace in message\")]\n    FirstVariant,\n    SecondVariant,\n}\n",
        )
        .unwrap();
        // Neither is referenced → both should be flagged; the point is that
        // SecondVariant is still SEEN (the body didn't end at the `{`).
        let cands = scan_program(&root).unwrap();
        let mut names: Vec<&str> = cands.iter().map(|c| c.handler.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["FirstVariant", "SecondVariant"],
            "got {names:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
