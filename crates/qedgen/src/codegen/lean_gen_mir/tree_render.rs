//! Lean rendering of `mir::ExprTree` — #151 Slice 2.
//!
//! One renderer, parameterized by [`LeanCx`], replaces the pre-rendered
//! `Expr.lean` string: the state receiver (`s.` vs `s'.`) becomes a
//! renderer parameter chosen at emission time, and the RHS-shape
//! heuristics in `util::effect_value_to_lean_mir` become structural.
//!
//! Output contract: token-for-token compatible with the legacy
//! `chumsky_adapter::expr_to_lean`, except paren placement — the tree has
//! no `Paren` nodes, so grouping is re-derived with minimal
//! precedence-correct parens that preserve the tree's evaluation order
//! (load-bearing on Lean `Nat`: `a - (b - c)` ≠ `a - b - c`).
//!
//! Matches over `ExprTree` / `BindingKind` are exhaustive by discipline —
//! no `_` arms (see `mir::expr_tree` module docs).

// "IR ahead of consumers": the lean_gen_mir emission port is the second
// half of Slice 2. The corpus parity test below keeps the renderer honest
// until then.
#![allow(dead_code)]

use crate::mir::expr_tree::{
    BindingKind, ExprTree, NumKind, QuantKind, TreeArithOp, TreeBoolOp, TreeCmpOp, TreePath,
    TreeSeg,
};
use crate::mir::Ty;

/// How state reads (and `old(...)`) pick their Lean receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeanBinder {
    /// Single-state contexts (requires, invariants, unary properties):
    /// `state.x` → `s.x`; `old(state.x)` collapses to `s.x`.
    S,
    /// Two-state ensures contexts: `state.x` → `s'.x`,
    /// `old(state.x)` → `s.x`.
    SPrime,
}

/// How `Map` subscripts render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeanSubscript {
    /// `x[i]` — the adapter's surface form (requires/ensures strings).
    Brackets,
    /// `(x i)` — the MIR Lean model represents `Map[N] T` fields as
    /// functions; transition bodies apply them. Replaces the
    /// `rewrite_subscripts_lean` string pass.
    Application,
}

/// Render context for Lean emission.
#[derive(Debug, Clone, Copy)]
pub struct LeanCx {
    pub binder: LeanBinder,
    pub subscript: LeanSubscript,
}

impl LeanCx {
    pub fn guard() -> Self {
        LeanCx {
            binder: LeanBinder::S,
            subscript: LeanSubscript::Brackets,
        }
    }
    pub fn ensures() -> Self {
        LeanCx {
            binder: LeanBinder::SPrime,
            subscript: LeanSubscript::Brackets,
        }
    }
    pub fn with_application_subscripts(self) -> Self {
        LeanCx {
            subscript: LeanSubscript::Application,
            ..self
        }
    }
}

/// Lean operator precedence for minimal-paren rendering; `Atom` includes
/// every self-delimited form (parenthesized matches, record literals,
/// applications). Bool chains stay flat at equal precedence — `∧`/`∨` are
/// associative in `Prop`, matching the legacy flat rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Implies,
    Or,
    And,
    Cmp,
    AddSub,
    MulDiv,
    Atom,
}

/// Render `e` under `cx`.
pub fn render_lean(e: &ExprTree, cx: LeanCx) -> String {
    render(e, cx, false).0
}

