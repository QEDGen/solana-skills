//! Param substitution for CPI ensures discharge / propagation — tree-native
//! (#156; replaces the word-boundary regex lanes).
//!
//! When a handler does `call Iface.foo(args)` and `Iface.foo` declares
//! `ensures`, two backends propagate the contract into the caller:
//! 1. **Lean** (`render_cpi_theorems`) — per-call-site theorem stating the
//!    callee's `ensures` with call-site args substituted; Tier-1 closes via
//!    the bundled `ensures_axiom_<idx>`, Tier-0 emits `:= by sorry`.
//! 2. **Kani** ensures-preservation harness — `kani::assume(<substituted>)`
//!    after the transition call, before the caller's own `assert!`s.
//!
//! Both operate on the callee's `ensures` **tree** (adapter-populated for
//! every production interface, inline or imported):
//!
//! * `Param` leaves naming a callee formal → the caller's argument tree,
//!   spliced structurally. The old regex lane pasted raw text, so a
//!   compound caller arg (`a + b`) inside `amount * 2` produced the
//!   mis-parenthesized `a + b * 2`; tree splicing renders with correct
//!   precedence by construction.
//! * The callee's result binder (declared `-> <ident> : T`, defaulting to
//!   the literal `result`) → the caller's `let X = call …` binding.
//! * `StateField` paths (the callee's abstract state) → the caller's
//!   `state_binders` projection, encoded as verbatim `pre.<field>` /
//!   `post.<field>` paths (`BindingKind::Unresolved` renders the spelling
//!   as-is in both backends). The pre/post split is structural — `Old(…)`
//!   marks pre-state — so no `s'.`-needle text rewriting is involved.

use crate::check::{ParsedCall, ParsedStateBinder};
use crate::mir::expr_tree::{BindingKind, ExprTree, TreePath, TreeSeg};

/// Substitute call-site data into a callee `ensures` tree. See module docs
/// for the three substitution rules. Callee params the caller didn't bind
/// keep their formal name (Lean surfaces them as free variables — the lint
/// catches it; Rust as compile errors).
///
/// `callee_result_binder` is the identifier the callee's `ensures` uses for
/// its return value; `None` falls back to the conventional `"result"`
/// literal for specs without a declared binder.
pub fn substitute_callee_ensures_tree(
    ensures: &ExprTree,
    call: &ParsedCall,
    callee_result_binder: Option<&str>,
) -> ExprTree {
    let cx = SubstCx {
        call,
        result_binder: callee_result_binder.unwrap_or("result"),
    };
    crate::mir::expr_tree::map_paths(ensures, &mut |p, in_old| Some(subst_path(p, &cx, in_old)))
}

/// Abstract State-field projections read by a callee `ensures` tree
/// (`state.X` / `old(state.X)` in the callee's frame), in first-occurrence
/// order. Backends use this with [`missing_state_binders`] to avoid
/// importing callee ensures into a caller frame that cannot name the
/// callee's abstract state.
pub fn scan_abstract_state_fields(ensures: &ExprTree) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    crate::rust_codegen_util::tree_render::for_each_path(ensures, &mut |p| {
        let is_state_read = matches!(p.binding, BindingKind::StateField | BindingKind::Ghost)
            // Explicitly-spelled snapshot reads (`post.X` / `pre.X`)
            // resolve `Unresolved` in the interface-handler scope.
            || (matches!(p.binding, BindingKind::Unresolved)
                && (p.root == "pre" || p.root == "post"));
        if is_state_read {
            if let Some(TreeSeg::Field(f)) = p.segments.first() {
                if seen.insert(f.clone()) {
                    out.push(f.clone());
                }
            }
        }
    });
    out
}

/// Return the abstract fields from `fields` that are not covered by a
/// `state_binders` entry.
pub fn missing_state_binders(fields: &[String], binders: &[ParsedStateBinder]) -> Vec<String> {
    fields
        .iter()
        .filter(|f| !binders.iter().any(|b| &b.callee_field == *f))
        .cloned()
        .collect()
}

struct SubstCx<'a> {
    call: &'a ParsedCall,
    result_binder: &'a str,
}

