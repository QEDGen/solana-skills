use super::*;
use std::path::Path as FsPath;

/// The flat-state transition renders `Stmt::Branch` as a real Lean
/// `match` with per-arm semantics: checked-add bounds gate only their
/// own arm, and the model applies exactly one arm (applying the
/// *union* of every arm unconditionally is semantically wrong).
#[test]
fn flat_transition_renders_branch_as_lean_match() {
    let src = r#"spec CondFee
program_id "11111111111111111111111111111111"
type State
  | Active of { a : U64, b : U64, d : U64 }
type Error
  | InvalidAmount
handler route (fee_type : U8) (amount : U64) : State.Active -> State.Active {
  permissionless
  requires amount > 0 else InvalidAmount
  effect {
    match fee_type {
      0 => a +=! amount,
      1 => b += amount,
      _ => d := 0,
    }
  }
}
"#;
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let mut out = String::new();
    emit_handler_transition(&mut out, &mir, &mir.handlers[0]);

    assert!(
        out.contains("match fee_type with"),
        "transition must render a Lean match:\n{out}"
    );
    // Bound guards mirror `build_guard_cond_parts` exactly: ALL add
    // kinds (checked / saturating / wrapping) gain the `≤ MAX`
    // conjunct in the flat renderer — per-arm, gating only their
    // own arm. (Refining Sat/Wrap Lean semantics is a separate,
    // pre-existing concern — see the same all-kinds match in
    // `build_guard_cond_parts`.)
    assert!(
        out.contains(
            "| 0 => if s.a + amount \u{2264} 18446744073709551615 then \
                 some { s with a := s.a + amount } else none"
        ),
        "saturating arm's bound gates only that arm:\n{out}"
    );
    assert!(
        out.contains(
            "| 1 => if s.b + amount \u{2264} 18446744073709551615 then \
                 some { s with b := s.b + amount } else none"
        ),
        "checked arm's overflow bound gates only that arm:\n{out}"
    );
    assert!(
        out.contains("| _ => some { s with d := 0 }"),
        "wildcard arm renders from Branch.default:\n{out}"
    );
    // The union must not leak into the top-level guard: only the
    // requires conjunct remains.
    assert!(
        out.contains("if amount > 0 then\n"),
        "top-level guard keeps only the requires conjunct:\n{out}"
    );
    // Exactly one record update per field (no union duplication).
    assert_eq!(
        out.matches("a := s.a + amount").count(),
        1,
        "arm effect rendered exactly once:\n{out}"
    );
}

/// The ADT (inductive multi-variant) transition renders `Stmt::Branch`
/// as a NESTED Lean `match` on the scrutinee — each arm builds the
/// post-variant from only its OWN effects, checked-add arms carry their
/// own per-arm overflow guard, and untaken arms don't abort. Before this
/// #66 follow-up the ADT renderer flattened all arms to their union
/// (applying every arm's effect unconditionally — semantically wrong).
/// Parallel to `flat_transition_renders_branch_as_lean_match`.
#[test]
fn adt_transition_renders_branch_as_nested_lean_match() {
    let mir = lower_fixture(
        "crates/qedgen/tests/fixtures/regressions/issue-42-conditional/adt_router.qedspec",
    );
    assert!(mir.adt_state, "fixture must use `pragma state_repr = adt`");
    let out = render(&mir);

    // Pre-variant match → requires guard → nested scrutinee match.
    assert!(
        out.contains(
            "  | .Active bucket_a bucket_b bucket_c =>\n    \
                 if amount > 0 then\n      match bucket with\n"
        ),
        "ADT branch must nest a scrutinee match under the pre-variant + \
             requires guard:\n{out}"
    );
    // Each arm applies ONLY its own field, gated by its own overflow bound.
    assert!(
        out.contains(
            "      | 0 => if bucket_a + amount \u{2264} 18446744073709551615 then \
                 some (.Active (bucket_a + amount) bucket_b bucket_c) else none"
        ),
        "arm 0 applies only bucket_a, guarded by its own overflow bound:\n{out}"
    );
    assert!(
        out.contains(
            "      | 1 => if bucket_b + amount \u{2264} 18446744073709551615 then \
                 some (.Active bucket_a (bucket_b + amount) bucket_c) else none"
        ),
        "arm 1 applies only bucket_b:\n{out}"
    );
    assert!(
        out.contains("      | _ => some (.Active bucket_a bucket_b 0)\n    else none"),
        "wildcard arm zeros only bucket_c; `else none` closes the requires \
             guard:\n{out}"
    );
    // No union flattening: bucket_b's add appears in exactly one arm.
    assert_eq!(
        out.matches("(bucket_b + amount)").count(),
        1,
        "no union flattening — bucket_b add appears in exactly one arm:\n{out}"
    );
}

