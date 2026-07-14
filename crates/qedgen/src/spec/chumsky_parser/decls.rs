//! Data, type, property, and Solana-native top-level declarations:
//! consts and const folding, records/ADTs/aliases, properties, covers,
//! liveness, ref_impl, ghost/hook/schema/invariant, PDAs, events,
//! environments, pubkeys, and the error table.

use super::*;

// ----------------------------------------------------------------------------
// Top-level declarations
// ----------------------------------------------------------------------------

pub(super) fn const_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    // RHS is any const-foldable expression: the full `expr()` grammar,
    // then `try_const_fold` reduces to a single `i128` or fails naming the
    // non-const subterm. Supported: integer literals (negatives via the
    // desugared `Sub(Int(0), Int(N))`), `+ - * / %`, parens. Bare ident
    // references to other consts and shifts are deferred.
    kw("const")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(expr().try_map(|node, span| {
            try_const_fold(&node.node).map_err(|reason| {
                Rich::custom(
                    span,
                    format!("const RHS isn't a const expression: {}", reason),
                )
            })
        }))
        .map(|(name, value)| TopItem::Const { name, value })
}

/// Fold a `const NAME = <expr>` body into an `i128`: integer literals,
/// paren, arithmetic over constants; rejects anything runtime-dependent
/// (paths, calls, quantifiers). Called from `const_decl::try_map` so the
/// error carries the parse span of the unsupported subterm.
pub(super) fn try_const_fold(e: &Expr) -> std::result::Result<i128, String> {
    match e {
        Expr::Int(v) => {
            i128::try_from(*v).map_err(|_| "integer literal overflows i128".to_string())
        }
        Expr::Paren(inner) => try_const_fold(&inner.node),
        Expr::Arith { op, lhs, rhs } => {
            let l = try_const_fold(&lhs.node)?;
            let r = try_const_fold(&rhs.node)?;
            let result = match op {
                ArithOp::Add => l.checked_add(r),
                ArithOp::Sub => l.checked_sub(r),
                ArithOp::Mul => l.checked_mul(r),
                ArithOp::Div => {
                    if r == 0 {
                        return Err("division by zero in const expression".to_string());
                    }
                    l.checked_div(r)
                }
                ArithOp::Mod => {
                    if r == 0 {
                        return Err("modulo by zero in const expression".to_string());
                    }
                    l.checked_rem(r)
                }
            };
            result.ok_or_else(|| "arithmetic overflow in const expression".to_string())
        }
        Expr::Bool(_) => Err("boolean literal not allowed in const expression".to_string()),
        Expr::Path(_) => Err(
            "path / bare identifier references in const expressions are deferred to v2.30; \
             inline the literal value here for now"
                .to_string(),
        ),
        _ => Err(
            "unsupported subterm — const expressions accept integer literals, paren, and \
             arithmetic (+ - * / %) only"
                .to_string(),
        ),
    }
}

pub(super) fn typed_field<'a>() -> impl Parser<'a, &'a str, TypedField, Err<'a>> + Clone {
    non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, ty)| TypedField { name, ty })
}

pub(super) fn typed_field_list<'a>() -> impl Parser<'a, &'a str, Vec<TypedField>, Err<'a>> + Clone {
    typed_field()
        .then_ignore(wsc())
        .separated_by(just(',').then_ignore(wsc()))
        .allow_trailing()
        .collect::<Vec<TypedField>>()
}

// Record: type T = { field : Type, ... }
pub(super) fn record_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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

// state { field : Type, ... } — sugar for `type State = { ... }`. Accepts
// comma-separated (canonical) or newline-separated (as documented in
// references/qedspec-dsl.md §"state (sugar)") field forms.
pub(super) fn state_sugar_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    // Separator: optional comma, always tolerant of surrounding whitespace.
    // This accepts `a : U64, b : U8`, `a : U64\n  b : U8`, or trailing commas.
    let sep = wsc().then_ignore(just(',').or_not()).then_ignore(wsc());
    let fields = typed_field()
        .then_ignore(sep)
        .repeated()
        .collect::<Vec<TypedField>>();
    just("state")
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .ignore_then(fields)
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|fields| {
            TopItem::Record(RecordDecl {
                name: "State".to_string(),
                fields,
            })
        })
}

