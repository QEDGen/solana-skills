//! Rust expression rendering: typed AST → ASCII-operator Rust strings,
//! including Pod-aware lowering for the Quasar target.

use super::*;

/// Per-render options for `expr_to_rust`. `pod_aware` is set for the Quasar
/// target, where state/record integer fields lower to Pod companions and
/// need `.get()` on access. `state_mode` selects unary vs binary state-path
/// lowering ([`StateMode`]); `inside_old` tracks descent into an `old(...)`
/// subexpression so nested state refs render against pre-state.
#[derive(Copy, Clone)]
pub(super) struct RustOpts<'a, 'env> {
    pod_aware: bool,
    env: &'a TypeEnv<'env>,
    state_mode: StateMode,
    inside_old: bool,
}

impl<'a, 'env> RustOpts<'a, 'env> {
    /// Return a copy with `inside_old = true`. Used when descending into
    /// `Expr::Old(_)` so nested state-path renders see the pre-state
    /// prefix.
    fn with_inside_old(self) -> Self {
        RustOpts {
            inside_old: true,
            ..self
        }
    }

    /// Copy with the given `state_mode` (Binary when rendering a
    /// `PropertyClass::Binary` property body).
    pub(super) fn with_state_mode(self, state_mode: StateMode) -> Self {
        RustOpts { state_mode, ..self }
    }
}

/// `RustOpts` matching the legacy non-Pod-aware behavior. Used for the
/// `rust_expr` field that codegen consumes when emitting for Anchor (or
/// for any consumer that expects native Rust integer types).
pub(super) fn opts_native<'a, 'env>(env: &'a TypeEnv<'env>) -> RustOpts<'a, 'env> {
    RustOpts {
        pod_aware: false,
        env,
        state_mode: StateMode::Unary,
        inside_old: false,
    }
}

/// `RustOpts` for the Pod-aware companion field (`rust_expr_pod`). Used
/// when codegen is emitting for Quasar.
pub(super) fn opts_pod<'a, 'env>(env: &'a TypeEnv<'env>) -> RustOpts<'a, 'env> {
    RustOpts {
        pod_aware: true,
        env,
        state_mode: StateMode::Unary,
        inside_old: false,
    }
}