/// sBPF Lean codegen regression gate. The bundled `examples/sbpf/*`
/// specs use modern `handler` syntax with no `instruction` blocks and
/// only exercise `render_sbpf`'s header path; this old-syntax `VaultLock`
/// fixture exercises the full renderer: per-instruction namespaces,
/// offset/`ea_*` lemmas, guard theorem stubs (`==`, `>=`,
/// field-vs-field RHS, no-`checks`), the completeness `structure Spec`,
/// and property stubs. Regenerate intentionally:
/// `UPDATE_SBPF_GOLDEN=1 cargo test sbpf_render_matches_golden`.
const VAULT_LOCK_SBPF_SPEC: &str = include_str!("../../../tests/fixtures/vault_lock_sbpf.qedspec");
const VAULT_LOCK_SBPF_GOLDEN: &str =
    include_str!("../../../tests/fixtures/vault_lock_sbpf.Spec.lean.golden");

#[test]
fn sbpf_render_matches_golden() {
    let parsed = crate::chumsky_adapter::parse_str(VAULT_LOCK_SBPF_SPEC)
        .expect("parse vault-lock sBPF fixture");
    assert!(
        parsed.is_assembly_target(),
        "vault-lock fixture should be an assembly target"
    );
    let ported = render_sbpf(&parsed);

    if std::env::var("UPDATE_SBPF_GOLDEN").is_ok() {
        std::fs::write(
            format!(
                "{}/tests/fixtures/vault_lock_sbpf.Spec.lean.golden",
                std::env::var("CARGO_MANIFEST_DIR").unwrap()
            ),
            &ported,
        )
        .unwrap();
        return;
    }

    // Guard against a vacuous golden: the full renderer must fire.
    for marker in [
        "namespace LockVault",
        "@[simp] theorem ea_",
        "theorem rejects_invalid_discriminant",
        "structure Spec (progAt",
        "theorem memory_safety : True := trivial",
    ] {
        assert!(
            ported.contains(marker),
            "ported sBPF output missing `{marker}`:\n{ported}"
        );
    }

    assert_eq!(
        VAULT_LOCK_SBPF_GOLDEN, ported,
        "render_sbpf output drifted from the golden — \
             regenerate with UPDATE_SBPF_GOLDEN=1 if intentional"
    );
}

fn lower_fixture(rel_path: &str) -> Mir {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = FsPath::new(&manifest_dir)
        .ancestors()
        .nth(2)
        .expect("workspace root above crates/qedgen");
    let spec_path = workspace_root.join(rel_path);
    let parsed = crate::check::parse_spec_file(&spec_path)
        .unwrap_or_else(|e| panic!("parse {}: {e}", spec_path.display()));
    crate::mir::lower(&parsed)
}

