//! Rust rendering of `mir::ExprTree` — #151 Slice 1.
//!
//! One renderer, parameterized by [`RustCx`], replaces the six pre-rendered
//! string fields on `mir::Expr` (`rust` / `rust_pod` / `rust_binary` /
//! `rust_math` / `rust_binary_math` / `effects_rust`): the state receiver,
//! arithmetic policy, and Pod access style become renderer *parameters*
//! chosen by the consumer at emission time, instead of six parallel strings
//! chosen by the adapter at parse time.
//!
//! Output contract: token-for-token compatible with the legacy
//! `chumsky_adapter::expr_to_rust` forms, except for paren placement — the
//! tree has no `Paren` nodes, so grouping is re-derived from structure with
//! minimal precedence-correct parens (redundant source parens don't
//! survive). The corpus parity test (`tests.rs`) checks structural
//! equivalence via `syn` for every expression in the pilot fixtures.
//!
//! Matches over `ExprTree` / `BindingKind` are exhaustive by discipline —
//! no `_` arms (see `mir::expr_tree` module docs).

use crate::mir::expr_tree::{
    BindingKind, ExprTree, NumKind, QuantKind, TreeArithOp, TreeBoolOp, TreeCmpOp, TreePath,
    TreeSeg,
};
use crate::mir::Ty;

/// How `state.<field>` reads (and `old(...)`) pick their receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binder<'a> {
    /// Single-state contexts (transition fns, property bodies, guards):
    /// `state.x` → `s.x`; `old(state.x)` collapses to `s.x`.
    S,
    /// Scaffold positions where state lives behind an account binding:
    /// `state.x` → `<name>.x`. (Slice 3 consumer; `old` collapses like `S`.)
    SelfAcct(&'a str),
    /// Two-state harness positions: `state.x` → `post.x`,
    /// `old(state.x)` → `pre.x`.
    PrePost,
    /// Conformance-harness expected values: state reads bind to the flat
    /// pre-state snapshot locals — `state.x` → `pre_x` (no receiver;
    /// `old(...)` collapses, everything is pre-state here). The harness
    /// snapshots every mutable field as `let pre_<f> = s.<f>;` before the
    /// transition call, so an expected value must never read post-state.
    PreLocal,
}

/// Arithmetic policy for `Arith` / `MulDiv*` nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithMode {
    /// Plain operators — scaffold-facing rendering.
    Native,
    /// Effect-RHS rendering (issue #146): bare arithmetic lowers to
    /// `checked_*` + `?`; `mul_div_*` narrows via `.try_into().ok()?`.
    /// The consumer must evaluate the result inside an `Option` closure.
    Checked,
    /// Math-exact predicate rendering (issue #146): arithmetic inside
    /// comparisons widens to `u128`/`i128` so evaluation can't
    /// overflow-panic; matches the Lean `Nat` model.
    Widened,
    /// The proptest guard composite — `Widened` plus the legacy
    /// `wrap_arithmetic` pass: the *top-level* `+` spine of each
    /// comparison side lowers to `.wrapping_add(…)` (recursing left,
    /// matching Lean's left-associative chains), a top-level `i128` `-`
    /// to `.saturating_sub(…)`; arithmetic nested under method calls
    /// keeps the plain `Widened` form. Byte-compatible with
    /// `wrap_arithmetic(translate_guard_to_rust(rust_expr_math))`.
    Wrapping,
}

/// How handler-account reads lower in scaffold guard positions (#151
/// Slice 3): bare `<acct>` and `<acct>.pubkey` both mean "this account's
/// runtime address", loaded off the guard fn's `ctx` param. Tree-native
/// replacement for `bind_state`'s account-ident string rewrite; the
/// rendered forms MUST stay in sync with
/// `FrameworkSurface::account_key_expr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcctKeyStyle {
    /// Anchor: `ctx.<name>.key()`.
    AnchorCtx,
    /// Quasar: `(*ctx.<name>.to_account_view().address())`.
    QuasarCtx,
}

impl AcctKeyStyle {
    fn render(self, name: &str) -> String {
        match self {
            AcctKeyStyle::AnchorCtx => format!("ctx.{}.key()", name),
            AcctKeyStyle::QuasarCtx => format!("(*ctx.{}.to_account_view().address())", name),
        }
    }
}

/// Render context — the Slice 1 collapse of the six string forms.
#[derive(Debug, Clone, Copy)]
pub struct RustCx<'a> {
    pub binder: Binder<'a>,
    pub arith: ArithMode,
    /// Quasar Pod access: state/record integer fields (width ≥ 16) read
    /// through `.get()`.
    pub pod: bool,
    /// Account-environment binder: `Some("accounts")` renders handler
    /// account pubkey reads (`owner.pubkey`) through the generated env
    /// struct (`accounts.owner.pubkey`). Tree-native replacement for
    /// `rewrite_account_pubkey_refs`'s string substitution.
    pub acct_env: Option<&'a str>,
    /// Scaffold-guard account lowering: bare `<acct>` / `<acct>.pubkey`
    /// render as the target's runtime key load (see [`AcctKeyStyle`]).
    /// Mutually exclusive with `acct_env` by construction (guards vs
    /// harness positions).
    pub acct_key: Option<AcctKeyStyle>,
}

impl<'a> RustCx<'a> {
    pub fn native() -> Self {
        RustCx {
            binder: Binder::S,
            arith: ArithMode::Native,
            pod: false,
            acct_env: None,
            acct_key: None,
        }
    }
    /// Test-only convenience (corpus parity + mode tests); non-test
    /// consumers set `pod` via struct update on a mode-specific context.
    #[cfg(test)]
    pub fn pod() -> Self {
        RustCx {
            pod: true,
            ..RustCx::native()
        }
    }
    pub fn with_binder(self, binder: Binder<'a>) -> Self {
        RustCx { binder, ..self }
    }
    pub fn with_arith(self, arith: ArithMode) -> Self {
        RustCx { arith, ..self }
    }
    pub fn with_acct_env(self, acct_env: Option<&'a str>) -> Self {
        RustCx { acct_env, ..self }
    }
    pub fn with_acct_key(self, acct_key: Option<AcctKeyStyle>) -> Self {
        RustCx { acct_key, ..self }
    }
}

/// Operator precedence for minimal-paren rendering. Higher binds tighter.
/// `Atom` never needs wrapping; strings that self-parenthesize (bool ops,
/// casts, `let`/`if` blocks, method-call chains) report `Atom`.
///
/// The paren rules preserve the *tree's* evaluation order, not just parse
/// validity: `Add(a, Sub(b, c))` must render `a + (b - c)` — `a + b - c`
/// re-associates and changes overflow behavior on unsigned types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Or,
    Cmp,
    AddSub,
    MulDiv,
    Atom,
}

/// Render `e` under `cx`. Entry point for every Rust-emitting consumer.
pub fn render_rust(e: &ExprTree, cx: RustCx) -> String {
    render(e, cx, false).0
}

