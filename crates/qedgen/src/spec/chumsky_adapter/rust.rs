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
    /// Math-exact predicate rendering (issue #146): arithmetic inside the
    /// expression widens to u128/i128 (`-` on Nat kinds saturates), so a
    /// guard / property / ensures predicate can't overflow-panic when
    /// evaluated on unconstrained symbolic state. Matches the Lean `Nat`
    /// model. Off for scaffold-facing renders.
    widen_arith: bool,
    /// Checked value rendering for effect RHS (issue #146): `a - b` →
    /// `(a).checked_sub(b)?` etc. The emitted string uses `?`, so the
    /// consumer must evaluate it inside an `Option` context (the Kani /
    /// proptest transition emitters wrap it in an `(|| Some(…))()`
    /// closure and `return false` on `None`). Off everywhere else.
    checked_arith: bool,
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

    /// Copy with math-exact predicate widening on (see `widen_arith`).
    pub(super) fn with_widen_arith(self) -> Self {
        RustOpts {
            widen_arith: true,
            ..self
        }
    }

    /// Copy with checked effect-RHS arithmetic on (see `checked_arith`).
    pub(super) fn with_checked_arith(self) -> Self {
        RustOpts {
            checked_arith: true,
            ..self
        }
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
        widen_arith: false,
        checked_arith: false,
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
        widen_arith: false,
        checked_arith: false,
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
        } => match opts.env.fin_bound(binder_ty) {
            Some(bound) => format!(
                "(0..({bound} as usize)).map(|{binder}| {}).fold(0, |__sum, __item| __sum + __item)",
                expr_to_rust(&body.node, ctx, consts, opts)
            ),
            None => format!(
                "/* {}: {binder} : {binder_ty} requires a finite Fin[N] domain */",
                crate::check::QEDGEN_UNSUPPORTED_SUM_MARKER
            ),
        },
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
        Expr::QuantIn {
            kind,
            binder,
            coll,
            body,
        } => {
            // Bounded quantifier over a collection value: `coll.iter().any|all(
            // |x| body)`. `.iter()` yields `&Element`; field access / matches in
            // the body auto-deref, so no clone needed for the common cases.
            let method = match kind {
                a::Quantifier::Forall => "all",
                a::Quantifier::Exists => "any",
            };
            format!(
                "{}.iter().{}(|{}| {})",
                expr_to_rust(&coll.node, ctx, consts, opts),
                method,
                binder,
                expr_to_rust(&body.node, ctx, consts, opts)
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
            // Math-exact predicate mode: a comparison whose spine carries
            // bare arithmetic evaluates both sides widened (u128/i128), so
            // the predicate itself can't overflow-panic on unconstrained
            // symbolic state — the Lean `Nat` model computes these exactly
            // (issue #146). Non-numeric comparisons (Pubkey, Bool) and
            // arithmetic-free ones keep the native rendering byte-for-byte.
            if opts.widen_arith && (spine_has_arith(&lhs.node) || spine_has_arith(&rhs.node)) {
                let lk = rust_infer_kind(opts.env, &lhs.node);
                let rk = rust_infer_kind(opts.env, &rhs.node);
                if matches!(lk, Kind::Nat | Kind::Int) && matches!(rk, Kind::Nat | Kind::Int) {
                    let wide = if matches!(lk, Kind::Int) || matches!(rk, Kind::Int) {
                        "i128"
                    } else {
                        "u128"
                    };
                    let l = render_widened_term(&lhs.node, ctx, consts, opts, wide);
                    let r = render_widened_term(&rhs.node, ctx, consts, opts, wide);
                    return format!("{} {} {}", l, sym, r);
                }
            }
            let (l_str, r_str) = render_rust_binary_with_coercion(lhs, rhs, ctx, consts, opts);
            format!("{} {} {}", l_str, sym, r_str)
        }
        Expr::Arith { op, lhs, rhs } => {
            let (l_str, r_str) = render_rust_binary_with_coercion(lhs, rhs, ctx, consts, opts);
            // Checked effect-RHS mode (issue #146): bare arithmetic lowers
            // to `checked_*`+`?`, matching the DSL's checked-by-default
            // `+=`/`-=` doctrine — over/underflow makes the transition
            // return false instead of panicking. The consumer wraps the
            // whole RHS in an `Option` closure (see `RustOpts::checked_arith`).
            if opts.checked_arith {
                let method = match op {
                    a::ArithOp::Add => "checked_add",
                    a::ArithOp::Sub => "checked_sub",
                    a::ArithOp::Mul => "checked_mul",
                    a::ArithOp::Div => "checked_div",
                    a::ArithOp::Mod => "checked_rem",
                };
                return format!("({}).{}({})?", l_str, method, r_str);
            }
            let sym = match op {
                a::ArithOp::Add => " + ",
                a::ArithOp::Sub => " - ",
                a::ArithOp::Mul => " * ",
                a::ArithOp::Div => " / ",
                a::ArithOp::Mod => " % ",
            };
            format!("{}{}{}", l_str, sym, r_str)
        }
        Expr::Paren(inner) => format!("({})", expr_to_rust(&inner.node, ctx, consts, opts)),
        // mul_div_{floor,ceil}_u128 are u128-typed helpers (the intermediate
        // `a * b` can overflow u64 even when both operands are u64-bounded).
        // Inside arbitrary expression contexts (`requires` / `ensures` /
        // `effect` RHS) the u128 width is intentional — the spec author
        // may compare against a u128 literal (e.g. a perp risk engine's `…
        // mul_div_floor(...) <= 100000000000000000000`). The let-binding
        // emit site (see `HandlerClause::Let` handler below) narrows back
        // to U64 explicitly when the spec writes `let X = mul_div_*(…)`,
        // because the binding's spec-declared type is U64 and downstream
        // U64 uses (e.g. `total - X`) need to typecheck.
        Expr::MulDivFloor { a, b, d } => {
            let call = format!(
                "mul_div_floor_u128({}, {}, {})",
                render_helper_arg(&a.node, ctx, consts, opts),
                render_helper_arg(&b.node, ctx, consts, opts),
                render_helper_arg(&d.node, ctx, consts, opts)
            );
            // Checked effect-RHS mode: the helper is u128-typed but the
            // assignment target is the field's native width — narrow with
            // a fallible conversion so an out-of-range result rejects the
            // transition instead of truncating.
            if opts.checked_arith {
                format!("({}).try_into().ok()?", call)
            } else {
                call
            }
        }
        Expr::MulDivCeil { a, b, d } => {
            let call = format!(
                "mul_div_ceil_u128({}, {}, {})",
                render_helper_arg(&a.node, ctx, consts, opts),
                render_helper_arg(&b.node, ctx, consts, opts),
                render_helper_arg(&d.node, ctx, consts, opts)
            );
            if opts.checked_arith {
                format!("({}).try_into().ok()?", call)
            } else {
                call
            }
        }
        Expr::MulDivRoundHalfUp { a, b, d } => {
            let call = format!(
                "mul_div_round_half_up_u128({}, {}, {})",
                render_helper_arg(&a.node, ctx, consts, opts),
                render_helper_arg(&b.node, ctx, consts, opts),
                render_helper_arg(&d.node, ctx, consts, opts)
            );
            if opts.checked_arith {
                format!("({}).try_into().ok()?", call)
            } else {
                call
            }
        }
        // `contains(coll, elem)` → `coll.contains(&elem)`. The pre/post path
        // rewrite (`state.X` → `pre_X`/`post_X`) runs downstream on the string;
        // a `Vec` `coll` snapshotted `pre_X` must be `.clone()`d (non-Copy).
        Expr::Contains { coll, elem } => format!(
            "{}.contains(&{})",
            expr_to_rust(&coll.node, ctx, consts, opts),
            expr_to_rust(&elem.node, ctx, consts, opts)
        ),
        // `len(coll)` → `coll.len() as u64` — matches the common `u64` threshold
        // operand (`len(state.approved) >= threshold`) without a widening cast.
        Expr::Len(coll) => format!(
            "({}.len() as u64)",
            expr_to_rust(&coll.node, ctx, consts, opts)
        ),
        Expr::Match { scrutinee, arms } => {
            // Resolve the scrutinee's enum so each arm's pattern is shape-correct
            // (`Enum::Custom(s)` tuple / `Enum::OneTime` unit / `Enum::Active { .. }`
            // struct), with a `_` catch-all. Mirrors the ExprTree renderer.
            let sc = expr_to_rust(&scrutinee.node, ctx, consts, opts);
            let hint = match &scrutinee.node {
                Expr::Path(p) => opts.env.path_type_name(p),
                _ => None,
            };
            // Scrutinee ownership: an OWNED snapshot local (`pre.X` / `post.X`,
            // which the downstream pre/post rewrite turns into a bare owned
            // `pre_X` / `post_X`) is matched by REFERENCE — `match &(x)` — so a
            // deep container (a snapshotted `Vec<Hook>` policy field) is NOT
            // re-cloned. Its payload binders are read by-reference (field access,
            // `.iter()`, `.contains()`), which is exactly what a snapshot's
            // struct/collection payload wants. Any OTHER scrutinee (a `&`-place
            // like `c.field` under `.iter()`, or a value used as a scalar binder
            // `Custom(s) => s > 0`) keeps `.clone()` so the binder is OWNED and
            // there's no "cannot move out of a shared reference".
            let by_ref = scrutinee_is_owned_snapshot(&sc);
            let mut out = if by_ref {
                format!("match &({}) {{", sc)
            } else {
                format!("match ({}).clone() {{", sc)
            };
            for arm in arms {
                let pat = if arm.variant == "_" {
                    "_".to_string()
                } else {
                    let (enum_name, shape) =
                        match opts.env.resolve_variant(hint.as_deref(), &arm.variant) {
                            Some(pair) => pair,
                            None => (
                                hint.clone().unwrap_or_default(),
                                crate::mir::VariantShape::Struct,
                            ),
                        };
                    shape.arm_pattern(&enum_name, &arm.variant, arm.binder.as_deref())
                };
                out.push_str(&format!(
                    "\n    {} => {},",
                    pat,
                    expr_to_rust(&arm.body.node, ctx, consts, opts)
                ));
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
            // Resolve the scrutinee's enum type and the variant's shape so the
            // `matches!` pattern is shape-correct: `Enum::V { .. }` (struct,
            // `Approved of { timestamp }`), `Enum::V(..)` (tuple, `Custom of I64`),
            // `Enum::V` (unit). A Path scrutinee (the common `state.status is
            // .Approved`) resolves its enum via the type env; `resolve_variant`
            // falls back to a global unique-name search.
            let sc = expr_to_rust(&scrutinee.node, ctx, consts, opts);
            let hint = match &scrutinee.node {
                Expr::Path(p) => opts.env.path_type_name(p),
                _ => None,
            };
            let (enum_name, shape) = match opts.env.resolve_variant(hint.as_deref(), variant) {
                Some(pair) => pair,
                // Unresolved shape: keep the enum hint if we have one and
                // assume the struct shape (dominant for status enums).
                None => (
                    hint.unwrap_or_else(|| variant.clone()),
                    crate::mir::VariantShape::Struct,
                ),
            };
            format!(
                "matches!({}, {})",
                sc,
                shape.match_pattern(&enum_name, variant)
            )
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
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } | Expr::MulDivRoundHalfUp { .. } => {
            Kind::Nat
        }
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
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } | Expr::MulDivRoundHalfUp { .. } => true,
        Expr::Paren(inner) => is_mul_div_let_rhs(&inner.node),
        Expr::Old(inner) => is_mul_div_let_rhs(&inner.node),
        _ => false,
    }
}

