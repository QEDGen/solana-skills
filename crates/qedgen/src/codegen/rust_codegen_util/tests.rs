//! `rust_codegen_util` unit tests — moved verbatim from the pre-split
//! `rust_codegen_util.rs` test module.

use super::*;
use crate::chumsky_adapter::parse_str;

/// #66 adaptor invariant — for every handler, the effect triples
/// projected from the lowered MIR body must equal
/// `ParsedHandler.effects` exactly (order + content). This is what
/// makes the `Stmt`-driven Kani/proptest emission byte-identical to
/// the former `op.effects` iteration. Checked over a synthetic spec
/// covering all seven op kinds plus the non-effect statement shapes.
#[test]
fn stmt_effect_triples_round_trip_parsed_effects() {
    let src = r#"spec RoundTrip
state {
  pool : U64,
  fees : U64,
  spent : U64,
  cap : U64,
  owner : Pubkey,
}

type Error | Overflow | Unauthorized
handler churn (amount : U64) (who : Pubkey) {
  requires amount > 0 else Unauthorized
  effect {
    pool += amount,
    fees +=! amount,
    spent +=? amount,
    cap -= amount,
    pool -=! amount,
    fees -=? amount,
    owner := who,
  }
}
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    for op in &spec.handlers {
        let body = mir.handler_block(&op.name).expect("mir body");
        let got: Vec<(String, String, String)> = block_effect_triples(body)
            .into_iter()
            .map(|(f, k, v)| (effect_path_source(f), k.to_string(), mir_expr_rust(v)))
            .collect();
        let want: Vec<(String, String, String)> = op
            .effects
            .iter()
            .map(|e| (e.field.clone(), e.op.clone(), e.value.clone()))
            .collect();
        assert_eq!(
            got, want,
            "MIR effect triples diverge from op.effects for `{}`",
            op.name
        );
    }
    // Sanity: the seven kinds all round-tripped (not vacuous).
    let kinds: Vec<&str> = block_effect_triples(mir.handler_block("churn").expect("mir body"))
        .iter()
        .map(|(_, k, _)| *k)
        .collect();
    assert_eq!(
        kinds,
        vec!["add", "add_sat", "add_wrap", "sub", "sub_sat", "sub_wrap", "set"]
    );
}

#[test]
fn effect_lhs_stays_structured_and_uses_the_canonical_rust_renderer() {
    let src = r#"spec TypedLhs
const CAP = 4
type State | Active of { voted : Map[CAP] U8 }
handler vote (member_index : U8) : State.Active -> State.Active {
  effect { Active.voted[member_index] := 1 }
}
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let body = mir.handler_block("vote").expect("MIR body");
    let (path, _, _) = stmt_effect_triple(&body.stmts[0]).expect("effect");

    assert!(path.tree.is_some(), "adapter effects retain a typed LHS");
    assert_eq!(
        render_effect_target(path, &spec, "s"),
        "s.voted[(member_index) as usize]"
    );
}

/// Phase-5 #42 — the transition emitter renders `effect { match … }`
/// from the lowered `Stmt::Branch` (per-arm semantics, wildcard arm
/// as `_`), pinned against the exact pre-Phase-5 `effect_branches`
/// output shape so the Kani/proptest harness text is unchanged.
#[test]
fn transition_fn_renders_branch_as_match() {
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
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = &spec.handlers[0];
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    let expected = "    match fee_type {\n\
                        \x20       0 => {\n\
                        \x20           s.a = s.a.saturating_add(amount);\n\
                        \x20       }\n\
                        \x20       1 => {\n\
                        \x20           match s.b.checked_add(amount) {\n\
                        \x20               Some(__v) => s.b = __v,\n\
                        \x20               None => return false,\n\
                        \x20           }\n\
                        \x20       }\n\
                        \x20       _ => {\n\
                        \x20           s.d = 0;\n\
                        \x20       }\n\
                        \x20   }\n";
    assert!(
        out.contains(expected),
        "transition must render the Branch as a Rust match:\n{out}"
    );
    // The flat union must NOT leak alongside the match.
    assert_eq!(
        out.matches("s.a = s.a.saturating_add(amount);").count(),
        1,
        "arm effect emitted exactly once:\n{out}"
    );
}

