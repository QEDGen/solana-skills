//! sBPF / instruction grammar: layout fields and blocks, guard decls,
//! CPI fields, sBPF property clauses, instruction items and the
//! `instruction` declaration.

use super::*;

// layout_field: name : Type @ [-]offset ["desc"]
pub(super) fn layout_field<'a>() -> impl Parser<'a, &'a str, LayoutField, Err<'a>> + Clone {
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

pub(super) fn input_layout_block<'a>() -> impl Parser<'a, &'a str, InstructionItem, Err<'a>> + Clone
{
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

pub(super) fn insn_layout_block<'a>() -> impl Parser<'a, &'a str, InstructionItem, Err<'a>> + Clone
{
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
pub(super) fn guard_decl<'a>() -> impl Parser<'a, &'a str, GuardDecl, Err<'a>> + Clone {
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
        .then_ignore(bare_kw("guard"))
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
pub(super) fn cpi_field<'a>() -> impl Parser<'a, &'a str, (String, String), Err<'a>> + Clone {
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
pub(super) fn sbpf_prop_clause<'a>() -> impl Parser<'a, &'a str, SbpfPropClause, Err<'a>> + Clone {
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
pub(super) fn sbpf_property_decl<'a>() -> impl Parser<'a, &'a str, SbpfPropertyDecl, Err<'a>> + Clone
{
    doc_comments()
        .then_ignore(bare_kw("property"))
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
pub(super) fn instruction_item<'a>() -> impl Parser<'a, &'a str, InstructionItem, Err<'a>> + Clone {
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
        .then(
            // Mirror top-level const_decl: optional `-` so per-instruction
            // consts also support negative literals.
            just('-')
                .or_not()
                .then(integer())
                .try_map(|(sign, v), span| {
                    if v > i128::MAX as u128 {
                        return Err(Rich::custom(span, "const value overflows i128"));
                    }
                    let as_i = v as i128;
                    Ok(if sign.is_some() { -as_i } else { as_i })
                }),
        )
        .map(|(name, value)| InstructionItem::Const { name, value });
    let errors_c = bare_kw("errors")
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
pub(super) fn instruction_decl<'a>() -> impl Parser<'a, &'a str, TopItem, Err<'a>> + Clone {
    doc_comments()
        .then_ignore(bare_kw("instruction"))
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
