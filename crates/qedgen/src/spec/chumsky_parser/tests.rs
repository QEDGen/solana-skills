//! Parser unit tests, moved verbatim from the original
//! `chumsky_parser.rs`. `include_str!` paths gain one extra `../` for
//! the new directory depth.

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

/// `Option T` and `Vec <Record>` parse in State/record field positions
/// (G9/G10 #173/#174) — needed so a State can mirror a real Anchor account
/// struct (`Option<Pubkey>`, `Vec<SmartAccountSigner>`). Before the
/// `type_ref` param rule these were parse errors.
#[test]
fn option_and_vec_record_fields_parse() {
    let src = r#"spec ParamFields
program_id "11111111111111111111111111111111"

type Permissions = { mask : U8 }
type Signer = { key : Pubkey, permissions : Permissions }

type State
  | Active of {
      authority : Option Pubkey,
      quota     : Option U64,
      signers   : Vec Signer,
      count     : U64,
    }

type Error
  | E

handler t : State.Active -> State.Active {
  permissionless
  effect {
    count += 1
  }
}
"#;
    // Before the `type_ref` param rule, the `Option Pubkey` / `Option U64` /
    // `Vec Signer` fields were parse errors; `parse_ok` panics on failure, so
    // reaching here is the regression check.
    let _ = parse_ok(src);
}

/// Index expressions inside a Map slot reference accept dotted
/// state-field paths (`lsts[state.lst_count]`).
#[test]
fn map_index_accepts_dotted_state_field() {
    let src = r#"spec MapIndex
program_id "11111111111111111111111111111111"

type Slot = { active : U8, balance : U64, }

type State
  | Active of {
      lst_count : U64,
      lsts      : Map[MAX] Slot,
    }

const MAX = 8

type Error
  | MathOverflow

handler register : State.Active -> State.Active {
  permissionless
  effect {
    lsts[state.lst_count].active := 1
  }
}
"#;
    let _ = parse_ok(src);
}

/// Deep dotted index expressions (`accounts[a.b.c].field`) — the
/// parser shouldn't special-case depth-1 vs depth-N.
#[test]
fn map_index_accepts_deep_dotted_path() {
    let src = r#"spec DeepIndex
program_id "11111111111111111111111111111111"

type Inner = { idx : U64, }
type Outer = { inner : Inner, }
type State
  | Active of {
      outer : Outer,
      items : Map[MAX] U64,
    }

const MAX = 8

type Error
  | MathOverflow

handler t : State.Active -> State.Active {
  permissionless
  effect {
    items[state.outer.inner.idx] := 1
  }
}
"#;
    let _ = parse_ok(src);
}

#[test]
fn byte_offset_to_line_col_basic() {
    let src = "line1\nline2\nline3";
    assert_eq!(byte_offset_to_line_col(src, 0), (1, 1));
    assert_eq!(byte_offset_to_line_col(src, 5), (1, 6)); // end of "line1"
    assert_eq!(byte_offset_to_line_col(src, 6), (2, 1)); // start of "line2"
    assert_eq!(byte_offset_to_line_col(src, 12), (3, 1)); // start of "line3"
}

#[test]
fn byte_offset_clamps_past_end() {
    // If chumsky reports a span past EOF (unterminated construct), don't
    // panic; clamp to the last valid offset.
    let src = "abc";
    let (line, col) = byte_offset_to_line_col(src, 99);
    assert_eq!((line, col), (1, 4));
}

#[test]
fn parse_error_names_the_enclosing_construct() {
    // #254: a broken handler signature (comma-separated tuple form is
    // invalid — params are curried `(name : Type)` groups) must name the
    // construct being parsed, not just dump char-class expectations.
    let src = "spec Demo\n\nhandler deposit(amount, to) : State.A -> State.B {\n  auth user\n}\n";
    match parse(src) {
        Ok(_) => panic!("expected parse to fail"),
        Err(errs) => {
            let msg = format_parse_error(&errs[0], src);
            assert!(
                msg.contains("`handler` construct"),
                "parse error should name the enclosing construct — got: {msg}"
            );
        }
    }
}

