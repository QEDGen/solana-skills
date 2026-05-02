# Release v2.16.0 — Structured counterexamples + Mollusk sandbox infrastructure

v2.16 closes the aspiration/existing gap on Kani + proptest
counterexamples, ships the Mollusk-backed sandbox crate, and lands
the auditor reproducer-only contract. PLAN-v2.16's D3 (per-probe
repro generators) is **deliberately deferred to v3** — the
mechanical-template approach was replaced mid-design by an
agent-authored repro architecture that fits the broader v3
surface-area cleanup. v2.16 ships the **infrastructure** the agent-fill
workflow runs on; the auditor subagent already operates end-to-end
on top of it without D3.

## What's in

### Structured counterexamples — D1 + D2

`qedgen verify --proptest` and `qedgen verify --kani` now parse
backend output into uniform `(harness, var, value, line)` tuples
attached to `BackendReport.counterexamples[]` in the JSON output. The
existing `detail` field stays for human consumption.

- **D2 — proptest parser** (`verify_proptest_parse.rs`): walks
  libtest's failure block, extracts the panic location, message
  (after `Test failed:`), and brace-balanced `name = value`
  assignments from the `minimal failing input:` line. Reads the
  persisted seed from `proptest-regressions/<harness>.txt` so the
  user can re-run deterministically with `PROPTEST_REGRESSION_FILE=`.
  7 fixture-based unit tests.
- **D1 — Kani CBMC parser** (`verify_kani_parse.rs`): walks
  `Checking harness ...` blocks bounded by `VERIFICATION:- FAILED`,
  extracts the Description / Location of the first FAILURE check, and
  reads `<var> = <value>` lines under each `State N:` header (with
  `line` populated from the State location). Strips CBMC's verbose
  binary-rep tail. 8 fixture-based unit tests.
- **Shared types** (`verify_counterexample.rs`): `Counterexample`,
  `CounterexampleVar`. Same shape for both backends; consumers
  (auditor, JSON pipelines) pin against this canonical model.

### Mollusk sandbox + verb — D4

