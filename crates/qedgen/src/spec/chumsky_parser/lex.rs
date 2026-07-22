//! Lexical primitives and path grammar: whitespace/comment eating,
//! keyword and identifier matching, integer/string/doc-comment literals,
//! type references, and dotted/subscripted paths. The lexical floor every
//! other submodule builds on.

use super::*;

// ----------------------------------------------------------------------------
// Tokenless primitives
// ----------------------------------------------------------------------------

/// Whitespace and line-comment eater. Used between tokens.
pub(super) fn wsc<'a>() -> impl Parser<'a, &'a str, (), Err<'a>> + Clone {
    let ws = any::<&'a str, Err<'a>>()
        .filter(|c: &char| c.is_whitespace())
        .ignored();
    let line_comment = just("//")
        .then(any().and_is(just('\n').not()).repeated())
        .ignored();
    choice((ws, line_comment)).repeated().ignored()
}

/// Match a keyword with a word boundary on the trailing side — rejects
/// `justify` matching `just`. Does NOT consume trailing ws/comments;
/// use `kw()` when the trailing `wsc()` should be eaten too.
pub(super) fn bare_kw<'a>(keyword: &'static str) -> impl Parser<'a, &'a str, (), Err<'a>> + Clone {
    just(keyword)
        .then(
            any::<&'a str, Err<'a>>()
                .filter(|c: &char| c.is_ascii_alphanumeric() || *c == '_')
                .rewind()
                .not(),
        )
        .ignored()
}

/// `bare_kw` + trailing ws/comment consumption.
pub(super) fn kw<'a>(keyword: &'static str) -> impl Parser<'a, &'a str, (), Err<'a>> + Clone {
    bare_kw(keyword).then_ignore(wsc())
}

/// Identifier: `[A-Za-z_][A-Za-z0-9_]*` — returned as an owned `String`.
pub(super) fn ident<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
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
pub(super) const KEYWORDS: &[&str] = &[
    "spec",
    "const",
    "type",
    "dimension",
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
    "mul_div_round_half_up",
    "interface",
    "pragma",
    "let",
    "in",
    "permissionless",
    "schema",
    "establishes",
    // The trailing `from` is contextual (matched via `kw("from")` only
    // inside `import_decl`) — handlers still use `from = expr` in call args.
    "import",
    "ref_impl",
];

pub(super) fn non_keyword_ident<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
    ident().try_map(|s, span| {
        if KEYWORDS.contains(&s.as_str()) {
            Err(Rich::custom(span, format!("unexpected keyword `{}`", s)))
        } else {
            Ok(s)
        }
    })
}

/// Integer literal, optionally with underscore separators. Returns u128.
pub(super) fn integer<'a>() -> impl Parser<'a, &'a str, u128, Err<'a>> + Clone {
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

/// Double-quoted string literal. Escapes: `\\`, `\"`, `\n`, `\t`, plus
/// `\<newline>` line continuation (consumed, emits nothing) so long
/// descriptions join into one logical line. Whitespace after the consumed
/// newline is preserved verbatim — indented continuations keep their
/// leading whitespace in the joined string.
pub(super) fn string_lit<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
    #[derive(Clone, Copy)]
    enum CharOrEmpty {
        Char(char),
        Empty,
    }
    // `\<newline>` continuation — emits no character. Optional `\r`
    // before `\n` accommodates CRLF source files.
    let line_continuation = just('\\')
        .ignore_then(just('\r').or_not())
        .then_ignore(just('\n'))
        .map(|_| CharOrEmpty::Empty);
    let escape = just('\\')
        .ignore_then(choice((just('\\'), just('"'), just('n'), just('t'))))
        .map(|c| {
            CharOrEmpty::Char(match c {
                'n' => '\n',
                't' => '\t',
                other => other,
            })
        });
    let plain = any::<&'a str, Err<'a>>()
        .filter(|c: &char| *c != '"' && *c != '\\')
        .map(CharOrEmpty::Char);
    let char_inner = choice((line_continuation, escape, plain));
    just('"')
        .ignore_then(char_inner.repeated().collect::<Vec<_>>())
        .then_ignore(just('"'))
        .map(|chunks: Vec<CharOrEmpty>| {
            let mut out = String::with_capacity(chunks.len());
            for c in chunks {
                if let CharOrEmpty::Char(ch) = c {
                    out.push(ch);
                }
            }
            out
        })
}

