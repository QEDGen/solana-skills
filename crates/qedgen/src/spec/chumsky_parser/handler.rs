//! Handler grammar: params, account attributes/descriptors, effect
//! statements and blocks, handler clauses, and the `handler` declaration.

use super::*;

// Handler params: ML-currying `(i : T) (amount : U)` — each in its own parens.
pub(super) fn handler_param<'a>() -> impl Parser<'a, &'a str, TypedField, Err<'a>> + Clone {
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

/// `name = expr` — one keyword argument inside a `call(...)` arg list.
fn call_kw_arg<'a>() -> impl Parser<'a, &'a str, CallArg, Err<'a>> + Clone {
    non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(expr())
        .map(|(name, value)| CallArg { name, value })
}

/// Comma-separated `name = expr` list (trailing comma allowed) — the
/// `call Target.handler(...)` argument grammar.
fn call_kw_args<'a>() -> impl Parser<'a, &'a str, Vec<CallArg>, Err<'a>> + Clone {
    call_kw_arg()
        .then_ignore(wsc())
        .separated_by(just(',').then_ignore(wsc()))
        .allow_trailing()
        .collect::<Vec<CallArg>>()
}

pub(super) fn account_attr<'a>() -> impl Parser<'a, &'a str, AccountAttr, Err<'a>> + Clone {
    let pda_attr = just("pda")
        .then_ignore(wsc())
        .ignore_then(just('['))
        .then_ignore(wsc())
        .ignore_then(
            // Distinguish string-literal seeds (`"vault"`) from identifier
            // seeds (`creator`) at parse time. Codegen emits the former as
            // `b"vault"` byte-string literals and the latter as
            // `<name>.key().as_ref()` Pubkey accessors. We mark literals by
            // re-attaching the quote chars; the consumer in
            // `codegen_shared::quasar_account_attr` splits on leading `"`.
            choice((
                string_lit().map(|s| format!("\"{}\"", s)),
                non_keyword_ident(),
            ))
            .then_ignore(wsc())
            .separated_by(just(',').then_ignore(wsc()))
            .collect::<Vec<String>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just(']'))
        .map(AccountAttr::Pda);
    // Dotted type refs (`Foreign.State`) alongside bare `token` / `mint` /
    // `State`: first ident is a built-in, local `account_type`, or imported
    // namespace; optional second ident is the type inside it. The adapter
    // splits on `.` to populate `ParsedHandlerAccount::imported_namespace`
    // so codegen routes imported types through `src/imported/<ns>.rs`.
    let type_attr = just("type")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then(just('.').ignore_then(non_keyword_ident()).or_not())
        .map(|(head, tail)| match tail {
            Some(t) => AccountAttr::Type(format!("{}.{}", head, t)),
            None => AccountAttr::Type(head),
        });
    let authority_attr = just("authority")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(AccountAttr::Authority);
    let simple = non_keyword_ident().map(AccountAttr::Simple);
    choice((pda_attr, type_attr, authority_attr, simple))
}

pub(super) fn account_descriptor<'a>(
) -> impl Parser<'a, &'a str, AccountDescriptor, Err<'a>> + Clone {
    // Attr separator is a comma, BUT only when the following tokens don't look
    // like a new descriptor start (`<ident> :`). This lets single-line blocks
    // like `accounts { admin : signer, battle : writable }` parse without the
    // comma being swallowed as "another attribute for `admin`".
    let attr_sep = just(',').then_ignore(wsc()).then_ignore(
        ident()
            .then_ignore(wsc())
            .then_ignore(just(':'))
            .rewind()
            .not(),
    );
    non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(
            account_attr()
                .then_ignore(wsc())
                .separated_by(attr_sep)
                .at_least(1)
                .collect::<Vec<AccountAttr>>(),
        )
        .map(|(name, attrs)| AccountDescriptor { name, attrs })
}

pub(super) fn effect_stmt<'a>() -> impl Parser<'a, &'a str, EffectStmt, Err<'a>> + Clone {
    // Order matters: `+=!` and `+=?` must be tried before `+=`, else the
    // `just("+=")` would greedy-match and leave the `!` / `?` hanging.
    let op = choice((
        just("+=!").to(EffectOp::AddSat),
        just("+=?").to(EffectOp::AddWrap),
        just("+=").to(EffectOp::Add),
        just("-=!").to(EffectOp::SubSat),
        just("-=?").to(EffectOp::SubWrap),
        just("-=").to(EffectOp::Sub),
        just(":=").to(EffectOp::Set),
        just('=').to(EffectOp::Set),
    ));
    // Optional `else <Variant>` suffix on checked `+=` / `-=` (same shape
    // as `requires <expr> else <Err>`; `or` would collide with the boolean
    // infix in `expr()`). Adapter / lint enforce per-op applicability —
    // the parser stays permissive so errors point at the postfix, not `else`.
    let on_error = just("else")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .or_not();
    path()
        .then_ignore(wsc())
        .then(op)
        .then_ignore(wsc())
        .then(expr())
        .then_ignore(wsc())
        .then(on_error)
        .then_ignore(wsc())
        .map(|(((lhs, op), rhs), on_error)| EffectStmt {
            lhs,
            op,
            rhs,
            on_error,
        })
}

