# Pinocchio probe: unchecked_lamport_arith

## Pattern

```rust
let source_lamports = unsafe { src.borrow_mut_lamports_unchecked() };
*source_lamports -= amount;
// or
*src.borrow_mut_lamports_unchecked() = src.lamports() + amount;
// or
account.set_lamports(account.lamports() - amount);
```

Manual arithmetic on lamports via raw mutable references or `set_lamports`
without `checked_add` / `checked_sub`.

## Why it matters

Comments at canonical p-token sites claim "the `lamports` on the account
is always greater than `amount`" — but the upstream check is often on
token amounts, not lamports. **Native (wrapped SOL) accounts track
lamports separately from `Account::amount`**; the token-amount bound
does NOT propagate. A native-account caller can underflow lamports by
1 token-amount-unit because the proof for tokens doesn't cover
lamports.

## What the agent should check

1. **Bound source**: trace the upstream check the comment cites. Is
   the bound on `amount` (token), `lamports`, or `state.balance`? Only
   a lamport-specific check covers this site.
2. **Native account path**: does the handler reach this code on the
   `is_native()` branch? If yes, the token-amount bound does not
   apply.
3. **Rent-exempt floor**: even when the math is technically valid,
   does the post-state lamport balance drop below rent exemption?
   That's a related-but-distinct finding (`rent_exempt_violation`,
   reserve a slot for follow-up).

## What counts as a finding

- **High severity** when the lamport math runs on a wrapped-native
  path without a lamport-specific bound check.
- **High severity** when the comment cites a token-amount bound for a
  lamport operation.
- **Medium** when no comment exists and the bound is implicit.

## Mollusk reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_mollusk.rs
//
// Repro: invoke ${FN} on a native (is_native()) source account where
// source.amount >= amount but source.lamports < amount. Assert the
// `*source_lamports -= amount` underflows the lamport balance.
#[test]
fn probe_${ID}_lamport_underflow_native() {
    let mollusk = Mollusk::new(&program_id, "${PROGRAM_BIN}");

    // Build native-wrapped account: amount field = 100, lamports = 50,
    // is_native = Some(native_reserve).
    let src = build_native_token_account(/* amount */ 100, /* lamports */ 50);

    let ix = build_${FN}_ix(/* amount */ 75);
    let result = mollusk.process_instruction(&ix, &accounts);

    // Pre-fix: handler returns Ok; src.lamports wraps to ~u64::MAX.
    // Post-fix: handler returns Err (insufficient lamports).
    if result.program_result.is_ok() {
        let post_src = result.get_account(&src_pubkey).unwrap();
        assert!(post_src.lamports < 50, "lamports underflowed");
    }
}
```

## Miri reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_miri.rs
//
// Underflow via direct handler call. Miri's overflow detection
// flags `*source_lamports -= amount` when amount > source_lamports.
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_underflow() {
    let mut accounts = adversarial::short_balance_setup();

    let res = ${FN}(&accounts, /* amount */ &75u64.to_le_bytes());

    if res.is_ok() {
        invariants::assert_lamport_conservation(/* pre/post */);
        panic!("expected lamport underflow");
    }
}
```

## Cross-references

- Pairs with `unchecked_amount_arith`.
- Composes with `stale_safety_comment` when a SAFETY block conflates
  token-amount and lamport bounds.
