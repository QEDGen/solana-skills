//! End-to-end manifest tests over real specs (#332): the four known
//! silent-skip classes surface as distinct `unsupported` reasons, and the
//! reconcile safety net stays quiet on bundled examples (a `failed` entry
//! on a bundled single-account example means the expected inventory and
//! an emitter disagree — OUR bug, not the spec's).

use std::path::Path;

use super::*;
use crate::check::ParsedSpec;
use crate::mir::Mir;

fn lower_fixture(rel_path: &str) -> (Mir, ParsedSpec) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/qedgen/ under repo root");
    let spec_path = root.join(rel_path);
    let parsed = crate::check::parse_spec_file(&spec_path).expect("fixture parses");
    let mir = crate::mir::lower(&parsed);
    (mir, parsed)
}

fn lower_inline(src: &str) -> (Mir, ParsedSpec) {
    let parsed = crate::chumsky_adapter::parse_str(src).expect("inline fixture parses");
    let mir = crate::mir::lower(&parsed);
    (mir, parsed)
}

fn entries_with_reason(
    entries: &[ObligationEntry],
    reason: UnsupportedReason,
) -> Vec<&ObligationEntry> {
    entries
        .iter()
        .filter(|e| e.status == ObligationStatus::Unsupported { reason })
        .collect()
}

fn failed_entries(entries: &[ObligationEntry]) -> Vec<&ObligationEntry> {
    entries
        .iter()
        .filter(|e| matches!(e.status, ObligationStatus::Failed { .. }))
        .collect()
}

/// #324 — multi-account Kani drops covers / liveness / environment. The
/// bundled lending example (Pool + Loan accounts, two covers, one
/// liveness, one environment) must surface every file-level obligation as
/// `unsupported(kani_multi_account_file_level)`, never absent.
#[test]
fn kani_multi_account_file_level_obligations_surface() {
    let (mir, parsed) = lower_fixture("examples/rust/lending/lending.qedspec");
    assert!(parsed.account_types.len() > 1, "lending is multi-account");
    let entries = reconciled(
        ObligationBackend::Kani,
        &mir,
        &parsed,
        crate::kani_mir::collect_obligations(&mir, &parsed),
    );

    let dropped = entries_with_reason(&entries, UnsupportedReason::KaniMultiAccountFileLevel);
    let expected_file_level = parsed.covers.iter().map(|c| c.traces.len()).sum::<usize>()
        + parsed.liveness_props.len()
        + parsed.environments.len()
            * parsed
                .properties
                .iter()
                .filter(|p| p.expression.is_some())
                .count();
    assert!(
        expected_file_level > 0,
        "lending declares file-level obligations"
    );
    assert_eq!(
        dropped.len(),
        expected_file_level,
        "every file-level obligation must be reported: {:#?}",
        entries
    );
}

/// #326 — `pragma state_repr = adt` is invisible to Kani. The bundled
/// cross-program-vault example must carry the representation-gap entry.
#[test]
fn kani_adt_state_repr_gap_is_reported() {
    let (mir, parsed) = lower_fixture("examples/rust/cross-program-vault/vault.qedspec");
    assert!(
        parsed.state_repr_is_adt(),
        "fixture declares adt state_repr"
    );
    let entries = crate::kani_mir::collect_obligations(&mir, &parsed);
    let gap = entries_with_reason(&entries, UnsupportedReason::KaniAdtStateRepr);
    assert_eq!(gap.len(), 1);
    assert_eq!(gap[0].kind, ObligationKind::StateRepresentation);
}

/// #328 — Lean drops requires clauses naming a handler account's pubkey.
/// The bundled escrow example (`requires initializer_ta.pubkey == …`)
/// must report each dropped clause instead of weakening silently.
#[test]
fn lean_handler_account_pubkey_drops_are_reported() {
    let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
    let entries = reconciled(
        ObligationBackend::Lean,
        &mir,
        &parsed,
        crate::lean_gen_mir::collect_obligations(&mir),
    );
    let dropped = entries_with_reason(&entries, UnsupportedReason::LeanHandlerAccountPubkey);
    assert!(
        !dropped.is_empty(),
        "escrow's pubkey-authorization clauses must surface: {:#?}",
        entries
    );
}

