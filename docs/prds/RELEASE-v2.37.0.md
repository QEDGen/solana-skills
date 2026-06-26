# QEDGen v2.37.0 — the qedsvm discharge seam (sBPF bridge `.refines` proven against pinned bytes)

**Status:** release-prep (on `chore/release-v2.37.0`). **Theme:** close the
"qedgen names the bytes but doesn't prove them" gap for the first handler shape —
the spec→byte-level proof chain now runs end to end and the generated sBPF-bridge
`.refines` is sorry-free.

Everything since v2.36.0 (#119–#134). Headline is the discharge seam; the rest is
codegen-refactor + lint follow-ups.

## What shipped

### 1. The qedsvm discharge seam — spec → pinned bytes (Slice A)

The bundled CPI-callee `ensures` and the sBPF refinement bridge were *axiomatized
against a `binary_hash` pin* — qedgen named the bytes, but didn't prove they honor
the contract. Slice A closes that for a single-field constant-increment handler
(`field += <int literal>`), end to end:

- **Descriptor producer** (#125–#127) — `qedgen descriptor` emits a name-level
  refinement descriptor (JSON); `qedgen discharge` chains it through qedsvm's
  `qedlift` and reports a verdict against the decoded program bytes (offsets
  resolved IDL-side). Schema v1 (`add_const`) + v2 (`add_param`, arbitrary literals).
- **A1 — ELF cache** (#130, `verify/upstream_check.rs`) — a verified
  `--check-upstream` match content-addresses + stashes the program bytes so the
  discharge tactic has them without a second fetch.
- **A2a — discharge persist** (#132) — `qedgen discharge --out-dir` writes the
  discharged `<Module>Refinement.lean` + `<Module>TracedLifted.lean` into the
  project instead of a temp dir.
- **A2b — the Bridge↔qedlift adapter** (#134) — the `qedbridge` `.refines`
  elaborator now emits the *provable* shape (threads the discharge
  `AsmRefinesFieldUpdate` + the `cr.SatisfiedBy` program constraint as hypotheses
  instead of quantifying over a free `progAt`), discharges the halt through a
  proven execution adapter (`BridgeAdapter.lean`), and closes the post
  `codecCoarse → encodeState` leg via qedsvm#48's `CodecRead.lean` family. The
  generated `Vault.Bridge.increment.refines` is **sorry-free** (`#print axioms`
  = `propext / Classical.choice / Quot.sound`, no `sorryAx`).
- **qedsvm pin** — `v0.6.0 → v0.7.0` (#133) → `v0.8.0` (this release, ships #48's
  `CodecRead.lean`), in `lean_solana/lakefile.lean` + both manifests.

Boundary (still axiomatized / `sorry`, by design): the abort path (`.rejects`,
gated on qedsvm#40), `decode_encode` (codec round-trip), and any handler shape
beyond single-field constant-increment. Design: `docs/design/qedsvm-discharge.md`,
`docs/design/a2b-handoff.md`.

### 2. Codegen + lint follow-ups

- `lean_gen_mir.rs` split into a per-concern directory module (#122); `anchor_adapt.rs`
  likewise (#120) — continues the v3.0-prep monolith breakup.
- Per-arm ADT branch rendering on the Lean side (#121) — unparks the #66 Lean follow-up.
- `check`: exclude import-dependency subtrees from the multi-file sibling sweep
  (#119, fixes #100).
- Dep bump `quinn-proto 0.11.14 → 0.11.15` (RUSTSEC-2026-0185).

## New CLI

- `qedgen descriptor` / `qedgen discharge` — the producer + driver of the seam
  (experimental). Documented in [`references/cli.md`](../../references/cli.md#discharge-experimental--the-qedgen--qedsvm-seam).

## Compatibility notes

- qedsvm pin moves to **v0.8.0**. Programs whose `formal_verification/` vendors an
  older qedsvm are unaffected (examples pin their own version); only the top-level
  `lean_solana` / `lean_solana_mathlib` packages bump.
- `descriptor` / `discharge` are additive and experimental — no change to existing
  commands or generated output beyond the version-tag re-stamp.

## Gates (RELEASING.md)

- Version bumped in `Cargo.toml` + `package.json`; `check-version-consistency.sh` clean (2.37.0).
- Version-pinned artifacts re-stamped (`UPDATE_SNAPSHOTS codegen_snapshot` + `qedgen check --regen-drift --write`); diff is **tag-line-only** across the 6 snapshots + 8 example `Cargo.toml`.
- `cargo fmt --check` ✅ · `cargo clippy -- -D warnings` ✅ · `cargo test` ✅ (1022+ unit/integration; mir/kani/codegen/proptest snapshot suites green).
- `check-readme-drift.sh` ✅ (21 commands) · `qedgen check --regen-drift` clean · `qedgen check --frozen` clean (no stale locks).
- Zero unintended `sorry` in generated example proofs (the only matches are codegen-helper occurrences in the vendored support lib — `mkSorryTheorem`, Bridge DSL — identical to v2.36.0).
- Supply-chain (`cargo audit` / `cargo deny`) green on the merged seam PRs; deps unchanged by the version bump (CI re-runs on the release PR).
- Example `lake build` (`check-lake-build.sh`) unchanged from v2.36.0 — the examples are independent of the top-level `lean_solana` bump (their `formal_verification/` vendors its own pinned qedsvm); CI gate re-runs on the release PR.
