use super::*;

fn empty_spec() -> ParsedSpec {
    ParsedSpec::default()
}

#[test]
fn wrapping_arithmetic_lint_fires_on_wrap() {
    let mut spec = empty_spec();
    let mut h = make_handler("tick");
    h.effects
        .push(("epoch".to_string(), "add_wrap".to_string(), "1".to_string()));
    spec.handlers.push(h);
    let warnings = check_wrapping_arithmetic_opt_in(&spec);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].rule, "wrapping_arithmetic");
    assert_eq!(warnings[0].severity, Severity::Warning);
    assert!(warnings[0].message.contains("wrapping"));
}

#[test]
fn wrapping_arithmetic_lint_fires_on_saturating() {
    let mut spec = empty_spec();
    let mut h = make_handler("apply");
    h.effects.push((
        "balance".to_string(),
        "add_sat".to_string(),
        "delta".to_string(),
    ));
    spec.handlers.push(h);
    let warnings = check_wrapping_arithmetic_opt_in(&spec);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].rule, "saturating_arithmetic");
    assert_eq!(warnings[0].severity, Severity::Info);
}

#[test]
fn wrapping_arithmetic_lint_silent_on_default_checked() {
    let mut spec = empty_spec();
    let mut h = make_handler("deposit");
    h.effects
        .push(("total".to_string(), "add".to_string(), "amount".to_string()));
    h.effects.push((
        "fee_pool".to_string(),
        "sub".to_string(),
        "amount".to_string(),
    ));
    spec.handlers.push(h);
    assert!(check_wrapping_arithmetic_opt_in(&spec).is_empty());
}

#[test]
fn wrapping_arithmetic_lint_fires_per_op() {
    let mut spec = empty_spec();
    let mut h = make_handler("complex");
    h.effects
        .push(("a".to_string(), "add_wrap".to_string(), "1".to_string()));
    h.effects
        .push(("b".to_string(), "sub_sat".to_string(), "1".to_string()));
    spec.handlers.push(h);
    let warnings = check_wrapping_arithmetic_opt_in(&spec);
    assert_eq!(warnings.len(), 2);
}

fn make_handler(name: &str) -> ParsedHandler {
    ParsedHandler {
        name: name.to_string(),
        doc: None,
        who: Some("authority".to_string()),
        on_account: None,
        pre_status: Some("Active".to_string()),
        post_status: Some("Active".to_string()),
        takes_params: vec![],
        guard_str: None,
        guard_str_rust: None,
        aborts_if: vec![],
        requires: vec![],
        ensures: vec![],
        modifies: None,
        let_bindings: vec![],
        aborts_total: false,
        permissionless: false,
        effects: vec![],
        effect_on_error: vec![],
        accounts: vec![],
        transfers: vec![],
        emits: vec![],
        invariants: vec![],
        establishes: vec![],
        properties: vec![],
        schema_includes: vec![],
        calls: vec![],
        effect_branches: None,
        abstract_binders: vec![],
    }
}

