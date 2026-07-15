//! End-to-end `parse_str` / `adapt` round-trip tests — moved verbatim from
//! the pre-split `chumsky_adapter.rs` test module.

use super::*;

/// Inline stand-in for the retired perp-risk-engine example: consts,
/// a `Fin[...]` alias, a record, a multi-variant ADT `State`, per-requires
/// error names, const substitution in guards, a top-level `match` handler
/// (arm expansion), and property/cover/liveness items — the full-spec
/// shape the adapter must keep round-tripping.
const RISK_ENGINE_SPEC: &str = r#"spec RiskEngine

const MAX_SLOTS = 64
const MAX_VAULT_TVL = 10_000_000_000_000_000

type SlotIdx = Fin[MAX_SLOTS]

type Slot = {
  active  : U8,
  capital : U128,
}

type State
  | Active of {
      authority : Pubkey,
      V         : U128,
      slots     : Map[MAX_SLOTS] Slot,
    }
  | Halted

type Error
  | SlotInactive
  | SlotHealthy
  | BankruptPosition
  | VaultOverflow

handler deposit (i : SlotIdx) (amount : U128) : State.Active -> State.Active {
  auth authority
  accounts { authority : signer, writable
             vault     : writable }

  requires state.slots[i].active == 1 else SlotInactive
  requires state.V + amount <= MAX_VAULT_TVL else VaultOverflow

  effect {
    slots[i].capital += amount
    V += amount
  }
}

handler liquidate (i : SlotIdx) : State.Active -> State.Active {
  auth authority
  accounts { authority : signer
             vault     : writable }

  requires state.slots[i].active == 1 else SlotInactive

  match
    | state.slots[i].capital >= 1 =>
        abort SlotHealthy
    | state.slots[i].capital == 0 =>
        effect { slots[i].active := 0 }
    | _ =>
        abort BankruptPosition
}

handler halt : State.Active -> State.Halted {
  auth authority
  accounts { authority : signer }
}

handler resume : State.Halted -> State.Active {
  auth authority
  accounts { authority : signer }
}

property conservation :
  state.V >= (sum i : SlotIdx, state.slots[i].capital)
  preserved_by all

cover happy_path [deposit, liquidate]

liveness engine_recovers : State.Halted ~> State.Active via [resume] within 1
"#;

/// `Map[<EnumType>] T` is recognized when the bound names a unit-only
/// enum (all variants payload-free).
#[test]
fn map_keyed_by_enum_routes_to_sum_types() {
    let src = r#"spec EnumMap
program_id "11111111111111111111111111111111"

type AddressField
  | Owner
  | Manager
  | Treasury

type ProposalSlot = { proposed : Pubkey, deadline : U64, }

type State
  | Active of {
      proposals : Map[AddressField] ProposalSlot,
    }

type Error
  | NoMatch
"#;
    let spec = parse_str(src).expect("parse");
    // AddressField should route to sum_types (not account_types)
    // because it's used as a Map key.
    let has_sum = spec.sum_types.iter().any(|s| s.name == "AddressField");
    assert!(
        has_sum,
        "AddressField should land in sum_types when used as a Map key; got sum_types: {:?}",
        spec.sum_types.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    // And the unit-only check passes (all variants payload-free).
    let af = spec
        .sum_types
        .iter()
        .find(|s| s.name == "AddressField")
        .unwrap();
    assert!(
        af.variants.iter().all(|v| v.fields.is_empty()),
        "AddressField should be unit-only"
    );
}

/// `state := .Variant { f := e, ... }` desugars into per-field effects
/// with variant-prefixed LHS for `emit_cross_variant_promotion`.
#[test]
fn state_variant_promotion_expands_to_per_field_effects() {
    let src = r#"spec Lifecycle
program_id "11111111111111111111111111111111"

type State
  | Uninitialized
  | Setup of { admin : Pubkey, balance : U64, }

type Error
  | WrongState

handler initialize : State.Uninitialized -> State.Setup {
  accounts {
    admin : signer
  }
  effect {
    state := .Setup { admin := admin.pubkey, balance := 0 }
  }
}
"#;
    let spec = parse_str(src).expect("parse");
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "initialize")
        .expect("initialize handler");
    // The single `state := .Setup { ... }` effect should have
    // expanded into two per-field effects with variant-prefixed
    // LHS, not a single bare-state effect.
    let lhs_strs: Vec<&String> = handler.effects.iter().map(|e| &e.field).collect();
    assert!(
        lhs_strs.iter().any(|s| s.as_str() == "Setup.admin"),
        "expected Setup.admin in effect LHS list; got: {:?}",
        lhs_strs
    );
    assert!(
        lhs_strs.iter().any(|s| s.as_str() == "Setup.balance"),
        "expected Setup.balance in effect LHS list; got: {:?}",
        lhs_strs
    );
    assert!(
        !lhs_strs.iter().any(|s| s.as_str() == "state"),
        "bare-state LHS should have been desugared away; got: {:?}",
        lhs_strs
    );
}

/// `abstract <name> : <Type>` clauses parse into
/// `ParsedHandler::abstract_binders` as (name, verbatim DSL type).
#[test]
fn abstract_binder_clause_parses_into_handler() {
    let src = r#"spec Earn
program_id "11111111111111111111111111111111"

type State
  | Active of { lp_supply : U64 }

type Error
  | InvalidAmount

handler user_deposit (amount_stablecoin : U64) : State.Active -> State.Active {
  accounts { user : signer }
  abstract d : U64
  requires d > 0 else InvalidAmount
  requires d <= amount_stablecoin else InvalidAmount
  effect { lp_supply += d }
}
"#;
    let spec = parse_str(src).expect("parse");
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "user_deposit")
        .expect("user_deposit handler");
    assert_eq!(handler.abstract_binders.len(), 1);
    assert_eq!(handler.abstract_binders[0].0, "d");
    assert_eq!(handler.abstract_binders[0].1, "U64");
}

/// Negative integer literals desugar to `Arith { Sub, Int(0), Int(v) }`
/// at parse time.
#[test]
fn negative_integer_literal_parses_as_sub_of_zero() {
    let src = r#"spec Exp
program_id "11111111111111111111111111111111"

type State
  | Active of { exp : U64 }

type Error
  | Bad

handler set_exp : State.Active -> State.Active {
  accounts { authority : signer }
  requires state.exp == -4 else Bad
  effect { exp := 0 }
}
"#;
    // The successful parse is the assertion.
    let _spec = parse_str(src).expect("negative literal must parse");
}

/// `const NAME = -VALUE` parses with the value stored as a signed `i128`.
#[test]
fn const_decl_accepts_negative_literal() {
    let src = r#"spec ExpConst
program_id "11111111111111111111111111111111"

const N6 = -6

type State
  | Active of { exp : U64 }

type Error
  | Bad
"#;
    let spec = parse_str(src).expect("parse");
    let n6 = spec
        .constants
        .iter()
        .find(|(n, _)| n == "N6")
        .expect("N6 const must exist");
    assert_eq!(n6.1, "-6");
}

/// Unit-variant promotion (`state := .Closed`) drops to zero effects;
/// the wrapper assignment in `emit_cross_variant_promotion` handles the
/// transition from `handler.post_status` directly.
#[test]
fn state_unit_variant_promotion_emits_no_effects() {
    let src = r#"spec Lifecycle
program_id "11111111111111111111111111111111"

type State
  | Open of { x : U64 }
  | Closed

type Error
  | WrongState

handler close : State.Open -> State.Closed {
  accounts {
    authority : signer
  }
  effect {
    state := .Closed
  }
}
"#;
    let spec = parse_str(src).expect("parse");
    let handler = spec
        .handlers
        .iter()
        .find(|h| h.name == "close")
        .expect("close handler");
    assert!(
        handler.effects.is_empty(),
        "unit-variant promotion should desugar to zero effects; got: {:?}",
        handler.effects
    );
    assert_eq!(handler.post_status.as_deref(), Some("Closed"));
}

/// `call Interface.handler(...)` is legal inside a match arm body; the
/// call-arm synth gets the CPI on its `calls` slot (same shape as a
/// top-level call clause).
#[test]
fn match_arm_accepts_call_body() {
    let src = r#"spec MatchCall
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb_loss (amount : U64) {
    accounts { vault : writable }
  }
}