/// Render typed expression to a Rust-compatible string (ASCII operators).
pub(super) fn expr_to_rust(
    e: &Expr,
    ctx: Ctx,
    consts: ConstTable,
    opts: RustOpts<'_, '_>,
) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Path(p) => render_path_with_pod(p, ctx, consts, opts),
        // `old(...)` routes through `opts.inside_old`: the path renderer
        // emits `pre.x` (Binary mode) instead of `post.x`; non-Path inner
        // exprs render recursively with the flag set (a comment-form
        // lowering would be invalid Rust in expression position).
        Expr::Old(inner) => expr_to_rust(&inner.node, ctx, consts, opts.with_inside_old()),
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => format!(
            "sum_over::<{}>(|{}| {})",
            binder_ty,
            binder,
            expr_to_rust(&body.node, ctx, consts, opts)
        ),
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body,
        } => {
            // A quantifier over a bounded domain lowers to an exhaustive
            // `RangeInclusive::all` (forall) / `any` (exists) — correct and
            // cheap for test suites. `Fin[N]` index domains iterate `0..N`;
            // small integers (U8/I8) exhaust their full range. Wider integer
            // domains can't be exhausted in a test loop, so the sentinel
            // tells the caller to skip or escalate to harness-level lowering.
            let method = match kind {
                a::Quantifier::Forall => "all",
                a::Quantifier::Exists => "any",
            };
            let body_rust = expr_to_rust(&body.node, ctx, consts, opts);
            // `exists` over a bounded index domain (`Fin[N]`, directly or via
            // an alias): iterate `0..N` with `.any(…)` — a real, non-vacuous
            // predicate usable wherever a bool is expected. `forall` over
            // `Fin[N]` deliberately does NOT take this path: it keeps the
            // per-slot lowering (`{prop}_at`) so a preserved-property
            // assertion checks the one modified slot rather than unwinding a
            // whole-array loop in Kani. Existence has no per-slot analogue.
            if matches!(kind, a::Quantifier::Exists) {
                if let Some(bound) = opts.env.fin_bound(binder_ty) {
                    return format!("(0..({} as usize)).any(|{}| {})", bound, binder, body_rust);
                }
            }
            // Small integer domains (U8, I8) can be exhausted directly (256
            // iterations max).
            let rust_ty = match binder_ty.as_str() {
                "U8" => Some("u8"),
                "I8" => Some("i8"),
                _ => None,
            };
            let Some(rust_ty) = rust_ty else {
                let kind_name = match kind {
                    a::Quantifier::Forall => "forall",
                    a::Quantifier::Exists => "exists",
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
        Expr::BoolOp { op, lhs, rhs } => {
            let lhs_r = expr_to_rust(&lhs.node, ctx, consts, opts);
            let rhs_r = expr_to_rust(&rhs.node, ctx, consts, opts);
            match op {
                a::BoolOp::And => format!("({}) && ({})", lhs_r, rhs_r),
                a::BoolOp::Or => format!("({}) || ({})", lhs_r, rhs_r),
                // `a implies b` ≡ `!a || b`; parenthesize both sides to survive
                // surrounding precedence (matters once callers compose via `&&`/`||`).
                a::BoolOp::Implies => format!("(!({})) || ({})", lhs_r, rhs_r),
            }
        }
        Expr::Not(inner) => format!("!({})", expr_to_rust(&inner.node, ctx, consts, opts)),
        Expr::Cmp { op, lhs, rhs } => {
            let sym = match op {
                a::CmpOp::Eq => "==",
                a::CmpOp::Ne => "!=",
                a::CmpOp::Le => "<=",
                a::CmpOp::Ge => ">=",
                a::CmpOp::Lt => "<",
                a::CmpOp::Gt => ">",
            };
            let (l_str, r_str) = render_rust_binary_with_coercion(lhs, rhs, ctx, consts, opts);
            format!("{} {} {}", l_str, sym, r_str)
        }
        Expr::Arith { op, lhs, rhs } => {
            let sym = match op {
                a::ArithOp::Add => " + ",
                a::ArithOp::Sub => " - ",
                a::ArithOp::Mul => " * ",
                a::ArithOp::Div => " / ",
                a::ArithOp::Mod => " % ",
            };
            let (l_str, r_str) = render_rust_binary_with_coercion(lhs, rhs, ctx, consts, opts);
            format!("{}{}{}", l_str, sym, r_str)
        }
        Expr::Paren(inner) => format!("({})", expr_to_rust(&inner.node, ctx, consts, opts)),
        // mul_div_{floor,ceil}_u128 are u128-typed helpers (the intermediate
        // `a * b` can overflow u64 even when both operands are u64-bounded).
        // Inside arbitrary expression contexts (`requires` / `ensures` /
        // `effect` RHS) the u128 width is intentional — the spec author
        // may compare against a u128 literal (e.g. percolator's `…
        // mul_div_floor(...) <= 100000000000000000000`). The let-binding
        // emit site (see `HandlerClause::Let` handler below) narrows back
        // to U64 explicitly when the spec writes `let X = mul_div_*(…)`,
        // because the binding's spec-declared type is U64 and downstream
        // U64 uses (e.g. `total - X`) need to typecheck.
        Expr::MulDivFloor { a, b, d } => format!(
            "mul_div_floor_u128({}, {}, {})",
            render_helper_arg(&a.node, ctx, consts, opts),
            render_helper_arg(&b.node, ctx, consts, opts),
            render_helper_arg(&d.node, ctx, consts, opts)
        ),
        Expr::MulDivCeil { a, b, d } => format!(
            "mul_div_ceil_u128({}, {}, {})",
            render_helper_arg(&a.node, ctx, consts, opts),
            render_helper_arg(&b.node, ctx, consts, opts),
            render_helper_arg(&d.node, ctx, consts, opts)
        ),
        Expr::Match { scrutinee, arms } => {
            let sc = expr_to_rust(&scrutinee.node, ctx, consts, opts);
            let mut out = format!("match {} {{", sc);
            for arm in arms {
                out.push_str(&format!("\n    {}::{}", "/* ty */", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push_str(&format!("({})", b));
                }
                out.push_str(" => ");
                out.push_str(&expr_to_rust(&arm.body.node, ctx, consts, opts));
                out.push(',');
            }
            out.push_str("\n}");
            out
        }
        Expr::Ctor { variant, payload } => match payload {
            None => format!("{}::{}", "/* ty */", variant),
            Some(p) => format!(
                "{}::{}({})",
                "/* ty */",
                variant,
                expr_to_rust(&p.node, ctx, consts, opts)
            ),
        },
        Expr::RecordLit(fields) => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{}: {}", n, expr_to_rust(&v.node, ctx, consts, opts)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {} }}", "/* ty */", body)
        }
        Expr::RecordUpdate { base, updates } => {
            let base_str = expr_to_rust(&base.node, ctx, consts, opts);
            let body = updates
                .iter()
                .map(|(n, v)| format!("{}: {}", n, expr_to_rust(&v.node, ctx, consts, opts)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {}, ..{} }}", "/* ty */", body, base_str)
        }
        Expr::IsVariant { scrutinee, variant } => {
            let sc = expr_to_rust(&scrutinee.node, ctx, consts, opts);
            format!("matches!({}, {}::{}(..))", sc, "/* ty */", variant)
        }
        Expr::App { func, args } => {
            // `now()` lowers to the on-chain clock read. `unwrap()` rather
            // than `?` so the expression is valid in assertion / property
            // bodies (the surrounding fn may not return Result); Clock is a
            // sysvar that always succeeds in practice. The i64→u64 cast is
            // sign-bit-preserving; negative unix_timestamp doesn't happen
            // on chain.
            if func == "now" && args.is_empty() {
                return "(solana_program::clock::Clock::get().unwrap().unix_timestamp as u64)"
                    .to_string();
            }
            // `current_epoch()` reads `.epoch` (already u64) — no cast.
            if func == "current_epoch" && args.is_empty() {
                return "solana_program::clock::Clock::get().unwrap().epoch".to_string();
            }
            let args_str: Vec<String> = args
                .iter()
                .map(|n| expr_to_rust(&n.node, ctx, consts, opts))
                .collect();
            format!("{}({})", func, args_str.join(", "))
        }
        Expr::Field { base, field } => {
            let base_str = expr_to_rust(&base.node, ctx, consts, opts);
            format!("{}.{}", base_str, field)
        }
        Expr::Let { name, value, body } => {
            // Rust lowers a let-in expression to a block. Parentheses are
            // safe around the block for embedding in larger expressions.
            format!(
                "({{ let {} = {}; {} }})",
                name,
                expr_to_rust(&value.node, ctx, consts, opts),
                expr_to_rust(&body.node, ctx, consts, opts)
            )
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => format!(
            "(if {} {{ {} }} else {{ {} }})",
            expr_to_rust(&cond.node, ctx, consts, opts),
            expr_to_rust(&then_branch.node, ctx, consts, opts),
            expr_to_rust(&else_branch.node, ctx, consts, opts),
        ),
    }
}

/// Render a Path, applying a `.get()` postfix when it resolves to a
/// Pod-flavored field on Quasar (`pod_aware`). Non-Pod fields (`u8`/`i8`/
/// `Bool` already alignment 1, paths into non-state types) pass through.
fn render_path_with_pod(
    p: &a::Path,
    ctx: Ctx,
    consts: ConstTable,
    opts: RustOpts<'_, '_>,
) -> String {
    let base = path_to_rust(p, ctx, consts, opts);
    if opts.pod_aware && opts.env.path_is_pod_field(p) {
        format!("{}.get()", base)
    } else {
        base
    }
}

/// Rust-flavor kind inference: mostly the same as `TypeEnv::infer` but
/// `MulDivFloor` / `MulDivCeil` always report `Nat` because the codegen
/// lowers them to `mul_div_floor_u128` / `_ceil_u128` helpers that
/// return `u128`. Without this override the Lean-style inheritance
/// (`Int` if any operand is `Int`) bleeds the wrong type into Rust
/// comparisons against the helper's u128 result.
pub(super) fn rust_infer_kind(env: &TypeEnv, e: &Expr) -> Kind {
    match e {
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } => Kind::Nat,
        Expr::Paren(inner) => rust_infer_kind(env, &inner.node),
        Expr::Old(inner) => rust_infer_kind(env, &inner.node),
        _ => env.infer(e),
    }
}

