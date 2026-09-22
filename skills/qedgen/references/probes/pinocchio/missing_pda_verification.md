# Pinocchio probe: missing_pda_verification

## Pattern

```rust
let pda = &accounts[N];
// no find_program_address call against pda.key()
let data = unsafe { load_mut::<State>(pda.borrow_mut_data_unchecked())? };
```

Account treated as program-owned PDA but no `find_program_address`
derivation reachable in the handler.

## Why it matters

PDAs are the program's *own* state. Without re-deriving the expected
address from canonical seeds, an attacker can pass any account at
position N (even one they fully control elsewhere) and have the
handler treat it as program state.

## What the agent should check

1. **Derivation site**: grep the handler body and immediate callees
   for `find_program_address` / `create_program_address`. The
   derivation seeds should be content-addressable (user pubkey,
   sequence number, ...).
2. **Address comparison**: after derivation, is the result compared
   to `pda.key()` and the handler aborted on mismatch?
3. **Bump persistence**: when the canonical bump is stored on chain
   (the v3.0 named-invariants pattern), is the on-chain bump used
   in derivation rather than re-deriving from scratch?

## What counts as a finding

- **High severity** when no derivation is reachable from the use site.
- **Medium** when derivation exists but uses non-canonical seeds
  (attacker can grind).

## Mollusk reproducer

```rust
#[test]
fn probe_${ID}_non_pda_accepted() {
    let attacker_pda = Pubkey::new_unique(); // NOT derived from program seeds
    let acct = Account {
        owner: program_id, // attacker initialized it
        ..Account::default()
    };

    let ix = build_${FN}_ix_with_pda(attacker_pda);
    let r = mollusk.process_instruction(&ix, &[(attacker_pda, acct), ..]);

    assert!(r.program_result.is_err(),
        "handler accepted non-PDA account as program state");
}
```

## Miri reproducer

```rust
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_non_pda() {
    let mut accounts = adversarial::non_pda_at_position::<${INDEX}>();
    let res = ${FN}(&accounts, &[]);
    if res.is_ok() {
        invariants::assert_no_unowned_writes(/* pre, post */);
    }
}
```