type State
  | Active of { pnl : I64 }

type Error
  | NoMatch

handler liquidate (loss : U64) : State.Active -> State.Active {
  permissionless
  match
    | state.pnl < 0 => call Pool.absorb_loss(amount = loss)
    | _ => abort NoMatch
}
"#;
    let spec = parse_str(src).expect("parse");
    // The match clause expands into one synth handler per arm.
    // The call-arm synth should have one ParsedCall on it.
    let synths: Vec<_> = spec
        .handlers
        .iter()
        .filter(|h| h.name.starts_with("liquidate"))
        .collect();
    let with_call: Vec<_> = synths.iter().filter(|h| !h.calls.is_empty()).collect();
    assert_eq!(
        with_call.len(),
        1,
        "expected exactly one synth handler with a call body; got {} synths total, {} with calls",
        synths.len(),
        with_call.len()
    );
    assert_eq!(with_call[0].calls[0].target_interface, "Pool");
    assert_eq!(with_call[0].calls[0].target_handler, "absorb_loss");
}

/// `let X = call Foo.handler(...)` records the binding name on
/// `ParsedCall.result_binding`.
#[test]
fn call_with_let_binding_records_result_name() {
    let src = r#"spec CallLet
program_id "11111111111111111111111111111111"

interface Pool {
  program_id "11111111111111111111111111111111"
  handler absorb_loss (amount : U64) {
    accounts {
      vault : writable
    }
  }
}

type State
  | Active of { total_loss : U64 }

type Error
  | MathOverflow

handler liquidate (loss : U64) : State.Active -> State.Active {
  permissionless
  let burned = call Pool.absorb_loss(amount = loss)
  effect { Active.total_loss += loss }
}

handler unbound_call : State.Active -> State.Active {
  permissionless
  call Pool.absorb_loss(amount = 1)
  effect { Active.total_loss += 1 }
}
"#;
    let spec = parse_str(src).expect("parse");
    let liquidate = spec
        .handlers
        .iter()
        .find(|h| h.name == "liquidate")
        .expect("liquidate handler");
    let unbound = spec
        .handlers
        .iter()
        .find(|h| h.name == "unbound_call")
        .expect("unbound_call handler");
    assert_eq!(liquidate.calls.len(), 1);
    assert_eq!(
        liquidate.calls[0].result_binding.as_deref(),
        Some("burned"),
        "result_binding should carry the `let` name; got: {:?}",
        liquidate.calls[0].result_binding
    );
    assert_eq!(unbound.calls.len(), 1);
    assert_eq!(
        unbound.calls[0].result_binding, None,
        "bare `call …` keeps result_binding None"
    );
}

/// Top-level `schema name { requires … }` blocks parse, and `include
/// <schema>` expands every schema requires into the handler's requires
/// list at adapt time.
#[test]
fn schema_include_expands_into_handler_requires() {
    let src = r#"spec SchemaDemo
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, paused : U8 }

type Error
  | Paused
  | MathOverflow

schema gated_by_pause {
  requires state.paused == 0 else Paused
}

handler deposit (amount : U64) : State.Active -> State.Active {
  permissionless
  include gated_by_pause
  requires amount > 0 else MathOverflow
  effect { Active.balance += amount }
}

handler withdraw (amount : U64) : State.Active -> State.Active {
  permissionless
  include gated_by_pause
  requires amount > 0 else MathOverflow
  effect { Active.balance -= amount }
}
"#;
    let spec = parse_str(src).expect("parse");
    // Both handlers got the schema's `requires state.paused == 0`
    // appended to their existing `amount > 0` clause.
    for handler_name in ["deposit", "withdraw"] {
        let h = spec
            .handlers
            .iter()
            .find(|h| h.name == handler_name)
            .unwrap_or_else(|| panic!("missing handler {handler_name}"));
        assert!(
                h.requires.iter().any(|r| r.lean_expr.contains("paused")),
                "handler {handler_name} should pick up `paused` requires from gated_by_pause; got: {:?}",
                h.requires.iter().map(|r| &r.lean_expr).collect::<Vec<_>>()
            );
        assert!(
            h.requires
                .iter()
                .any(|r| r.error_name.as_deref() == Some("Paused")),
            "handler {handler_name} should pick up the schema's Paused error; got: {:?}",
            h.requires.iter().map(|r| &r.error_name).collect::<Vec<_>>()
        );
        assert!(
            h.schema_includes.contains(&"gated_by_pause".to_string()),
            "handler {handler_name} should remember its includes list"
        );
    }
    // Schema is also surfaced on spec.schemas for downstream
    // consumers (lint / docs / future tooling).
    assert!(
        spec.schemas.iter().any(|s| s.name == "gated_by_pause"),
        "spec.schemas should list gated_by_pause; got: {:?}",
        spec.schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// `preserved_by all except [h1, h2]` expands to the full handler list
/// minus the excluded names ("every handler other than the one whose
/// job is to break it").
#[test]
fn preserved_by_all_except_expands_to_complement() {
    let src = r#"spec ExceptDemo
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, paused : U8 }

type Error
  | MathOverflow

handler deposit (amount : U64) : State.Active -> State.Active {
  permissionless
  requires amount > 0 else MathOverflow
  effect { Active.balance += amount }
}

handler pause : State.Active -> State.Active {
  permissionless
  effect { Active.paused := 1 }
}

handler unpause : State.Active -> State.Active {
  permissionless
  effect { Active.paused := 0 }
}

property still_unpaused :
  state.paused == 0
  preserved_by all except [pause]
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "still_unpaused")
        .expect("property still_unpaused");
    let names: std::collections::HashSet<&str> =
        prop.preserved_by.iter().map(String::as_str).collect();
    assert!(
        names.contains("deposit"),
        "expected deposit in preserved_by; got: {:?}",
        prop.preserved_by
    );
    assert!(
        names.contains("unpause"),
        "expected unpause in preserved_by; got: {:?}",
        prop.preserved_by
    );
    assert!(
        !names.contains("pause"),
        "pause should be excluded; got: {:?}",
        prop.preserved_by
    );
    assert!(
        !names.contains("all"),
        "sentinel `all` should be expanded away; got: {:?}",
        prop.preserved_by
    );
}

/// `invariant Foo` and `establishes Foo` route to distinct
/// `ParsedHandler` fields. Backends key off the split: invariants →
/// preserves (assume pre-state), establishes → no pre-assume.
#[test]
fn handler_invariant_clauses_route_to_invariants_vs_establishes() {
    let src = include_str!(
        "../../../tests/fixtures/regressions/invariants/repro-establishes-clause.qedspec"
    );
    let spec = parse_str(src).expect("parse");
    let init = spec
        .handlers
        .iter()
        .find(|h| h.name == "init")
        .expect("init handler");
    let update = spec
        .handlers
        .iter()
        .find(|h| h.name == "update")
        .expect("update handler");
    assert_eq!(init.establishes, vec!["root_set".to_string()]);
    assert!(init.invariants.is_empty(), "init only `establishes`");
    assert_eq!(update.invariants, vec!["root_set".to_string()]);
    assert!(
        update.establishes.is_empty(),
        "update only `invariant` (preserves)"
    );
}

#[test]
fn handler_invariant_clause_routes_to_invariants() {
    let src = include_str!(
        "../../../tests/fixtures/regressions/invariants/repro-handler-invariant-clause.qedspec"
    );
    let spec = parse_str(src).expect("parse");
    for h in &spec.handlers {
        assert_eq!(
            h.invariants,
            vec!["count_bounded".to_string()],
            "handler {} should list count_bounded as `invariant`",
            h.name
        );
        assert!(h.establishes.is_empty());
    }
    // The top-level invariant decl carries the predicate body that the
    // adapter lowers via translate_property_to_rust.
    let inv = spec
        .invariants
        .iter()
        .find(|i| i.name == "count_bounded")
        .expect("count_bounded invariant decl");
    assert!(inv.lean_expr.is_some(), "lean_expr populated");
    assert!(inv.rust_expr.is_some(), "rust_expr populated");
    let rust = inv.rust_expr.as_deref().unwrap();
    assert!(
        rust.contains("s.count"),
        "rust_expr should reference s.count, got: {rust}"
    );
}