// `state { fields }` sugar must expose Map-typed fields to
// `check_map_and_subscript` — otherwise `subscript_not_map` fires on
// every effect LHS that subscripts a sugared Map field.
#[test]
fn state_sugar_map_field_is_visible_to_subscript_lint() {
    let src = r#"
spec Probe
const MAX = 8
type User = { active : Bool, balance : U64, }
state {
  lsts : Map[MAX] User,
}
type Error
  | InvalidAmount
handler deposit (idx : U64) (amt : U64) {
  effect { lsts[idx].balance := amt }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_map_and_subscript(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "subscript_not_map"),
        "spurious subscript_not_map on `state {{ ... }}` sugar: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

// `modifies [X]` + no effect write + no ensures referencing X =
// completely unconstrained field; fires P0.
#[test]
fn unconstrained_modifies_lint_fires_on_uncovered_field() {
    let src = r#"
spec Probe
state { pool_balance : U64, lp_supply : U64 }
type Error
  | InvalidAmount
  | MathOverflow
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  effect { pool_balance += amount }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_unconstrained_modifies(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "unconstrained_modifies")
        .expect("unconstrained_modifies fires for lp_supply");
    assert_eq!(hit.severity, Severity::Error);
    assert!(
        hit.message.contains("'lp_supply'"),
        "message names the field, got: {}",
        hit.message
    );
    // pool_balance is in modifies AND in effect — no warning for it.
    assert!(
        !warnings
            .iter()
            .any(|w| w.message.contains("'pool_balance'")),
        "pool_balance must not fire — it's written by the effect"
    );
}

// Inverse: when an `ensures` clause references the field, the
// lint stays silent. The field is constrained even if the effect
// block doesn't write it (the "Kani checks impl" pattern).
#[test]
fn unconstrained_modifies_lint_silent_when_ensures_references_field() {
    let src = r#"
spec Probe
state { pool_balance : U64, lp_supply : U64 }
type Error
  | InvalidAmount
  | MathOverflow
handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  modifies [pool_balance, lp_supply]
  effect { pool_balance += amount }
  ensures lp_supply >= old(state.lp_supply)
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_unconstrained_modifies(&spec);
    assert!(
        warnings.is_empty(),
        "lint must stay silent when ensures references the field, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

// ========================================================================
// adt_state_missing_wrong_state lint
// ========================================================================

/// `pragma state_repr = adt` selects the inductive representation,
/// whose variant-mismatch fallthrough returns `Err(WrongState)`.
/// Declaring the pragma without the error variant would emit
/// non-compiling Rust, so `check` surfaces it. Without the pragma the
/// same spec lowers flat and the lint stays silent.
#[test]
fn adt_state_pragma_without_wrong_state_fires() {
    let body = r#"
program_id "11111111111111111111111111111111"

type State
  | Uninitialized
  | Active of { balance : U64 }
  | Closed

type Error
  | InvalidAmount

handler open (amount : U64) : State.Uninitialized -> State.Active {
  auth owner
  accounts { owner : signer, writable }
  requires amount > 0 else InvalidAmount
}"#;

    // pragma set, no WrongState → fires
    let adt =
        crate::chumsky_adapter::parse_str(&format!("spec Adt\npragma state_repr = adt\n{body}"))
            .expect("parse adt");
    let w = check_completeness(&adt);
    let hit = w
        .iter()
        .find(|w| w.rule == "adt_state_missing_wrong_state")
        .unwrap_or_else(|| {
            panic!(
                "lint must fire; got: {:?}",
                w.iter().map(|w| &w.rule).collect::<Vec<_>>()
            )
        });
    assert_eq!(hit.severity, Severity::Warning);
    assert_eq!(hit.priority, 2);

    // no pragma (flat) → silent even without WrongState
    let flat =
        crate::chumsky_adapter::parse_str(&format!("spec Flat\n{body}")).expect("parse flat");
    assert!(
        !check_completeness(&flat)
            .iter()
            .any(|w| w.rule == "adt_state_missing_wrong_state"),
        "flat specs don't need WrongState; lint must stay silent"
    );
}

// ========================================================================
// multi_cpi_same_field lint
// ========================================================================

/// Two CPI calls whose substituted ensures both reference the same
/// caller-state field (`post.vault_balance`) → lint fires P2 Info.
/// Mirrors the bear-hug scenario where two `Token.transfer` calls
/// drain the same vault. Without per-call snapshot frames (v3.0),
/// the Kani harness can over-constrain.
#[test]
fn multi_cpi_same_field_fires_on_two_token_transfers_from_same_vault() {
    let src = r#"spec MultiCpi
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.vault_balance == old(state.vault_balance) - amount
  }
}

state { vault_balance : U64 }

handler split (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call Token.transfer(from = 0, to = 1, amount = a, authority = 0)
  call Token.transfer(from = 0, to = 2, amount = b, authority = 0)
  effect { vault_balance -= a }
  ensures state.vault_balance == old(state.vault_balance) - a - b
}"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_multi_cpi_same_field(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "multi_cpi_same_field")
        .unwrap_or_else(|| {
            panic!(
                "multi_cpi_same_field must fire; got: {:?}",
                warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
            )
        });
    assert_eq!(hit.severity, Severity::Info);
    assert_eq!(hit.priority, 2);
    assert!(
        hit.message.contains("'vault_balance'"),
        "message must name the shared field; got: {}",
        hit.message
    );
    assert!(
        hit.message.contains("Token.transfer"),
        "message must name the call pair; got: {}",
        hit.message
    );
    assert_eq!(hit.subject.as_deref(), Some("split"));
}

/// Disjoint Token.transfer resources are handled by the Pinocchio
/// impl-targeted token projection backend. The abstract callee fields
/// are the same, but the generated proof reads and asserts each token
/// account's concrete amount independently.
#[test]
fn multi_cpi_same_field_silent_on_disjoint_token_transfer_resources() {
    let src = r#"spec MultiCpiDisjointToken
program_id "11111111111111111111111111111111"

interface Token {
  program_id "11111111111111111111111111111111"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures state.from_balance == old(state.from_balance) - amount
    ensures state.to_balance == old(state.to_balance) + amount
  }
}

state { from_balance : U64, to_balance : U64 }

handler swap_like (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call Token.transfer(from = user_input, to = hub_input, amount = a, authority = auth)
  call Token.transfer(from = hub_output, to = user_output, amount = b, authority = auth)
  ensures state.from_balance == old(state.from_balance) - a
}"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_multi_cpi_same_field(&spec);
    assert!(
        warnings.is_empty(),
        "disjoint Token.transfer resources use per-account projections; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// Two CPI calls whose substituted ensures reference disjoint
/// caller-state fields → lint stays silent. No (pre, post) snapshot
/// pair is shared, so the over-constraint risk doesn't apply.
#[test]
fn multi_cpi_same_field_silent_on_disjoint_fields() {
    let src = r#"spec MultiCpiDisjoint
program_id "11111111111111111111111111111111"

interface VaultA {
  program_id "11111111111111111111111111111111"
  handler debit (amount : U64) {
    accounts { vault : writable }
    requires amount > 0
    ensures state.vault_a_balance == old(state.vault_a_balance) - amount
  }
}

interface VaultB {
  program_id "11111111111111111111111111111111"
  handler debit (amount : U64) {
    accounts { vault : writable }
    requires amount > 0
    ensures state.vault_b_balance == old(state.vault_b_balance) - amount
  }
}

state { vault_a_balance : U64, vault_b_balance : U64 }

handler tap_both (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call VaultA.debit(amount = a)
  call VaultB.debit(amount = b)
  effect { vault_a_balance -= a }
  effect { vault_b_balance -= b }
  ensures state.vault_a_balance == old(state.vault_a_balance) - a
  ensures state.vault_b_balance == old(state.vault_b_balance) - b
}"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_multi_cpi_same_field(&spec);
    assert!(
        warnings.is_empty(),
        "disjoint-field CPI ensures must not fire multi_cpi_same_field; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// Tier-0 callees (no `ensures` declared) → no substituted field
/// references → lint stays silent regardless of CPI multiplicity.
/// Catches the spec-shape where the user hasn't yet declared the
/// callee's contract; the `cpi_no_callee_ensures` lint surfaces
/// that gap separately.
#[test]
fn multi_cpi_same_field_silent_on_tier0_callees() {
    let src = r#"spec MultiCpiTier0
program_id "11111111111111111111111111111111"

interface Logger {
  program_id "11111111111111111111111111111111"
  handler log (msg : U64) {
    accounts { sink : writable }
  }
}

state { counter : U64 }

handler tick_twice (a : U64) (b : U64) {
  permissionless
  requires a > 0 else InvalidAmount
  requires b > 0 else InvalidAmount
  call Logger.log(msg = a)
  call Logger.log(msg = b)
  effect { counter += a }
  ensures state.counter == old(state.counter) + a
}"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_multi_cpi_same_field(&spec);
    assert!(
        warnings.is_empty(),
        "tier-0 callees produce no field refs → lint must stay silent; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

#[test]
fn error_declared_as_record_lint_fires_and_suggests_pipe_form() {
    let src = r#"
spec Probe
state { balance : U64 }
type Error = {
  InvalidAmount : U64,
  Unauthorized : U64,
}
handler init { effect { balance := 0 } }
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
    let warnings = check_error_declared_as_record(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "error_declared_as_record")
        .expect("error_declared_as_record fires");
    assert_eq!(hit.severity, Severity::Error);
    let example = hit.example.as_deref().unwrap_or("");
    assert!(
        example.contains("type Error\n  | InvalidAmount"),
        "example should suggest pipe form, got: {}",
        example
    );
}

#[test]
fn test_missing_guard_from_takes_fires() {
    let mut h = make_handler("deposit");
    h.takes_params = vec![("amount".to_string(), "U64".to_string())];
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "missing_guard_from_takes"),
        "expected missing_guard_from_takes, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_guard_from_takes_skips_when_guard_exists() {
    let mut h = make_handler("deposit");
    h.takes_params = vec![("amount".to_string(), "U64".to_string())];
    h.guard_str = Some("amount > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "missing_guard_from_takes"),
        "should not fire when guard exists"
    );
}

#[test]
fn test_missing_effect_fires() {
    let mut h = make_handler("deposit");
    h.takes_params = vec![("amount".to_string(), "U64".to_string())];
    h.guard_str = Some("amount > 0".to_string());
    // has lifecycle (pre/post set via make_handler) but no effect
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| w.rule == "missing_effect"),
        "expected missing_effect, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

/// `call X.handler(...)`, `transfers { … }`, or `modifies [...]` all
/// count as effect-satisfying — the lint must not fire on CPI-only
/// handlers where state writes are the wrong abstraction.
#[test]
fn test_missing_effect_skips_when_handler_has_only_calls() {
    let mut h = make_handler("init_mint");
    h.takes_params = vec![("decimals".to_string(), "U64".to_string())];
    h.guard_str = Some("decimals > 0".to_string());
    h.calls = vec![ParsedCall {
        target_interface: "Token".to_string(),
        target_handler: "initialize_mint".to_string(),
        args: vec![],
        result_binding: None,
        state_binders: Vec::new(),
    }];
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_effect"),
        "missing_effect should not fire when handler has CPI calls; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

/// `modifies [field, ...]` is the frame-condition shape for handlers
/// whose writes the spec doesn't model further — must satisfy the lint.
#[test]
fn test_missing_effect_skips_when_handler_has_modifies() {
    let mut h = make_handler("opaque_update");
    h.takes_params = vec![("payload".to_string(), "U64".to_string())];
    h.guard_str = Some("payload > 0".to_string());
    h.modifies = Some(vec!["balance".to_string()]);
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_effect"),
        "missing_effect should not fire when handler declares `modifies`; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_effect_skips_when_effect_exists() {
    let mut h = make_handler("deposit");
    h.takes_params = vec![("amount".to_string(), "U64".to_string())];
    h.guard_str = Some("amount > 0".to_string());
    h.effects = vec![(
        "balance".to_string(),
        "add".to_string(),
        "amount".to_string(),
    )];
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_effect"),
        "should not fire when effect exists"
    );
}

#[test]
fn test_missing_effect_uses_on_account_fields() {
    let mut h = make_handler("borrow");
    h.on_account = Some("Loan".to_string());
    h.takes_params = vec![("loan_amount".to_string(), "U64".to_string())];
    h.guard_str = Some("loan_amount > 0".to_string());
    h.pre_status = Some("Empty".to_string());
    h.post_status = Some("Active".to_string());

    let spec = ParsedSpec {
        handlers: vec![h],
        account_types: vec![
            ParsedAccountType {
                name: "Pool".to_string(),
                fields: vec![("total_deposits".to_string(), "U64".to_string())],
                lifecycle: vec!["Active".to_string()],
                pda_ref: None,
                variants: vec![],
            },
            ParsedAccountType {
                name: "Loan".to_string(),
                fields: vec![("loan_amount".to_string(), "U64".to_string())],
                lifecycle: vec!["Empty".to_string(), "Active".to_string()],
                pda_ref: None,
                variants: vec![],
            },
        ],
        state_fields: vec![("total_deposits".to_string(), "U64".to_string())],
        lifecycle_states: vec!["Empty".to_string(), "Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    let warning = warnings
        .iter()
        .find(|w| w.rule == "missing_effect")
        .expect("expected missing_effect warning");
    let example = warning
        .example
        .as_deref()
        .expect("missing_effect should include example");
    assert!(
        example.contains("loan_amount += loan_amount"),
        "expected account-aware suggestion, got: {}",
        example
    );
    assert!(
        !example.contains("total_deposits"),
        "should not use fields from a different account type: {}",
        example
    );
}

#[test]
fn permissionless_skips_no_access_control() {
    // `permissionless` opts out of the P1 `no_access_control` lint;
    // without the marker, who-less handlers still fire.
    let mut h = make_handler("init_user");
    h.who = None;
    h.permissionless = true;
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "no_access_control"),
        "permissionless handler must not fire no_access_control: {warnings:?}"
    );
}

#[test]
fn no_access_control_still_fires_without_marker() {
    // Control: handler with no auth and no permissionless marker still
    // triggers the lint.
    let mut h = make_handler("init_user");
    h.who = None;
    // h.permissionless stays false
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| w.rule == "no_access_control"),
        "who-less handler without permissionless should fire: {warnings:?}"
    );
}

#[test]
fn permissionless_with_auth_surfaces_contradictory_auth() {
    // Both `auth X` and `permissionless` is contradictory — not a silent
    // precedence situation. Lint surfaces a clear P1.
    let mut h = make_handler("weird");
    h.who = Some("authority".to_string());
    h.permissionless = true;
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    let w = warnings
        .iter()
        .find(|w| w.rule == "contradictory_auth")
        .expect("contradictory_auth should fire");
    assert!(
        w.message.contains("authority") && w.message.contains("permissionless"),
        "message should name both: {}",
        w.message
    );
}

#[test]
fn test_no_properties_fires() {
    let mut h = make_handler("deposit");
    h.effects = vec![(
        "balance".to_string(),
        "add".to_string(),
        "amount".to_string(),
    )];
    h.guard_str = Some("amount > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| w.rule == "no_properties"),
        "expected no_properties, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_no_properties_skips_with_property() {
    let mut h = make_handler("deposit");
    h.effects = vec![(
        "balance".to_string(),
        "add".to_string(),
        "amount".to_string(),
    )];
    h.guard_str = Some("amount > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        properties: vec![ParsedProperty {
            name: "conservation".to_string(),
            expression: Some("state.balance >= 0".to_string()),
            rust_expression: Some("s.balance >= 0".to_string()),
            rust_expression_pod: Some("s.balance >= 0".to_string()),
            preserved_by: vec!["deposit".to_string()],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        }],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "no_properties"),
        "should not fire when properties exist"
    );
}

#[test]
fn test_missing_cpi_for_token_context() {
    let mut h = make_handler("transfer");
    // Has token program in accounts but no transfers block
    h.accounts = vec![
        ParsedHandlerAccount {
            name: "authority".to_string(),
            is_signer: true,
            is_writable: false,
            is_program: false,
            pda_seeds: None,
            account_type: None,
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "source".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "dest".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "token_program".to_string(),
            is_signer: false,
            is_writable: false,
            is_program: true,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
    ];
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "missing_cpi_for_token_context"),
        "expected missing_cpi_for_token_context, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_cpi_for_token_context_suppressed_on_lifecycle_init() {
    // An `initialize` handler creating a writable token account via
    // Anchor's `#[account(init, ...)]` needs no explicit `transfers` /
    // `call Token.*` — the init macro handles the SPL CPI implicitly.
    let mut h = make_handler("initialize");
    h.pre_status = Some("Uninitialized".to_string());
    h.post_status = Some("Active".to_string());
    h.accounts = vec![
        ParsedHandlerAccount {
            name: "authority".to_string(),
            is_signer: true,
            is_writable: false,
            is_program: false,
            pda_seeds: None,
            account_type: None,
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "vault".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: Some(vec!["vault".to_string(), "authority".to_string()]),
            account_type: Some("token".to_string()),
            authority: Some("vault_pda".to_string()),
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "token_program".to_string(),
            is_signer: false,
            is_writable: false,
            is_program: true,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
    ];
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Uninitialized".to_string(), "Active".to_string()],
        account_types: vec![ParsedAccountType {
            name: "State".to_string(),
            fields: vec![],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![
                ParsedVariant {
                    name: "Uninitialized".to_string(),
                    fields: vec![],
                },
                ParsedVariant {
                    name: "Active".to_string(),
                    fields: vec![("balance".to_string(), "U64".to_string())],
                },
            ],
        }],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "missing_cpi_for_token_context"),
        "lifecycle-init handler creating a token account should NOT fire \
             missing_cpi_for_token_context; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_cpi_for_token_context_suppressed_on_non_canonical_init_name() {
    // The suppression keys on "pre-state variant has no payload", not
    // a hardcoded name list — specs naming the pre-init variant
    // `Uninit` / `Created` / etc. must stay silent too. Mirror of the
    // canonical-name test above with `Uninit` substituted.
    let mut h = make_handler("initialize");
    h.pre_status = Some("Uninit".to_string());
    h.post_status = Some("Active".to_string());
    h.accounts = vec![
        ParsedHandlerAccount {
            name: "authority".to_string(),
            is_signer: true,
            is_writable: false,
            is_program: false,
            pda_seeds: None,
            account_type: None,
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "vault".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: Some(vec!["vault".to_string(), "authority".to_string()]),
            account_type: Some("token".to_string()),
            authority: Some("vault_pda".to_string()),
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "token_program".to_string(),
            is_signer: false,
            is_writable: false,
            is_program: true,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
    ];
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Uninit".to_string(), "Active".to_string()],
        account_types: vec![ParsedAccountType {
            name: "State".to_string(),
            fields: vec![],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![
                ParsedVariant {
                    name: "Uninit".to_string(),
                    fields: vec![],
                },
                ParsedVariant {
                    name: "Active".to_string(),
                    fields: vec![("balance".to_string(), "U64".to_string())],
                },
            ],
        }],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "missing_cpi_for_token_context"),
        "init handler with non-canonical pre-state variant `Uninit` \
             must NOT fire missing_cpi_for_token_context (v2.29.2 shape \
             predicate); got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_cpi_for_token_context_suppressed_when_no_typed_token_account() {
    // The suppression must not require a writable account typed
    // `token`: real specs leave token accounts bare-typed and rely on
    // Anchor's `init, associated_token::*` constraints to resolve the
    // type. `is_lifecycle_init && !has_calls()` is sufficient.
    let mut h = make_handler("initialize");
    h.pre_status = Some("Uninit".to_string());
    h.post_status = Some("Active".to_string());
    h.accounts = vec![
        ParsedHandlerAccount {
            name: "authority".to_string(),
            is_signer: true,
            is_writable: false,
            is_program: false,
            pda_seeds: None,
            account_type: None,
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            // Bare writable, no `type token` — Anchor would type it
            // via an `init, associated_token::*` constraint set the
            // spec doesn't repeat.
            name: "pool_balance_account".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: None,
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "token_program".to_string(),
            is_signer: false,
            is_writable: false,
            is_program: true,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
    ];
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Uninit".to_string(), "Active".to_string()],
        account_types: vec![ParsedAccountType {
            name: "State".to_string(),
            fields: vec![],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![
                ParsedVariant {
                    name: "Uninit".to_string(),
                    fields: vec![],
                },
                ParsedVariant {
                    name: "Active".to_string(),
                    fields: vec![("balance".to_string(), "U64".to_string())],
                },
            ],
        }],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "missing_cpi_for_token_context"),
        "lifecycle-init handler with token_program but no `type token` \
             writable account must NOT fire missing_cpi_for_token_context \
             (v2.29.2 — Anchor init handles SPL implicitly via constraint \
             set); got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_missing_cpi_for_token_context_still_fires_on_non_init() {
    // Complement to the suppression: a handler in a non-init
    // lifecycle (e.g. Active → Active) with token_program and a
    // writable token account but no transfers SHOULD still fire —
    // Anchor's init macro doesn't apply, so the missing CPI is a
    // real spec gap.
    let mut h = make_handler("transfer");
    h.pre_status = Some("Active".to_string());
    h.post_status = Some("Active".to_string());
    h.accounts = vec![
        ParsedHandlerAccount {
            name: "authority".to_string(),
            is_signer: true,
            is_writable: false,
            is_program: false,
            pda_seeds: None,
            account_type: None,
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "source".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
        ParsedHandlerAccount {
            name: "token_program".to_string(),
            is_signer: false,
            is_writable: false,
            is_program: true,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        },
    ];
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "missing_cpi_for_token_context"),
        "non-init handler with token_program and no transfers SHOULD \
             still fire; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_lifecycle_unreachable_state() {
    let mut h = make_handler("initialize");
    h.pre_status = Some("Uninitialized".to_string());
    h.post_status = Some("Active".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec![
            "Uninitialized".to_string(),
            "Active".to_string(),
            "Closed".to_string(),
        ],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "lifecycle_unreachable_state"
                && w.subject.as_deref() == Some("Closed")),
        "expected lifecycle_unreachable_state for Closed, got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.subject))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_lifecycle_disconnected_subgraph_is_unreachable() {
    let mut init = make_handler("initialize");
    init.pre_status = Some("Uninitialized".to_string());
    init.post_status = Some("Active".to_string());

    let mut close = make_handler("close");
    close.pre_status = Some("Frozen".to_string());
    close.post_status = Some("Closed".to_string());

    let spec = ParsedSpec {
        handlers: vec![init, close],
        lifecycle_states: vec![
            "Uninitialized".to_string(),
            "Active".to_string(),
            "Frozen".to_string(),
            "Closed".to_string(),
        ],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| {
            w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Frozen")
        }),
        "expected disconnected state Frozen to be unreachable, got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.subject))
            .collect::<Vec<_>>()
    );
    assert!(
        warnings.iter().any(|w| {
            w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Closed")
        }),
        "expected downstream state Closed to be unreachable, got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.subject))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_global_initial_state_seeded_when_account_lifecycle_differs() {
    // Account lifecycle starts at "Active", but the global initial state
    // is "Uninitialized". Without always seeding the global initial state,
    // "Uninitialized" would be flagged as unreachable even though it is
    // the entry point of the lifecycle.
    let mut init = make_handler("initialize");
    init.pre_status = Some("Uninitialized".to_string());
    init.post_status = Some("Active".to_string());

    let spec = ParsedSpec {
        handlers: vec![init],
        account_types: vec![ParsedAccountType {
            name: "Pool".to_string(),
            fields: vec![],
            lifecycle: vec!["Active".to_string(), "Frozen".to_string()],
            pda_ref: None,
            variants: vec![],
        }],
        lifecycle_states: vec![
            "Uninitialized".to_string(),
            "Active".to_string(),
            "Frozen".to_string(),
        ],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
            !warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state"
                    && w.subject.as_deref() == Some("Uninitialized")
            }),
            "Uninitialized is the global initial state and should NOT be flagged as unreachable, got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "lifecycle_unreachable_state")
                .map(|w| &w.subject)
                .collect::<Vec<_>>()
        );
}

