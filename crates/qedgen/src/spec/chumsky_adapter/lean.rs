//! Lean expression rendering: typed AST → unicode-operator Lean strings.

use super::*;

/// Render typed expression to a Lean-compatible string (unicode operators).
/// Threads a `TypeEnv` through so arithmetic/comparison can promote Nat→Int
/// when operands' kinds differ.
pub(super) fn expr_to_lean(e: &Expr, ctx: Ctx, consts: ConstTable, env: &TypeEnv) -> String {
    match e {
        Expr::Int(v) => v.to_string(),
        // Lean 4 Bool literals are lowercase `true`/`false`; `True`/`False`
        // are *Props*, so `flag := True` would type-error when `flag : Bool`.
        Expr::Bool(b) => b.to_string(),
        Expr::Path(p) => path_to_lean(p, ctx, /*inside_old=*/ false, consts),
        Expr::Old(inner) => path_or_expr_to_lean_old(&inner.node, ctx, consts, env),
        Expr::Sum {
            binder,
            binder_ty,
            body,
        } => format!(
            "(\u{2211} {} : {}, {})",
            binder,
            binder_ty,
            expr_to_lean(&body.node, ctx, consts, env)
        ),
        Expr::Quant {
            kind,
            binder,
            binder_ty,
            body,
        } => {
            let sym = match kind {
                a::Quantifier::Forall => "\u{2200}",
                a::Quantifier::Exists => "\u{2203}",
            };
            let lean_ty = match binder_ty.as_str() {
                "U64" | "U32" | "U16" | "U8" | "U128" => "Nat",
                "I64" | "I32" | "I16" | "I8" | "I128" => "Int",
                other => other,
            };
            format!(
                "{} {} : {}, {}",
                sym,
                binder,
                lean_ty,
                expr_to_lean(&body.node, ctx, consts, env)
            )
        }
        Expr::QuantIn {
            kind,
            binder,
            coll,
            body,
        } => {
            // `∃|∀ x ∈ coll, body` — a bounded quantifier over a List (Vec).
            let sym = match kind {
                a::Quantifier::Forall => "\u{2200}",
                a::Quantifier::Exists => "\u{2203}",
            };
            format!(
                "({} {} \u{2208} {}, {})",
                sym,
                binder,
                expr_to_lean(&coll.node, ctx, consts, env),
                expr_to_lean(&body.node, ctx, consts, env)
            )
        }
        Expr::BoolOp { op, lhs, rhs } => {
            let sym = match op {
                a::BoolOp::And => " \u{2227} ",
                a::BoolOp::Or => " \u{2228} ",
                a::BoolOp::Implies => " \u{2192} ",
            };
            format!(
                "{}{}{}",
                expr_to_lean(&lhs.node, ctx, consts, env),
                sym,
                expr_to_lean(&rhs.node, ctx, consts, env)
            )
        }
        Expr::Not(inner) => {
            format!("\u{00AC}({})", expr_to_lean(&inner.node, ctx, consts, env))
        }
        Expr::Cmp { op, lhs, rhs } => {
            let sym = match op {
                a::CmpOp::Eq => "=",
                a::CmpOp::Ne => "\u{2260}",
                a::CmpOp::Le => "\u{2264}",
                a::CmpOp::Ge => "\u{2265}",
                a::CmpOp::Lt => "<",
                a::CmpOp::Gt => ">",
            };
            let (l_str, r_str) =
                render_binary_with_coercion(&lhs.node, &rhs.node, ctx, consts, env);
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
            let (l_str, r_str) =
                render_binary_with_coercion(&lhs.node, &rhs.node, ctx, consts, env);
            format!("{}{}{}", l_str, sym, r_str)
        }
        Expr::Paren(inner) => format!("({})", expr_to_lean(&inner.node, ctx, consts, env)),
        Expr::MulDivFloor { a, b, d } => {
            // Lean Int is unbounded — the math simplifies to `(a * b) / d`
            // with integer division. If any operand is Int, the whole expr
            // is Int; otherwise we stay in Nat. Overflow is a Rust-codegen
            // concern, not a proof concern.
            let (a_str, b_str) = render_binary_with_coercion(&a.node, &b.node, ctx, consts, env);
            let d_str = expr_to_lean(&d.node, ctx, consts, env);
            format!("((({}) * ({})) / ({}))", a_str, b_str, d_str)
        }
        // `contains(coll, elem)` → Lean membership `elem ∈ coll` (List.Mem).
        Expr::Contains { coll, elem } => format!(
            "({} ∈ {})",
            expr_to_lean(&elem.node, ctx, consts, env),
            expr_to_lean(&coll.node, ctx, consts, env)
        ),
        // `len(coll)` → Lean `coll.length`.
        Expr::Len(coll) => format!("({}).length", expr_to_lean(&coll.node, ctx, consts, env)),
        Expr::Match { scrutinee, arms } => {
            // Render as Lean's `match ... with | Ctor binder? => body | ...`.
            // If the body doesn't reference the binder, emit `_` instead —
            // Lean's Decidable-synthesis is tripped up by named binders in
            // Prop-valued arms that don't use them.
            let sc = expr_to_lean(&scrutinee.node, ctx, consts, env);
            let mut out = String::new();
            out.push_str("(match ");
            out.push_str(&sc);
            out.push_str(" with");
            for arm in arms {
                let body_str = expr_to_lean(&arm.body.node, ctx, consts, env);
                let binder_used = arm
                    .binder
                    .as_deref()
                    .map(|b| body_mentions_binder(&body_str, b))
                    .unwrap_or(false);
                out.push_str(&format!("\n    | .{}", arm.variant));
                if let Some(b) = &arm.binder {
                    out.push(' ');
                    if binder_used {
                        out.push_str(b);
                    } else {
                        out.push('_');
                    }
                }
                out.push_str(" => ");
                out.push_str(&body_str);
            }
            out.push(')');
            out
        }
        Expr::Ctor { variant, payload } => {
            // Lean anonymous constructor: `.Variant` or `.Variant <payload>`.
            // Payload is typically a record literal or record update; renders
            // verbatim. Lean's elaborator resolves the expected type.
            match payload {
                None => format!(".{}", variant),
                Some(p) => format!(".{} {}", variant, expr_to_lean(&p.node, ctx, consts, env)),
            }
        }
        Expr::RecordLit(fields) => {
            let body = fields
                .iter()
                .map(|(n, v)| format!("{} := {}", n, expr_to_lean(&v.node, ctx, consts, env)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {} }}", body)
        }
        Expr::RecordUpdate { base, updates } => {
            let base_str = expr_to_lean(&base.node, ctx, consts, env);
            let body = updates
                .iter()
                .map(|(n, v)| format!("{} := {}", n, expr_to_lean(&v.node, ctx, consts, env)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {} with {} }}", base_str, body)
        }
        Expr::IsVariant { scrutinee, variant } => {
            // Route through the per-variant helper when we can resolve the
            // scrutinee's type. `TypeName.isVariant x = true` is always
            // Decidable (Bool equality), unlike a raw match on a Prop.
            // Fallback path (unknown type): inline match, may not elaborate
            // if Lean can't synthesize Decidable.
            let sc = expr_to_lean(&scrutinee.node, ctx, consts, env);
            if let Expr::Path(p) = &scrutinee.node {
                if let Some(ty_name) = env.path_type_name(p) {
                    return format!("({}.is{} {} = true)", ty_name, variant, sc);
                }
            }
            format!("(match {} with | .{} _ => True | _ => False)", sc, variant)
        }
        Expr::MulDivCeil { a, b, d } => {
            // ceil(a*b/d) = (a*b + d - 1) / d   for positive d.
            // Lean: we emit the identity directly. Signed operands still
            // work because Lean's integer division rounds toward zero; for
            // positive `d` and nonnegative `a*b` this matches ceiling.
            // Spec authors assume `d > 0`; downstream proofs rely on that.
            let (a_str, b_str) = render_binary_with_coercion(&a.node, &b.node, ctx, consts, env);
            let d_str = expr_to_lean(&d.node, ctx, consts, env);
            format!(
                "((({}) * ({}) + ({}) - 1) / ({}))",
                a_str, b_str, d_str, d_str
            )
        }
        Expr::MulDivRoundHalfUp { a, b, d } => {
            // For non-negative quantities and positive d, adding floor(d/2)
            // implements nearest rounding with exact halves rounded upward.
            let (a_str, b_str) = render_binary_with_coercion(&a.node, &b.node, ctx, consts, env);
            let d_str = expr_to_lean(&d.node, ctx, consts, env);
            format!(
                "((({}) * ({}) + ({}) / 2) / ({}))",
                a_str, b_str, d_str, d_str
            )
        }
        Expr::App { func, args } => {
            // `now()` is an axiomatized symbolic timestamp: the support
            // library declares `axiom now : Nat` (in scope because
            // lean_gen.rs imports QEDGen.Solana).
            if func == "now" && args.is_empty() {
                return "now".to_string();
            }
            // `current_epoch()` resolves the same way — axiomatized at
            // `QEDGen.Solana.Valid.current_epoch : Nat`.
            if func == "current_epoch" && args.is_empty() {
                return "current_epoch".to_string();
            }
            // Lean function application: `f a b c` (space-separated, parenthesized
            // args). Leaves `func` as the raw name — downstream users declare
            // these as uninterpreted helpers (axioms or defs) in a support module.
            let args_str: Vec<String> = args
                .iter()
                .map(|n| format!("({})", expr_to_lean(&n.node, ctx, consts, env)))
                .collect();
            format!("({} {})", func, args_str.join(" "))
        }
        Expr::Field { base, field } => {
            let base_str = expr_to_lean(&base.node, ctx, consts, env);
            format!("({}).{}", base_str, field)
        }
        Expr::Let { name, value, body } => {
            // Lean's `let x := v; body` is semicolon-separated inside a
            // tactic-free term position, which is what ensures/requires give us.
            format!(
                "(let {} := {}; {})",
                name,
                expr_to_lean(&value.node, ctx, consts, env),
                expr_to_lean(&body.node, ctx, consts, env)
            )
        }
        Expr::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => format!(
            "(if {} then {} else {})",
            expr_to_lean(&cond.node, ctx, consts, env),
            expr_to_lean(&then_branch.node, ctx, consts, env),
            expr_to_lean(&else_branch.node, ctx, consts, env),
        ),
    }
}