// Regression: Pubkey := <int> must be rejected at check time, not
// deferred to lake build's "OfNat Pubkey 0" error.
#[test]
fn finding_7_pubkey_assign_from_int_rejected() {
    let src = include_str!(
        "../../../tests/fixtures/regressions/issue-8/repro-07-pubkey-literal-assign.qedspec"
    );
    let err = parse_str(src).expect_err("expected Pubkey := 0 to fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("Pubkey field cannot be assigned a numeric literal"),
        "unexpected error message: {msg}"
    );
}

// Regression: state.<Pubkey> != <int> in a `requires` clause must also
// be rejected.
#[test]
fn finding_8_pubkey_compare_with_int_rejected() {
    let src = include_str!(
        "../../../tests/fixtures/regressions/issue-8/repro-08-pubkey-literal-compare.qedspec"
    );
    let err = parse_str(src).expect_err("expected state.key != 0 to fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("compares Pubkey"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn finding_7_pubkey_assign_from_numeric_const_rejected() {
    let src = r#"spec Repro7Const
program_id "11111111111111111111111111111111"
const ZERO = 0
type State
  | Uninitialized
  | Active of { key : Pubkey }
type Error | E
handler h : State.Uninitialized -> State.Active {
  permissionless
  effect { key := ZERO }
}
"#;
    let err = parse_str(src).expect_err("expected key := ZERO to fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("Pubkey field cannot be assigned a numeric literal"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn finding_8_pubkey_compare_with_numeric_const_rejected() {
    let src = r#"spec Repro8Const
program_id "11111111111111111111111111111111"
const ZERO = 0
type State
  | Uninitialized
  | Active of { key : Pubkey }
type Error | E
handler h : State.Active -> State.Active {
  permissionless
  requires state.key != ZERO else E
  effect { }
}
"#;
    let err = parse_str(src).expect_err("expected state.key != ZERO to fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("compares Pubkey"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn pubkey_param_paths_remain_allowed_with_numeric_consts_present() {
    let src = r#"spec ReproConstGuard
program_id "11111111111111111111111111111111"
const ZERO = 0
type State
  | Uninitialized
  | Active of { key : Pubkey }
type Error | E
handler h (k : Pubkey) : State.Active -> State.Active {
  permissionless
  requires state.key != k else E
  effect { key := k }
}
"#;
    parse_str(src).expect("Pubkey param assignment/comparison should remain valid");
}

// Guard: bundled specs that legitimately compare/assign Pubkey
// must not regress (e.g. `signer == state.admin`, `state.pk := p`).
#[test]
fn pubkey_typecheck_does_not_break_bundled_examples() {
    for src in [
        include_str!("../../../../../examples/rust/escrow/escrow.qedspec"),
        include_str!("../../../../../examples/rust/lending/lending.qedspec"),
        include_str!("../../../../../examples/rust/multisig/multisig.qedspec"),
        include_str!("../../../tests/fixtures/regressions/issue-8/pool.qedspec"),
    ] {
        parse_str(src).unwrap();
    }
}

// Structural smoke test — the risk-engine spec produces the shape we
// expect. When pest existed this compared parser-for-parser; now it's
// a regression fence against future adapter changes.
#[test]
fn risk_engine_shape() {
    let spec = parse_str(RISK_ENGINE_SPEC).expect("chumsky parse");
    // 3 plain handlers + `liquidate` expanded into 3 branch arms = 6.
    assert_eq!(spec.handlers.len(), 6);
    assert_eq!(spec.properties.len(), 1);
    assert_eq!(spec.covers.len(), 1);
    assert_eq!(spec.liveness_props.len(), 1);

    let deposit = spec.handlers.iter().find(|h| h.name == "deposit").unwrap();
    assert_eq!(deposit.requires.len(), 2);
    assert_eq!(
        deposit.requires[0].error_name,
        Some("SlotInactive".to_string())
    );

    // Const substitution in guards: MAX_VAULT_TVL should be inlined.
    assert!(deposit.requires[1].lean_expr.contains("10000000000000000"));
}

// B1 regression: ADTs with multiple variants sharing the same field
// names must produce a SINGLE entry per field (first-variant wins), not
// a struct with N copies of each field.
#[test]
fn adt_variants_with_shared_fields_deduplicate() {
    let src = r#"spec T
type Battle
  | Active  of { pool : U64, status : U8 }
  | Frozen  of { pool : U64, status : U8 }
  | Settled of { pool : U64, status : U8 }
"#;
    let spec = parse_str(src).expect("parse");
    assert_eq!(spec.account_types.len(), 1);
    let at = &spec.account_types[0];
    assert_eq!(at.name, "Battle");
    assert_eq!(
        at.fields.len(),
        2,
        "shared-field variants must dedupe to 2 fields, got {:?}",
        at.fields
    );
    let names: Vec<&str> = at.fields.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["pool", "status"]);
    // Lifecycle retains every variant name (Active/Frozen/Settled) for
    // Status enum generation.
    assert_eq!(at.lifecycle, vec!["Active", "Frozen", "Settled"]);
}

// Regression: property bodies referencing `state.x` must render as
// `s.x` in the Rust form — `s` is the function parameter that
// `emit_property_predicates` binds.
#[test]
fn property_state_root_renders_as_s_in_rust() {
    let src = r#"spec T
state { x : U64 }
property x_bounded :
  state.x >= 0
  preserved_by all
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "x_bounded")
        .expect("property");
    let rust = prop.rust_expression.as_deref().expect("rust rendering");
    assert!(
        rust.contains("s.x"),
        "state.x should render as s.x, got: {}",
        rust
    );
    assert!(
        !rust.contains("state."),
        "no residual `state.` prefix in rust form: {}",
        rust
    );
}

// B2 regression: `implies` and `forall` must not leak Unicode symbols into
// the Rust rendering of a property body.
#[test]
fn property_implies_renders_to_valid_rust() {
    let src = r#"spec T
state { x : U8 }
property implies_case :
  state.x == 2 implies state.x >= 2
  preserved_by all
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "implies_case")
        .expect("property");
    let rust = prop.rust_expression.as_deref().expect("rust rendering");
    // No lingering Lean arrows that would mojibake as `â` in downstream Rust.
    assert!(!rust.contains('\u{2192}'), "rust form has → : {}", rust);
    // Explicit desugaring check: `implies` must lower to `!(…) || (…)`.
    assert!(rust.contains("!("), "expected negation in: {}", rust);
    assert!(rust.contains("||"), "expected disjunction in: {}", rust);
    assert!(
        !crate::check::rust_expr_is_unsupported(rust),
        "implies should lower, not be marked unsupported: {}",
        rust
    );
}

#[test]
fn property_forall_u8_lowers_to_iterator() {
    // U8 is small enough to exhaust (256 values) — must not emit the
    // unsupported sentinel; must lower to a `.all(|v| …)` expression.
    let src = r#"spec T
state { x : U8 }
property forall_case :
  forall v : U8, v >= 0
  preserved_by all
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "forall_case")
        .expect("property");
    let rust = prop.rust_expression.as_deref().expect("rust rendering");
    assert!(
        !crate::check::rust_expr_is_unsupported(rust),
        "U8 forall must lower to an iterator, not emit the unsupported marker: {}",
        rust
    );
    assert!(
        rust.contains("u8::MIN") && rust.contains("u8::MAX"),
        "must use u8 range: {}",
        rust
    );
    assert!(rust.contains(".all("), "must use .all(): {}", rust);
    assert!(
        !rust.contains('\u{2200}'),
        "rust must not contain ∀: {}",
        rust
    );
}

#[test]
fn property_forall_large_type_marked_unsupported_in_rust() {
    // U64 cannot be exhausted in a test loop — must still emit the sentinel.
    let src = r#"spec T
state { x : U64 }
property forall_u64 :
  forall v : U64, v >= 0
  preserved_by all
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "forall_u64")
        .expect("property");
    let rust = prop.rust_expression.as_deref().expect("rust rendering");
    assert!(
        crate::check::rust_expr_is_unsupported(rust),
        "U64 forall must still emit the unsupported sentinel: {}",
        rust
    );
    assert!(
        rust.trim_start().starts_with("/*"),
        "marker must be a Rust block comment: {}",
        rust
    );
    assert!(
        rust.trim_end().ends_with("*/"),
        "marker must close the comment: {}",
        rust
    );
    assert!(
        !rust.contains('\u{2200}'),
        "rust must not contain ∀: {}",
        rust
    );
}

