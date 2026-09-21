# Pinocchio probe: offset_overrun

## Pattern

```rust
let d = unsafe { acct.borrow_unchecked() };
let amount = u64::from_le_bytes(d[OFFSET..OFFSET+8].try_into().unwrap());
```

Or any `data[CONST..CONST+N]` indexing where `CONST+N` may exceed
the minimum account size on some path.

## Why it matters

A short account triggers a panic (`try_into().unwrap()` on a too-short
slice) or — worse — UB if the unchecked variants return raw pointers
into a buffer the caller assumes is at least `N` bytes long.

The danger surface is the **handler accepting an account with
`data.len() < OFFSET + N`** without an explicit length check.

## What the agent should check

1. **Explicit length check**: is there an `if acct.data_len() < N`
   guard dominating the index?
2. **Init helper coverage**: does the load helper validate length
   against `size_of::<T>()` internally?
3. **OFFSET arithmetic**: when OFFSET is computed at runtime (sums of
   user-controlled fields), is the upper bound proven?

## What counts as a finding

- **Medium severity** for the panic class (short buffer → unwrap
  panics; handler aborts but no state corruption).
- **High severity** for the UB class (raw pointer arithmetic past the
  end without a length check).

## Mollusk reproducer

```rust
#[test]
fn probe_${ID}_short_buffer() {
    let short_acct = Account {
        lamports: 1_000_000,
        owner: program_id,
        data: vec![0; ${LO}], // shorter than ${HI}
        ..Account::default()
    };
    let ix = build_${FN}_ix(/* params */);
    let r = mollusk.process_instruction(&ix, &accounts);
    assert!(r.program_result.is_err(),
        "handler did not check data_len before reading offset ${HI}");
}
```

## Miri reproducer

```rust
#![cfg(miri)]

#[test]
fn probe_${ID}_miri_short_buffer() {
    let mut accounts = adversarial::short_buffer::<${HI}>();
    let res = ${FN}(&accounts, &[]);
    // Miri flags OOB read on the raw pointer arithmetic.
}
```