#[test]
fn test_no_errors_block_fires() {
    let mut h = make_handler("deposit");
    h.guard_str = Some("amount > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| w.rule == "no_errors_block"),
        "expected no_errors_block, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn test_priority_ordering() {
    // Build a spec that triggers multiple rules at different priorities
    let mut h = make_handler("deposit");
    h.who = None; // priority 1: no_access_control
    h.takes_params = vec![("amount".to_string(), "U64".to_string())];
    h.effects = vec![(
        "balance".to_string(),
        "add".to_string(),
        "amount".to_string(),
    )];
    // no guard → priority 1: unguarded_arithmetic + missing_guard_from_takes
    // no properties → priority 3: no_properties
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![
            ("authority".to_string(), "Pubkey".to_string()),
            ("balance".to_string(), "U64".to_string()),
        ],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    // Verify sorted ascending by priority
    for window in warnings.windows(2) {
        assert!(
            window[0].priority <= window[1].priority,
            "warnings not sorted by priority: {} ({}) should come before {} ({})",
            window[0].rule,
            window[0].priority,
            window[1].rule,
            window[1].priority
        );
    }
}

#[test]
fn test_complete_spec_clean() {
    let spec_content = include_str!("../../../../examples/rust/escrow/escrow.qedspec");
    let spec =
        crate::chumsky_adapter::parse_str(spec_content).expect("escrow.qedspec should parse");
    let warnings = check_completeness(&spec);
    // A well-formed spec should have zero `Warning`-severity findings.
    // (P6 on Pubkey state fields is Info-only, so it never appears here.)
    let warning_rules: Vec<&str> = warnings
        .iter()
        .filter(|w| w.severity == Severity::Warning)
        .map(|w| w.rule.as_str())
        .collect();
    assert!(
        warning_rules.is_empty(),
        "escrow.qedspec should be Warning-clean but got: {:?}",
        warning_rules
    );
}

// ========================================================================
// Spec-authoring lint regression tests. Each fixture mirrors an audit
// finding shape — if the lints stop firing, those recurring spec-shape
// gaps go uncaught.
// ========================================================================

/// Fixture mirroring the percolator-CRIT shape: `auth authority` but
/// no `authority` field on the state. Every handler is reachable by
/// any signer.
const UNBOUND_AUTH_FIXTURE: &str = r#"
spec Vault

type State
  | Uninitialized
  | Active of {
      balance : U64,
    }

type Error | InvalidAmount

handler init : State.Uninitialized -> State.Active {
  auth authority
  accounts {
    authority : signer
    vault     : writable
  }
  effect { balance := 0 }
}

handler withdraw (amount : U64) : State.Active -> State.Active {
  auth authority
  accounts {
    authority : signer
    vault     : writable
  }
  requires amount > 0 else InvalidAmount
  effect { balance -= amount }
}
"#;

#[test]
fn lint_unbound_auth_fires() {
    let spec =
        crate::chumsky_adapter::parse_str(UNBOUND_AUTH_FIXTURE).expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let unbound: Vec<&CompletenessWarning> = warnings
        .iter()
        .filter(|w| w.rule == "unbound_auth")
        .collect();
    assert!(
            !unbound.is_empty(),
            "expected unbound_auth to fire on a spec with `auth authority` and no state field; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
}

/// Dotted-auth desugar (`auth <acct>.<field>`) synthesizes a
/// `requires <acct>.<field> == <signer>.pubkey else Unauthorized`
/// clause and rewrites `who` to the signer name; `unbound_auth` must
/// recognize the imported-account binding shape (not just `s.<field>`
/// state references) and stay silent. Fixture distills the bundled
/// cross-program-vault `emergency_close` shape.
const DOTTED_AUTH_BOUND_FIXTURE: &str = r#"
spec Vault

type State
  | Active of {
      total_deposits : U64,
    }

type AdminConfig
  | Active of {
      admin : Pubkey,
    }

type Error | Unauthorized

handler close : State.Active -> State.Active {
  auth admin_config.admin
  accounts {
    admin        : signer
    vault        : writable
    admin_config : type AdminConfig
  }
  effect { total_deposits := 0 }
}
"#;

#[test]
fn lint_unbound_auth_silent_on_dotted_auth_desugar() {
    let spec = crate::chumsky_adapter::parse_str(DOTTED_AUTH_BOUND_FIXTURE)
        .expect("dotted-auth fixture should parse");
    let warnings = check_completeness(&spec);
    let unbound: Vec<&CompletenessWarning> = warnings
        .iter()
        .filter(|w| w.rule == "unbound_auth")
        .collect();
    assert!(
        unbound.is_empty(),
        "unbound_auth must stay silent when the synthesized `requires \
             <acct>.<field> == <signer>.pubkey` clause binds the signer \
             via an imported account (v2.29.2 escape); got: {:?}",
        unbound.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

/// Fixture mirroring the multisig::approve/reject HIGH: handler
/// takes `member_index` and mutates `state.voted[member_index]` but
/// no `requires` binds the index to the signer.
const UNGUARDED_INDEXED_FIXTURE: &str = r#"
spec Voting

const N = 8

type State
  | Uninitialized
  | Active of {
      voted : Map[N] U8,
      count : U8,
    }

type Error | OutOfRange | MathOverflow

handler vote (member_index : U8) : State.Active -> State.Active {
  auth voter
  accounts {
    voter : signer
    vault : writable
  }
  requires member_index < 8 else OutOfRange
  effect {
    count += 1
    voted[member_index] := 1
  }
}
"#;

#[test]
fn lint_unguarded_indexed_mutation_fires() {
    let spec =
        crate::chumsky_adapter::parse_str(UNGUARDED_INDEXED_FIXTURE).expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<&CompletenessWarning> = warnings
        .iter()
        .filter(|w| w.rule == "unguarded_indexed_mutation")
        .collect();
    assert!(
            !hits.is_empty(),
            "expected unguarded_indexed_mutation to fire on a vote-by-index handler with no signer↔index binding; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
}

/// Fixture mirroring the lending::liquidate HIGH: handler
/// transitions to a terminal state with no `requires`.
const UNGUARDED_TERMINAL_FIXTURE: &str = r#"
spec Loan

type State
  | Empty
  | Active of {
      borrower : Pubkey,
      amount   : U64,
    }
  | Liquidated

type Error | NotFound

handler liquidate : State.Active -> State.Liquidated {
  auth liquidator
  accounts {
    liquidator : signer
    loan       : writable
  }
  effect { amount := 0 }
}
"#;

#[test]
fn lint_unguarded_terminal_transition_fires() {
    let spec = crate::chumsky_adapter::parse_str(UNGUARDED_TERMINAL_FIXTURE)
        .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<&CompletenessWarning> = warnings
        .iter()
        .filter(|w| w.rule == "unguarded_terminal_transition")
        .collect();
    assert!(
            !hits.is_empty(),
            "expected unguarded_terminal_transition to fire on a Liquidated transition with no requires; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
}

/// Inverse: when the transition IS gated by an explicit `requires`,
/// the lint should NOT fire (audit-fixed lending::liquidate shape).
const GATED_TERMINAL_FIXTURE: &str = r#"
spec Loan

type State
  | Empty
  | Active of {
      borrower   : Pubkey,
      amount     : U64,
      collateral : U64,
    }
  | Liquidated

type Error | AccountHealthy

handler liquidate : State.Active -> State.Liquidated {
  auth liquidator
  accounts {
    liquidator : signer
    loan       : writable
  }
  requires state.amount > state.collateral else AccountHealthy
  effect { amount := 0 }
}
"#;

#[test]
fn lint_gated_terminal_transition_does_not_fire() {
    let spec =
        crate::chumsky_adapter::parse_str(GATED_TERMINAL_FIXTURE).expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<&str> = warnings
        .iter()
        .filter(|w| w.rule == "unguarded_terminal_transition")
        .map(|w| w.rule.as_str())
        .collect();
    assert!(
        hits.is_empty(),
        "unguarded_terminal_transition should not fire on health-gated liquidate; got: {:?}",
        hits
    );
}

// ========================================================================
// Coverage matrix, write_without_read, circular_lifecycle
// ========================================================================

#[test]
fn test_coverage_matrix_full_coverage() {
    let spec_content = include_str!("../../../../examples/rust/multisig/multisig.qedspec");
    let spec =
        crate::chumsky_adapter::parse_str(spec_content).expect("multisig.qedspec should parse");
    let matrix = coverage_matrix(&spec);
    assert_eq!(matrix.coverage_pct, 100.0);
    assert!(matrix.gaps.is_empty());
    // 8 handlers: create_vault, propose, approve, reject, execute,
    // cancel_proposal, add_member, remove_member.
    assert_eq!(matrix.operations.len(), 8);
    assert_eq!(matrix.properties.len(), 2);
}

#[test]
fn test_coverage_matrix_detects_gaps() {
    let mut h_covered = make_handler("deposit");
    h_covered.effects = vec![("balance".into(), "add".into(), "amount".into())];
    let mut h_uncovered = make_handler("withdraw");
    h_uncovered.effects = vec![("balance".into(), "sub".into(), "amount".into())];

    let spec = ParsedSpec {
        handlers: vec![h_covered, h_uncovered],
        state_fields: vec![("balance".into(), "U64".into())],
        properties: vec![ParsedProperty {
            name: "conservation".to_string(),
            expression: Some("state.balance >= 0".to_string()),
            rust_expression: Some("s.balance >= 0".to_string()),
            rust_expression_pod: Some("s.balance >= 0".to_string()),
            preserved_by: vec!["deposit".to_string()], // only covers deposit
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        }],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let matrix = coverage_matrix(&spec);
    assert_eq!(matrix.gaps, vec!["withdraw"]);
    assert!(matrix.coverage_pct < 100.0);
}

#[test]
fn test_write_without_read_lint() {
    let mut h = make_handler("deposit");
    h.guard_str = Some("amount > 0".to_string());
    h.effects = vec![
        ("balance".into(), "add".into(), "amount".into()),
        ("counter".into(), "add".into(), "1".into()),
    ];
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![
            ("authority".into(), "Pubkey".into()),
            ("balance".into(), "U64".into()),
            ("counter".into(), "U64".into()),
        ],
        properties: vec![ParsedProperty {
            name: "conservation".to_string(),
            expression: Some("s.balance >= 0".to_string()),
            rust_expression: Some("s.balance >= 0".to_string()),
            rust_expression_pod: Some("s.balance >= 0".to_string()),
            preserved_by: vec!["deposit".to_string()],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        }],
        lifecycle_states: vec!["Active".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    // "counter" is written but never read in any guard or property
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("counter")),
        "expected write_without_read for 'counter', got: {:?}",
        warnings
            .iter()
            .filter(|w| w.rule == "write_without_read")
            .map(|w| &w.subject)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_circular_lifecycle_no_terminal() {
    let mut h1 = make_handler("advance");
    h1.pre_status = Some("A".to_string());
    h1.post_status = Some("B".to_string());
    let mut h2 = make_handler("retreat");
    h2.pre_status = Some("B".to_string());
    h2.post_status = Some("A".to_string());
    let spec = ParsedSpec {
        handlers: vec![h1, h2],
        lifecycle_states: vec!["A".to_string(), "B".to_string()],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "circular_lifecycle_no_terminal"),
        "expected circular_lifecycle_no_terminal, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

// ---- contains_word unit tests ----

#[test]
fn test_contains_word_basic() {
    assert!(contains_word("balance > 0", "balance"));
    assert!(contains_word("check balance here", "balance"));
    assert!(!contains_word("imbalance > 0", "balance"));
    assert!(!contains_word("rebalance_flag", "balance"));
    assert!(!contains_word("my_balance_v2", "balance"));
}

#[test]
fn test_contains_word_short_field() {
    // Field "id" must not match inside "valid", "provide", "identity"
    assert!(!contains_word("valid > 0", "id"));
    assert!(!contains_word("provide_service", "id"));
    assert!(!contains_word("identity = true", "id"));
    // But should match when standalone
    assert!(contains_word("id > 0", "id"));
    assert!(contains_word("state.id > 0", "id"));
    assert!(contains_word("check id here", "id"));
}

#[test]
fn test_contains_word_at_boundaries() {
    assert!(contains_word("id", "id"));
    assert!(contains_word("id ", "id"));
    assert!(contains_word(" id", "id"));
    assert!(contains_word("(id)", "id"));
    assert!(contains_word("id+1", "id"));
    assert!(!contains_word("kid", "id"));
    assert!(!contains_word("ids", "id"));
}

// ---- write_without_read word-boundary tests ----

#[test]
fn test_write_without_read_no_substring_match() {
    // Field "id" written in effects, guard only has "valid" — should NOT count as read
    let mut h = make_handler("update");
    h.effects = vec![("id".to_string(), "set".to_string(), "1".to_string())];
    h.guard_str = Some("valid > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![
            ("id".to_string(), "U64".to_string()),
            ("valid".to_string(), "U64".to_string()),
        ],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
            warnings
                .iter()
                .any(|w| w.rule == "write_without_read"
                    && w.subject.as_deref() == Some("id")),
            "field 'id' should be flagged as write_without_read when guard only contains 'valid', got: {:?}",
            warnings.iter().filter(|w| w.rule == "write_without_read").collect::<Vec<_>>()
        );
}

#[test]
fn test_write_without_read_bare_word_match() {
    // Field "balance" written in effects, guard has "balance > 0" — should count as read
    let mut h = make_handler("deposit");
    h.effects = vec![(
        "balance".to_string(),
        "add".to_string(),
        "amount".to_string(),
    )];
    h.guard_str = Some("balance > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("balance".to_string(), "U64".to_string())],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("balance")),
        "field 'balance' should NOT be flagged when guard contains bare word 'balance', got: {:?}",
        warnings
            .iter()
            .filter(|w| w.rule == "write_without_read")
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_write_without_read_prefixed_match() {
    // Field "id" written, guard has "state.id > 0" — should count as read
    let mut h = make_handler("update");
    h.effects = vec![("id".to_string(), "set".to_string(), "1".to_string())];
    h.guard_str = Some("state.id > 0".to_string());
    let spec = ParsedSpec {
        handlers: vec![h],
        state_fields: vec![("id".to_string(), "U64".to_string())],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("id")),
        "field 'id' should NOT be flagged when guard contains 'state.id', got: {:?}",
        warnings
            .iter()
            .filter(|w| w.rule == "write_without_read")
            .collect::<Vec<_>>()
    );
}

// ──────────────────────────────────────────────────────────────────────
// Multi-file spec loader
// ──────────────────────────────────────────────────────────────────────

const SPEC_ROOT: &str = r#"
spec Demo

type State
  | Active of { count : U64 }
"#;

const SPEC_INC: &str = r#"
spec Demo

/// Increments count
handler inc (x : U64) : State.Active -> State.Active {
  effect { count += x }
}
"#;

const SPEC_DEC: &str = r#"
spec Demo

handler dec (x : U64) : State.Active -> State.Active {
  effect { count -= x }
}
"#;

#[test]
fn multi_file_spec_merges_handlers_across_fragments() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("demo.qedspec"), SPEC_ROOT).unwrap();
    std::fs::create_dir_all(dir.path().join("handlers")).unwrap();
    std::fs::write(dir.path().join("handlers/inc.qedspec"), SPEC_INC).unwrap();
    std::fs::write(dir.path().join("handlers/dec.qedspec"), SPEC_DEC).unwrap();

    let parsed = parse_spec_file(dir.path()).unwrap();
    assert_eq!(parsed.program_name, "Demo");
    let names: Vec<_> = parsed.handlers.iter().map(|h| h.name.as_str()).collect();
    assert!(names.contains(&"inc"), "got handlers: {:?}", names);
    assert!(names.contains(&"dec"), "got handlers: {:?}", names);
}

#[test]
fn parse_spec_file_surfaces_clear_error_for_missing_path() {
    // A non-existent --spec path must say so explicitly instead of
    // falling through to the extension check ("Unsupported spec format: .").
    let missing = std::path::PathBuf::from("/tmp/does_not_exist_g5.qedspec");
    let err = parse_spec_file(&missing).unwrap_err().to_string();
    assert!(
        err.contains("does not exist"),
        "expected 'does not exist' in error, got: {err}"
    );
    assert!(
        !err.contains("Unsupported spec format"),
        "should not surface the extension-check error for missing path: {err}"
    );
}

#[test]
fn multi_file_spec_rejects_name_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.qedspec"), SPEC_ROOT).unwrap();
    std::fs::write(
        dir.path().join("b.qedspec"),
        "spec Other\n\nhandler noop : State.Active -> State.Active { effect {} }\n",
    )
    .unwrap();

    let err = parse_spec_file(dir.path()).unwrap_err().to_string();
    assert!(
        err.contains("spec name mismatch"),
        "expected name-mismatch error, got: {err}"
    );
}

// ──────────────────────────────────────────────────────────────────────
// [shape_only_cpi] lint
// ──────────────────────────────────────────────────────────────────────

/// Declared Tier-0 interfaces with no `ensures` must not fire
/// `shape_only_cpi` — firing would force `ensures true` tautologies on
/// handlers with no meaningful post-condition. The lint still fires for
/// undeclared interfaces / missing handlers (real spec bugs).
#[test]
fn shape_only_cpi_silent_on_declared_tier0_interface() {
    let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_completeness(&parsed);
    let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
    assert!(
        hits.is_empty(),
        "Tier-0 interface with no `ensures` should not fire shape_only_cpi; got: {:?}",
        hits.iter().map(|w| &w.message).collect::<Vec<_>>()
    );
}

#[test]
fn shape_only_cpi_fires_on_undeclared_interface() {
    let src = r#"spec Demo

handler pay : State.A -> State.A {
  call Jupiter.swap(pool = amm, amount_in = 100, min_out = 90)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_completeness(&parsed);
    let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
    assert_eq!(
        hits.len(),
        1,
        "expected one shape_only_cpi warning, got {:?}",
        ws
    );
    assert!(hits[0].message.contains("not declared"));
}

#[test]
fn shape_only_cpi_silent_on_tier1_interface() {
    // Interface declares at least one ensures — no lint should fire.
    let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_completeness(&parsed);
    let hits: Vec<_> = ws.iter().filter(|w| w.rule == "shape_only_cpi").collect();
    assert!(
        hits.is_empty(),
        "Tier 1 interfaces should not lint, got: {:?}",
        hits
    );
}

// ----- cpi_unverified_callee P2 lint -----

#[test]
fn cpi_unverified_callee_fires_on_unverified_import() {
    // Simulates an `import Token from "..."` whose provider didn't
    // ship a proof package. The resolver wouldn't have populated
    // `verified_callees` so the lint should fire.
    let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_cpi_unverified_callee(&parsed);
    assert_eq!(
        ws.len(),
        1,
        "expected one unverified-callee warning; got: {ws:?}"
    );
    assert_eq!(ws[0].rule, "cpi_unverified_callee");
    assert_eq!(ws[0].priority, 2);
    assert!(ws[0].message.contains("Stance-1 axiom"));
    assert!(ws[0].fix.contains(".qed/proofs"));
    assert!(
        ws[0].fix.contains("tokenProofs"),
        "fix message should name the expected lake package; got: {}",
        ws[0].fix
    );
}

#[test]
fn cpi_unverified_callee_silent_when_verified_callees_lists_iface() {
    // Same shape but `verified_callees` has the import registered,
    // simulating a provider that did ship proofs.
    let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let mut parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    parsed
        .verified_callees
        .insert("Token".to_string(), std::path::PathBuf::from("/tmp/x"));
    let ws = check_cpi_unverified_callee(&parsed);
    assert!(
        ws.is_empty(),
        "verified callee should suppress the lint; got: {ws:?}"
    );
}

#[test]
fn cpi_unverified_callee_silent_on_in_spec_interfaces() {
    // Interface declared inline (no `import` statement) — the
    // author owns both the contract and the call, so there's no
    // external trust gap to surface.
    let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_cpi_unverified_callee(&parsed);
    assert!(
        ws.is_empty(),
        "inline interface (no import) should not fire; got: {ws:?}"
    );
}

#[test]
fn cpi_unverified_callee_silent_on_tier0_imports() {
    // Imported interface with no `ensures` — cpi_no_callee_ensures
    // (P1) owns that case; cpi_unverified_callee should stay quiet.
    let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_cpi_unverified_callee(&parsed);
    assert!(
        ws.is_empty(),
        "Tier-0 imports should not double-fire; got: {ws:?}"
    );
}

#[test]
fn cpi_unverified_callee_deduplicates_repeated_calls() {
    // Two handlers both calling Token.transfer — the lint should
    // surface the trust-gap once per (interface, handler), not per
    // call site.
    let src = r#"spec Demo

import Token from "spl_token"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:0000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay_a : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}

handler pay_b : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 2)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let ws = check_cpi_unverified_callee(&parsed);
    assert_eq!(ws.len(), 1, "should dedupe across call sites; got: {ws:?}");
}

// ----- end cpi_unverified_callee -----

#[test]
fn call_clause_populates_handler_calls() {
    let src = r#"spec Demo

handler exchange : State.A -> State.B {
  call Token.transfer(from = taker_ta, to = initializer_ta, amount = taker_amount)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let handler = &parsed.handlers[0];
    assert_eq!(handler.calls.len(), 1);
    let c = &handler.calls[0];
    assert_eq!(c.target_interface, "Token");
    assert_eq!(c.target_handler, "transfer");
    assert_eq!(c.args.len(), 3);
    assert_eq!(c.args[0].name, "from");
    assert_eq!(c.args[2].name, "amount");
    // Args carry both renderings so backends can pick the form they want.
    assert!(!c.args[0].rust_expr.is_empty());
    assert!(!c.args[0].lean_expr.is_empty());
}

// ──────────────────────────────────────────────────────────────────────
// pragma sbpf { ... } adaptation
// ──────────────────────────────────────────────────────────────────────

#[test]
fn pragma_sbpf_unpacks_inner_items() {
    let src = r#"spec Transfer

pragma sbpf {
  pubkey TOKEN_PROGRAM [6, 221, 246, 225]

  instruction transfer {
    discriminant 3
    entry 0
  }
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
    assert_eq!(parsed.pubkeys.len(), 1);
    assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
    assert_eq!(parsed.instructions.len(), 1);
    assert_eq!(parsed.instructions[0].name, "transfer");
}

#[test]
fn pragma_body_adapts_into_standard_parsed_spec_fields() {
    // Items wrapped in `pragma sbpf { ... }` must land in the same
    // ParsedSpec fields downstream consumers already read — pubkeys,
    // instructions, etc. The pragma is a grammatical namespace, not
    // a new parallel tree.
    let src = r#"spec T

pragma sbpf {
  pubkey TOKEN_PROGRAM [1, 2, 3, 4]

  instruction foo {
    discriminant 1
    entry 0
  }
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
    assert!(parsed.has_pragma("sbpf"));
    assert_eq!(parsed.pubkeys.len(), 1);
    assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
    assert_eq!(parsed.instructions.len(), 1);
    assert_eq!(parsed.instructions[0].name, "foo");
}

#[test]
fn top_level_sbpf_items_now_rejected() {
    // Platform-specifics (pubkey, instruction, assembly) only parse
    // behind `pragma sbpf { ... }` — the grammar keeps them out of the
    // core surface.
    let src = r#"spec T

pubkey TOKEN_PROGRAM [1, 2, 3, 4]
"#;
    assert!(
        crate::chumsky_adapter::parse_str(src).is_err(),
        "top-level `pubkey` should no longer parse"
    );
}

// ──────────────────────────────────────────────────────────────────────
// ML syntax — let...in in expressions
// ──────────────────────────────────────────────────────────────────────

#[test]
fn let_in_renders_to_lean_and_rust() {
    let src = r#"spec T
type State | A of { balance : U64 }

handler h (amount : U64) : State.A -> State.A {
  ensures let delta = old(state.balance) - state.balance in delta == amount
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let handler = &parsed.handlers[0];
    assert_eq!(handler.ensures.len(), 1);
    let e = &handler.ensures[0];
    // Lean form uses Lean's let-binding syntax.
    assert!(
        e.lean_expr.contains("let delta :="),
        "expected Lean let-binding, got: {}",
        e.lean_expr
    );
    // Rust form lowers to a block expression.
    assert!(
        e.rust_expr.contains("let delta ="),
        "expected Rust let-in-block, got: {}",
        e.rust_expr
    );
}

// ──────────────────────────────────────────────────────────────────────
// Smoke test — match and ctors in the grammar.
// ──────────────────────────────────────────────────────────────────────

#[test]
fn ml_match_and_ctor_already_parse() {
    let src = r#"spec T
type State | Active of { count : U64 } | Closed

handler inspect : State.Active -> State.Active {
  ensures
    match state with
    | Active a => a.count >= 0
    | Closed => true
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    assert_eq!(parsed.handlers.len(), 1);
    assert_eq!(parsed.handlers[0].ensures.len(), 1);
    // The rendered form should reference both variants.
    let lean = &parsed.handlers[0].ensures[0].lean_expr;
    assert!(lean.contains("Active"), "got: {}", lean);
    assert!(lean.contains("Closed"), "got: {}", lean);
}

#[test]
fn interface_block_populates_parsed_spec() {
    let src = r#"spec Escrow

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:abc"
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
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    assert_eq!(parsed.interfaces.len(), 1);
    let i = &parsed.interfaces[0];
    assert_eq!(i.name, "Token");
    assert_eq!(
        i.program_id.as_deref(),
        Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
    );

    let u = i.upstream.as_ref().expect("upstream present");
    assert_eq!(u.binary_hash.as_deref(), Some("sha256:abc"));
    // Lean absent by design — no overclaiming.
    assert!(!u.verified_with.contains(&"lean".to_string()));

    assert_eq!(i.handlers.len(), 1);
    let h = &i.handlers[0];
    assert_eq!(h.name, "transfer");
    assert_eq!(h.params, vec![("amount".to_string(), "U64".to_string())]);
    assert_eq!(h.accounts.len(), 3);
    assert_eq!(h.requires.len(), 1);
    assert_eq!(h.ensures.len(), 1);
}

#[test]
fn unchecked_quantifier_lint_fires_for_large_type() {
    // U64 quantifier can't be exhausted — check.rs must warn so the user
    // knows the property is being silently skipped in proptest/Kani.
    let spec = ParsedSpec {
        properties: vec![ParsedProperty {
            name: "all_balances_positive".to_string(),
            expression: Some("∀ v : Nat, v ≥ 0".to_string()),
            rust_expression: Some(
                "/* QEDGEN_UNSUPPORTED_QUANTIFIER: forall v : U64 \
                     — lower at harness level */"
                    .to_string(),
            ),
            rust_expression_pod: None,
            preserved_by: vec![],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        }],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| w.rule == "unchecked_quantifier"),
        "expected unchecked_quantifier lint for U64 forall, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
    let w = warnings
        .iter()
        .find(|w| w.rule == "unchecked_quantifier")
        .unwrap();
    assert_eq!(w.priority, 1, "unchecked_quantifier must be P1");
    assert!(
        w.message.contains("all_balances_positive"),
        "message must name the property"
    );
}

#[test]
fn unchecked_quantifier_lint_does_not_fire_for_u8() {
    // U8 forall lowers to a real iterator — no lint should fire.
    let spec = ParsedSpec {
        properties: vec![ParsedProperty {
            name: "bytes_nonneg".to_string(),
            expression: Some("∀ v : Nat, v ≥ 0".to_string()),
            rust_expression: Some("(u8::MIN..=u8::MAX).all(|v| v >= 0)".to_string()),
            rust_expression_pod: None,
            preserved_by: vec![],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        }],
        ..empty_spec()
    };
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "unchecked_quantifier"),
        "U8 forall must not fire unchecked_quantifier"
    );
}

#[test]
fn multi_file_spec_source_matches_single_file_concat() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("1.qedspec"), SPEC_ROOT).unwrap();
    std::fs::write(dir.path().join("2.qedspec"), SPEC_INC).unwrap();

    // read_spec_source must emit fragments in sorted-path order so
    // spec_hash_for_handler finds handler bodies regardless of which
    // fragment they live in.
    let src = read_spec_source(dir.path()).unwrap();
    assert!(
        src.contains("type State"),
        "root fragment missing in merged source"
    );
    assert!(
        src.contains("handler inc"),
        "handler fragment missing in merged source"
    );
}

#[test]
fn build_counterexample_resolves_named_const_in_effect() {
    let handler = ParsedHandler {
        name: "reset".to_string(),
        effects: vec![("counter".to_string(), "set".to_string(), "ZERO".to_string())],
        ..make_handler("reset")
    };
    let constants = vec![("ZERO".to_string(), "0".to_string())];
    let ce = build_counterexample(
        "s.counter \u{2264} 5",
        "bounded",
        &["counter"],
        &handler,
        &["counter"],
        &constants,
    )
    .expect("should produce a counterexample");
    let post = ce
        .post_state
        .iter()
        .find(|(f, _)| f == "counter")
        .unwrap()
        .1;
    assert_eq!(post, 0, "ZERO should resolve to 0, not fall back to 1");
}

#[test]
fn preserved_by_all_potential_violation_fires_for_named_const_effect() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Test
program_id "11111111111111111111111111111111"
const STEP = 5
type State | Active of { counter : U64 }
type Error | E
property counter_small :
  state.counter <= 3
  preserved_by all
handler tick : State.Active -> State.Active {
  permissionless
  effect { counter := STEP }
}"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "preserved_by_all_potential_violation"),
        "must warn when preserved_by all handler demonstrably violates the property"
    );
}

/// Transition property `counter >= old(counter)` preserved by an `add`
/// handler must NOT fire — guards against the counterexample builder
/// misreading `s'.counter` as a constant and applying the effect to the
/// `old(...)` side (inverting the relation into a bogus violation).
#[test]
fn preserved_by_transition_property_silent_when_add_preserves_monotonicity() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Test
program_id "11111111111111111111111111111111"
type State | Active of { counter : U64 }
type Error | E
property counter_monotonic :
  state.counter >= old(state.counter)
  preserved_by all
handler grow (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { counter += delta }
}"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "preserved_by_all_potential_violation"),
        "add preserves `counter >= old(counter)` — must not flag a violation"
    );
}

/// The same transition property `counter >= old(counter)` claimed-
/// preserved by a `sub` handler MUST still fire — decreasing the post
/// side genuinely breaks monotonicity.
#[test]
fn preserved_by_transition_property_fires_when_sub_breaks_monotonicity() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Test
program_id "11111111111111111111111111111111"
type State | Active of { counter : U64 }
type Error | E
property counter_monotonic :
  state.counter >= old(state.counter)
  preserved_by all
handler shrink : State.Active -> State.Active {
  permissionless
  effect { counter -= 1 }
}"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "preserved_by_all_potential_violation"),
        "sub breaks `counter >= old(counter)` — must flag the violation"
    );
}

