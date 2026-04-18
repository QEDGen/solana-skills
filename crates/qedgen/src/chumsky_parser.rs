//! Chumsky-based parser for `.qedspec` files — Phase 1.
//!
//! Strangler pattern: pest remains the default. This module parses the spec
//! into the typed AST (`ast::Spec`) defined alongside. Downstream consumers
//! still expect the legacy `ParsedSpec`; an adapter in `chumsky_adapter.rs`
//! translates typed AST → `ParsedSpec` for backward compatibility.
//!
//! Coverage in Phase 1: enough to parse `examples/rust/percolator/percolator.qedspec`.
//!   - spec header, const, record, ADT (state + error)
//!   - handler blocks: auth, accounts, requires, ensures, effect
//!   - property, cover, liveness, invariant
//!   - expressions: arithmetic, comparisons, and/or/implies/not,
//!     forall/exists, sum, old(), subscripts, parenthesized groups
//!
//! Deliberately not in Phase 1: sBPF instruction blocks, schemas,
//! environments, PDAs, events. pest continues to handle specs that use those.

#![allow(dead_code)] // scaffolding; consumers land in subsequent phases

use chumsky::prelude::*;

use crate::ast::*;

type Err<'a> = extra::Err<Rich<'a, char>>;

// ----------------------------------------------------------------------------
// Tokenless primitives
// ----------------------------------------------------------------------------

/// Whitespace and line-comment eater. Used between tokens.
fn wsc<'a>() -> impl Parser<'a, &'a str, (), Err<'a>> + Clone {
    let ws = any::<&'a str, Err<'a>>()
        .filter(|c: &char| c.is_whitespace())
        .ignored();
    let line_comment = just("//")
        .then(any().and_is(just('\n').not()).repeated())
        .ignored();
    choice((ws, line_comment)).repeated().ignored()
}

/// Pad a parser's trailing whitespace/comments.
fn tok<'a, O: 'a>(
    p: impl Parser<'a, &'a str, O, Err<'a>> + Clone + 'a,
) -> impl Parser<'a, &'a str, O, Err<'a>> + Clone + 'a {
    p.then_ignore(wsc())
}

/// Match a keyword with a word boundary on the trailing side — rejects
/// `justify` matching `just`. Consumes trailing ws/comments.
fn kw<'a>(keyword: &'static str) -> impl Parser<'a, &'a str, (), Err<'a>> + Clone {
    just(keyword)
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .ignored()
        .then_ignore(wsc())
}

/// Identifier: `[A-Za-z_][A-Za-z0-9_]*` — returned as an owned `String`.
fn ident<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
    any::<&'a str, Err<'a>>()
        .filter(|c: &char| c.is_ascii_alphabetic() || *c == '_')
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .repeated()
                .collect::<String>(),
        )
        .map(|(first, rest)| {
            let mut s = String::with_capacity(rest.len() + 1);
            s.push(first);
            s.push_str(&rest);
            s
        })
}

/// Globally-reserved words. Contextual words like `auth`, `accounts`,
/// `requires`, `ensures`, `effect`, `emits`, `modifies`, `let`, `include`,
/// `aborts_total`, `via`, `within`, `preserved_by`, `all`, `else` are NOT
/// reserved — they only act as keywords inside their respective clause
/// grammars (via leading `just(...)` matches). This lets users name fields
/// `accounts` or `effect` without colliding.
const KEYWORDS: &[&str] = &[
    "spec",
    "const",
    "type",
    "of",
    "handler",
    "property",
    "invariant",
    "cover",
    "liveness",
    "forall",
    "exists",
    "sum",
    "old",
    "implies",
    "and",
    "or",
    "not",
    "Map",
    "match",
    "with",
    "abort",
    "true",
    "false",
    "is",
    "mul_div_floor",
    "mul_div_ceil",
    "interface",
    "pragma",
    "let",
    "in",
];

fn non_keyword_ident<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
    ident().try_map(|s, span| {
        if KEYWORDS.contains(&s.as_str()) {
            Err(Rich::custom(span, format!("unexpected keyword `{}`", s)))
        } else {
            Ok(s)
        }
    })
}

/// Integer literal, optionally with underscore separators. Returns u128.
fn integer<'a>() -> impl Parser<'a, &'a str, u128, Err<'a>> + Clone {
    any::<&'a str, Err<'a>>()
        .filter(|c: &char| c.is_ascii_digit())
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_digit() || *c == '_')
                .repeated()
                .collect::<String>(),
        )
        .try_map(|(first, rest), span| {
            let mut s = String::with_capacity(rest.len() + 1);
            s.push(first);
            s.push_str(&rest);
            s.replace('_', "")
                .parse::<u128>()
                .map_err(|e| Rich::custom(span, e.to_string()))
        })
}

/// Double-quoted string literal. Simplified: no escape handling beyond `\\` and `\"`.
fn string_lit<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
    let escape = just('\\').ignore_then(choice((just('\\'), just('"'), just('n'), just('t'))));
    let char_inner = choice((
        escape.map(|c| match c {
            'n' => '\n',
            't' => '\t',
            other => other,
        }),
        any::<&'a str, Err<'a>>().filter(|c: &char| *c != '"' && *c != '\\'),
    ));
    just('"')
        .ignore_then(char_inner.repeated().collect::<String>())
        .then_ignore(just('"'))
}

/// Doc comment line: `/// ...\n`. Returns the text after `///`, trimmed.
fn doc_line<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
    just("///")
        .ignore_then(
            any::<&'a str, Err<'a>>()
                .and_is(just('\n').not())
                .repeated()
                .collect::<String>(),
        )
        .map(|s: String| s.trim().to_string())
}

/// Zero or more doc comments, joined into one string (newline-separated).
/// Consumes trailing whitespace/newlines between lines.
fn doc_comments<'a>() -> impl Parser<'a, &'a str, Option<String>, Err<'a>> + Clone {
    doc_line()
        .then_ignore(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_whitespace())
                .repeated(),
        )
        .repeated()
        .collect::<Vec<_>>()
        .map(|v: Vec<String>| {
            if v.is_empty() {
                None
            } else {
                Some(v.join("\n"))
            }
        })
}

// ----------------------------------------------------------------------------
// Type references: Named, Param, Map[N] T
// ----------------------------------------------------------------------------

fn type_ref<'a>() -> impl Parser<'a, &'a str, TypeRef, Err<'a>> + Clone {
    // Map[N] T — bounded map keyed by an index domain of size `N`.
    let map_ty = just("Map")
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .map(|(bound, inner_name)| TypeRef::Map {
            bound,
            inner: Box::new(TypeRef::Named(inner_name)),
        });

    // Fin[N] — bounded natural index domain.
    let fin_ty = just("Fin")
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(|bound| TypeRef::Fin { bound });

    // Simple type: a single ident.
    let simple = non_keyword_ident().map(TypeRef::Named);

    choice((map_ty, fin_ty, simple))
}

// ----------------------------------------------------------------------------
// Qualified path (no subscripts): `State.Active`, `Pool.Empty`
// ----------------------------------------------------------------------------

fn qualified_path<'a>() -> impl Parser<'a, &'a str, QualifiedPath, Err<'a>> + Clone {
    non_keyword_ident()
        .separated_by(just('.'))
        .at_least(1)
        .collect::<Vec<String>>()
        .map(QualifiedPath)
}

// ----------------------------------------------------------------------------
// Path with subscripts: `state.accounts[i].capital`
// ----------------------------------------------------------------------------

fn path<'a>() -> impl Parser<'a, &'a str, Path, Err<'a>> + Clone {
    let field_seg = just('.').ignore_then(ident()).map(PathSeg::Field);
    let index_seg = just('[')
        .ignore_then(ident())
        .then_ignore(just(']'))
        .map(PathSeg::Index);
    let seg = choice((field_seg, index_seg));
    ident()
        .then(seg.repeated().collect::<Vec<PathSeg>>())
        .map(|(root, segments)| Path { root, segments })
}

// ----------------------------------------------------------------------------
// Expressions (the main win of the typed AST)
// ----------------------------------------------------------------------------