/// One item inside an `effect { … }` body: a leaf statement (`x += y`) or
/// a `match`-shape branch.
pub(super) fn effect_block<'a>() -> impl Parser<'a, &'a str, EffectBlock, Err<'a>> + Clone {
    recursive(|effect_block| {
        let wildcard_pat = bare_kw("_").to(EffectPattern::Wildcard);
        let literal_pat = integer().map(EffectPattern::Literal);
        let pattern = choice((wildcard_pat, literal_pat));

        // Note: building Node<EffectBlock> via a regular .map (no
        // span tracking on the inner item) — chumsky 0.12's type
        // inference fails to instantiate `map_with` over the recursive
        // self-reference; using `.map` keeps inference straightforward
        // and the inner span isn't read by downstream consumers.
        let arm = pattern
            .then_ignore(wsc())
            .then_ignore(just("=>"))
            .then_ignore(wsc())
            .then(effect_block.clone().map(|b| Node::new(b, 0..0)))
            .map(|(pattern, nested)| EffectMatchArm {
                pattern,
                body: vec![nested],
            });

        let match_block = just("match")
            .then_ignore(wsc())
            .ignore_then(expr())
            .then_ignore(wsc())
            .then_ignore(just('{'))
            .then_ignore(wsc())
            .then(
                arm.then_ignore(wsc())
                    .separated_by(just(',').then_ignore(wsc()))
                    .allow_trailing()
                    .collect::<Vec<EffectMatchArm>>(),
            )
            .then_ignore(wsc())
            .then_ignore(just('}'))
            .map(|(scrutinee, arms)| EffectBlock::Match { scrutinee, arms });

        choice((match_block, effect_stmt().map(EffectBlock::Stmt)))
    })
}

