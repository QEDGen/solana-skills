# Paired-validator probe: paired_validator_input_domain_mismatch

## Pattern

Two `if <cond> { return Err(...) }` validator sites in the same
program apply distinct accept-domains to the same logical field.

```rust
// create_fixed_delegation.rs (REJECTS expiry_ts == 0)
if self.expiry_ts < current_time.saturating_sub(TIME_DRIFT_ALLOWED_SECS) {
    return Err(MultiDelegatorError::FixedDelegationExpiryInPast);
}

// transfer_validation.rs (TREATS expiry_ts == 0 AS "never expires")
if expiry_ts != 0 && current_ts > expiry_ts {
    return Err(MultiDelegatorError::DelegationExpired);
}
```

## Why it matters

The mismatch is a **sentinel-semantics drift across handlers**. Users
following the docs for one path can hit a hard rejection on the
other. A delegate created with `expiry_ts = 0` to mean "never
expires" passes the transfer validator (which honors the sentinel)
but fails create-path validation (which compares against
`current_time - drift` and finds `0` to be "very far in the past").

Canonical examples (a real-world subscription/delegation program):

- `create_fixed_delegation::validate` vs `transfer_validation` on
  `expiry_ts == 0`.
- `create_recurring_delegation::validate` vs `transfer_validation` on
  `expiry_ts == 0`.
- `create_plan::validate` enforces `end_ts >= now + period_secs`;
  `update_plan::validate` enforces only `end_ts > now`.
- creation uses a `TIME_DRIFT_ALLOWED_SECS` tolerance; transfer uses
  strict `>`.

## What the agent should check

For each of the distinct shapes listed in `${SHAPES}`:

1. **Identify the sentinel**: what specific input value triggers the
   *distinct* branch in each validator?
   - `expiry_ts == 0` is the canonical "never expires" sentinel.
   - `period_length_s == 0` is "instantaneous renewal."
   - `amount == u64::MAX` is "unlimited."
2. **Check the docs**: is the sentinel documented (`/// 0 means
   "never expires"`)? If yes, the validator that REJECTS the
   sentinel is the bug.
3. **Trace the user path**: walk the program's typical caller flow.
   Which validator does the user hit first? Does the documented
   sentinel get rejected anywhere on the path?

## Severity escalation rules

(Per PRD-v2.22 §S2.1)

- **High** when (a) the sentinel is documented as having a special
  meaning (comment / doc-string near the field declaration), AND (b)
  exactly one validator honors it. Documented-sentinel drift is
  unambiguous spec violation.
- **High** when the mismatch is between create-path (stricter) and
  update-path (laxer) on a fund-flow field (`amount`, `end_ts`,
  `period_hours`). An attacker who can flip the field via the laxer
  update path bypasses the create-path invariant.
- **Medium** otherwise (validator strictness drift with no clear
  sentinel semantics in play). The default emission.

## What counts as a finding

- **High** per escalation rule above.
- **Medium** per default.
- **Suppress** when the validators operate on different
  *logical* fields that happen to share a name (e.g. two distinct
  account types each with their own `bump` field; the rule
  shouldn't pair them).

## Recommended fix

Three remediation patterns (per PRD-v2.22 §S2.1):

1. **Align**: pick the stricter shape and apply it everywhere. The
   conservative shape becomes the documented contract.
2. **Document + audit**: keep the sentinel, document it explicitly,
   audit every validator for compliance.
3. **Split**: if the semantics are truly different, split the field
   (`expiry_ts: Option<i64>` instead of `i64` where 0 is a sentinel).

## Mollusk reproducer

Substitutions: `${FIELD}`, `${SITE_A}`, `${CONDITION_A}`,
`${SITE_B}`, `${CONDITION_B}`, `${SHAPES}` (multi-line summary).

```rust
// .qed/probes/paired_validator/${ID}/repro.rs
//
// Reproducer for paired_validator_input_domain_mismatch on field
// `${FIELD}`.
//
//   Site A: ${SITE_A}
//   Cond:   ${CONDITION_A}
//
//   Site B: ${SITE_B}
//   Cond:   ${CONDITION_B}
//
// Pick an input value in the symmetric difference of the two
// validators' accept-domains — typically the sentinel `0`. The
// test should pass through one validator and fail at the other.
//
// Agent fills:
//   1. Identify the sentinel value (often `0`).
//   2. Construct an instruction call that hits Site A with the
//      sentinel. Confirm accept-or-reject matches the validator's
//      shape.
//   3. Construct an instruction call that hits Site B with the
//      same sentinel. Confirm the OPPOSITE outcome.
//   4. The pair of opposite outcomes proves the asymmetry.

use litesvm::LiteSVM;

#[test]
fn paired_validator_mismatch_on_${FIELD}() {
    let mut svm = LiteSVM::new();
    // TODO(agent): set up the program state shared by both
    //              handler paths.
    todo!("agent-fill: program setup");

    // TODO(agent): pick the sentinel value for ${FIELD}.
    let sentinel = todo!("agent-fill: sentinel value");

    // TODO(agent): invoke Site A's handler with ${FIELD} = sentinel.
    let outcome_a = todo!("agent-fill: site A invocation");

    // TODO(agent): invoke Site B's handler with ${FIELD} = sentinel.
    let outcome_b = todo!("agent-fill: site B invocation");

    // The asymmetry: outcomes differ. The user can interact via one
    // path but not the other.
    assert_ne!(
        outcome_a.is_ok(),
        outcome_b.is_ok(),
        "validator asymmetry on `${FIELD}`: site A {outcome_a:?}, site B {outcome_b:?}"
    );
}
```

Time-to-fired-repro target: ≤ 20 min per finding (often less — the
sentinel value is usually obvious from the doc-strings).