fn expr<'a>() -> impl Parser<'a, &'a str, Node<Expr>, Err<'a>> + Clone {
    recursive(|expr| {
        let int = integer().map_with(|v, e| Node::new(Expr::Int(v), e.span().into_range()));

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

        // Parenthesized sub-expression
        let paren = just('(')
            .then_ignore(wsc())
            .ignore_then(expr.clone())
            .then_ignore(wsc())
            .then_ignore(just(')'))
            .map_with(|inner, e| Node::new(Expr::Paren(Box::new(inner)), e.span().into_range()));

        // mul_div_floor(a, b, d) / mul_div_ceil(a, b, d) — built-in triads
        // for scaled integer math. The VM has no native fixed-point; this is
        // the canonical `widen → multiply → floor-divide by scale` pattern.
        let mdf_args = |kw_name: &'static str, is_ceil: bool| {
            let e1 = expr.clone();
            let e2 = expr.clone();
            let e3 = expr.clone();
            just(kw_name)
                .then(
                    any::<&'a str, Err<'a>>()
                        .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                        .rewind()
                        .not(),
                )
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
                    let node = if is_ceil {
                        Expr::MulDivCeil {
                            a: Box::new(a),
                            b: Box::new(b),
                            d: Box::new(d),
                        }
                    } else {
                        Expr::MulDivFloor {
                            a: Box::new(a),
                            b: Box::new(b),
                            d: Box::new(d),
                        }
                    };
                    Node::new(node, e.span().into_range())
                })
        };
        let mul_div_floor_atom = mdf_args("mul_div_floor", false);
        let mul_div_ceil_atom = mdf_args("mul_div_ceil", true);

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
        // scrutinee and `with` keyword, producing a value.
        let match_arm_pat = non_keyword_ident()
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

        // atom — must stay under chumsky's `choice` arity limit; split.
        // `.boxed()` tames the type complexity that otherwise trips Apple's
        // linker on overlong symbol names.
        let group_a = choice((int, bool_lit, old, let_in, sum, quant)).boxed();
        let group_b = choice((mul_div_floor_atom, mul_div_ceil_atom, match_expr)).boxed();
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

// ----------------------------------------------------------------------------
// Top-level declarations
// ----------------------------------------------------------------------------

fn const_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("const")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(integer())
        .map(|(name, value)| TopItem::Const { name, value })
}

fn typed_field<'a>() -> impl Parser<'a, &'a str, TypedField, Err<'a>> + Clone {
    non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, ty)| TypedField { name, ty })
}

fn typed_field_list<'a>() -> impl Parser<'a, &'a str, Vec<TypedField>, Err<'a>> + Clone {
    typed_field()
        .then_ignore(wsc())
        .separated_by(just(',').then_ignore(wsc()))
        .allow_trailing()
        .collect::<Vec<TypedField>>()
}

// Record: type T = { field : Type, ... }
fn record_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("type")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(typed_field_list())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|(name, fields)| TopItem::Record(RecordDecl { name, fields }))
}

// Type alias: type Name = <type_ref>   (when `{` doesn't follow `=`)
// Order matters in the `choice()` at top_item: record_decl is tried first
// so `type T = { ... }` is consumed by record, not by this alias rule.
fn type_alias_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("type")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, target)| TopItem::TypeAlias(TypeAliasDecl { name, target }))
}

// ADT variant: `| Name [= code] ["desc"] [of { fields }]`
fn variant<'a>() -> impl Parser<'a, &'a str, Variant, Err<'a>> + Clone {
    let code = just('=')
        .then_ignore(wsc())
        .ignore_then(integer())
        .map(|n| n as u64)
        .then_ignore(wsc());
    let desc = string_lit().then_ignore(wsc());
    let fields = just("of")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(typed_field_list())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .then_ignore(wsc());

    just('|')
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then(code.or_not())
        .then(desc.or_not())
        .then(fields.or_not())
        .map(|(((name, code), description), fields)| Variant {
            name,
            code,
            description,
            fields: fields.unwrap_or_default(),
        })
}

// ADT: type T | V1 | V2 of { ... } | V3
fn adt_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("type")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then(
            variant()
                .then_ignore(wsc())
                .repeated()
                .at_least(1)
                .collect::<Vec<Variant>>(),
        )
        .map(|(name, variants)| TopItem::Adt(AdtDecl { name, variants }))
}

// Handler params: ML-currying `(i : T) (amount : U)` — each in its own parens.
fn handler_param<'a>() -> impl Parser<'a, &'a str, TypedField, Err<'a>> + Clone {
    just('(')
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .then_ignore(wsc())
        .then_ignore(just(')'))
        .map(|(name, ty)| TypedField { name, ty })
}

fn account_attr<'a>() -> impl Parser<'a, &'a str, AccountAttr, Err<'a>> + Clone {
    let pda_attr = just("pda")
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(
            choice((string_lit(), non_keyword_ident()))
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(AccountAttr::Pda);
    let type_attr = just("type")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(AccountAttr::Type);
    let authority_attr = just("authority")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(AccountAttr::Authority);
    let simple = non_keyword_ident().map(AccountAttr::Simple);
    choice((pda_attr, type_attr, authority_attr, simple))
}

fn account_descriptor<'a>() -> impl Parser<'a, &'a str, AccountDescriptor, Err<'a>> + Clone {
    non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(
            account_attr()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .at_least(1)
                .collect::<Vec<AccountAttr>>(),
        )
        .map(|(name, attrs)| AccountDescriptor { name, attrs })
}

fn effect_stmt<'a>() -> impl Parser<'a, &'a str, EffectStmt, Err<'a>> + Clone {
    let op = choice((
        just("+=").to(EffectOp::Add),
        just("-=").to(EffectOp::Sub),
        just(":=").to(EffectOp::Set),
        just('=').to(EffectOp::Set),
    ));
    path()
        .then_ignore(wsc())
        .then(op)
        .then_ignore(wsc())
        .then(expr())
        .then_ignore(wsc())
        .map(|((lhs, op), rhs)| EffectStmt { lhs, op, rhs })
}

fn handler_clause<'a>() -> impl Parser<'a, &'a str, HandlerClause, Err<'a>> + Clone {
    let auth = just("auth")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Auth);

    let accounts = just("accounts")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            account_descriptor()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<AccountDescriptor>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(HandlerClause::Accounts);

    let requires = just("requires")
        .then_ignore(wsc())
        .ignore_then(expr())
        .then_ignore(wsc())
        .then(
            just("else")
                .then_ignore(wsc())
                .ignore_then(non_keyword_ident())
                .or_not(),
        )
        .map(|(guard, on_fail)| HandlerClause::Requires { guard, on_fail });

    let ensures = just("ensures")
        .then_ignore(wsc())
        .ignore_then(expr())
        .map(HandlerClause::Ensures);

    let modifies = just("modifies")
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(HandlerClause::Modifies);

    let let_c = just("let")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(expr())
        .map(|(name, value)| HandlerClause::Let { name, value });

    let effect = just("effect")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            effect_stmt()
                .map_with(|s, e| Node::new(s, e.span().into_range()))
                .repeated()
                .collect::<Vec<Node<EffectStmt>>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(HandlerClause::Effect);

    let emits = just("emits")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Emits);

    // branch {
    //   case <expr>: abort ErrName
    //   case <expr>: effect { ... }
    //   otherwise:   abort ErrName
    // }
    let match_body = choice((
        // abort ErrName
        kw("abort")
            .ignore_then(non_keyword_ident())
            .map(MatchBody::Abort),
        // effect { ... }
        kw("effect")
            .ignore_then(just('{'))
            .then_ignore(wsc())
            .ignore_then(
                effect_stmt()
                    .map_with(|s, e| Node::new(s, e.span().into_range()))
                    .repeated()
                    .collect::<Vec<Node<EffectStmt>>>(),
            )
            .then_ignore(wsc())
            .then_ignore(just('}'))
            .map(MatchBody::Effect),
    ));

    // ML-style arms:
    //   | <expr> => <body>
    //   | _      => <body>     (wildcard / fallthrough)
    let wildcard_guard = just('_')
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .to(None::<Node<Expr>>);
    let arm_guard = choice((wildcard_guard, expr().map(Some)));
    let match_arm = just('|')
        .then_ignore(wsc())
        .ignore_then(arm_guard)
        .then_ignore(wsc())
        .then_ignore(just("=>"))
        .then_ignore(wsc())
        .then(match_body.clone())
        .map(|(guard, body)| {
            let label = if guard.is_some() {
                String::new()
            } else {
                "otherwise".to_string()
            };
            MatchArm { guard, body, label }
        });

    let match_c = kw("match")
        .ignore_then(
            match_arm
                .then_ignore(wsc())
                .repeated()
                .at_least(1)
                .collect::<Vec<MatchArm>>(),
        )
        .map(|arms| {
            // Assign ordinal labels where the user didn't supply one.
            let mut out = Vec::with_capacity(arms.len());
            for (i, mut arm) in arms.into_iter().enumerate() {
                if arm.label.is_empty() {
                    arm.label = format!("case_{}", i);
                }
                out.push(arm);
            }
            HandlerClause::Match(MatchClause { arms: out })
        });

    // Legacy sugar: `takes { x : T, ... }` or `takes x : T`.
    let takes_block_form = just('{')
        .then_ignore(wsc())
        .ignore_then(typed_field_list().or_not())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|fs| fs.unwrap_or_default());
    let takes_inline_form = non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, ty)| vec![TypedField { name, ty }]);
    let takes = kw("takes")
        .ignore_then(choice((takes_block_form, takes_inline_form)))
        .map(HandlerClause::Takes);

    // transfers { from A to B [amount X] [authority Y] ... }
    let transfer_amount = choice((
        integer().map(TransferAmount::Literal),
        path().map(TransferAmount::Path),
    ));
    let transfer_clause = just("from")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just("to"))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then(
            just("amount")
                .then_ignore(wsc())
                .ignore_then(transfer_amount)
                .then_ignore(wsc())
                .or_not(),
        )
        .then(
            just("authority")
                .then_ignore(wsc())
                .ignore_then(non_keyword_ident())
                .then_ignore(wsc())
                .or_not(),
        )
        .map(|(((from, to), amount), authority)| TransferClause {
            from,
            to,
            amount,
            authority,
        });

    let transfers = just("transfers")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            transfer_clause
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<TransferClause>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(HandlerClause::Transfers);

    let aborts_total = just("aborts_total").to(HandlerClause::AbortsTotal);
    let invariant = just("invariant")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Invariant);
    let include = just("include")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Include);

    // call Target.handler(name = expr, name = expr, ...)
    let call_kw_arg = non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(expr())
        .map(|(name, value)| CallArg { name, value });

    let call_c = just("call")
        .then_ignore(wsc())
        .ignore_then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just('('))
        .then_ignore(wsc())
        .then(
            call_kw_arg
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .allow_trailing()
                .collect::<Vec<CallArg>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(')'))
        .map(|(target, args)| HandlerClause::Call(CallExpr { target, args }));

    // `choice()` has an arity limit; split into groups.
    let grp_a = choice((auth, accounts, requires, ensures, modifies, let_c, effect));
    let grp_b = choice((transfers, takes, emits, aborts_total, invariant, include));
    let grp_c = choice((match_c, call_c));
    choice((grp_a, grp_b, grp_c))
}

