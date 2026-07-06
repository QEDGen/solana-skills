# QEDGen v2.31.1 — patch

**Date:** 2026-05-31
**Type:** patch (bug fixes only — no new features, no breaking changes).

## Summary

Two real-world codegen fixes on top of v2.31.0. No DSL, CLI, or behavioral
surface changes beyond making previously-broken output correct.

## Fixes

| Item | Source |
|---|---|
| Narrow `let X = mul_div_*(...)` to u64 at the binding site (the canonical fee-math pattern compiles) | PR #68 (@0xharp) |
| Pinocchio: `Pubkey` state fields read by value (no `.get()`); guards compare against the deref'd `*ctx.<acct>.key()` | issue #71 / PR #72 |
| Pinocchio: effect body binds account refs through `self` (the handler method), not the guard fn's `ctx`; `Pubkey` set assigns the deref'd value (no `.into()`) | issue #71 / PR #72 |
| Kani impl-targeted harness: integer handler-param PDA seeds serialize via `to_le_bytes()` (not `u64::as_ref()`) | issue #71 / PR #72 |
| `qedgen setup`: embed `CommandBuilders.lean` (the embedded `Spec.lean` imports it) so the validation workspace `lake build`s | issue #71 / PR #72 |
| Anchor: a state type named like a prelude wrapper (e.g. the perp-dex example's `type Account = { … }`) no longer makes `Account<'info, _>` an ambiguous glob import — codegen re-imports the colliding wrapper explicitly. Fixes the perp-dex scaffold-compile CI failure (a pre-existing red on `main` from `ambiguous_glob_imports` going deny-by-default) | PR #72 |

## Regression coverage added

- `config-pubkey` Pinocchio compile-gate fixture (Pubkey state + account-key
  effect/guard) — verified `cargo build`s, wired into `smoke_pinocchio_scaffold`.
- Unit tests: Pinocchio Pubkey guard/effect shape, integer-param seed,
  embedded-module import-closure (fails fast if the Lean embed list drifts).
- Unit tests: Anchor lib disambiguates a prelude-colliding state type, and
  (scoped) emits nothing extra when there's no collision.

## Known follow-ups (tracked for the v2.32 minor)

Surfaced by issue #71 — all deferred to v2.32:

- **State-account resolution** can't resolve a PDA-named state account
  (e.g. `hub_authority` holding `HubAccount`); the init effect bails with a
  `TODO` breadcrumb instead of emitting the writes.
- **Bundled-example support-dir regen** — `bundled-stdlib-demo`'s
  `lean_solana/QEDGen/Solana/` is missing `CommandBuilders.lean` (latent until
  something imports `QEDGen.Solana.Spec`). Re-run codegen + `check-lake-build
  --strict` on the bundled examples.
- **Quasar/Anchor R28 PDA-seed check** has the same param-vs-state-field seed
  ambiguity as the Kani path — audit `codegen.rs`.