/// Same invariant over every bundled example spec — the real-world
/// shapes (indexed records, variant prefixes, match-arm expansion,
/// transfers/CPI/emit interleaving).
#[test]
fn stmt_effect_triples_round_trip_bundled_examples() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repo root")
        .join("examples/rust");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&examples).expect("examples/rust") {
        let dir = entry.expect("dir entry").path();
        if !dir.is_dir() {
            continue;
        }
        let Some(spec_path) = std::fs::read_dir(&dir)
            .expect("example dir")
            .filter_map(|e| Some(e.ok()?.path()))
            .find(|p| p.extension().is_some_and(|x| x == "qedspec"))
        else {
            continue;
        };
        let spec = match crate::check::parse_spec_file(&spec_path) {
            Ok(s) => s,
            // Brownfield onboarding fixtures may be intentionally
            // partial — the invariant only applies to parseable specs.
            Err(_) => continue,
        };
        let mir = crate::mir::lower(&spec);
        for op in &spec.handlers {
            let body = mir
                .handler_block(&op.name)
                .unwrap_or_else(|| panic!("MIR missing `{}` in {:?}", op.name, spec_path));
            let got: Vec<(String, String, String)> = block_effect_triples(body)
                .into_iter()
                .map(|(f, k, v)| (effect_path_source(f), k.to_string(), mir_expr_rust(v)))
                .collect();
            let want: Vec<(String, String, String)> = op
                .effects
                .iter()
                .map(|e| (e.field.clone(), e.op.clone(), e.value.clone()))
                .collect();
            assert_eq!(
                got, want,
                "MIR effect triples diverge from op.effects for `{}` in {:?}",
                op.name, spec_path
            );
            checked += 1;
        }
    }
    assert!(
        checked > 10,
        "expected to check many handlers, got {checked}"
    );
}

#[test]
fn effect_target_base_strips_subscripts_and_dots() {
    assert_eq!(effect_target_base("plain"), "plain");
    assert_eq!(effect_target_base("accounts[i].active"), "accounts");
    assert_eq!(effect_target_base("s.foo"), "s");
    assert_eq!(effect_target_base("map[0]"), "map");
    assert_eq!(effect_target_base("  padded  "), "padded");
}

#[test]
fn emit_transition_fn_default_add_emits_checked() {
    // `pool += amount` defaults to checked semantics — overflow
    // short-circuits via `return false`, matching deployed
    // `checked_add(..).ok_or(err)?`.
    let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool += amount } }
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = &spec.handlers[0];
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        out.contains("checked_add(amount)"),
        "default `+=` should emit checked_add: {out}"
    );
    assert!(
        out.contains("None => return false"),
        "checked should short-circuit on None: {out}"
    );
    assert!(
        !out.contains("wrapping_add") && !out.contains("saturating_add"),
        "default `+=` should NOT emit wrapping/saturating: {out}"
    );
}

#[test]
fn emit_transition_fn_saturating_add_emits_saturating() {
    let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool +=! amount } }
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = &spec.handlers[0];
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        out.contains("saturating_add(amount)"),
        "`+=!` should emit saturating_add: {out}"
    );
    assert!(
        !out.contains("checked_add") && !out.contains("wrapping_add"),
        "`+=!` should NOT emit checked/wrapping: {out}"
    );
}

#[test]
fn emit_transition_fn_wrapping_add_emits_wrapping() {
    let src = r#"spec T
state { pool : U64 }
handler buy (amount : U64) { effect { pool +=? amount } }
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = &spec.handlers[0];
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        out.contains("wrapping_add(amount)"),
        "`+=?` should emit wrapping_add: {out}"
    );
    assert!(
        !out.contains("checked_add") && !out.contains("saturating_add"),
        "`+=?` should NOT emit checked/saturating: {out}"
    );
}

#[test]
fn emit_transition_fn_sub_three_tiers() {
    // Mirror: `-=` / `-=!` / `-=?` emit checked / saturating / wrapping.
    for (op_str, expected) in &[
        ("-=", "checked_sub(amount)"),
        ("-=!", "saturating_sub(amount)"),
        ("-=?", "wrapping_sub(amount)"),
    ] {
        let src = format!(
            "spec T\nstate {{ pool : U64 }}\nhandler buy (amount : U64) {{ effect {{ pool {op_str} amount }} }}\n"
        );
        let spec = parse_str(&src).expect("parse");
        let mir = crate::mir::lower(&spec);
        let op = &spec.handlers[0];
        let mut out = String::new();
        emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
            crate::codegen_shared::map_type(t, &spec)
        })
        .expect("emit");
        assert!(
            out.contains(expected),
            "`{op_str}` should emit {expected}:\n{out}"
        );
    }
}