// ----- adapter populates ParsedSpec.imports -----

#[test]
fn adapter_populates_imports() {
    let src = r#"spec T
import Token from "spl_token"
import MyAmm from "my_amm"
"#;
    let spec = parse_str(src).expect("parse");
    assert_eq!(spec.imports.len(), 2);
    assert_eq!(spec.imports[0].name, "Token");
    assert_eq!(spec.imports[0].from, "spl_token");
    assert_eq!(spec.imports[1].name, "MyAmm");
    assert_eq!(spec.imports[1].from, "my_amm");
}

#[test]
fn adapter_imports_empty_for_specs_without_import_stmts() {
    let src = r#"spec T
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#;
    let spec = parse_str(src).expect("parse");
    assert!(spec.imports.is_empty());
}

// ----- if-then-else expressions -----

#[test]
fn if_then_else_renders_to_lean_native_form() {
    let src = r#"spec T
type State | A of { x : U64, y : U64 }
property if_branch :
  if state.x > 0 then state.y == state.x else state.y == 0
  preserved_by all
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "if_branch")
        .expect("property");
    let lean = prop.expression.as_deref().expect("lean rendering");
    // Lean's native if-then-else syntax. State fields prefix with `s.`
    // in Ctx::Guard.
    assert!(
        lean.contains("if s.x > 0 then s.y = s.x else s.y = 0"),
        "expected native Lean if-then-else; got: {}",
        lean
    );
}

#[test]
fn if_then_else_renders_to_rust_block_form() {
    let src = r#"spec T
type State | A of { x : U64, y : U64 }
property if_branch :
  if state.x > 0 then state.y == state.x else state.y == 0
  preserved_by all
"#;
    let spec = parse_str(src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == "if_branch")
        .unwrap();
    let rust = prop.rust_expression.as_deref().expect("rust rendering");
    assert!(
        rust.contains("if s.x > 0 { s.y == s.x } else { s.y == 0 }"),
        "expected Rust block-form if-else; got: {}",
        rust
    );
}

// ----- `now()` builtin -----

#[test]
fn now_builtin_parses_in_effect() {
    let src = r#"spec NowTest
type State | Active of { last_update : U64 }
handler refresh : State.Active -> State.Active {
  permissionless
  effect { last_update := now() }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec.handlers.iter().find(|h| h.name == "refresh").unwrap();
    // Effect RHS for complex expressions is captured in Lean form
    // (consumed by lean_gen). `now()` lowers to the bare `now` symbol
    // which resolves at elaboration via QEDGen.Solana.Valid.now.
    let rhs = &h
        .effects
        .iter()
        .find(|e| e.field == "last_update")
        .expect("last_update effect")
        .value;
    assert_eq!(
        rhs.trim(),
        "now",
        "Lean rendering of now() should be the bare ident `now`; got: {rhs}"
    );
}

#[test]
fn now_builtin_parses_in_requires() {
    let src = r#"spec NowReq
type State | Active of { last_update : U64 }
type Error | TooSoon
handler refresh : State.Active -> State.Active {
  permissionless
  requires state.last_update + 60 <= now() else TooSoon
  effect { last_update := state.last_update + 1 }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec.handlers.iter().find(|h| h.name == "refresh").unwrap();
    let req = h.requires.first().expect("requires clause");
    // Lean form references the support-library axiom by its unqualified
    // name; QEDGen.Solana.Valid.now resolves after `open QEDGen.Solana`.
    assert!(
        req.lean_expr.contains("now"),
        "lean expr should mention now; got: {}",
        req.lean_expr
    );
    assert!(
        req.rust_expr.contains("Clock::get"),
        "rust expr should mention Clock::get; got: {}",
        req.rust_expr
    );
}

/// `current_epoch()` parses as a zero-arg builtin: Rust
/// `Clock::get().unwrap().epoch`, Lean bare ident `current_epoch`
/// (axiomatized at QEDGen.Solana.Valid).
#[test]
fn current_epoch_builtin_parses_in_requires() {
    let src = r#"spec EpochReq
type State | Active of { last_epoch : U64 }
type Error | StaleEpoch
handler refresh : State.Active -> State.Active {
  permissionless
  requires state.last_epoch < current_epoch() else StaleEpoch
  effect { last_epoch := current_epoch() }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec.handlers.iter().find(|h| h.name == "refresh").unwrap();
    let req = h.requires.first().expect("requires clause");
    assert!(
        req.lean_expr.contains("current_epoch"),
        "lean expr should reference current_epoch; got: {}",
        req.lean_expr
    );
    assert!(
        req.rust_expr.contains("Clock::get"),
        "rust expr should mention Clock::get; got: {}",
        req.rust_expr
    );
    assert!(
        req.rust_expr.contains(".epoch"),
        "rust expr should read .epoch (not .unix_timestamp); got: {}",
        req.rust_expr
    );
}

// ========================================================================
// Property classification snapshot tests
// ========================================================================

/// Helper: parse a tiny spec and return the named property's class.
fn class_of(spec_src: &str, prop_name: &str) -> crate::check::PropertyClass {
    let spec = parse_str(spec_src).expect("parse");
    let prop = spec
        .properties
        .iter()
        .find(|p| p.name == prop_name)
        .unwrap_or_else(|| panic!("property `{}` not found", prop_name));
    prop.class
}

const CLASSIFY_SPEC_HEAD: &str = r#"
spec ClassifyTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, settled : U64, admin : U64 }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}
"#;

#[test]
fn classify_property_bare_comparison_is_unary() {
    // No `old(...)`, no temporal markers — single-state predicate.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD, r#"property balance_nonneg : state.balance >= 0 preserved_by all"#
    );
    assert_eq!(
        class_of(&src, "balance_nonneg"),
        crate::check::PropertyClass::Unary
    );
}

#[test]
fn classify_property_with_single_old_is_binary() {
    // `old(state.x)` anywhere ⇒ Binary — routed through the binary
    // preservation harness instead of silently lowering to `s.x >= s.x`.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
    );
    assert_eq!(
        class_of(&src, "balance_monotonic"),
        crate::check::PropertyClass::Binary
    );
}

#[test]
fn classify_property_with_old_under_not_is_binary() {
    // `old(...)` nested under boolean negation still triggers Binary.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"property settled_changed : not (state.settled == old(state.settled)) preserved_by all"#
    );
    assert_eq!(
        class_of(&src, "settled_changed"),
        crate::check::PropertyClass::Binary
    );
}

#[test]
fn classify_property_with_old_in_implication_is_binary() {
    // `old(...)` on the LHS of an implication body — Binary.
    // Mirrors `vectors_seeded_latches_true` from pool.qedspec:694.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"property latches : old(state.settled) == 1 implies state.settled == 1 preserved_by all"#
    );
    assert_eq!(
        class_of(&src, "latches"),
        crate::check::PropertyClass::Binary
    );
}

#[test]
fn classify_property_constant_body_is_unary() {
    // No state refs at all — Unary. Lowers to a constant predicate.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD, r#"property trivially_true : 1 == 1 preserved_by all"#
    );
    assert_eq!(
        class_of(&src, "trivially_true"),
        crate::check::PropertyClass::Unary
    );
}

#[test]
fn environment_constraints_retain_typed_temporal_metadata() {
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"
environment rate_change {
  mutates balance : U64
  constraint state.balance >= old(state.balance)
  constraint state.balance > 0
}
"#
    );
    let spec = parse_str(&src).expect("parse environment constraints");
    let environment = &spec.environments[0];

    assert_eq!(environment.typed_constraints.len(), 2);
    for (index, typed) in environment.typed_constraints.iter().enumerate() {
        assert!(
            typed.tree.is_some(),
            "constraint {index} must retain its tree"
        );
    }
    assert_eq!(
        environment.typed_constraints[0].class,
        crate::check::PropertyClass::Binary
    );
    assert_eq!(
        environment.typed_constraints[1].class,
        crate::check::PropertyClass::Unary
    );
}

