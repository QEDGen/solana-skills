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
        if suppress_requires(req, op, account_binder) {
            continue;
        }
        parts.push(format!(
            "({})",
            render_requires(req, op, wrapping, account_binder)
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

/// Accounts-only requires are dropped from the pure-model projection when
/// no account env is bound (the harness `State` carries no handler
/// accounts). Structural on the tree; substring scan for legacy strings.
fn suppress_requires(
    req: &crate::check::ParsedRequires,
    op: &ParsedHandler,
    account_binder: Option<&str>,
) -> bool {
    if account_binder.is_some() {
        return false;
    }
    match &req.tree {
        Some(tree) => super::tree_render::tree_mentions_account_pubkey(tree),
        None => mentions_handler_account_pubkey(&req.rust_expr, &op.accounts),
    }
}

/// One requires clause as a Rust predicate. Tree-native (#151 Slice 1):
/// one render call under the right arithmetic policy — `Widened`
/// (math-exact; issue #146) or the `Wrapping` proptest-guard composite —
/// replaces the pre-rendered math string + `translate_guard_to_rust` +
/// `rewrite_account_pubkey_refs` chain. Legacy string path stays for
/// tree-less clauses (IDL ingest, probes).
fn render_requires(
    req: &crate::check::ParsedRequires,
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> String {
    use super::tree_render::{render_rust, ArithMode, RustCx};
    match &req.tree {
        Some(tree) => {
            let arith = if wrapping {
                ArithMode::Wrapping
            } else {
                ArithMode::Widened
            };
            let cx = RustCx::native()
                .with_arith(arith)
                .with_acct_env(account_binder);
            render_rust(tree, cx)
        }
        None => {
            // Harness predicates evaluate on unconstrained symbolic state —
            // prefer the math-exact form (arithmetic widened to u128/i128)
            // so the guard itself can't overflow-panic (issue #146).
            let translated = translate_guard_to_rust(requires_math_or_rust(req), wrapping);
            account_binder
                .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
                .unwrap_or(translated)
        }
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
    use super::tree_render::{render_rust, top_conjuncts, ArithMode, RustCx};

    let mut terms = Vec::new();
    if let Some(ref guard) = op.guard_str {
        let translated = translate_guard_to_rust(guard, wrapping);
        let translated = account_binder
            .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
            .unwrap_or(translated);
        push_split_guard_terms(&mut terms, GuardTermSource::Guard, &translated);
    }
    for req in &op.requires {
        if suppress_requires(req, op, account_binder) {
            continue;
        }
        let source = GuardTermSource::Requires {
            error_name: req.error_name.clone(),
        };
        match &req.tree {
            // Tree-native conjunct split: the top `And` node's operands,
            // matching the legacy top-level `&&` string split (each
            // operand keeps the parens the bool-op rendering gave it).
            Some(tree) => {
                let arith = if wrapping {
                    ArithMode::Wrapping
                } else {
                    ArithMode::Widened
                };
                let cx = RustCx::native()
                    .with_arith(arith)
                    .with_acct_env(account_binder);
                let conjuncts = top_conjuncts(tree);
                let multi = conjuncts.len() > 1;
                for c in conjuncts {
                    let rendered = render_rust(c, cx);
                    terms.push(GuardTerm {
                        source: source.clone(),
                        rust_expr: if multi {
                            format!("({})", rendered)
                        } else {
                            rendered
                        },
                    });
                }
            }
            None => {
                let translated = translate_guard_to_rust(requires_math_or_rust(req), wrapping);
                let translated = account_binder
                    .map(|binder| rewrite_account_pubkey_refs(&translated, &op.accounts, binder))
                    .unwrap_or(translated);
                push_split_guard_terms(&mut terms, source, &translated);
            }
        }
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
