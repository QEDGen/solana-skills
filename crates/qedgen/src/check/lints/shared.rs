//! Cross-cutting helpers shared across the lint rules: word-boundary
//! matching, comparison parsing, field-type classification, and the
//! overflow-risk / `old(...)` predicates several rules build on.

use super::*;

/// Whole-word match: boundaries are start/end of string or any non-alphanumeric, non-underscore byte.
pub(crate) fn contains_word(haystack: &str, needle: &str) -> bool {
    for (i, _) in haystack.match_indices(needle) {
        let before_ok = i == 0 || {
            let b = haystack.as_bytes()[i - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after = i + needle.len();
        let after_ok = after >= haystack.len() || {
            let b = haystack.as_bytes()[after];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// Split a rendered Rust comparison `<lhs> <op> <rhs>` at the top-level
/// comparison operator (string-level, no AST). Top-level = not inside
/// parens, generic args (`Vec<...>`), or `[...]` indices; first depth-0
/// comparison wins, with `==`/`!=`/`<=`/`>=` matched before `<`/`>`.
/// `None` if the expression isn't a top-level comparison.
pub(crate) fn parse_top_level_cmp(expr: &str) -> Option<(&str, &str, &str)> {
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'(' | b'[' | b'<' => {
                // `<` could be the comparison or the start of a generic.
                // Heuristic: if the next char is `=`, it's `<=` — handle
                // below. Otherwise treat `<` as depth-increment only when
                // preceded by an alphanumeric (generic) or whitespace
                // around a punctuation form is the comparison case.
                if b == b'<' {
                    let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                    let next = if i + 1 < bytes.len() {
                        bytes[i + 1]
                    } else {
                        b' '
                    };
                    // `<=` — comparison
                    if next == b'=' && depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 2..].trim();
                        return Some((lhs, "<=", rhs));
                    }
                    // bare `<` at depth 0 after an identifier could be a
                    // generic-list start (e.g. `Vec<u8>`). Treat as depth
                    // increment in that case.
                    if prev.is_ascii_alphanumeric() || prev == b'_' {
                        depth += 1;
                    } else if depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 1..].trim();
                        return Some((lhs, "<", rhs));
                    }
                } else {
                    depth += 1;
                }
            }
            b')' | b']' | b'>' => {
                if b == b'>' {
                    let next = if i + 1 < bytes.len() {
                        bytes[i + 1]
                    } else {
                        b' '
                    };
                    if next == b'=' && depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 2..].trim();
                        return Some((lhs, ">=", rhs));
                    }
                    if depth > 0 {
                        depth -= 1;
                    } else if depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 1..].trim();
                        return Some((lhs, ">", rhs));
                    }
                } else if depth > 0 {
                    depth -= 1;
                }
            }
            b'=' => {
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' && depth == 0 {
                    let lhs = expr[..i].trim();
                    let rhs = expr[i + 2..].trim();
                    return Some((lhs, "==", rhs));
                }
            }
            b'!' => {
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' && depth == 0 {
                    let lhs = expr[..i].trim();
                    let rhs = expr[i + 2..].trim();
                    return Some((lhs, "!=", rhs));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parsed form of a field type string. Captures the distinction between a
/// plain type (e.g. `U128`, `Account`) and a bounded map (`Map[N] T`).
///
/// Only `Map { .. }` is inspected by the current consumer; `Simple` carries
/// the trimmed type string for future linting passes (e.g., primitive-type
/// checks, alias resolution) and intentionally remains exhaustive.
#[derive(Debug)]
pub(crate) enum FieldTypeShape<'a> {
    Simple(#[allow(dead_code)] &'a str),
    Map { bound: &'a str, inner: &'a str },
}

/// Parse a field-type source string into a structured view.
/// Returns `Simple` for `U128`, `Account`, `Vec U64` and `Map { ... }` for
/// `Map[CONST] T` (bound and inner trimmed).
pub(crate) fn classify_field_type(s: &str) -> FieldTypeShape<'_> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound = rest[..close].trim();
                let inner = rest[close + 1..].trim();
                return FieldTypeShape::Map { bound, inner };
            }
        }
    }
    FieldTypeShape::Simple(trimmed)
}

pub(crate) fn make_old_in_single_state_warning(
    holder: &str,
    kind: &str,
    body_snippet: &str,
) -> CompletenessWarning {
    CompletenessWarning {
        rule: "old_in_single_state_context".to_string(),
        severity: Severity::Warning,
        priority: 1,
        message: format!(
            "'{}' uses `old(...)` inside a `{}` body ({}) — only meaningful in \
             `ensures` or `property` bodies (a binary transition context). \
             `requires` and `invariant` describe a single state and have no \
             \"old\" value to reference.",
            holder, kind, body_snippet
        ),
        subject: Some(holder.to_string()),
        fix: "If you meant a precondition on the pre-state, drop `old(...)` \
              and reference `state.x` directly. If you meant a property across \
              the transition, lift the clause into a `property X : ... \
              preserved_by Y`."
            .to_string(),
        example: None,
        counterexample: None,
        fix_options: vec![],
    }
}

/// Predicate shared with `kani_impl::spec_triggers_impl_harness`: true iff
/// a ref_impl carries arithmetic that could overflow on bounded Rust types
/// (the Lean lowering on `Nat`/`Int` cannot). Used as both a lint trigger
/// and the impl-targeted Kani auto-trigger so ref_impl-bearing specs always
/// get the bit-width-bounded verification surface.
pub fn ref_impl_has_overflow_risk(r: &ParsedRefImpl) -> bool {
    let has_numeric_io = std::iter::once(&r.return_type)
        .chain(r.params.iter().map(|(_, t)| t))
        .any(|t| {
            matches!(
                t.trim(),
                "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128"
            )
        });
    if !has_numeric_io {
        return false;
    }
    // Pure-expression bodies — `*` is always multiplication, `<<` is always
    // left-shift, `+`/`-` are always add/sub (no pointer arithmetic, no
    // unary `-` ambiguity in our DSL emission). A simple substring check
    // is sufficient and the lint's false-positive cost is "user is told
    // to run Kani" — tolerable.
    let body = &r.rust_body;
    body.contains('*') || body.contains("<<") || body.contains('+') || body.contains('-')
}