#[test]
fn format_parse_error_prefixes_line_col() {
    // Trigger a parse error and verify the formatter attaches `line X, col Y:`.
    // Use a one-line invalid spec; the error span points into it.
    let src = "spec";
    match parse(src) {
        Ok(_) => panic!("expected parse to fail"),
        Err(errs) => {
            let msg = format_parse_error(&errs[0], src);
            assert!(
                msg.contains("line 1, col"),
                "error should start with `line X, col Y:` — got: {msg}"
            );
            // The raw byte-offset `at N..M` form should NOT appear.
            assert!(
                !msg.contains(" at ") || msg.contains("line "),
                "should not render raw byte offsets without a line:col prefix: {msg}"
            );
        }
    }
}

#[test]
fn string_lit_supports_backslash_newline_continuation() {
    // Long invariant descriptions with `\<newline>` join across lines
    // into a single logical string.
    let src = "spec T\ninvariant foo \"first \\\nsecond\"";
    let s = parse_ok(src);
    match &s.items[0].node {
        TopItem::Invariant(decl) => match &decl.body {
            InvariantBody::Description(text) => {
                assert!(
                    text.starts_with("first ") && text.contains("second"),
                    "expected `first ...second` joined; got: {text:?}"
                );
                assert!(
                    !text.contains('\n'),
                    "backslash-newline must be consumed; got: {text:?}"
                );
            }
            other => panic!("expected Description body, got {other:?}"),
        },
        other => panic!("expected Invariant, got {other:?}"),
    }
}

#[test]
fn string_lit_supports_crlf_continuation() {
    // Spec authored on Windows / mixed line endings still joins.
    let src = "spec T\ninvariant foo \"first \\\r\nsecond\"";
    let s = parse_ok(src);
    let body = match &s.items[0].node {
        TopItem::Invariant(decl) => &decl.body,
        other => panic!("expected Invariant, got {other:?}"),
    };
    let text = match body {
        InvariantBody::Description(t) => t,
        other => panic!("expected Description, got {other:?}"),
    };
    assert!(text.contains("first") && text.contains("second"));
    assert!(!text.contains('\r') && !text.contains('\n'));
}

#[test]
fn string_lit_preserves_existing_escapes() {
    // Regression — \\, \", \n, \t must still produce their literal chars.
    let src = "spec T\ninvariant foo \"tab:\\t newline:\\n quote:\\\" backslash:\\\\\"";
    let s = parse_ok(src);
    let body = match &s.items[0].node {
        TopItem::Invariant(decl) => &decl.body,
        other => panic!("expected Invariant, got {other:?}"),
    };
    let text = match body {
        InvariantBody::Description(t) => t,
        other => panic!("expected Description, got {other:?}"),
    };
    assert!(text.contains("tab:\t"), "got: {text:?}");
    assert!(text.contains("newline:\n"), "got: {text:?}");
    assert!(text.contains("quote:\""), "got: {text:?}");
    assert!(text.contains("backslash:\\"), "got: {text:?}");
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
fn parses_single_line_accounts_block() {
    // B8 repro: comma-separated descriptors on one line must parse.
    let src = r#"spec T
handler foo {
  accounts { admin : signer, battle : writable, pool : writable, pda ["pool"] }
}"#;
    let s = parse_ok(src);
    let h = match &s.items[0].node {
        TopItem::Handler(h) => h,
        o => panic!("expected Handler, got {:?}", o),
    };
    let accounts = h
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Accounts(a) => Some(a),
            _ => None,
        })
        .expect("accounts clause");
    assert_eq!(
        accounts.len(),
        3,
        "expected 3 descriptors, got {:?}",
        accounts
    );
    assert_eq!(accounts[0].name, "admin");
    assert_eq!(accounts[1].name, "battle");
    assert_eq!(accounts[2].name, "pool");
    // pool has two attrs (writable + pda).
    assert_eq!(accounts[2].attrs.len(), 2);
}

