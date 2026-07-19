//! Chumsky-based parser for `.qedspec` files: source → typed AST
//! (`ast::Spec`). `chumsky_adapter.rs` translates the typed AST into the
//! `ParsedSpec` consumed downstream.
//!
//! Split into per-concern submodules. The directory rename keeps the module
//! path `crate::spec::chumsky_parser` (and the root re-export
//! `crate::chumsky_parser`) intact; the globs below re-export each
//! submodule's items so the existing `crate::chumsky_parser::<name>` call
//! sites — and the cross-submodule references — continue to resolve
//! unchanged. The top-level driver (`spec_parser` / `parse` /
//! `format_parse_error`) stays in this facade.

use chumsky::prelude::*;

use crate::ast::*;

type Err<'a> = extra::Err<Rich<'a, char>>;

// Per-concern submodules. Each starts with `use super::*;` to pull in the
// shared `Err` alias, the chumsky prelude, and the AST glob — plus every
// other submodule's items via the internal re-export globs below.
mod decls;
mod expr;
mod handler;
mod instruction;
mod interface;
mod lex;

// Internal re-exports: surface each submodule's items to the facade (so the
// driver below can call them) and to sibling submodules (through their
// `use super::*;`). `pub(in crate::spec::chumsky_parser)` keeps this from
// leaking anything crate-wide — only the explicit `pub fn`s below are
// reachable as `crate::chumsky_parser::<name>`.
pub(in crate::spec::chumsky_parser) use decls::*;
pub(in crate::spec::chumsky_parser) use expr::*;
pub(in crate::spec::chumsky_parser) use handler::*;
pub(in crate::spec::chumsky_parser) use instruction::*;
pub(in crate::spec::chumsky_parser) use interface::*;
pub(in crate::spec::chumsky_parser) use lex::*;

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

/// `pragma <key> = <value>` top-level assignment (e.g.
/// `checked_overflow_error`). Distinct from the `pragma <name> { … }`
/// namespace form — disambiguated by `=` vs `{` at the call site. Unknown
/// keys parse but are flagged at lint time, so new keys don't break specs.
fn pragma_assign_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    // Value is a `::`-joined path (`state::policies::utils::foo` for
    // `pragma state_module`); a bare ident is just a length-1 path, so existing
    // pragmas (`state_repr = adt`) are unaffected. A `*` segment is accepted so
    // `pragma harness_use = crate::foo::bar::*` can request a glob import
    // (`state_module` values never contain `*`, so this is backward-compatible).
    // A bare integer segment is accepted so numeric-valued pragmas parse —
    // before #192 the grammar REJECTED them, making the documented
    // `pragma kani_vec_bound = <N>` unusable.
    let path_seg = choice((
        non_keyword_ident(),
        just("*").to("*".to_string()),
        text::int(10).map(|s: &str| s.to_string()),
    ));
    let path_value = path_seg
        .separated_by(just("::"))
        .at_least(1)
        .collect::<Vec<String>>()
        .map(|parts| parts.join("::"));
    kw("pragma")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(path_value)
        .map(|(name, value)| TopItem::PragmaAssign { name, value })
}

// import <Name> from "<dep_key>" — `Name` is the local bound name;
// `dep_key` keys into qed.toml's `[dependencies]`. Resolution lives in
// `import_resolver.rs` and runs after parse, before lint.
fn import_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let as_clause = kw("as").ignore_then(wsc()).ignore_then(non_keyword_ident());
    kw("import")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(kw("from"))
        .then_ignore(wsc())
        .then(string_lit())
        .then_ignore(wsc())
        .then(as_clause.or_not())
        .map(|((name, from), as_name)| TopItem::Import {
            name,
            from,
            as_name,
        })
}

// Top-level item: priority-ordered choice.
// record_decl must precede adt_decl (PEG-style backtracking via .or).
fn top_item<'a>() -> impl Parser<'a, &'a str, Node<TopItem>, Err<'a>> + Clone {
    // Priority matters for `type` forms — try record (`type T = { ... }`)
    // first, then type alias (`type T = <type_ref>`). ADT (`type T | ...`)
    // uses a different shape after the name and can be disambiguated.
    let group_a = choice((
        const_decl(),
        dimension_decl(),
        record_decl(),
        type_alias_decl(),
        adt_decl(),
        state_sugar_decl(),
        handler_decl(),
        property_decl(),
        cover_decl(),
        liveness_decl(),
        invariant_decl(),
        schema_decl(),
        ref_impl_decl(),
    ));
    let group_b = choice((
        pda_decl(),
        event_decl(),
        environment_decl(),
        ghost_decl(),
        hook_decl(),
        program_id_decl(),
    ));
    // Note: `pubkey`, `instruction`, `assembly`, and the `errors [...]`
    // sugar are platform-specific and only parse inside
    // `pragma sbpf { ... }`. Use `type Error | A | B | ...` for errors at
    // the core-DSL level. The platform-agnostic top level is the point.
    // pragma_assign_decl tried before pragma_decl: both start with `kw("pragma")
    // <ident>` and diverge on `=` (assign) vs `{` (namespace). chumsky's
    // choice() backtracks on parse failure, so the assign branch fails fast
    // on `{` and the namespace branch picks up.
    let group_c = choice((
        interface_decl(),
        pragma_assign_decl(),
        pragma_decl(),
        import_decl(),
    ));
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

/// Byte offset → 1-indexed `line:col` for error messages.
fn byte_offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(src.len());
    let before = &src[..clamped];
    let line = 1 + before.bytes().filter(|b| *b == b'\n').count();
    let col = match before.rfind('\n') {
        Some(nl) => src[nl + 1..clamped].chars().count() + 1,
        None => before.chars().count() + 1,
    };
    (line, col)
}

/// Coarse construct context for a parse error (#254): the nearest
/// preceding line whose first token is a known construct keyword names
/// what was being parsed. The raw `Rich` expected-set is token-level
/// (char classes) and not actionable alone for spec authors.
fn construct_context(src: &str, offset: usize) -> Option<&'static str> {
    const CONSTRUCTS: &[&str] = &[
        "spec",
        "pragma",
        "import",
        "type",
        "const",
        "pda",
        "event",
        "errors",
        "interface",
        "handler",
        "operation",
        "instruction",
        "accounts",
        "requires",
        "ensures",
        "modifies",
        "effect",
        "transfers",
        "emits",
        "guards",
        "match",
        "let",
        "abstract",
        "ref_impl",
        "property",
        "invariant",
        "cover",
        "liveness",
        "environment",
        "auth",
        "call",
        "upstream",
        "state_binders",
    ];
    let clamped = offset.min(src.len());
    src[..clamped].lines().rev().find_map(|line| {
        let first = line.split_whitespace().next()?;
        CONSTRUCTS.iter().find(|k| **k == first).copied()
    })
}

/// Render a chumsky parse error with a `line:col` prefix instead of raw
/// byte offsets, plus the nearest construct keyword as context. Keeps
/// the full `Rich` detail (expected set, reason) so users can still see
/// which tokens were expected.
pub fn format_parse_error(err: &Rich<'_, char>, src: &str) -> String {
    let span = err.span();
    let (line, col) = byte_offset_to_line_col(src, span.start);
    match construct_context(src, span.start) {
        Some(kw) => {
            format!("line {line}, col {col} (while parsing the `{kw}` construct): {err:?}")
        }
        None => format!("line {line}, col {col}: {err:?}"),
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests;