/// True iff the expression is bare arithmetic at its spine — an `Arith`
/// node, possibly under `Paren`/`Old` wrappers or nested in another
/// `Arith`. `mul_div_*` doesn't count (the u128 helpers guard zero and
/// saturate internally — they can't panic), and arithmetic buried inside
/// call args / quantifier bodies doesn't count either: those positions
/// need native types and keep their own rendering.
fn spine_has_arith(e: &Expr) -> bool {
    match e {
        // `(a * b) / 10000` is exempt: the Kani backend rewrites that
        // exact shape to its solver-tuned `mul_bps_floor_u128` helper
        // (`rewrite_kani_bps_mul_div`); widening here would hide the
        // pattern and replace a q/r-decomposed helper with generic
        // 256-bit multiplication.
        e if is_bps_div_shape(e) => false,
        Expr::Arith { .. } => true,
        Expr::Paren(inner) | Expr::Old(inner) => spine_has_arith(&inner.node),
        _ => false,
    }
}

/// `(a * b) / 10000` — possibly under `Paren`/`Old` — the shape
/// `rewrite_kani_bps_mul_div` recognizes. See `spine_has_arith`.
fn is_bps_div_shape(e: &Expr) -> bool {
    fn is_mul(e: &Expr) -> bool {
        match e {
            Expr::Paren(inner) | Expr::Old(inner) => is_mul(&inner.node),
            Expr::Arith {
                op: a::ArithOp::Mul,
                ..
            } => true,
            _ => false,
        }
    }
    match e {
        Expr::Paren(inner) | Expr::Old(inner) => is_bps_div_shape(&inner.node),
        Expr::Arith {
            op: a::ArithOp::Div,
            lhs,
            rhs,
        } => matches!(&rhs.node, Expr::Int(10000)) && is_mul(&lhs.node),
        _ => false,
    }
}