/// The inductive multi-variant State representation is opted into via
/// `pragma state_repr = adt`, decoupled from the incidental
/// `WrongState` error variant that once keyed it (footgun:
/// adding/removing a lifecycle error silently flipped flat↔ADT).
/// Same State shape + same `WrongState` error ⇒ flat by default,
/// `inductive State` only when the pragma is present.
#[test]
fn state_repr_pragma_dispatches_inductive_vs_flat() {
    // No effect bodies → no `Variant.field`-vs-bare effect-syntax
    // dependence; the dispatch keys only on the pragma + shape.
    let body = "\n\
            program_id \"11111111111111111111111111111111\"\n\
            \n\
            type State\n\
            \x20 | Uninitialized\n\
            \x20 | Active of { balance : U64 }\n\
            \x20 | Closed\n\
            \n\
            type Error\n\
            \x20 | InvalidAmount\n\
            \x20 | WrongState\n\
            \n\
            handler open (amount : U64) : State.Uninitialized -> State.Active {\n\
            \x20 auth owner\n\
            \x20 accounts { owner : signer, writable }\n\
            \x20 requires amount > 0 else InvalidAmount\n\
            }\n";

    let flat =
        crate::chumsky_adapter::parse_str(&format!("spec Flat\n{body}")).expect("parse flat spec");
    let flat_lean = render(&crate::mir::lower(&flat));
    assert!(
        flat_lean.contains("structure State where"),
        "default (no pragma) must lower to the flat struct"
    );
    assert!(
        !flat_lean.contains("inductive State where"),
        "default must NOT take the inductive ADT path"
    );

    let adt =
        crate::chumsky_adapter::parse_str(&format!("spec Adt\npragma state_repr = adt\n{body}"))
            .expect("parse adt spec");
    let adt_mir = crate::mir::lower(&adt);
    assert!(adt_mir.adt_state, "pragma must lift to Mir::adt_state");
    let adt_lean = render(&adt_mir);
    assert!(
        adt_lean.contains("inductive State where"),
        "pragma state_repr = adt must route to render_single_account_adt"
    );
}

#[test]
fn flat_effect_rhs_field_ref_is_qualified() {
    // A `set` effect copying one state field into another
    // (`reserved := state.cap`) is stored bare (`cap`) by the adapter;
    // emitted verbatim it's an unknown Lean identifier. It must
    // re-qualify to `s.cap` while a handler param stays bare.
    let src = "spec BudgetRepro\n\
            type State\n\
            \x20 | Uninitialized\n\
            \x20 | Active of { cap : U64, used : U64, reserved : U64 }\n\
            type Error\n\
            \x20 | Zero\n\
            handler open (c : U64) : State.Uninitialized -> State.Active {\n\
            \x20 permissionless\n\
            \x20 accounts { payer : signer, writable }\n\
            \x20 requires c > 0 else Zero\n\
            \x20 effect { cap := c\n\
            \x20          used := 0\n\
            \x20          reserved := 0 }\n\
            }\n\
            handler reset : State.Active -> State.Active {\n\
            \x20 permissionless\n\
            \x20 accounts { caller : signer }\n\
            \x20 effect { used := 0\n\
            \x20          reserved := state.cap }\n\
            }\n";
    let parsed = crate::chumsky_adapter::parse_str(src).expect("parse budget spec");
    let out = render(&crate::mir::lower(&parsed));
    assert!(
        out.contains("reserved := s.cap"),
        "state-field RHS must render qualified `s.cap`:\n{out}"
    );
    assert!(
        !out.contains("reserved := cap,") && !out.contains("reserved := cap "),
        "bare `cap` (unknown identifier) must not appear:\n{out}"
    );
    // A handler param assigned to a field stays bare (it IS in scope).
    assert!(
        out.contains("cap := c"),
        "handler-param RHS must stay bare `c`:\n{out}"
    );
}

#[test]
fn render_emits_header_namespace_state() {
    let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir);

    // Header imports present.
    assert!(out.contains("import QEDGen.Solana.Account"));
    assert!(out.contains("import QEDGen.Solana.State"));

    // Namespace matches the spec name.
    assert!(out.contains("namespace Escrow"));
    assert!(out.contains("end Escrow"));

    // open QEDGen.Solana follows the namespace.
    assert!(out.contains("open QEDGen.Solana"));
}

#[test]
fn render_lifecycle_marker_threshold() {
    // escrow has 3 lifecycle states (Uninitialized | Open | Closed);
    // verify the <2 no-Status-marker boundary.
    let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir);
    let lifecycle_count = mir.state.lifecycle_states.len();
    if lifecycle_count >= 2 {
        assert!(out.contains("inductive Status"));
    } else {
        assert!(!out.contains("inductive Status"));
    }
}