/// Core renderer: returns the rendered string plus its top-level
/// precedence so parents can insert only the parens structure requires.
/// `inside_old` tracks descent through `Old(...)` for the `PrePost` binder.
fn render(e: &ExprTree, cx: RustCx, inside_old: bool) -> (String, Prec) {
    match e {
        ExprTree::Int(v) => (v.to_string(), Prec::Atom),
        ExprTree::Bool(b) => (b.to_string(), Prec::Atom),
        ExprTree::Path(p) => (render_path(p, cx, inside_old), Prec::Atom),
        ExprTree::Old(inner) => render(inner, cx, true),
        ExprTree::Sum {
            binder,
            binder_ty,
            body,
        } => (
            format!(
                "sum_over::<{}>(|{}| {})",
                binder_ty,
                binder,
                render(body, cx, inside_old).0
            ),
            Prec::Atom,
        ),
        ExprTree::Quant {
            kind,
            binder,
            binder_ty,
            fin_bound,
            body,
        } => (
            render_quant(*kind, binder, binder_ty, fin_bound, body, cx, inside_old),
            Prec::Atom,
        ),
        ExprTree::QuantIn {
            kind,
            binder,
            coll,
            body,
        } => {
            // Bounded quantifier over a collection: `coll.iter().any|all(|x| …)`.
            let method = match kind {
                QuantKind::Forall => "all",
                QuantKind::Exists => "any",
            };
            (
                format!(
                    "{}.iter().{}(|{}| {})",
                    render(coll, cx, inside_old).0,
                    method,
                    binder,
                    render(body, cx, inside_old).0
                ),
                Prec::Atom,
            )
        }
        // Bool ops parenthesize both operands unconditionally — matches the
        // legacy output byte-for-byte and sidesteps `&&`/`||` precedence.
        ExprTree::BoolOp { op, lhs, rhs } => {
            let l = render(lhs, cx, inside_old).0;
            let r = render(rhs, cx, inside_old).0;
            let s = match op {
                TreeBoolOp::And => format!("({}) && ({})", l, r),
                TreeBoolOp::Or => format!("({}) || ({})", l, r),
                // `a implies b` ≡ `!a || b`.
                TreeBoolOp::Implies => format!("(!({})) || ({})", l, r),
            };
            (s, Prec::Atom)
        }
        ExprTree::Not(inner) => (
            format!("!({})", render(inner, cx, inside_old).0),
            Prec::Atom,
        ),
        ExprTree::Cmp { op, lhs, rhs } => (render_cmp(*op, lhs, rhs, cx, inside_old), Prec::Cmp),
        ExprTree::Arith { op, lhs, rhs } => render_arith(*op, lhs, rhs, cx, inside_old),
        ExprTree::MulDivFloor { a, b, d } => (
            render_mul_div("mul_div_floor_u128", a, b, d, cx, inside_old),
            Prec::Atom,
        ),
        ExprTree::MulDivCeil { a, b, d } => (
            render_mul_div("mul_div_ceil_u128", a, b, d, cx, inside_old),
            Prec::Atom,
        ),
        // `contains(coll, elem)` → `coll.contains(&elem)`.
        ExprTree::Contains { coll, elem } => (
            format!(
                "{}.contains(&{})",
                render(coll, cx, inside_old).0,
                render(elem, cx, inside_old).0
            ),
            Prec::Atom,
        ),
        // `len(coll)` → `coll.len() as u64`.
        ExprTree::Len(coll) => (
            format!("({}.len() as u64)", render(coll, cx, inside_old).0),
            Prec::Atom,
        ),
        ExprTree::Match {
            scrutinee,
            arms,
            enum_ty,
        } => {
            let sc = render(scrutinee, cx, inside_old).0;
            // Enum name: the build-time-resolved `enum_ty`, else the scrutinee
            // Path leaf `Ty::Custom`.
            let enum_name = enum_ty.clone().or_else(|| match scrutinee.as_ref() {
                ExprTree::Path(p) => match &p.ty {
                    Some(Ty::Custom(name)) => Some(name.clone()),
                    _ => None,
                },
                _ => None,
            });
            let en = enum_name.as_deref().unwrap_or("");
            // `.clone()` the scrutinee → owned payload binders (works for value
            // and collection uses; can't move out of a `&`-place under `.iter()`).
            let mut out = format!("match ({}).clone() {{", sc);
            for arm in arms {
                let pat = if arm.variant == "_" {
                    "_".to_string()
                } else {
                    arm.shape
                        .arm_pattern(en, &arm.variant, arm.binder.as_deref())
                };
                out.push_str(&format!(
                    "\n    {} => {},",
                    pat,
                    render(&arm.body, cx, inside_old).0
                ));
            }
            out.push_str("\n}");
            (out, Prec::Atom)
        }
        ExprTree::Ctor { variant, payload } => (
            match payload {
                None => format!("{}::{}", "/* ty */", variant),
                Some(p) => format!(
                    "{}::{}({})",
                    "/* ty */",
                    variant,
                    render(p, cx, inside_old).0
                ),
            },
            Prec::Atom,
        ),
        ExprTree::RecordLit(fields) => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{}: {}", n, render(v, cx, inside_old).0))
                .collect::<Vec<_>>()
                .join(", ");
            (format!("{} {{ {} }}", "/* ty */", body), Prec::Atom)
        }
        ExprTree::RecordUpdate { base, updates } => {
            let base_str = render(base, cx, inside_old).0;
            let body = updates
                .iter()
                .map(|(n, v)| format!("{}: {}", n, render(v, cx, inside_old).0))
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!("{} {{ {}, ..{} }}", "/* ty */", body, base_str),
                Prec::Atom,
            )
        }
        ExprTree::IsVariant {
            scrutinee,
            variant,
            enum_ty,
            shape,
        } => {
            let sc = render(scrutinee, cx, inside_old).0;
            // Enum name: the build-time-resolved `enum_ty`, else the
            // scrutinee Path leaf `Ty::Custom` (the same source the Lean
            // renderer uses).
            let enum_name = enum_ty.clone().or_else(|| match scrutinee.as_ref() {
                ExprTree::Path(p) => match &p.ty {
                    Some(Ty::Custom(name)) => Some(name.clone()),
                    _ => None,
                },
                _ => None,
            });
            let pat = match enum_name {
                Some(e) => shape.match_pattern(&e, variant),
                // Truly unresolved (non-Path scrutinee, untyped) — emit the
                // resolved shape on the bare variant name as a last resort.
                None => shape
                    .match_pattern("", variant)
                    .trim_start_matches("::")
                    .to_string(),
            };
            (format!("matches!({}, {})", sc, pat), Prec::Atom)
        }
        ExprTree::App { func, args } => {
            // Builtins: `now()` reads the on-chain clock; `unwrap()` so the
            // expression is valid in assertion/property bodies.
            if func == "now" && args.is_empty() {
                return (
                    "(solana_program::clock::Clock::get().unwrap().unix_timestamp as u64)"
                        .to_string(),
                    Prec::Atom,
                );
            }
            if func == "current_epoch" && args.is_empty() {
                return (
                    "solana_program::clock::Clock::get().unwrap().epoch".to_string(),
                    Prec::Atom,
                );
            }
            let args_str: Vec<String> = args.iter().map(|a| render(a, cx, inside_old).0).collect();
            (format!("{}({})", func, args_str.join(", ")), Prec::Atom)
        }
        ExprTree::Field { base, field } => (
            format!("{}.{}", render(base, cx, inside_old).0, field),
            Prec::Atom,
        ),
        ExprTree::Let { name, value, body } => (
            format!(
                "({{ let {} = {}; {} }})",
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
                "(if {} {{ {} }} else {{ {} }})",
                render(cond, cx, inside_old).0,
                render(then_branch, cx, inside_old).0,
                render(else_branch, cx, inside_old).0,
            ),
            Prec::Atom,
        ),
    }
}

/// Wrap `child` in parens when its precedence is strictly weaker than the
/// slot — the *left*-operand rule (left-associative operators keep their
/// tree shape at equal precedence).
fn atom_for(slot: Prec, child: (String, Prec)) -> String {
    if child.1 < slot {
        format!("({})", child.0)
    } else {
        child.0
    }
}

