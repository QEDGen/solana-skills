# Arithmetic-symbol probe: silent_success_arithmetic

## Pattern

```rust
let time_since_start = current_ts.saturating_sub(*current_period_start_ts);
if time_since_start >= period_length {
    // period advancement — gives the merchant fresh budget
    advance_period(ctx)?;
}
```

`saturating_sub` (or `saturating_add`) on a timestamp-shape receiver
(`current_ts`, `Clock::get()?.unix_timestamp`, `slot`, `epoch`,
`block_height`, or any identifier ending in `_ts` / `_secs` / `_time`)
whose result feeds a `>=` / `>` comparison gating a non-trivial effect.

## Why it matters

`saturating_sub` returns 0 when the conceptual operation underflows.
For timestamp arithmetic the underflow case is "time hasn't reached the
threshold yet" — but the boundary value 0 is *also* "exactly at the
threshold." A `>=` comparison can't tell them apart, so a downstream
gate fires when it shouldn't.

Canonical example (a real-world subscription program):

```rust
// transfer_validation.rs:61
let time_since_start = current_ts.saturating_sub(*current_period_start_ts);
if time_since_start >= period_length {
    // period advancement — gives the merchant fresh budget
    ...
}
```

When `current_ts < current_period_start_ts` (e.g. clock drift, system
clock rollback, or a future-dated `start_ts`), `saturating_sub` returns
0. The `>= period_length` test then fires only when `period_length == 0`
— but `period_length == 0` means "instantaneous renewal," which combined
with the boundary-value collapse opens a fund-flow gate that should
have stayed closed.

## What the agent should check

1. **Receiver shape**: confirm the LHS is a timestamp / slot / epoch
   value, not a balance or counter. Counter `saturating_sub` (fee
   accounting, supply caps) is legitimate.
2. **Gated effect**: trace the downstream `if` block. If it touches
   funds (transfer, mint, state advance), this is a fund-flow leak.
   If it just logs / increments a metric, suppress.
3. **Underflow possibility**: confirm the receiver can be less than
   the operand on any reachable path. Common shapes: clock drift,
   future-dated start times, system clock rollback.
4. **Existing guard**: is there an `if recv < operand { return Err(...) }`
   *before* the `saturating_sub`? If yes, the operator is defensive and
   the comparison is sound — suppress.

## What counts as a finding

- **High severity** when the gated effect mutates fund-flow state
  (transfer, mint, period advancement) and no upstream `recv >=
  operand` guard exists.
- **Medium severity** when the gated effect is reversible (e.g. just
  flips a status flag) — still suppress-worthy in many cases.
- **Suppress** when the call is followed only by logging / metrics, or
  preceded by an explicit underflow guard.

## Recommended fix

Replace `saturating_*` with an explicit underflow check:

```rust
if current_ts < *current_period_start_ts {
    return Err(MultiDelegatorError::ClockDriftRollback.into());
}
let time_since_start = current_ts - *current_period_start_ts;
if time_since_start >= period_length {
    advance_period(ctx)?;
}
```

The early return makes the "time hasn't elapsed" branch
distinguishable from the "time has elapsed" branch. The bare `-` is
safe because the early return proves the LHS is greater than or equal
to the RHS.

## Mollusk reproducer

Substitutions: `${FILE}`, `${LINE}`, `${RECEIVER}`, `${OPERATOR}`, `${FN}`.

```rust
// .qed/probes/arithmetic_symbol/${ID}/repro.rs
//
// Reproducer for silent_success_arithmetic at ${FILE}:${LINE}.
// `${RECEIVER}.${OPERATOR}(...)` inside `${FN}`.
//
// Attack: set the receiver value strictly less than the operand. The
// operator returns 0; the downstream `>=` gate fires when it should
// stay closed.
//
// Agent fills:
//   1. The litesvm setup that creates the program account in the state
//      where the gated effect is observable (subscription / plan /
//      delegation, depending on the cited handler).
//   2. The attack input (a `current_ts` injected via Clock::set_sysvar
//      / a manipulated `start_ts` field).
//   3. The assertion that the gated effect fired despite the precondition
//      not being met (state changed when it shouldn't have).

use litesvm::LiteSVM;
use solana_pubkey::Pubkey;

#[test]
fn silent_success_arithmetic_at_${FILE}_line_${LINE}() {
    let mut svm = LiteSVM::new();
    // TODO(agent): load the program .so and create the program state
    //              that exposes the `${FN}` handler.
    todo!("agent-fill: program setup");

    // TODO(agent): set the system clock such that `${RECEIVER}` reads a
    //              value strictly less than the comparand operand. For
    //              `current_ts` shapes this is a rolled-back clock or a
    //              future-dated start_ts; for `slot` shapes use
    //              `svm.warp_to_slot`.
    todo!("agent-fill: clock manipulation");

    // TODO(agent): invoke `${FN}` and capture the effect.
    let outcome = todo!("agent-fill: tx send");

    // The attack succeeds when the gated effect fired despite the
    // precondition not being met. Concretely: state advanced /
    // funds transferred / mint authorized — anything the IDL's
    // post-state declares as the handler's effect.
    assert!(
        gated_effect_fired(&svm),
        "saturating_{} collapsed the boundary-value branch and the gate fired",
        "${OPERATOR}".trim_start_matches("saturating_"),
    );
}

fn gated_effect_fired(_svm: &LiteSVM) -> bool {
    // TODO(agent): read the on-chain state and return true when the
    //              downstream gated effect (transfer / mint / state
    //              advance) happened.
    todo!()
}
```

Time-to-fired-repro target: ≤ 20 min per finding.
