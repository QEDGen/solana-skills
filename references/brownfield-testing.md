# Brownfield Testing Strategy

This file preserves the brownfield testing guidance that used to live in
`SKILL.md`.

## Read Existing Tests First

Before generating or writing new harnesses, inspect:

- `tests/`
- `#[cfg(test)]` modules
- `#[cfg(kani)]` modules
- Fixture builders
- Program-test or Anchor test helpers
- Existing proptest strategies

Existing tests show the project's state constructors, account fixtures,
mock CPIs, and known invariants. Reuse that infrastructure unless there is a
clear reason not to.

## Prefer Complementary Harnesses

Use generated harnesses to cover properties missing from existing tests.
Avoid replacing tests that already check behavior well.

Good brownfield additions:

- A signer cannot mutate another user's account.
- A failed guard leaves state unchanged.
- Deposits and withdrawals conserve assets.
- Indexed mutations affect only the selected account.
- Lifecycle handlers reject invalid pre-status values.

## Anchor Adapter Loop

For Anchor source:

```bash
qedgen adapt --program programs/my_program --out program.qedspec
qedgen check --spec program.qedspec --anchor-project programs/my_program
qedgen adapt --program programs/my_program --spec program.qedspec
```

Paste emitted `#[qed(verified, ...)]` attributes only after reviewing the
spec and source diff. Never auto-update hashes without inspecting why they
changed.

## When To Stop

Stop adding generated tests once the spec has coverage for the user-visible
security contract and the existing test suite plus generated backends cover
the high-risk paths. Do not chase exhaustive runtime simulation if the spec
property is already proven by a smaller model.