// handler name (params)* : Pre -> Post { clauses }
fn handler_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let transition = just(':')
        .then_ignore(wsc())
        .ignore_then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just("->"))
        .then_ignore(wsc())
        .then(qualified_path());

    doc_comments()
        .then_ignore(kw("handler"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then(
            handler_param()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<TypedField>>(),
        )
        .then(transition.or_not())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            handler_clause()
                .map_with(|c, e| Node::new(c, e.span().into_range()))
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<Node<HandlerClause>>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((((doc, name), params), trans), clauses)| {
            let (pre, post) = match trans {
                Some((p, q)) => (Some(p), Some(q)),
                None => (None, None),
            };
            TopItem::Handler(HandlerDecl {
                name,
                doc,
                params,
                pre,
                post,
                clauses,
            })
        })
}

// property name : expr preserved_by all | [a, b, ...]
fn property_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let preserved = just("preserved_by").then_ignore(wsc()).ignore_then(choice((
        just("all").to(PreservedBy::All),
        just('[')
            .then_ignore(wsc())
            .ignore_then(
                non_keyword_ident()
                    .then_ignore(wsc())
                    .separated_by(just(',').then_ignore(wsc()))
                    .collect::<Vec<String>>(),
            )
            .then_ignore(wsc())
            .then_ignore(just(']'))
            .map(PreservedBy::Some),
    )));

    doc_comments()
        .then_ignore(kw("property"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(expr())
        .then_ignore(wsc())
        .then(preserved)
        .map(|(((doc, name), body), preserved_by)| {
            TopItem::Property(PropertyDecl {
                name,
                doc,
                body,
                preserved_by,
            })
        })
}

// cover name [a, b, c]
fn cover_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("cover")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('['))
        .then_ignore(wsc())
        .then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(|(name, trace)| {
            TopItem::Cover(CoverDecl {
                name,
                traces: vec![trace],
                reachable: Vec::new(),
            })
        })
}

// liveness name : From ~> To via [...] within N
fn liveness_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("liveness")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just("~>"))
        .then_ignore(wsc())
        .then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just("via"))
        .then_ignore(wsc())
        .then_ignore(just('['))
        .then_ignore(wsc())
        .then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .then_ignore(wsc())
        .then_ignore(just("within"))
        .then_ignore(wsc())
        .then(integer())
        .map(|((((name, from_state), to_state), via), within)| {
            TopItem::Liveness(LivenessDecl {
                name,
                from_state,
                to_state,
                via,
                within: within as u64,
            })
        })
}

// invariant name : expr  OR  invariant name "description"
fn invariant_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("invariant")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then(choice((
            just(':')
                .then_ignore(wsc())
                .ignore_then(expr())
                .map(InvariantBody::Expr),
            string_lit().map(InvariantBody::Description),
        )))
        .map(|(name, body)| TopItem::Invariant(InvariantDecl { name, body }))
}

// program_id "base58..."
fn program_id_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("program_id")
        .ignore_then(string_lit())
        .map(TopItem::ProgramId)
}

// pda name [seed1, seed2, ...]
fn pda_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let seed = choice((
        string_lit().map(PdaSeed::Literal),
        non_keyword_ident().map(PdaSeed::Ident),
    ));
    kw("pda")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('['))
        .then_ignore(wsc())
        .then(
            seed.then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .at_least(1)
                .collect::<Vec<PdaSeed>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(|(name, seeds)| TopItem::Pda(PdaDecl { name, seeds }))
}

// event name { field : Type, ... }
fn event_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("event")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(typed_field_list().or_not())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|(name, fields)| {
            TopItem::Event(EventDecl {
                name,
                fields: fields.unwrap_or_default(),
            })
        })
}

// environment name { mutates field : T | constraint expr }
fn environment_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let mutates = kw("mutates")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .map(|(field, ty)| EnvClause::Mutates { field, ty });

    let constraint = kw("constraint")
        .ignore_then(expr())
        .map(EnvClause::Constraint);

    let clause = choice((mutates, constraint)).map_with(|c, e| Node::new(c, e.span().into_range()));

    kw("environment")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            clause
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<Node<EnvClause>>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|(name, clauses)| TopItem::Environment(EnvironmentDecl { name, clauses }))
}

// ----------------------------------------------------------------------------
// sBPF constructs: pubkey, errors (top-level sugar), instruction block
// ----------------------------------------------------------------------------

// pubkey NAME [c0, c1, c2, c3]
fn pubkey_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    just("pubkey")
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('['))
        .then_ignore(wsc())
        .then(
            integer()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .at_least(1)
                .collect::<Vec<u128>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(|(name, chunks)| TopItem::Pubkey(PubkeyDecl { name, chunks }))
}

/// One entry in an `errors [...]` list. Accepts either:
///   Name
///   Name = N
///   Name = N "desc"
fn error_entry<'a>() -> impl Parser<'a, &'a str, ErrorEntry, Err<'a>> + Clone {
    let tail = just('=')
        .then_ignore(wsc())
        .ignore_then(integer())
        .then_ignore(wsc())
        .then(string_lit().then_ignore(wsc()).or_not())
        .map(|(code, desc)| (Some(code as u64), desc));
    non_keyword_ident()
        .then_ignore(wsc())
        .then(tail.or_not())
        .map(|(name, tail)| {
            let (code, description) = tail.unwrap_or((None, None));
            ErrorEntry {
                name,
                code,
                description,
            }
        })
}

