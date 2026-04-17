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

use chumsky::input::ValueInput;
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
fn kw<'a>(
    keyword: &'static str,
) -> impl Parser<'a, &'a str, (), Err<'a>> + Clone {
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
        .then_ignore(any::<&'a str, Err<'a>>().filter(|c: &char| c.is_whitespace()).repeated())
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

        let bool_lit = choice((
            kw("true").to(true),
            kw("false").to(false),
        ))
        .map_with(|b, e| Node::new(Expr::Bool(b), e.span().into_range()));

        let path_expr =
            path().map_with(|p, e| Node::new(Expr::Path(p), e.span().into_range()));

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

        // forall / exists i : T, body
        let quant = choice((
            just("forall").to(Quantifier::Forall),
            just("exists").to(Quantifier::Exists),
        ))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just(','))
        .then_ignore(wsc())
        .then(expr.clone())
        .map_with(|(((kind, binder), binder_ty), body), e| {
            let ty_name = binder_ty.0.join(".");
            Node::new(
                Expr::Quant {
                    kind,
                    binder,
                    binder_ty: ty_name,
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
        let match_arm = just('|')
            .then_ignore(wsc())
            .ignore_then(match_arm_pat);
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
            .map_with(|fields, e| {
                Node::new(Expr::RecordLit(fields), e.span().into_range())
            })
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

        // atom — must stay under chumsky's `choice` arity limit; split.
        // `.boxed()` tames the type complexity that otherwise trips Apple's
        // linker on overlong symbol names.
        let group_a = choice((int, bool_lit, old, sum, quant)).boxed();
        let group_b = choice((mul_div_floor_atom, mul_div_ceil_atom, match_expr)).boxed();
        // `record_update` must precede `ctor` (leading `.` distinguishes
        // them, but this ordering is clearer). Try record_update before
        // record_lit; both before the bare-path fallback.
        let group_c = choice((record_update, record_lit, ctor, paren, path_expr)).boxed();
        let atom_base = choice((group_a, group_b, group_c)).then_ignore(wsc()).boxed();

        // Postfix `is .Variant` check — layers on any atom result.
        let is_postfix = kw("is")
            .ignore_then(just('.'))
            .ignore_then(non_keyword_ident())
            .then_ignore(wsc());
        let atom = atom_base
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
        let product = atom
            .clone()
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
        let add_op = choice((just('+').to(ArithOp::Add), just('-').to(ArithOp::Sub)))
            .then_ignore(wsc());
        let arith = product
            .clone()
            .foldl_with(add_op.then(product.clone()).repeated(), |lhs, (op, rhs), e| {
                Node::new(
                    Expr::Arith {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                )
            });

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
        let cmp = arith.clone().then(cmp_op.then(arith.clone()).or_not()).map_with(
            |(lhs, maybe_rhs), e| match maybe_rhs {
                None => lhs,
                Some((op, rhs)) => Node::new(
                    Expr::Cmp {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    e.span().into_range(),
                ),
            },
        );

        // not: ("not" cmp) | cmp
        let not_expr = recursive(|not_expr| {
            choice((
                just("not")
                    .then_ignore(wsc())
                    .ignore_then(not_expr.clone())
                    .map_with(|inner, e| Node::new(Expr::Not(Box::new(inner)), e.span().into_range())),
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
        .then(variant().then_ignore(wsc()).repeated().at_least(1).collect::<Vec<Variant>>())
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
    let arm_guard = choice((
        wildcard_guard,
        expr().map(Some),
    ));
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

    // `choice()` has an arity limit; split into groups.
    let grp_a = choice((auth, accounts, requires, ensures, modifies, let_c, effect));
    let grp_b = choice((transfers, takes, emits, aborts_total, invariant, include));
    let grp_c = choice((match_c,));
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

// target assembly | target quasar
fn target_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("target")
        .ignore_then(choice((
            kw("assembly").to("assembly".to_string()),
            kw("quasar").to("quasar".to_string()),
        )))
        .map(TopItem::Target)
}

// program_id "base58..."
fn program_id_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("program_id")
        .ignore_then(string_lit())
        .map(TopItem::ProgramId)
}

// assembly "path/to/file.s"
fn assembly_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("assembly").ignore_then(string_lit()).map(TopItem::Assembly)
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

    let clause = choice((mutates, constraint))
        .map_with(|c, e| Node::new(c, e.span().into_range()));

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
        target_decl(),
        program_id_decl(),
        assembly_decl(),
    ));
    choice((group_a, group_b))
        .map_with(|item, e| Node::new(item, e.span().into_range()))
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
                    Expr::Sum { binder, binder_ty, .. } => {
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
                TopItem::Target(_) => "target",
                TopItem::ProgramId(_) => "program_id",
                TopItem::Assembly(_) => "assembly",
                TopItem::TypeAlias(_) => "type_alias",
            })
            .fold(std::collections::BTreeMap::<&str, usize>::new(), |mut m, k| {
                *m.entry(k).or_default() += 1;
                m
            });

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
        let h = s.items.iter().find_map(|i| match &i.node {
            TopItem::Handler(h) => Some(h),
            _ => None,
        }).unwrap();
        // requires: IsVariant
        let req = h.clauses.iter().find_map(|c| match &c.node {
            HandlerClause::Requires { guard, .. } => Some(guard),
            _ => None,
        }).unwrap();
        match &req.node {
            Expr::IsVariant { variant, .. } => assert_eq!(variant, "Active"),
            o => panic!("expected IsVariant, got {:?}", o),
        }
        // effect RHS: Match containing RecordUpdate on the Active arm
        let eff = h.clauses.iter().find_map(|c| match &c.node {
            HandlerClause::Effect(s) => Some(s),
            _ => None,
        }).unwrap();
        match &eff[0].node.rhs.node {
            Expr::Match { arms, .. } => {
                match &arms[0].body.node {
                    Expr::Ctor { variant: v, payload } => {
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
                }
            }
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
        let reset = s.items.iter().find_map(|i| match &i.node {
            TopItem::Handler(h) if h.name == "reset_slot" => Some(h),
            _ => None,
        }).unwrap();
        let reset_effect = reset.clauses.iter().find_map(|c| match &c.node {
            HandlerClause::Effect(stmts) => Some(stmts),
            _ => None,
        }).unwrap();
        match &reset_effect[0].node.rhs.node {
            Expr::Ctor { variant, payload } => {
                assert_eq!(variant, "Inactive");
                assert!(payload.is_none());
            }
            o => panic!("expected Ctor, got {:?}", o),
        }

        let init = s.items.iter().find_map(|i| match &i.node {
            TopItem::Handler(h) if h.name == "init_slot" => Some(h),
            _ => None,
        }).unwrap();
        let init_effect = init.clauses.iter().find_map(|c| match &c.node {
            HandlerClause::Effect(stmts) => Some(stmts),
            _ => None,
        }).unwrap();
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
}
