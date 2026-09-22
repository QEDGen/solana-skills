# Pinocchio probe: mutable_borrow_aliasing

## Pattern

```rust
let a = unsafe { x.borrow_mut_data_unchecked() };
let b = unsafe { x.borrow_mut_data_unchecked() }; // same account
*a = ...;
*b = ...;
```

Two `borrow_mut_*_unchecked()` calls on the same account with
overlapping lifetimes. RefCell normally catches this; the unchecked
variants bypass the check.

## Why it matters

Two live `&mut` references to the same buffer is UB. The compiler
won't catch it (the unchecked variant returns a raw `&mut [u8]`),
the runtime won't catch it (no RefCell). Bugs manifest as
non-deterministic state corruption — Miri catches it deterministically.

## What the agent should check

1. **Same-account double-borrow**: list every `borrow_mut_*_unchecked`
   call. For each pair on the same `AccountInfo`, check lifetime
   overlap.
2. **Position aliasing**: the handler may borrow two accounts that
   are actually the same `AccountInfo` because the caller passed it
   twice. Combine with `swap_position` adversarial input.
3. **Cross-handler aliasing**: a CPI to self can re-enter a handler
   with the same accounts — recursion + unchecked borrows = UB.

## What counts as a finding

- **High severity** when Miri flags aliasing in the generated repro.
- **Medium** when the aliasing is only reachable via a self-CPI path.

## Mollusk reproducer

```rust
// Repro: invoke ${FN} with the same account passed at both positions.
#[test]
fn probe_${ID}_aliasing_via_swap() {
    let alias = Pubkey::new_unique();
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(alias, false), // src
            AccountMeta::new(alias, false), // dst — same!
            AccountMeta::new(authority, true),
        ],
        data: build_${FN}_payload(/* amount */ 10),
    };

    let r = mollusk.process_instruction(&ix, &accounts);

    // Mollusk runs the deployed `.so` — UB may manifest as wrong
    // state, not panic. Assert one of:
    //  (a) handler returns Err (program defensively checks aliasing), or
    //  (b) state is the canonical "two-distinct-account" outcome.
    assert!(r.program_result.is_err() || /* state matches expected */ true);
}
```

## Miri reproducer

```rust
// Critical UB lane: Miri's Stacked Borrows / Tree Borrows model
// flags two live `&mut [u8]` to the same backing buffer.
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_alias() {
    let mut accounts = adversarial::alias_buffer_setup();

    let _r = ${FN}(&accounts, /* data */ &[10]);
    // Miri exits with an aliasing error if both unchecked borrows
    // become live concurrently. Test "passes" by reaching here
    // without a Miri abort — failing under Miri is the bug.
}
```

## Cross-references

- Pairs with `account_type_confusion` (same-account-two-types is also
  same-account-two-borrows).
- Composes with `position_based_account_without_type_tag`.