// errors [ Name = N "desc", Name = M, ... ]
fn errors_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    just("errors")
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(
            error_entry()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .at_least(1)
                .allow_trailing()
                .collect::<Vec<ErrorEntry>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(TopItem::Errors)
}

/// Signed integer (for layout offsets).
fn signed_integer<'a>() -> impl Parser<'a, &'a str, i64, Err<'a>> + Clone {
    just('-')
        .or_not()
        .then(integer())
        .try_map(|(sign, v), span| {
            if v > i64::MAX as u128 {
                return Err(Rich::custom(span, "integer overflow for i64 offset"));
            }
            let as_i = v as i64;
            Ok(if sign.is_some() { -as_i } else { as_i })
        })
}

// layout_field: name : Type @ [-]offset ["desc"]
fn layout_field<'a>() -> impl Parser<'a, &'a str, LayoutField, Err<'a>> + Clone {
    non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('@'))
        .then_ignore(wsc())
        .then(signed_integer())
        .then_ignore(wsc())
        .then(string_lit().or_not())
        .map(|(((name, field_type), offset), description)| LayoutField {
            name,
            field_type,
            offset,
            description,
        })
}

fn input_layout_block<'a>() -> impl Parser<'a, &'a str, InstructionItem, Err<'a>> + Clone {
    just("input_layout")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            layout_field()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<LayoutField>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(InstructionItem::InputLayout)
}

fn insn_layout_block<'a>() -> impl Parser<'a, &'a str, InstructionItem, Err<'a>> + Clone {
    just("insn_layout")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            layout_field()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<LayoutField>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(InstructionItem::InsnLayout)
}

// guard NAME { checks? error NAME fuel? }
fn guard_decl<'a>() -> impl Parser<'a, &'a str, GuardDecl, Err<'a>> + Clone {
    #[derive(Clone)]
    enum Item {
        Checks(Node<Expr>),
        Error(String),
        Fuel(u64),
    }
    let checks = just("checks")
        .then_ignore(wsc())
        .ignore_then(expr())
        .map(Item::Checks);
    let error = just("error")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(Item::Error);
    let fuel = just("fuel")
        .then_ignore(wsc())
        .ignore_then(integer())
        .map(|n| Item::Fuel(n as u64));
    let item = choice((checks, error, fuel)).boxed();

    doc_comments()
        .then_ignore(just("guard"))
        .then_ignore(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(item.then_ignore(wsc()).repeated().collect::<Vec<Item>>())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, name), items)| {
            let mut checks = None;
            let mut error = String::new();
            let mut fuel = None;
            for it in items {
                match it {
                    Item::Checks(e) => checks = Some(e),
                    Item::Error(s) => error = s,
                    Item::Fuel(n) => fuel = Some(n),
                }
            }
            GuardDecl {
                name,
                doc,
                checks,
                error,
                fuel,
            }
        })
}

// cpi_field: ident (ident | [ident_list])
fn cpi_field<'a>() -> impl Parser<'a, &'a str, (String, String), Err<'a>> + Clone {
    let list_rhs = just('[')
        .then_ignore(wsc())
        .ignore_then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .at_least(1)
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(|xs| format!("[{}]", xs.join(", ")));
    let single_rhs = non_keyword_ident();
    non_keyword_ident()
        .then_ignore(wsc())
        .then(choice((list_rhs, single_rhs)))
        .map(|(k, v)| (k, v))
}

// sbpf property clause: expr / preserved_by / scope / flow / cpi / after / exit
fn sbpf_prop_clause<'a>() -> impl Parser<'a, &'a str, SbpfPropClause, Err<'a>> + Clone {
    let expr_c = just("expr")
        .then_ignore(wsc())
        .ignore_then(expr())
        .map(SbpfPropClause::Expr);

    let preserved = just("preserved_by")
        .then_ignore(wsc())
        .ignore_then(choice((
            just("all").to(PreservedBy::All),
            just('[')
                .then_ignore(wsc())
                .ignore_then(
                    non_keyword_ident()
                        .then_ignore(wsc())
                        .separated_by(just(',').then_ignore(wsc()))
                        .collect::<Vec<String>>(),
                )
                .then_ignore(wsc())
                .then_ignore(just(']'))
                .map(PreservedBy::Some),
        )))
        .map(SbpfPropClause::PreservedBy);

    // scope (guards | [names])
    let scope = just("scope")
        .then_ignore(wsc())
        .ignore_then(choice((
            just("guards").to(Vec::<String>::new()),
            just('[')
                .then_ignore(wsc())
                .ignore_then(
                    non_keyword_ident()
                        .then_ignore(wsc())
                        .separated_by(just(',').then_ignore(wsc()))
                        .collect::<Vec<String>>(),
                )
                .then_ignore(wsc())
                .then_ignore(just(']')),
        )))
        .map(SbpfPropClause::Scope);

    // flow IDENT (from seeds [...] | through [...])
    let from_seeds = just("from")
        .then_ignore(wsc())
        .then_ignore(just("seeds"))
        .then_ignore(wsc())
        .then_ignore(just('['))
        .then_ignore(wsc())
        .ignore_then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(SbpfFlowKind::FromSeeds);
    let through = just("through")
        .then_ignore(wsc())
        .then_ignore(just('['))
        .then_ignore(wsc())
        .ignore_then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(SbpfFlowKind::Through);
    let flow = just("flow")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then(choice((from_seeds, through)))
        .map(|(target, kind)| SbpfPropClause::Flow { target, kind });

    // cpi PROG INSTR { fields }
    let cpi = just("cpi")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            cpi_field()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<(String, String)>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((program, instruction), fields)| SbpfPropClause::Cpi {
            program,
            instruction,
            fields,
        });

    // after all guards
    let after = just("after")
        .then_ignore(wsc())
        .then_ignore(just("all"))
        .then_ignore(wsc())
        .then_ignore(just("guards"))
        .to(SbpfPropClause::AfterAllGuards);

    // exit N
    let exit = just("exit")
        .then_ignore(wsc())
        .ignore_then(integer())
        .map(|n| SbpfPropClause::Exit(n as u64));

    let grp_a = choice((expr_c, preserved, scope)).boxed();
    let grp_b = choice((flow, cpi, after, exit)).boxed();
    choice((grp_a, grp_b))
}

// sBPF property block:
// property NAME { scope / flow / cpi / after + exit / expr + preserved_by }
fn sbpf_property_decl<'a>() -> impl Parser<'a, &'a str, SbpfPropertyDecl, Err<'a>> + Clone {
    doc_comments()
        .then_ignore(just("property"))
        .then_ignore(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            sbpf_prop_clause()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<SbpfPropClause>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, name), clauses)| SbpfPropertyDecl { name, doc, clauses })
}

// instruction_item: discriminant / entry / const / errors / input_layout /
//                   insn_layout / guard / sbpf property
fn instruction_item<'a>() -> impl Parser<'a, &'a str, InstructionItem, Err<'a>> + Clone {
    let discriminant = just("discriminant")
        .then_ignore(wsc())
        .ignore_then(choice((
            integer().map(|n| n.to_string()),
            non_keyword_ident(),
        )))
        .map(InstructionItem::Discriminant);
    let entry = just("entry")
        .then_ignore(wsc())
        .ignore_then(integer())
        .map(|n| InstructionItem::Entry(n as u64));
    let const_c = just("const")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(integer())
        .map(|(name, value)| InstructionItem::Const { name, value });
    let errors_c = just("errors")
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(
            error_entry()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .at_least(1)
                .allow_trailing()
                .collect::<Vec<ErrorEntry>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(InstructionItem::Errors);

    let guard = guard_decl().map(InstructionItem::Guard);
    let prop = sbpf_property_decl().map(InstructionItem::SbpfProperty);

    let grp_a = choice((discriminant, entry, const_c, errors_c)).boxed();
    let grp_b = choice((input_layout_block(), insn_layout_block())).boxed();
    let grp_c = choice((guard, prop)).boxed();
    choice((grp_a, grp_b, grp_c))
}

// instruction NAME { instruction_items }
fn instruction_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    doc_comments()
        .then_ignore(just("instruction"))
        .then_ignore(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            instruction_item()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<InstructionItem>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, name), items)| TopItem::Instruction(InstructionDecl { name, doc, items }))
}

