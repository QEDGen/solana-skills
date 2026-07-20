//! Tree-native guard rendering: requires clauses → Rust predicates, plus
//! the top-level `&&` splitter and handler-account-pubkey suppression used
//! to drop accounts-only requires from the pure-model harness projection.

use super::*;

/// Collect requires clauses as a single Rust expression; None if no
/// guards. Skips `requires` bodies referencing
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
    for req in &op.requires {
        for tree in projected_requires_trees(req, account_binder) {
            parts.push(format!(
                "({})",
                render_requires_tree(tree, wrapping, account_binder)
            ));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" && "))
    }
}

/// Project one requires clause onto the account-free harness model.
/// With an account env bound, the whole clause is expressible. Without
/// one, keep the account-free conjuncts and drop the rest: the harness
/// `State` carries no handler accounts, so ANY account read — bare
/// `approver`, not just `.pubkey` (#295; the pubkey-only scan let
/// `s.members[i] == approver` through as a free variable, E0425) — is
/// unexpressible there. Term-by-term projection over the top `and`
/// spine, so an account term does not erase adjacent state/param
/// constraints; other boolean shapes stay atomic (pruning below
/// `or`/`not` would change their meaning). Same contract as the
/// unit-test emitter's guard projection.
fn projected_requires_trees<'a>(
    req: &'a crate::check::ParsedRequires,
    account_binder: Option<&str>,
) -> Vec<&'a crate::mir::ExprTree> {
    let tree = requires_tree(req);
    if account_binder.is_some() {
        return vec![tree];
    }
    account_free_conjuncts(tree)
}

/// Flatten `and` nodes and retain the conjuncts free of account reads.
fn account_free_conjuncts(tree: &crate::mir::ExprTree) -> Vec<&crate::mir::ExprTree> {
    use crate::mir::expr_tree::{ExprTree, TreeBoolOp};

    match tree {
        ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs,
            rhs,
        } => {
            let mut out = account_free_conjuncts(lhs);
            out.extend(account_free_conjuncts(rhs));
            out
        }
        _ if super::tree_render::tree_mentions_account(tree) => Vec::new(),
        _ => vec![tree],
    }
}

/// The typed tree of a requires clause. Post-#151 every production
/// `ParsedRequires` is adapter-built with `tree: Some(...)`; a `None`
/// here is a hand-built fixture that must be fixed, not worked around.
fn requires_tree(req: &crate::check::ParsedRequires) -> &crate::mir::ExprTree {
    req.tree
        .as_ref()
        .expect("ParsedRequires.tree is always populated by the chumsky adapter (#151/#156)")
}

/// One projected requires tree as a Rust predicate. Tree-native (#151
/// Slice 1): one render call under the right arithmetic policy —
/// `Widened` (math-exact; issue #146) or the `Wrapping` proptest-guard
/// composite.
fn render_requires_tree(
    tree: &crate::mir::ExprTree,
    wrapping: bool,
    account_binder: Option<&str>,
) -> String {
    use super::tree_render::{render_rust, ArithMode, RustCx};
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

/// Per-conjunct guard terms of a handler's requires clauses, rendered as
/// Rust predicates. Tree-native conjunct split: the top `And` node's
/// operands, matching the legacy top-level `&&` string split (each
/// operand keeps the parens the bool-op rendering gave it). Without an
/// account env, terms are the account-free projection (#295).
pub fn collect_guard_terms_with_account_env(
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> Vec<String> {
    use super::tree_render::{render_rust, top_conjuncts, ArithMode, RustCx};

    let mut terms = Vec::new();
    for req in &op.requires {
        let arith = if wrapping {
            ArithMode::Wrapping
        } else {
            ArithMode::Widened
        };
        let cx = RustCx::native()
            .with_arith(arith)
            .with_acct_env(account_binder);
        // `projected_requires_trees` already splits the top `and` spine
        // when projecting; run `top_conjuncts` over each projected tree so
        // the with-account-env path keeps its historical per-term split.
        let projected = projected_requires_trees(req, account_binder);
        for tree in &projected {
            let conjuncts = top_conjuncts(tree);
            let multi = conjuncts.len() > 1 || projected.len() > 1;
            for c in conjuncts {
                let rendered = render_rust(c, cx);
                terms.push(if multi {
                    format!("({})", rendered)
                } else {
                    rendered
                });
            }
        }
    }
    terms
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