#[test]
fn emit_transition_fn_lifecycle_emits_status_guard_and_assignment() {
    // Spec with a multi-state lifecycle. `transition` declares `Open ->
    // Closed`, so the generated transition fn must (1) reject when the
    // current status isn't `Open`, and (2) write `Status::Closed` on
    // success. Without these, lifecycle-only handlers compile to
    // `fn h() -> bool { true }` and every cover/liveness harness
    // against them passes vacuously.
    let src = r#"spec T
type State
  | Open of { x : U64 }
  | Closed
handler close : State.Open -> State.Closed { effect { x := 0 } }
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = &spec.handlers[0];
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        out.contains("if s.status != Status::Open"),
        "lifecycle handler must reject when status mismatches pre_status:\n{out}"
    );
    assert!(
        out.contains("s.status = Status::Closed;"),
        "lifecycle handler must drive post_status assignment:\n{out}"
    );
}

#[test]
fn emit_transition_fn_no_lifecycle_skips_status_lines() {
    // Spec without a multi-state lifecycle (single State variant or
    // flat record). emit_transition_fn must NOT emit any status guard
    // or assignment — there's no Status enum to reference.
    let src = r#"spec T
state { balance : U64 }
handler deposit (amount : U64) { effect { balance += amount } }
"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = &spec.handlers[0];
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        !out.contains("Status::"),
        "lifecycle-free spec must not reference Status:\n{out}"
    );
}

#[test]
fn emit_state_struct_appends_status_when_lifecycle_present() {
    let src = r#"spec T
type State
  | Open of { x : U64 }
  | Closed
handler close : State.Open -> State.Closed { effect { x := 0 } }
"#;
    let spec = parse_str(src).expect("parse");
    let mutable = field_refs(&spec.state_fields);
    let mut out = String::new();
    emit_state_struct_with_lifecycle(
        &mut out,
        &mutable,
        "Clone, Copy",
        |t| Ok(t.to_string()),
        has_lifecycle(&spec),
    )
    .expect("emit");
    assert!(
        out.contains("status: Status,"),
        "lifecycle spec must inject `status: Status` field:\n{out}"
    );
}

#[test]
fn kani_pubkey_rewrite_handles_account_and_state_fields() {
    let src = r#"spec T
type State | Active of { admin_key : Pubkey }
type Error | Unauthorized
handler set_admin : State.Active -> State.Active {
  accounts { admin : signer }
  requires admin.pubkey == state.admin_key else Unauthorized
  effect { admin_key := admin.pubkey }
}
"#;
    let spec = parse_str(src).expect("parse");
    let op = &spec.handlers[0];
    let expr = "(accounts.admin.pubkey == s.admin_key) && (amount > 0)";
    let rewritten = rewrite_kani_pubkey_comparisons(expr, op, &spec);
    assert_eq!(
        rewritten,
        "(pubkey_eq(&accounts.admin.pubkey, &s.admin_key)) && (amount > 0)"
    );
}

#[test]
fn kani_pubkey_rewrite_handles_indexed_pubkey_arrays() {
    let src = r#"spec T
const MAX_MEMBERS = 32
type State | Active of { members : Map[MAX_MEMBERS] Pubkey }
handler approve (member_index : U8) (approver : Pubkey) : State.Active -> State.Active {
  requires state.members[member_index] == approver
}
"#;
    let spec = parse_str(src).expect("parse");
    let op = &spec.handlers[0];
    let expr = "(s.members[(member_index) as usize] == approver) && (member_index < 32)";
    let rewritten = rewrite_kani_pubkey_comparisons(expr, op, &spec);
    assert_eq!(
        rewritten,
        "(pubkey_eq(&s.members[(member_index) as usize], &approver)) && (member_index < 32)"
    );
}

#[test]
fn split_top_level_and_splits_only_balanced_top_level_terms() {
    assert_eq!(
        split_top_level_and("amount > 0 && fee_bps <= 100 && min_out > 0"),
        vec!["amount > 0", "fee_bps <= 100", "min_out > 0"]
    );
    assert_eq!(
        split_top_level_and("(amount > 0 && fee_bps <= 100) && min_out > 0"),
        vec!["(amount > 0 && fee_bps <= 100)", "min_out > 0"]
    );
    assert_eq!(
        split_top_level_and(
            "is_allowed(mints[(lane) as usize] == mint && lane < 32) && amount > 0"
        ),
        vec![
            "is_allowed(mints[(lane) as usize] == mint && lane < 32)",
            "amount > 0"
        ]
    );
}