/// `build_fix_suggestions` must not emit a nonsensical
/// `requires state.counter > state.counter` guard for a transition
/// property (same field on both sides). Fix A is suppressed; Fix B
/// (add to preserved_by) still applies.
#[test]
fn build_fix_suggestions_skips_self_guard_for_transition_property() {
    let handler = ParsedHandler {
        name: "shrink".to_string(),
        effects: vec![("counter".to_string(), "sub".to_string(), "1".to_string())],
        ..make_handler("shrink")
    };
    let fixes = build_fix_suggestions(
        "s'.counter \u{2265} s.counter",
        "counter_monotonic",
        &handler,
        &["counter"],
        &["counter"],
    );
    assert!(
        !fixes
            .iter()
            .any(|f| f.snippet.contains("state.counter > state.counter")
                || f.snippet.contains("state.counter < state.counter")),
        "must not suggest a self-comparison guard; got: {:?}",
        fixes.iter().map(|f| &f.snippet).collect::<Vec<_>>()
    );
    assert!(
        fixes.iter().any(|f| f.label == "Add to preserved_by"),
        "the preserved_by fix should still be offered"
    );
}

// ----- PDA seed collision -----

#[test]
fn pda_seed_collision_fires_for_identical_seeds() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"
            spec CollisionTest

            pda vault ["vault", user]
            pda escrow ["vault", user]

            state { dummy : U64 }
            "#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        warnings.iter().any(|w| w.rule == "pda_seed_collision"),
        "must warn on identical seed tuples; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