// Type alias: type Name = <type_ref>   (when `{` doesn't follow `=`)
// Order matters in the `choice()` at top_item: record_decl is tried first
// so `type T = { ... }` is consumed by record, not by this alias rule.
pub(super) fn type_alias_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("type")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, target)| TopItem::TypeAlias(TypeAliasDecl { name, target }))
}

// Nominal numeric dimension: `dimension Lamports = U64`.
pub(super) fn dimension_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("dimension")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, base)| TopItem::Dimension(DimensionDecl { name, base }))
}

// ADT variant: `| Name [= code] ["desc"] [of { fields }]`
pub(super) fn variant<'a>() -> impl Parser<'a, &'a str, Variant, Err<'a>> + Clone {
    let code = just('=')
        .then_ignore(wsc())
        .ignore_then(integer())
        .map(|n| n as u64)
        .then_ignore(wsc());
    let desc = string_lit().then_ignore(wsc());
    // Payload: `of { named : T, … }` (struct variant) OR `of <Type>` (a
    // single-field TUPLE variant, e.g. `Custom of I64` → Rust `Custom(i64)`).
    // The tuple field gets the synthetic name "0" — impossible for a real field
    // (whose grammar requires an identifier), so downstream codegen detects the
    // numeric name and renders positional `Enum::V(val)` instead of
    // `Enum::V { name: val }`. G13b (#177 follow-on).
    let struct_body = just('{')
        .then_ignore(wsc())
        .ignore_then(typed_field_list())
        .then_ignore(wsc())
        .then_ignore(just('}'));
    let tuple_body = type_ref().map(|ty| {
        vec![TypedField {
            name: "0".to_string(),
            ty,
        }]
    });
    let fields = just("of")
        .then_ignore(wsc())
        .ignore_then(choice((struct_body, tuple_body)))
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
pub(super) fn adt_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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

// property name : expr preserved_by all | [a, b, ...]
pub(super) fn property_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    // `preserved_by all except [h1, h2, ...]` expands at adapt time
    // against the full handler list. Try the longer form first so bare
    // `all` doesn't greedy-match and leave `except` hanging.
    let list = just('[')
        .then_ignore(wsc())
        .ignore_then(
            non_keyword_ident()
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'));
    let all_except = just("all")
        .then_ignore(wsc())
        .then_ignore(just("except"))
        .then_ignore(wsc())
        .ignore_then(list.clone())
        .map(PreservedBy::AllExcept);
    let preserved = just("preserved_by").then_ignore(wsc()).ignore_then(choice((
        all_except,
        just("all").to(PreservedBy::All),
        list.map(PreservedBy::Some),
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
pub(super) fn cover_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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
pub(super) fn liveness_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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

/// Top-level `ref_impl name (p1 : T1) (p2 : T2) : R = <expr>` — reference
/// implementation `ensures` clauses can call. Pure: no state mutation, no
/// side effects, no calls to other ref_impls (yet). Lowers to a Lean `def`;
/// Kani inlines the body at assertion sites; Rust codegen skips it.
pub(super) fn ref_impl_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    // Parens around each `(name : Type)` are required so the parser can
    // disambiguate params from the return-type `: R` that follows.
    let param = just('(')
        .then_ignore(wsc())
        .ignore_then(typed_field())
        .then_ignore(wsc())
        .then_ignore(just(')'));
    let params = param.then_ignore(wsc()).repeated().collect::<Vec<_>>();

    doc_comments()
        .then_ignore(kw("ref_impl"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then(params)
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(expr())
        .map(|((((doc, name), params), return_type), body)| {
            TopItem::RefImpl(RefImplDecl {
                name,
                doc,
                params,
                return_type,
                body,
            })
        })
}

/// `ghost <name> : <Ty> { init { <expr> } on <handler> { <name> := <expr> }
/// … }` — one `init` clause then zero or more `on <handler>` updates. Each
/// update reuses `effect_stmt()`, so `:=` / `+=` / `-=` (and `state.<ghost>`
/// RHS references) work as in a real handler effect.
pub(super) fn ghost_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let init_clause = kw("init")
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .ignore_then(expr())
        .then_ignore(wsc())
        .then_ignore(just('}'));

    let on_clause = kw("on")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(effect_stmt())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|(handler, stmt)| GhostUpdate { handler, stmt });

    doc_comments()
        .then_ignore(kw("ghost"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(init_clause)
        .then_ignore(wsc())
        .then(
            on_clause
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<GhostUpdate>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((((doc, name), ty), init), updates)| {
            TopItem::Ghost(GhostDecl {
                name,
                doc,
                ty,
                init,
                updates,
            })
        })
}

/// `hook after_store(<field>) { assert <expr> … }` /
/// `hook before_cpi[(<Iface>)] { assert <expr> … }` — cross-cutting
/// assertion(s) checked at a MIR-statement boundary.
pub(super) fn hook_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let assert_clause = kw("assert")
        .ignore_then(expr())
        .then_ignore(wsc())
        .then_ignore(just(';').or_not());
    let asserts = assert_clause
        .then_ignore(wsc())
        .repeated()
        .collect::<Vec<Node<Expr>>>();

    let after_store = kw("after_store")
        .then_ignore(wsc())
        .then_ignore(just('('))
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(')'))
        .map(HookKind::AfterStore);

    let before_cpi = kw("before_cpi")
        .then_ignore(wsc())
        .ignore_then(
            just('(')
                .then_ignore(wsc())
                .ignore_then(non_keyword_ident())
                .then_ignore(wsc())
                .then_ignore(just(')'))
                .or_not(),
        )
        .map(HookKind::BeforeCpi);

    let kind = choice((after_store, before_cpi));

    doc_comments()
        .then_ignore(kw("hook"))
        .then(kind)
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(asserts)
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, kind), asserts)| TopItem::Hook(HookDecl { doc, kind, asserts }))
}

/// Top-level `schema name { requires expr else Err … }` — reusable
/// cross-cutting guard set. Handlers reference a schema via the existing
/// `include <name>` clause; the adapter expands every requires in the
/// schema into the handler's requires list.
pub(super) fn schema_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let req = just("requires")
        .then_ignore(wsc())
        .ignore_then(expr())
        .then_ignore(wsc())
        .then(
            just("else")
                .then_ignore(wsc())
                .ignore_then(non_keyword_ident())
                .or_not(),
        );
    doc_comments()
        .then_ignore(kw("schema"))
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .then(req.then_ignore(wsc()).repeated().collect::<Vec<_>>())
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(|((doc, name), requires)| {
            TopItem::Schema(SchemaDecl {
                name,
                doc,
                requires,
            })
        })
}

// invariant name : expr  OR  invariant name "description"
pub(super) fn invariant_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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
pub(super) fn program_id_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    kw("program_id")
        .ignore_then(string_lit())
        .map(TopItem::ProgramId)
}

// pda name [seed1, seed2, ...]
pub(super) fn pda_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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
pub(super) fn event_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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
pub(super) fn environment_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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

    let external = kw("external")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('.'))
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|((object, field), ty)| EnvClause::External { object, field, ty });

    let clause = choice((external, mutates, constraint))
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

// ----------------------------------------------------------------------------
// sBPF constructs: pubkey, errors (top-level sugar), instruction block
// ----------------------------------------------------------------------------

// pubkey NAME [c0, c1, c2, c3]
pub(super) fn pubkey_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    bare_kw("pubkey")
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
pub(super) fn error_entry<'a>() -> impl Parser<'a, &'a str, ErrorEntry, Err<'a>> + Clone {
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
pub(super) fn errors_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    bare_kw("errors")
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
