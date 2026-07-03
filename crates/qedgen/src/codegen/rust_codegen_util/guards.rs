//! Guard translation: qedspec guard/requires expressions → Rust, plus the
//! top-level `&&` splitter and handler-account-pubkey suppression used to
//! drop accounts-only requires from the pure-model harness projection.

use super::*;

/// Translate a qedspec guard expression to Rust syntax.
///
/// Handles: state.field → s.field, Unicode operators → ASCII,
/// Lean `=` equality → Rust `==`.
pub fn translate_guard_to_rust(guard: &str, wrapping: bool) -> String {
    let result = guard
        .replace("state.", "s.")
        .replace('≤', "<=")
        .replace('≥', ">=")
        .replace('∧', "&&")
        .replace('∨', "||")
        .replace('≠', "!=")
        .replace(" and ", " && ")
        .replace(" or ", " || ");
    // Lean uses `=` for equality; Rust needs `==`. Replace standalone ` = `
    // that isn't part of `<=`, `>=`, `!=`, or `==`.
    let result = fix_equality_operator(&result);
    if wrapping {
        wrap_arithmetic(&result)
    } else {
        result
    }
}

/// Collect guard_str + requires clauses as a single Rust expression; None
/// if no guards. Skips `requires` bodies referencing
/// `<handler-account>.pubkey` — the harness `State` model doesn't carry
/// handler accounts, so they'd be compile errors. The runtime-side check
/// still emits in the real handler; only the property-test projection
/// drops it (same shape as the lean_gen drop).
pub fn collect_full_guard(op: &ParsedHandler, wrapping: bool) -> Option<String> {
    collect_full_guard_with_account_env(op, wrapping, None)
}

pub fn collect_full_guard_with_account_env(
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ref guard) = op.guard_str {
        let translated = translate_guard_to_rust(guard, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        parts.push(format!("({})", translated));
    }
    for req in &op.requires {
        if account_binder.is_none() && mentions_handler_account_pubkey(&req.rust_expr, &op.accounts)
        {
            continue;
        }
        // Harness predicates evaluate on unconstrained symbolic state —
        // prefer the math-exact form (arithmetic widened to u128/i128) so
        // the guard itself can't overflow-panic (issue #146).
        let translated = translate_guard_to_rust(requires_math_or_rust(req), wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        parts.push(format!("({})", translated));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

/// The math-exact predicate form when rendered, else the plain Rust form
/// (ParsedRequires built outside the chumsky adapter leave it empty).
fn requires_math_or_rust(req: &crate::check::ParsedRequires) -> &str {
    if req.rust_expr_math.is_empty() {
        &req.rust_expr
    } else {
        &req.rust_expr_math
    }
}

pub fn collect_guard_terms_with_account_env(
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> Vec<GuardTerm> {
    let mut terms = Vec::new();
    if let Some(ref guard) = op.guard_str {
        let translated = translate_guard_to_rust(guard, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        push_split_guard_terms(&mut terms, GuardTermSource::Guard, &translated);
    }
    for req in &op.requires {
        if account_binder.is_none() && mentions_handler_account_pubkey(&req.rust_expr, &op.accounts)
        {
            continue;
        }
        let translated = translate_guard_to_rust(requires_math_or_rust(req), wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        push_split_guard_terms(
            &mut terms,
            GuardTermSource::Requires {
                error_name: req.error_name.clone(),
            },
            &translated,
        );
    }
    terms
}

fn push_split_guard_terms(terms: &mut Vec<GuardTerm>, source: GuardTermSource, expr: &str) {
    for term in split_top_level_and(expr) {
        terms.push(GuardTerm {
            source: source.clone(),
            rust_expr: term,
        });
    }
}

pub fn split_top_level_and(expr: &str) -> Vec<String> {
    let bytes = expr.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
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
            b'&' if i + 1 < bytes.len()
                && bytes[i + 1] == b'&'
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0 =>
            {
                let part = expr[start..i].trim();
                if !part.is_empty() {
                    parts.push(part.to_string());
                }
                i += 2;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    let tail = expr[start..].trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    parts
}

/// True when `expr` mentions `<handler_account>.pubkey` (or `.key()`) —
/// used to suppress such `requires` from property-test guard collection.
pub(crate) fn mentions_handler_account_pubkey(
    expr: &str,
    accounts: &[crate::check::ParsedHandlerAccount],
) -> bool {
    accounts.iter().any(|a| {
        let needle_pubkey = format!("{}.pubkey", a.name);
        let needle_key = format!("{}.key()", a.name);
        expr.contains(&needle_pubkey) || expr.contains(&needle_key)
    })
}