#[test]
fn pda_seed_collision_no_false_positive_for_distinct_seeds() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"
            spec CollisionTest

            pda vault ["vault", user]
            pda escrow ["escrow", user]

            state { dummy : U64 }
            "#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "pda_seed_collision"),
        "must NOT warn when seeds differ by literal discriminator"
    );
}

#[test]
fn pda_seed_possible_collision_fires_when_literals_match_but_vars_differ() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"
            spec CollisionTest

            pda order_a ["order", user_a]
            pda order_b ["order", user_b]

            state { dummy : U64 }
            "#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "pda_seed_possible_collision"),
        "must warn on same literals but different variable seeds; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
}

// ----- missing_math_overflow lint -----

#[test]
fn missing_math_overflow_fires_when_checked_arith_used_without_declaration() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | InvalidAmount

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "missing_math_overflow")
        .expect("expected missing_math_overflow warning");
    assert!(hit.message.contains("deposit"));
    assert!(hit.message.contains("PoolError::MathOverflow"));
}

#[test]
fn missing_math_overflow_silent_when_variant_is_declared() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | InvalidAmount

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
        "should not warn when MathOverflow is declared in Error sum"
    );
}

#[test]
fn missing_math_overflow_silent_when_no_checked_arithmetic() {
    // Spec uses only `effect { x := ... }` (set, no overflow path).
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Reset
program_id "11111111111111111111111111111111"
type State | Active of { counter : U64 }
type Error | InvalidAmount

handler clear : State.Active -> State.Active {
  permissionless
  effect { counter := 0 }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
        "no checked arith → no MathOverflow obligation"
    );
}