pub(super) fn handler_clause<'a>() -> impl Parser<'a, &'a str, HandlerClause, Err<'a>> + Clone {
    // Dotted form `auth <acct>.<field>` lets the signing identity live on
    // an imported program's account: the adapter splits on `.` and
    // synthesizes `requires <acct>.<field> == <signer>.pubkey else
    // Unauthorized` against the lone signer. Bare `auth <name>` keeps the
    // state-field lookup behavior.
    let auth = just("auth")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then(just('.').ignore_then(non_keyword_ident()).or_not())
        .map(|(head, tail)| {
            let actor = match tail {
                Some(t) => format!("{}.{}", head, t),
                None => head,
            };
            HandlerClause::Auth(actor)
        });

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

    // `let <ident> = call Foo.handler(...)` binds the call's return value;
    // `let <ident> = <expr>` is the handler-level let. They diverge after
    // `=` — the call form is tried first so the parser doesn't commit to
    // an expression and then choke on `call`.
    enum LetRhs {
        Expr(Node<Expr>),
        Call(QualifiedPath, Vec<CallArg>),
    }
    let let_c = just("let")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(choice((
            just("call")
                .then_ignore(wsc())
                .ignore_then(qualified_path())
                .then_ignore(wsc())
                .then_ignore(just('('))
                .then_ignore(wsc())
                .then(call_kw_args())
                .then_ignore(wsc())
                .then_ignore(just(')'))
                .map(|(target, args)| LetRhs::Call(target, args)),
            expr().map(LetRhs::Expr),
        )))
        .map(|(name, rhs)| match rhs {
            LetRhs::Expr(value) => HandlerClause::Let { name, value },
            LetRhs::Call(target, args) => HandlerClause::Call(CallExpr {
                target,
                args,
                result_binding: Some(name),
                // The bound `let X = call …` form doesn't yet accept a
                // `state_binders { ... }` block (callee-frame semantics).
                state_binders: Vec::new(),
            }),
        });

    // `effect { … }` admits leaf stmts and `match` blocks.
    let effect = just("effect")
        .then_ignore(wsc())
        .ignore_then(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            effect_block()
                .map_with(|b, e| Node::new(b, e.span().into_range()))
                .then_ignore(wsc())
                .then_ignore(just(',').or_not())
                .then_ignore(wsc())
                .repeated()
                .collect::<Vec<Node<EffectBlock>>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .map(HandlerClause::Effect);

    let emits = just("emits")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Emits);

    // `call Interface.handler(args, ...)` inside a match-arm body:
    // captured as `MatchBody::Call`, expanded into a synthetic handler
    // issuing the CPI just like `MatchBody::Effect` expands to a per-arm
    // effect handler (outcome-conditional CPI).
    let match_call = just("call")
        .then_ignore(wsc())
        .ignore_then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just('('))
        .then_ignore(wsc())
        .then(call_kw_args())
        .then_ignore(wsc())
        .then_ignore(just(')'))
        .map(|(target, args)| {
            MatchBody::Call(
                CallExpr {
                    target,
                    args,
                    result_binding: None,
                    // Match-arm CPI doesn't accept `state_binders { ... }`
                    // yet — same callee-frame fallback as the bound form.
                    state_binders: Vec::new(),
                },
                Vec::new(),
            )
        });

    let match_body = choice((
        // abort ErrName
        kw("abort")
            .ignore_then(non_keyword_ident())
            .map(MatchBody::Abort),
        // call Interface.handler(...)
        match_call,
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
    let wildcard_guard = bare_kw("_").to(None::<Node<Expr>>);
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
    // `permissionless` — deliberate opt-out of the no_access_control P1 lint.
    let permissionless = just("permissionless").to(HandlerClause::Permissionless);
    let invariant = just("invariant")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Invariant);
    let establishes = just("establishes")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Establishes);
    let include = just("include")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .map(HandlerClause::Include);

    // `state_binders { callee_field = state.X, ... }` sub-block: maps each
    // callee-side abstract field (LHS) to a caller-side state path (RHS).
    // Contextual — the token is only recognized inside a `call(...)` arg
    // list, so spec authors can still name a handler param `state_binders`.
    let state_binder_entry = non_keyword_ident()
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc())
        .then(expr())
        .map(|(callee_field, caller_expr)| StateBinder {
            callee_field,
            caller_expr,
        });
    // `.boxed()` here is load-bearing: without it, the chumsky combinator
    // type chain that flows into `call_arg_item` pushes the longest
    // mangled symbol name (a `core::ptr::drop_in_place` instantiation for
    // the parser combinator tree) past Apple ld's symbol-string limit
    // (~16KB observed at link time). Boxing erases the type at this seam.
    let state_binders_block = just("state_binders")
        .then_ignore(wsc())
        .then_ignore(just('{'))
        .then_ignore(wsc())
        .ignore_then(
            state_binder_entry
                .then_ignore(wsc())
                .separated_by(just(',').then_ignore(wsc()))
                .allow_trailing()
                .collect::<Vec<StateBinder>>(),
        )
        .then_ignore(wsc())
        .then_ignore(just('}'))
        .boxed();

    // Mixed-arg sequence: each item in the call's arg list is either a
    // `name = expr` keyword arg or the `state_binders { ... }` sub-block.
    #[derive(Debug, Clone)]
    enum CallArgItem {
        Kw(CallArg),
        Binders(Vec<StateBinder>),
    }
    let call_arg_item = choice((
        state_binders_block.map(CallArgItem::Binders),
        call_kw_arg().map(CallArgItem::Kw),
    ));

    // Optional `let <ident> = ` prefix binds the call's return value;
    // without it the call is a terminal statement. The interface handler's
    // return type gives the binding semantics — without one it's opaque.
    let call_let_prefix = kw("let")
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just('='))
        .then_ignore(wsc());
    let call_args = call_arg_item
        .then_ignore(wsc())
        .separated_by(just(',').then_ignore(wsc()))
        .allow_trailing()
        .collect::<Vec<CallArgItem>>()
        .map(|items| {
            let mut args: Vec<CallArg> = Vec::new();
            let mut binders: Vec<StateBinder> = Vec::new();
            for item in items {
                match item {
                    CallArgItem::Kw(a) => args.push(a),
                    // Multiple `state_binders { ... }` blocks concatenate —
                    // friendliest semantics; the adapter dedups by
                    // callee_field anyway.
                    CallArgItem::Binders(mut b) => binders.append(&mut b),
                }
            }
            (args, binders)
        });
    let call_body = just("call")
        .then_ignore(wsc())
        .ignore_then(qualified_path())
        .then_ignore(wsc())
        .then_ignore(just('('))
        .then_ignore(wsc())
        .then(call_args.clone())
        .then_ignore(wsc())
        .then_ignore(just(')'));
    let call_c = choice((
        // Try the bound form first so the bare `call …` doesn't shadow it.
        call_let_prefix.then(call_body.clone()).map(
            |(binding, (target, (args, state_binders)))| {
                HandlerClause::Call(CallExpr {
                    target,
                    args,
                    result_binding: Some(binding),
                    state_binders,
                })
            },
        ),
        call_body.map(|(target, (args, state_binders))| {
            HandlerClause::Call(CallExpr {
                target,
                args,
                result_binding: None,
                state_binders,
            })
        }),
    ));

    // `abstract <name> : <Type>` — existentially-quantified value usable
    // in `requires` / `effect` / `ensures`. Lowers per-backend: Kani
    // `kani::any()` + `assume`, proptest `prop_assume!`, Lean `∃`, Rust
    // `let name: T = todo!(...)`.
    let abstract_c = just("abstract")
        .then_ignore(wsc())
        .ignore_then(non_keyword_ident())
        .then_ignore(wsc())
        .then_ignore(just(':'))
        .then_ignore(wsc())
        .then(type_ref())
        .map(|(name, ty)| HandlerClause::Abstract { name, ty });

    // `choice()` has an arity limit; split into groups.
    let grp_a = choice((auth, accounts, requires, ensures, modifies, let_c, effect));
    let grp_b = choice((transfers, takes, emits, aborts_total, invariant, include));
    let grp_c = choice((match_c, call_c, permissionless, establishes, abstract_c));
    choice((grp_a, grp_b, grp_c))
}

// handler name (params)* : Pre -> Post { clauses }
pub(super) fn handler_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
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