impl SubstCx<'_> {
    /// The caller's argument tree bound to a callee formal, if any. Post-#151
    /// every production call arg carries a tree; a `None` is a hand-built
    /// fixture that must be fixed, not worked around.
    fn arg_tree(&self, param: &str) -> Option<&ExprTree> {
        self.call.args.iter().find(|a| a.name == param).map(|a| {
            a.tree
                .as_ref()
                .expect("ParsedCallArg.tree is always populated by the chumsky adapter (#151/#156)")
        })
    }

    fn caller_binder_for(&self, callee_field: &str) -> Option<&str> {
        self.call
            .state_binders
            .iter()
            .find(|b| b.callee_field == callee_field)
            .map(|b| b.caller_field.as_str())
    }
}

/// Verbatim-spelling path (`BindingKind::Unresolved` renders the root and
/// segments as written in both backends).
fn verbatim_path(root: &str, segments: Vec<TreeSeg>) -> ExprTree {
    ExprTree::Path(TreePath {
        root: root.to_string(),
        binding: BindingKind::Unresolved,
        segments,
        ty: None,
    })
}

fn subst_path(p: &TreePath, cx: &SubstCx<'_>, in_old: bool) -> ExprTree {
    match &p.binding {
        // Callee abstract state read → caller State projection per
        // `state_binders`. Post-state reads bind `post.`; reads under
        // `old(…)` bind `pre.` (matching the `(pre post : State)` /
        // pre-post snapshot binders on the caller side). Unmapped fields
        // keep the callee's field name — they bind to a caller snapshot
        // field of the same name when one exists (the Lean lane and the
        // preservation harness gate on `missing_state_binders` first, so
        // pass-through only reaches the ungated kani-impl lane).
        BindingKind::StateField | BindingKind::Ghost => {
            let Some(TreeSeg::Field(callee_field)) = p.segments.first() else {
                return ExprTree::Path(p.clone());
            };
            let caller_field = cx.caller_binder_for(callee_field).unwrap_or(callee_field);
            let mut segments = vec![TreeSeg::Field(caller_field.to_string())];
            segments.extend(p.segments[1..].iter().cloned());
            verbatim_path(if in_old { "pre" } else { "post" }, segments)
        }
        // Callee formals: the result binder rewrites to the caller's
        // `let X = call …` binding; other params rewrite to the caller's
        // argument tree (already resolved in the CALLER's scope). A
        // *declared* binder (`-> price : U64`) resolves as `Param`
        // (`TreeCx::for_interface_handler` inserts it); the conventional
        // `result` literal of binder-less callees resolves `Unresolved` —
        // both spell the return value, so both arms rename it.
        BindingKind::Param | BindingKind::Unresolved => {
            // Interface ensures may spell snapshots explicitly
            // (`post.from_balance`) instead of `state.X` / `old(state.X)`;
            // those resolve `Unresolved` with a `pre`/`post` root. Map the
            // projected field through `state_binders`, same as the
            // state-rooted form.
            if p.root == "pre" || p.root == "post" {
                if let Some(TreeSeg::Field(callee_field)) = p.segments.first() {
                    if let Some(caller_field) = cx.caller_binder_for(callee_field) {
                        let mut segments = vec![TreeSeg::Field(caller_field.to_string())];
                        segments.extend(p.segments[1..].iter().cloned());
                        return verbatim_path(&p.root, segments);
                    }
                }
                return ExprTree::Path(p.clone());
            }
            if p.root == cx.result_binder {
                if let Some(binding) = &cx.call.result_binding {
                    return verbatim_path(binding, p.segments.clone());
                }
            }
            match cx.arg_tree(&p.root) {
                Some(arg) => splice_segments(arg, p),
                None => ExprTree::Path(p.clone()),
            }
        }
        // Everything else is already in some resolved frame; the
        // substitution has nothing to rewrite.
        BindingKind::Const(_)
        | BindingKind::LetBound
        | BindingKind::Account
        | BindingKind::External
        | BindingKind::AbstractBinder
        | BindingKind::ExprBinder => ExprTree::Path(p.clone()),
    }
}