// ----------------------------------------------------------------------------
// interface Name { program_id "...", upstream { ... }, handler h(args) { ... } }
// ----------------------------------------------------------------------------

/// Internal: items that appear at the top of an `interface` block.
/// Folded into `InterfaceDecl` by the decl combinator.
enum InterfaceItem {
    ProgramId(String),
    Upstream(UpstreamDecl),
    Handler(InterfaceHandlerDecl),
}

/// Internal: items that appear inside an `upstream { ... }` block.
enum UpstreamItem {
    Package(String),
    Version(String),
    Source(String),
    BinaryHash(String),
    IdlHash(String),
    VerifiedWith(Vec<String>),
    VerifiedAt(String),
}

// upstream { package "...", version "...", binary_hash "...", ... }
fn upstream_block<'a>() -> impl Parser<'a, &'a str, UpstreamDecl, Err<'a>> + Clone {
    let package = kw("package")
        .ignore_then(string_lit())
        .map(UpstreamItem::Package);
    let version = kw("version")
        .ignore_then(string_lit())
        .map(UpstreamItem::Version);
    let source = kw("source")
        .ignore_then(string_lit())
        .map(UpstreamItem::Source);
    let binary_hash = kw("binary_hash")
        .ignore_then(string_lit())
        .map(UpstreamItem::BinaryHash);
    let idl_hash = kw("idl_hash")
        .ignore_then(string_lit())
        .map(UpstreamItem::IdlHash);
    let verified_with = kw("verified_with")
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(
            string_lit()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .allow_trailing()
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(UpstreamItem::VerifiedWith);
    let verified_at = kw("verified_at")
        .ignore_then(string_lit())
        .map(UpstreamItem::VerifiedAt);

    let item = choice((
        package,
        version,
        source,
        binary_hash,
        idl_hash,
        verified_with,
        verified_at,
    ));

    kw("upstream")
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            item.then_ignore(wsc())
                .repeated()
                .collect::<Vec<UpstreamItem>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|items| {
            let mut u = UpstreamDecl::default();
            for it in items {
                match it {
                    UpstreamItem::Package(s) => u.package = Some(s),
                    UpstreamItem::Version(s) => u.version = Some(s),
                    UpstreamItem::Source(s) => u.source = Some(s),
                    UpstreamItem::BinaryHash(s) => u.binary_hash = Some(s),
                    UpstreamItem::IdlHash(s) => u.idl_hash = Some(s),
                    UpstreamItem::VerifiedWith(v) => u.verified_with = v,
                    UpstreamItem::VerifiedAt(s) => u.verified_at = Some(s),
                }
            }
            u
        })
}

// Clauses inside an interface-handler body: discriminant, accounts, requires, ensures.
fn interface_handler_clause<'a>(
) -> impl Parser<'a, &'a str, InterfaceHandlerClause, Err<'a>> + Clone {
    let discriminant = kw("discriminant")
        .ignore_then(choice((string_lit(), non_keyword_ident())))
        .map(InterfaceHandlerClause::Discriminant);

    let accounts = just("accounts")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            account_descriptor()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<AccountDescriptor>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(InterfaceHandlerClause::Accounts);

    let requires = just("requires")
        .then_ignore(wsc())
        .ignore_then(expr())
        .then_ignore(wsc())
        .then(
            just("else")
                .then_ignore(wsc())
                .ignore_then(non_keyword_ident())
                .or_not(),
        )
        .map(|(guard, on_fail)| InterfaceHandlerClause::Requires { guard, on_fail });

    let ensures = just("ensures")
        .then_ignore(wsc())
        .ignore_then(expr())
        .map(InterfaceHandlerClause::Ensures);

    choice((discriminant, accounts, requires, ensures))
}

// handler h(params)* { discriminant, accounts, requires, ensures }  — inside an interface block.
fn interface_handler_decl<'a>() -> impl Parser<'a, &'a str, InterfaceHandlerDecl, Err<'a>> + Clone {
    doc_comments()
        .then_ignore(kw("handler"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then(
            handler_param()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<TypedField>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            interface_handler_clause()
                .map_with(|c, e| Node::new(c, e.span().into_range()))
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<Node<InterfaceHandlerClause>>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|(((doc, name), params), clauses)| InterfaceHandlerDecl {
            name,
            doc,
            params,
            clauses,
        })
}

fn interface_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let program_id = kw("program_id")
        .ignore_then(string_lit())
        .map(InterfaceItem::ProgramId);
    let upstream = upstream_block().map(InterfaceItem::Upstream);
    let handler = interface_handler_decl().map(InterfaceItem::Handler);

    let item = choice((program_id, upstream, handler));

    doc_comments()
        .then_ignore(kw("interface"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            item.then_ignore(wsc())
                .repeated()
                .collect::<Vec<InterfaceItem>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, name), items)| {
            let mut program_id = None;
            let mut upstream = None;
            let mut handlers = Vec::new();
            for it in items {
                match it {
                    InterfaceItem::ProgramId(s) => program_id = Some(s),
                    InterfaceItem::Upstream(u) => upstream = Some(u),
                    InterfaceItem::Handler(h) => handlers.push(h),
                }
            }
            TopItem::Interface(InterfaceDecl {
                name,
                doc,
                program_id,
                upstream,
                handlers,
            })
        })
}

// ----------------------------------------------------------------------------
// pragma <name> { <platform_item>* } — platform-specific namespace.
// ----------------------------------------------------------------------------

/// Items allowed inside a pragma body. Restricted whitelist — keeps the
/// grammar tight and emits clearer errors on misplaced constructs. Extend
/// as new platforms or pragma content arrives.
fn pragma_item<'a>() -> impl Parser<'a, &'a str, Node<TopItem>, Err<'a>> + Clone {
    choice((
        const_decl(),
        pubkey_decl(),
        instruction_decl(),
        errors_decl(),
    ))
    .map_with(|item, e| Node::new(item, e.span().into_range()))
}

fn pragma_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    doc_comments()
        .then_ignore(kw("pragma"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(
            pragma_item()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<Node<TopItem>>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, name), items)| TopItem::Pragma(PragmaDecl { name, doc, items }))
}

// Top-level item: priority-ordered choice.
// record_decl must precede adt_decl (PEG-style backtracking via .or).
fn top_item<'a>() -> impl Parser<'a, &'a str, Node<TopItem>, Err<'a>> + Clone {
    // Priority matters for `type` forms — try record (`type T = { ... }`)
    // first, then type alias (`type T = <type_ref>`). ADT (`type T | ...`)
    // uses a different shape after the name and can be disambiguated.
    let group_a = choice((
        const_decl(),
        record_decl(),
        type_alias_decl(),
        adt_decl(),
        handler_decl(),
        property_decl(),
        cover_decl(),
        liveness_decl(),
        invariant_decl(),
    ));
    let group_b = choice((
        pda_decl(),
        event_decl(),
        environment_decl(),
        program_id_decl(),
    ));
    // Note: `pubkey`, `instruction`, `assembly`, and the `errors [...]`
    // sugar are platform-specific and only parse inside
    // `pragma sbpf { ... }`. Use `type Error | A | B | ...` for errors at
    // the core-DSL level. The platform-agnostic top level is the point.
    let group_c = choice((interface_decl(), pragma_decl()));
    choice((group_a, group_b, group_c)).map_with(|item, e| Node::new(item, e.span().into_range()))
}

pub fn spec_parser<'a>() -> impl Parser<'a, &'a str, Spec, Err<'a>> + Clone {
    wsc()
        .ignore_then(kw("spec"))
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then(
            top_item()
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<Node<TopItem>>>(),
        )
        .then_ignore(wsc())
        .map(|(name, items)| Spec { name, items })
}