/// `true` iff `e` is a `mul_div_floor` / `mul_div_ceil` call, possibly
/// wrapped in `Paren` and/or `Old`. Mirrors the peel pattern in
/// `rust_infer_kind` above so the let-binding narrow gate stays in
/// lock-step — `let X = (mul_div_floor(...))` and `let X =
/// old(mul_div_floor(...))` both want the same narrowing as the bare
/// form.
pub(super) fn is_mul_div_let_rhs(e: &Expr) -> bool {
    match e {
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } => true,
        Expr::Paren(inner) => is_mul_div_let_rhs(&inner.node),
        Expr::Old(inner) => is_mul_div_let_rhs(&inner.node),
        _ => false,
    }
}

/// Render both sides of a binary op, casting to `i128` when kinds mix.
/// Mirrors the Lean-side `render_binary_with_coercion`. The Nat→Int cast is
/// target-independent (Rust rejects `u128 + i128` everywhere) — do NOT gate
/// it on `pod_aware`, which is Quasar-only and would silently break Anchor
/// scaffolds mixing U128 + I128.
fn render_rust_binary_with_coercion(
    lhs: &Node<Expr>,
    rhs: &Node<Expr>,
    ctx: Ctx,
    consts: ConstTable,
    opts: RustOpts<'_, '_>,
) -> (String, String) {
    let lk = rust_infer_kind(opts.env, &lhs.node);
    let rk = rust_infer_kind(opts.env, &rhs.node);
    let l = expr_to_rust(&lhs.node, ctx, consts, opts);
    let r = expr_to_rust(&rhs.node, ctx, consts, opts);
    // Widening Nat → Int must cast BOTH sides to the same wide type —
    // casting only the Nat side leaves `i64 >= i128`, which doesn't
    // typecheck. Symmetric i128 widening loses no precision.
    match (lk, rk) {
        (Kind::Nat, Kind::Int) => (format!("(({}) as i128)", l), format!("(({}) as i128)", r)),
        (Kind::Int, Kind::Nat) => (format!("(({}) as i128)", l), format!("(({}) as i128)", r)),
        _ => (l, r),
    }
}