/// Render a numeric term so its Rust type is exactly `wide` (`u128` /
/// `i128`), evaluating internal arithmetic without panics: `+` is exact
/// (u64-range operands can't overflow the wide type), `-` on `u128`
/// saturates (Lean `Nat` monus), `*` saturates at the wide MAX, `/` and
/// `%` follow the Lean total-function convention (`x / 0 = 0`,
/// `x % 0 = x`). Leaves render natively and cast up.
fn render_widened_term(
    e: &Expr,
    ctx: Ctx,
    consts: ConstTable,
    opts: RustOpts<'_, '_>,
    wide: &str,
) -> String {
    match e {
        Expr::Paren(inner) => format!(
            "({})",
            render_widened_term(&inner.node, ctx, consts, opts, wide)
        ),
        Expr::Old(inner) => {
            render_widened_term(&inner.node, ctx, consts, opts.with_inside_old(), wide)
        }
        Expr::Arith { op, lhs, rhs } => {
            let l = render_widened_term(&lhs.node, ctx, consts, opts, wide);
            let r = render_widened_term(&rhs.node, ctx, consts, opts, wide);
            match op {
                a::ArithOp::Add => format!("{} + {}", l, r),
                a::ArithOp::Sub => {
                    if wide == "u128" {
                        format!("({}).saturating_sub({})", l, r)
                    } else {
                        format!("{} - {}", l, r)
                    }
                }
                a::ArithOp::Mul => format!("({}).saturating_mul({})", l, r),
                a::ArithOp::Div => format!("({}).checked_div({}).unwrap_or(0)", l, r),
                a::ArithOp::Mod => format!("({}).checked_rem({}).unwrap_or({})", l, r, l),
            }
        }
        // Already u128-typed helpers — cast only when the wide type differs.
        Expr::MulDivFloor { .. } | Expr::MulDivCeil { .. } | Expr::MulDivRoundHalfUp { .. } => {
            let s = expr_to_rust(e, ctx, consts, opts);
            if wide == "u128" {
                s
            } else {
                format!("(({}) as {})", s, wide)
            }
        }
        _ => format!("(({}) as {})", expr_to_rust(e, ctx, consts, opts), wide),
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

/// True when a rendered scrutinee is a single snapshot-local field access
/// (`pre.<field>` / `post.<field>`) — the impl-Kani ensures form that the
/// downstream pre/post rewrite turns into a bare OWNED local `pre_<field>` /
/// `post_<field>`. Such a scrutinee can be matched by reference (`match &(x)`)
/// without a defensive `.clone()`. Anything with further path segments
/// (`post.a.b`), a `&`-place under `.iter()` (`c.field`), or a non-snapshot
/// base returns `false` and keeps the clone.
fn scrutinee_is_owned_snapshot(sc: &str) -> bool {
    let rest = sc.strip_prefix("pre.").or_else(|| sc.strip_prefix("post."));
    match rest {
        Some(field) => {
            !field.is_empty()
                && field
                    .bytes()
                    .all(|b| b == b'_' || b.is_ascii_alphanumeric())
        }
        None => false,
    }
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
