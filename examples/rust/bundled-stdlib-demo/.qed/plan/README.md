# .qed/plan/

Agent-maintained ledger of what qedgen caught, what it missed, and what
reviewers surfaced after the fact. Committed by default.

## Layout

- `findings/NNN-<slug>.md` — pattern-tagged entries: a probe that fired,
  a reviewer's callout, a gap that surfaced in testing. One pattern per
  file. Reference the pattern, not the incident.
- `sessions/YYYY-MM-DD-<topic>.md` — session summaries written at
  meaningful boundaries (spec finalized, proofs shipped, bug resolved).
  Three fields: what we tried, what worked, what we'd do differently.
- `gaps.md` — running list of "qedgen didn't catch X; Y did" with a
  one-line hypothesis for what lint or harness would've caught it.
- `reviewers.md` — external-review feedback, pattern-tagged.
- `scoping.md` — moments the agent recommended NOT engaging qedgen's
  default flow (target shape doesn't fit, Phase 2 direct, skip-spec,
  forced-shim). Each entry: target shape, why `.qedspec` didn't fit
  structurally, what was recommended instead, and one-line DSL-evolution
  hypothesis. This is the richest signal for extending the DSL —
  capture it **before** walking away, not after.

Subdirectories are created lazily as entries are written.

## What to capture

Capture **patterns**, not business specifics. A good entry names a class
of bug and the shape of the guard that would catch it. A bad entry names
an account, a pubkey, a user, or a dollar value.

Good: *"Generic const parameter flowed into an `as u16` cast without a
force-evaluated compile-time bound — silent wrap at the 65,536th push."*

Bad: *"Alice's vault overflowed when she deposited 2^16 times."*

## Telemetry (future)

This ledger is the seed corpus for future qedgen lints and probes. A
future opt-in `qedgen telemetry push` will upload entries anonymised;
until that command ships, `.qed/plan/` is local-to-your-repo. You
control what leaves: inspect, edit, or delete any entry before
uploading. Scrubbing rules above are the contract.
