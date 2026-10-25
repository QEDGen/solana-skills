# Crucible Account Observation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generated Crucible harnesses verify primitive spec fields from real post-transaction Anchor account data.

**Architecture:** Parse a versioned mapping against the spec and IDL before harness generation. Feed resolved bindings into readiness and code generation; generated actions use emitted instruction account identities to read and validate LiteSVM accounts, then decode pinned IDL generated `state::*` types.

**Tech Stack:** Rust, clap, serde_json, Anchor IDL generated Rust, Crucible/LiteSVM.

**Spec:** `docs/superpowers/specs/2026-10-25-crucible-account-observation-design.md`

## Global Constraints

- Preserve all existing worktree changes; no commit or push.
- Schema version 1; primitive bool and signed/unsigned 8/16/32/64/128 bit scalars only.
- Reject unsupported IDL layout; validate full IDL discriminator and program owner.
- Mapping joins freshness fingerprint. No stale shadow may count as observed state.
- Keep accepted input mirrors distinct from stored state observations.

## Review Focus

- Handler lacking a mapped account role fails mapping validation before generation.
- Wrong owner, wrong discriminator, short data, missing/closed account: test guards and live error behavior.
- Two handlers using different roles for same field: test identity expression per action.
- Borsh field following unsupported layout: reject at validation.
- Existing harness with changed mapping: reject via fingerprint.

---

### Task 1: Resolve the mapping

**Files:** Create `crates/qedgen/src/probe/crucible_state_mapping.rs`; modify `crates/qedgen/src/probe/mod.rs`, `crates/qedgen/src/cli.rs`, `crates/qedgen/src/run.rs`.

**Interfaces:** `resolve(spec: &ParsedSpec, idl: &[u8], mapping: &[u8]) -> Result<ResolvedStateMapping>`; `ResolvedStateMapping` contains field/account type/data field/discriminator/handler role.

- [x] Write tests for valid scalar binding and invalid name, type, layout, discriminator, and role.
- [x] Run focused tests and confirm failure.
- [x] Implement parser, validation, CLI routing, and fingerprint input.
- [x] Validate the ordered scalar setup recipe against handler arguments and emit real setup calls.
- [x] Run focused tests and confirm pass.

### Task 2: Generate checked post-state reads

**Files:** Modify `crates/qedgen/src/codegen/crucible_gen.rs`; test there and in `crates/qedgen/tests/codegen_determinism.rs`.

**Interfaces:** Thread `Option<&ResolvedStateMapping>` into generation and readiness; map handler role to the same pubkey expression used by `.accounts(...)`.

- [x] Write tests for read after action, exact guards, stale flag invalidation, no effect dependency, and mirror separation.
- [x] Run focused tests and confirm failure.
- [x] Generate checked owner/discriminator/Borsh reads with a dedicated observation error diagnostic; clear flags before reading and guard assertions with flags.
- [x] Run focused tests and confirm pass.

### Task 3: Live stored-state regression

**Files:** Add a real Anchor fixture under `crates/qedgen/tests/fixtures/regressions/`; modify `crates/qedgen/tests/crucible_brownfield_smoke.rs` and docs.

- [x] Add initialization plus buggy and corrected stored-state update variants and a mapped invariant.
- [x] Run the pinned live Crucible gate and confirm buggy crash/replay identifies the invariant while corrected behavior does not.
- [x] Run `cargo test -p qedgen-solana-skills --offline` and `cargo clippy -p qedgen-solana-skills --bin qedgen --offline -- -D warnings`.