#[test]
fn parses_state_sugar_newline_separated() {
    // Documented form in references/qedspec-dsl.md §"state (sugar)".
    let src = "spec T\nstate {\n  balance : U64\n  owner : Pubkey\n}";
    let s = parse_ok(src);
    match &s.items[0].node {
        TopItem::Record(r) => {
            assert_eq!(r.name, "State");
            assert_eq!(r.fields.len(), 2);
            assert_eq!(r.fields[0].name, "balance");
            assert_eq!(r.fields[1].name, "owner");
        }
        o => panic!("expected Record from state sugar, got {:?}", o),
    }
}

#[test]
fn parses_state_sugar_comma_separated() {
    let src = "spec T\nstate { balance : U64, owner : Pubkey }";
    let s = parse_ok(src);
    match &s.items[0].node {
        TopItem::Record(r) => {
            assert_eq!(r.name, "State");
            assert_eq!(r.fields.len(), 2);
        }
        o => panic!("expected Record from state sugar, got {:?}", o),
    }
}

#[test]
fn parses_ref_impl_with_multiple_params_and_if_body() {
    let src = "spec T\n\
                   ref_impl lp_out (s : U64) (p : U64) (amt : U64) : U64 =\n  \
                     if s == 0 then amt else (amt * s) / p\n";
    let s = parse_ok(src);
    match &s.items[0].node {
        TopItem::RefImpl(r) => {
            assert_eq!(r.name, "lp_out");
            assert_eq!(r.params.len(), 3);
            assert_eq!(r.params[0].name, "s");
            assert_eq!(r.params[2].name, "amt");
            match &r.return_type {
                TypeRef::Named(n) => assert_eq!(n, "U64"),
                other => panic!("expected Named return type, got {:?}", other),
            }
        }
        o => panic!("expected RefImpl, got {:?}", o),
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

    // One effect clause with two stmts (effect items are EffectBlock —
    // drill into the leaves via flatten_effect_blocks).
    let effect_clauses: Vec<_> = handler
        .clauses
        .iter()
        .filter_map(|c| match &c.node {
            HandlerClause::Effect(blocks) => Some(blocks),
            _ => None,
        })
        .collect();
    assert_eq!(effect_clauses.len(), 1);
    let blocks = effect_clauses[0];
    let stmts = flatten_effect_blocks(blocks);
    assert_eq!(stmts.len(), 2);
    // Second stmt: accounts[i].capital += amount
    let s2 = stmts[1];
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
fn parses_full_pool_spec() {
    const SRC: &str = include_str!("../../../tests/fixtures/regressions/issue-8/pool.qedspec");
    let s = parse_ok(SRC);
    assert_eq!(s.name, "Pool");

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
            TopItem::Dimension(_) => "dimension",
            TopItem::Pubkey(_) => "pubkey",
            TopItem::Errors(_) => "errors",
            TopItem::Instruction(_) => "instruction",
            TopItem::Interface(_) => "interface",
            TopItem::Pragma(_) => "pragma",
            TopItem::PragmaAssign { .. } => "pragma_assign",
            TopItem::Import { .. } => "import",
            TopItem::Schema(_) => "schema",
            TopItem::RefImpl(_) => "ref_impl",
            TopItem::Ghost(_) => "ghost",
            TopItem::Hook(_) => "hook",
        })
        .fold(
            std::collections::BTreeMap::<&str, usize>::new(),
            |mut m, k| {
                *m.entry(k).or_default() += 1;
                m
            },
        );

    assert_eq!(counts.get("const"), Some(&7), "consts: {:?}", counts);
    assert_eq!(counts.get("adt"), Some(&2), "adts: {:?}", counts); // State + Error
    assert_eq!(counts.get("handler"), Some(&13), "handlers: {:?}", counts);
    assert_eq!(
        counts.get("property"),
        Some(&12),
        "properties: {:?}",
        counts
    );
    assert_eq!(counts.get("cover"), Some(&5), "covers: {:?}", counts);
    assert_eq!(counts.get("event"), Some(&6), "events: {:?}", counts);
    assert_eq!(
        counts.get("invariant"),
        Some(&6),
        "invariants: {:?}",
        counts
    );
    assert_eq!(counts.get("pda"), Some(&4), "pdas: {:?}", counts);
    assert_eq!(
        counts.get("interface"),
        Some(&1),
        "interfaces: {:?}",
        counts
    );
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
    let eff_blocks = h
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Effect(s) => Some(s),
            _ => None,
        })
        .unwrap();
    let eff = flatten_effect_blocks(eff_blocks);
    match &eff[0].rhs.node {
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
fn parses_tuple_variant_of_bare_type() {
    // `Custom of I64` (tuple variant) vs `Windowed of { secs : U32 }` (struct):
    // the tuple form's positional field is named "0" (a marker downstream
    // codegen renders as `Enum::V(val)`); the struct form keeps named fields.
    let src = "spec T\ntype P | OneTime | Custom of I64 | Windowed of { secs : U32 }";
    let s = parse_ok(src);
    let adt = s
        .items
        .iter()
        .find_map(|i| match &i.node {
            TopItem::Adt(a) => Some(a),
            _ => None,
        })
        .expect("adt");
    let custom = adt.variants.iter().find(|v| v.name == "Custom").unwrap();
    assert_eq!(
        custom.fields.len(),
        1,
        "tuple variant has one positional field"
    );
    assert_eq!(custom.fields[0].name, "0", "positional field named \"0\"");
    match &custom.fields[0].ty {
        TypeRef::Named(n) => assert_eq!(n, "I64"),
        o => panic!("expected Named(I64), got {o:?}"),
    }
    let oneshot = adt.variants.iter().find(|v| v.name == "OneTime").unwrap();
    assert!(oneshot.fields.is_empty(), "unit variant has no fields");
    let windowed = adt.variants.iter().find(|v| v.name == "Windowed").unwrap();
    assert_eq!(
        windowed.fields[0].name, "secs",
        "struct variant keeps named fields"
    );
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
    let reset_effect_blocks = reset
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Effect(blocks) => Some(blocks),
            _ => None,
        })
        .unwrap();
    let reset_effect = flatten_effect_blocks(reset_effect_blocks);
    match &reset_effect[0].rhs.node {
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
    let init_effect_blocks = init
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Effect(blocks) => Some(blocks),
            _ => None,
        })
        .unwrap();
    let init_effect = flatten_effect_blocks(init_effect_blocks);
    match &init_effect[0].rhs.node {
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
fn parses_mul_div_round_half_up() {
    let src = r#"
spec T
handler noop (amount : U64) : State -> State {
  requires mul_div_round_half_up(amount, 3, 2) >= 0
}
type State = { total : U64 }
"#;
    let s = parse_ok(src);
    let handler = s
        .items
        .iter()
        .find_map(|item| match &item.node {
            TopItem::Handler(handler) => Some(handler),
            _ => None,
        })
        .unwrap();
    let guard = handler
        .clauses
        .iter()
        .find_map(|clause| match &clause.node {
            HandlerClause::Requires { guard, .. } => Some(guard),
            _ => None,
        })
        .unwrap();
    let Expr::Cmp { lhs, .. } = &guard.node else {
        panic!("expected comparison");
    };
    assert!(matches!(&lhs.node, Expr::MulDivRoundHalfUp { .. }));
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
fn parses_effect_block_match_v220() {
    // `match` inside `effect { … }` (not the handler-level `match`
    // clause).
    let src = r#"spec T
type State | Active of { a : U64, b : U64, c : U64, }
type Error | E
handler route (k : U8) (amount : U64) : State.Active -> State.Active {
  permissionless
  requires amount > 0 else E
  effect {
    match k {
      0 => a += amount,
      1 => b += amount,
      _ => c := 0,
    }
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
        .expect("handler");
    let blocks = h
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Effect(b) => Some(b),
            _ => None,
        })
        .expect("effect clause");
    assert_eq!(blocks.len(), 1, "one top-level effect item");
    match &blocks[0].node {
        EffectBlock::Match { arms, .. } => {
            assert_eq!(arms.len(), 3, "three arms (0, 1, _)");
            match &arms[0].pattern {
                EffectPattern::Literal(v) => assert_eq!(*v, 0),
                o => panic!("expected Literal(0), got {:?}", o),
            }
            match &arms[2].pattern {
                EffectPattern::Wildcard => {}
                o => panic!("expected Wildcard, got {:?}", o),
            }
        }
        o => panic!("expected EffectBlock::Match, got {:?}", o),
    }
    // Flattened leaves: 3 stmts (a += amount, b += amount, c := 0).
    let leaves = flatten_effect_blocks(blocks);
    assert_eq!(leaves.len(), 3);
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
// interface block
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
    let src = "spec T\ninterface Empty {\n  program_id \"11111111111111111111111111111111\"\n}\n";
    let s = parse_ok(src);
    match &s.items[0].node {
        TopItem::Interface(i) => {
            assert_eq!(i.name, "Empty");
            assert!(i.handlers.is_empty());
        }
        o => panic!("expected Interface, got {:?}", o),
    }
}

// `-> <ident> : <Type>` named-result-binding.
#[test]
fn interface_handler_with_explicit_result_binding_parses() {
    let src = r#"spec Demo
interface Pool {
  program_id "11111111111111111111111111111111"

  handler absorb (amount : U64) -> result : U64 {
    requires amount > 0
    ensures  result <= amount
  }
}
"#;
    let s = parse_ok(src);
    let i = match &s.items[0].node {
        TopItem::Interface(i) => i,
        o => panic!("expected Interface, got {:?}", o),
    };
    let h = &i.handlers[0];
    assert_eq!(h.name, "absorb");
    assert_eq!(h.result_binder.as_deref(), Some("result"));
    match &h.return_type {
        Some(TypeRef::Named(n)) => assert_eq!(n, "U64"),
        other => panic!("expected Named return type, got {:?}", other),
    }
}

#[test]
fn interface_handler_with_named_result_binder_parses() {
    // The binder doesn't have to be the word "result" — any
    // identifier is fine (e.g. `price`, `out`, `total`).
    let src = r#"spec Demo
interface Oracle {
  program_id "11111111111111111111111111111111"

  handler quote (base : Pubkey) -> price : U64 {
    ensures price > 0
  }
}
"#;
    let s = parse_ok(src);
    let i = match &s.items[0].node {
        TopItem::Interface(i) => i,
        o => panic!("expected Interface, got {:?}", o),
    };
    let h = &i.handlers[0];
    assert_eq!(h.result_binder.as_deref(), Some("price"));
    match &h.return_type {
        Some(TypeRef::Named(n)) => assert_eq!(n, "U64"),
        other => panic!("expected Named return type, got {:?}", other),
    }
}

#[test]
fn interface_handler_without_result_binding_still_parses() {
    // Back-compat: bare `-> Type` (no named binder) keeps working;
    // `result_binder` is `None` and downstream substitution falls
    // back to the literal "result".
    let src = r#"spec Demo
interface Pool {
  program_id "11111111111111111111111111111111"

  handler absorb (amount : U64) -> U64 {
    requires amount > 0
  }
}
"#;
    let s = parse_ok(src);
    let i = match &s.items[0].node {
        TopItem::Interface(i) => i,
        o => panic!("expected Interface, got {:?}", o),
    };
    let h = &i.handlers[0];
    assert!(h.result_binder.is_none());
    match &h.return_type {
        Some(TypeRef::Named(n)) => assert_eq!(n, "U64"),
        other => panic!("expected Named return type, got {:?}", other),
    }
}

#[test]
fn interface_handler_without_any_return_still_parses() {
    // Back-compat: no `-> …` at all (terminal CPI). Both fields
    // are `None`.
    let src = r#"spec Demo
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  handler transfer (amount : U64) {
    requires amount > 0
  }
}
"#;
    let s = parse_ok(src);
    let i = match &s.items[0].node {
        TopItem::Interface(i) => i,
        o => panic!("expected Interface, got {:?}", o),
    };
    let h = &i.handlers[0];
    assert!(h.result_binder.is_none());
    assert!(h.return_type.is_none());
}

// ------------------------------------------------------------------
// call clause
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
// pragma sbpf { ... }
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
// ML-style `let x = v in body` in expressions
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
fn parses_if_then_else_in_expression_position() {
    // Use an `ensures` clause to exercise expr-position parsing.
    let src = r#"spec T
type State | A of { x : U64, y : U64 }

handler h : State.A -> State.A {
  ensures
    if state.x > 0 then state.y == state.x else state.y == 0
}
"#;
    let s = parse_ok(src);
    // Find the ensures clause and assert its top is an IfThenElse.
    let clauses = first_handler_clauses(&s);
    let ensures = clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Ensures(e) => Some(e),
            _ => None,
        })
        .expect("expected an Ensures clause");
    assert!(
        matches!(ensures.node, Expr::IfThenElse { .. }),
        "expected top-level IfThenElse, got {:?}",
        ensures.node
    );
}

