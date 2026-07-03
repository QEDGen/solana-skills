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

// Renderer lands ahead of its consumers (same ratified pattern as
// `mir::mod`): the Kani/proptest emission port is the second half of
// Slice 1 and removes this allow. The corpus parity test below is the
// current consumer keeping the renderer honest.
#![allow(dead_code)]

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
    /// Wrapping operators (`+=?` scaffold lowering; Slice 3 consumer).
    Wrapping,
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
}

impl<'a> RustCx<'a> {
    pub fn native() -> Self {
        RustCx {
            binder: Binder::S,
            arith: ArithMode::Native,
            pod: false,
            acct_env: None,
        }
    }
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
        ExprTree::Match { scrutinee, arms } => {
            let sc = render(scrutinee, cx, inside_old).0;
            let mut out = format!("match {} {{", sc);
            for arm in arms {
                out.push_str(&format!("\n    {}::{}", "/* ty */", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push_str(&format!("({})", b));
                }
                out.push_str(" => ");
                out.push_str(&render(&arm.body, cx, inside_old).0);
                out.push(',');
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
        ExprTree::IsVariant { scrutinee, variant } => {
            let sc = render(scrutinee, cx, inside_old).0;
            (
                format!("matches!({}, {}::{}(..))", sc, "/* ty */", variant),
                Prec::Atom,
            )
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
    match &p.binding {
        BindingKind::StateField | BindingKind::Ghost => {
            let prefix = match (cx.binder, inside_old) {
                (Binder::S, _) => "s",
                (Binder::SelfAcct(name), _) => name,
                (Binder::PrePost, true) => "pre",
                (Binder::PrePost, false) => "post",
            };
            out.push_str(prefix);
        }
        BindingKind::Account => {
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
    for seg in &p.segments {
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
        Some(Ty::U8 | Ty::Pubkey | Ty::Map { .. }) => false,
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
        ExprTree::MulDivFloor { .. } | ExprTree::MulDivCeil { .. } => NumKind::Nat,
        ExprTree::Old(inner) => rust_num_kind(inner),
        ExprTree::Int(_)
        | ExprTree::Bool(_)
        | ExprTree::Path(_)
        | ExprTree::Sum { .. }
        | ExprTree::Quant { .. }
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
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
        | ExprTree::BoolOp { .. }
        | ExprTree::Not(_)
        | ExprTree::Cmp { .. }
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
    // Math-exact predicate mode: a comparison whose spine carries bare
    // arithmetic evaluates both sides widened so the predicate can't
    // overflow-panic (issue #146). Non-numeric and arithmetic-free
    // comparisons keep the native rendering.
    if cx.arith == ArithMode::Widened && (spine_has_arith(lhs) || spine_has_arith(rhs)) {
        let lk = rust_num_kind(lhs);
        let rk = rust_num_kind(rhs);
        if matches!(lk, NumKind::Nat | NumKind::Int) && matches!(rk, NumKind::Nat | NumKind::Int) {
            let wide = if lk == NumKind::Int || rk == NumKind::Int {
                "i128"
            } else {
                "u128"
            };
            let l = render_widened_term(lhs, cx, inside_old, wide);
            let r = render_widened_term(rhs, cx, inside_old, wide);
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
    if cx.arith == ArithMode::Wrapping {
        let method = match op {
            TreeArithOp::Add => "wrapping_add",
            TreeArithOp::Sub => "wrapping_sub",
            TreeArithOp::Mul => "wrapping_mul",
            TreeArithOp::Div => "wrapping_div",
            TreeArithOp::Mod => "wrapping_rem",
        };
        let (l, r) = render_pair_with_coercion(lhs, rhs, cx, inside_old, Prec::Or);
        return (format!("({}).{}({})", l, method, r), Prec::Atom);
    }
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
        ExprTree::BoolOp { lhs, rhs, .. } | ExprTree::Cmp { lhs, rhs, .. } => {
            contains_fallible_arith(lhs) || contains_fallible_arith(rhs)
        }
        ExprTree::Match { scrutinee, arms } => {
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
            "examples/rust/percolator/percolator.qedspec",
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
                for (i, t) in h.effects_tree.iter().enumerate() {
                    let Some(t) = t else { continue };
                    // Simple shapes (bare path / literal) keep the legacy
                    // state-stripped string for `resolve_value`; only
                    // compound shapes were adapter-rendered (issues
                    // #143/#144) and are comparable here.
                    if matches!(t, ExprTree::Path(_) | ExprTree::Int(_)) {
                        continue;
                    }
                    let Some(legacy) = h.effects_rust.get(i) else {
                        continue;
                    };
                    let at = format!("{fixture} {} effect[{i}]", h.name);
                    check(&mut mismatches, &at, t, cx_checked, legacy);
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
        let wrapped = render_rust(&e, RustCx::native().with_arith(ArithMode::Wrapping));
        assert_eq!(wrapped, "(s.balance).wrapping_sub(amount)");
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
}