/// Substitute an argument tree for a param path, re-applying the path's
/// trailing segments. Bare params (the overwhelmingly common case) clone
/// the arg tree; a projected param (`param.field`) splices onto a Path arg
/// or wraps a compound arg in `Field` nodes. An `Index` segment on a
/// non-path arg has no tree shape — keep the formal path (lint territory,
/// same as an unbound param).
fn splice_segments(arg: &ExprTree, formal: &TreePath) -> ExprTree {
    if formal.segments.is_empty() {
        return arg.clone();
    }
    if let ExprTree::Path(ap) = arg {
        let mut p = ap.clone();
        p.segments.extend(formal.segments.iter().cloned());
        p.ty = formal.ty.clone();
        return ExprTree::Path(p);
    }
    let mut out = arg.clone();
    for seg in &formal.segments {
        match seg {
            TreeSeg::Field(f) => {
                out = ExprTree::Field {
                    base: Box::new(out),
                    field: f.clone(),
                };
            }
            TreeSeg::Index(_) => return ExprTree::Path(formal.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::ParsedCallArg;
    use crate::mir::expr_tree::{TreeArithOp, TreeBoolOp, TreeCmpOp};

    fn param(name: &str) -> ExprTree {
        ExprTree::Path(TreePath {
            root: name.to_string(),
            binding: BindingKind::Param,
            segments: vec![],
            ty: None,
        })
    }

    fn state_field(name: &str) -> ExprTree {
        ExprTree::Path(TreePath {
            root: "state".to_string(),
            binding: BindingKind::StateField,
            segments: vec![TreeSeg::Field(name.to_string())],
            ty: None,
        })
    }

    fn caller_state_arg(name: &str) -> ExprTree {
        // An arg tree resolved in the CALLER's scope — a state-field read.
        state_field(name)
    }

    fn gt_zero(lhs: ExprTree) -> ExprTree {
        ExprTree::Cmp {
            op: TreeCmpOp::Gt,
            lhs: Box::new(lhs),
            rhs: Box::new(ExprTree::Int(0)),
        }
    }

    fn mk_call(args: &[(&str, ExprTree)]) -> ParsedCall {
        ParsedCall {
            target_interface: "Token".to_string(),
            target_handler: "transfer".to_string(),
            args: args
                .iter()
                .map(|(n, tree)| ParsedCallArg {
                    name: n.to_string(),
                    rust_expr: String::new(),
                    tree: Some(tree.clone()),
                })
                .collect(),
            result_binding: None,
            state_binders: Vec::new(),
        }
    }

    #[test]
    fn param_swaps_for_caller_arg_tree() {
        let call = mk_call(&[("amount", caller_state_arg("taker_amount"))]);
        let out = substitute_callee_ensures_tree(&gt_zero(param("amount")), &call, None);
        assert_eq!(out, gt_zero(caller_state_arg("taker_amount")));
    }

    #[test]
    fn unbound_param_keeps_formal_name() {
        let call = mk_call(&[("amount", param("amount"))]);
        let ensures = ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs: Box::new(gt_zero(param("amount"))),
            rhs: Box::new(gt_zero(param("recipient"))),
        };
        let out = substitute_callee_ensures_tree(&ensures, &call, None);
        assert_eq!(
            out,
            ExprTree::BoolOp {
                op: TreeBoolOp::And,
                lhs: Box::new(gt_zero(param("amount"))),
                rhs: Box::new(gt_zero(param("recipient"))),
            }
        );
    }

    #[test]
    fn compound_arg_stays_structural() {
        // Regex lane pasted `a + b` into `amount * 2` → `a + b * 2`
        // (wrong precedence). Tree splice keeps the arg a subtree, so
        // rendering parenthesizes correctly.
        let sum = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(param("a")),
            rhs: Box::new(param("b")),
        };
        let call = mk_call(&[("amount", sum.clone())]);
        let ensures = ExprTree::Arith {
            op: TreeArithOp::Mul,
            lhs: Box::new(param("amount")),
            rhs: Box::new(ExprTree::Int(2)),
        };
        let out = substitute_callee_ensures_tree(&ensures, &call, None);
        assert_eq!(
            out,
            ExprTree::Arith {
                op: TreeArithOp::Mul,
                lhs: Box::new(sum),
                rhs: Box::new(ExprTree::Int(2)),
            }
        );
    }

    #[test]
    fn result_binder_rewrites_to_caller_binding() {
        let mut call = mk_call(&[("amount", param("amount"))]);
        call.result_binding = Some("delta".to_string());
        // Default literal `result`.
        let out = substitute_callee_ensures_tree(&gt_zero(param("result")), &call, None);
        assert_eq!(out, gt_zero(verbatim_path("delta", vec![])));
        // Declared binder name takes precedence over the literal.
        let out = substitute_callee_ensures_tree(&gt_zero(param("price")), &call, Some("price"));
        assert_eq!(out, gt_zero(verbatim_path("delta", vec![])));
    }

    #[test]
    fn state_binders_map_post_and_pre_reads() {
        let mut call = mk_call(&[("amount", param("amount"))]);
        call.state_binders = vec![ParsedStateBinder {
            callee_field: "from_balance".to_string(),
            caller_field: "pool_balance".to_string(),
        }];
        // `state.from_balance = old(state.from_balance) + amount`
        let ensures = ExprTree::Cmp {
            op: TreeCmpOp::Eq,
            lhs: Box::new(state_field("from_balance")),
            rhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Add,
                lhs: Box::new(ExprTree::Old(Box::new(state_field("from_balance")))),
                rhs: Box::new(param("amount")),
            }),
        };
        let out = substitute_callee_ensures_tree(&ensures, &call, None);
        assert_eq!(
            out,
            ExprTree::Cmp {
                op: TreeCmpOp::Eq,
                lhs: Box::new(verbatim_path(
                    "post",
                    vec![TreeSeg::Field("pool_balance".to_string())]
                )),
                rhs: Box::new(ExprTree::Arith {
                    op: TreeArithOp::Add,
                    lhs: Box::new(ExprTree::Old(Box::new(verbatim_path(
                        "pre",
                        vec![TreeSeg::Field("pool_balance".to_string())]
                    )))),
                    rhs: Box::new(param("amount")),
                }),
            }
        );
    }

    #[test]
    fn unmapped_state_field_keeps_callee_name_in_caller_frame() {
        // No binder for `to_balance` — the read still lands in the
        // caller's snapshot frame under the callee's own field name
        // (pass-through-by-name; gated lanes skip emission first).
        let call = mk_call(&[("amount", param("amount"))]);
        let ensures = gt_zero(state_field("to_balance"));
        let out = substitute_callee_ensures_tree(&ensures, &call, None);
        assert_eq!(
            out,
            gt_zero(verbatim_path(
                "post",
                vec![TreeSeg::Field("to_balance".to_string())]
            ))
        );
    }

    #[test]
    fn scan_collects_state_fields_in_first_occurrence_order() {
        let ensures = ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs: Box::new(gt_zero(state_field("from_balance"))),
            rhs: Box::new(ExprTree::Cmp {
                op: TreeCmpOp::Eq,
                lhs: Box::new(ExprTree::Old(Box::new(state_field("to_balance")))),
                rhs: Box::new(state_field("from_balance")),
            }),
        };
        assert_eq!(
            scan_abstract_state_fields(&ensures),
            vec!["from_balance", "to_balance"]
        );
        // Param-only ensures scan empty.
        assert!(scan_abstract_state_fields(&gt_zero(param("amount"))).is_empty());
    }

    #[test]
    fn scans_and_reports_missing_state_binders() {
        let fields = vec!["from_balance".to_string(), "to_balance".to_string()];
        assert_eq!(missing_state_binders(&fields, &[]), fields);
        let missing = missing_state_binders(
            &fields,
            &[ParsedStateBinder {
                callee_field: "from_balance".into(),
                caller_field: "pool_balance".into(),
            }],
        );
        assert_eq!(missing, vec!["to_balance"]);
    }
}
