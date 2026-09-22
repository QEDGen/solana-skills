# Pinocchio probe: unchecked_account_load

## Pattern

```rust
let acct = unsafe { load_mut::<T>(info.borrow_mut_data_unchecked())? };
// or
let acct = unsafe { load_mut_unchecked::<T>(info.borrow_mut_data_unchecked())? };
```

Any call site where `_unchecked` account data is wrapped in a typed
load (`load`, `load_mut`, `load_unchecked`, `load_mut_unchecked`,
`StateWithExtensions::unpack_unchecked`, bytemuck::from_bytes, custom
`load*` helpers).

## Why it matters

Pinocchio's `_unchecked` family skips the framework's owner / init /
length / discriminator checks. Anchor's `AccountLoader<T>` does all
four; the Pinocchio author has to write each one inline and prove
they hold on every CF path. A missing check is a CVE.

The `_unchecked` *suffix* on the load helper specifically skips the
init-flag check — the SAFETY comment then has to enumerate where that
guarantee comes from. Read the comment, then find the supporting
check.

## What the agent should check

For each unchecked-load site (in order):

1. **Owner check**: `info.owner() == &TOKEN_PROGRAM_ID` (or equivalent
   constant) must dominate the load on every CF path. Grep callers
   of the containing fn for the wrapping owner-check helper.
2. **Length check**: `info.data_len() >= size_of::<T>()` either by an
   explicit `if !=` guard or because the load helper validates it
   internally (read the helper).
3. **Init / discriminator check**: when the load is `_unchecked`, find
   the upstream call (often `validate_account_or_create` /
   `process_initialize_mint` lifecycle gate) that proved the account
   is initialized.
4. **Distinctness**: when two loads target different accounts and the
   SAFETY comment claims `src != dst`, find the explicit pubkey
   inequality check.

## What counts as a finding

- **High severity** if any check is *absent on a reachable path* and a
  Mollusk repro demonstrates the missing check (foreign owner, short
  buffer, uninit account, swapped position) reaches the load.
- **Medium** if the check exists but is conditional and the agent
  cannot conclusively prove the condition holds.
- **Suppress** with rationale comment when the check provably holds
  via type / context (e.g. account came from a typed
  `find_program_address` derivation earlier in the same fn).

## Mollusk reproducer

Substitutions: `${FILE}`, `${LINE}`, `${FN}`, `${EXPR}`,
`${SAFETY_CLAIM}`, `${CALLEE}`.

```rust
// .qed/probes/pinocchio/${ID}/repro_mollusk.rs
//
// Repro: invoke ${FN} with a foreign-owned source account and assert
// the handler rejects (Err) — or, if it accepts, the SAFETY claim at
// ${FILE}:${LINE} is stale and state corrupts.
//
// Auto-generated stub. Fill in the TODOs below before running.
//
// Run: cargo test --manifest-path target/qedgen-repros/Cargo.toml \
//      --test probe_${ID}
use mollusk_svm::{Mollusk, result::Check};
use solana_sdk::{account::Account, instruction::Instruction, pubkey::Pubkey};

#[test]
fn probe_${ID}_foreign_owner_rejected() {
    let program_id = Pubkey::new_unique();
    let mollusk = Mollusk::new(&program_id, "${PROGRAM_BIN}");

    let attacker = Pubkey::new_unique(); // foreign owner — NOT program_id
    // TODO: populate src/dst account data per ${EXPR} expectations.
    let src = Account { lamports: 1_000, owner: attacker, ..Account::default() };
    let dst = Account { lamports: 1_000, owner: program_id, ..Account::default() };

    let ix = Instruction {
        program_id,
        accounts: vec![/* TODO: AccountMeta entries for src/dst */],
        data: vec![/* TODO: handler discriminator + payload */],
    };

    mollusk.process_and_validate_instruction(
        &ix,
        &[/* TODO: (pubkey, account) pairs */],
        &[Check::err_msg("expected ownership check failure")],
    );
}
```

## Miri reproducer

Direct handler call (no SVM) under `cargo +nightly miri test`.
Substitutions same as above plus `${INVARIANT_ASSERTS}` and
`${ADVERSARIAL_INPUTS}`.

```rust
// .qed/probes/pinocchio/${ID}/repro_miri.rs
//
// Drives ${FN} with the adversarial inputs derived from
// ${SAFETY_CLAIM}. Run: cargo +nightly miri test --manifest-path
// .qed/probes/pinocchio/${ID}/Cargo.toml
#![cfg(miri)]

use crate::_harness::{adversarial, invariants, state};

#[test]
fn probe_${ID}_miri() {
    // TODO: build AccountInfo array via _harness::account::synth(...)
    // applying each entry in ${ADVERSARIAL_INPUTS} (e.g.
    // `foreign_owner`, `swap_position`, ...).
    let mut accounts = adversarial::build_inputs::<{NEGATION}>();
    let pre = state::capture_global_state(&accounts);

    let res = process_${FN}(&accounts, /* TODO: instruction data */ &[]);
    let post = state::capture_global_state(&accounts);

    if res.is_ok() {
        // The handler accepted the adversarial input — SAFETY claim is
        // stale. Drop into invariant asserts; any failure raises the
        // finding to Critical.
        ${INVARIANT_ASSERTS}
        panic!("SAFETY claim stale: handler accepted adversarial input");
    }
    // res is Err — expected outcome. No-op.
}
```

## Cross-references

- Pairs with `stale_safety_comment` whenever the site has a
  `// SAFETY:` block.
- Composes with `account_type_confusion` when the same `AccountInfo`
  is loaded as `T1` and `T2` in different handlers.