- **New crate `qedgen-sandbox`** (workspace member, **not** a `qedgen`
  CLI dependency — Mollusk + Agave + Solana SDK are isolated to this
  crate per PLAN-v2.16's "keep dep surface bounded"). Re-exports
  `Mollusk`, `InstructionResult`. Provides `Sandbox::for_program`,
  `Sandbox::invoke`, `Sandbox::invoke_with_system`. Pinned to
  `mollusk-svm = 0.12.1-agave-4.0`.
- **New CLI verb `qedgen verify --probe-repros`** walks
  `<project>/target/qedgen-repros/` (ephemeral; never committed),
  runs each repro test, and emits per-finding pass/fail. Maps cargo
  test outcomes to `Fired` (bug reproduced — finding stays), `Silent`
  (assertion failed — finding suppressed silently), `BuildError`
  (insufficient evidence — finding stays structural). Supports both
  shared-crate and per-finding-crate layouts.

### Auditor reproducer-only contract — D5

`.claude/skills/qedgen-auditor/SKILL.md` updated with the v2.16
requirement: every CRIT/HIGH finding must be backed by a Mollusk
repro that fires.

- New top-level "Reproducer-only contract (v2.16)" section.
- New step 5 in the workflow: write
  `target/qedgen-repros/audit/<finding-id>.rs`, run
  `qedgen verify --probe-repros --json`, gate on `status`.
- Per-finding output format gains a "Reproducer (CRIT/HIGH only)"
  subsection with status / test path / invocation / observed.
- Digest format extended with per-finding repro status (`fired` /
  `inconclusive`) and a `n silent-repro` count surfacing how many
  CRIT/HIGH candidates were dropped because their attack didn't
  reproduce.
- "Don't run Lean / Kani / proptest" rule clarified to exempt
  ephemeral Mollusk repros under `target/qedgen-repros/audit/` —
  those are gating tests, not opt-in verification artifacts.

The auditor IS the agent: it writes the Mollusk repros directly via
the harness's Write tool, runs them via `qedgen verify --probe-repros`,
and surfaces only fired findings. No new codegen path required.

### Probe contract — Reproducer field + drop-on-fail

`probe.rs` schema bumps to `version: 2`. New `Reproducer` enum on
`Finding` (`Kani` / `Proptest` / `Sandbox` variants) — optional
during the v2.16-to-v3 transition. Spec-aware mode now runs every
candidate finding through `probe_repro::construct_reproducer`; on
`Err(ConstructFailure::*)` the candidate is silently dropped. No
advisory tier per `feedback_probes_reproducible_only.md`. Until v3's
agent-fill workflow lands, every constructor returns
`NotImplemented` and the spec-aware probe correctly emits zero
findings — the user-visible behavior the contract demands. Bootstrap
mode is unchanged (it never emitted findings).

### Bonus — wrapping/saturating arithmetic lints

`qedgen check` now flags `+=?` / `-=?` (wrapping) and `+=!` / `-=!`
(saturating) effect operators as `wrapping_arithmetic` (Warning,
priority 1) / `saturating_arithmetic` (Info, priority 2) lints. This
is a fast-path structural advisory at check time — companion to (not
replacement for) the probe finding of the same shape. The lint says
"you opted in"; the probe finding (once v3 ships agent-fill repros)
says "here's the reproduced state corruption."

### Cleanup

- `qedgen probe --json` was a no-op flag (probe always emits JSON).
  Removed from the CLI surface.

## What's NOT in (deferred to v3)

### D3 — Per-probe repro generators

Replaced by **agent-authored repros via structured prompts**. PLAN-v2.16
originally sketched per-`Category` Rust template constructors; design
review concluded the mechanical portion of a probe repro is ~10-15%
of the file (Cargo.toml header, `#[test]` shell, sandbox setup), and
adding per-category template paths would compound the existing
codegen-bug surface (see `project_v252_eval_roaster.md`,
`project_lean_codegen_bugs_v2_12.md`,
`project_quasar_codegen_program_unit.md`). v3 will land the
agent-fill design alongside the broader surface-area cleanup. See
`feedback_repros_agent_authored.md` and
`feedback_cleanup_v3.md`.

The end-to-end multisig duplicate-signer demonstration originally
listed in PLAN-v2.16's success criteria moves to v3.

### GH #38 — Lean liveness vacuous-on-aborts

Carried from v2.15.1. Proof generator must emit existence form
`∃ ops s', applyOps s signer ops = some s' ∧ s'.status = .To` (with
witness construction or `sorry` placeholder), not the trivially-true
implication form. v2.16 didn't get to this; tracked for v2.16.1 or
v3.

## Schema changes

- `qedgen probe` JSON output: `version: 2` (was 1).
  `Finding.reproducer: Option<Reproducer>` field added (omitempty —
  v1 consumers continue to work). Spec-aware findings without a
  reproducer are silently dropped.
- `qedgen verify --proptest --json` / `--kani --json`:
  `BackendReport.counterexamples: Vec<Counterexample>` added
  (omitempty when empty). Existing `detail` field unchanged.
- `qedgen verify --probe-repros --json`: new report shape
  `{ repros_dir, results: Vec<{ finding_id, status, log_excerpt? }>,
  duration_ms, note? }`.

## Gates

- 508 + 24 unit tests pass (was 484 + 24)
- `cargo fmt --check`, `cargo clippy --release -- -D warnings`,
  `bash scripts/check-version-consistency.sh`,
  `bash scripts/check-readme-drift.sh`,
  `bash scripts/check-lake-build.sh --strict` (10 / 10 examples build) clean
- `qedgen-sandbox` builds independently (Mollusk + Agave isolated;
  CLI dep graph unchanged)
- `qedgen verify --probe-repros --json` end-to-end smoke clean
  (returns `note: no repros found` placeholder pre-D3)
- **End-to-end pipeline validated against an external Quasar program.**
  The auditor (D5) walked an arms-length example program, identified
  a CRIT-severity bug, wrote a Mollusk-driven repro using the
  framework's typed ser/de helpers, and confirmed the repro fires
  deterministically against the compiled `.so` via cargo-build-sbf +
  Mollusk. The finding is being handled via responsible disclosure
  to the upstream maintainer; specifics are intentionally not in
  these notes per the auditor SKILL's third-party-disclosure rule.
- Zero unintended `sorry` in `examples/**/*.lean` (CLAUDE.md filter
  applied; matches are all macro doc-comments / template strings, not
  proof tactic positions)
- `qedgen check --frozen` lock-currency check passes for all 5
  bundled spec dirs (no `stale (--frozen)` messages emitted).
  **Carryover:** multisig spec has 1 pre-existing
  `excluded_op_modifies_property` warning predating v2.15.1 — the
  lint flags that `approve` modifies `approval_count` (used in
  `votes_bounded`) but is excluded from `preserved_by`, so the
  inductive theorem arm emits `sorry` for that case. The warning
  shipped in v2.15.1 (which used `--regen-drift`, not `--frozen`,
  as its release gate). Tracked for follow-up; not introduced by
  v2.16.

## Migration

- Consumers pinning `qedgen probe`'s `version: 1` need to update to
  `2` to pick up the optional `reproducer` field. Findings emitted
  pre-v2.16 won't carry `reproducer`; nothing serializes it today
  since constructors return `NotImplemented`. Plain field addition,
  no existing field shape changed.
- Auditor SKILL consumers (the audit subagent) should adopt the new
  reproducer-only contract for CRIT/HIGH findings. No structural
  change; existing audits still work, but findings without a
  Mollusk-fired repro should now be suppressed.
- `qedgen probe --json` is gone. The flag was a no-op (probe always
  emitted JSON) so passing it produced silent success previously;
  scripts that pass it will now fail with clap's "unknown argument."
  Remove the flag from invocations.