/// Wrap `child` at equal-or-weaker precedence — the *right*-operand rule
/// (and both operands of non-associative comparisons): `Add(a, Add(b, c))`
/// renders `a + (b + c)` so Rust's left-assoc parse can't re-associate the
/// tree, and `Cmp(Cmp(a, b), c)` renders `(a == b) == c` (chained
/// comparison is a parse error).
fn strict_atom_for(slot: Prec, child: (String, Prec)) -> String {
    if child.1 <= slot {
        format!("({})", child.0)
    } else {
        child.0
    }
}

// ----------------------------------------------------------------------
// Paths
// ----------------------------------------------------------------------

fn render_path(p: &TreePath, cx: RustCx, inside_old: bool) -> String {
    // Const substitution: resolved value replaces the name (legacy parity —
    // bare idents only; a const with segments renders as a path).
    if let BindingKind::Const(value) = &p.binding {
        if p.segments.is_empty() {
            return value.clone();
        }
    }
    let mut out = String::new();
    // PreLocal folds the first field segment into the flat snapshot-local
    // name (`pre_balance`); the remaining segments render normally.
    let mut segs: &[TreeSeg] = &p.segments;
    match &p.binding {
        BindingKind::StateField | BindingKind::Ghost => match (cx.binder, inside_old) {
            (Binder::S, _) => out.push('s'),
            (Binder::SelfAcct(name), _) => out.push_str(name),
            (Binder::PrePost, true) => out.push_str("pre"),
            (Binder::PrePost, false) => out.push_str("post"),
            (Binder::PreLocal, _) => {
                out.push_str("pre");
                if let Some(TreeSeg::Field(first)) = p.segments.first() {
                    out.push('_');
                    out.push_str(first);
                    segs = &p.segments[1..];
                }
            }
        },
        BindingKind::Account => {
            // Scaffold-guard positions: bare `<acct>` and `<acct>.pubkey`
            // both lower to the runtime key load (`ctx.<name>.key()` on
            // Anchor) — the `.pubkey` segment is consumed by the load, so
            // return early. Other projections fall through to the plain
            // path rendering, matching the legacy rewrite's "keep the `.`
            // access path" rule.
            if let Some(style) = cx.acct_key {
                let is_pubkey_read = p.segments.is_empty()
                    || matches!(p.segments.as_slice(), [TreeSeg::Field(f)] if f == "pubkey");
                if is_pubkey_read {
                    return style.render(&p.root);
                }
            }
            // Pubkey reads route through the generated account env when
            // one is bound (`owner.pubkey` → `accounts.owner.pubkey`) —
            // scoped to the exact `.pubkey` shape the legacy string
            // rewrite handled; other account projections pass through.
            if let (Some(env), [TreeSeg::Field(f)]) = (cx.acct_env, p.segments.as_slice()) {
                if f == "pubkey" {
                    out.push_str(env);
                    out.push('.');
                }
            }
            out.push_str(&p.root);
        }
        BindingKind::Param
        | BindingKind::Const(_)
        | BindingKind::LetBound
        | BindingKind::AbstractBinder
        | BindingKind::ExprBinder
        | BindingKind::Unresolved => out.push_str(&p.root),
    }
    for seg in segs {
        match seg {
            TreeSeg::Field(f) => {
                out.push('.');
                out.push_str(f);
            }
            TreeSeg::Index(i) => {
                // `Map[N] T` lowers to `[T; N]`; the index may be a
                // u8/u16/Fin param — cast to usize (always safe: unsigned).
                out.push_str(&format!("[({}) as usize]", i));
            }
        }
    }
    if cx.pod && path_is_pod_field(p) {
        out.push_str(".get()");
    }
    out
}

/// Pod companion gate: state-rooted reads whose leaf type lowers to a
/// Quasar Pod type (`u8`/`i8` stay native — alignment 1 already).
/// Tree-native replacement for `TypeEnv::path_is_pod_field`.
fn path_is_pod_field(p: &TreePath) -> bool {
    let state_rooted = matches!(p.binding, BindingKind::StateField | BindingKind::Ghost);
    if !state_rooted {
        return false;
    }
    match &p.ty {
        Some(Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 | Ty::I64 | Ty::I128 | Ty::Bool) => true,
        // `I8` arrives as `Custom` (like `I16`/`I32` — `mir::Ty` doesn't
        // model the narrow signed widths natively) but stays native:
        // alignment 1 already, no Pod companion.
        Some(Ty::Custom(name)) => matches!(name.as_str(), "I16" | "I32"),
        Some(Ty::U8 | Ty::Pubkey | Ty::Bytes32 | Ty::Bytes64 | Ty::Map { .. }) => false,
        None => false,
    }
}

// ----------------------------------------------------------------------
// Quantifiers
// ----------------------------------------------------------------------

fn render_quant(
    kind: QuantKind,
    binder: &str,
    binder_ty: &str,
    fin_bound: &Option<String>,
    body: &ExprTree,
    cx: RustCx,
    inside_old: bool,
) -> String {
    let method = match kind {
        QuantKind::Forall => "all",
        QuantKind::Exists => "any",
    };
    let body_rust = render(body, cx, inside_old).0;
    // Bounded `exists` iterates `0..N`. `forall` over `Fin[N]` deliberately
    // does NOT take this path — it keeps the per-slot lowering so
    // preservation checks the one modified slot (see the legacy renderer).
    if matches!(kind, QuantKind::Exists) {
        if let Some(bound) = fin_bound {
            return format!("(0..({} as usize)).any(|{}| {})", bound, binder, body_rust);
        }
    }
    let rust_ty = match binder_ty {
        "U8" => Some("u8"),
        "I8" => Some("i8"),
        _ => None,
    };
    let Some(rust_ty) = rust_ty else {
        let kind_name = match kind {
            QuantKind::Forall => "forall",
            QuantKind::Exists => "exists",
        };
        return format!(
            "/* QEDGEN_UNSUPPORTED_QUANTIFIER: {} {} : {} — lower at harness level */",
            kind_name, binder, binder_ty
        );
    };
    format!(
        "({}::MIN..={}::MAX).{}(|{}| {})",
        rust_ty, rust_ty, method, binder, body_rust
    )
}

// ----------------------------------------------------------------------
// Kind inference (Rust flavor)
// ----------------------------------------------------------------------

