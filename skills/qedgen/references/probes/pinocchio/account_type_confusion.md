# Pinocchio probe: account_type_confusion

## Pattern

Same `AccountInfo` loaded as `T1` in handler `A` and `T2` in handler
`B` without a discriminator distinguishing them.

```rust
// handler_a: treats `state` as `Account`
let state = unsafe { load_mut::<Account>(info.borrow_mut_data_unchecked())? };

// handler_b (elsewhere): treats the same account position as `Mint`
let state = unsafe { load_mut::<Mint>(info.borrow_mut_data_unchecked())? };
```

## Why it matters

Pinocchio has no `#[derive(Accounts)]` to validate the runtime layout
of the account at a given position. If two handlers reading the same
account position interpret the bytes differently and the program
doesn't store a discriminator, an attacker who controls one handler's
output can poison the other.

## What the agent should check

1. **Cross-handler scan**: list every `load*::<T>(info.borrow_*)` site
   grouped by which `AccountInfo` position they read. Look for
   collisions where two handlers read the same position as different
   `T`.
2. **Discriminator gate**: does the loaded type have a first-byte
   tag / enum discriminator that the handler checks before treating
   the payload as `T`?
3. **Owner check as a proxy**: a strict `owner == this_program_id`
   gate plus a single-type invariant per owner program closes the
   gap. Absence of either is a finding.

## What counts as a finding

- **High severity** when a Mollusk repro initializes account at
  position N via handler A (type T1), then invokes handler B which
  loads N as T2 and reaches a state-mutating effect.
- **Medium** when the load types differ but the handlers are isolated
  by lifecycle (one writes, the other only reads).
- **Suppress** when a runtime discriminator gates the type cast.

## Mollusk reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_mollusk.rs
//
// Repro: invoke handler_a to initialize account X as type T1.
// Then invoke handler_b which reinterprets X as T2 and mutates it.
// Assert post-state shows handler_b's mutation applied to bytes that
// were structured as T1.
#[test]
fn probe_${ID}_type_confusion() {
    let mollusk = Mollusk::new(&program_id, "${PROGRAM_BIN}");

    // Step 1: init X as T1.
    let init_ix = build_handler_a_ix(/* params */);
    let r1 = mollusk.process_instruction(&init_ix, &accounts);
    assert!(r1.program_result.is_ok());

    // Step 2: invoke handler_b on X as T2.
    let confuse_ix = build_handler_b_ix(/* params */);
    let r2 = mollusk.process_instruction(&confuse_ix, &accounts_after_init);

    // If r2 is Ok, the type confusion succeeded.
    assert!(r2.program_result.is_err(),
        "type confusion: handler_b accepted T1-shaped account");
}
```

## Miri reproducer

```rust
// .qed/probes/pinocchio/${ID}/repro_miri.rs
//
// Under Miri, casting a T1-shaped buffer to &T2 with mismatched
// validity invariants triggers a UB diagnostic.
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_validity() {
    let mut buf = build_T1_bytes(/* params */);

    // Handler-style load.
    let r = unsafe {
        load_mut::<T2>(&mut buf[..])
    };

    // Miri should flag invalid bit patterns for T2 — e.g. an enum
    // discriminant out of range.
}
```

## Cross-references

- Composes with `position_based_account_without_type_tag`.
- Pairs with `missing_pda_verification` when the confused account is
  expected to be a PDA.