fn render(e: &ExprTree, cx: LeanCx, inside_old: bool) -> (String, Prec) {
    match e {
        ExprTree::Int(v) => (v.to_string(), Prec::Atom),
        // Lean 4 Bool literals are lowercase `true`/`false`; `True`/`False`
        // are *Props* (`flag := True` would type-error when `flag : Bool`).
        ExprTree::Bool(b) => (b.to_string(), Prec::Atom),
        ExprTree::Path(p) => (render_path(p, cx, inside_old), Prec::Atom),
        ExprTree::Old(inner) => render_old(inner, cx),
        ExprTree::Sum {
            binder,
            binder_ty,
            fin_bound: _,
            body,
        } => (
            format!(
                "(\u{2211} {} : {}, {})",
                binder,
                binder_ty,
                render(body, cx, inside_old).0
            ),
            Prec::Atom,
        ),
        ExprTree::Quant {
            kind,
            binder,
            binder_ty,
            // Lean quantifies the declared domain directly; the resolved
            // bound is a Rust-lowering concern.
            fin_bound: _,
            body,
        } => {
            let sym = match kind {
                QuantKind::Forall => "\u{2200}",
                QuantKind::Exists => "\u{2203}",
            };
            let lean_ty = match binder_ty.as_str() {
                "U64" | "U32" | "U16" | "U8" | "U128" => "Nat",
                "I64" | "I32" | "I16" | "I8" | "I128" => "Int",
                other => other,
            };
            (
                format!(
                    "{} {} : {}, {}",
                    sym,
                    binder,
                    lean_ty,
                    render(body, cx, inside_old).0
                ),
                // A quantifier body swallows everything to its right;
                // treat as the weakest binding so parents parenthesize.
                Prec::Implies,
            )
        }
        ExprTree::QuantIn {
            kind,
            binder,
            coll,
            body,
        } => {
            // `∃|∀ x ∈ coll, body` — bounded quantifier over a List (Vec).
            let sym = match kind {
                QuantKind::Forall => "\u{2200}",
                QuantKind::Exists => "\u{2203}",
            };
            (
                format!(
                    "({} {} \u{2208} {}, {})",
                    sym,
                    binder,
                    render(coll, cx, inside_old).0,
                    render(body, cx, inside_old).0
                ),
                Prec::Atom,
            )
        }
        ExprTree::BoolOp { op, lhs, rhs } => {
            let (sym, prec) = match op {
                TreeBoolOp::And => (" \u{2227} ", Prec::And),
                TreeBoolOp::Or => (" \u{2228} ", Prec::Or),
                TreeBoolOp::Implies => (" \u{2192} ", Prec::Implies),
            };
            // Flat at equal precedence (∧/∨ associative in Prop, →
            // right-assoc — matches both Lean's parse and the legacy flat
            // output); strictly-weaker children parenthesize.
            let l = atom_for(prec, render(lhs, cx, inside_old));
            let r = atom_for(prec, render(rhs, cx, inside_old));
            (format!("{}{}{}", l, sym, r), prec)
        }
        ExprTree::Not(inner) => (
            format!("\u{00AC}({})", render(inner, cx, inside_old).0),
            Prec::Atom,
        ),
        ExprTree::Cmp { op, lhs, rhs } => {
            let sym = match op {
                TreeCmpOp::Eq => "=",
                TreeCmpOp::Ne => "\u{2260}",
                TreeCmpOp::Le => "\u{2264}",
                TreeCmpOp::Ge => "\u{2265}",
                TreeCmpOp::Lt => "<",
                TreeCmpOp::Gt => ">",
            };
            let (l, r) = render_pair_with_coercion(lhs, rhs, cx, inside_old, Prec::Cmp);
            (format!("{} {} {}", l, sym, r), Prec::Cmp)
        }
        ExprTree::Arith { op, lhs, rhs } => {
            let (sym, prec) = match op {
                TreeArithOp::Add => (" + ", Prec::AddSub),
                TreeArithOp::Sub => (" - ", Prec::AddSub),
                TreeArithOp::Mul => (" * ", Prec::MulDiv),
                TreeArithOp::Div => (" / ", Prec::MulDiv),
                TreeArithOp::Mod => (" % ", Prec::MulDiv),
            };
            let (l, r) = render_pair_with_coercion(lhs, rhs, cx, inside_old, prec);
            (format!("{}{}{}", l, sym, r), prec)
        }
        // Lean Int/Nat are unbounded — `mul_div_floor` simplifies to
        // integer division; overflow is a Rust-codegen concern.
        ExprTree::MulDivFloor { a, b, d } => {
            let (a_str, b_str) = coerced_pair_strings(a, b, cx, inside_old);
            let d_str = render(d, cx, inside_old).0;
            (
                format!("((({}) * ({})) / ({}))", a_str, b_str, d_str),
                Prec::Atom,
            )
        }
        // ceil(a*b/d) = (a*b + d - 1) / d for positive d (spec authors
        // assume d > 0; downstream proofs rely on it).
        ExprTree::Contains { coll, elem } => (
            format!(
                "({} ∈ {})",
                render(elem, cx, inside_old).0,
                render(coll, cx, inside_old).0
            ),
            Prec::Atom,
        ),
        ExprTree::Len(coll) => (
            format!("({}).length", render(coll, cx, inside_old).0),
            Prec::Atom,
        ),
        ExprTree::MulDivCeil { a, b, d } => {
            let (a_str, b_str) = coerced_pair_strings(a, b, cx, inside_old);
            let d_str = render(d, cx, inside_old).0;
            (
                format!(
                    "((({}) * ({}) + ({}) - 1) / ({}))",
                    a_str, b_str, d_str, d_str
                ),
                Prec::Atom,
            )
        }
        ExprTree::MulDivRoundHalfUp { a, b, d } => {
            let (a_str, b_str) = coerced_pair_strings(a, b, cx, inside_old);
            let d_str = render(d, cx, inside_old).0;
            (
                format!(
                    "((({}) * ({}) + ({}) / 2) / ({}))",
                    a_str, b_str, d_str, d_str
                ),
                Prec::Atom,
            )
        }
        ExprTree::Match {
            scrutinee, arms, ..
        } => {
            // `match … with | .Ctor binder? => body`. A binder the body
            // never reads renders `_` — Lean's Decidable synthesis trips on
            // named-but-unused binders in Prop-valued arms. The check is
            // structural (does the arm body reference the binder?), not a
            // scan of the rendered text.
            let sc = render(scrutinee, cx, inside_old).0;
            let mut out = String::new();
            out.push_str("(match ");
            out.push_str(&sc);
            out.push_str(" with");
            for arm in arms {
                let body_str = render(&arm.body, cx, inside_old).0;
                // `"_"` is the catch-all — Lean's wildcard is `| _`, not `| ._`.
                if arm.variant == "_" {
                    out.push_str("\n    | _ => ");
                    out.push_str(&body_str);
                    continue;
                }
                out.push_str(&format!("\n    | .{}", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push(' ');
                    if tree_mentions_ident(&arm.body, b) {
                        out.push_str(b);
                    } else {
                        out.push('_');
                    }
                }
                out.push_str(" => ");
                out.push_str(&body_str);
            }
            out.push(')');
            (out, Prec::Atom)
        }
        ExprTree::Ctor {
            variant, payload, ..
        } => (
            match payload {
                None => format!(".{}", variant),
                Some(p) => format!(".{} {}", variant, render(p, cx, inside_old).0),
            },
            // Anonymous-constructor application binds like an application
            // but is context-elaborated; parents should parenthesize.
            Prec::Implies,
        ),
        ExprTree::RecordLit { fields, .. } => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{} := {}", n, render(v, cx, inside_old).0))
                .collect::<Vec<_>>()
                .join(", ");
            (format!("{{ {} }}", body), Prec::Atom)
        }
        ExprTree::RecordUpdate { base, updates, .. } => {
            let base_str = render(base, cx, inside_old).0;
            let body = updates
                .iter()
                .map(|(n, v)| format!("{} := {}", n, render(v, cx, inside_old).0))
                .collect::<Vec<_>>()
                .join(", ");
            (format!("{{ {} with {} }}", base_str, body), Prec::Atom)
        }
        ExprTree::IsVariant {
            scrutinee, variant, ..
        } => {
            // Route through the generated per-variant helper when the
            // scrutinee's type is known — `T.isV x = true` is always
            // Decidable, unlike a raw Prop match. The type comes from the
            // tree's resolved leaf `Ty` (no TypeEnv at emission time).
            let sc = render(scrutinee, cx, inside_old).0;
            let ty_name = match scrutinee.as_ref() {
                ExprTree::Path(p) => match &p.ty {
                    Some(Ty::Custom(name)) => Some(name.clone()),
                    _ => None,
                },
                _ => None,
            };
            (
                match ty_name {
                    Some(name) => format!("({}.is{} {} = true)", name, variant, sc),
                    None => {
                        format!("(match {} with | .{} _ => True | _ => False)", sc, variant)
                    }
                },
                Prec::Atom,
            )
        }
        ExprTree::App { func, args } => {
            // `now()` / `current_epoch()` are axiomatized symbolic reads in
            // the QEDGen.Solana support library.
            if func == "now" && args.is_empty() {
                return ("now".to_string(), Prec::Atom);
            }
            if func == "current_epoch" && args.is_empty() {
                return ("current_epoch".to_string(), Prec::Atom);
            }
            let args_str: Vec<String> = args
                .iter()
                .map(|a| format!("({})", render(a, cx, inside_old).0))
                .collect();
            (format!("({} {})", func, args_str.join(" ")), Prec::Atom)
        }
        ExprTree::Field { base, field } => (
            format!("({}).{}", render(base, cx, inside_old).0, field),
            Prec::Atom,
        ),
        ExprTree::Let { name, value, body } => (
            format!(
                "(let {} := {}; {})",
                name,
                render(value, cx, inside_old).0,
                render(body, cx, inside_old).0
            ),
            Prec::Atom,
        ),
        ExprTree::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => (
            format!(
                "(if {} then {} else {})",
                render(cond, cx, inside_old).0,
                render(then_branch, cx, inside_old).0,
                render(else_branch, cx, inside_old).0,
            ),
            Prec::Atom,
        ),
    }
}