#[test]
fn parses_nested_if_then_else() {
    // Nested then/else branches.
    let src = r#"spec T
type State | A of { x : U64, y : U64 }

handler h : State.A -> State.A {
  ensures
    if state.x > 0 then
      if state.y > 0 then state.x == state.y else state.x > state.y
    else
      state.y == 0
}
"#;
    parse_ok(src);
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

// ----- import statements -----

#[test]
fn parses_single_import() {
    let s = parse_ok("spec T\nimport Token from \"spl_token\"");
    assert_eq!(s.items.len(), 1);
    match &s.items[0].node {
        TopItem::Import {
            name,
            from,
            as_name,
        } => {
            assert_eq!(name, "Token");
            assert_eq!(from, "spl_token");
            assert!(as_name.is_none(), "no `as` clause = None alias");
        }
        other => panic!("expected Import, got {:?}", other),
    }
}

#[test]
fn parses_import_with_as_alias() {
    let s = parse_ok("spec T\nimport Token from \"spl_token\" as MyToken");
    assert_eq!(s.items.len(), 1);
    match &s.items[0].node {
        TopItem::Import {
            name,
            from,
            as_name,
        } => {
            assert_eq!(name, "Token");
            assert_eq!(from, "spl_token");
            assert_eq!(as_name.as_deref(), Some("MyToken"));
        }
        other => panic!("expected Import with alias, got {:?}", other),
    }
}

#[test]
fn parses_multiple_imports() {
    let src = r#"spec T
import Token from "spl_token"
import System from "system_program"
import MyAmm from "my_amm"
"#;
    let s = parse_ok(src);
    assert_eq!(s.items.len(), 3);
    let names: Vec<&str> = s
        .items
        .iter()
        .map(|i| match &i.node {
            TopItem::Import { name, .. } => name.as_str(),
            other => panic!("expected Import, got {:?}", other),
        })
        .collect();
    assert_eq!(names, vec!["Token", "System", "MyAmm"]);
}

#[test]
fn import_does_not_reserve_from_as_global_keyword() {
    // `from` is contextual to import_decl; users must still be able to
    // pass `from = expr` as a call argument inside handler bodies.
    let src = r#"spec T
import Token from "spl_token"

type State | A of { x : U64 }

handler h (a : U64) : State.A -> State.A {
  call Token.transfer(from = a, to = a, amount = 1)
}
"#;
    parse_ok(src);
}

/// `call X.y(state_binders { ... })` parses, with the binders
/// surfacing on the lowered `CallExpr.state_binders`.
#[test]
fn call_accepts_state_binders_block() {
    let src = r#"spec S
type State | A of { pool_balance : U64, user_balance : U64 }

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) { discriminant "0x03" accounts { } }
}

