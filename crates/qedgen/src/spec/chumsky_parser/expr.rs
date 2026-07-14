//! The expression grammar: the recursive `expr` combinator covering
//! literals, paths, arithmetic, quantifiers, `match`, `let`, and the rest
//! of the typed-AST expression surface.

use super::*;

// ----------------------------------------------------------------------------
// Expressions (the main win of the typed AST)
// ----------------------------------------------------------------------------

/// `name()` — a zero-arg builtin atom parsing to
/// `Expr::App { func: name, args: [] }` (e.g. `now()`, `current_epoch()`).
fn zero_arg_builtin<'a>(
    name: &'static str,
) -> impl Parser<'a, &'a str, Node<Expr>, Err<'a>> + Clone {
    bare_kw(name)
        .then_ignore(wsc())
        .ignore_then(just('('))
        .then_ignore(wsc())
        .then_ignore(just(')'))
        .map_with(move |_, e| {
            Node::new(
                Expr::App {
                    func: name.to_string(),
                    args: vec![],
                },
                e.span().into_range(),
            )
        })
}

pub(super) fn expr<'a>() -> impl Parser<'a, &'a str, Node<Expr>, Err<'a>> + Clone {
    recursive(|expr| {
        let int = integer().map_with(|v, e| Node::new(Expr::Int(v), e.span().into_range()));

        // Unary minus on integer literals only (`-6`, not `-x` or
        // `-(a + b)`). Lowers to `Arith { Sub, Int(0), Int(v) }` — no AST
        // change; renderers handle `0 - 6` as subtraction. Restricting to
        // immediate literals keeps prefix-vs-infix `-` disambiguation
        // trivial: this atom only fires at atom-start, so binary `a - 6`
        // is unaffected (`-` is consumed by `add_op`).
        let neg_int = just('-').ignore_then(integer()).map_with(|v, e| {
            let span = e.span().into_range();
            Node::new(
                Expr::Arith {
                    op: ArithOp::Sub,
                    lhs: Box::new(Node::new(Expr::Int(0), span.clone())),
                    rhs: Box::new(Node::new(Expr::Int(v), span.clone())),
                },
                span,
            )
        });

        let bool_lit = choice((kw("true").to(true), kw("false").to(false)))
            .map_with(|b, e| Node::new(Expr::Bool(b), e.span().into_range()));

        let path_expr = path().map_with(|p, e| Node::new(Expr::Path(p), e.span().into_range()));

        // old(expr)
        let old = just("old")
            .then_ignore(wsc())
            .ignore_then(just('('))
            .then_ignore(wsc())
            .ignore_then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(just(')'))
            .map_with(|inner, e| Node::new(Expr::Old(Box::new(inner)), e.span().into_range()));

        // sum i : T, body
        let sum = just("sum")
            .then_ignore(wsc())
            .ignore_then(non_keyword_ident())
            .then_ignore(wsc())
            .then_ignore(just(':'))
            .then_ignore(wsc())
            .then(qualified_path())
            .then_ignore(wsc())
            .then_ignore(just(','))
            .then_ignore(wsc())
            .then(expr.clone())
            .map_with(|((binder, binder_ty), body), e| {
                let ty_name = binder_ty.0.join(".");
                Node::new(
                    Expr::Sum {
                        binder,
                        binder_ty: ty_name,
                        body: Box::new(body),
                    },
                    e.span().into_range(),
                )
            });

        // forall / exists i : T, body      (single binder)
        // forall / exists i j k : T, body   (multi-binder — desugars to nested quantifiers,
        //                                    all binders share the same type annotation)
        let quant = choice((
            just("forall").to(Quantifier::Forall),
            just("exists").to(Quantifier::Exists),
        ))
        .then_ignore(wsc())
        .then(
            non_keyword_ident()
                .then_ignore(wsc())
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just(','))
        .then_ignore(wsc())
        .then(expr.clone())
        .map_with(|(((kind, binders), binder_ty), body), e| {
            let ty_name = binder_ty.0.join(".");
            let span = e.span().into_range();
            // Fold binders right-to-left so the outermost binder is the first listed.
            binders.into_iter().rev().fold(body, |acc, binder| {
                Node::new(
                    Expr::Quant {
                        kind,
                        binder,
                        binder_ty: ty_name.clone(),
                        body: Box::new(acc),
                    },
                    span.clone(),
                )
            })
        });

        // forall / exists x in <collection>, body — a bounded quantifier over a
        // COLLECTION value (a `Vec` field), not a type domain. Disambiguated from
        // `quant` by `in` vs `:` after the (single) binder; tried first in the
        // atom choice so `in` wins before `quant` consumes `:`.
        let quant_in = choice((
            just("forall").to(Quantifier::Forall),
            just("exists").to(Quantifier::Exists),
        ))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(kw("in"))
        .then(expr.clone())
        .then_ignore(wsc())
        .then_ignore(just(','))
        .then_ignore(wsc())
        .then(expr.clone())
        .map_with(|(((kind, binder), coll), body), e| {
            Node::new(
                Expr::QuantIn {
                    kind,
                    binder,
                    coll: Box::new(coll),
                    body: Box::new(body),
                },
                e.span().into_range(),
            )
        });

        // Parenthesized sub-expression
        let paren = just('(')
            .then_ignore(wsc())
            .ignore_then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(just(')'))
            .map_with(|inner, e| Node::new(Expr::Paren(Box::new(inner)), e.span().into_range()));

        // Scaled-integer built-in triads.
        // for scaled integer math. The VM has no native fixed-point; this is
        // the canonical `widen → multiply → floor-divide by scale` pattern.
        #[derive(Clone, Copy)]
        enum MulDivMode {
            Floor,
            Ceil,
            RoundHalfUp,
        }
        let mdf_args = |kw_name: &'static str, mode: MulDivMode| {
            let e1 = expr.clone();
            let e2 = expr.clone();
            let e3 = expr.clone();
            bare_kw(kw_name)
                .then_ignore(wsc())
                .ignore_then(just('('))
                .then_ignore(wsc())
                .ignore_then(e1)
                .then_ignore(wsc())
                .then_ignore(just(','))
                .then_ignore(wsc())
                .then(e2)
                .then_ignore(wsc())
                .then_ignore(just(','))
                .then_ignore(wsc())
                .then(e3)
                .then_ignore(wsc())
                .then_ignore(just(')'))
                .map_with(move |((a, b), d), e| {
                    let node = match mode {
                        MulDivMode::Ceil => Expr::MulDivCeil {
                            a: Box::new(a),
                            b: Box::new(b),
                            d: Box::new(d),
                        },
                        MulDivMode::Floor => Expr::MulDivFloor {
                            a: Box::new(a),
                            b: Box::new(b),
                            d: Box::new(d),
                        },
                        MulDivMode::RoundHalfUp => Expr::MulDivRoundHalfUp {
                            a: Box::new(a),
                            b: Box::new(b),
                            d: Box::new(d),
                        },
                    };
                    Node::new(node, e.span().into_range())
                })
        };
        let mul_div_floor_atom = mdf_args("mul_div_floor", MulDivMode::Floor);
        let mul_div_ceil_atom = mdf_args("mul_div_ceil", MulDivMode::Ceil);
        let mul_div_round_half_up_atom = mdf_args("mul_div_round_half_up", MulDivMode::RoundHalfUp);

        // len(coll) — collection length → `Expr::Len`.
        let len_atom = {
            let ecoll = expr.clone();
            bare_kw("len")
                .then_ignore(wsc())
                .ignore_then(just('('))
                .then_ignore(wsc())
                .ignore_then(ecoll)
                .then_ignore(wsc())
                .then_ignore(just(')'))
                .map_with(|coll, e| Node::new(Expr::Len(Box::new(coll)), e.span().into_range()))
        };

        // contains(coll, elem) — collection membership → `Expr::Contains`.
        // Built-in (not an `in` operator: `in` is a reserved keyword).
        let contains_atom = {
            let ecoll = expr.clone();
            let eelem = expr.clone();
            bare_kw("contains")
                .then_ignore(wsc())
                .ignore_then(just('('))
                .then_ignore(wsc())
                .ignore_then(ecoll)
                .then_ignore(wsc())
                .then_ignore(just(','))
                .then_ignore(wsc())
                .then(eelem)
                .then_ignore(wsc())
                .then_ignore(just(')'))
                .map_with(|(coll, elem), e| {
                    Node::new(
                        Expr::Contains {
                            coll: Box::new(coll),
                            elem: Box::new(elem),
                        },
                        e.span().into_range(),
                    )
                })
        };

        // `now()` — zero-arg builtin: fresh symbolic `u64` timestamp.
        // Lowers per-backend: Rust `Clock::get().unwrap().unix_timestamp`,
        // Lean axiomatized `QEDGen.Solana.Valid.now`, Kani/proptest
        // `any::<u64>()`. Parses to `Expr::App { func: "now", args: [] }`,
        // special-cased in `chumsky_adapter::expr_to_rust` / `expr_to_lean`.
        let now_atom = zero_arg_builtin("now");

        // `current_epoch()` — like `now()` but Rust reads
        // `Clock::get().unwrap().epoch`; Lean axiomatizes
        // `QEDGen.Solana.Valid.current_epoch : Nat`.
        let current_epoch_atom = zero_arg_builtin("current_epoch");

        // Generic function application: `f(arg1, arg2, ...)`.
        // Must precede path_expr in the atom choice (both start with ident);
        // `.and_is(just('(').rewind())` ensures we only commit to `app` when
        // the ident is immediately followed by `(`, so bare paths like
        // `state.foo` still route to path_expr.
        let app_expr = non_keyword_ident()
            .and_is(
                any::<&'a str, Err<'a>>()
                    .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                    .repeated()
                    .ignore_then(just('('))
                    .rewind(),
            )
            .then_ignore(just('('))
            .then_ignore(wsc())
            .then(
                expr.clone()
                    .separated_by(just(',').then_ignore(wsc()))
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .then_ignore(wsc())
            .then_ignore(just(')'))
            .map_with(|(func, args), e| Node::new(Expr::App { func, args }, e.span().into_range()))
            .boxed();

        // Inline `match scrutinee with | Variant binder? => body | ...`.
        // Distinct from the handler-clause `match` — this one has an explicit
        // scrutinee and `with` keyword, producing a value. A `_` arm is the
        // catch-all (variant sentinel `"_"`, no binder) — lets a payload-binding
        // match (`| Custom s => s > 0 | _ => true`) avoid enumerating every
        // other variant.
        let wildcard_arm = just('_')
            .then_ignore(wsc())
            .then_ignore(just("=>"))
            .then_ignore(wsc())
            .ignore_then(expr.clone())
            .map(|body| MatchExprArm {
                variant: "_".to_string(),
                binder: None,
                body: Box::new(body),
            });
        let named_arm = non_keyword_ident()
            .then_ignore(wsc())
            .then(non_keyword_ident().or_not())
            .then_ignore(wsc())
            .then_ignore(just("=>"))
            .then_ignore(wsc())
            .then(expr.clone())
            .map(|((variant, binder), body)| MatchExprArm {
                variant,
                binder,
                body: Box::new(body),
            });
        let match_arm_pat = choice((wildcard_arm.boxed(), named_arm.boxed()));
        let match_arm = just('|').then_ignore(wsc()).ignore_then(match_arm_pat);
        let match_expr = kw("match")
            .ignore_then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(kw("with"))
            .then(
                match_arm
                    .then_ignore(wsc())
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<MatchExprArm>>(),
            )
            .map_with(|(scrutinee, arms), e| {
                Node::new(
                    Expr::Match {
                        scrutinee: Box::new(scrutinee),
                        arms,
                    },
                    e.span().into_range(),
                )
            });

        // Field-init list: `field := expr, ...`. Boxed to curb type blow-up
        // that triggers Apple's linker symbol-length assertion.
        let field_init = non_keyword_ident()
            .then_ignore(wsc())
            .then_ignore(just(":="))
            .then_ignore(wsc())
            .then(expr.clone())
            .map(|(n, v)| (n, v))
            .boxed();
        let field_init_list = field_init
            .clone()
            .then_ignore(wsc())
            .separated_by(just(',').then_ignore(wsc()))
            .allow_trailing()
            .collect::<Vec<(String, Node<Expr>)>>()
            .boxed();

        // `{ base with f := v, ... }` — record update. PEG: tried before
        // record literal so the `with` keyword discriminates.
        let record_update = just('{')
            .then_ignore(wsc())
            .ignore_then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(kw("with"))
            .then(field_init_list.clone())
            .then_ignore(wsc())
            .then_ignore(just('}'))
            .map_with(|(base, updates), e| {
                Node::new(
                    Expr::RecordUpdate {
                        base: Box::new(base),
                        updates,
                    },
                    e.span().into_range(),
                )
            })
            .boxed();

        // `{ f := v, ... }` — anonymous record literal (no `with`).
        let record_lit = just('{')
            .then_ignore(wsc())
            .ignore_then(field_init_list.clone())
            .then_ignore(wsc())
            .then_ignore(just('}'))
            .map_with(|fields, e| Node::new(Expr::RecordLit(fields), e.span().into_range()))
            .boxed();

        // `.Variant` or `.Variant payload`. Payload is a record literal or
        // record update (or, in principle, any expression — we constrain to
        // braced forms for readability).
        let ctor_payload = choice((record_update.clone(), record_lit.clone())).boxed();
        let ctor = just('.')
            .ignore_then(non_keyword_ident())
            .then_ignore(wsc())
            .then(ctor_payload.or_not())
            .map_with(|(variant, payload_opt), e| {
                Node::new(
                    Expr::Ctor {
                        variant,
                        payload: payload_opt.map(Box::new),
                    },
                    e.span().into_range(),
                )
            })
            .boxed();

        // `let NAME = value in body` — ML-style expression binding.
        // Inside ensures/requires/effect-rhs, lets you derive a value once
        // and reference it by name. Lowers to Lean's `let NAME := value; body`.
        let let_in = kw("let")
            .ignore_then(non_keyword_ident())
            .then_ignore(wsc())
            .then_ignore(just('='))
            .then_ignore(wsc())
            .then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(kw("in"))
            .then(expr.clone())
            .map_with(|((name, value), body), e| {
                Node::new(
                    Expr::Let {
                        name,
                        value: Box::new(value),
                        body: Box::new(body),
                    },
                    e.span().into_range(),
                )
            })
            .boxed();

        // `if cond then a else b` — full conditional in expression
        // position (v2.8 fold-in F9). `if` / `then` / `else` are
        // contextual keywords matched only at the start of this atom;
        // they aren't reserved globally so handler fields named `if` or
        // `then` (unlikely but possible) keep working.
        let if_then_else = kw("if")
            .ignore_then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(kw("then"))
            .then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(kw("else"))
            .then(expr.clone())
            .map_with(|((cond, then_branch), else_branch), e| {
                Node::new(
                    Expr::IfThenElse {
                        cond: Box::new(cond),
                        then_branch: Box::new(then_branch),
                        else_branch: Box::new(else_branch),
                    },
                    e.span().into_range(),
                )
            })
            .boxed();

        // atom — must stay under chumsky's `choice` arity limit; split.
        // `.boxed()` tames the type complexity that otherwise trips Apple's
        // linker on overlong symbol names.
        // `neg_int` precedes `int` so the leading `-` doesn't fail
        // the digit-first `integer()` filter. Order within the
        // `choice` doesn't affect performance — both branches commit
        // on their first character.
        let group_a = choice((
            neg_int,
            int,
            bool_lit,
            old,
            let_in,
            if_then_else,
            sum,
            quant_in,
            quant,
        ))
        .boxed();
        let group_b = choice((
            now_atom,
            current_epoch_atom,
            mul_div_floor_atom,
            mul_div_ceil_atom,
            mul_div_round_half_up_atom,
            contains_atom,
            len_atom,
            match_expr,
        ))
        .boxed();
        // `record_update` must precede `ctor` (leading `.` distinguishes
        // them, but this ordering is clearer). `app_expr` must precede
        // `path_expr` (both start with ident; app commits only when `(`
        // follows, so bare paths still route to path_expr). Try
        // record_update before record_lit; both before bare-path fallback.
        let group_c = choice((record_update, record_lit, ctor, paren, app_expr, path_expr)).boxed();
        let atom_base = choice((group_a, group_b, group_c))
            .then_ignore(wsc())
            .boxed();

        // Postfix `.field` — layers on any atom result. Used for chains
        // like `left(n).key` where the base isn't a bare path.
        // `.` must NOT be followed by `0-9` (could be a float) or an
        // uppercase ident (`.Variant` constructor syntax); but we already
        // distinguish variants by being at atom position not postfix.
        let field_postfix = just('.')
            .then(
                any::<&'a str, Err<'a>>()
                    .filter(|c: &char| c.is_ascii_lowercase() || *c == '_')
                    .rewind(),
            )
            .ignore_then(non_keyword_ident())
            .then_ignore(wsc())
            .boxed();
        let atom_with_fields = atom_base.foldl_with(field_postfix.repeated(), |base, field, e| {
            Node::new(
                Expr::Field {
                    base: Box::new(base),
                    field,
                },
                e.span().into_range(),
            )
        });

        // Postfix `is .Variant` check — layers on any atom result.
        let is_postfix = kw("is")
            .ignore_then(just('.'))
            .ignore_then(non_keyword_ident())
            .then_ignore(wsc());
        let atom =
            atom_with_fields
                .then(is_postfix.or_not())
                .map_with(|(base, is_v), e| match is_v {
                    None => base,
                    Some(variant) => Node::new(
                        Expr::IsVariant {
                            scrutinee: Box::new(base),
                            variant,
                        },
                        e.span().into_range(),
                    ),
                });

        // product: atom (('*' | '/' | '%') atom)*
        let mul_op = choice((
            just('*').to(ArithOp::Mul),
            just('/').to(ArithOp::Div),
            just('%').to(ArithOp::Mod),
        ))
        .then_ignore(wsc());
        let product =
            atom.clone()
                .foldl_with(mul_op.then(atom.clone()).repeated(), |lhs, (op, rhs), e| {
                    Node::new(
                        Expr::Arith {
                            op,
                            lhs: Box::new(lhs),
                            rhs: Box::new(rhs),
                        },
                        e.span().into_range(),
                    )
                });

        // sum-expr (arithmetic additive): product (('+' | '-') product)*
        let add_op =
            choice((just('+').to(ArithOp::Add), just('-').to(ArithOp::Sub))).then_ignore(wsc());
        let arith = product.clone().foldl_with(
            add_op.then(product.clone()).repeated(),
            |lhs, (op, rhs), e| {
                Node::new(
                    Expr::Arith {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                )
            },
        );

        // comparison: arith (cmp_op arith)?
        let cmp_op = choice((
            just("<=").to(CmpOp::Le),
            just(">=").to(CmpOp::Ge),
            just("!=").to(CmpOp::Ne),
            just("==").to(CmpOp::Eq),
            just('<').to(CmpOp::Lt),
            just('>').to(CmpOp::Gt),
        ))
        .then_ignore(wsc());
        let cmp = arith
            .clone()
            .then(cmp_op.then(arith.clone()).or_not())
            .map_with(|(lhs, maybe_rhs), e| match maybe_rhs {
                None => lhs,
                Some((op, rhs)) => Node::new(
                    Expr::Cmp {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                ),
            });

        // not: ("not" cmp) | cmp
        let not_expr = recursive(|not_expr| {
            choice((
                just("not")
                    .then_ignore(wsc())
                    .ignore_then(not_expr.clone())
                    .map_with(|inner, e| {
                        Node::new(Expr::Not(Box::new(inner)), e.span().into_range())
                    }),
                cmp.clone(),
            ))
        });

        // and: not ("and" | "/\") not  (left-assoc)
        let and_op = choice((just("and").ignored(), just("/\\").ignored())).then_ignore(wsc());
        let and = not_expr.clone().foldl_with(
            and_op.then(not_expr.clone()).repeated(),
            |lhs, ((), rhs), e| {
                Node::new(
                    Expr::BoolOp {
                        op: BoolOp::And,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                )
            },
        );

        // implies: and ("implies" and)*   (right-assoc conventional, left here)
        let implies_op = just("implies").then_ignore(wsc()).ignored();
        let implies = and.clone().foldl_with(
            implies_op.then(and.clone()).repeated(),
            |lhs, ((), rhs), e| {
                Node::new(
                    Expr::BoolOp {
                        op: BoolOp::Implies,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                )
            },
        );

        // or: implies (("or" | "\/") implies)*
        let or_op = choice((just("or").ignored(), just("\\/").ignored())).then_ignore(wsc());
        let or = implies.clone().foldl_with(
            or_op.then(implies.clone()).repeated(),
            |lhs, ((), rhs), e| {
                Node::new(
                    Expr::BoolOp {
                        op: BoolOp::Or,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                )
            },
        );

        or
    })
}
