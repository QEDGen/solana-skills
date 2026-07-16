# ptoken-transfer fixture

Minimal Pinocchio-shaped `process_transfer` modeled on
`solana-program/token/pinocchio/program/src/processor/shared/transfer.rs`.

## v2.19 success-bar patterns

| Site | Pattern | Probe |
|---|---|---|
| `src/transfer.rs:53` | `load_mut(src.borrow_mut_data_unchecked())` | `unchecked_account_load` |
| `src/transfer.rs:68` | `load_mut_unchecked(dst.borrow_mut_data_unchecked())` + SAFETY claim | `unchecked_account_load` + `stale_safety_comment` |
| `src/transfer.rs:75` | `dst.set_amount(dst.amount() + amount)` | `unchecked_amount_arith` |
| `src/transfer.rs:84` | `*source_lamports -= amount` | `unchecked_lamport_arith` |

## Running

```bash
qedgen probe --program crates/qedgen/tests/fixtures/pinocchio-fixtures/ptoken-transfer
```

Expected: ≥17 findings, runtime detected as `pinocchio`, at least one
`stale_safety_comment` paired with an `unchecked_account_load`.

The full diff is at `expected_findings.json`. CI gates the audit shape
against this golden.

## Attribution

Pattern shapes inspired by Solana Foundation's
`solana-program/token/pinocchio` (a.k.a. p-token). This is **synthesized**
fixture source — not a vendored checkout — scoped down to the three
patterns covered by the maintained
`references/probes/pinocchio/` taxonomy.