#[test]
fn render_aborts_if_clauses() {
    let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir);

    // escrow declares `requires deposit_amount > 0 and receive_amount > 0
    // else InvalidAmount` on initialize — should produce an
    // `initialize_aborts_if_InvalidAmount` theorem with the
    // negated predicate as hypothesis.
    assert!(
        out.contains("theorem initialize_aborts_if_InvalidAmount"),
        "expected initialize_aborts_if_InvalidAmount theorem:\n{}",
        &out[..out.len().min(2000)]
    );
    // The hypothesis should be the negation of the requires
    // predicate.
    assert!(
        out.contains("¬(deposit_amount > 0"),
        "expected negated requires hypothesis"
    );
    // The abort-conditions header should appear.
    assert!(out.contains("Abort conditions"));
}

#[test]
fn render_skips_account_pubkey_aborts() {
    // exchange + cancel both have `requires initializer_ta.pubkey == ...`
    // — these reference a handler-account's .pubkey field which isn't
    // in Lean scope. The filter should skip them.
    let mir = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let out = render(&mir);
    // No theorem should reference `initializer_ta.pubkey` in its
    // hypothesis — it's filtered out.
    assert!(
        !out.contains("(h : ¬(initializer_ta.pubkey"),
        "account-pubkey requires should be filtered from abort theorems:\n{}",
        out
    );
}

#[test]
fn render_emits_constants() {
    // The issue-8 pool fixture declares SCHEDULE_LANES,
    // RATE_PRECISION, VECTOR_COUNT, ….
    let mir = lower_fixture("crates/qedgen/tests/fixtures/regressions/issue-8/pool.qedspec");
    let out = render(&mir);
    assert!(
        out.contains("abbrev SCHEDULE_LANES : Nat := 32"),
        "expected SCHEDULE_LANES abbrev"
    );
    assert!(out.contains("abbrev RATE_PRECISION : Nat := 1000000"));
}

#[test]
fn render_emits_uninterpreted_helpers() {
    // Synthetic MIR with an uninterpreted helper. Pilot fixtures
    // don't declare helpers explicitly (check.rs infers them when
    // an undeclared fn is referenced), so build the MIR by hand
    // to exercise the emit path.
    let mir = Mir {
        name: "T".to_string(),
        state: crate::mir::StateAdt::default(),
        account_states: vec![],
        accounts: crate::mir::AccountTable::default(),
        errors: crate::mir::ErrorEnum::default(),
        imports: std::collections::BTreeMap::new(),
        handlers: vec![],
        invariants: vec![],
        events: vec![],
        constants: vec![],
        hooks: vec![],
        uninterpreted_helpers: vec![crate::mir::UninterpretedHelper {
            name: "is_valid".to_string(),
            arg_types: vec!["Nat".to_string()],
            return_type: "Bool".to_string(),
        }],
        ref_impls: vec![],
        properties: vec![],
        covers: vec![],
        liveness_props: vec![],
        environments: vec![],
        ghosts: vec![],
        records: vec![],
        is_assembly: false,
        adt_state: false,
    };
    let out = render(&mir);
    assert!(
        out.contains("opaque is_valid : Nat \u{2192} Bool"),
        "expected opaque is_valid : Nat → Bool in:\n{}",
        out
    );
    assert!(out.contains("Uninterpreted helpers"));
}

#[test]
fn render_emits_ref_impls() {
    let mir = Mir {
        name: "T".to_string(),
        state: crate::mir::StateAdt::default(),
        account_states: vec![],
        accounts: crate::mir::AccountTable::default(),
        errors: crate::mir::ErrorEnum::default(),
        imports: std::collections::BTreeMap::new(),
        handlers: vec![],
        invariants: vec![],
        events: vec![],
        constants: vec![],
        hooks: vec![],
        uninterpreted_helpers: vec![],
        ref_impls: vec![crate::mir::RefImpl {
            name: "scale".to_string(),
            doc: None,
            params: vec![
                ("a".to_string(), "U64".to_string()),
                ("b".to_string(), "U64".to_string()),
            ],
            return_type: "U64".to_string(),
            lean_body: "a * b".to_string(),
            rust_body: "a * b".to_string(),
        }],
        properties: vec![],
        covers: vec![],
        liveness_props: vec![],
        environments: vec![],
        ghosts: vec![],
        records: vec![],
        is_assembly: false,
        adt_state: false,
    };
    let out = render(&mir);
    assert!(
        out.contains("def scale (a : Nat) (b : Nat) : Nat := a * b"),
        "expected ref_impl scale lowered:\n{}",
        out
    );
}

