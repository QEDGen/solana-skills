# Release v2.15.1 — example bug-fix hotfix + quorum probe wiring

Hotfix landing the May-2 GH burst (#32–#37) plus the META gap surfaced
by the v2 Quasar re-audit. #38 (Lean liveness vacuous-on-aborts) is
acknowledged but defers to v2.16 — the fix needs witness-construction
proof generation, not just statement-shape rewriting.

## Issues closed

- **#32** lending handlers don't update `pool.total_borrows`. Spec
  edit adds `pool.total_borrows += amount` (and the symmetric
  decrement on repay/liquidate). Cross-account effect lowering is a
  v2.16 codegen feature, so the user-owned handler bodies are
  hand-edited here to do the actual update with `checked_add` /
  `checked_sub`. Hashes refreshed.
- **#33** multisig `propose` permissionless. Replaced with
  `auth creator` so an external party can no longer reset proposal
  counters mid-tally. Spec + handler regenerated.
- **#34** multisig `execute` permissionless. Now takes a
  `member_index` param and enforces `state.members[i] == executor`
  alongside the existing threshold check — quorum is the
  authorization, member-binding is the gate against arbitrary
  external triggers.
- **#35** multisig `add_member` overwrites / duplicates / zero
  pubkeys. Partial fix: documented in the spec as a v2.16 DSL gap.
  The DSL doesn't yet support `Pubkey != 0` or `state.members[i] ==
  0` comparisons (numeric-literal vs Pubkey type mismatch in
  `requires`); those checks land alongside Pubkey-constant support
  in v2.16. The creator-only `auth` already constrains who can
  trigger the op.
- **#36** Anchor escrow `initialize` accepts zero amounts. Hand-edit
  in `examples/rust/escrow/programs/escrow/src/lib.rs` adds
  `require!(amount > 0, EscrowError::InvalidAmount)` /
  `require!(taker_amount > 0, ...)` plus an explanatory comment.
- **#37** is a duplicate of #29 (closed in v2.15.0). Verified the
  fix fires by writing a partial-accounts-metadata test crate; the
  proc-macro errors with the expected "missing `accounts_hash`"
  message. Closed as duplicate.
- **#38** Lean liveness theorems are vacuous when transitions abort.
  **Deferred to v2.16.** The fix requires the proof generator to
  emit witness construction for the existence form (`∃ ops s',
  applyOps ... = some s' ∧ s'.status = .To`) instead of the
  trivially-true implication form (`∀ s', applyOps ... = some s'
  → s'.status`). v2.15.1 acknowledges the issue but doesn't ship
  the proof-generator change; that lands alongside the
  counterexample plumbing in v2.16.

## Probe meta-gap closed

- Multi-actor / quorum primitive family wired into
  `probe.rs::applicable_categories` for Anchor / Native / Quasar /
  QedgenCodegen runtimes. Pre-v2.15.1 the catalog (SKILL.md /
  exploits.md) named these primitives as text but the structured
  probe output didn't list them — auditor caught the multisig
  duplicate-signer CRIT through the prose escalation rule. Now they
  surface as proper `applicable_categories` entries.

## Examples

- escrow / lending / multisig regenerated against the new spec
  inputs. spec_hash + body_hash refreshed via `qedgen check
  --update-hashes`. `qedgen check --regen-drift` clean across all 5
  examples.
- The cross-account effect lowering in lending (`pool.total_borrows`
  from the borrow handler that primarily writes loan fields) is the
  v2.16 codegen feature — for v2.15.1 the spec is correct but the
  hand-edited handler bodies provide the actual runtime update.

## Gates

- 482 + 24 unit tests pass
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `bash
  scripts/check-readme-drift.sh`, `bash scripts/check-version-
  consistency.sh`, `qedgen check --regen-drift` all clean
- The new multisig `execute` and `propose` shapes compile end-to-
  end against the Quasar codegen output

## Migration

Users with `examples/rust/{lending,multisig}/` checked out at
v2.15.0 will see drift in the regenerated harnesses; a fresh
`qedgen codegen` against the updated specs lands clean. The Anchor
escrow hand-edit is committed; users can pull it directly.
