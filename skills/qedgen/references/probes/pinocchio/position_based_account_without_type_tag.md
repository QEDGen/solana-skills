# Pinocchio probe: position_based_account_without_type_tag

## Pattern

```rust
let [src, mint, dst, auth, ..] = accounts else { return Err(...) };

// later, no owner check, no discriminator check:
let src_data = unsafe { src.borrow_data_unchecked() };
```

The handler trusts position N is type `T` because of the array
destructure alone — no owner check, no first-byte tag check.

## Why it matters

A user can pass *any* account at position N (subject only to lamport
constraints), and the handler will interpret it as `T`. This is the
core difference from Anchor's `#[derive(Accounts)]`, which validates
each position has the expected owner / discriminator / PDA before
the handler body runs.

## What the agent should check

1. **Owner check**: does the handler call `check_account_owner` or
   compare `src.owner()` against an expected program id before using
   `src`?
2. **Init / discriminator check**: does the load helper or the handler
   body verify the first byte / discriminator / state field of the
   account before mutating?
3. **PDA derivation**: if the account is expected to be a PDA, does
   `find_program_address(seeds, program_id)` get re-derived and
   compared to `src.key()`?

## What counts as a finding

- **High severity** when no owner check and no discriminator gate
  dominates the use site.
- **Medium** when an owner check exists but the discriminator is
  trusted implicitly.

## Mollusk reproducer

```rust
// Repro: pass a system-owned account at position N (where T is expected).
// Assert handler accepts (= finding) or rejects (= safe).
#[test]
fn probe_${ID}_wrong_owner_at_position() {
    let attacker_owned = Account {
        lamports: 1_000_000,
        owner: solana_sdk::system_program::ID, // wrong!
        data: vec![0; 165],
        ..Account::default()
    };

    let ix = build_${FN}_ix(/* params */);
    let r = mollusk.process_instruction(&ix, &accounts);

    assert!(r.program_result.is_err(),
        "handler accepted system-owned account at position ${INDEX}");
}
```

## Miri reproducer

```rust
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_position() {
    let mut accounts = adversarial::wrong_owner_at_position::<${INDEX}>();
    let res = ${FN}(&accounts, &[]);
    if res.is_ok() {
        invariants::assert_no_unowned_writes(/* pre, post, program_id */);
        panic!("handler treated foreign-owned account as program type");
    }
}
```

## Cross-references

- Composes with `account_type_confusion`.
- Pairs with `missing_pda_verification` when the position is expected
  to hold a PDA.
