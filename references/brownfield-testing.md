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

## Anchor Spec-Elicitation Loop

For Anchor source:

```bash
qedgen probe --program programs/my_program --emit-spec-candidates --audit-dir .qed/audit/<ts>
# review <audit-dir>/hypotheses.json; record decisions in <audit-dir>/answers.json
qedgen ratify --audit-dir .qed/audit/<ts>
qedgen check --spec program.qedspec --anchor-project programs/my_program
qedgen verify --spec program.qedspec
qedgen stamp --program programs/my_program --spec program.qedspec
```

The probe hypothesizes evidence-anchored invariants (authorization,
lifecycle/init-once, arithmetic-bound, conservation, CPI-integrity,
unwired-guard, state-machine) and writes a spec skeleton plus
`hypotheses.json`; `ratify` lowers the confirmed hypotheses to executable
clauses, gated on parse + lint. (`qedgen adapt --program` is deprecated:
functional in v2.x with a warning, removed in v3.0.)

`qedgen stamp` (formerly `qedgen adapt --spec`, deprecated) emits the
`#[qed(verified, ...)]` attributes. It is gated on
`.qed/verify-evidence.json` — recorded by `qedgen verify` — matching the
spec and program-source hashes with at least one passing
implementation-bound backend (Miri or a `kani_impl*.rs` harness); checking,
model-tested results, and bug-oriented `--probe-repros`/Mollusk runs are not
eligible.

Paste emitted `#[qed(verified, ...)]` attributes only after reviewing the
spec and source diff. Never auto-update hashes without inspecting why they
changed.

## When To Stop

Stop adding generated tests once the spec has coverage for the user-visible
security contract and the existing test suite plus generated backends cover
the high-risk paths. Do not chase exhaustive runtime simulation if the spec
property is already proven by a smaller model.
