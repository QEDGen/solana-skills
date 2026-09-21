# Pinocchio probes (v2.19)

The audit subagent reads these probes when `qedgen probe --program
<path>` detects a Pinocchio runtime (Cargo.toml has a `pinocchio`
dep, or `--runtime pinocchio` is passed).

Each probe markdown follows the same shape:

- **Pattern** — the source-level shape `pinocchio_probe.rs` detects.
- **Why it matters** — what the framework would do that the author
  is now responsible for.
- **What the agent should check** — CF / dataflow questions to apply
  via rust-analyzer.
- **What counts as a finding** — severity guidance.
- **Mollusk reproducer** — SVM-mediated repro template.
- **Miri reproducer** — direct-call repro template under
  `cargo +nightly miri test`.

## Catalog

| Probe | Site kinds | Severity |
|---|---|---|
| [unchecked_account_load](unchecked_account_load.md) | `BorrowUnchecked`, `CustomLoadCall` | High |
| [unchecked_amount_arith](unchecked_amount_arith.md) | `SetAmountArith` | High |
| [unchecked_lamport_arith](unchecked_lamport_arith.md) | `SetLamportsArith` | High |
| [account_type_confusion](account_type_confusion.md) | `BytemuckCall`, `RawPtrCastFromAccount` | Medium |
| [mutable_borrow_aliasing](mutable_borrow_aliasing.md) | `BorrowUnchecked` pairs | High |
| [position_based_account_without_type_tag](position_based_account_without_type_tag.md) | `IndexedAccountAccess` | Medium |
| [offset_overrun](offset_overrun.md) | `IndexedDataSlice`, `TryIntoUnwrapOnSlice` | Medium |
| [missing_pda_verification](missing_pda_verification.md) | derived (no direct site) | High |
| [stale_safety_comment](stale_safety_comment.md) | `SafetyComment` attached to any unsafe site | Critical |

## Composition

`stale_safety_comment` composes with every other probe. When a high-
or medium-severity probe fires on a site that also carries a `// SAFETY:`
block, the agent should run the stale-safety probe in parallel — most
of the v2.19 success bar (transfer.rs:168 distinctness, transfer.rs:172
amount-bound) lives in that overlap.

## Reproducer harness

All Miri repros import from `crates/qedgen/tests/fixtures/pinocchio-fixtures/_harness/`:

- `account.rs` — synthesizes Pinocchio `AccountInfo` from raw bytes.
- `adversarial.rs` — input-negation primitives keyed to SAFETY-comment
  strategies.
- `invariants.rs` — conservation / distinctness / write-ownership asserts.
- `state.rs` — `capture_global_state` for pre-post diffs.

## Authoring conventions

- Substitutions in templates use `${UPPER_CASE}`. The site enumerator
  flattens `PinocchioSite.extra` into substitution keys
  (uppercased) before invoking the template.
- Test names are `probe_${ID}_<short-desc>` where `${ID}` is the
  16-char hex hash from the site catalogue. Stable across runs so
  suppression files key off it.
- Severity assignments live in `findings_from_catalogue` in
  `crates/qedgen/src/pinocchio_probe.rs`; markdown text is descriptive
  but the canonical mapping is in code.
