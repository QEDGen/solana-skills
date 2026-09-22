# Pinocchio probe: stale_safety_comment

## Pattern

```rust
// SAFETY: the account is guaranteed to be initialized and different
// than `source_account_info`; it was also already validated to be a
// token account.
let destination_account = unsafe {
    load_mut_unchecked::<Account>(destination_account_info.borrow_mut_data_unchecked())?
};
```

A `// SAFETY:` block enumerates preconditions; the agent's CF read
cannot find one or more of them enforced on every reachable path.

## Why it matters

This is the **highest-leverage Pinocchio probe**. Authors have already
done our work for us — every SAFETY clause is a free test specification.
We negate each clause, drive the negated input through the handler,
and observe whether the program rejects (claim is upheld) or accepts
(claim is stale and state corrupts silently).

Anchor users get the framework reading their `#[account(...)]`
constraints back; Pinocchio users get nothing. We are that nothing.

## What the agent should check

For each `// SAFETY:` clause:

1. **Negate the clause**: pick a negation strategy from the table below.
2. **Build the adversarial input**: use the
   `crates/qedgen/tests/fixtures/pinocchio-fixtures/_harness/adversarial.rs` builder
   matching the strategy.
3. **Drive the handler**: call the impl with the adversarial input
   (Miri lane: direct handler call; Mollusk lane: SVM-mediated tx).
4. **Observe the outcome**: handler `Err` → claim is upheld; handler
   `Ok` → claim is stale and the finding is Critical.

### SAFETY-clause → negation-strategy table

| SAFETY claim shape | Negation strategy | Expected outcome |
|---|---|---|
| "single mutable borrow to X" | `alias_buffer` | Miri aliasing diagnostic |
| "X is initialized" | `uninit_init_flag` | handler Err |
| "X != Y" (distinctness) | `swap_position` | handler Err or Miri UB |
| "lamports >= amount" | `short_balance` | handler Err or underflow |
| "data.len() >= N" | `short_buffer` | handler Err or Miri OOB |
| "owner == program_id" | `foreign_owner` | handler Err |
| "amount <= supply" | `oversized_amount` | overflow / wrap |

## What counts as a finding

- **Critical** when the adversarial input drives the handler to `Ok`
  *and* a conservation invariant fails post-state.
- **High** when the adversarial input drives `Ok` but no conservation
  invariant fails (claim is technically unenforced but exploit path
  is unclear).
- **Medium** when the agent can't conclusively demonstrate the gap
  but the SAFETY claim has no in-context supporting check.
- **Suppress** when every clause is provably upheld by the CF graph.

## Mollusk reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_mollusk.rs
//
// One test per SAFETY clause negation. ${ADVERSARIAL_INPUTS} expands
// into N test bodies, one per entry.
#[test]
fn probe_${ID}_safety_${STRATEGY}() {
    // Negation strategy: ${STRATEGY}
    // Original claim: ${CLAIM_TEXT}

    let attack_accounts = adversarial::${STRATEGY}_mollusk(/* params */);
    let ix = build_${FN}_ix(/* attack params */);
    let r = mollusk.process_instruction(&ix, &attack_accounts);

    if matches!(${EXPECTED_OUTCOME}, "handler_err") {
        assert!(r.program_result.is_err(),
            "SAFETY claim stale: handler accepted ${STRATEGY} input");
    }
}
```

## Miri reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_miri.rs
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_${STRATEGY}() {
    let mut accounts = adversarial::${STRATEGY}_setup();
    let pre = state::capture_global_state(&accounts);

    let res = ${FN}(&accounts, /* data */ &[]);
    let post = state::capture_global_state(&accounts);

    // SAFETY claim is honest iff:
    //   - res is Err, OR
    //   - Miri's aliasing / OOB / overflow checker fired before reaching
    //     this assertion (Miri exits the test process on UB).
    if res.is_ok() {
        ${INVARIANT_ASSERTS}
        panic!("SAFETY claim STALE: handler accepted ${STRATEGY}; state may have corrupted silently");
    }
}
```

## Cross-references

- **Compose with every other Pinocchio probe.** Stale SAFETY claims
  are the meta-finding — when an unchecked_account_load probe is
  uncertain, the SAFETY comment is the next reading.