/// Parse a `.qedspec` source string into a typed AST.
pub fn parse(src: &str) -> Result<Spec, Vec<Rich<'_, char>>> {
    spec_parser().parse(src).into_result()
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Spec {
        match parse(src) {
            Ok(s) => s,
            Err(errs) => {
                for e in &errs {
                    eprintln!("parse error: {:?}", e);
                }
                panic!("parse failed");
            }
        }
    }

    #[test]
    fn parses_spec_header() {
        let s = parse_ok("spec Foo");
        assert_eq!(s.name, "Foo");
        assert!(s.items.is_empty());
    }

    #[test]
    fn parses_const() {
        let s = parse_ok("spec T\nconst MAX = 1_024");
        assert_eq!(s.items.len(), 1);
        match &s.items[0].node {
            TopItem::Const { name, value } => {
                assert_eq!(name, "MAX");
                assert_eq!(*value, 1024);
            }
            other => panic!("expected Const, got {:?}", other),
        }
    }

    #[test]
    fn parses_record() {
        let src = "spec T\ntype Account = {\n  active : U8,\n  capital : U128,\n}";
        let s = parse_ok(src);
        match &s.items[0].node {
            TopItem::Record(r) => {
                assert_eq!(r.name, "Account");
                assert_eq!(r.fields.len(), 2);
                assert_eq!(r.fields[0].name, "active");
                match &r.fields[0].ty {
                    TypeRef::Named(n) => assert_eq!(n, "U8"),
                    o => panic!("expected Named, got {:?}", o),
                }
            }
            o => panic!("expected Record, got {:?}", o),
        }
    }

    #[test]
    fn parses_adt_with_map() {
        let src = r#"spec T
const MAX = 8
type Account = { capital : U128, }
type State
  | Active of { V : U128, accounts : Map[MAX] Account, }
  | Halted
"#;
        let s = parse_ok(src);
        // items: [const, record, adt]
        assert_eq!(s.items.len(), 3);
        match &s.items[2].node {
            TopItem::Adt(a) => {
                assert_eq!(a.name, "State");
                assert_eq!(a.variants.len(), 2);
                assert_eq!(a.variants[0].name, "Active");
                assert_eq!(a.variants[0].fields.len(), 2);
                match &a.variants[0].fields[1].ty {
                    TypeRef::Map { bound, inner } => {
                        assert_eq!(bound, "MAX");
                        match inner.as_ref() {
                            TypeRef::Named(n) => assert_eq!(n, "Account"),
                            o => panic!("inner: {:?}", o),
                        }
                    }
                    o => panic!("expected Map, got {:?}", o),
                }
                assert_eq!(a.variants[1].name, "Halted");
            }
            o => panic!("expected Adt, got {:?}", o),
        }
    }

    #[test]
    fn parses_handler_with_subscripts() {
        let src = r#"spec T
const MAX = 8
type Account = { capital : U128, }
type State | Active of { V : U128, accounts : Map[MAX] Account, }

handler deposit (i : AccountIdx) (amount : U128) : State.Active -> State.Active {
  auth authority
  requires state.accounts[i].capital >= 0
  effect {
    V += amount
    accounts[i].capital += amount
  }
}
"#;
        let s = parse_ok(src);
        let handler = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Handler(h) => Some(h),
                _ => None,
            })
            .expect("handler");
        assert_eq!(handler.name, "deposit");
        assert_eq!(handler.params.len(), 2);
        assert_eq!(handler.params[0].name, "i");
        assert!(handler.pre.is_some());

        // One effect clause with two stmts
        let effect_clauses: Vec<_> = handler
            .clauses
            .iter()
            .filter_map(|c| match &c.node {
                HandlerClause::Effect(stmts) => Some(stmts),
                _ => None,
            })
            .collect();
        assert_eq!(effect_clauses.len(), 1);
        let stmts = effect_clauses[0];
        assert_eq!(stmts.len(), 2);
        // Second stmt: accounts[i].capital += amount
        let s2 = &stmts[1].node;
        assert_eq!(s2.lhs.root, "accounts");
        assert_eq!(s2.lhs.segments.len(), 2);
        match &s2.lhs.segments[0] {
            PathSeg::Index(n) => assert_eq!(n, "i"),
            o => panic!("expected Index, got {:?}", o),
        }
        match &s2.lhs.segments[1] {
            PathSeg::Field(n) => assert_eq!(n, "capital"),
            o => panic!("expected Field, got {:?}", o),
        }
        assert_eq!(s2.op, EffectOp::Add);
    }

    #[test]
    fn parses_property_with_sum() {
        let src = r#"spec T
const MAX = 8
type Account = { capital : U128, }
type State | Active of { V : U128, accounts : Map[MAX] Account, }

property conservation :
  state.V >= sum i : AccountIdx, state.accounts[i].capital
  preserved_by all
"#;
        let s = parse_ok(src);
        let prop = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Property(p) => Some(p),
                _ => None,
            })
            .expect("property");
        assert_eq!(prop.name, "conservation");
        assert!(matches!(prop.preserved_by, PreservedBy::All));
        // Body should be a Cmp with a Sum on the RHS
        match &prop.body.node {
            Expr::Cmp { op, rhs, .. } => {
                assert_eq!(*op, CmpOp::Ge);
                match &rhs.node {
                    Expr::Sum {
                        binder, binder_ty, ..
                    } => {
                        assert_eq!(binder, "i");
                        assert_eq!(binder_ty, "AccountIdx");
                    }
                    o => panic!("expected Sum, got {:?}", o),
                }
            }
            o => panic!("expected Cmp, got {:?}", o),
        }
    }

    #[test]
    fn parses_full_percolator_spec() {
        const SRC: &str = include_str!("../../../examples/rust/percolator/percolator.qedspec");
        let s = parse_ok(SRC);
        assert_eq!(s.name, "Percolator");

        // Quick structural sanity check.
        let counts = s
            .items
            .iter()
            .map(|i| match &i.node {
                TopItem::Const { .. } => "const",
                TopItem::Record(_) => "record",
                TopItem::Adt(_) => "adt",
                TopItem::Handler(_) => "handler",
                TopItem::Property(_) => "property",
                TopItem::Cover(_) => "cover",
                TopItem::Liveness(_) => "liveness",
                TopItem::Invariant(_) => "invariant",
                TopItem::Pda(_) => "pda",
                TopItem::Event(_) => "event",
                TopItem::Environment(_) => "environment",
                TopItem::ProgramId(_) => "program_id",
                TopItem::TypeAlias(_) => "type_alias",
                TopItem::Pubkey(_) => "pubkey",
                TopItem::Errors(_) => "errors",
                TopItem::Instruction(_) => "instruction",
                TopItem::Interface(_) => "interface",
                TopItem::Pragma(_) => "pragma",
            })
            .fold(
                std::collections::BTreeMap::<&str, usize>::new(),
                |mut m, k| {
                    *m.entry(k).or_default() += 1;
                    m
                },
            );

        assert_eq!(counts.get("const"), Some(&4), "consts: {:?}", counts);
        assert_eq!(counts.get("record"), Some(&1));
        assert_eq!(counts.get("adt"), Some(&2)); // State + Error
        assert_eq!(counts.get("handler"), Some(&15));
        assert_eq!(counts.get("property"), Some(&3));
        assert_eq!(counts.get("cover"), Some(&2));
        assert_eq!(counts.get("liveness"), Some(&1));
    }

    #[test]
    fn parses_record_update_and_is_check() {
        let src = r#"
spec T
const MAX = 8
type Account
  | Inactive
  | Active of {
      capital : U128,
      pnl     : I128,
    }

type State
  | Active of { accounts : Map[MAX] Account, }

handler h (i : U16) (amount : U128) : State.Active -> State.Active {
  requires state.accounts[i] is .Active else SlotInactive
  effect {
    accounts[i] := match state.accounts[i] with
      | Active a => .Active { a with capital := a.capital + amount }
      | Inactive => .Inactive
  }
}
"#;
        let s = parse_ok(src);
        let h = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Handler(h) => Some(h),
                _ => None,
            })
            .unwrap();
        // requires: IsVariant
        let req = h
            .clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Requires { guard, .. } => Some(guard),
                _ => None,
            })
            .unwrap();
        match &req.node {
            Expr::IsVariant { variant, .. } => assert_eq!(variant, "Active"),
            o => panic!("expected IsVariant, got {:?}", o),
        }
        // effect RHS: Match containing RecordUpdate on the Active arm
        let eff = h
            .clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Effect(s) => Some(s),
                _ => None,
            })
            .unwrap();
        match &eff[0].node.rhs.node {
            Expr::Match { arms, .. } => match &arms[0].body.node {
                Expr::Ctor {
                    variant: v,
                    payload,
                } => {
                    assert_eq!(v, "Active");
                    let p = payload.as_ref().expect("payload");
                    match &p.node {
                        Expr::RecordUpdate { updates, .. } => {
                            assert_eq!(updates.len(), 1);
                            assert_eq!(updates[0].0, "capital");
                        }
                        o => panic!("expected RecordUpdate payload, got {:?}", o),
                    }
                }
                o => panic!("expected Ctor in Active arm, got {:?}", o),
            },
            o => panic!("expected Match on effect RHS, got {:?}", o),
        }
    }

    #[test]
    fn parses_ctor_in_effect() {
        let src = r#"
spec T
const MAX = 8
type Account
  | Inactive
  | Active of {
      capital : U128,
      pnl     : I128,
    }

type State
  | Active of { accounts : Map[MAX] Account, }

handler reset_slot (i : U16) : State.Active -> State.Active {
  auth authority
  effect {
    accounts[i] := .Inactive
  }
}

handler init_slot (i : U16) : State.Active -> State.Active {
  auth authority
  effect {
    accounts[i] := .Active { capital := 0, pnl := 0 }
  }
}
"#;
        let s = parse_ok(src);
        let reset = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Handler(h) if h.name == "reset_slot" => Some(h),
                _ => None,
            })
            .unwrap();
        let reset_effect = reset
            .clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Effect(stmts) => Some(stmts),
                _ => None,
            })
            .unwrap();
        match &reset_effect[0].node.rhs.node {
            Expr::Ctor { variant, payload } => {
                assert_eq!(variant, "Inactive");
                assert!(payload.is_none());
            }
            o => panic!("expected Ctor, got {:?}", o),
        }

        let init = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Handler(h) if h.name == "init_slot" => Some(h),
                _ => None,
            })
            .unwrap();
        let init_effect = init
            .clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Effect(stmts) => Some(stmts),
                _ => None,
            })
            .unwrap();
        match &init_effect[0].node.rhs.node {
            Expr::Ctor { variant, payload } => {
                assert_eq!(variant, "Active");
                let p = payload.as_ref().expect("payload");
                match &p.node {
                    Expr::RecordLit(fields) => {
                        assert_eq!(fields.len(), 2);
                        assert_eq!(fields[0].0, "capital");
                        assert_eq!(fields[1].0, "pnl");
                    }
                    o => panic!("expected RecordLit payload, got {:?}", o),
                }
            }
            o => panic!("expected Ctor, got {:?}", o),
        }
    }

    #[test]
    fn parses_inline_match_expr() {
        let src = r#"
spec T
type Account
  | Inactive
  | Active of {
      capital : U128,
      pnl     : I128,
    }

property x :
  match state.accounts[i] with
    | Active a => a.capital >= 0
    | Inactive => 0 >= 0
  preserved_by all
"#;
        let s = parse_ok(src);
        let prop = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Property(p) => Some(p),
                _ => None,
            })
            .unwrap();
        match &prop.body.node {
            Expr::Match { scrutinee: _, arms } => {
                assert_eq!(arms.len(), 2);
                assert_eq!(arms[0].variant, "Active");
                assert_eq!(arms[0].binder.as_deref(), Some("a"));
                assert_eq!(arms[1].variant, "Inactive");
                assert!(arms[1].binder.is_none());
            }
            o => panic!("expected Match, got {:?}", o),
        }
    }

    #[test]
    fn parses_mul_div_floor() {
        let src = r#"
spec T
const SCALE = 1_000_000

handler noop (size : U128) (price : U64) : State.Active -> State.Active {
  requires mul_div_floor(size, price, SCALE) >= 0
}

type State | Active
"#;
        let s = parse_ok(src);
        let h = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Handler(h) => Some(h),
                _ => None,
            })
            .unwrap();
        let req = h
            .clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Requires { guard, .. } => Some(guard),
                _ => None,
            })
            .unwrap();
        // Expect: Cmp { MulDivFloor >= 0 }
        match &req.node {
            Expr::Cmp { op, lhs, rhs: _ } => {
                assert_eq!(*op, CmpOp::Ge);
                match &lhs.node {
                    Expr::MulDivFloor { a: _, b: _, d } => {
                        // `d` should be a Path to `SCALE`
                        match &d.node {
                            Expr::Path(p) => assert_eq!(p.root, "SCALE"),
                            o => panic!("expected Path, got {:?}", o),
                        }
                    }
                    o => panic!("expected MulDivFloor, got {:?}", o),
                }
            }
            o => panic!("expected Cmp, got {:?}", o),
        }
    }

    #[test]
    fn parses_type_alias() {
        let src = r#"
spec T
const MAX = 1024
type AccountIdx = Fin[MAX]
type Size = U128
"#;
        let s = parse_ok(src);
        let aliases: Vec<&TypeAliasDecl> = s
            .items
            .iter()
            .filter_map(|i| match &i.node {
                TopItem::TypeAlias(a) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].name, "AccountIdx");
        match &aliases[0].target {
            TypeRef::Fin { bound } => assert_eq!(bound, "MAX"),
            o => panic!("expected Fin, got {:?}", o),
        }
        assert_eq!(aliases[1].name, "Size");
        match &aliases[1].target {
            TypeRef::Named(n) => assert_eq!(n, "U128"),
            o => panic!("expected Named, got {:?}", o),
        }
    }

    #[test]
    fn parses_match_clause() {
        let src = r#"
spec T
type State | Active
type Error | Healthy | Bankrupt

handler liquidate : State.Active -> State.Active {
  match
    | state.V >= 100 => abort Healthy
    | state.V >= 50  => effect { V -= 10 }
    | _              => abort Bankrupt
}
"#;
        let s = parse_ok(src);
        let h = s
            .items
            .iter()
            .find_map(|i| match &i.node {
                TopItem::Handler(h) => Some(h),
                _ => None,
            })
            .unwrap();
        let m = h
            .clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Match(b) => Some(b),
                _ => None,
            })
            .expect("match clause");
        assert_eq!(m.arms.len(), 3);
        assert!(m.arms[0].guard.is_some());
        assert!(m.arms[2].guard.is_none()); // wildcard
        match &m.arms[0].body {
            MatchBody::Abort(n) => assert_eq!(n, "Healthy"),
            _ => panic!("expected abort body"),
        }
        match &m.arms[1].body {
            MatchBody::Effect(stmts) => assert_eq!(stmts.len(), 1),
            _ => panic!("expected effect body"),
        }
    }

    #[test]
    fn parses_liveness() {
        let src = r#"spec T
liveness drain : State.Draining ~> State.Active via [a, b] within 2"#;
        let s = parse_ok(src);
        match &s.items[0].node {
            TopItem::Liveness(l) => {
                assert_eq!(l.name, "drain");
                assert_eq!(l.from_state.0, vec!["State", "Draining"]);
                assert_eq!(l.to_state.0, vec!["State", "Active"]);
                assert_eq!(l.via, vec!["a", "b"]);
                assert_eq!(l.within, 2);
            }
            o => panic!("expected Liveness, got {:?}", o),
        }
    }

    // ------------------------------------------------------------------
    // interface block (v2.5 slice 1)
    // ------------------------------------------------------------------

    #[test]
    fn parses_tier0_interface_shape_only() {
        let src = r#"spec Demo
interface Jupiter {
  program_id "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4"

  handler swap (amount_in : U64) (min_amount_out : U64) {
    discriminant "0xE445A52E51CB9A1D"
    accounts {
      user_input_ta  : writable, type token
      user_output_ta : writable, type token
      user           : signer
    }
  }
}
"#;
        let s = parse_ok(src);
        let i = match &s.items[0].node {
            TopItem::Interface(i) => i,
            o => panic!("expected Interface, got {:?}", o),
        };
        assert_eq!(i.name, "Jupiter");
        assert_eq!(
            i.program_id.as_deref(),
            Some("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4")
        );
        assert!(i.upstream.is_none());
        assert_eq!(i.handlers.len(), 1);
        let h = &i.handlers[0];
        assert_eq!(h.name, "swap");
        assert_eq!(h.params.len(), 2);
        // Tier-0: no requires/ensures.
        let has_requires = h
            .clauses
            .iter()
            .any(|c| matches!(c.node, InterfaceHandlerClause::Requires { .. }));
        let has_ensures = h
            .clauses
            .iter()
            .any(|c| matches!(c.node, InterfaceHandlerClause::Ensures(_)));
        assert!(!has_requires, "Tier-0 interface should have no requires");
        assert!(!has_ensures, "Tier-0 interface should have no ensures");
    }

    #[test]
    fn parses_tier1_interface_with_upstream_and_ensures() {
        let src = r#"spec Demo
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:abcdef1234567890"
    verified_with ["proptest", "kani"]
    verified_at  "2026-04-18"
  }

  handler transfer (amount : U64) {
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#;
        let s = parse_ok(src);
        let i = match &s.items[0].node {
            TopItem::Interface(i) => i,
            o => panic!("expected Interface, got {:?}", o),
        };
        let u = i.upstream.as_ref().expect("upstream present");
        assert_eq!(u.package.as_deref(), Some("spl-token"));
        assert_eq!(u.version.as_deref(), Some("4.0.3"));
        assert_eq!(u.binary_hash.as_deref(), Some("sha256:abcdef1234567890"));
        assert_eq!(
            u.verified_with,
            vec!["proptest".to_string(), "kani".to_string()]
        );
        // Lean deliberately absent — no overclaiming.
        assert!(!u.verified_with.contains(&"lean".to_string()));

        let h = &i.handlers[0];
        let has_requires = h
            .clauses
            .iter()
            .any(|c| matches!(c.node, InterfaceHandlerClause::Requires { .. }));
        let has_ensures = h
            .clauses
            .iter()
            .any(|c| matches!(c.node, InterfaceHandlerClause::Ensures(_)));
        assert!(has_requires);
        assert!(has_ensures);
    }

    #[test]
    fn parses_empty_interface() {
        // An interface with no handlers is valid (e.g. a stub pre-codegen).
        let src =
            "spec T\ninterface Empty {\n  program_id \"11111111111111111111111111111111\"\n}\n";
        let s = parse_ok(src);
        match &s.items[0].node {
            TopItem::Interface(i) => {
                assert_eq!(i.name, "Empty");
                assert!(i.handlers.is_empty());
            }
            o => panic!("expected Interface, got {:?}", o),
        }
    }

    // ------------------------------------------------------------------
    // call clause (v2.5 slice 2)
    // ------------------------------------------------------------------

    fn first_handler_clauses(spec: &Spec) -> &Vec<Node<HandlerClause>> {
        match spec.items.iter().find_map(|n| match &n.node {
            TopItem::Handler(h) => Some(h),
            _ => None,
        }) {
            Some(h) => &h.clauses,
            None => panic!("no handler in spec"),
        }
    }

    #[test]
    fn parses_call_clause_with_kw_args() {
        let src = r#"spec T
handler exchange : State.A -> State.B {
  call Token.transfer(from = taker_ta, to = initializer_ta, amount = taker_amount, authority = taker)
}
"#;
        let s = parse_ok(src);
        let clauses = first_handler_clauses(&s);
        let call = clauses.iter().find_map(|c| match &c.node {
            HandlerClause::Call(e) => Some(e),
            _ => None,
        });
        let call = call.expect("expected a Call clause");
        assert_eq!(
            call.target.0,
            vec!["Token".to_string(), "transfer".to_string()]
        );
        assert_eq!(call.args.len(), 4);
        assert_eq!(call.args[0].name, "from");
        assert_eq!(call.args[3].name, "authority");
    }

    #[test]
    fn parses_call_with_trailing_comma() {
        let src = r#"spec T
handler h : State.A -> State.A {
  call Token.transfer(
    from   = a,
    to     = b,
    amount = 100,
  )
}
"#;
        let s = parse_ok(src);
        let clauses = first_handler_clauses(&s);
        let has_call = clauses
            .iter()
            .any(|c| matches!(c.node, HandlerClause::Call(_)));
        assert!(has_call);
    }

    #[test]
    fn parses_call_with_no_args() {
        let src = r#"spec T
handler h : State.A -> State.A {
  call Clock.current()
}
"#;
        let s = parse_ok(src);
        let clauses = first_handler_clauses(&s);
        let call = clauses.iter().find_map(|c| match &c.node {
            HandlerClause::Call(e) => Some(e),
            _ => None,
        });
        let call = call.expect("expected a Call");
        assert!(call.args.is_empty());
    }

    // ------------------------------------------------------------------
    // pragma sbpf { ... } (v2.5 slice — platform-specific namespace)
    // ------------------------------------------------------------------

    #[test]
    fn parses_pragma_sbpf_with_instruction() {
        let src = r#"spec Transfer
pragma sbpf {
  pubkey TOKEN_PROGRAM [1, 2, 3, 4]

  instruction transfer {
    discriminant 3
    entry 0
  }
}
"#;
        let s = parse_ok(src);
        let p = match &s.items[0].node {
            TopItem::Pragma(p) => p,
            o => panic!("expected Pragma, got {:?}", o),
        };
        assert_eq!(p.name, "sbpf");
        assert_eq!(p.items.len(), 2);
        // Order is preserved: pubkey first, instruction second.
        assert!(matches!(p.items[0].node, TopItem::Pubkey(_)));
        assert!(matches!(p.items[1].node, TopItem::Instruction(_)));
    }

    #[test]
    fn pragma_body_rejects_non_whitelisted_items() {
        // A `handler` at the top level of a pragma is not in the whitelist
        // — it belongs to the core DSL. The parser fails on the closing
        // brace because the handler doesn't consume.
        let src = r#"spec T
pragma sbpf {
  handler nope : State.A -> State.A { effect {} }
}
"#;
        assert!(
            parse(src).is_err(),
            "pragma body should reject `handler`; core DSL items belong at top level"
        );
    }

    #[test]
    fn empty_pragma_parses() {
        let src = "spec T\npragma sbpf {}\n";
        let s = parse_ok(src);
        match &s.items[0].node {
            TopItem::Pragma(p) => {
                assert_eq!(p.name, "sbpf");
                assert!(p.items.is_empty());
            }
            o => panic!("expected Pragma, got {:?}", o),
        }
    }

    // ------------------------------------------------------------------
    // ML-style `let x = v in body` in expressions (v2.5)
    // ------------------------------------------------------------------

    #[test]
    fn parses_let_in_inside_ensures() {
        let src = r#"spec T
type State | A of { balance : U64 }

handler withdraw (amount : U64) : State.A -> State.A {
  effect { balance = balance - amount }
  ensures let delta = old(state.balance) - state.balance in delta == amount
}
"#;
        let s = parse_ok(src);
        let clauses = first_handler_clauses(&s);
        let ensures = clauses
            .iter()
            .find_map(|c| match &c.node {
                HandlerClause::Ensures(e) => Some(e),
                _ => None,
            })
            .expect("expected an Ensures clause");
        // Top of the ensures expression is the Let binding; its body is a Cmp.
        match &ensures.node {
            Expr::Let { name, body, .. } => {
                assert_eq!(name, "delta");
                assert!(
                    matches!(body.node, Expr::Cmp { .. }),
                    "expected Cmp in let body, got {:?}",
                    body.node
                );
            }
            other => panic!("expected Let at top of ensures, got {:?}", other),
        }
    }

    #[test]
    fn parses_nested_let_in() {
        let src = r#"spec T
type State | A of { x : U64, y : U64 }

handler h : State.A -> State.A {
  ensures
    let a = state.x in
    let b = state.y in
    a + b == a + b
}
"#;
        parse_ok(src);
    }

    #[test]
    fn let_keyword_still_works_as_handler_clause() {
        // Keyword-ifying `let` must not break the statement-level clause.
        let src = r#"spec T
type State | A of { count : U64 }

handler h (amount : U64) : State.A -> State.A {
  let doubled = amount + amount
  effect { count = count + doubled }
}
"#;
        parse_ok(src);
    }
}