/// #331 — multi-account proptest cannot model spec-global ghosts. A
/// ghost-reading property on a two-account spec must be reported per
/// preserved_by handler, not silently filtered.
#[test]
fn proptest_multi_account_ghost_is_reported() {
    let (mir, parsed) = lower_inline(
        r#"
spec GhostMulti

type Pool
  | Uninitialized
  | Active of { total : U64 }

type Loan
  | Empty
  | Open of { amount : U64 }

type Error
  | InvalidAmount
  | MathOverflow

ghost lifetime_total : U64 {
  init { 0 }
  on deposit { lifetime_total += amount }
}

handler init_pool : Pool.Uninitialized -> Pool.Active {
  auth authority
  accounts {
    authority : signer, writable
  }
  effect {
    total := 0
  }
}

handler deposit (amount : U64) : Pool.Active -> Pool.Active {
  permissionless
  accounts {
    depositor : signer
  }
  requires amount > 0 else InvalidAmount
  effect {
    total += amount
  }
}

handler open_loan (amount : U64) : Loan.Empty -> Loan.Open {
  permissionless
  accounts {
    borrower : signer
  }
  requires amount > 0 else InvalidAmount
  effect {
    amount := amount
  }
}

property ghost_tracks_total :
  state.lifetime_total >= state.total
  preserved_by [deposit]
"#,
    );
    assert!(parsed.account_types.len() > 1, "fixture is multi-account");
    assert!(!parsed.ghosts.is_empty(), "fixture declares a ghost");
    let entries = reconciled(
        ObligationBackend::Proptest,
        &mir,
        &parsed,
        crate::proptest_gen_mir::collect_obligations(&mir, &parsed),
    );
    let ghost_dropped = entries_with_reason(&entries, UnsupportedReason::ProptestMultiAccountGhost);
    assert!(
        !ghost_dropped.is_empty(),
        "ghost property must be reported unsupported: {:#?}",
        entries
    );
    assert!(ghost_dropped
        .iter()
        .all(|e| e.kind == ObligationKind::PropertyPreservation));
}

/// Inventory safety net: on bundled single-account examples, every
/// expected obligation must be reported by every backend — zero `failed`
/// entries. A failure here means an emitter and the expected inventory
/// disagree about an identity (key mismatch or a NEW silent skip).
#[test]
fn bundled_single_account_examples_have_no_failed_entries() {
    for example in [
        "examples/rust/escrow/escrow.qedspec",
        "examples/rust/multisig/multisig.qedspec",
        "examples/rust/bundled-stdlib-demo/pool.qedspec",
        "examples/rust/cross-program-vault/vault.qedspec",
    ] {
        let (mir, parsed) = lower_fixture(example);
        let entries = collect_all(&mir, &parsed);
        assert!(!entries.is_empty(), "{}: expected obligations", example);
        let failed = failed_entries(&entries);
        assert!(
            failed.is_empty(),
            "{}: reconcile mismatch (inventory vs emitters):\n{:#?}",
            example,
            failed
        );
    }
}

/// Multi-account examples may carry `unsupported` entries (the known
/// #324/#331 gaps) but must not carry `failed` ones either — cross-module
/// drops map to `multi_account_cross_account_obligation` by design.
#[test]
fn bundled_multi_account_example_has_no_failed_entries() {
    let (mir, parsed) = lower_fixture("examples/rust/lending/lending.qedspec");
    let entries = collect_all(&mir, &parsed);
    let failed = failed_entries(&entries);
    assert!(
        failed.is_empty(),
        "lending: reconcile mismatch:\n{:#?}",
        failed
    );
}

/// `verify --strict` semantics: bundled examples with known capability
/// gaps must gate; a gap-free spec must not.
#[test]
fn strict_gate_fires_on_known_gaps_only() {
    let (mir, parsed) = lower_fixture("examples/rust/lending/lending.qedspec");
    let counts = StatusCounts::of(&collect_all(&mir, &parsed));
    assert!(counts.gates_strict(), "lending has known #324 gaps");

    let (mir, parsed) = lower_inline(
        r#"
spec Tiny

type State
  | Uninitialized
  | Active of { total : U64 }

type Error
  | InvalidAmount
  | MathOverflow

handler init : State.Uninitialized -> State.Active {
  auth authority
  accounts {
    authority : signer, writable
  }
  effect {
    total := 0
  }
}

handler add (amount : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    caller : signer
  }
  requires amount > 0 else InvalidAmount
  effect {
    total += amount
  }
}

property total_bounded :
  state.total >= 0
  preserved_by [add]
"#,
    );
    let entries = collect_all(&mir, &parsed);
    let counts = StatusCounts::of(&entries);
    assert!(
        !counts.gates_strict(),
        "gap-free spec must pass strict: {:#?}",
        entries
            .iter()
            .filter(|e| !matches!(e.status, ObligationStatus::Emitted { .. }))
            .collect::<Vec<_>>()
    );
}