/// Left-operand / flat-chain rule: parens only when strictly weaker.
fn atom_for(slot: Prec, child: (String, Prec)) -> String {
    if child.1 < slot {
        format!("({})", child.0)
    } else {
        child.0
    }
}

/// Right-operand rule for non-associative arithmetic: equal precedence
/// wraps too (`a - (b - c)` must not flatten — Lean `Nat` monus is
/// left-associative).
fn strict_atom_for(slot: Prec, child: (String, Prec)) -> String {
    if child.1 <= slot {
        format!("({})", child.0)
    } else {
        child.0
    }
}

/// `old(inner)`. Path reads collapse to the pre-state receiver; non-path
/// `old` in a single-state context renders the legacy `«old(…)»` marker
/// (surfaced by the `old_in_single_state_context` lint), while an ensures
/// context re-renders the inner expression against pre-state.
fn render_old(inner: &ExprTree, cx: LeanCx) -> (String, Prec) {
    match inner {
        ExprTree::Path(p) => (render_path(p, cx, true), Prec::Atom),
        ExprTree::Int(_)
        | ExprTree::Bool(_)
        | ExprTree::Old(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::QuantIn { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
        | ExprTree::Contains { .. }
        | ExprTree::Len(_)
        | ExprTree::Arith { .. }
        | ExprTree::MulDivFloor { .. }
        | ExprTree::MulDivCeil { .. }
        | ExprTree::MulDivRoundHalfUp { .. }
        | ExprTree::Match { .. }
        | ExprTree::Ctor { .. }
        | ExprTree::RecordLit { .. }
        | ExprTree::RecordUpdate { .. }
        | ExprTree::IsVariant { .. }
        | ExprTree::App { .. }
        | ExprTree::Field { .. }
        | ExprTree::Let { .. }
        | ExprTree::IfThenElse { .. } => match cx.binder {
            LeanBinder::S => {
                let rendered = render(inner, LeanCx::guard(), false).0;
                (
                    format!("\u{00AB}old({})\u{00BB}", strip_state_prefix(&rendered)),
                    Prec::Atom,
                )
            }
            LeanBinder::SPrime => render(inner, LeanCx::guard(), false),
        },
    }
}

fn render_path(p: &TreePath, cx: LeanCx, inside_old: bool) -> String {
    if let BindingKind::Const(value) = &p.binding {
        if p.segments.is_empty() {
            return value.clone();
        }
    }
    let mut out = String::new();
    match &p.binding {
        BindingKind::StateField | BindingKind::Ghost => {
            let prefix = if inside_old {
                "s"
            } else {
                match cx.binder {
                    LeanBinder::S => "s",
                    LeanBinder::SPrime => "s'",
                }
            };
            out.push_str(prefix);
        }
        BindingKind::External => {
            out.push_str(if inside_old { "pre_" } else { "post_" });
            out.push_str(&p.root);
            if let Some(TreeSeg::Field(first)) = p.segments.first() {
                out.push('_');
                out.push_str(first);
                for seg in &p.segments[1..] {
                    match seg {
                        TreeSeg::Field(field) => {
                            out.push('.');
                            out.push_str(field);
                        }
                        TreeSeg::Index(index) => match cx.subscript {
                            LeanSubscript::Brackets => {
                                out.push('[');
                                out.push_str(index);
                                out.push(']');
                            }
                            LeanSubscript::Application => {
                                out = format!("({} {})", out, index);
                            }
                        },
                    }
                }
                return out;
            }
        }
        BindingKind::Param
        | BindingKind::Const(_)
        | BindingKind::LetBound
        | BindingKind::Account
        | BindingKind::AbstractBinder
        | BindingKind::ExprBinder
        | BindingKind::Unresolved => out.push_str(&p.root),
    }
    for seg in &p.segments {
        match seg {
            TreeSeg::Field(f) => {
                out.push('.');
                out.push_str(f);
            }
            TreeSeg::Index(i) => match cx.subscript {
                LeanSubscript::Brackets => {
                    out.push('[');
                    out.push_str(i);
                    out.push(']');
                }
                LeanSubscript::Application => {
                    out = format!("({} {})", out, i);
                }
            },
        }
    }
    out
}

/// Render both sides of a binary op, coercing the `Nat` side to `Int`
/// when kinds mix (Lean doesn't implicitly coerce in arithmetic).
/// Asymmetric — only the Nat side wraps, matching the legacy form.
fn render_pair_with_coercion(
    lhs: &ExprTree,
    rhs: &ExprTree,
    cx: LeanCx,
    inside_old: bool,
    slot: Prec,
) -> (String, String) {
    let lk = lhs.num_kind();
    let rk = rhs.num_kind();
    let l = render(lhs, cx, inside_old);
    let r = render(rhs, cx, inside_old);
    match (lk, rk) {
        (NumKind::Nat, NumKind::Int) => (format!("((({}) : Int))", l.0), strict_wrap(slot, r)),
        (NumKind::Int, NumKind::Nat) => (atom_wrap(slot, l), format!("((({}) : Int))", r.0)),
        _ => (atom_wrap(slot, l), strict_wrap(slot, r)),
    }
}

fn atom_wrap(slot: Prec, child: (String, Prec)) -> String {
    match slot {
        // Comparisons are non-associative: both sides strict.
        Prec::Cmp => strict_atom_for(slot, child),
        Prec::Implies | Prec::Or | Prec::And | Prec::AddSub | Prec::MulDiv | Prec::Atom => {
            atom_for(slot, child)
        }
    }
}

fn strict_wrap(slot: Prec, child: (String, Prec)) -> String {
    match slot {
        Prec::Cmp | Prec::AddSub | Prec::MulDiv => strict_atom_for(slot, child),
        Prec::Implies | Prec::Or | Prec::And | Prec::Atom => atom_for(slot, child),
    }
}

/// `a * b` operand pair for the mul_div helpers (each side is
/// re-parenthesized by the caller's template, so no slot wrapping).
fn coerced_pair_strings(
    a: &ExprTree,
    b: &ExprTree,
    cx: LeanCx,
    inside_old: bool,
) -> (String, String) {
    let ak = a.num_kind();
    let bk = b.num_kind();
    let a_str = render(a, cx, inside_old).0;
    let b_str = render(b, cx, inside_old).0;
    match (ak, bk) {
        (NumKind::Nat, NumKind::Int) => (format!("((({}) : Int))", a_str), b_str),
        (NumKind::Int, NumKind::Nat) => (a_str, format!("((({}) : Int))", b_str)),
        _ => (a_str, b_str),
    }
}

fn strip_state_prefix(s: &str) -> String {
    s.strip_prefix("s.")
        .or_else(|| s.strip_prefix("s'."))
        .map(|r| r.to_string())
        .unwrap_or_else(|| s.to_string())
}

/// Does the tree reference `name` as a free identifier (expression binder,
/// param, unresolved, …)? Structural replacement for the rendered-text
/// whole-word scan deciding match-arm `_` binders.
pub fn tree_mentions_ident(e: &ExprTree, name: &str) -> bool {
    match e {
        ExprTree::Path(p) => {
            p.root == name
                || p.segments.iter().any(|s| match s {
                    TreeSeg::Field(_) => false,
                    TreeSeg::Index(i) => i == name,
                })
        }
        ExprTree::Int(_) | ExprTree::Bool(_) => false,
        ExprTree::Old(inner) | ExprTree::Not(inner) => tree_mentions_ident(inner, name),
        ExprTree::Sum { body, .. } | ExprTree::Quant { body, .. } => {
            tree_mentions_ident(body, name)
        }
        ExprTree::QuantIn { coll, body, .. } => {
            tree_mentions_ident(coll, name) || tree_mentions_ident(body, name)
        }
        ExprTree::Contains { coll, elem } => {
            tree_mentions_ident(coll, name) || tree_mentions_ident(elem, name)
        }
        ExprTree::Len(coll) => tree_mentions_ident(coll, name),
        ExprTree::BoolOp { lhs, rhs, .. }
        | ExprTree::Cmp { lhs, rhs, .. }
        | ExprTree::Arith { lhs, rhs, .. } => {
            tree_mentions_ident(lhs, name) || tree_mentions_ident(rhs, name)
        }
        ExprTree::MulDivFloor { a, b, d }
        | ExprTree::MulDivCeil { a, b, d }
        | ExprTree::MulDivRoundHalfUp { a, b, d } => {
            tree_mentions_ident(a, name)
                || tree_mentions_ident(b, name)
                || tree_mentions_ident(d, name)
        }
        ExprTree::Match {
            scrutinee, arms, ..
        } => {
            tree_mentions_ident(scrutinee, name)
                || arms.iter().any(|a| tree_mentions_ident(&a.body, name))
        }
        ExprTree::Ctor { payload, .. } => payload
            .as_ref()
            .is_some_and(|p| tree_mentions_ident(p, name)),
        ExprTree::RecordLit { fields, .. } => {
            fields.iter().any(|(_, v)| tree_mentions_ident(v, name))
        }
        ExprTree::RecordUpdate { base, updates, .. } => {
            tree_mentions_ident(base, name)
                || updates.iter().any(|(_, v)| tree_mentions_ident(v, name))
        }
        ExprTree::IsVariant { scrutinee, .. } => tree_mentions_ident(scrutinee, name),
        ExprTree::App { args, .. } => args.iter().any(|a| tree_mentions_ident(a, name)),
        ExprTree::Field { base, .. } => tree_mentions_ident(base, name),
        ExprTree::Let { value, body, .. } => {
            tree_mentions_ident(value, name) || tree_mentions_ident(body, name)
        }
        ExprTree::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            tree_mentions_ident(cond, name)
                || tree_mentions_ident(then_branch, name)
                || tree_mentions_ident(else_branch, name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Corpus parity: for every expression the adapter carried a tree for,
    /// the Lean tree renderer must agree with the legacy pre-rendered
    /// string — exactly, or up to redundant parens (compared with parens
    /// and whitespace stripped; no Lean parser is available in-process, so
    /// this masks pure grouping differences while still catching operator,
    /// receiver, coercion, and ordering divergences).
    fn normalized(s: &str) -> String {
        s.chars()
            .filter(|c| *c != '(' && *c != ')' && !c.is_whitespace())
            .collect()
    }

    fn equivalent(ours: &str, legacy: &str) -> bool {
        ours == legacy || normalized(ours) == normalized(legacy)
    }

    fn check(
        mismatches: &mut Vec<String>,
        where_: &str,
        tree: &ExprTree,
        cx: LeanCx,
        legacy: &str,
    ) {
        if legacy.is_empty() {
            return;
        }
        // Synthetic match-arm aborts: the legacy string is the `0 = 1`
        // Prop workaround while the tree carries the honest `Bool(false)`
        // literal — a deliberate divergence (the emission port decides the
        // Prop spelling at the render site).
        if legacy == "0 = 1" && matches!(tree, ExprTree::Bool(false)) {
            return;
        }
        let ours = render_lean(tree, cx);
        if !equivalent(&ours, legacy) {
            mismatches.push(format!("{where_}\n  tree:   {ours}\n  legacy: {legacy}"));
        }
    }

    fn parse_fixture(rel_path: &str) -> crate::check::ParsedSpec {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let root = std::path::Path::new(&manifest_dir)
            .ancestors()
            .nth(2)
            .expect("workspace root");
        crate::check::parse_spec_file(&root.join(rel_path))
            .unwrap_or_else(|e| panic!("parse {rel_path}: {e}"))
    }

    #[test]
    fn corpus_parity_with_legacy_lean_strings() {
        let mut mismatches = Vec::new();
        for fixture in &[
            "examples/rust/escrow/escrow.qedspec",
            "examples/rust/escrow-split",
            "examples/rust/lending/lending.qedspec",
            "examples/rust/multisig/multisig.qedspec",
            "examples/rust/bundled-stdlib-demo/pool.qedspec",
            "examples/rust/cross-program-vault",
            // #156 Lean emission port: widen the corpus with the regression
            // fixtures so the string→tree swaps in lean_gen_mir are parity-
            // gated over quantifiers, uninterpreted helpers, conditional
            // effects, invariants, and let bindings — not just the pilots.
            "crates/qedgen/tests/fixtures/let-bindings/fee_split.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/repro-01-u16-type.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/repro-02-composite-or-parens.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/repro-03-duplicate-theorem.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/repro-04-liveness-params.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/repro-05-uninterpreted-helper.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-8/repro-06-cover-witness-bool.qedspec",
            // (repro-07-* / repro-08-* are parse-rejection fixtures — not
            // parseable corpus.)
            "crates/qedgen/tests/fixtures/regressions/quantifiers/repro-forall-u64.qedspec",
            "crates/qedgen/tests/fixtures/regressions/quantifiers/repro-forall-u8.qedspec",
            "crates/qedgen/tests/fixtures/regressions/quantifiers/repro-forall-value-binder.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-139-bare-state-refs/generic_vault.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-42-conditional/fee_router.qedspec",
            "crates/qedgen/tests/fixtures/regressions/issue-42-conditional/adt_router.qedspec",
            "crates/qedgen/tests/fixtures/regressions/invariants/repro-establishes-clause.qedspec",
            "crates/qedgen/tests/fixtures/regressions/invariants/repro-handler-invariant-clause.qedspec",
        ] {
            let spec = parse_fixture(fixture);
            for h in &spec.handlers {
                for (i, r) in h.requires.iter().enumerate() {
                    let Some(t) = &r.tree else { continue };
                    let at = format!("{fixture} {} requires[{i}]", h.name);
                    check(&mut mismatches, &at, t, LeanCx::guard(), &r.lean_expr);
                }
                for (i, e) in h.ensures.iter().enumerate() {
                    let Some(t) = &e.tree else { continue };
                    let at = format!("{fixture} {} ensures[{i}]", h.name);
                    check(&mut mismatches, &at, t, LeanCx::ensures(), &e.lean_expr);
                }
            }
            for p in &spec.properties {
                let Some(t) = &p.tree else { continue };
                let Some(lean) = &p.expression else { continue };
                let cx = match p.class {
                    crate::check::PropertyClass::Unary => LeanCx::guard(),
                    crate::check::PropertyClass::Binary => LeanCx::ensures(),
                };
                let at = format!("{fixture} property {}", p.name);
                check(&mut mismatches, &at, t, cx, lean);
            }
            for inv in &spec.invariants {
                let (Some(t), Some(lean)) = (&inv.tree, &inv.lean_expr) else {
                    continue;
                };
                let at = format!("{fixture} invariant {}", inv.name);
                check(&mut mismatches, &at, t, LeanCx::guard(), lean);
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} Lean renderer/legacy divergences:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    use crate::mir::expr_tree::TreePath;

    fn state_field(name: &str, ty: Ty) -> ExprTree {
        ExprTree::Path(TreePath {
            root: "state".into(),
            binding: BindingKind::StateField,
            segments: vec![TreeSeg::Field(name.into())],
            ty: Some(ty),
        })
    }

    fn param(name: &str, ty: Ty) -> ExprTree {
        ExprTree::Path(TreePath {
            root: name.into(),
            binding: BindingKind::Param,
            segments: vec![],
            ty: Some(ty),
        })
    }

    #[test]
    fn binder_modes_pick_receivers() {
        let read = state_field("balance", Ty::U64);
        assert_eq!(render_lean(&read, LeanCx::guard()), "s.balance");
        assert_eq!(render_lean(&read, LeanCx::ensures()), "s'.balance");
        let old = ExprTree::Old(Box::new(read));
        assert_eq!(render_lean(&old, LeanCx::guard()), "s.balance");
        assert_eq!(render_lean(&old, LeanCx::ensures()), "s.balance");
    }

    #[test]
    fn nat_int_mix_coerces_nat_side_only() {
        let e = ExprTree::Cmp {
            op: TreeCmpOp::Ge,
            lhs: Box::new(state_field("pnl", Ty::I128)),
            rhs: Box::new(param("amount", Ty::U64)),
        };
        assert_eq!(
            render_lean(&e, LeanCx::guard()),
            "s.pnl \u{2265} (((amount) : Int))"
        );
    }

    #[test]
    fn nat_subtraction_structure_is_preserved() {
        // a - (b - c) must keep its parens — Lean Nat monus re-associates
        // to a different value without them.
        let e = ExprTree::Arith {
            op: TreeArithOp::Sub,
            lhs: Box::new(param("a", Ty::U64)),
            rhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Sub,
                lhs: Box::new(param("b", Ty::U64)),
                rhs: Box::new(param("c", Ty::U64)),
            }),
        };
        assert_eq!(render_lean(&e, LeanCx::guard()), "a - (b - c)");
        let chain = ExprTree::Arith {
            op: TreeArithOp::Sub,
            lhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Sub,
                lhs: Box::new(param("a", Ty::U64)),
                rhs: Box::new(param("b", Ty::U64)),
            }),
            rhs: Box::new(param("c", Ty::U64)),
        };
        assert_eq!(render_lean(&chain, LeanCx::guard()), "a - b - c");
    }

    #[test]
    fn bool_ops_stay_flat_weaker_children_parenthesize() {
        let a = || {
            Box::new(ExprTree::Cmp {
                op: TreeCmpOp::Gt,
                lhs: Box::new(param("a", Ty::U64)),
                rhs: Box::new(ExprTree::Int(0)),
            })
        };
        let and_chain = ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs: Box::new(ExprTree::BoolOp {
                op: TreeBoolOp::And,
                lhs: a(),
                rhs: a(),
            }),
            rhs: a(),
        };
        assert_eq!(
            render_lean(&and_chain, LeanCx::guard()),
            "a > 0 \u{2227} a > 0 \u{2227} a > 0"
        );
        let or_under_and = ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs: Box::new(ExprTree::BoolOp {
                op: TreeBoolOp::Or,
                lhs: a(),
                rhs: a(),
            }),
            rhs: a(),
        };
        assert_eq!(
            render_lean(&or_under_and, LeanCx::guard()),
            "(a > 0 \u{2228} a > 0) \u{2227} a > 0"
        );
    }
}
