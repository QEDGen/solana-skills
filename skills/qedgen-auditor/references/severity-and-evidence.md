# Severity and Evidence

Evidence and impact are independent axes.

## Evidence

| State | Required basis | Report treatment |
|---|---|---|
| Confirmed | Fired executable reproducer | Vulnerability total |
| Structural | Reachable vulnerable path established from source; execution unavailable | Separate structural total |
| Hypothesis | Intent, reachability, or precondition unresolved | Open questions only |
| Rejected | Disproved, protected, unreachable, or non-reproducing | Suppressed count |

`Inconclusive` is a tool outcome, not an evidence state. Convert it to
`structural` only when the source independently establishes reachability;
otherwise convert it to `hypothesis`.

## Severity

- CRITICAL: direct and repeatable loss of user funds, unbounded mint, total
  authority takeover, or permanent protocol-wide denial of service without a
  special market or victim-action precondition.
- HIGH: conditional fund loss, partial authority takeover, or protocol-wide
  griefing requiring timing, market state, or victim action.
- MEDIUM: bounded exploitation, partial denial of service, or integrity loss
  under a narrow reachable precondition.
- LOW: concrete but low-impact anomaly that does not create a stronger reachable
  composition.

State preconditions explicitly. Do not inflate severity by composing with a
second primitive that is absent or unreachable in the audited code. Do not
downgrade impact merely because exploitation is inconvenient; instead record
the gate and reflect it in priority.

To grade consistently, apply the four-step procedure in
[report-and-grading.md](report-and-grading.md) ("Severity grading
(attacker-capability rubric)") to every finding: rate the impact ceiling
with preconditions assumed true, record gates as qualifiers rather than
discounts, downgrade only for no articulable attacker capability or genuine
unreachability, and report a composition at the ceiling of its chain.