#[test]
fn environment_external_fields_use_a_distinct_typed_namespace() {
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"
environment clock_advance {
  external clock.slot : U64
  constraint clock.slot >= old(clock.slot)
}
"#
    );
    let spec = parse_str(&src).expect("parse external environment");
    let environment = &spec.environments[0];
    assert_eq!(
        environment.external_fields,
        vec![("clock".into(), "slot".into(), "U64".into())]
    );
    let tree = environment.typed_constraints[0]
        .tree
        .as_ref()
        .expect("typed external constraint");
    let crate::mir::ExprTree::Cmp { lhs, rhs, .. } = tree else {
        panic!("expected external comparison, got {tree:?}");
    };
    let crate::mir::ExprTree::Path(lhs) = lhs.as_ref() else {
        panic!("expected external path");
    };
    assert_eq!(lhs.binding, crate::mir::expr_tree::BindingKind::External);
    assert_eq!(lhs.ty, Some(crate::mir::Ty::U64));
    let crate::mir::ExprTree::Old(rhs) = rhs.as_ref() else {
        panic!("expected old external path");
    };
    assert!(matches!(
        rhs.as_ref(),
        crate::mir::ExprTree::Path(path)
            if path.binding == crate::mir::expr_tree::BindingKind::External
    ));
}

#[test]
fn cross_environment_external_reference_is_rejected() {
    // `clock.slot` is declared external in `clock_env`; referencing it from
    // `oracle_env` (which does not declare it) would lower to an unresolved
    // identifier in Kani/Lean. The spec checker must reject it up front.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"
environment clock_env {
  external clock.slot : U64
  constraint clock.slot >= old(clock.slot)
}

environment oracle_env {
  external oracle.price : U64
  constraint oracle.price >= clock.slot
}
"#
    );
    let err = parse_str(&src).expect_err("cross-env external must be rejected");
    let msg = format!("{err:#}");
    assert!(msg.contains("oracle_env"), "{msg}");
    assert!(msg.contains("clock"), "{msg}");
    assert!(msg.contains("clock_env"), "{msg}");
}

#[test]
fn external_shared_across_environments_is_allowed() {
    // The same external declared in BOTH environments is fine — each
    // environment redeclares it, so the reference resolves locally.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"
environment a {
  external clock.slot : U64
  constraint clock.slot >= 0
}

environment b {
  external clock.slot : U64
  constraint clock.slot >= 0
}
"#
    );
    parse_str(&src).expect("shared external redeclared in each env parses");
}

// ========================================================================
// RustOpts.state_mode + inside_old round-trips
// ========================================================================

/// Helper: parse a tiny spec and render the named property's body via
/// `expr_to_rust` under the given `RustOpts`. Returns the rendered
/// string for assertion.
fn render_property_body(spec_src: &str, prop_name: &str, mode: StateMode) -> String {
    let typed = crate::chumsky_parser::parse(spec_src)
        .map_err(|e| format!("parse failed: {:?}", e))
        .expect("parse");
    // Find the property in the typed AST.
    let prop_decl = typed
        .items
        .iter()
        .find_map(|item| match &item.node {
            a::TopItem::Property(p) if p.name == prop_name => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| panic!("property `{}` not found in spec", prop_name));
    let env = TypeEnv::from_spec(&typed);
    let consts: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let opts = opts_native(&env).with_state_mode(mode);
    expr_to_rust(&prop_decl.body.node, Ctx::Guard, &consts, opts)
}

#[test]
fn render_unary_mode_state_x_lowers_to_s_dot_x() {
    // Today's behavior preserved under StateMode::Unary: `state.x` → `s.x`.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD, r#"property balance_nonneg : state.balance >= 0 preserved_by all"#
    );
    let rendered = render_property_body(&src, "balance_nonneg", StateMode::Unary);
    assert!(
        rendered.contains("s.balance"),
        "expected `s.balance` in unary mode; got: {}",
        rendered
    );
    assert!(
        !rendered.contains("post.balance") && !rendered.contains("pre.balance"),
        "unary mode must not emit pre./post.; got: {}",
        rendered
    );
}

#[test]
fn render_binary_mode_state_x_lowers_to_post_dot_x() {
    // Binary mode: `state.x` (no `old`) → `post.x`.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD, r#"property balance_nonneg : state.balance >= 0 preserved_by all"#
    );
    let rendered = render_property_body(&src, "balance_nonneg", StateMode::Binary);
    assert!(
        rendered.contains("post.balance"),
        "expected `post.balance` in binary mode; got: {}",
        rendered
    );
    assert!(
        !rendered.contains("s.balance") && !rendered.contains("pre.balance"),
        "binary mode without old() must use post., not s. or pre.; got: {}",
        rendered
    );
}

// ========================================================================
// Bounded `exists` over a `Fin[N]` index domain lowers to a real
// `(0..N).any(…)` predicate; unbounded `exists` keeps the sentinel.
// ========================================================================

const EXISTS_SPEC_HEAD: &str = r#"
spec ExistsTest
const MAX = 8
type Idx = Fin[MAX]
type Member = { active : U64 }
type State = { members : Map[MAX] Member, count : U64 }
type Error
  | E
handler tick { effect { count := state.count + 1 } }
"#;

#[test]
fn render_bounded_exists_over_fin_alias_lowers_to_any() {
    let src = format!(
        "{}{}",
        EXISTS_SPEC_HEAD,
        r#"property bounded_ex : exists i : Idx, state.members[i].active == 1 preserved_by [tick]"#
    );
    let rendered = render_property_body(&src, "bounded_ex", StateMode::Unary);
    assert!(
        rendered.contains("(0..(MAX as usize)).any(|i|"),
        "bounded exists over a Fin[N] alias must iterate 0..N with .any(); got: {}",
        rendered
    );
    assert!(
        !rendered.contains(crate::check::QEDGEN_UNSUPPORTED_MARKER),
        "bounded exists must not emit the unsupported sentinel; got: {}",
        rendered
    );
}

#[test]
fn render_unbounded_exists_keeps_sentinel() {
    let src = format!(
        "{}{}",
        EXISTS_SPEC_HEAD,
        r#"property unbounded_ex : exists v : U64, state.count == v preserved_by [tick]"#
    );
    let rendered = render_property_body(&src, "unbounded_ex", StateMode::Unary);
    assert!(
        rendered.contains(crate::check::QEDGEN_UNSUPPORTED_MARKER),
        "exists over an unbounded U64 domain must keep the sentinel; got: {}",
        rendered
    );
}

// ========================================================================
// `ghost` lowers to a State field + per-handler update.
// ========================================================================

#[test]
fn ghost_lowers_to_state_field_and_per_handler_update() {
    let src = r#"
spec G
state { balance : U64 }
type Error
  | E
ghost total : U64 {
  init { 0 }
  on mint { total := state.total + amount }
}

handler mint (amount : U64) {
  effect { balance := state.balance + amount }
}
property p : state.total == state.balance preserved_by all
"#;
    let typed = crate::chumsky_parser::parse(src).expect("parse");
    let spec = adapt(&typed);
    assert_eq!(spec.ghosts.len(), 1, "one ghost expected");
    let g = &spec.ghosts[0];
    assert_eq!(g.name, "total");
    assert_eq!(g.ty, "U64");
    assert!(
        matches!(&g.init_tree, Some(crate::mir::ExprTree::Int(0))),
        "init tree should be the 0 literal; got {:?}",
        g.init_tree
    );
    assert_eq!(g.updates.len(), 1);
    assert_eq!(g.updates[0].handler, "mint");
    // `state.total` resolves to `s.total` (ghost registered as a state
    // field) and the handler param `amount` is in scope.
    assert!(
        g.updates[0].value_rust.contains("s.total") && g.updates[0].value_rust.contains("amount"),
        "update value_rust should read the ghost + param; got {}",
        g.updates[0].value_rust
    );
    // The folded tree reads the ghost's pre-value: `s.total + (amount)`
    // structurally (Lean renders from this tree).
    assert!(
        matches!(
            &g.updates[0].value_tree,
            Some(crate::mir::ExprTree::Arith { .. })
        ),
        "update value_tree should carry the folded add; got {:?}",
        g.updates[0].value_tree
    );
    // The property references the ghost as a state field.
    let prop = spec.properties.iter().find(|p| p.name == "p").unwrap();
    assert!(prop
        .rust_expression
        .as_deref()
        .is_some_and(|r| r.contains("s.total")));
}