/// `mul_div_{floor,ceil}_u128` take `u128` args; spec operands may be
/// U64 / I64 / I128 / native params. Cast unconditionally on every target
/// (gating on `pod_aware` would break Anchor) — `as u128` from u64 widens;
/// from i128 it truncates, matching the Lean side's Int → u128 lowering.
fn render_helper_arg(e: &Expr, ctx: Ctx, consts: ConstTable, opts: RustOpts<'_, '_>) -> String {
    let rendered = expr_to_rust(e, ctx, consts, opts);
    format!("(({}) as u128)", rendered)
}

fn path_to_rust(p: &a::Path, _ctx: Ctx, consts: ConstTable, opts: RustOpts<'_, '_>) -> String {
    let mut out = String::new();
    if p.segments.is_empty() && p.root != "state" {
        // Bare ident — substitute if declared as a const (pest parity).
        if let Some(v) = consts.get(&p.root) {
            return v.clone();
        }
    }
    // `state.X` lowers to `s.X` — every Rust consumer (property fn bodies,
    // transition-fn assume predicates, abort.rust_expr) binds state to `s`.
    // In Binary state_mode the prefix splits by `inside_old`:
    //   inside_old=true  → `pre.<field>`   (old(state.x))
    //   inside_old=false → `post.<field>`  (state.x)
    // Mirrors `path_to_lean`. Unary callers keep `s.<field>` regardless.
    if p.root == "state" {
        let prefix = match (opts.state_mode, opts.inside_old) {
            (StateMode::Unary, _) => "s",
            (StateMode::Binary, true) => "pre",
            (StateMode::Binary, false) => "post",
        };
        out.push_str(prefix);
    } else {
        out.push_str(&p.root);
    }
    for seg in &p.segments {
        match seg {
            a::PathSeg::Field(f) => {
                out.push('.');
                out.push_str(f);
            }
            a::PathSeg::Index(i) => {
                // Cast index expression to `usize`. A Map[N] T lowers to
                // `[T; N]`; the spec's index could be a u8/u16/u32/Fin
                // handler param, none of which Rust accepts directly as
                // an array index. The `as usize` cast is always safe (no
                // negative values reach this path — Fin/U* are unsigned).
                out.push('[');
                out.push('(');
                out.push_str(i);
                out.push_str(") as usize");
                out.push(']');
            }
        }
    }
    out
}

// ============================================================================
// Type reference rendering (to the legacy type-string form)
// ============================================================================

/// True if `name` is used as the inner value type of any `Map[N] T` field
/// in any record or state ADT variant anywhere in `spec`. Sum types that
/// qualify get inductive Lean codegen; other ADTs stay on the flatten path.
pub(super) fn is_map_value_sum_type(name: &str, spec: &a::Spec) -> bool {
    // Check record and ADT variant fields for `Map[N] <name>` (value) OR
    // `Map[<name>] T` (enum used as key).
    fn type_ref_mentions(t: &a::TypeRef, name: &str) -> bool {
        match t {
            a::TypeRef::Map { inner, bound } => {
                let value_match = matches!(inner.as_ref(), a::TypeRef::Named(n) if n == name);
                // Key match: the bound is a raw ident string, resolved
                // later — a bare name match is the routing signal.
                let key_match = bound == name;
                value_match || key_match
            }
            _ => false,
        }
    }
    for Node { node, .. } in &spec.items {
        match node {
            TopItem::Record(r) => {
                for f in &r.fields {
                    if type_ref_mentions(&f.ty, name) {
                        return true;
                    }
                }
            }
            TopItem::Adt(adt) => {
                for v in &adt.variants {
                    for f in &v.fields {
                        if type_ref_mentions(&f.ty, name) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

pub(super) fn type_ref_to_string(t: &a::TypeRef) -> String {
    match t {
        a::TypeRef::Named(n) => n.clone(),
        a::TypeRef::Param(head, tail) => format!("{} {}", head, tail),
        a::TypeRef::Map { bound, inner } => {
            format!("Map[{}] {}", bound, type_ref_to_string(inner))
        }
        a::TypeRef::Fin { bound } => format!("Fin[{}]", bound),
    }
}
