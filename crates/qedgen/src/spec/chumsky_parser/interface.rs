//! Interface grammar: the `interface` block — program id, `upstream`
//! pin block, interface-handler clauses/decls, and the `interface`
//! declaration.

use super::*;

// ----------------------------------------------------------------------------
// interface Name { program_id "...", upstream { ... }, handler h(args) { ... } }
// ----------------------------------------------------------------------------

/// Internal: items that appear at the top of an `interface` block.
/// Folded into `InterfaceDecl` by the decl combinator.
pub(super) enum InterfaceItem {
    ProgramId(String),
    Upstream(UpstreamDecl),
    StateFields(Vec<TypedField>),
    Handler(InterfaceHandlerDecl),
}

/// Internal: items that appear inside an `upstream { ... }` block.
pub(super) enum UpstreamItem {
    Package(String),
    Version(String),
    Source(String),
    BinaryHash(String),
    IdlHash(String),
    VerifiedWith(Vec<String>),
    VerifiedAt(String),
}

// upstream { package "...", version "...", binary_hash "...", ... }
pub(super) fn upstream_block<'a>() -> impl Parser<'a, &'a str, UpstreamDecl, Err<'a>> + Clone {
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
pub(super) fn interface_handler_clause<'a>(
) -> impl Parser<'a, &'a str, InterfaceHandlerClause, Err<'a>> + Clone {
    let discriminant = kw("discriminant")
        .ignore_then(choice((string_lit(), non_keyword_ident())))
        .map(InterfaceHandlerClause::Discriminant);

    // Interface accounts accept optional commas between descriptors,
    // matching the top-level `accounts { … }` grammar.
    let accounts = just("accounts")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            account_descriptor()
                .then_ignore(wsc())
                .then_ignore(just(',').or_not())
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
pub(super) fn interface_handler_decl<'a>(
) -> impl Parser<'a, &'a str, InterfaceHandlerDecl, Err<'a>> + Clone {
    // Optional return after the params: `-> <Type>` (binding lowers to a
    // `get_return_data` read) or `-> <ident> : <Type>` where the ident is
    // the name the callee's `ensures` uses for the return value (CPI
    // substitution maps it to the caller's `let X = …` binder). Plain
    // `-> <Type>` defaults the binder to `"result"` downstream.
    let named_return = just("->")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .then_ignore(wsc())
        .map(|(name, ty)| (Some(name), ty));
    let bare_return = just("->")
        .then_ignore(wsc())
        .ignore_then(type_ref())
        .then_ignore(wsc())
        .map(|ty| (None, ty));
    // `named_return` first — it has a more specific shape (`ident :`) so
    // chumsky's `choice` resolves greedily without backtracking surprises.
    let return_decl = choice((named_return, bare_return));
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
        .then(return_decl.or_not())
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
        .map(|((((doc, name), params), ret_decl), clauses)| {
            let (result_binder, return_type) = match ret_decl {
                Some((binder, ty)) => (binder, Some(ty)),
                None => (None, None),
            };
            InterfaceHandlerDecl {
                name,
                doc,
                params,
                return_type,
                result_binder,
                clauses,
            }
        })
}

pub(super) fn interface_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    let program_id = kw("program_id")
        .ignore_then(string_lit())
        .map(InterfaceItem::ProgramId);
    let upstream = upstream_block().map(InterfaceItem::Upstream);
    // Interface-level `state { name : Type, ... }` — abstract callee-state
    // vocabulary. Entries separated by commas, newlines, or both. Empty
    // block rejected (at_least(1)); omit the block instead.
    let state_block = kw("state")
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            typed_field()
                .then_ignore(wsc())
                .then_ignore(just(',').or_not())
                .then_ignore(wsc())
                .repeated()
                .at_least(1)
                .collect::<Vec<TypedField>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(InterfaceItem::StateFields);
    let handler = interface_handler_decl().map(InterfaceItem::Handler);

    let item = choice((program_id, upstream, state_block, handler));

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
            let mut state_fields = Vec::new();
            let mut handlers = Vec::new();
            for it in items {
                match it {
                    InterfaceItem::ProgramId(s) => program_id = Some(s),
                    InterfaceItem::Upstream(u) => upstream = Some(u),
                    InterfaceItem::StateFields(fs) => state_fields.extend(fs),
                    InterfaceItem::Handler(h) => handlers.push(h),
                }
            }
            TopItem::Interface(InterfaceDecl {
                name,
                doc,
                program_id,
                upstream,
                state_fields,
                handlers,
            })
        })
}
