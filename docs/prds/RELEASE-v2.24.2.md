# Release v2.24.2 — DSL gotcha fixes

Patch release. Closes two first-contact gotchas reported by a v2.24.1
user authoring their first `.qedspec`. Both are about the DSL surface
accepting a shape the downstream tooling didn't handle, with no
parser hint pointing at the working alternative.

No `.qedspec` syntax changes. No new CLI commands. No new behavior on
specs that already lint clean against v2.24.1.

## What's in

### Gotcha 1 — `state { lsts : Map[CONST] T }` parses but the lint never sees the Map type (#54)

The `state { … }` sugar lowers to a Record named `State` in
`out.records`. `check_map_and_subscript` only walks `out.account_types`,
so the Map type information was never visible — every effect that
subscripted the field fired a spurious `subscript_not_map` P0:

```
handler 'deposit' has effect `lsts[idx].balance` but
'lsts' is not a Map-typed state field
```

The pre-fix workaround was to switch to the verbose
`type State | Variant of { ... }` ADT form, which goes through
the ADT path that populates `account_types`.

**Fix:** at the end of `chumsky_adapter::adapt`, when
`out.account_types` is empty *and* there's a Record named `State`,
synthesize a single-variant `ParsedAccountType` from the record.
The record stays in `out.records` for other consumers; only the
lint surface changes.

### Gotcha 2 — `type Error = { … }` brace form silently produces no error variants (#54)

`type Error = { InvalidAmount : U64, ... }` parses cleanly because
`type Name = { ... }` is the record syntax. The spec ends up with
an empty `error_codes` list, so every downstream consumer that
matches against declared error variants (`WrongState` gate,
`MathOverflow` check, CPI error refs) misbehaves silently.

The pipe-less brace form (`type Error { ... }`, no `=`) errors at
column 12 ("found `{`, expected `|` or `=`"), which is the wrong
shape of hint — it doesn't tell the user the pipe form exists.

**Fix:** new `error_declared_as_record` P0 lint in `check.rs`.
Detects a Record named `Error`, surfaces the pipe form as the
fix-it, and quotes the user's record fields back into a generated
`example:` block as a pipe-form skeleton:

```
E [P0] [error_declared_as_record] `type Error = { ... }` (record
  brace form) does not declare error variants — the parser treats it
  as a struct named `Error` and `spec.error_codes` ends up empty.
  Downstream lowering then misbehaves silently (CPI error refs
  unresolved, `WrongState` / `MathOverflow` gates don't fire).
  Fix: Use the pipe form instead of `= { ... }`. Each variant goes
       on its own line with a leading `|`.
  Example:
    type Error
      | InvalidAmount
      | Unauthorized
```

Doesn't try to fix the parser — the brace form is structurally a
record and could be valid for non-Error records.

## Migration

No action required. Existing v2.24.1 specs that already lint clean
continue to lint clean against v2.24.2. New specs authored with the
`state { … }` sugar containing Map-typed fields now lint correctly
without forcing the user to migrate to the ADT form.

Specs that accidentally declared `type Error = { … }` will see a new
P0 with the fix-it. Switch to the pipe form to clear it.

## Tested against

  - `cargo test --release --bin qedgen` — 806/806 pass (2 new tests)
  - `cargo test --release --test codegen_smoke -- --ignored` — 4/4 pass
  - `cargo fmt --check`, `cargo clippy --release -- -D warnings`,
    `bash scripts/check-readme-drift.sh` — all clean
  - `qedgen check --frozen --spec examples/rust/{escrow,multisig,lending}/` (plus the perp-dex example, since removed)
    — exit 0 across the bundled set
