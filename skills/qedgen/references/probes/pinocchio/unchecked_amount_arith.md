# Pinocchio probe: unchecked_amount_arith

## Pattern

```rust
destination_account.set_amount(destination_account.amount() + amount);
// or
account.set_amount(account.amount() - amount);
```

Manual arithmetic on token amounts using `+` / `-` rather than
`checked_add` / `checked_sub`.

## Why it matters

The comment at the canonical p-token site claims "the amount of a token
account is always within the range of the mint supply (`u64`)" — but
the actual bound isn't enforced at this call site. Mint supplies are
not capped at `u64::MAX / 2`, and split-account holdings can sum past
the supply. A wrap on a token amount silently drains accounting.

## What the agent should check

1. **Bound proof**: is there a `requires` clause / explicit guard
   proving `amount + delta <= u64::MAX` on every CF path? If yes,
   suppress; if no, this is a finding.
2. **Source of `amount`**: does the input come from an attacker-controlled
   instruction payload, or is it computed internally with a known
   small bound? Attacker-controlled → high severity.
3. **Comment vs reality**: when a comment claims "amount is bounded by
   X", grep for X being enforced at *this* call site, not just at a
   distant initialization site.

## What counts as a finding

- **High severity** when the input to the arithmetic is reachable from
  an unbounded handler parameter (`amount: u64`) without intermediate
  checked arithmetic.
- **Medium** when the amount is bounded but the bound is derived from
  a comment rather than a check.
- **Suppress** when the call site is unreachable from external input
  (constructor-time math with literals).

## Mollusk reproducer

Substitutions: `${FILE}`, `${LINE}`, `${FN}`.

```rust
// .qed/probes/pinocchio/${ID}/repro_mollusk.rs
//
// Repro: invoke ${FN} with destination.amount = u64::MAX - 1 and
// amount = 2 to trigger wrap. Assert post-state destination.amount
// is silently 1 (wrapped) — proves the bound isn't enforced.
#[test]
fn probe_${ID}_amount_wrap() {
    let mollusk = Mollusk::new(&program_id, "${PROGRAM_BIN}");

    // Pre-state: dst.amount = u64::MAX - 1.
    let dst_data = build_token_account(/* amount */ u64::MAX - 1);
    // TODO: build src account, mint, authority.

    let ix = build_${FN}_ix(/* amount */ 2);

    let result = mollusk.process_instruction(&ix, &accounts);
    let post_dst = result.get_account(&dst_pubkey).unwrap();
    let post_amount = read_amount(&post_dst.data);

    assert!(post_amount < u64::MAX - 1, "amount wrapped: was MAX-1, now {}", post_amount);
}
```

## Miri reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_miri.rs
//
// Direct handler call with oversized inputs. Under Miri's overflow
// checks, `set_amount(amount() + delta)` with delta = 2 and amount =
// MAX-1 panics; under Mollusk's release-mode build, it wraps silently.
// The divergence between the two is the strongest signal.
#![cfg(miri)]

use crate::_harness::{adversarial, invariants, state};

#[test]
fn probe_${ID}_miri_overflow() {
    let mut accounts = adversarial::oversized_amount_setup();
    let pre = state::capture_global_state(&accounts);

    let res = ${FN}(&accounts, /* amount */ &2u64.to_le_bytes());
    let post = state::capture_global_state(&accounts);

    if res.is_ok() {
        // Miri should already have panicked on the `+` overflow. If
        // we reach here, the arithmetic was checked or used
        // wrapping_add — re-read source.
        ${INVARIANT_ASSERTS}
        panic!("expected overflow panic, got Ok");
    }
}
```

## Cross-references

- Pairs with `unchecked_lamport_arith` whenever lamport math sits next
  to amount math.
- Composes with `stale_safety_comment` when a SAFETY block claims the
  bound.