#[test]
fn bounded_sum_alias_lowers_to_self_contained_binary_rust() {
    let src = r#"
spec SumVault
const MAX = 8
type Slot = Fin[MAX]
state { balances : Map[MAX] U64 }
type Error | E
handler rebalance { }
property conservation :
  sum i : Slot, state.balances[i] >= sum i : Slot, old(state.balances[i])
  preserved_by [rebalance]
"#;
    let typed = crate::chumsky_parser::parse(src).expect("parse bounded sum");
    let spec = adapt(&typed);
    let property = spec
        .properties
        .iter()
        .find(|property| property.name == "conservation")
        .expect("conservation property");
    assert_eq!(property.class, crate::check::PropertyClass::Binary);
    let rust = property
        .rust_expression
        .as_deref()
        .expect("bounded sum must have Rust lowering");
    assert!(rust.contains("0..(MAX as usize)"), "{rust}");
    assert!(rust.contains("post.balances"), "{rust}");
    assert!(rust.contains("pre.balances"), "{rust}");
    assert!(!rust.contains("sum_over"), "{rust}");
    assert!(
        !rust.contains(crate::check::QEDGEN_UNSUPPORTED_MARKER),
        "{rust}"
    );
}

#[test]
fn unbounded_sum_is_explicitly_unsupported_in_rust() {
    let src = r#"
spec UnboundedSum
state { total : U64 }
type Error | E
handler tick { }
property bad_sum : sum i : U64, state.total >= 0 preserved_by [tick]
"#;
    let typed = crate::chumsky_parser::parse(src).expect("parse unbounded sum");
    let spec = adapt(&typed);
    let rust = spec.properties[0]
        .rust_expression
        .as_deref()
        .expect("unsupported sentinel retained for diagnostics");
    assert!(rust.contains("QEDGEN_UNSUPPORTED_SUM"), "{rust}");
    assert!(!rust.contains("sum_over"), "{rust}");
    // The skip-guard layer must recognize the sum sentinel — otherwise the
    // bare comment escapes into Kani/proptest/Crucible expression position
    // as a syntax error and counts as an executable assertion.
    assert!(crate::check::rust_expr_is_unsupported(rust), "{rust}");
}

#[test]
fn sum_binder_resolves_fin_through_alias_chain() {
    // `Idx -> AccountIdx -> Fin[4]`: fin_bound must resolve transitively,
    // matching resolve_alias_name, not stop after one hop.
    let src = r#"
spec AliasChainSum
const MAX = 4
type AccountIdx = Fin[MAX]
type Idx = AccountIdx
state { balances : Map[MAX] U64 }
type Error | E
handler rebalance { }
property conservation :
  sum i : Idx, state.balances[i] >= 0
  preserved_by [rebalance]
"#;
    let typed = crate::chumsky_parser::parse(src).expect("parse alias-chain sum");
    let spec = adapt(&typed);
    let rust = spec.properties[0]
        .rust_expression
        .as_deref()
        .expect("alias-chain sum must have Rust lowering");
    assert!(rust.contains("0..(MAX as usize)"), "{rust}");
    assert!(
        !rust.contains(crate::check::QEDGEN_UNSUPPORTED_SUM_MARKER),
        "{rust}"
    );
}

// ========================================================================
// `hook` lowers to a per-field assertion set.
// ========================================================================

#[test]
fn hook_after_store_lowers_to_field_assertion() {
    let src = r#"
spec H
state { balance : U64, cap : U64 }
type Error
  | E
hook after_store(balance) {
  assert state.balance <= state.cap
}
handler deposit (amount : U64) {
  effect { balance := state.balance + amount }
}
"#;
    let typed = crate::chumsky_parser::parse(src).expect("parse");
    let spec = adapt(&typed);
    assert_eq!(spec.hooks.len(), 1);
    let h = &spec.hooks[0];
    assert!(matches!(
        &h.kind,
        crate::check::ParsedHookKind::AfterStore(f) if f == "balance"
    ));
    assert_eq!(h.asserts.len(), 1);
    let rendered = crate::rust_codegen_util::tree_render::render_rust(
        h.asserts[0].tree.as_ref().expect("hook assert tree"),
        crate::rust_codegen_util::tree_render::RustCx::native(),
    );
    assert!(
        rendered.contains("s.balance") && rendered.contains("s.cap"),
        "assert tree should read the state fields; got {rendered}"
    );
}

#[test]
fn hook_before_cpi_parses_with_optional_callee() {
    let src = r#"
spec H2
state { x : U64 }
type Error
  | E
hook before_cpi(Token) {
  assert state.x > 0
}
handler bump { effect { x := state.x + 1 } }
"#;
    let typed = crate::chumsky_parser::parse(src).expect("parse");
    let spec = adapt(&typed);
    assert_eq!(spec.hooks.len(), 1);
    assert!(matches!(
        &spec.hooks[0].kind,
        crate::check::ParsedHookKind::BeforeCpi(Some(c)) if c == "Token"
    ));
}

#[test]
fn render_binary_mode_old_state_x_lowers_to_pre_dot_x() {
    // Binary mode: `old(state.x)` → `pre.x` — the temporal marker is
    // honored in the rendered Rust.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
    );
    let rendered = render_property_body(&src, "balance_monotonic", StateMode::Binary);
    // Expect BOTH post.balance (LHS) and pre.balance (RHS, inside old)
    // to appear — the binary obligation made explicit in the rendered
    // expression.
    assert!(
        rendered.contains("post.balance"),
        "expected `post.balance` for LHS; got: {}",
        rendered
    );
    assert!(
        rendered.contains("pre.balance"),
        "expected `pre.balance` for RHS inside old(); got: {}",
        rendered
    );
    assert!(
        !rendered.contains("s.balance"),
        "binary mode must not emit `s.balance`; got: {}",
        rendered
    );
}

#[test]
fn render_unary_mode_old_collapses_to_s_dot_x() {
    // Unary path: `old(state.x)` and `state.x` both render to `s.x` —
    // the tautology shape. The vacuous-lowering lint P1s this when the
    // AST contains `Expr::Old(_)`; the path stays for compat with all
    // non-property callsites.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
    );
    let rendered = render_property_body(&src, "balance_monotonic", StateMode::Unary);
    // Both sides collapse to s.balance — the structural tautology.
    let s_count = rendered.matches("s.balance").count();
    assert!(
        s_count >= 2,
        "expected ≥2 `s.balance` (tautology shape) in unary mode; got: {} ({})",
        s_count,
        rendered
    );
}

#[test]
fn classify_property_authored_tautology_no_old_is_unary() {
    // Author-written `state.x == state.x` (no `old(...)`) — Unary.
    // The vacuous-lowering lint must NOT fire on this case.
    let src = format!(
        "{}{}",
        CLASSIFY_SPEC_HEAD,
        r#"property balance_tracked : state.balance == state.balance preserved_by all"#
    );
    assert_eq!(
        class_of(&src, "balance_tracked"),
        crate::check::PropertyClass::Unary
    );
}

/// When state sugar (or `type State = { ... }`) is used and a handler
/// has no explicit `accounts { ... }`, a default `state` handler-account
/// is synthesized so guards.rs can rewrite `s.X` → `ctx.state.X`
/// (otherwise generated guards leak raw `s.X` — compile error).
#[test]
fn state_sugar_handler_without_accounts_synthesizes_state_account() {
    let src = r#"spec Pool
const MAX = 4
type Error | InvalidAmount
type State = { values : Map[MAX] U64, total : U64 }

handler set_total (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { total := amt }
}

handler check_total (idx : U64) {
  requires state.values[idx] > 0 else InvalidAmount
  effect { }
}
"#;
    let spec = parse_str(src).expect("parse");
    let set_total = spec
        .handlers
        .iter()
        .find(|h| h.name == "set_total")
        .unwrap();
    let check_total = spec
        .handlers
        .iter()
        .find(|h| h.name == "check_total")
        .unwrap();

    // Effect-bearing handler: synthesized writable state account.
    assert_eq!(set_total.accounts.len(), 1);
    assert_eq!(set_total.accounts[0].name, "state");
    assert!(set_total.accounts[0].is_writable);
    assert_eq!(set_total.accounts[0].account_type.as_deref(), Some("State"));

    // Read-only handler referencing state.X via requires:
    // synthesized read-only state account.
    assert_eq!(check_total.accounts.len(), 1);
    assert_eq!(check_total.accounts[0].name, "state");
    assert!(!check_total.accounts[0].is_writable);
    assert_eq!(
        check_total.accounts[0].account_type.as_deref(),
        Some("State")
    );
}

