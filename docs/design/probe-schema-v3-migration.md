# Probe output schema v3 — migration note (#227)

`qedgen probe` output moves from `version: 2` to `version: 3`. The change is
**additive at the field level** but **semantically breaking for one specific
consumer habit**: reading an empty `findings[]` as "the program is clean".

## What's new

Four fields on the `ProbeOutput` envelope:

| Field | Type | Meaning |
|---|---|---|
| `candidates[]` | array | Predicate hits / static patterns to investigate. **No severity, no reproducer.** Not a claim of exploitability. |
| `engine_runs[]` | array | Per-engine `passed \| partial \| blocked \| failed \| skipped`, plus `candidates_dropped` and `skipped_files`. |
| `coverage` | object \| absent | What the run discovered/exercised/asserted (`handlers_discovered`, `actions_generated`, `corpus_size`, …). |
| `outcome` | enum | `passed_with_coverage \| no_findings_low_coverage \| blocked_incomplete_harness \| engine_failed \| dry_run`. |

`findings[]` is unchanged: every entry still carries a reproducer (the
reproducible-only contract). What changed is that a predicate hit whose
reproducer can't be constructed is now **preserved in `candidates[]`** instead
of being silently dropped — so a spec with live predicate hits is no longer
indistinguishable from a clean one.

## Why the tier split

`candidates[]` sits deliberately below `findings[]`:

- `findings[]` — reproducer-backed, confirmed-subject-to-review. The "no
  advisory tier" rule keeps unproven claims out of here.
- `candidates[]` — a work list. Because candidates carry no severity and no
  reproducer, they can't be mistaken for confirmed issues, which is what keeps
  the no-advisory-tier rule intact rather than reversing it.

## Migrating a consumer

1. **If you only read `findings[]`** and treat entries as reproducer-backed
   issues: no change required. Same field, same contract.

2. **If you treated empty `findings[]` as "clean"**: you must now branch on
   `outcome`. Only `passed_with_coverage` licenses that reading. The others
   mean the probe under-ran:
   - `no_findings_low_coverage` — engines ran but exercised little.
   - `blocked_incomplete_harness` — a required harness was incomplete.
   - `dry_run` — budget-0 preview; nothing executed.
   - `engine_failed` — an engine errored.
   Also consult `candidates[]`: a non-empty candidate list next to an empty
   `findings[]` means "investigate", not "clean".

3. **If you want per-engine health** (which files were skipped, whether an
   engine was blocked): read `engine_runs[]`. A `partial` run lists the
   unreadable `skipped_files`.

## Version gate

The envelope's `version` field is the single source of truth; the auditor
skill and any tooling should gate on `version == 3`. A v2 reader that ignores
unknown fields keeps working against `findings[]` but silently loses the
empty-vs-underran distinction — upgrade the "empty means clean" check first.
