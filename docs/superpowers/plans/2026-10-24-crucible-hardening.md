# Crucible Hardening Implementation Plan

> **For agentic workers:** Use the parallel-agent workflow with isolated worktrees and task-scoped ownership. Use regression-first implementation and independent review.

**Goal:** Make Crucible findings and empty results accurately reflect executed checks and expose incomplete harnesses.

**Architecture:** Retain codegen, runner, replay and CLI layers. Add explicit generation/readiness metadata and freshness validation, then connect these to the existing probe envelope. Separate heuristic protocol alerts from semantic counterexamples.

**Tech Stack:** Rust, serde, generated Crucible Rust harnesses, shell/GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-24-crucible-hardening-design.md`

## Global Constraints

- Preserve user changes, existing runtime boundaries, and manual harness edits.
- Do not commit, push, publish or open issues/PRs.
- Use GPT-6.1 Sol workers, three parallel seats maximum, isolated worktrees.
- Tests must exercise behavior; generated-text assertions alone do not establish live correctness.
- Unsupported coverage is explicit. Do not invent success counts, deserialization, or program semantics.

## Review Focus

- Nonzero process exit with an old crash directory must not pass.
- A truncated crash file must be surfaced, not silently dropped.
- A balance assertion over an unchanged shadow must not be credited as executable semantic coverage.
- A legitimate withdrawal or initialized realloc tail must not be reported as a confirmed vulnerability.
- Reordered handlers/new bindings with a reused harness must not replay seeds against stale action layouts.

## Task 1: Runner and evidence integrity

**Owner:** runner worker. **Files:** `crates/qedgen/src/probe/crucible_probe.rs`, `crucible_replay.rs`, runner-specific tests. Parent integrates CLI changes.

- [ ] Add failing regression tests for abnormal exit without crashes, old crashes, malformed metadata, and replay timeout/error distinction.
- [ ] Make run/harvest errors explicit with actionable context; retain supported crash exits only when fresh evidence establishes a crash.
- [ ] Preserve bounded triage, surface incomplete replay honestly, and classify broad protocol guard firings as heuristic alerts rather than confirmed vulnerabilities.
- [ ] Design coverage transport using actual runtime output only; coordinate generated marker requirements with the codegen worker and parent.
- [ ] Run `cargo test -p qedgen-solana-skills --bin qedgen crucible` and report results plus required CLI hook changes.

## Task 2: Generated harness readiness and check quality

**Owner:** codegen worker. **Files:** `crates/qedgen/src/codegen/crucible_gen.rs`, `crates/qedgen/templates/crucible-snapshot-infra.rs`, focused new tests/modules as needed. No edits to `run.rs` or runner files.

- [ ] Write regressions for stale shadow assertions, unfilled accounts, unsupported seeds and fixed compound arguments.
- [ ] Emit a machine-readable readiness/limitations artifact with a public validation API usable by the parent. Domain mode must fail clearly if semantic assertions depend on fields not synchronized from observed state; distinguish supported accepted-input mirrors.
- [ ] Add truthful action execution counters/summary markers if supported by Crucible lifecycle; communicate the exact contract before integrating.
- [ ] Correct provably unsound generic guard assumptions (especially realloc-tail writes and non-Anchor discriminators), retaining useful leads with honest heuristic semantics.
- [ ] Test supported primitive/option inputs and domain-boundary fixture compatibility. Keep complex defaults only with explicit coverage limitations.
- [ ] Run focused unit tests and provide parent hook instructions.

## Task 3: Artifact freshness, dependency pin and live gate

**Owner:** artifacts worker. **Files:** new `crucible_artifacts` module, dedicated integration tests/workflow/scripts. Parent owns `run.rs`, module registration, and dependency-string integration.

- [ ] Add fingerprint recording/checking APIs for generation inputs. Source edits must be preserved; stale spec/mode/IDL/binding inputs must be rejected before replay. Legacy harnesses require actionable explicit regeneration rather than silent acceptance for domain claims.
- [ ] Test unchanged reuse, changed spec/bindings/IDL, and manual harness edits.
- [ ] Find a verifiable compatible Crucible revision and add a dedicated generated-harness live CI gate using that revision for CLI and libraries; provide the parent the required dependency change.
- [ ] Exercise protocol-negative/domain-positive behavior and require live opt-in tests to execute in the gate.
- [ ] Run relevant tests and report evidence/blockers without claiming unexecuted validation.

## Task 4: Integration, review and explanation

**Owner:** parent, with fresh GPT-6.1 Sol review workers after implementation seats finish.

- [ ] Integrate worker changes without commits; connect readiness/freshness/coverage APIs in `run.rs` and expose accurate probe output.
- [ ] Review every task for spec compliance and code quality, resolve conflicts and run focused unit/integration suites.
- [ ] Run live protocol/domain regression and formatting/clippy checks appropriate to changes. Broaden to the full Rust suite if focused checks pass.
- [ ] Update user-facing limits/docs and plan progress with exact evidence.
- [ ] Explain how fuzz inputs and sequences expose violated invariants/properties, using the verified fixture and the new reporting behavior. State remaining limits.

## Verification record (2026-10-24)

- The pinned live gate `bash scripts/crucible-live-gate.sh` passed its named protocol-negative/domain-positive test (1 passed). Earlier live failures exposed too-short replay deadlines and replaying `.meta.json` rather than raw crash bytes; both were fixed before the passing run.
- `cargo test -p qedgen-solana-skills --offline -q` passed, including 1,654 main unit tests and the integration groups. The codegen determinism group passed after excluding the intentionally run-specific generation-input manifest from its source snapshot. A focused follow-up test confirmed the unresolved PDA seed emits a blocking, valid-Rust stub.
- `cargo clippy -p qedgen-solana-skills --bin qedgen --offline -- -D warnings`, `cargo fmt --check`, and `git diff --check` passed.
- The three requested GPT-6.1 Sol workers implemented runner, codegen, and artifact tracks in isolated worktrees; their sessions reached usage limits before a separate final review. Parent integrated and reviewed the changes. No commit or push was made.