/// Explicit `accounts { ... }` declarations win over the synthesis —
/// specs declaring accounts must not pick up a stray `state` field.
#[test]
fn explicit_accounts_clause_suppresses_state_synthesis() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { total : U64 }

handler bump (amt : U64) {
  accounts { vault : writable }
  requires amt > 0 else InvalidAmount
  effect { total := amt }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec.handlers.iter().find(|h| h.name == "bump").unwrap();
    assert_eq!(h.accounts.len(), 1);
    assert_eq!(h.accounts[0].name, "vault");
}

/// Handlers that don't touch state stay account-less even when the spec
/// has state_fields — otherwise pure helpers would silently grow a
/// surprise state account in their Anchor instruction signature.
#[test]
fn no_state_synthesis_when_handler_does_not_touch_state() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { total : U64 }

handler noop (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec.handlers.iter().find(|h| h.name == "noop").unwrap();
    assert!(
        h.accounts.is_empty(),
        "noop handler should stay account-less"
    );
}

/// `expr_to_rust` must render `mul_div_floor` / `mul_div_ceil` with
/// an `as u64` narrowing cast — the helpers return `u128` (the
/// intermediate `a * b` can overflow u64), but the spec-level
/// operation is U64 → U64. Without the cast, the canonical
/// `let fee = mul_div_floor(total, fee_bps, BPS); let to_provider =
/// total - fee` lowers to `u64 - u128`, which rejects at
/// `cargo build`.
#[test]
fn mul_div_floor_narrows_to_u64_at_call_site() {
    let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"

const BPS_DENOMINATOR = 10_000

type State
  | Active of { total_collected : U64 }

type Error
  | MathOverflow

handler accept (total : U64) (fee_bps : U64) : State.Active -> State.Active {
  permissionless
  let fee = mul_div_floor(total, fee_bps, BPS_DENOMINATOR)
  let to_provider = total - fee
  effect { Active.total_collected += fee }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec
        .handlers
        .iter()
        .find(|h| h.name == "accept")
        .expect("accept handler");

    // Find the `fee` binding's rendered RHS.
    let fee_rhs = &h
        .let_bindings
        .iter()
        .find(|b| b.name == "fee")
        .expect("fee binding")
        .rust_expr;

    assert!(
        fee_rhs.contains("mul_div_floor_u128"),
        "rendered RHS should still call the u128 helper for the intermediate width; got: {fee_rhs}"
    );
    assert!(
            fee_rhs.contains("as u64"),
            "rendered RHS must narrow back to u64 so downstream u64 uses (e.g. `total - fee`) typecheck; got: {fee_rhs}"
        );
}

/// Same contract for `mul_div_ceil`.
#[test]
fn mul_div_ceil_narrows_to_u64_at_call_site() {
    let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"

const BPS_DENOMINATOR = 10_000

type State
  | Active of { total_collected : U64 }

type Error
  | MathOverflow

handler accept (total : U64) (fee_bps : U64) : State.Active -> State.Active {
  permissionless
  let fee = mul_div_ceil(total, fee_bps, BPS_DENOMINATOR)
  let to_provider = total - fee
  effect { Active.total_collected += fee }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec
        .handlers
        .iter()
        .find(|h| h.name == "accept")
        .expect("accept handler");
    let fee_rhs = &h
        .let_bindings
        .iter()
        .find(|b| b.name == "fee")
        .expect("fee binding")
        .rust_expr;
    assert!(
        fee_rhs.contains("mul_div_ceil_u128") && fee_rhs.contains("as u64"),
        "ceil variant must narrow too; got: {fee_rhs}"
    );
}

#[test]
fn mul_div_round_half_up_narrows_to_u64_at_call_site() {
    let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"
type State = { total_collected : U64 }

handler accept (total : U64) (rate : U64) : State -> State {
  permissionless
  let rounded = mul_div_round_half_up(total, rate, 10_000)
  effect { total_collected += rounded }
}
"#;
    let spec = parse_str(src).expect("parse");
    let binding = spec.handlers[0]
        .let_bindings
        .iter()
        .find(|b| b.name == "rounded")
        .expect("rounded binding");
    let rust_rhs = &binding.rust_expr;
    assert!(
        rust_rhs.contains("mul_div_round_half_up_u128") && rust_rhs.contains("as u64"),
        "half-up variant must use the helper and narrow; got: {rust_rhs}"
    );
    // The Lean half-up bias (`(a * b + d / 2) / d`) renders from the tree;
    // the binding must carry the structural helper node.
    assert!(
        matches!(
            &binding.tree,
            Some(crate::mir::ExprTree::MulDivRoundHalfUp { .. })
        ),
        "half-up binding must carry the MulDivRoundHalfUp tree; got {:?}",
        binding.tree
    );
}

/// The let-binding narrow gate peels through `Paren` wrappers so the
/// author-written `let X = (mul_div_floor(...))` shape gets the same
/// narrowing as the bare form. Mirrors the `rust_infer_kind` peel
/// precedent — without it, a parenthesised RHS would silently keep
/// the u128 width and re-trigger the original `u64 - u128` mismatch.
#[test]
fn mul_div_in_paren_let_rhs_still_narrows_to_u64() {
    let src = r#"spec FeeMath
program_id "11111111111111111111111111111111"

const BPS_DENOMINATOR = 10_000

type State
  | Active of { total_collected : U64 }

type Error
  | MathOverflow

handler accept (total : U64) (fee_bps : U64) : State.Active -> State.Active {
  permissionless
  let fee = (mul_div_floor(total, fee_bps, BPS_DENOMINATOR))
  let to_provider = total - fee
  effect { Active.total_collected += fee }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = spec
        .handlers
        .iter()
        .find(|h| h.name == "accept")
        .expect("accept handler");
    let fee_rhs = &h
        .let_bindings
        .iter()
        .find(|b| b.name == "fee")
        .expect("fee binding")
        .rust_expr;
    assert!(
        fee_rhs.contains("mul_div_floor_u128") && fee_rhs.contains("as u64"),
        "parenthesised mul_div RHS must still narrow; got: {fee_rhs}"
    );
}

/// Issue #139: bare state-field reads in `requires` must pick up the state
/// receiver in both string projections (Kani/proptest read `rust_expr`, the
/// Lean transition reads `lean_expr`). Handler params stay bare.
#[test]
fn bare_state_field_refs_in_requires_get_state_receiver() {
    let src = r#"spec GenericVault
program_id "11111111111111111111111111111111"

type State = {
  active : U8,
  fee : U64,
}

type Error | Unauthorized

handler execute (amount : U64) : State -> State {
  permissionless
  requires active == 0 else Unauthorized
  requires amount > 0 else Unauthorized
  ensures fee == amount
  effect { fee := amount }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = &spec.handlers[0];
    assert_eq!(h.requires[0].rust_expr, "s.active == 0");
    assert_eq!(h.requires[0].lean_expr, "s.active = 0");
    // Param ref stays bare.
    assert_eq!(h.requires[1].rust_expr, "amount > 0");
    // Ensures: bare state field reads post-state.
    assert!(
        h.ensures[0].lean_expr.contains("s'.fee"),
        "ensures must canonicalize bare state refs; got: {}",
        h.ensures[0].lean_expr
    );
}

/// Names bound closer than state win: handler params, quantifier binders,
/// `let … in` binders, declared consts, and handler accounts all suppress
/// the `state.` rewrite even when they collide with a state field name.
#[test]
fn bound_names_shadow_state_fields_in_requires() {
    let src = r#"spec ShadowVault
program_id "11111111111111111111111111111111"

const LIMIT = 100

type State = {
  fee : U64,
  slots : U64,
}

type Error | Bad

handler pay (fee : U64) : State -> State {
  permissionless
  requires fee > 0 else Bad
  requires slots < LIMIT else Bad
  requires forall slots : U8, slots >= 0
  effect { fee := fee }
}
"#;
    let spec = parse_str(src).expect("parse");
    let h = &spec.handlers[0];
    // `fee` is a param — stays bare despite the state field of the same name.
    assert_eq!(h.requires[0].rust_expr, "fee > 0");
    // `slots` is only a state field — canonicalized; `LIMIT` substitutes.
    assert_eq!(h.requires[1].rust_expr, "s.slots < 100");
    // Quantifier binder shadows the state field inside its body.
    assert!(
        h.requires[2].rust_expr.contains("|slots| slots >= 0"),
        "binder must shadow state field; got: {}",
        h.requires[2].rust_expr
    );
}

// ============================================================================
// ExprTree construction (#151 Slice 0)
// ============================================================================

mod expr_tree_slice0 {
    use super::*;
    use crate::mir::expr_tree::{BindingKind, ExprTree, TreeCmpOp, TreeSeg};
    use crate::mir::Ty;

    const TREE_SPEC: &str = r#"spec TreeDemo
program_id "11111111111111111111111111111111"

const LIMIT = 100

type State = {
  balance : U64,
  pnl : I128,
  active : U64,
}

type Error | Bad | Overflow

handler pay (amount : U64) : State -> State {
  auth owner
  accounts {
    owner : signer,
    vault : writable,
  }
  requires amount > 0 else Bad
  requires active < LIMIT else Bad
  ensures state.balance == old(state.balance) + amount
  effect {
    balance += amount else Overflow,
    active := active + 1,
  }
}

property solvent :
  state.balance >= 0
  preserved_by all
"#;

    /// Requires trees resolve every binding class: params stay `Param`,
    /// canonicalized bare state reads become `StateField` with the real
    /// declared `Ty`, consts carry their resolved value.
    #[test]
    fn requires_tree_resolves_bindings_and_types() {
        let spec = parse_str(TREE_SPEC).expect("parse");
        let h = &spec.handlers[0];

        // `amount > 0` — LHS is a Param with Ty::U64 from the signature.
        let t0 = h.requires[0].tree.as_ref().expect("requires[0] tree");
        let ExprTree::Cmp { op, lhs, .. } = t0 else {
            panic!("expected Cmp, got {t0:?}");
        };
        assert_eq!(*op, TreeCmpOp::Gt);
        let ExprTree::Path(p) = lhs.as_ref() else {
            panic!("expected Path LHS, got {lhs:?}");
        };
        assert_eq!(p.binding, BindingKind::Param);
        assert_eq!(p.root, "amount");
        assert_eq!(p.ty, Some(Ty::U64));

        // `active < LIMIT` — bare `active` canonicalized to a StateField
        // read; `LIMIT` resolves to Const("100").
        let t1 = h.requires[1].tree.as_ref().expect("requires[1] tree");
        let ExprTree::Cmp { lhs, rhs, .. } = t1 else {
            panic!("expected Cmp, got {t1:?}");
        };
        let ExprTree::Path(state_read) = lhs.as_ref() else {
            panic!("expected Path LHS, got {lhs:?}");
        };
        assert_eq!(state_read.binding, BindingKind::StateField);
        assert_eq!(state_read.root, "state");
        assert_eq!(
            state_read.segments,
            vec![TreeSeg::Field("active".to_string())]
        );
        assert_eq!(state_read.ty, Some(Ty::U64));
        let ExprTree::Path(limit) = rhs.as_ref() else {
            panic!("expected Path RHS, got {rhs:?}");
        };
        assert_eq!(limit.binding, BindingKind::Const("100".to_string()));
    }

    /// Ensures trees keep `old(...)` structural — one tree serves the
    /// unary and binary render modes.
    #[test]
    fn ensures_tree_keeps_old_node() {
        let spec = parse_str(TREE_SPEC).expect("parse");
        let h = &spec.handlers[0];
        let t = h.ensures[0].tree.as_ref().expect("ensures tree");
        let ExprTree::Cmp { rhs, .. } = t else {
            panic!("expected Cmp, got {t:?}");
        };
        let ExprTree::Arith { lhs, .. } = rhs.as_ref() else {
            panic!("expected Arith RHS, got {rhs:?}");
        };
        assert!(
            matches!(lhs.as_ref(), ExprTree::Old(_)),
            "old(state.balance) must stay a structural Old node; got {lhs:?}"
        );
    }

    /// Effect RHS trees ride on each `ParsedEffect`, for both the
    /// simple shape (`amount` — historically skipped canonicalization) and
    /// the compound shape (`active + 1`).
    #[test]
    fn effect_rhs_trees_parallel_to_triples() {
        let spec = parse_str(TREE_SPEC).expect("parse");
        let h = &spec.handlers[0];

        let t0 = h.effects[0].tree.as_ref().expect("effect[0] tree");
        let ExprTree::Path(p) = t0 else {
            panic!("expected Path, got {t0:?}");
        };
        assert_eq!(p.binding, BindingKind::Param);

        let t1 = h.effects[1].tree.as_ref().expect("effect[1] tree");
        let ExprTree::Arith { lhs, .. } = t1 else {
            panic!("expected Arith, got {t1:?}");
        };
        let ExprTree::Path(active) = lhs.as_ref() else {
            panic!("expected Path, got {lhs:?}");
        };
        assert_eq!(
            active.binding,
            BindingKind::StateField,
            "bare `active` in effect RHS canonicalizes into a state read"
        );
    }

    /// Property bodies get spec-level trees; the tree-native kind
    /// inference agrees with the declared field type.
    #[test]
    fn property_tree_present_with_kind_inference() {
        let spec = parse_str(TREE_SPEC).expect("parse");
        let prop = &spec.properties[0];
        let t = prop.tree.as_ref().expect("property tree");
        assert_eq!(t.num_kind(), crate::mir::expr_tree::NumKind::Bool);
    }

    /// Auth-actor and account names resolve to `Account`; quantifier
    /// binders shadow same-named state fields as `ExprBinder`.
    #[test]
    fn account_and_shadow_binding_kinds() {
        let src = r#"spec Shadow
program_id "11111111111111111111111111111111"

type State = { active : U64, }
type Error | Bad

handler close : State -> State {
  auth owner
  accounts { owner : signer, }
  requires owner.pubkey == owner.pubkey else Bad
  requires forall active : U8, active >= 0
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];

        let t0 = h.requires[0].tree.as_ref().expect("tree");
        let ExprTree::Cmp { lhs, .. } = t0 else {
            panic!("expected Cmp, got {t0:?}");
        };
        let ExprTree::Path(owner) = lhs.as_ref() else {
            panic!("expected Path, got {lhs:?}");
        };
        assert_eq!(owner.binding, BindingKind::Account);

        let t1 = h.requires[1].tree.as_ref().expect("tree");
        let ExprTree::Quant { body, .. } = t1 else {
            panic!("expected Quant, got {t1:?}");
        };
        let ExprTree::Cmp { lhs, .. } = body.as_ref() else {
            panic!("expected Cmp body, got {body:?}");
        };
        let ExprTree::Path(binder_ref) = lhs.as_ref() else {
            panic!("expected Path, got {lhs:?}");
        };
        assert_eq!(
            binder_ref.binding,
            BindingKind::ExprBinder,
            "quantifier binder must shadow the same-named state field"
        );
    }

    /// Mixed-kind arithmetic (U64 state field + I128 state field) infers
    /// `Int` from the tree alone — no TypeEnv at the call site.
    #[test]
    fn tree_kind_inference_mixed_arith() {
        let src = r#"spec Mixed
program_id "11111111111111111111111111111111"

type State = { balance : U64, pnl : I128, }
type Error | Bad

handler check : State -> State {
  permissionless
  requires state.balance + state.pnl >= 0 else Bad
}
"#;
        let spec = parse_str(src).expect("parse");
        let t = spec.handlers[0].requires[0].tree.as_ref().expect("tree");
        let ExprTree::Cmp { lhs, .. } = t else {
            panic!("expected Cmp, got {t:?}");
        };
        assert_eq!(lhs.num_kind(), crate::mir::expr_tree::NumKind::Int);
    }
}
