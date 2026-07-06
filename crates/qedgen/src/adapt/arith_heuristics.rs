//! Shared raw-arithmetic heuristics for the extractor twins (T7d / F27).
//!
//! `anchor_extractor` and `native_extractor` carried byte-identical copies
//! of `contains_assignment` / `has_arith_operator`, and `scan_arith_sites`
//! variants differing only by the Anchor-attribute filter — parameterized
//! here as `skip_line`.

use regex::Regex;

/// 1-based line numbers of raw arithmetic outside the
/// `checked_*`/`saturating_*`/`wrapping_*`/`overflowing_*` family.
/// `skip_line(line_idx, line)` (0-based idx) lets the Anchor extractor
/// exclude `#[account(...)]` attribute spans and attr-field assignments —
/// macro-expansion-time layout math, not runtime arithmetic.
pub(crate) fn scan_arith_sites(
    source: &str,
    checked_call: &Regex,
    skip_line: impl Fn(usize, &str) -> bool,
) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, line) in source.lines().enumerate() {
        if skip_line(i, line) {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }
        if checked_call.is_match(line) {
            continue;
        }
        if !contains_assignment(line) {
            continue;
        }
        let after_eq = line.split_once('=').map(|(_, r)| r).unwrap_or("");
        if has_arith_operator(after_eq) {
            out.push(i + 1);
        }
    }
    out
}

/// `=` not preceded by `<>!=` and not part of `==` — accepts `x = y`,
/// `x += y`; rejects `x == y`, `a <= b`.
pub(crate) fn contains_assignment(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b' '
            };
            // Skip `==`, `!=`, `<=`, `>=`. Accept `=`, `+=`, `-=`, `*=`, `/=`.
            if next == b'=' || matches!(prev, b'!' | b'<' | b'>' | b'=') {
                continue;
            }
            return true;
        }
    }
    false
}

/// `+ - * /` outside compound assignments, `//` comments, and `->`.
pub(crate) fn has_arith_operator(s: &str) -> bool {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b'+' | b'-' | b'*' | b'/') {
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b' '
            };
            if next == b'=' {
                continue;
            }
            if b == b'/' && next == b'/' {
                // Comment marker.
                return false;
            }
            // Skip `->` (return arrow).
            if b == b'-' && next == b'>' {
                continue;
            }
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_shapes() {
        assert!(contains_assignment("x = y"));
        assert!(contains_assignment("x += y"));
        assert!(!contains_assignment("x == y"));
        assert!(!contains_assignment("a <= b"));
        assert!(!contains_assignment("a >= b"));
        assert!(!contains_assignment("a != b"));
    }

    #[test]
    fn arith_operator_shapes() {
        assert!(has_arith_operator("a + b"));
        assert!(has_arith_operator("a * b"));
        assert!(!has_arith_operator("f() -> u64"));
        assert!(!has_arith_operator("// a + b"));
        assert!(!has_arith_operator("a += 1".split_once('=').unwrap().1)); // "= 1" tail
    }

    #[test]
    fn scan_finds_raw_sites_and_honors_filters() {
        let checked =
            Regex::new(r"\b(?:checked|saturating|wrapping|overflowing)_(?:add|sub|mul|div|rem)\b")
                .unwrap();
        let src =
            "let a = b + c;\nlet d = b.checked_add(c);\n// let e = b + c;\nskip_me = x + 1;\n";
        let sites = scan_arith_sites(src, &checked, |_, line| line.starts_with("skip_me"));
        assert_eq!(sites, vec![1]);
    }
}