#[test]
fn collect_guard_terms_splits_requires_without_nested_or_splits() {
    let src = r#"spec T
type State | Active of { admin_key : Pubkey, allowed : Bool }
type Error | Unauthorized | InvalidAmount
handler swap (amount : U64) (min_out : U64) : State.Active -> State.Active {
  accounts { admin : signer }
  requires admin.pubkey == state.admin_key else Unauthorized
  requires amount >= min_out and min_out > 0 else InvalidAmount
}
"#;
    let spec = parse_str(src).expect("parse");
    let op = &spec.handlers[0];
    let exprs = collect_guard_terms_with_account_env(op, false, Some("accounts"));
    assert_eq!(
        exprs,
        vec![
            "accounts.admin.pubkey == s.admin_key",
            "amount >= min_out",
            "min_out > 0",
        ]
    );
}

#[test]
fn rewrite_kani_bps_mul_div_uses_solver_friendly_helper() {
    assert_eq!(
        rewrite_kani_bps_mul_div(
            "fee_output_normalized >= (fee_input_normalized * retained_value_bps) / 10000"
        ),
        "fee_output_normalized >= mul_bps_floor_u128(fee_input_normalized, retained_value_bps)"
    );
    assert_eq!(
        rewrite_kani_bps_mul_div("amount_in * fee_bps / 10000 <= amount_in"),
        "mul_bps_floor_u128(amount_in, fee_bps) <= amount_in"
    );
    assert_eq!(
        rewrite_kani_bps_mul_div("(a + b) * fee_bps / 10000 <= a"),
        "(a + b) * fee_bps / 10000 <= a"
    );
}

#[test]
fn rewrite_kani_checked_add_equality_avoids_overflow_checks() {
    assert_eq!(
        rewrite_kani_guard_arithmetic("max_fee_bps + retained_value_bps == 10000"),
        "max_fee_bps.checked_add(retained_value_bps) == Some(10000)"
    );
    assert_eq!(
        rewrite_kani_guard_arithmetic(
            "fee_output_normalized >= (fee_input_normalized * retained_value_bps) / 10000 && max_fee_bps + retained_value_bps == 10000"
        ),
        "fee_output_normalized >= mul_bps_floor_u128(fee_input_normalized, retained_value_bps) && max_fee_bps.checked_add(retained_value_bps) == Some(10000)"
    );
}

#[test]
fn negate_simple_top_level_comparison_flips_only_outer_operator() {
    assert_eq!(
        negate_simple_top_level_comparison(
            "fee_output_normalized >= mul_bps_floor_u128(fee_input_normalized, retained_value_bps)"
        ),
        Some(
            "fee_output_normalized < mul_bps_floor_u128(fee_input_normalized, retained_value_bps)"
                .to_string()
        )
    );
    assert_eq!(
        negate_simple_top_level_comparison("(mul_bps_floor_u128(amount_in, fee_bps) <= amount_in)"),
        Some("mul_bps_floor_u128(amount_in, fee_bps) > amount_in".to_string())
    );
    assert_eq!(
        negate_simple_top_level_comparison(
            "pubkey_eq(&accounts.input_mint.pubkey, &s.allowed_mint_0) || amount > 0"
        ),
        None
    );
}

