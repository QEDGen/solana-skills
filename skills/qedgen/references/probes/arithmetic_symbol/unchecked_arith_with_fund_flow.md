# Arithmetic-symbol probe: unchecked_arith_with_fund_flow

## Pattern

```rust
period_length_s = plan.data.period_hours * 3600;
// ... later
transfer_with_delegate(/* uses period_length_s */)?;
```

Bare `*` / `+` / `-` arithmetic of the form `<ident> <op> <literal>`
inside a function whose body also contains a token / system CPI
(direct: `invoke`, `Transfer`, `MintTo`; helper-shaped:
`transfer_with_delegate`, `mint_to_user`, `deposit_pool`).

## Why it matters

The arithmetic is locally safe today under the program's current
bounds (e.g. a subscription program where `period_hours` is capped at
`MAX_PLAN_PERIOD_HOURS = 8760` upstream), but the local code makes
no explicit invariant claim. If the upstream bound ever loosens, the
operator wraps and the fund-flow effect proceeds on a corrupted
value. This is the canonical "fuse hidden three call frames away"
failure mode.

Canonical example (a real-world subscription program):

```rust
// transfer_subscription.rs:61
period_length_s = plan.data.period_hours * 3600;
```

`period_hours: u16` × `3600` is bounded today. But `period_hours` is
loaded from a maintainer-mutable plan struct; a plan update that
loosens the cap would silently wrap into a tiny `period_length_s`,
making the subscription billable many times more frequently than
intended.

## What the agent should check

1. **Upstream bound**: trace `<lhs>`'s origin. Does it have an
   explicit invariant (`requires`, `assert!`, constant cap) that
   bounds it? If yes, document the bound in a comment at the
   arithmetic site and confirm the bound is enforced on *every* path.
2. **Fund-flow reachability**: confirm the result flows into the
   CPI. If the arithmetic is local book-keeping (a debug log, a
   metric increment), suppress.
3. **Bound stability**: who can change the upstream cap? If a
   handler exists that updates the bound (mutable plan, governance
   update), the local arithmetic should not assume the bound is
   constant — use `checked_*`.

## What counts as a finding

- **Low severity** (always). The recommendation is preventive: even
  when the bound holds today, the checked variant survives future
  loosening of upstream constraints.
- **Suppress** when the result is provably bounded inside the same
  function (e.g. multiplication of two constants, or an LHS that
  was just clamped with `min`).

## Recommended fix

Replace the bare operator with its checked variant:

```rust
period_length_s = plan
    .data
    .period_hours
    .checked_mul(3600)
    .ok_or(MultiDelegatorError::ArithmeticOverflow)?;
```

The explicit error path documents the local bound assumption and
survives upstream changes that loosen `period_hours`'s range.

## Mollusk reproducer

Substitutions: `${FILE}`, `${LINE}`, `${LHS}`, `${OPERATOR}`,
`${RHS}`, `${FN}`.

```rust
// .qed/probes/arithmetic_symbol/${ID}/repro.rs
//
// Reproducer for unchecked_arith_with_fund_flow at ${FILE}:${LINE}.
// `${LHS} ${OPERATOR} ${RHS}` inside `${FN}`.
//
// The probe is preventive; a reproducer that *fires today* requires
// loosening the upstream bound on `${LHS}` to a value where the
// operator wraps. Two test patterns are useful:
//
//   1. Bound-stress: set `${LHS}` to its current max via the
//      mutating upstream path; assert the arithmetic returns the
//      expected (still-safe) result. The test pins the current
//      invariant — when someone loosens the bound, this test
//      reaches the wrap and fails.
//
//   2. Counterexample mock: temporarily expose a setter that bypasses
//      the upstream bound (test-only `#[cfg(test)]` accessor),
//      drive `${LHS}` past the wrap boundary, assert the fund-flow
//      effect proceeds with a corrupted value. Demonstrates the
//      latent bug.

use litesvm::LiteSVM;

#[test]
fn unchecked_arith_at_${FILE}_line_${LINE}_pins_current_bound() {
    let mut svm = LiteSVM::new();
    // TODO(agent): load the program .so, set up the upstream entity
    //              that bounds `${LHS}`. Drive `${LHS}` to its
    //              documented maximum.
    todo!("agent-fill: bound-stress setup");

    // TODO(agent): invoke `${FN}` and confirm the arithmetic produces
    //              the safe result. This test FAILS when the upstream
    //              bound loosens — the canary that surfaces the
    //              latent wrap.
    let outcome = todo!("agent-fill: tx send");
    assert!(outcome.is_ok(), "arithmetic still safe under current bound");
}
```

Time-to-fired-repro target: ≤ 20 min per finding.