#[test]
fn render_emits_properties_with_preservation() {
    // Lending is multi-account (Pool + Loan), so the pool_solvency
    // predicate and master theorem both bind to `PoolState` /
    // `PoolOperation` (the property's fields live on Pool).
    let mir = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir);

    assert!(
        out.contains("def pool_solvency (s : PoolState) : Prop :="),
        "expected pool_solvency predicate def on PoolState:\n{}",
        &out[..out.len().min(3000)]
    );

    assert!(
        out.contains("theorem pool_solvency_inductive"),
        "expected pool_solvency_inductive master theorem"
    );
    assert!(out.contains("(op : PoolOperation)"));
}

#[test]
fn render_emits_invariant_theorems() {
    // Multi-account specs emit invariants as structured comments;
    // variant-typed binder lowering is a v3.0 item.
    let mir = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir);
    assert!(
            out.contains("-- INVARIANT OBLIGATION (declared, multi-account translation deferred): collateral_backing"),
            "expected collateral_backing invariant comment"
        );
    assert!(
        out.contains("--   predicate body:"),
        "expected predicate body line in invariant comment"
    );
}

#[test]
fn render_emits_cover_theorems() {
    // Lending's two cover traces span both accounts → skip-comments,
    // with the section header still written. Single-account
    // auto-discharge is covered by the escrow snapshot.
    let mir = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir);
    assert!(
        out.contains("-- Cover properties"),
        "expected cover section header even when all skipped"
    );
    assert!(
            out.contains(
                "-- cover_borrow_repay_cycle: trace [init_pool, deposit, borrow, repay] spans multiple account types, skipped"
            ),
            "expected borrow_repay_cycle skip-comment"
        );
    assert!(
            out.contains(
                "-- cover_liquidation_path: trace [init_pool, deposit, borrow, liquidate] spans multiple account types, skipped"
            ),
            "expected liquidation_path skip-comment"
        );
}

#[test]
fn render_emits_liveness_theorems() {
    // Lending: `liveness loan_settles : Loan.Active ~> Loan.Empty via
    // [repay] within 1`. The per-liveness state type resolves from
    // `via_ops[0].on_account` → the theorem binds to `LoanState` +
    // `applyLoanOps`. `find_liveness_path` succeeds, so the
    // universal-implication form closes with no trailing `sorry`.
    let mir = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir);
    assert!(
        out.contains("-- Liveness properties"),
        "expected liveness section header"
    );
    assert!(
        out.contains("def applyLoanOps (s : LoanState)"),
        "expected applyLoanOps helper bound to LoanState"
    );
    assert!(
        out.contains("theorem liveness_loan_settles (s : LoanState)"),
        "expected liveness_loan_settles theorem on LoanState"
    );
    assert!(
        out.contains("ops.length \u{2264} 1"),
        "expected within-step bound of 1"
    );
    assert!(
        out.contains("\u{2200} s', applyLoanOps s signer ops = some s'"),
        "expected auto-discharged universal-implication form"
    );
}

#[test]
fn render_emits_environment_theorems() {
    // Lending declares
    //   environment interest_rate_change { mutates interest_rate :
    //   U64; constraint interest_rate > 0 }
    // and `property pool_solvency` — cross product emits one
    // `pool_solvency_under_interest_rate_change` theorem.
    let mir = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir);
    assert!(
        out.contains("-- Environment"),
        "expected environment section header"
    );
    assert!(
        out.contains("theorem pool_solvency_under_interest_rate_change"),
        "expected pool_solvency_under_interest_rate_change theorem"
    );
    assert!(
        out.contains("new_interest_rate : Nat"),
        "expected new_<field> param of MIR-rendered type"
    );
    assert!(
        out.contains("(h_inv : pool_solvency s)"),
        "expected (h_inv : <prop> s) hypothesis"
    );
    assert!(
        out.contains("{ s with interest_rate := new_interest_rate }"),
        "expected struct-update with mutated field"
    );
}