#[test]
fn check_effect_targets_accepts_declared_fields() {
    let src = r#"spec T
state { balance : U64 }
handler deposit (amount : U64) {
  effect { balance += amount }
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(check_effect_targets(&spec).is_ok());
}

#[test]
fn check_effect_targets_errors_on_undeclared_target() {
    // Effect writes `phantom` but the state declares only `balance`;
    // the error must name the handler and the bad field.
    let src = r#"spec T
state { balance : U64 }
handler bogus (amount : U64) {
  effect { phantom := amount }
}"#;
    let spec = parse_str(src).expect("parse");
    let err = check_effect_targets(&spec).unwrap_err().to_string();
    assert!(err.contains("bogus"), "should name handler: {err}");
    assert!(err.contains("phantom"), "should name field: {err}");
}

#[test]
fn check_effect_targets_rejects_duplicate_target_in_one_block() {
    // `b += 1; b += 2` diverges under parallel semantics (Lean keeps the
    // last write → 2; sequential Rust accumulates → 3). Codegen must
    // refuse rather than emit two contradictory artifacts.
    let src = r#"spec T
state { b : U64 }
handler bump {
  effect { b += 1
           b += 2 }
}"#;
    let spec = parse_str(src).expect("parse");
    let err = check_effect_targets(&spec).unwrap_err().to_string();
    assert!(err.contains("bump"), "should name handler: {err}");
    assert!(
        err.contains("more than once"),
        "should explain the dup: {err}"
    );
}

#[test]
fn check_effect_targets_allows_same_field_in_distinct_match_arms() {
    // Writes to `b` in mutually-exclusive arms are NOT duplicates.
    let src = r#"spec T
state { b : U64 }
handler pick (mode : U8) {
  effect {
    match mode {
      0 => b += 1,
      _ => b += 2,
    }
  }
}"#;
    let spec = parse_str(src).expect("parse");
    assert!(
        check_effect_targets(&spec).is_ok(),
        "same field in different arms must not be flagged as a duplicate"
    );
}

#[test]
fn duplicate_effect_targets_reports_normalized_paths_once() {
    let src = r#"spec T
state { a : U64, b : U64 }
handler h {
  effect { a += 1
           b := 5
           a += 2 }
}"#;
    let spec = parse_str(src).expect("parse");
    let h = spec.handlers.first().unwrap();
    let dups = duplicate_effect_targets(&h.effects, &spec);
    assert_eq!(dups, vec!["a".to_string()], "only `a` is written twice");
}

/// v2.44 parallel effect semantics — transition fns snapshot fields the
/// block both writes and reads, and RHS reads route through the
/// snapshot. Pre-v2.44 `effect { balance += amount, last_seen := balance }`
/// emitted `s.last_seen = s.balance;` AFTER the add — the sequential
/// (post-state) read — while the Lean model and the Kani conformance
/// assertion both mean pre-state: a broken program could pass every
/// green artifact.
#[test]
fn transition_fn_snapshots_read_after_write_fields() {
    let src = r#"spec Raw
state {
  balance : U64,
  last_seen : U64,
}
handler deposit (amount : U64) {
  requires amount > 0
  effect { balance += amount
           last_seen := balance }
}"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = spec.handlers.first().expect("handler");
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        out.contains("let pre_balance = s.balance;"),
        "snapshot bound before effects:\n{out}"
    );
    assert!(
        out.contains("s.last_seen = pre_balance;"),
        "read-after-write RHS observes pre-state:\n{out}"
    );
    // The checked-add self-read stays on `s.` — single write per field,
    // so its own read is still pre-state at that statement.
    assert!(
        out.contains("s.balance.checked_add(amount)"),
        "self-read of the written field unchanged:\n{out}"
    );
}

/// A field that is read but NOT written (or written but not read) needs
/// no snapshot — the emitted transition stays byte-identical to the
/// pre-v2.44 form for such specs.
#[test]
fn transition_fn_skips_snapshot_without_read_after_write() {
    let src = r#"spec Plain
state {
  balance : U64,
  cap : U64,
}
handler deposit (amount : U64) {
  requires amount <= cap
  effect { balance += amount }
}"#;
    let spec = parse_str(src).expect("parse");
    let mir = crate::mir::lower(&spec);
    let op = spec.handlers.first().expect("handler");
    let mut out = String::new();
    emit_transition_fn(&mut out, &mir, op, &spec, false, |t| {
        crate::codegen_shared::map_type(t, &spec)
    })
    .expect("emit");
    assert!(
        !out.contains("let pre_"),
        "no snapshot for write-only / read-only fields:\n{out}"
    );
}

/// `substitute_pre_reads` token boundaries: only exact `<receiver>.<field>`
/// reads rewrite — longer field names, other receivers, and the write
/// position must survive.
#[test]
fn substitute_pre_reads_respects_token_boundaries() {
    let fields = vec!["balance".to_string()];
    assert_eq!(
        substitute_pre_reads("s.balance + s.balance_total", "s", &fields),
        "pre_balance + s.balance_total"
    );
    assert_eq!(
        substitute_pre_reads("accounts.balance + s.balance", "s", &fields),
        "accounts.balance + pre_balance"
    );
    assert_eq!(
        substitute_pre_reads("s.balance.checked_add(x)", "s", &fields),
        "pre_balance.checked_add(x)"
    );
}
