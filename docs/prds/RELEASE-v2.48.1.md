# Release v2.48.1 — honest probe coverage on Pinocchio

Patch release. One fix, in three parts, for a false green in
`qedgen probe --bootstrap`.

An auditor benchmark sweep ran two independent audit workers against a
Pinocchio program. Both reported that the probe contributed nothing and both
fell back entirely to manual review, while the probe itself reported
`passed_with_coverage` with zero handlers resolved. A user reading that
verdict would reasonably conclude the program had been probed clean.

Three independent defects combined to produce it.

## 1. Pinocchio had no source-level handler discovery

`run_bootstrap` matched Anchor, Quasar, QedgenCodegen and Native, then fell
through to a catch-all that returned an empty handler list. Pinocchio handlers
could therefore only ever come from the IDL overlay, so a repo with no
discoverable IDL emitted an empty work list.

`discover_pinocchio_handlers` now walks the source and resolves both
dispatch-body conventions that appear in real programs:

| Convention | Handler name from |
|---|---|
| `pub fn process_<name>(..)` | the function name |
| `pub fn process(..)`, one per `instructions/<name>.rs` | the module file stem |

The function named by `entrypoint!(...)` is the dispatcher, not a handler, and
is excluded. The identifier is read out of the macro rather than hardcoded,
because a program can call its dispatcher anything.

## 2. The IDL ancestor walk only recognised Anchor-shaped repo roots

`discover_workspace_ancestor_idl` accepted an `Anchor.toml` or a `programs/`
parent directory. A repo with one crate at `program/` and its IDL committed at
the root `idl/` matched neither, so the walk stopped before it could look one
directory up. Cargo's own `[workspace]` table now also marks the root.

## 3. The bootstrap outcome was optimistic by default

`ProbeOutput::envelope` set `passed_with_coverage` with a comment claiming
every real construction site overwrites it. `run_bootstrap` did not: it spread
the envelope and inherited the default. An empty work list therefore read as
"probed, nothing found" when the truth was "nothing was probed".

`run_bootstrap` now reports `no_findings_low_coverage` when it discovered no
handlers, and the envelope default is the weak claim, so a construction site
that forgets to set the outcome under-reports coverage instead of asserting a
clean bill of health it never established. The schema already documented this
contract: only `passed_with_coverage` licenses "found nothing".

Defect 3 is what made 1 and 2 silent.

## Measured

| Target | Before | After |
|---|---:|---:|
| Pinocchio program, crate dir | 0 handlers | 14 handlers |
| Second Pinocchio program | 14 via IDL | 14 via source |
| Anchor program | 39 handlers | 39 handlers |
| Empty project outcome | `passed_with_coverage` | `no_findings_low_coverage` |

The two Pinocchio programs previously failed and succeeded by opposite
mechanisms: one would have been found by source discovery but was blocked by
defect 2, the other used the bare-`process` convention and was carried by its
IDL. Fixing either defect alone would have left the other program broken.

## Tests

Five regression tests: four in `pinocchio_bootstrap_tests` (prefixed
convention, bare-`process` convention, dispatcher exclusion, empty-list
outcome) and one in `idl_overlay` for the `[workspace]` marker. Every
Pinocchio fixture puts the dispatcher in `entrypoint.rs` rather than
`lib.rs`, which is the layout that caused the original failure.

Suite: 1595 passed, 0 failed.

## Release gates

All gates in `docs/RELEASING.md` ran green except step 6,
`check-lake-build.sh --strict`, which was skipped deliberately. This release
touches `crates/qedgen/src/probe/` only. It changes no Lean emission, no
codegen and no MIR path, so the Lake sweep had no reachable failure mode to
catch. Steps 1, 1a, 2, 3, 4, 5, 7, 8a, 8b and 9 all ran and passed.

## Known gaps, not addressed here

The same sweep found two further issues, both untouched by this release:

- `engine_runs` is empty on every spec-less audit, including Anchor targets
  that resolve handlers correctly. No engine ran anywhere.
- `verify --probe-repros` refuses to run without a resolved `.qedspec`, so it
  is unusable in the spec-less brownfield audit it exists to serve.
