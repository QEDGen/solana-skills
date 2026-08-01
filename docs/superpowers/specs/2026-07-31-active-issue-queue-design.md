# Active Issue Queue Design

## Scope and ordering

This change implements the active QEDGen issue queue in the agreed priority
order on one branch and in one pull request:

1. #398 — source/IDL reconciliation in `readiness` and `check-upgrade`
2. #399 — canonical vulnerability-category identity
3. #395 — compile the complete `codegen --all` artifact set
4. #401 and #402 — benchmark schemas and difficulty-tier scoring
5. #400 — machine-checkable auditor knowledge-base provenance

#341 remains open and blocked on Crucible. #87 is deferred. #123 is a
non-blocking roadmap tracker.

Each workstream must be independently testable and land as a separate commit.
The final pull request closes only the issues implemented by this branch.

## #398: source-aware deployment gates

`readiness` gains an optional `--root <project>` argument. `check-upgrade`
gains the same argument, applied only to the candidate (`--new`) side. Without
`--root`, reports and exit codes remain byte-for-byte compatible with current
behavior.

With `--root`, QEDGen reuses the existing source-handler discovery and IDL
overlay rather than creating a second parser. Source-only handlers become
QEDGen-owned unsafe findings in the Ratchet report. IDL-only instructions are
recorded as stale interface entries, and P-rule findings attached solely to
those instructions are clearly annotated or demoted so they are not presented
as live source risks.

The CLI-to-verifier boundary carries an optional project root. A focused
adapter translates overlay drift candidates into Ratchet diagnostics. Tests use
the existing `probe-corpus/specless/anchor-idl` fixture and cover both drift
directions, the no-`--root` compatibility path, JSON output, and exit codes.

## #399: one category identity

The Rust `Category` enum and `Category::tag()` become the only place that can
mint a probe category tag. `applicable_categories()` returns typed categories;
string conversion occurs only at the serialization boundary.

The current Pinocchio split is preserved because the two work-list entries and
probe references distinguish amount arithmetic from lamport arithmetic.
Accordingly, the collapsed `PinocchioUncheckedArith` identity is split into
typed amount and lamport variants, and producer sites choose the appropriate
variant. Existing checked-in finding expectations are migrated deliberately.

A Rust unit test proves every applicable category round-trips through the
canonical tag set. `scripts/check-auditor-skill.sh` compares the canonical
category surface with the Markdown catalog. Explicit, reviewed allowlists cover
categories intentionally limited to model guidance or probe machinery; raw
accidental drift fails CI.

## #395: compile the shipped `--all` invocation

The generated-artifact gate adds one Anchor case that invokes `codegen --all`
once and runs `cargo check --tests`. It does not execute the Parallax scaffold,
because that scaffold requires a built SBF program. Existing tests that
generate narrower artifact sets and execute unit/proptest output remain
unchanged.

The new case asserts the documented file set before compilation. This catches
flag-interaction, manifest-upsert, emission-order, and silent-skip failures in
the exact invocation users are told to run.

## #401 and #402: benchmark contracts and comparable scoring

The auditor benchmark receives JSON Schemas for:

- corpus manifests;
- normalized reports;
- score reports.

The corpus schema requires an entry-level `difficulty` value from a closed,
documented vocabulary and validates repository identity, audited commit,
program root, runtime, setup/test commands, sanitization rules, labeled
findings, and optional domain expectations. A synthetic manifest and report
fixtures exercise the schemas through a portable validation script.

The benchmark skill requires recall and precision to be reported separately per
difficulty tier. An aggregate may be printed only beside the contributing
per-tier entry counts. Skill- and model-regression comparisons must reject or
mark invalid any run whose tier composition differs. The existing auditor
skill gate verifies that these requirements and schema artifacts remain
present.

## #400: verifiable knowledge-base provenance

Every vulnerability catalog entry has a `Basis:` field that may identify:

- a runtime/framework source repository plus path or symbol;
- a specification or documentation URL;
- a repository fixture or labeled corpus identifier.

Fixture references must resolve to an existing path. The same provenance rule
applies to actionable `Grep for:` signals in the security primer.

To avoid a low-value big-bang documentation rewrite, CI is staged:

- missing or malformed basis on newly added/modified entries is fatal;
- unresolved fixture paths are always fatal;
- historical prose-only basis and categories with no linked labeled example
  are reported as warnings initially.

The validation logic lives in a focused script invoked from
`check-auditor-skill.sh`. Fixtures test valid source, URL, and fixture bases,
plus missing and dangling references.

## Error handling and compatibility

New optional CLI behavior fails with contextual messages when a supplied root
cannot be inspected. Existing invocations without new options retain their
current behavior. Schema validation errors name the artifact and failing field.
Shell gates collect all catalog/provenance failures before exiting so one CI
run exposes the complete repair set.

No new network-dependent test is added. All schemas, fixtures, source/IDL
drift cases, and generated-artifact checks run from checked-in or generated
local data.

## Verification strategy

Every behavior change follows red-green-refactor:

1. add the narrowest failing unit, CLI, schema, or shell-gate test;
2. run it and confirm it fails for the missing behavior;
3. implement the minimum change;
4. rerun the focused test and its neighboring suite.

Before the pull request:

- run formatting and shell syntax checks;
- run focused Rust tests for Ratchet, probes, and generated artifacts;
- run the auditor skill and benchmark validators;
- run the repository’s standard CI-equivalent Rust suite where practical;
- review the final diff issue by issue;
- ensure each closing keyword corresponds to a fully satisfied issue gate.