// ----- -= raises MathUnderflow (with back-compat) -----

#[test]
fn missing_math_overflow_fires_on_sub_without_underflow_or_overflow() {
    // Pure `-=` with neither MathOverflow nor MathUnderflow declared
    // → fires for MathUnderflow (the default for `-=`).
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | InvalidAmount

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance -= n }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "missing_math_overflow")
        .expect("expected missing_math_overflow warning for MathUnderflow");
    assert!(
        hit.message.contains("MathUnderflow"),
        "v2.24: `-=` defaults to MathUnderflow; message was {:?}",
        hit.message
    );
}

#[test]
fn missing_math_overflow_silent_on_sub_with_only_overflow_declared() {
    // Back-compat: declared MathOverflow but not MathUnderflow →
    // `-=` falls back to MathOverflow; lint stays silent.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler withdraw (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance -= n }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
        "back-compat: only MathOverflow declared → -= falls back; no warning"
    );
}

// ----- unknown_error_variant lint -----

#[test]
fn unknown_error_variant_fires_on_per_site_override_with_undeclared() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MathUnderflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n else MintOverflow }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "unknown_error_variant")
        .expect("expected unknown_error_variant warning");
    assert!(hit.message.contains("MintOverflow"));
    assert!(hit.message.contains("deposit"));
}

#[test]
fn unknown_error_variant_fires_on_pragma_with_undeclared() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MathUnderflow

pragma checked_overflow_error = MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    let hit = warnings
        .iter()
        .find(|w| w.rule == "unknown_error_variant")
        .expect("expected unknown_error_variant warning for pragma");
    assert!(hit.message.contains("checked_overflow_error"));
    assert!(hit.message.contains("MintOverflow"));
}

#[test]
fn unknown_error_variant_silent_when_override_is_declared() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow | MintOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  effect { balance += n else MintOverflow }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "unknown_error_variant"),
        "per-site override referencing a declared variant should not fire"
    );
    // The site provides an override, so missing_math_overflow defers
    // (the `+=` doesn't fall back to the builtin default).
    assert!(
        !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
        "per-site override defers missing_math_overflow"
    );
}

// ----- Rule 17: invariant_no_body -----

#[test]
fn invariant_no_body_fires_on_doc_only_invariant() {
    // The escrow / escrow-split shape: invariant declared with only a
    // description string, no `expr` body. Lean codegen would emit
    // `theorem conservation : True := trivial`.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Demo
type State | Active of { counter : U64 }

invariant conservation "total tokens preserved across all handlers"

handler bump : State.Active -> State.Active {
  auth admin
  accounts { admin : signer }
  effect { counter += 1 }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    let hits: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "invariant_no_body")
        .collect();
    assert_eq!(hits.len(), 1, "expected one finding: {hits:#?}");
    assert!(hits[0].message.contains("conservation"));
}

#[test]
fn invariant_no_body_silent_on_real_body() {
    // An invariant with a proper expression body — no finding.
    // The DSL form: `invariant <name> : <expr>` (one-liner, no
    // preserved_by — the expression body alone is what matters
    // for this lint).
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Demo
type State | Active of { counter : U64 }

invariant counter_nonneg : state.counter >= 0

handler bump : State.Active -> State.Active {
  auth admin
  accounts { admin : signer }
  effect { counter += 1 }
}
"#,
    )
    .unwrap();
    let warnings = check_completeness(&spec);
    assert!(
        !warnings.iter().any(|w| w.rule == "invariant_no_body"),
        "real expr body should suppress: {warnings:#?}"
    );
}

// ── P6: pubkey_state_field_unsupported ────────────────────────────────
//
// Guards the structural lowering note: a State carrying
// `authority : Pubkey` lowers to `[u8; 32]` in the verification State;
// P6 surfaces the lowering at check time.

#[test]
fn pubkey_state_field_lint_fires_on_account_type() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec PubkeyState
type State
  | Active of {
      authority : Pubkey,
      balance : U64,
    }
handler h : State.Active -> State.Active {
  permissionless
  effect { balance += 1 }
}
"#,
    )
    .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "pubkey_state_field_unsupported")
        .collect();
    assert_eq!(hits.len(), 1, "expected exactly one P6 hit: {hits:#?}");
    let w = hits[0];
    assert!(
        w.message.contains("P6:") && w.message.contains("'authority'"),
        "message must cite P6 and name the field: {}",
        w.message
    );
    // P6 is Info-only: Pubkey state fields lower to `[u8; 32]`
    // automatically; the lint just documents the lowering.
    assert!(
        w.message.contains("lowered to `[u8; 32]`"),
        "message must describe the lowering: {}",
        w.message
    );
    assert_eq!(w.priority, 3, "P6 is now a P3 informational");
    assert_eq!(w.severity, Severity::Info);
}

#[test]
fn pubkey_state_field_lint_silent_without_pubkey_field() {
    // Control: no Pubkey field in state → no P6.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec NoPubkey
type State | Active of { balance : U64 }
handler bump : State.Active -> State.Active {
  permissionless
  effect { balance += 1 }
}
"#,
    )
    .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "pubkey_state_field_unsupported"),
        "no Pubkey field → no P6, got: {warnings:#?}"
    );
}

#[test]
fn pubkey_state_field_lint_fires_per_field() {
    // Two Pubkey fields → two P6 lints, each naming its specific
    // field. The non-Pubkey `balance` must not appear in any hit's
    // subject. This pins field-scoped reporting (mirrors how
    // `wrapping_arithmetic` fires per-op).
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec PubkeyMulti
type State
  | Active of {
      authority : Pubkey,
      mint : Pubkey,
      balance : U64,
    }
handler h : State.Active -> State.Active {
  permissionless
  effect { balance += 1 }
}
"#,
    )
    .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "pubkey_state_field_unsupported")
        .collect();
    assert_eq!(hits.len(), 2, "expected two P6 hits: {hits:#?}");
    let subjects: Vec<&str> = hits
        .iter()
        .map(|w| w.subject.as_deref().unwrap_or(""))
        .collect();
    assert!(
        subjects.iter().any(|s| s.ends_with(".authority")),
        "must name authority: {subjects:?}"
    );
    assert!(
        subjects.iter().any(|s| s.ends_with(".mint")),
        "must name mint: {subjects:?}"
    );
    assert!(
        !subjects.iter().any(|s| s.ends_with(".balance")),
        "must NOT name balance: {subjects:?}"
    );
}

// ── P7: undeclared_state_field_in_effect ──────────────────────────────

#[test]
fn p7_fires_on_lhs_undeclared_field() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec P7Lhs
type State | Active of { balance : U64 }
handler bump : State.Active -> State.Active {
  permissionless
  effect { undeclared += 1 }
}
"#,
    )
    .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "undeclared_state_field_in_effect")
        .collect();
    assert!(
        hits.iter()
            .any(|w| w.message.contains("LHS") && w.message.contains("'undeclared'")),
        "expected LHS hit naming `undeclared`; got: {hits:#?}"
    );
}

#[test]
fn p7_fires_on_rhs_undeclared_state_reference() {
    // RHS check catches `state.<field>` references inside complex
    // expressions. A bare `state.X` RHS goes through render_effect's
    // path-stripping shortcut (it ends up as just `X`), which is
    // indistinguishable from a param reference at lint time — that
    // case is caught downstream by codegen unless the user wrote
    // any composition. We pin the composition case here.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec P7Rhs
type State | Active of { balance : U64 }
handler bump : State.Active -> State.Active {
  permissionless
  effect { balance := state.missing + 1 }
}
"#,
    )
    .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    let hits: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "undeclared_state_field_in_effect")
        .collect();
    assert!(
        hits.iter()
            .any(|w| w.message.contains("RHS") && w.message.contains("'missing'")),
        "expected RHS hit naming `missing`; got: {hits:#?}"
    );
}

#[test]
fn p7_silent_when_all_fields_declared() {
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec P7Clean
type State | Active of { balance : U64, total : U64 }
handler add : State.Active -> State.Active {
  permissionless
  effect { total := state.balance }
}
"#,
    )
    .expect("fixture should parse");
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "undeclared_state_field_in_effect"),
        "clean spec must not fire P7, got: {warnings:#?}"
    );
}