#[test]
fn render_external_clock_environment_uses_distinct_parameters() {
    let src = r#"spec ExternalClock
type State = { rate : U64 }
property rate_nonnegative : state.rate >= 0 preserved_by all
environment clock_advance {
  external clock.slot : U64
  constraint clock.slot >= old(clock.slot)
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
    let mir = crate::mir::lower(&parsed);
    let out = render(&mir);
    assert!(out.contains("(pre_clock_slot : Nat)"), "{out}");
    assert!(out.contains("(post_clock_slot : Nat)"), "{out}");
    assert!(
        out.contains("h_c0 : post_clock_slot ≥ pre_clock_slot"),
        "{out}"
    );
    assert!(out.contains("rate_nonnegative s := by"), "{out}");
    assert!(!out.contains("{ s with  }"), "{out}");
}

#[test]
fn render_environment_state_read_with_external_binds_s_not_s_prime() {
    // Regression: a constraint that reads STATE inside an environment with an
    // external field must render the state through `s.` (rewritten to
    // `new_<field>` for mutated fields), never the two-state `s'` — the
    // theorem binds `s`, `new_*`, and `pre_/post_*`, but no `s'`.
    let src = r#"spec OracleGuard
type State = { rate : U64, max_rate : U64 }
property rate_bounded : state.rate <= state.max_rate preserved_by all
environment oracle_step {
  external oracle.price : U64
  mutates rate : U64
  constraint state.rate <= oracle.price
}
"#;
    let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
    let mir = crate::mir::lower(&parsed);
    let out = render(&mir);
    // `state.rate` (mutated) → `new_rate`; `oracle.price` → `post_oracle_price`.
    assert!(
        out.contains("h_c0 : new_rate ≤ post_oracle_price"),
        "state read must resolve to new_rate, external to post_oracle_price:\n{out}"
    );
    // No unbound two-state receiver.
    assert!(!out.contains("s'."), "no s' may appear:\n{out}");
    assert!(!out.contains("s'.new_rate"), "{out}");
}

#[test]
fn render_emits_overflow_theorems() {
    // Lending: `deposit` issues a `+=` effect, which MIR lowers to
    // `CheckedAdd` (checked is the default arithmetic mode); the
    // overflow emitter produces `deposit_overflow_safe`.
    let mir = lower_fixture("examples/rust/lending/lending.qedspec");
    let out = render(&mir);
    assert!(
        out.contains("-- Overflow safety obligations"),
        "expected overflow section header"
    );
    assert!(
        out.contains("theorem deposit_overflow_safe"),
        "expected deposit_overflow_safe theorem"
    );
    // Pre-condition asserts `valid_<T>` on each numeric field.
    assert!(out.contains("valid_u64"), "expected valid_u64 in pre/post");
    assert!(
        out.contains("= some s'"),
        "expected `= some s'` hypothesis on transition"
    );
    // Overflow theorems auto-discharge via `overflow_proof_script`;
    // the `:= sorry` form is reserved for the ADT path.
    assert!(
        out.contains("simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX]; omega"),
        "expected overflow proof to discharge the changed-field obligation via `simp; omega`"
    );
    assert!(
        out.contains("unfold depositTransition at h; split at h"),
        "expected overflow proof to unfold the transition and split the guard"
    );
}

#[test]
fn render_pilot_fixtures_no_panic() {
    for fixture in &[
        "examples/rust/escrow/escrow.qedspec",
        "examples/rust/lending/lending.qedspec",
        "examples/rust/multisig/multisig.qedspec",
        "examples/rust/bundled-stdlib-demo/pool.qedspec",
    ] {
        let mir = lower_fixture(fixture);
        let out = render(&mir);
        assert!(out.contains("namespace "), "{}", fixture);
        assert!(out.contains("end "), "{}", fixture);
    }
}

/// Issue #139: a `requires` written with bare state-field names must render
/// the transition guard through the `s` receiver — `if s.active = 0`, not
/// the non-elaborating `if active = 0`. Params stay bare.
#[test]
fn bare_state_field_requires_render_with_receiver() {
    let mir = lower_fixture(
        "crates/qedgen/tests/fixtures/regressions/issue-139-bare-state-refs/generic_vault.qedspec",
    );
    let out = render(&mir);
    assert!(
        out.contains("if s.active = 0 ∧ amount > 0 then"),
        "transition guard must read state through `s`:\n{out}"
    );
    assert!(
        !out.contains("if active = 0"),
        "bare state-field guard leaked into the transition:\n{out}"
    );
}

/// Issues #143/#146 (Lean side): compound effect RHS is canonicalized at
/// the adapter, so state reads arrive `s.`-qualified — previously
/// `residual := fee - cut` rendered `s.fee - cut` (only the string's
/// front got the heuristic prefix; `cut` was unbound and the module
/// failed to elaborate).
#[test]
fn compound_effect_rhs_lean_is_fully_state_qualified() {
    let mir = lower_fixture(
        "crates/qedgen/tests/fixtures/regressions/issues-143-146-kani-arith/vault.qedspec",
    );
    let lean = render(&mir);
    assert!(
        lean.contains("residual := s.fee - s.cut"),
        "every state read in a compound effect RHS must be `s.`-qualified:\n{lean}"
    );
    assert!(
        lean.contains("fee := (bps_mul (amount) (s.rate))"),
        "ref_impl call args must be state-qualified:\n{lean}"
    );
    assert!(
        !lean.contains("- cut") && !lean.contains("(rate)"),
        "bare state-field read leaked into the Lean transition:\n{lean}"
    );
    // #148: `residual := fee - cut` lowers checked in the harness
    // (`checked_sub` → reject on underflow); the Lean transition must
    // carry the matching bound guard so the two models agree on the
    // underflow path.
    assert!(
        lean.contains("s.cut \u{2264} s.fee"),
        "bare-sub effect RHS must push an underflow guard into the \
             transition condition (#148):\n{lean}"
    );
}

/// Shared harness for the #148 bound-guard tests: parse a one-handler
/// spec and render its flat-state transition.
fn transition_for(src: &str) -> String {
    let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let mut out = String::new();
    emit_handler_transition(&mut out, &mir, &mir.handlers[0]);
    out
}

/// #148: bare param subtraction in a `:=` effect RHS gains an underflow
/// bound guard (`rhs ≤ lhs`), aligning the Lean transition with the
/// checked harness lowering (`checked_sub` → reject); a chained
/// left-associative sub gains cumulative guards.
#[test]
fn assign_rhs_bare_sub_gains_bound_guards() {
    let out = transition_for(
        "spec SubGuard\n\
             type State = { x : U64, y : U64 }\n\
             type Error | E\n\
             handler f (a : U64) (b : U64) (c : U64) : State -> State {\n\
             \x20 permissionless\n\
             \x20 effect { x := a - b\n\
             \x20          y := a - b - c }\n\
             }\n",
    );
    assert!(
        out.contains("if b \u{2264} a \u{2227} c \u{2264} a - b then"),
        "expected cumulative underflow guards `b ≤ a ∧ c ≤ a - b`:\n{out}"
    );
    assert!(
        out.contains("x := a - b") && out.contains("y := a - b - c"),
        "effect body must keep the plain Nat subtraction:\n{out}"
    );
}

/// #148: a sub under an add (`cap - used + bonus`) keeps its per-node
/// underflow guard AND — because the RHS contains a growth op on a
/// bounded target — the final-value MAX bound.
#[test]
fn assign_rhs_sub_under_add_gains_underflow_and_max_guards() {
    let out = transition_for(
        "spec MixGuard\n\
             type State = { cap : U64, used : U64, total : U64 }\n\
             type Error | E\n\
             handler f (bonus : U64) : State -> State {\n\
             \x20 permissionless\n\
             \x20 effect { total := cap - used + bonus }\n\
             }\n",
    );
    assert!(
        out.contains("s.used \u{2264} s.cap"),
        "sub node under an add must keep its underflow guard:\n{out}"
    );
    assert!(
        out.contains("s.cap - s.used + bonus \u{2264} 18446744073709551615"),
        "growth RHS on a bounded target must carry the final-value MAX \
             bound:\n{out}"
    );
}

/// #148 (`/`/`%` totalization): a non-literal divisor gains a `≠ 0`
/// guard (checked_div/checked_rem reject; Lean totalizes to 0/x); a
/// nonzero literal divisor does not.
#[test]
fn assign_rhs_division_gains_nonzero_divisor_guard() {
    let out = transition_for(
        "spec DivGuard\n\
             type State = { q : U64, r : U64, h : U64 }\n\
             type Error | E\n\
             handler f (a : U64) (b : U64) : State -> State {\n\
             \x20 permissionless\n\
             \x20 effect { q := a / b\n\
             \x20          r := a % b\n\
             \x20          h := a / 2 }\n\
             }\n",
    );
    assert!(
        out.contains("b \u{2260} 0"),
        "non-literal divisor must gain a `≠ 0` guard:\n{out}"
    );
    assert_eq!(
        out.matches("\u{2260} 0").count(),
        1,
        "one deduped divisor guard for `b`, none for the literal `2`:\n{out}"
    );
}

/// #148 scope boundaries: (a) arithmetic inside an `if … then … else`
/// RHS is conditionally evaluated in the harness — an unconditional Lean
/// guard would over-constrain the model, so none is emitted; (b) signed
/// (Int-kinded) subtraction doesn't truncate in Lean — no guard.
#[test]
fn assign_rhs_conditional_and_int_sub_stay_unguarded() {
    let out = transition_for(
        "spec CondSkip\n\
             type State = { flag : U8, x : U64, pnl : I128, d : I128 }\n\
             type Error | E\n\
             handler f (a : U64) (b : U64) (delta : I128) : State -> State {\n\
             \x20 permissionless\n\
             \x20 effect { x := if flag == 1 then a - b else 0\n\
             \x20          d := pnl - delta }\n\
             }\n",
    );
    assert!(
        !out.contains("b \u{2264} a"),
        "sub inside a conditional branch must NOT emit an unconditional \
             guard:\n{out}"
    );
    assert!(
        !out.contains("delta \u{2264}"),
        "Int-kinded subtraction must NOT emit an underflow guard:\n{out}"
    );
}

/// #148 interaction with the `field -= delta` shape: the existing
/// top-level guard (`<delta> ≤ s.<field>`) already covers compound
/// deltas; the tree walk adds guards only for arithmetic INSIDE the
/// delta — no duplication of the outer bound.
#[test]
fn checked_sub_compound_delta_gains_interior_guard_only() {
    let out = transition_for(
        "spec DeltaGuard\n\
             type State = { total : U64, fee : U64, cut : U64 }\n\
             type Error | E\n\
             handler f : State -> State {\n\
             \x20 permissionless\n\
             \x20 effect { total -= fee - cut }\n\
             }\n",
    );
    assert!(
        out.contains("s.fee - s.cut \u{2264} s.total"),
        "existing top-level underflow guard must survive for compound \
             deltas:\n{out}"
    );
    assert!(
        out.contains("s.cut \u{2264} s.fee"),
        "sub inside a compound delta must gain its own guard (#148):\n{out}"
    );
    assert_eq!(
        out.matches("s.fee - s.cut \u{2264} s.total").count(),
        1,
        "outer bound must not duplicate:\n{out}"
    );
}

/// #148 in conditional effects: a `match` arm's bare-sub RHS guard gates
/// only that arm (untaken arms must not abort), mirroring the per-arm
/// checked-add bounds.
#[test]
fn branch_arm_assign_sub_guard_gates_only_its_arm() {
    let out = transition_for(
        "spec ArmGuard\n\
             type State = { x : U64, y : U64 }\n\
             type Error | E\n\
             handler f (mode : U8) (a : U64) (b : U64) : State -> State {\n\
             \x20 permissionless\n\
             \x20 effect {\n\
             \x20   match mode {\n\
             \x20     0 => x := a - b,\n\
             \x20     _ => y := 0,\n\
             \x20   }\n\
             \x20 }\n\
             }\n",
    );
    assert!(
        out.contains("| 0 => if b \u{2264} a then some { s with x := a - b } else none"),
        "arm's sub guard must gate only that arm:\n{out}"
    );
    assert!(
        out.contains("| _ => some { s with y := 0 }"),
        "untaken arm must stay unguarded:\n{out}"
    );
}