/// Doc comment line: `/// ...\n`. Returns the text after `///`, trimmed.
pub(super) fn doc_line<'a>() -> impl Parser<'a, &'a str, String, Err<'a>> + Clone {
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
pub(super) fn doc_comments<'a>() -> impl Parser<'a, &'a str, Option<String>, Err<'a>> + Clone {
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

pub(super) fn type_ref<'a>() -> impl Parser<'a, &'a str, TypeRef, Err<'a>> + Clone {
    // Bound position accepts a const/unit-sum ident OR a numeric literal —
    // the documented `Fin[8]` / `Map[4] T` forms parse (#327; before this,
    // only ident bounds like `Map[MAX_MEMBERS]` did).
    let bound_lit = any::<&'a str, Err<'a>>()
        .filter(|c: &char| c.is_ascii_digit())
        .repeated()
        .at_least(1)
        .collect::<String>();
    let bound = choice((non_keyword_ident(), bound_lit));

    // Map[N] T — bounded map keyed by an index domain of size `N`.
    let map_ty = just("Map")
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(bound.clone())
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
        .ignore_then(bound)
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(|bound| TypeRef::Fin { bound });

    // `Vec T` / `Option T` — single-arg parameterized type; `T` is a named
    // type (scalar or record). Recognised by a `Vec`/`Option` head ident so it
    // can't shadow a user type like `Vector`. Nested args (`Vec (Option T)`)
    // aren't modelled (`TypeRef::Param` carries name strings), which is enough
    // to mirror real account fields (`Option<Pubkey>`, `Vec<Record>`).
    let param_ty = non_keyword_ident()
        .filter(|s: &String| s == "Vec" || s == "Option")
        .then_ignore(wsc())
        .then(non_keyword_ident())
        .map(|(ctor, inner)| TypeRef::Param(ctor, inner));

    // Simple type: a single ident.
    let simple = non_keyword_ident().map(TypeRef::Named);

    choice((map_ty, fin_ty, param_ty, simple))
}

// ----------------------------------------------------------------------------
// Qualified path (no subscripts): `State.Active`, `Pool.Empty`
// ----------------------------------------------------------------------------

pub(super) fn qualified_path<'a>() -> impl Parser<'a, &'a str, QualifiedPath, Err<'a>> + Clone {
    non_keyword_ident()
        .separated_by(just('.'))
        .at_least(1)
        .collect::<Vec<String>>()
        .map(QualifiedPath)
}

// ----------------------------------------------------------------------------
// Path with subscripts: `state.accounts[i].capital`
// ----------------------------------------------------------------------------

pub(super) fn path<'a>() -> impl Parser<'a, &'a str, Path, Err<'a>> + Clone {
    let field_seg = just('.').ignore_then(ident()).map(PathSeg::Field);
    // Dotted-path index expressions (`lsts[state.lst_count].mint`): joined
    // with `.` into a single `PathSeg::Index(String)`; codegen handles
    // `state.X` indices via `rewrite_index_to_usize` + state-binder
    // resolution, same as bare-ident indices.
    let dotted_index = ident()
        .then(
            just('.')
                .ignore_then(ident())
                .repeated()
                .collect::<Vec<String>>(),
        )
        .map(|(head, rest)| {
            if rest.is_empty() {
                head
            } else {
                let mut s = head;
                for seg in rest {
                    s.push('.');
                    s.push_str(&seg);
                }
                s
            }
        });
    let index_seg = just('[')
        .ignore_then(dotted_index)
        .then_ignore(just(']'))
        .map(PathSeg::Index);
    let seg = choice((field_seg, index_seg));
    ident()
        .then(seg.repeated().collect::<Vec<PathSeg>>())
        .map(|(root, segments)| Path { root, segments })
}

/// Signed integer (for layout offsets).
pub(super) fn signed_integer<'a>() -> impl Parser<'a, &'a str, i64, Err<'a>> + Clone {
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