handler deposit (amount : U64) : State.A -> State.A {
  call Token.transfer(
    amount = amount,
    state_binders {
      from_balance = state.pool_balance,
      to_balance   = state.user_balance,
    },
  )
}
"#;
    let s = parse_ok(src);
    // Walk the handler's clauses to find the Call.
    let handler = s
        .items
        .iter()
        .find_map(|i| match &i.node {
            TopItem::Handler(h) if h.name == "deposit" => Some(h),
            _ => None,
        })
        .expect("deposit handler parses");
    let call = handler
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Call(c) => Some(c),
            _ => None,
        })
        .expect("call site parses");
    assert_eq!(call.state_binders.len(), 2);
    assert_eq!(call.state_binders[0].callee_field, "from_balance");
    assert_eq!(call.state_binders[1].callee_field, "to_balance");
}

/// Back-compat: a call without `state_binders { ... }` still parses
/// and yields an empty binder list on the lowered shape.
#[test]
fn call_without_state_binders_is_back_compat() {
    let src = r#"spec S
type State | A of { x : U64 }

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) { discriminant "0x03" accounts { } }
}

handler deposit (amount : U64) : State.A -> State.A {
  call Token.transfer(amount = amount)
}
"#;
    let s = parse_ok(src);
    let handler = s
        .items
        .iter()
        .find_map(|i| match &i.node {
            TopItem::Handler(h) if h.name == "deposit" => Some(h),
            _ => None,
        })
        .expect("deposit handler parses");
    let call = handler
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Call(c) => Some(c),
            _ => None,
        })
        .expect("call site parses");
    assert!(call.state_binders.is_empty());
    assert_eq!(call.args.len(), 1);
    assert_eq!(call.args[0].name, "amount");
}