/// Rust-flavor kind: like `ExprTree::num_kind` but `MulDiv*` reports `Nat`
/// (the emitted helpers return `u128`). Mirrors the legacy
/// `rust_infer_kind` override.
fn rust_num_kind(e: &ExprTree) -> NumKind {
    match e {
        ExprTree::MulDivFloor { .. } | ExprTree::MulDivCeil { .. } | ExprTree::Len(_) => {
            NumKind::Nat
        }
        ExprTree::Old(inner) => rust_num_kind(inner),
        ExprTree::Int(_)
        | ExprTree::Bool(_)
        | ExprTree::Path(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::QuantIn { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
        | ExprTree::Contains { .. }
        | ExprTree::Arith { .. }
        | ExprTree::Match { .. }
        | ExprTree::Ctor { .. }
        | ExprTree::RecordLit(_)
        | ExprTree::RecordUpdate { .. }
        | ExprTree::IsVariant { .. }
        | ExprTree::App { .. }
        | ExprTree::Field { .. }
        | ExprTree::Let { .. }
        | ExprTree::IfThenElse { .. } => e.num_kind(),
    }
}

/// Bare arithmetic at the spine — an `Arith` node under `Old` wrappers.
/// The bps shape `(a * b) / 10000` is exempt: the Kani backend rewrites it
/// to its solver-tuned helper, and widening would hide the pattern.
fn spine_has_arith(e: &ExprTree) -> bool {
    if is_bps_div_shape(e) {
        return false;
    }
    match e {
        ExprTree::Arith { .. } => true,
        ExprTree::Old(inner) => spine_has_arith(inner),
        ExprTree::Int(_)
        | ExprTree::Bool(_)
        | ExprTree::Path(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::QuantIn { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
        | ExprTree::Contains { .. }
        | ExprTree::Len(_)
        | ExprTree::MulDivFloor { .. }
        | ExprTree::MulDivCeil { .. }
        | ExprTree::Match { .. }
        | ExprTree::Ctor { .. }
        | ExprTree::RecordLit(_)
        | ExprTree::RecordUpdate { .. }
        | ExprTree::IsVariant { .. }
        | ExprTree::App { .. }
        | ExprTree::Field { .. }
        | ExprTree::Let { .. }
        | ExprTree::IfThenElse { .. } => false,
    }
}

/// `(a * b) / 10000` (under optional `Old`) — the shape
/// `rewrite_kani_bps_mul_div` recognizes.
pub(crate) fn is_bps_div_shape(e: &ExprTree) -> bool {
    fn is_mul(e: &ExprTree) -> bool {
        match e {
            ExprTree::Old(inner) => is_mul(inner),
            ExprTree::Arith {
                op: TreeArithOp::Mul,
                ..
            } => true,
            _ => false,
        }
    }
    match e {
        ExprTree::Old(inner) => is_bps_div_shape(inner),
        ExprTree::Arith {
            op: TreeArithOp::Div,
            lhs,
            rhs,
        } => matches!(rhs.as_ref(), ExprTree::Int(10000)) && is_mul(lhs),
        _ => false,
    }
}

// ----------------------------------------------------------------------
// Comparisons and arithmetic
// ----------------------------------------------------------------------

fn cmp_sym(op: TreeCmpOp) -> &'static str {
    match op {
        TreeCmpOp::Eq => "==",
        TreeCmpOp::Ne => "!=",
        TreeCmpOp::Le => "<=",
        TreeCmpOp::Ge => ">=",
        TreeCmpOp::Lt => "<",
        TreeCmpOp::Gt => ">",
    }
}

fn render_cmp(
    op: TreeCmpOp,
    lhs: &ExprTree,
    rhs: &ExprTree,
    cx: RustCx,
    inside_old: bool,
) -> String {
    let sym = cmp_sym(op);
    // Math-exact predicate modes: a comparison whose spine carries bare
    // arithmetic evaluates both sides widened so the predicate can't
    // overflow-panic (issue #146); `Wrapping` further lowers the
    // top-level `+`/`-` spine to method chains. Non-numeric and
    // arithmetic-free comparisons keep the native rendering.
    if matches!(cx.arith, ArithMode::Widened | ArithMode::Wrapping)
        && (spine_has_arith(lhs) || spine_has_arith(rhs))
    {
        let lk = rust_num_kind(lhs);
        let rk = rust_num_kind(rhs);
        if matches!(lk, NumKind::Nat | NumKind::Int) && matches!(rk, NumKind::Nat | NumKind::Int) {
            let wide = if lk == NumKind::Int || rk == NumKind::Int {
                "i128"
            } else {
                "u128"
            };
            let term = |e: &ExprTree| {
                if cx.arith == ArithMode::Wrapping {
                    (
                        render_pred_wrapped_term(e, cx, inside_old, wide),
                        Prec::Atom,
                    )
                } else {
                    render_widened_term(e, cx, inside_old, wide)
                }
            };
            let l = term(lhs);
            let r = term(rhs);
            return format!(
                "{} {} {}",
                strict_atom_for(Prec::Cmp, l),
                sym,
                strict_atom_for(Prec::Cmp, r)
            );
        }
    }
    let (l, r) = render_pair_with_coercion(lhs, rhs, cx, inside_old, Prec::Cmp);
    format!("{} {} {}", l, sym, r)
}

/// `Wrapping`-mode comparison side: the top-level `+` spine chains
/// `.wrapping_add(…)` left-associatively; a top-level `i128` `-` chains
/// `.saturating_sub(…)` (`u128` subtraction already saturates in the
/// `Widened` form); everything below the spine keeps the `Widened`
/// rendering — arithmetic nested under a method call stays plain, exactly
/// like the legacy `wrap_arithmetic` pass that only rewrote top-level
/// operators of each comparison side.
fn render_pred_wrapped_term(e: &ExprTree, cx: RustCx, inside_old: bool, wide: &str) -> String {
    match e {
        ExprTree::Old(inner) => render_pred_wrapped_term(inner, cx, true, wide),
        // A `Bool` predicate (not a numeric term to widen) — render plainly.
        ExprTree::Contains { .. } => render(e, cx, inside_old).0,
        ExprTree::Len(_) => render(e, cx, inside_old).0,
        ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs,
            rhs,
        } => format!(
            "{}.wrapping_add({})",
            render_pred_wrapped_term(lhs, cx, inside_old, wide),
            render_widened_term(rhs, cx, inside_old, wide).0
        ),
        ExprTree::Arith {
            op: TreeArithOp::Sub,
            lhs,
            rhs,
        } if wide == "i128" => format!(
            "{}.saturating_sub({})",
            render_pred_wrapped_term(lhs, cx, inside_old, wide),
            render_widened_term(rhs, cx, inside_old, wide).0
        ),
        ExprTree::Int(_)
        | ExprTree::Bool(_)
        | ExprTree::Path(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::QuantIn { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
        | ExprTree::Arith { .. }
        | ExprTree::MulDivFloor { .. }
        | ExprTree::MulDivCeil { .. }
        | ExprTree::Match { .. }
        | ExprTree::Ctor { .. }
        | ExprTree::RecordLit(_)
        | ExprTree::RecordUpdate { .. }
        | ExprTree::IsVariant { .. }
        | ExprTree::App { .. }
        | ExprTree::Field { .. }
        | ExprTree::Let { .. }
        | ExprTree::IfThenElse { .. } => render_widened_term(e, cx, inside_old, wide).0,
    }
}

fn render_arith(
    op: TreeArithOp,
    lhs: &ExprTree,
    rhs: &ExprTree,
    cx: RustCx,
    inside_old: bool,
) -> (String, Prec) {
    // Checked effect-RHS mode: bare arithmetic lowers to `checked_*` + `?`
    // (over/underflow rejects the transition instead of panicking).
    if cx.arith == ArithMode::Checked {
        let method = match op {
            TreeArithOp::Add => "checked_add",
            TreeArithOp::Sub => "checked_sub",
            TreeArithOp::Mul => "checked_mul",
            TreeArithOp::Div => "checked_div",
            TreeArithOp::Mod => "checked_rem",
        };
        let (l, r) = render_pair_with_coercion(lhs, rhs, cx, inside_old, Prec::Or);
        return (format!("({}).{}({})?", l, method, r), Prec::Atom);
    }
    // `Wrapping` only rewrites the comparison-side spine (see
    // `render_pred_wrapped_term`); an `Arith` outside any comparison
    // renders native — matching the legacy pass, which never touched
    // arithmetic outside a `cmp` atom.
    let prec = match op {
        TreeArithOp::Add | TreeArithOp::Sub => Prec::AddSub,
        TreeArithOp::Mul | TreeArithOp::Div | TreeArithOp::Mod => Prec::MulDiv,
    };
    let sym = match op {
        TreeArithOp::Add => " + ",
        TreeArithOp::Sub => " - ",
        TreeArithOp::Mul => " * ",
        TreeArithOp::Div => " / ",
        TreeArithOp::Mod => " % ",
    };
    let lk = rust_num_kind(lhs);
    let rk = rust_num_kind(rhs);
    let lr = render(lhs, cx, inside_old);
    let rr = render(rhs, cx, inside_old);
    if kinds_mix(lk, rk) {
        // Nat/Int mix casts BOTH sides to i128 (casting one leaves
        // `u128 + i128`, which doesn't typecheck). Casts are atoms.
        let l = format!("(({}) as i128)", lr.0);
        let r = format!("(({}) as i128)", rr.0);
        return (format!("{}{}{}", l, sym, r), prec);
    }
    let l = atom_for(prec, lr);
    let r = strict_atom_for(prec, rr);
    (format!("{}{}{}", l, sym, r), prec)
}

fn kinds_mix(lk: NumKind, rk: NumKind) -> bool {
    matches!(
        (lk, rk),
        (NumKind::Nat, NumKind::Int) | (NumKind::Int, NumKind::Nat)
    )
}

/// Render both operands of a binary op, casting to `i128` when Nat/Int
/// kinds mix; otherwise wrap for the given slot precedence.
fn render_pair_with_coercion(
    lhs: &ExprTree,
    rhs: &ExprTree,
    cx: RustCx,
    inside_old: bool,
    slot: Prec,
) -> (String, String) {
    let lk = rust_num_kind(lhs);
    let rk = rust_num_kind(rhs);
    let l = render(lhs, cx, inside_old);
    let r = render(rhs, cx, inside_old);
    if kinds_mix(lk, rk) {
        (
            format!("(({}) as i128)", l.0),
            format!("(({}) as i128)", r.0),
        )
    } else {
        // Strict on both sides: the only meaningful slot here is `Cmp`
        // (non-associative — nested comparisons must parenthesize either
        // way); the checked/wrapping callers pass `Or`, which never wraps.
        (strict_atom_for(slot, l), strict_atom_for(slot, r))
    }
}

/// Render a numeric term so its Rust type is exactly `wide` (`u128` /
/// `i128`), evaluating internal arithmetic without panics: `+` is exact,
/// `-` on `u128` saturates (Lean `Nat` monus), `*` saturates, `/` and `%`
/// follow the Lean total-function convention (`x / 0 = 0`, `x % 0 = x`).
/// Leaves render natively and cast up.
fn render_widened_term(e: &ExprTree, cx: RustCx, inside_old: bool, wide: &str) -> (String, Prec) {
    match e {
        ExprTree::Old(inner) => render_widened_term(inner, cx, true, wide),
        // A `Bool` predicate (not a numeric term to widen) — render plainly.
        ExprTree::Contains { .. } => render(e, cx, inside_old),
        ExprTree::Len(_) => render(e, cx, inside_old),
        ExprTree::Arith { op, lhs, rhs } => {
            let l = render_widened_term(lhs, cx, inside_old, wide);
            let r = render_widened_term(rhs, cx, inside_old, wide);
            match op {
                TreeArithOp::Add => (
                    format!(
                        "{} + {}",
                        atom_for(Prec::AddSub, l),
                        strict_atom_for(Prec::AddSub, r)
                    ),
                    Prec::AddSub,
                ),
                TreeArithOp::Sub => {
                    if wide == "u128" {
                        (format!("({}).saturating_sub({})", l.0, r.0), Prec::Atom)
                    } else {
                        (
                            format!(
                                "{} - {}",
                                atom_for(Prec::AddSub, l),
                                strict_atom_for(Prec::AddSub, r)
                            ),
                            Prec::AddSub,
                        )
                    }
                }
                TreeArithOp::Mul => (format!("({}).saturating_mul({})", l.0, r.0), Prec::Atom),
                TreeArithOp::Div => (
                    format!("({}).checked_div({}).unwrap_or(0)", l.0, r.0),
                    Prec::Atom,
                ),
                TreeArithOp::Mod => (
                    format!("({}).checked_rem({}).unwrap_or({})", l.0, r.0, l.0),
                    Prec::Atom,
                ),
            }
        }
        // Already u128-typed helpers — cast only when the wide type differs.
        ExprTree::MulDivFloor { .. } | ExprTree::MulDivCeil { .. } => {
            let s = render(e, cx, inside_old).0;
            if wide == "u128" {
                (s, Prec::Atom)
            } else {
                (format!("(({}) as {})", s, wide), Prec::Atom)
            }
        }
        ExprTree::Int(_)
        | ExprTree::Bool(_)
        | ExprTree::Path(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::QuantIn { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
        | ExprTree::Match { .. }
        | ExprTree::Ctor { .. }
        | ExprTree::RecordLit(_)
        | ExprTree::RecordUpdate { .. }
        | ExprTree::IsVariant { .. }
        | ExprTree::App { .. }
        | ExprTree::Field { .. }
        | ExprTree::Let { .. }
        | ExprTree::IfThenElse { .. } => (
            format!("(({}) as {})", render(e, cx, inside_old).0, wide),
            Prec::Atom,
        ),
    }
}

// ----------------------------------------------------------------------
// mul_div helpers
// ----------------------------------------------------------------------

/// `mul_div_{floor,ceil}_u128` take `u128` args; operands cast
/// unconditionally (`as u128` widens from u64; from i128 it truncates,
/// matching the Lean Int → u128 lowering).
fn render_mul_div(
    helper: &str,
    a: &ExprTree,
    b: &ExprTree,
    d: &ExprTree,
    cx: RustCx,
    inside_old: bool,
) -> String {
    let arg = |e: &ExprTree| format!("(({}) as u128)", render(e, cx, inside_old).0);
    let call = format!("{}({}, {}, {})", helper, arg(a), arg(b), arg(d));
    // Checked effect-RHS mode: the helper is u128-typed but the assignment
    // target has the field's native width — narrow fallibly so an
    // out-of-range result rejects the transition instead of truncating.
    if cx.arith == ArithMode::Checked {
        format!("({}).try_into().ok()?", call)
    } else {
        call
    }
}

/// Does the tree read any handler-account pubkey (`<acct>.pubkey`)?
/// Structural replacement for `mentions_handler_account_pubkey`'s
/// substring scan — used to suppress accounts-only requires from the
/// pure-model harness projection (the harness `State` carries no handler
/// accounts) when no account env is bound. Rides the `for_each_path`
/// spine below, so new `ExprTree` variants are handled in one place.
pub fn tree_mentions_account_pubkey(e: &ExprTree) -> bool {
    let mut found = false;
    for_each_path(e, &mut |p| {
        if matches!(p.binding, BindingKind::Account)
            && matches!(p.segments.as_slice(), [TreeSeg::Field(f)] if f == "pubkey")
        {
            found = true;
        }
    });
    found
}

/// Walk every [`TreePath`] in `e`, pre-order. Shared spine for the
/// path-shape predicates below (one exhaustive match instead of one per
/// predicate).
pub fn for_each_path(e: &ExprTree, f: &mut impl FnMut(&TreePath)) {
    match e {
        ExprTree::Path(p) => f(p),
        ExprTree::Int(_) | ExprTree::Bool(_) => {}
        ExprTree::Old(inner) | ExprTree::Not(inner) => for_each_path(inner, f),
        ExprTree::Sum { body, .. } | ExprTree::Quant { body, .. } => for_each_path(body, f),
        ExprTree::QuantIn { coll, body, .. } => {
            for_each_path(coll, f);
            for_each_path(body, f);
        }
        ExprTree::BoolOp { lhs, rhs, .. }
        | ExprTree::Cmp { lhs, rhs, .. }
        | ExprTree::Arith { lhs, rhs, .. } => {
            for_each_path(lhs, f);
            for_each_path(rhs, f);
        }
        ExprTree::MulDivFloor { a, b, d } | ExprTree::MulDivCeil { a, b, d } => {
            for_each_path(a, f);
            for_each_path(b, f);
            for_each_path(d, f);
        }
        ExprTree::Contains { coll, elem } => {
            for_each_path(coll, f);
            for_each_path(elem, f);
        }
        ExprTree::Len(coll) => for_each_path(coll, f),
        ExprTree::Match {
            scrutinee, arms, ..
        } => {
            for_each_path(scrutinee, f);
            for arm in arms {
                for_each_path(&arm.body, f);
            }
        }
        ExprTree::Ctor { payload, .. } => {
            if let Some(p) = payload {
                for_each_path(p, f);
            }
        }
        ExprTree::RecordLit(fields) => {
            for (_, v) in fields {
                for_each_path(v, f);
            }
        }
        ExprTree::RecordUpdate { base, updates } => {
            for_each_path(base, f);
            for (_, v) in updates {
                for_each_path(v, f);
            }
        }
        ExprTree::IsVariant { scrutinee, .. } => for_each_path(scrutinee, f),
        ExprTree::App { args, .. } => {
            for a in args {
                for_each_path(a, f);
            }
        }
        ExprTree::Field { base, .. } => for_each_path(base, f),
        ExprTree::Let { value, body, .. } => {
            for_each_path(value, f);
            for_each_path(body, f);
        }
        ExprTree::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            for_each_path(cond, f);
            for_each_path(then_branch, f);
            for_each_path(else_branch, f);
        }
    }
}

/// Every handler-account read in `e` is key-style — bare `<acct>` or
/// `<acct>.pubkey` — i.e. a shape [`AcctKeyStyle`] can lower. Gate for the
/// scaffold-guards tree path: other account projections (imported-account
/// field reads like `<acct>.<field>`) still need the legacy mirror rewrite
/// in `bind_state`, so those clauses fall back to the string path.
pub fn account_reads_are_key_style(e: &ExprTree) -> bool {
    let mut ok = true;
    for_each_path(e, &mut |p| {
        if matches!(p.binding, BindingKind::Account) {
            let key_style = p.segments.is_empty()
                || matches!(p.segments.as_slice(), [TreeSeg::Field(f)] if f == "pubkey");
            if !key_style {
                ok = false;
            }
        }
    });
    ok
}

/// Does `e` read an `abstract <name> : <Type>` existential binder?
/// Structural replacement for the guards emitter's word-boundary substring
/// scan — the binder is computed AFTER the guard fn runs, so referencing
/// clauses are deferred to the handler body.
pub fn tree_references_abstract_binder(e: &ExprTree) -> bool {
    let mut found = false;
    for_each_path(e, &mut |p| {
        if matches!(p.binding, BindingKind::AbstractBinder) {
            found = true;
        }
    });
    found
}

/// Immediate conjuncts of the top node: `And(l, r)` → `[l, r]`, anything
/// else → `[e]`. Deliberately NON-recursive — the legacy string splitter
/// only broke the *top-level* `&&` (nested conjunctions sit inside the
/// bool-op's own parens), and per-term guard emission must match it.
pub fn top_conjuncts(e: &ExprTree) -> Vec<&ExprTree> {
    match e {
        ExprTree::BoolOp {
            op: TreeBoolOp::And,
            lhs,
            rhs,
        } => vec![lhs, rhs],
        _ => vec![e],
    }
}

/// Does a `Checked`-mode render of `e` contain fallible (`?`) operations?
/// True iff the tree carries any bare `Arith` or `MulDiv*` node — checked
/// mode lowers every one of those to `checked_*`+`?` / `.try_into().ok()?`.
/// Structural replacement for the `rendered.contains('?')` heuristic.
pub fn contains_fallible_arith(e: &ExprTree) -> bool {
    match e {
        ExprTree::Arith { .. } | ExprTree::MulDivFloor { .. } | ExprTree::MulDivCeil { .. } => true,
        ExprTree::Int(_) | ExprTree::Bool(_) | ExprTree::Path(_) => false,
        ExprTree::Old(inner) | ExprTree::Not(inner) => contains_fallible_arith(inner),
        ExprTree::Sum { body, .. } | ExprTree::Quant { body, .. } => contains_fallible_arith(body),
        ExprTree::QuantIn { coll, body, .. } => {
            contains_fallible_arith(coll) || contains_fallible_arith(body)
        }
        ExprTree::BoolOp { lhs, rhs, .. } | ExprTree::Cmp { lhs, rhs, .. } => {
            contains_fallible_arith(lhs) || contains_fallible_arith(rhs)
        }
        ExprTree::Contains { coll, elem } => {
            contains_fallible_arith(coll) || contains_fallible_arith(elem)
        }
        ExprTree::Len(coll) => contains_fallible_arith(coll),
        ExprTree::Match {
            scrutinee, arms, ..
        } => {
            contains_fallible_arith(scrutinee)
                || arms.iter().any(|a| contains_fallible_arith(&a.body))
        }
        ExprTree::Ctor { payload, .. } => {
            payload.as_ref().is_some_and(|p| contains_fallible_arith(p))
        }
        ExprTree::RecordLit(fields) => fields.iter().any(|(_, v)| contains_fallible_arith(v)),
        ExprTree::RecordUpdate { base, updates } => {
            contains_fallible_arith(base) || updates.iter().any(|(_, v)| contains_fallible_arith(v))
        }
        ExprTree::IsVariant { scrutinee, .. } => contains_fallible_arith(scrutinee),
        ExprTree::App { args, .. } => args.iter().any(contains_fallible_arith),
        ExprTree::Field { base, .. } => contains_fallible_arith(base),
        ExprTree::Let { value, body, .. } => {
            contains_fallible_arith(value) || contains_fallible_arith(body)
        }
        ExprTree::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            contains_fallible_arith(cond)
                || contains_fallible_arith(then_branch)
                || contains_fallible_arith(else_branch)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Corpus parity: for every expression the adapter carried a tree for,
    // the tree renderer must agree with the legacy pre-rendered string —
    // exactly, or up to redundant parens (checked structurally via syn;
    // source parens don't survive in the tree, and the renderer re-derives
    // minimal precedence-correct grouping).
    // ------------------------------------------------------------------

    /// Strip redundant paren/group nodes so paren-placement differences
    /// don't count as divergence.
    struct StripParens;
    impl syn::fold::Fold for StripParens {
        fn fold_expr(&mut self, e: syn::Expr) -> syn::Expr {
            match e {
                syn::Expr::Paren(p) => self.fold_expr(*p.expr),
                syn::Expr::Group(g) => self.fold_expr(*g.expr),
                other => syn::fold::fold_expr(self, other),
            }
        }
    }

    fn normalized(src: &str) -> Option<syn::Expr> {
        let parsed: syn::Expr = syn::parse_str(src).ok()?;
        Some(syn::fold::fold_expr(&mut StripParens, parsed))
    }

    /// Equal exactly, or equal after paren normalization.
    fn equivalent(ours: &str, legacy: &str) -> bool {
        if ours == legacy {
            return true;
        }
        // Unsupported-quantifier sentinels aren't expressions; require the
        // same sentinel on both sides.
        let sentinel = crate::check::QEDGEN_UNSUPPORTED_MARKER;
        if ours.contains(sentinel) || legacy.contains(sentinel) {
            return ours.contains(sentinel) && legacy.contains(sentinel);
        }
        match (normalized(ours), normalized(legacy)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn check(
        mismatches: &mut Vec<String>,
        where_: &str,
        tree: &ExprTree,
        cx: RustCx,
        legacy: &str,
    ) {
        if legacy.is_empty() {
            return;
        }
        let ours = render_rust(tree, cx);
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
    fn corpus_parity_with_legacy_strings() {
        let cx_native = RustCx::native();
        let cx_pod = RustCx::pod();
        let cx_math = RustCx::native().with_arith(ArithMode::Widened);
        let cx_binary = RustCx::native().with_binder(Binder::PrePost);
        let cx_binary_math = cx_binary.with_arith(ArithMode::Widened);
        let cx_checked = RustCx::native().with_arith(ArithMode::Checked);

        let mut mismatches = Vec::new();
        for fixture in &[
            "examples/rust/escrow/escrow.qedspec",
            "examples/rust/escrow-split",
            "examples/rust/lending/lending.qedspec",
            "examples/rust/multisig/multisig.qedspec",
            "examples/rust/bundled-stdlib-demo/pool.qedspec",
            "examples/rust/cross-program-vault",
        ] {
            let spec = parse_fixture(fixture);
            for h in &spec.handlers {
                for (i, r) in h.requires.iter().enumerate() {
                    let Some(t) = &r.tree else { continue };
                    let at = format!("{fixture} {} requires[{i}]", h.name);
                    check(&mut mismatches, &at, t, cx_native, &r.rust_expr);
                    check(&mut mismatches, &at, t, cx_pod, &r.rust_expr_pod);
                    check(&mut mismatches, &at, t, cx_math, &r.rust_expr_math);
                }
                for (i, e) in h.ensures.iter().enumerate() {
                    let Some(t) = &e.tree else { continue };
                    let at = format!("{fixture} {} ensures[{i}]", h.name);
                    check(&mut mismatches, &at, t, cx_native, &e.rust_expr);
                    check(&mut mismatches, &at, t, cx_pod, &e.rust_expr_pod);
                    check(&mut mismatches, &at, t, cx_binary, &e.rust_expr_binary);
                    check(
                        &mut mismatches,
                        &at,
                        t,
                        cx_binary_math,
                        &e.rust_expr_binary_math,
                    );
                }
                for (i, eff) in h.effects.iter().enumerate() {
                    let Some(t) = &eff.tree else { continue };
                    // Simple shapes (bare path / literal) keep the legacy
                    // state-stripped string for `resolve_value`; only
                    // compound shapes were adapter-rendered (issues
                    // #143/#144) and are comparable here.
                    if matches!(t, ExprTree::Path(_) | ExprTree::Int(_)) {
                        continue;
                    }
                    let at = format!("{fixture} {} effect[{i}]", h.name);
                    check(&mut mismatches, &at, t, cx_checked, &eff.value_rust);
                }
            }
            for p in &spec.properties {
                let Some(t) = &p.tree else { continue };
                let at = format!("{fixture} property {}", p.name);
                let (cx_p, cx_p_math) = match p.class {
                    crate::check::PropertyClass::Unary => (cx_native, cx_math),
                    crate::check::PropertyClass::Binary => (cx_binary, cx_binary_math),
                };
                if let Some(rust) = &p.rust_expression {
                    check(&mut mismatches, &at, t, cx_p, rust);
                }
                if let Some(pod) = &p.rust_expression_pod {
                    let cx_p_pod = RustCx { pod: true, ..cx_p };
                    check(&mut mismatches, &at, t, cx_p_pod, pod);
                }
                if let Some(math) = &p.rust_expression_math {
                    check(&mut mismatches, &at, t, cx_p_math, math);
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} renderer/legacy divergences:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    // ------------------------------------------------------------------
    // Targeted mode tests over hand-built trees
    // ------------------------------------------------------------------

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
        assert_eq!(render_rust(&read, RustCx::native()), "s.balance");
        assert_eq!(
            render_rust(&read, RustCx::native().with_binder(Binder::PrePost)),
            "post.balance"
        );
        let old = ExprTree::Old(Box::new(read.clone()));
        assert_eq!(render_rust(&old, RustCx::native()), "s.balance");
        assert_eq!(
            render_rust(&old, RustCx::native().with_binder(Binder::PrePost)),
            "pre.balance"
        );
        assert_eq!(
            render_rust(
                &read,
                RustCx::native().with_binder(Binder::SelfAcct("escrow"))
            ),
            "escrow.balance"
        );
    }

    #[test]
    fn pod_mode_adds_get_on_wide_state_fields() {
        let wide = state_field("balance", Ty::U64);
        let narrow = state_field("flag", Ty::U8);
        assert_eq!(render_rust(&wide, RustCx::pod()), "s.balance.get()");
        assert_eq!(render_rust(&narrow, RustCx::pod()), "s.flag");
        // Params are never pod-wrapped (dispatch shim unwraps them).
        assert_eq!(render_rust(&param("x", Ty::U64), RustCx::pod()), "x");
    }

    #[test]
    fn checked_mode_lowers_arith_to_checked_calls() {
        let e = ExprTree::Arith {
            op: TreeArithOp::Sub,
            lhs: Box::new(state_field("balance", Ty::U64)),
            rhs: Box::new(param("amount", Ty::U64)),
        };
        assert_eq!(
            render_rust(&e, RustCx::native().with_arith(ArithMode::Checked)),
            "(s.balance).checked_sub(amount)?"
        );
    }

    #[test]
    fn wrapping_mode_chains_top_level_adds() {
        // The proptest guard composite: `a + b + c <= cap` chains
        // wrapping_add left-associatively over widened operands; nested
        // arithmetic under the saturating_sub method call stays plain.
        let sum = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Add,
                lhs: Box::new(param("a", Ty::U64)),
                rhs: Box::new(param("b", Ty::U64)),
            }),
            rhs: Box::new(param("c", Ty::U64)),
        };
        let cmp = ExprTree::Cmp {
            op: TreeCmpOp::Le,
            lhs: Box::new(sum),
            rhs: Box::new(param("cap", Ty::U64)),
        };
        assert_eq!(
            render_rust(&cmp, RustCx::native().with_arith(ArithMode::Wrapping)),
            "((a) as u128).wrapping_add(((b) as u128)).wrapping_add(((c) as u128)) <= ((cap) as u128)"
        );
        // u128 subtraction saturates via the Widened form; the `+` under
        // it stays plain (legacy wrap never descended into method args).
        let sub = ExprTree::Cmp {
            op: TreeCmpOp::Ge,
            lhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Sub,
                lhs: Box::new(ExprTree::Arith {
                    op: TreeArithOp::Add,
                    lhs: Box::new(param("a", Ty::U64)),
                    rhs: Box::new(param("b", Ty::U64)),
                }),
                rhs: Box::new(param("c", Ty::U64)),
            }),
            rhs: Box::new(ExprTree::Int(0)),
        };
        assert_eq!(
            render_rust(&sub, RustCx::native().with_arith(ArithMode::Wrapping)),
            "(((a) as u128) + ((b) as u128)).saturating_sub(((c) as u128)) >= ((0) as u128)"
        );
    }

    #[test]
    fn widened_mode_saturates_comparison_arithmetic() {
        // state.balance + amount <= U64_MAX-style predicate: arithmetic
        // spine widens to u128; `-` saturates.
        let sum = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(state_field("balance", Ty::U64)),
            rhs: Box::new(param("amount", Ty::U64)),
        };
        let cmp = ExprTree::Cmp {
            op: TreeCmpOp::Le,
            lhs: Box::new(sum),
            rhs: Box::new(ExprTree::Int(1000)),
        };
        assert_eq!(
            render_rust(&cmp, RustCx::native().with_arith(ArithMode::Widened)),
            "((s.balance) as u128) + ((amount) as u128) <= ((1000) as u128)"
        );
    }

    #[test]
    fn nat_int_mix_casts_both_sides() {
        let e = ExprTree::Cmp {
            op: TreeCmpOp::Ge,
            lhs: Box::new(state_field("pnl", Ty::I128)),
            rhs: Box::new(param("amount", Ty::U64)),
        };
        assert_eq!(
            render_rust(&e, RustCx::native()),
            "((s.pnl) as i128) >= ((amount) as i128)"
        );
    }

    #[test]
    fn tree_structure_drives_paren_placement() {
        // Add(a, Sub(b, c)) must NOT flatten — evaluation order changes
        // overflow behavior on unsigned types.
        let e = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(param("a", Ty::U64)),
            rhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Sub,
                lhs: Box::new(param("b", Ty::U64)),
                rhs: Box::new(param("c", Ty::U64)),
            }),
        };
        assert_eq!(render_rust(&e, RustCx::native()), "a + (b - c)");
        // Left-assoc chains need no parens.
        let chain = ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Add,
                lhs: Box::new(param("a", Ty::U64)),
                rhs: Box::new(param("b", Ty::U64)),
            }),
            rhs: Box::new(param("c", Ty::U64)),
        };
        assert_eq!(render_rust(&chain, RustCx::native()), "a + b + c");
        // Mul over Add operand parenthesizes.
        let scaled = ExprTree::Arith {
            op: TreeArithOp::Mul,
            lhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Add,
                lhs: Box::new(param("a", Ty::U64)),
                rhs: Box::new(param("b", Ty::U64)),
            }),
            rhs: Box::new(param("c", Ty::U64)),
        };
        assert_eq!(render_rust(&scaled, RustCx::native()), "(a + b) * c");
    }

    #[test]
    fn bps_div_shape_is_exempt_from_widening() {
        // (a * b) / 10000 keeps its native form so the Kani bps rewrite
        // still recognizes it.
        let bps = ExprTree::Arith {
            op: TreeArithOp::Div,
            lhs: Box::new(ExprTree::Arith {
                op: TreeArithOp::Mul,
                lhs: Box::new(param("a", Ty::U64)),
                rhs: Box::new(param("b", Ty::U64)),
            }),
            rhs: Box::new(ExprTree::Int(10000)),
        };
        let cmp = ExprTree::Cmp {
            op: TreeCmpOp::Le,
            lhs: Box::new(bps),
            rhs: Box::new(param("cap", Ty::U64)),
        };
        let out = render_rust(&cmp, RustCx::native().with_arith(ArithMode::Widened));
        assert!(
            !out.contains("u128"),
            "bps shape must not widen; got: {out}"
        );
    }

    #[test]
    fn const_binding_substitutes_value() {
        let c = ExprTree::Path(TreePath {
            root: "LIMIT".into(),
            binding: BindingKind::Const("100".into()),
            segments: vec![],
            ty: None,
        });
        assert_eq!(render_rust(&c, RustCx::native()), "100");
    }

    #[test]
    fn checked_mul_div_narrows_fallibly() {
        let e = ExprTree::MulDivFloor {
            a: Box::new(param("total", Ty::U64)),
            b: Box::new(param("bps", Ty::U64)),
            d: Box::new(ExprTree::Int(10000)),
        };
        assert_eq!(
            render_rust(&e, RustCx::native().with_arith(ArithMode::Checked)),
            "(mul_div_floor_u128(((total) as u128), ((bps) as u128), ((10000) as u128))).try_into().ok()?"
        );
    }

    // ------------------------------------------------------------------
    // #151 Slice 3 — scaffold-guard account lowering + path predicates
    // ------------------------------------------------------------------

    fn acct_pubkey(name: &str) -> ExprTree {
        ExprTree::Path(TreePath {
            root: name.into(),
            binding: BindingKind::Account,
            segments: vec![TreeSeg::Field("pubkey".into())],
            ty: None,
        })
    }

    fn acct_bare(name: &str) -> ExprTree {
        ExprTree::Path(TreePath {
            root: name.into(),
            binding: BindingKind::Account,
            segments: vec![],
            ty: None,
        })
    }

    #[test]
    fn acct_key_style_lowers_pubkey_and_bare_account_reads() {
        // `<acct>.pubkey` and bare `<acct>` both mean the account's
        // runtime address in guard positions; the rendered forms must
        // match `FrameworkSurface::account_key_expr` byte-for-byte.
        let anchor = RustCx::native().with_acct_key(Some(AcctKeyStyle::AnchorCtx));
        let quasar = RustCx::native().with_acct_key(Some(AcctKeyStyle::QuasarCtx));
        assert_eq!(
            render_rust(&acct_pubkey("buyer"), anchor),
            "ctx.buyer.key()"
        );
        assert_eq!(render_rust(&acct_bare("buyer"), anchor), "ctx.buyer.key()");
        assert_eq!(
            render_rust(&acct_pubkey("buyer"), quasar),
            "(*ctx.buyer.to_account_view().address())"
        );
        // Combined with the SelfAcct state receiver — the scaffold-guards
        // composite (`requires buyer.pubkey == state.authority`).
        let cmp = ExprTree::Cmp {
            op: TreeCmpOp::Eq,
            lhs: Box::new(acct_pubkey("buyer")),
            rhs: Box::new(state_field("authority", Ty::Pubkey)),
        };
        let cx = anchor.with_binder(Binder::SelfAcct("ctx.escrow"));
        assert_eq!(
            render_rust(&cmp, cx),
            "ctx.buyer.key() == ctx.escrow.authority"
        );
        // Without the style, account reads keep the plain path form
        // (harness positions).
        assert_eq!(
            render_rust(&acct_pubkey("buyer"), RustCx::native()),
            "buyer.pubkey"
        );
    }

    #[test]
    fn account_reads_are_key_style_gates_field_projections() {
        assert!(account_reads_are_key_style(&acct_pubkey("buyer")));
        assert!(account_reads_are_key_style(&acct_bare("buyer")));
        assert!(account_reads_are_key_style(&state_field("pool", Ty::U64)));
        // Imported-account field read (`config.fee`) needs the legacy
        // mirror rewrite — the gate must reject it.
        let field_read = ExprTree::Path(TreePath {
            root: "config".into(),
            binding: BindingKind::Account,
            segments: vec![TreeSeg::Field("fee".into())],
            ty: None,
        });
        assert!(!account_reads_are_key_style(&field_read));
        let nested = ExprTree::Cmp {
            op: TreeCmpOp::Gt,
            lhs: Box::new(field_read),
            rhs: Box::new(ExprTree::Int(0)),
        };
        assert!(!account_reads_are_key_style(&nested));
    }

    #[test]
    fn abstract_binder_detection_is_structural() {
        let binder_read = ExprTree::Path(TreePath {
            root: "lp_out".into(),
            binding: BindingKind::AbstractBinder,
            segments: vec![],
            ty: None,
        });
        let cmp = ExprTree::Cmp {
            op: TreeCmpOp::Gt,
            lhs: Box::new(binder_read),
            rhs: Box::new(ExprTree::Int(0)),
        };
        assert!(tree_references_abstract_binder(&cmp));
        // A state field that merely shares the binder's name must NOT
        // defer — the legacy substring scan couldn't tell these apart.
        let same_name_field = ExprTree::Cmp {
            op: TreeCmpOp::Gt,
            lhs: Box::new(state_field("lp_out", Ty::U64)),
            rhs: Box::new(ExprTree::Int(0)),
        };
        assert!(!tree_references_abstract_binder(&same_name_field));
    }
}
