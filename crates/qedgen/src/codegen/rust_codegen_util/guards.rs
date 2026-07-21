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
            parts.push(render_requires_conjunct(tree, wrapping, account_binder));
        }
    }
    if parts.is_empty() {
        None
    } else {
        // Bounds first: `&&` short-circuits left-to-right, so no later
        // term can index out of range (#298).
        let mut all = requires_bounds_terms(op);
        all.extend(parts);
        Some(all.join(" && "))
    }
}

/// Synthesized bounds conjuncts for every identifier subscript the
/// handler's requires clauses read (`voted[member_index]` →
/// `((member_index) as usize) < s.voted.len()`), in first-seen order.
///
/// The model state space is wider than any deployed state: `arb_state` /
/// `kani::any` generate count fields past a bounded container's
/// capacity, so a guard like `i < s.member_count` passes for an index
/// beyond `s.voted.len()` and the next conjunct panics the harness
/// (proptest) or fails the proof spuriously (Kani). Deployed code
/// aborts the transaction on that access; the model must reject the
/// transition instead (#298). Synthesized for every requires clause —
/// including account-projected ones — because the deployed guard
/// evaluates them all.
pub fn requires_bounds_terms(op: &ParsedHandler) -> Vec<String> {
    render_bounds_terms(&requires_bounds_pairs(op))
}

/// The `(container, index)` pairs behind [`requires_bounds_terms`] —
/// exposed so the transition emitter can subtract them from its
/// effect-subscript pre-checks instead of double-guarding.
pub(super) fn requires_bounds_pairs(op: &ParsedHandler) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for req in &op.requires {
        collect_tree_subscripts(requires_tree(req), &mut pairs);
    }
    pairs
}

/// `(container, index)` pairs for every state-rooted `container[index]`
/// read in `tree`, where `index` is an identifier (numeric subscripts
/// are in range by construction — the container length is a spec
/// constant).
pub(super) fn collect_tree_subscripts(
    tree: &crate::mir::ExprTree,
    pairs: &mut Vec<(String, String)>,
) {
    use crate::mir::expr_tree::{BindingKind, TreeSeg};
    super::tree_render::for_each_path(tree, &mut |p| {
        if !matches!(p.binding, BindingKind::StateField | BindingKind::Ghost) {
            return;
        }
        let mut fields: Vec<&str> = Vec::new();
        for seg in &p.segments {
            match seg {
                TreeSeg::Field(name) => fields.push(name.as_str()),
                TreeSeg::Index(sym) => {
                    if !fields.is_empty() && !sym.chars().all(|c| c.is_ascii_digit()) {
                        let pair = (fields.join("."), sym.to_string());
                        if !pairs.contains(&pair) {
                            pairs.push(pair);
                        }
                    }
                }
            }
        }
    });
}

/// Render `(container, index)` pairs as `s.`-rooted bounds predicates.
/// Containers may themselves carry a subscript (`accounts[i].loans`);
/// `cast_subscripts` keeps those inner indexes `usize`.
pub(super) fn render_bounds_terms(pairs: &[(String, String)]) -> Vec<String> {
    pairs
        .iter()
        .map(|(container, idx)| {
            format!(
                "(({idx}) as usize) < s.{}.len()",
                cast_subscripts(container)
            )
        })
        .collect()
}

/// `(container, index)` pairs for identifier subscripts in a flattened
/// effect-target path string (`voted[member_index]` →
/// `("voted", "member_index")`). Numeric subscripts are skipped — in
/// range by construction.
pub(super) fn field_string_subscripts(field: &str, pairs: &mut Vec<(String, String)>) {
    let mut rest = field;
    let mut prefix_len = 0usize;
    while let Some(i) = rest.find('[') {
        let container = field[..prefix_len + i].to_string();
        let after = &rest[i + 1..];
        let Some(j) = after.find(']') else { return };
        let idx = &after[..j];
        let is_ident = !idx.is_empty()
            && idx.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !idx.starts_with(|c: char| c.is_ascii_digit());
        if is_ident && !container.is_empty() {
            let pair = (container, idx.to_string());
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
        prefix_len += i + 1 + j + 1;
        rest = &after[j + 1..];
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

/// Render one projected requires tree for an outer conjunction. The tree
/// renderer owns both arithmetic policy and the minimum parentheses needed
/// in that slot.
fn render_requires_conjunct(
    tree: &crate::mir::ExprTree,
    wrapping: bool,
    account_binder: Option<&str>,
) -> String {
    use super::tree_render::{render_rust_conjunct, ArithMode, RustCx};
    let arith = if wrapping {
        ArithMode::Wrapping
    } else {
        ArithMode::Widened
    };
    render_rust_conjunct(
        tree,
        RustCx::native()
            .with_arith(arith)
            .with_acct_env(account_binder),
    )
}

/// Per-conjunct guard terms of a handler's requires clauses, rendered as
/// Rust predicates. Tree-native conjunct split: the top `And` node's
/// operands become independent terms, and the renderer adds grouping only
/// where an outer conjunction needs it. Without an account env, terms are
/// the account-free projection (#295).
pub fn collect_guard_terms_with_account_env(
    op: &ParsedHandler,
    wrapping: bool,
    account_binder: Option<&str>,
) -> Vec<String> {
    use super::tree_render::{render_rust_conjunct, top_conjuncts, ArithMode, RustCx};

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
            for c in conjuncts {
                terms.push(render_rust_conjunct(c, cx));
            }
        }
    }
    if terms.is_empty() {
        terms
    } else {
        // Bounds first — same ordering contract as
        // `collect_full_guard_with_account_env` (#298); split-term
        // consumers emit terms in order, so bounds still evaluate before
        // any indexing term.
        let mut all = requires_bounds_terms(op);
        all.extend(terms);
        all
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