/// Empty `state_binders { }` block parses (empty binder list).
#[test]
fn call_accepts_empty_state_binders_block() {
    let src = r#"spec S
type State | A of { x : U64 }

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) { discriminant "0x03" accounts { } }
}

handler deposit (amount : U64) : State.A -> State.A {
  call Token.transfer(
    amount = amount,
    state_binders { },
  )
}
"#;
    let s = parse_ok(src);
    let handler = s
        .items
        .iter()
        .find_map(|i| match &i.node {
            TopItem::Handler(h) if h.name == "deposit" => Some(h),
            _ => None,
        })
        .expect("deposit handler parses");
    let call = handler
        .clauses
        .iter()
        .find_map(|c| match &c.node {
            HandlerClause::Call(c) => Some(c),
            _ => None,
        })
        .expect("call site parses");
    assert!(call.state_binders.is_empty());
}

#[test]
fn import_alongside_interface_and_handler() {
    // Import + native interface + handler in the same spec all parse.
    let src = r#"spec T
import Token from "spl_token"

interface Local {
  program_id "11111111111111111111111111111111"
  handler ping { discriminant "0x01" accounts { } }
}

type State | A of { x : U64 }

handler h : State.A -> State.A { effect { x := 1 } }
"#;
    let s = parse_ok(src);
    // Three top items: Import, Interface, Adt, Handler.
    assert_eq!(s.items.len(), 4);
    assert!(matches!(s.items[0].node, TopItem::Import { .. }));
    assert!(matches!(s.items[1].node, TopItem::Interface(_)));
}
