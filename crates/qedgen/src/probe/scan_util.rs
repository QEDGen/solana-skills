//! Shared source-scanner toolkit for the text-pattern probes (T7d / F5).
//!
//! `arithmetic_symbol_probe`, `lifecycle_probe`, and
//! `paired_validator_probe` used to carry byte-identical copies of these
//! helpers under a "deliberately duplicated" banner; the copies had
//! started to drift (doc comments, salt handling). One copy, explicit
//! parameters.

use regex::Regex;
use std::path::Path;

/// Snap a byte index DOWN to the nearest char boundary (also clamps to
/// `s.len()`). The probes take fixed-byte context windows around a regex
/// match (`start - 2`, `end + 400`, …); an arithmetic offset can land inside
/// a multi-byte char (an `—` in a comment) and panic the slice (#187). The
/// window sizes are heuristic, so shrinking by ≤3 bytes is always safe.
pub(crate) fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut idx = idx.min(s.len());
    while !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Resolve a byte offset to a 1-indexed line number.
pub(crate) fn byte_offset_to_line(source: &str, offset: usize) -> u32 {
    let prefix = &source[..offset.min(source.len())];
    1 + prefix.chars().filter(|c| *c == '\n').count() as u32
}

/// True when a `//` precedes `offset` on the same line. Block comments
/// (`/* ... */`) are out of scope.
pub(crate) fn line_is_commented(source: &str, offset: usize) -> bool {
    let bytes = source.as_bytes();
    let mut i = offset.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    let line_prefix = &source[i..offset.min(source.len())];
    if let Some(idx) = line_prefix.find("//") {
        // Rough string-literal guard: even quote count before the `//`
        // means it isn't inside a string.
        let before = &line_prefix[..idx];
        let quote_count = before.chars().filter(|c| *c == '"').count();
        quote_count % 2 == 0
    } else {
        false
    }
}

/// Stable finding id: 16-hex-char prefix of `sha256("<file>:<line>:<salt>")`.
///
/// `salt` disambiguates rule families (`Category::tag()` for the
/// arithmetic probes; `"<family>:<key>"` composites for lifecycle /
/// paired-validator). Suppression files key on these ids — changing the
/// salt at a call site invalidates users' suppressions, so salts are
/// frozen even where they no longer match the category tag.
pub(crate) fn make_id(rel_file: &Path, line: u32, salt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(rel_file.display().to_string().as_bytes());
    h.update(b":");
    h.update(line.to_string().as_bytes());
    h.update(b":");
    h.update(salt.as_bytes());
    let id = format!("{:x}", h.finalize());
    id[..16].to_string()
}

/// Fn body after a decl: first `{` at or after `start` to its
/// brace-matched `}`. Unterminated bodies return the remainder.
pub(crate) fn body_after(source: &str, start: usize) -> Option<String> {
    let bytes = source.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let body_start = i + 1;
    let mut depth: i32 = 1;
    let mut j = body_start;
    while j < bytes.len() && depth > 0 {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(source[body_start..j].to_string());
        }
        j += 1;
    }
    Some(source[body_start..].to_string())
}

/// Body of the fn enclosing `offset` (brace-matched), so signals can be
/// checked across the whole fn rather than just the call site. Empty
/// string when `offset` is not inside a fn.
pub(crate) fn enclosing_fn_body(source: &str, offset: usize) -> String {
    let head = &source[..offset.min(source.len())];
    let fn_re = Regex::new(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*\s*[<\(]").expect("static regex");
    let Some(fn_match) = fn_re.find_iter(head).last() else {
        return String::new();
    };
    body_after(source, fn_match.start()).unwrap_or_default()
}

/// Nearest enclosing fn: `(decl_start_offset, name)`; `None` when not
/// inside a fn. The `[<\(]` terminator captures both bare and generic fns
/// (`fn init<'a, T>`).
pub(crate) fn enclosing_fn_start_and_name(source: &str, offset: usize) -> Option<(usize, String)> {
    let head = &source[..offset.min(source.len())];
    let re = Regex::new(r"fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<\(]").expect("static regex");
    re.captures_iter(head)
        .last()
        .map(|c| (c.get(0).expect("full fn match").start(), c[1].to_string()))
}

/// Test-fn name predicate, for inline `#[cfg(test)]` fns that the file
/// walker's directory skip list can't catch.
pub(crate) fn is_test_fn_name(fn_name: &str) -> bool {
    let lower = fn_name.to_ascii_lowercase();
    lower.starts_with("test_")
        || lower.starts_with("it_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn byte_offset_to_line_counts_newlines() {
        let src = "a\nb\nc";
        assert_eq!(byte_offset_to_line(src, 0), 1);
        assert_eq!(byte_offset_to_line(src, 2), 2);
        assert_eq!(byte_offset_to_line(src, 4), 3);
        assert_eq!(byte_offset_to_line(src, 999), 3);
    }

    #[test]
    fn line_is_commented_detects_leading_slashes() {
        let src = "let x = 1;\n// let y = 2;\nlet z = 3;";
        let y_off = src.find("let y").unwrap();
        let z_off = src.find("let z").unwrap();
        assert!(line_is_commented(src, y_off));
        assert!(!line_is_commented(src, z_off));
    }

    #[test]
    fn line_is_commented_string_guard() {
        let src = "let s = \"// not a comment\"; let t = 1;";
        let t_off = src.find("let t").unwrap();
        assert!(!line_is_commented(src, t_off));
    }

    #[test]
    fn make_id_is_stable_and_salted() {
        let f = PathBuf::from("src/lib.rs");
        let a = make_id(&f, 10, "rule_a");
        let b = make_id(&f, 10, "rule_b");
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
        assert_eq!(a, make_id(&f, 10, "rule_a"));
    }

    #[test]
    fn body_after_matches_nested_braces() {
        let src = "fn f() { if x { y } z }";
        assert_eq!(body_after(src, 0).unwrap(), " if x { y } z ");
        assert_eq!(body_after("no braces", 0), None);
    }

    #[test]
    fn enclosing_fn_body_finds_surrounding_fn() {
        let src = "fn outer() { let a = call_site(); }";
        let off = src.find("call_site").unwrap();
        assert_eq!(enclosing_fn_body(src, off), " let a = call_site(); ");
        assert_eq!(enclosing_fn_body("let x = 1;", 5), "");
    }

    #[test]
    fn enclosing_fn_start_and_name_handles_generics() {
        let src = "fn init<'a, T>(x: T) { touch(x); }";
        let off = src.find("touch").unwrap();
        let (start, name) = enclosing_fn_start_and_name(src, off).unwrap();
        assert_eq!(start, 0);
        assert_eq!(name, "init");
    }

    #[test]
    fn test_fn_name_shapes() {
        assert!(is_test_fn_name("test_transfer"));
        assert!(is_test_fn_name("it_works"));
        assert!(is_test_fn_name("transfer_test"));
        assert!(!is_test_fn_name("process_transfer"));
    }

    /// #187: window offsets snap DOWN to a char boundary and clamp to len.
    #[test]
    fn floor_char_boundary_snaps_and_clamps() {
        let s = "ab\u{2014}cd"; // — occupies bytes 2..5
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 2), 2);
        assert_eq!(floor_char_boundary(s, 3), 2);
        assert_eq!(floor_char_boundary(s, 4), 2);
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(floor_char_boundary(s, 99), s.len());
    }
}