#[test]
fn unguarded_arithmetic_accepts_cumulative_bound_across_multiple_adds() {
    // A single `requires state.x + a + b <= U64_MAX` logically bounds
    // both `state.x += a` and `state.x += b`; the lint must accept the
    // cumulative form, not just per-pair patterns.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler deposit (a : U64) (b : U64) : State.Active -> State.Active {
  permissionless
  requires state.balance + a + b <= U64_MAX
  effect {
    balance += a
    balance += b
  }
}
"#,
    )
    .expect("cumulative-bound spec must parse");
    let warnings = check_completeness(&spec);
    let arith_hits: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "unguarded_arithmetic")
        .collect();
    assert!(
        arith_hits.is_empty(),
        "cumulative bound should satisfy unguarded_arithmetic for all adds; got: {arith_hits:#?}"
    );
}

#[test]
fn u64_max_builtin_resolves_in_requires_clause() {
    // `U64_MAX` (and friends) are seeded as builtin consts so users
    // don't have to declare `const U64_MAX = …` per spec.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Pool
program_id "11111111111111111111111111111111"
type State | Active of { balance : U64 }
type Error | MathOverflow

handler deposit (n : U64) : State.Active -> State.Active {
  permissionless
  requires state.balance + n <= U64_MAX
  effect { balance += n }
}
"#,
    )
    .expect("U64_MAX should resolve as a builtin");
    let warnings = check_completeness(&spec);
    // With the U64_MAX guard, unguarded_arithmetic should be silent.
    assert!(
        !warnings.iter().any(|w| w.rule == "unguarded_arithmetic"),
        "U64_MAX builtin should satisfy unguarded_arithmetic; got: {warnings:#?}"
    );
}

#[test]
fn p7_does_not_fire_on_state_variant_promotion() {
    // `state := .Variant { ... }` is the documented variant-promotion /
    // whole-state-assignment form; P7 must not strip the LHS root and
    // flag `state` as an undeclared field.
    let spec = crate::chumsky_adapter::parse_str(
        r#"spec Lifecycle
program_id "11111111111111111111111111111111"
type State
  | Setup of { x : U64 }
  | Active of { x : U64 }
type Error | E

handler activate : State.Setup -> State.Active {
  permissionless
  effect {
    state := .Active { x := 0 }
  }
}
"#,
    )
    .expect("variant-promotion spec must parse");
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "undeclared_state_field_in_effect"),
        "P7 must not fire on `state := .Variant {{...}}`; got: {warnings:#?}"
    );
}

#[test]
fn p7_ignores_synthetic_match_arm_handlers() {
    // `_case_N` / `_otherwise` synthetic handlers inherit their
    // parent's effects — they don't get a second P7 hit because
    // the parent already covers it.
    let mut spec = ParsedSpec::default();
    spec.account_types.push(ParsedAccountType {
        name: "State".into(),
        fields: vec![("balance".into(), "U64".into())],
        lifecycle: vec![],
        pda_ref: None,
        variants: vec![],
    });
    spec.handlers.push(ParsedHandler {
        name: "outer_case_0".into(),
        permissionless: true,
        effects: vec![("undeclared".into(), "set".into(), "0".into())],
        ..synthetic_handler_default("outer_case_0")
    });
    let warnings = check_completeness(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "undeclared_state_field_in_effect"),
        "P7 must not fire on `_case_N` synthetic handlers: {warnings:#?}"
    );
}

fn synthetic_handler_default(name: &str) -> ParsedHandler {
    ParsedHandler {
        name: name.into(),
        doc: None,
        who: None,
        on_account: None,
        pre_status: None,
        post_status: None,
        takes_params: vec![],
        guard_str: None,
        guard_str_rust: None,
        aborts_if: vec![],
        requires: vec![],
        ensures: vec![],
        modifies: None,
        let_bindings: vec![],
        aborts_total: false,
        permissionless: false,
        effects: vec![],
        effect_on_error: vec![],
        accounts: vec![],
        transfers: vec![],
        emits: vec![],
        invariants: vec![],
        establishes: vec![],
        properties: vec![],
        schema_includes: vec![],
        calls: vec![],
        effect_branches: None,
        abstract_binders: vec![],
    }
}

// Cross-ADT field-ambiguity lint. Three cases:
//   (a) two ADTs share a field name AND a property references the bare
//       name → lint fires.
//   (b) single-ADT spec → never fires (lint short-circuits).
//   (c) explicit `<adt>.<field>` qualification → does not fire.
#[test]
fn cross_adt_field_ambiguity_fires_on_bare_reference() {
    let src = r#"spec Pair

type Distribution
  | Empty
  | Active of {
      authority : Pubkey,
      balance   : U64,
    }

type Claim
  | Empty
  | Active of {
      claimant : Pubkey,
      balance  : U64,
    }

property positive_balance :
  state.balance >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_cross_adt_field_ambiguity(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "cross_adt_field_ambiguity"),
        "expected cross_adt_field_ambiguity to fire on bare `state.balance` ref, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
    );
    // The message names both ADTs so the user can pick.
    let msg = &warnings
        .iter()
        .find(|w| w.rule == "cross_adt_field_ambiguity")
        .unwrap()
        .message;
    assert!(
        msg.contains("Distribution"),
        "message must name Distribution: {}",
        msg
    );
    assert!(msg.contains("Claim"), "message must name Claim: {}", msg);
}

#[test]
fn cross_adt_field_ambiguity_silent_on_single_adt() {
    // Lending's exact shape: two ADTs but no overlapping field names.
    // Cross-ADT lint must stay silent. (We don't try lending itself
    // because the parser needs proper headers; use a synthetic two-ADT
    // spec with disjoint fields.)
    let src = r#"spec Lending

type Pool
  | Uninitialized
  | Active of {
      authority      : Pubkey,
      total_deposits : U64,
    }

type Loan
  | Empty
  | Active of {
      borrower : Pubkey,
      amount   : U64,
    }

property pool_nonneg :
  state.total_deposits >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_cross_adt_field_ambiguity(&spec);
    assert!(
        warnings.is_empty(),
        "no overlapping fields → no lint, got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.message))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn cross_adt_field_ambiguity_silent_when_qualified() {
    // Same shape as the positive-case fixture, but the property
    // qualifies the reference as `distribution.balance`. The lint
    // must NOT fire — the user has already disambiguated.
    let src = r#"spec Pair

type Distribution
  | Empty
  | Active of {
      authority : Pubkey,
      balance   : U64,
    }

type Claim
  | Empty
  | Active of {
      claimant : Pubkey,
      balance  : U64,
    }

property positive_balance :
  distribution.balance >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_cross_adt_field_ambiguity(&spec);
    assert!(
        warnings.is_empty(),
        "qualified `distribution.balance` should clear the ambiguity, got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.message))
            .collect::<Vec<_>>(),
    );
}

// ========================================================================
// ParsedAccountType.variants populated for multi-variant ADTs
// ========================================================================

#[test]
fn multi_variant_adt_populates_account_variants() {
    // Two-variant state ADT. Flat `fields` view stays the union (first
    // occurrence wins); `variants` carries the per-variant shape so
    // codegen can emit `pub enum State { Setup{...}, Active{...} }`.
    let src = r#"spec Multi
program_id "11111111111111111111111111111111"

type State
  | Setup of { owner : Pubkey }
  | Active of {
      owner : Pubkey,
      pool  : U64,
    }

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let state = spec
        .account_types
        .iter()
        .find(|a| a.name == "State")
        .expect("state account type present");

    assert_eq!(
        state.variants.len(),
        2,
        "two-variant ADT should produce two ParsedVariant entries"
    );
    assert_eq!(state.variants[0].name, "Setup");
    assert_eq!(state.variants[1].name, "Active");
    assert_eq!(state.variants[0].fields.len(), 1);
    assert_eq!(state.variants[1].fields.len(), 2);
    // Flat view stays populated as the union (back-compat).
    assert!(state.fields.iter().any(|(n, _)| n == "owner"));
    assert!(state.fields.iter().any(|(n, _)| n == "pool"));
}

#[test]
fn no_payload_variant_keeps_empty_field_list() {
    // A unit-style variant (no payload) should still appear in
    // `variants` with an empty field list so codegen can emit
    // `pub enum State { Inactive, Active{...} }`.
    let src = r#"spec NoPayload
program_id "11111111111111111111111111111111"

type State
  | Inactive
  | Active of { pool : U64 }

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let state = spec
        .account_types
        .iter()
        .find(|a| a.name == "State")
        .expect("state account type present");
    assert_eq!(state.variants.len(), 2);
    let inactive = state
        .variants
        .iter()
        .find(|v| v.name == "Inactive")
        .expect("unit variant retained");
    assert!(
        inactive.fields.is_empty(),
        "no-payload variant has zero fields"
    );
}

// ========================================================================
// Variant-prefixed effect LHS doesn't false-positive lints
// ========================================================================

#[test]
fn variant_prefixed_lhs_passes_all_effect_lints() {
    // `Active.pool := amount` on a multi-variant ADT state must NOT
    // trigger undeclared_state_field_in_effect (P7 LHS),
    // write_without_read (Rule 13), or unused_field (Rule 4) — all
    // three walk the LHS string and must not treat the variant prefix
    // as a field name.
    let src = r#"spec MultiVar
program_id "11111111111111111111111111111111"

type State
  | Setup of { owner : Pubkey }
  | Active of {
      owner : Pubkey,
      pool  : U64,
    }

type Error
  | MathOverflow

handler activate (amount : U64) : State.Setup -> State.Active {
  auth owner
  requires amount > 0
  effect {
    Active.pool := amount
  }
}

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_completeness(&spec);
    let rules: Vec<&str> = warnings.iter().map(|w| w.rule.as_str()).collect();

    assert!(
            !rules.contains(&"undeclared_state_field_in_effect"),
            "P7 should not fire on `Active.pool := amount` (Active is a variant, pool is its field) — got: {:?}",
            rules
        );
    assert!(
            !rules.contains(&"write_without_read"),
            "write_without_read should match `pool` (read by property) to `Active.pool` (written) — got: {:?}",
            rules
        );
    assert!(
        !rules.contains(&"unused_field"),
        "unused_field should see `pool` as modified via `Active.pool := amount` — got: {:?}",
        rules
    );
}

#[test]
fn variant_prefixed_lhs_still_catches_unknown_field() {
    // A real bug: `Active.poool := amount` (typo). P7 should fire
    // with subject `activate.Active.poool` — the variant prefix is
    // legal, the field name behind it isn't declared anywhere.
    let src = r#"spec MultiVarTypo
program_id "11111111111111111111111111111111"

type State
  | Setup of { owner : Pubkey }
  | Active of {
      owner : Pubkey,
      pool  : U64,
    }

type Error
  | MathOverflow

handler activate (amount : U64) : State.Setup -> State.Active {
  auth owner
  requires amount > 0
  effect {
    Active.poool := amount
  }
}

property pool_nonneg :
  state.pool >= 0
  preserved_by all
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_completeness(&spec);
    let p7s: Vec<&CompletenessWarning> = warnings
        .iter()
        .filter(|w| w.rule == "undeclared_state_field_in_effect")
        .collect();
    assert_eq!(
        p7s.len(),
        1,
        "expected exactly one P7 hit on the misspelled `poool`, got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
    );
    assert!(
        p7s[0].subject.as_deref().unwrap_or("").contains("poool"),
        "P7 subject should name the misspelled field, got: {:?}",
        p7s[0].subject
    );
}

// ========================================================================
// vacuous_property_lowering lint
// ========================================================================

const VPL_SPEC_HEAD: &str = r#"
spec VplTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, admin : U64 }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}
"#;

#[test]
fn parse_top_level_cmp_handles_simple_comparison() {
    let r = parse_top_level_cmp("s.balance >= s.balance");
    assert_eq!(r, Some(("s.balance", ">=", "s.balance")));
}

#[test]
fn parse_top_level_cmp_handles_equality() {
    let r = parse_top_level_cmp("s.admin == s.admin");
    assert_eq!(r, Some(("s.admin", "==", "s.admin")));
}

