//! Low-level string/comparison primitives: top-level operator detection,
//! balanced-paren stripping, comparison-boundary scanning, and whole-word
//! identifier matching. Shared across guard/pubkey/property lowering.

use super::*;

pub fn negate_simple_top_level_comparison(expr: &str) -> Option<String> {
    let trimmed = strip_balanced_outer_parens(expr.trim());
    if contains_top_level_logical_op(trimmed) {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            _ => {}
        }

        if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 {
            for (op, negated) in [
                ("==", "!="),
                ("!=", "=="),
                (">=", "<"),
                ("<=", ">"),
                (">", "<="),
                ("<", ">="),
            ] {
                if trimmed[i..].starts_with(op) {
                    let lhs = trimmed[..i].trim();
                    let rhs = trimmed[i + op.len()..].trim();
                    if !lhs.is_empty() && !rhs.is_empty() {
                        return Some(format!("{lhs} {negated} {rhs}"));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn contains_top_level_logical_op(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => in_string = true,
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'&' | b'|'
                if i + 1 < bytes.len()
                    && bytes[i + 1] == b
                    && paren_depth == 0
                    && bracket_depth == 0
                    && brace_depth == 0 =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn strip_balanced_outer_parens(mut expr: &str) -> &str {
    loop {
        let trimmed = expr.trim();
        if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
            return trimmed;
        }
        let inner = &trimmed[1..trimmed.len() - 1];
        if split_top_level_and(inner).len() == 1 && outer_parens_are_balanced(trimmed) {
            expr = inner;
        } else {
            return trimmed;
        }
    }
}

fn outer_parens_are_balanced(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, b) in bytes.iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && idx + 1 < bytes.len() {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

pub(crate) fn find_next_equality_op(expr: &str, from: usize) -> Option<(usize, &'static str)> {
    let eq = expr[from..].find(" == ").map(|p| (from + p, " == "));
    let ne = expr[from..].find(" != ").map(|p| (from + p, " != "));
    match (eq, ne) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

pub(crate) fn find_cmp_lhs_start(expr: &str, op_start: usize) -> usize {
    let bytes = expr.as_bytes();
    let mut i = op_start;
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }

    let mut depth = 0usize;
    while i > 0 {
        if depth == 0 && i >= 2 && (&expr[i - 2..i] == "&&" || &expr[i - 2..i] == "||") {
            break;
        }

        let b = bytes[i - 1];
        match b {
            b')' => {
                depth += 1;
                i -= 1;
            }
            b'(' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                i -= 1;
            }
            b',' if depth == 0 => break,
            _ => i -= 1,
        }
    }
    i
}

pub(crate) fn find_cmp_rhs_end(expr: &str, op_end: usize) -> usize {
    let bytes = expr.as_bytes();
    let mut i = op_end;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }

    let mut depth = 0usize;
    while i < bytes.len() {
        if depth == 0 && i + 1 < bytes.len() && (&expr[i..i + 2] == "&&" || &expr[i..i + 2] == "||")
        {
            break;
        }

        match bytes[i] {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                i += 1;
            }
            b',' if depth == 0 => break,
            _ => i += 1,
        }
    }
    i
}

/// True if `hay` contains `needle` as a whole word (not a substring of a
/// longer identifier). `net` in `amount - net` matches; `net` in `network`
/// does not. Alias of the shared scanner in `codegen_shared`.
pub(crate) fn contains_whole_word(hay: &str, needle: &str) -> bool {
    crate::codegen_shared::contains_word_boundary(hay, needle)
}