/// Render both sides of a binary op, inserting a `((x : Int))` coercion on
/// whichever side is Nat when the other is Int. Leaves operand pairs of
/// matching kind untouched.
fn render_binary_with_coercion(
    lhs: &Expr,
    rhs: &Expr,
    ctx: Ctx,
    consts: ConstTable,
    env: &TypeEnv,
) -> (String, String) {
    let lk = env.infer(lhs);
    let rk = env.infer(rhs);
    let l_str = expr_to_lean(lhs, ctx, consts, env);
    let r_str = expr_to_lean(rhs, ctx, consts, env);
    match (lk, rk) {
        (Kind::Nat, Kind::Int) => (format!("((({}) : Int))", l_str), r_str),
        (Kind::Int, Kind::Nat) => (l_str, format!("((({}) : Int))", r_str)),
        _ => (l_str, r_str),
    }
}

/// Render path to Lean form, honoring `state.X` prefix. Bare idents matching
/// a declared constant are substituted with the literal value (pest parity).
fn path_to_lean(p: &a::Path, ctx: Ctx, inside_old: bool, consts: ConstTable) -> String {
    let mut out = String::new();
    let is_state_path = p.root == "state";
    if is_state_path {
        let prefix = if inside_old {
            "s."
        } else {
            match ctx {
                Ctx::Guard => "s.",
                Ctx::Ensures => "s'.",
            }
        };
        out.push_str(prefix);
        for seg in &p.segments {
            match seg {
                a::PathSeg::Field(f) => {
                    if out.ends_with('.') {
                        out.push_str(f);
                    } else {
                        out.push('.');
                        out.push_str(f);
                    }
                }
                a::PathSeg::Index(i) => {
                    out.push('[');
                    out.push_str(i);
                    out.push(']');
                }
            }
        }
        if out.ends_with('.') {
            out.pop();
        }
    } else if p.segments.is_empty() {
        // Bare ident — substitute if declared as a const.
        if let Some(v) = consts.get(&p.root) {
            out.push_str(v);
        } else {
            out.push_str(&p.root);
        }
    } else {
        out.push_str(&p.to_source_string());
    }
    out
}

fn path_or_expr_to_lean_old(inner: &Expr, ctx: Ctx, consts: ConstTable, env: &TypeEnv) -> String {
    match inner {
        Expr::Path(p) => path_to_lean(p, ctx, /*inside_old=*/ true, consts),
        other => match ctx {
            Ctx::Guard => {
                let rendered = expr_to_lean(other, Ctx::Guard, consts, env);
                format!("\u{00AB}old({})\u{00BB}", strip_state_prefix(&rendered))
            }
            Ctx::Ensures => expr_to_lean(other, Ctx::Guard, consts, env),
        },
    }
}

/// Check if an arm body string mentions an identifier as a whole word.
/// Used to decide whether to preserve `binder` or emit `_` in match arms.
/// Alias of the shared scanner in `codegen_shared`.
fn body_mentions_binder(body: &str, binder: &str) -> bool {
    crate::codegen_shared::contains_word_boundary(body, binder)
}

fn strip_state_prefix(s: &str) -> String {
    s.strip_prefix("s.")
        .or_else(|| s.strip_prefix("s'."))
        .map(|r| r.to_string())
        .unwrap_or_else(|| s.to_string())
}