#[test]
fn parse_top_level_cmp_returns_none_on_non_comparison() {
    let r = parse_top_level_cmp("s.x + 1");
    assert!(r.is_none(), "expected None on non-comparison; got: {:?}", r);
}

#[test]
fn vpl_lint_silent_on_author_tautology_without_old() {
    // pool.qedspec:660-662 pattern — `state.x == state.x` with no
    // `old(...)` in the AST. The author wants the field surfaced in
    // proofs; the lint must NOT fire.
    let src = format!(
        "{}{}",
        VPL_SPEC_HEAD, r#"property admin_tracked : state.admin == state.admin preserved_by all"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_vacuous_property_lowering(&spec);
    assert!(
        warnings.is_empty(),
        "author-written tautology (no Expr::Old) must not fire; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    );
}

#[test]
fn vpl_lint_silent_on_distinct_sides() {
    // Distinct comparison — silent regardless of `old(...)`.
    let src = format!(
        "{}{}",
        VPL_SPEC_HEAD, r#"property balance_le_max : state.balance <= 1000 preserved_by all"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_vacuous_property_lowering(&spec);
    assert!(
        warnings.is_empty(),
        "distinct-sides comparison must not fire; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    );
}

#[test]
fn vpl_lint_silent_on_binary_property_post_slice_2() {
    // A binary property (`old(...)` in body) lowers to
    // `post.balance >= pre.balance` — distinct sides, no tautology.
    // If the lint fires here, codegen regressed.
    let src = format!(
        "{}{}",
        VPL_SPEC_HEAD,
        r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_vacuous_property_lowering(&spec);
    let vpl: Vec<_> = warnings
        .iter()
        .filter(|w| w.rule == "vacuous_property_lowering")
        .collect();
    assert!(
        vpl.is_empty(),
        "binary property correctly lowered to pre/post must not fire VPL; got: {:?}",
        vpl.iter().map(|w| &w.message).collect::<Vec<_>>(),
    );
}

#[test]
fn vpl_lint_fires_on_literal_true_body() {
    // Construct a property whose rust_expression is the literal "true"
    // — Rule 3 unconditionally fires.
    let mut spec = ParsedSpec::default();
    spec.properties.push(ParsedProperty {
        name: "always_true".to_string(),
        expression: Some("True".to_string()),
        rust_expression: Some("true".to_string()),
        rust_expression_pod: Some("true".to_string()),
        preserved_by: vec![],
        per_slot: None,
        quantifier_lint: None,
        class: PropertyClass::Unary,
        ast_body: None,
    });
    let warnings = check_vacuous_property_lowering(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "vacuous_property_lowering"),
        "literal `true` body must fire VPL; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
    );
}

// ========================================================================
// old_in_single_state_context lint
// ========================================================================

const OLD_SSC_SPEC_HEAD: &str = r#"
spec OldSscTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64 }

type Error
  | E
  | BadGuard
"#;

#[test]
fn old_ssc_lint_fires_on_old_in_requires() {
    // `old(...)` inside a `requires` body — category error, P1.
    let src = format!(
        "{}{}",
        OLD_SSC_SPEC_HEAD,
        r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  requires state.balance >= old(state.balance) else BadGuard
  effect { balance := balance + delta }
}
"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_old_in_single_state_context(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "old_in_single_state_context"),
        "expected lint to fire on old() inside requires; got: {:?}",
        warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
    );
    let w = &warnings[0];
    assert_eq!(w.severity, Severity::Warning);
    assert_eq!(w.priority, 1);
    assert!(w.message.contains("requires"), "msg: {}", w.message);
}

#[test]
fn old_ssc_lint_fires_on_old_in_invariant() {
    // `old(...)` inside an `invariant` body — category error, P1.
    let src = format!(
        "{}{}",
        OLD_SSC_SPEC_HEAD,
        r#"
invariant balance_nondec : state.balance >= old(state.balance)

handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}
"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_old_in_single_state_context(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "old_in_single_state_context" && w.message.contains("invariant")),
        "expected lint to fire on old() inside invariant; got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.message))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn old_ssc_lint_silent_on_clean_requires() {
    // `requires` without `old(...)` — silent, no false positive.
    let src = format!(
        "{}{}",
        OLD_SSC_SPEC_HEAD,
        r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  requires delta > 0 else BadGuard
  effect { balance := balance + delta }
}
"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_old_in_single_state_context(&spec);
    assert!(
        warnings.is_empty(),
        "clean requires must not fire the lint; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    );
}

#[test]
fn old_ssc_lint_silent_on_old_in_ensures() {
    // `old(...)` inside `ensures` — the right context, must NOT fire.
    let src = format!(
        "{}{}",
        OLD_SSC_SPEC_HEAD,
        r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
  ensures state.balance >= old(state.balance)
}
"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_old_in_single_state_context(&spec);
    assert!(
        warnings.is_empty(),
        "old() in ensures must not fire the lint; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    );
}

#[test]
fn old_ssc_lint_silent_on_old_in_property() {
    // `old(...)` inside a `property` body — the right context, must
    // NOT fire.
    let src = format!(
        "{}{}",
        OLD_SSC_SPEC_HEAD,
        r#"
handler tweak (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}

property balance_monotonic :
  state.balance >= old(state.balance)
  preserved_by all
"#
    );
    let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
    let warnings = check_old_in_single_state_context(&spec);
    assert!(
        warnings.is_empty(),
        "old() in property body must not fire the single-state lint; got: {:?}",
        warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
    );
}

#[test]
fn vpl_lint_fires_on_unsupported_quantifier_marker() {
    // Construct a property whose rust_expression carries the marker
    // — Rule 2 unconditionally fires.
    let mut spec = ParsedSpec::default();
    spec.properties.push(ParsedProperty {
        name: "stub_forall".to_string(),
        expression: Some("forall x : U64, x > 0".to_string()),
        rust_expression: Some(format!(
            "/* {} : forall x : U64, x > 0 */ true",
            QEDGEN_UNSUPPORTED_MARKER
        )),
        rust_expression_pod: Some("true".to_string()),
        preserved_by: vec![],
        per_slot: None,
        quantifier_lint: None,
        class: PropertyClass::Unary,
        ast_body: None,
    });
    let warnings = check_vacuous_property_lowering(&spec);
    assert!(
        warnings
            .iter()
            .any(|w| w.rule == "vacuous_property_lowering"
                && w.message.contains("QEDGEN_UNSUPPORTED_QUANTIFIER")),
        "marker body must fire VPL with marker mention; got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.message))
            .collect::<Vec<_>>(),
    );
}

/// ref_impl with multiplication over U64 params trips the lint: Lean
/// lowers to `Nat` (no overflow); Rust runs `u64 * u64` which can wrap
/// or panic.
#[test]
fn ref_impl_with_multiplication_over_u64_fires_unbounded_arith_lint() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl scaled (a : U64) (b : U64) : U64 = a * b

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_ref_impl_unbounded_arith(&spec);
    assert!(
            warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"
                    && w.subject.as_deref() == Some("scaled")),
            "expected ref_impl_unbounded_arith on `scaled`; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
}

/// Pure-division ref_impl doesn't trip the lint — `/` cannot produce
/// values exceeding the inputs in unsigned arithmetic.
#[test]
fn ref_impl_with_division_only_does_not_fire_unbounded_arith_lint() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { x : U64 }

ref_impl half (a : U64) : U64 = a / 2

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect { x := amt }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_ref_impl_unbounded_arith(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "ref_impl_unbounded_arith"),
        "lint should not fire on division-only ref_impl; got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.subject))
            .collect::<Vec<_>>(),
    );
}

/// Ref impls without bounded-numeric params (e.g., Pubkey predicates)
/// don't trip the lint even when they do arithmetic on other inputs.
/// Lean and Rust agree on Bool / Pubkey semantics, so no gap.
#[test]
fn ref_impl_with_no_numeric_params_does_not_fire_unbounded_arith_lint() {
    let src = r#"spec Pool
type Error | InvalidAmount
type State = { admin : Pubkey }

ref_impl is_admin (who : Pubkey) (admin : Pubkey) : Bool = who == admin

handler set (amt : U64) {
  requires amt > 0 else InvalidAmount
  effect {}
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let warnings = check_ref_impl_unbounded_arith(&spec);
    assert!(
        !warnings
            .iter()
            .any(|w| w.rule == "ref_impl_unbounded_arith"),
        "lint should not fire when ref_impl has no bounded-numeric IO; got: {:?}",
        warnings
            .iter()
            .map(|w| (&w.rule, &w.subject))
            .collect::<Vec<_>>(),
    );
}

// ------------------------------------------------------------------
// collect_require_verified_findings
// ------------------------------------------------------------------

#[test]
fn require_verified_fires_on_unverified_import_with_ensures() {
    // Non-sentinel binary_hash so the sentinel exemption doesn't
    // intercept. `verified_callees` is empty → provider shipped no
    // proof package → finding.
    let src = r#"spec Demo

import Token from "amm_lib"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let findings = collect_require_verified_findings(&parsed);
    assert_eq!(
        findings.len(),
        1,
        "expected one finding for unverified Token; got: {findings:?}"
    );
    assert_eq!(findings[0].interface_name, "Token");
    assert!(
        findings[0].fix_hint.contains(".qed/proofs"),
        "fix hint should point at the proof-package path; got: {}",
        findings[0].fix_hint
    );
}

#[test]
fn require_verified_silent_when_provider_shipped_proofs() {
    // verified_callees populated → provider has proofs → no finding.
    let src = r#"spec Demo

import Token from "amm_lib"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let mut parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    parsed
        .verified_callees
        .insert("Token".to_string(), std::path::PathBuf::from("/tmp/x"));
    let findings = collect_require_verified_findings(&parsed);
    assert!(
        findings.is_empty(),
        "verified callee must suppress the finding; got: {findings:?}"
    );
}

#[test]
fn require_verified_silent_on_tier0_imports() {
    // No ensures clauses on any handler → Tier 0. Owned by the
    // cpi_no_callee_ensures P1 lint, not by --require-verified.
    let src = r#"spec Demo

import Token from "amm_lib"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let findings = collect_require_verified_findings(&parsed);
    assert!(
        findings.is_empty(),
        "Tier-0 (no ensures) imports must not fire --require-verified; got: {findings:?}"
    );
}

#[test]
fn require_verified_silent_on_sentinel_pinned_natives() {
    // Sentinel binary_hash (sha256:00…00) marks a native program
    // (System Program style) — the validator runtime is the trust
    // boundary, not a proof package. `--require-verified` exempts
    // these so any spec that imports `from "system"` doesn't
    // false-fail.
    let src = r#"spec Demo

import System from "system_lib"

interface System {
  program_id "11111111111111111111111111111111"
  upstream { binary_hash "sha256:0000000000000000000000000000000000000000000000000000000000000000" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call System.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let findings = collect_require_verified_findings(&parsed);
    assert!(
        findings.is_empty(),
        "sentinel-pinned native must be exempt; got: {findings:?}"
    );
}

#[test]
fn require_verified_silent_on_inline_interfaces() {
    // Interface declared inline (no `import` statement) — author
    // owns both sides of the contract. `--require-verified` only
    // gates on imported interfaces.
    let src = r#"spec Demo

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream { binary_hash "sha256:abc123" }
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

handler pay : State.A -> State.A {
  call Token.transfer(from = src_ta, to = dst_ta, amount = 1)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
    let findings = collect_require_verified_findings(&parsed);
    assert!(
        findings.is_empty(),
        "inline interfaces must not fire; got: {findings:?}"
    );
}

// ----- end collect_require_verified_findings -----
