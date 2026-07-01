# QEDGen v2.38.0 — model refresh (Leanstral v1.5, auditor Fable 5 / Opus 4.8 / GPT-5.5)

**Status:** release-prep (on `chore/release-v2.38.0`). **Theme:** point the two
external-model seams at their current-generation defaults, plus the brownfield
crucible fixes already merged to `main` since v2.37.0.

Everything since v2.37.0 (#136–#137 + the model-default updates below). This is a
small config/maintenance release — no CLI surface change, no generated-output
change beyond the version-tag re-stamp.

## What shipped

### 1. Leanstral default model → `labs-leanstral-1-5`

`qedgen fill-sorry` / `generate` (the Lean sorry-filling dispatch) now target
Mistral's **Leanstral v1.5** (`labs-leanstral-1-5`, released 2026-06-30 — 119B
total / 6.5B active, 256k context), up from `labs-leanstral-2603`. Single
constant in `crates/qedgen/src/dispatch/api.rs`; both the `fill-sorry` and
`generate` paths consume it. Still a Labs-tier model (free), so the existing
org-enablement error path (`api.rs`) is unchanged.

### 2. Auditor default-model recommendation refresh

`skills/qedgen-auditor/SKILL.md` "Recommended model + reasoning budget" now reads:

- **Claude Fable 5** — preferred on Claude Code (newest flagship).
- **Claude Opus 4.8 with extended thinking** — fallback on Claude Code when
  Fable 5 isn't available. Both are lifted by the `ultrathink` UserPromptSubmit
  hook installed alongside the skill.
- **GPT-5.5 in high-reasoning mode** — Codex / Cursor / other agent-skills
  harnesses (budget set manually).

`hooks/README.md` updated to name Fable 5 / Opus 4.8 (was Opus 4.6 / 4.7) as the
models the `ultrathink` keyword lifts. Hook mechanism itself is unchanged and
model-agnostic. Supersedes the earlier Opus 4.7 xhigh recommendation.

### 3. Brownfield crucible (merged to `main` since v2.37.0)

- **#136** — brownfield Anchor fuzz fires end-to-end; the auditor gains a
  Crucible preflight. In-program SBF faults surface as tx-errors (not host
  panics), so the crash-first lane reads them correctly; Anchor brownfield now
  reads IDL accounts (falls back to a committed `idl.json`).
- **#137** — a brownfield demo fixture that models a real PDA-vault drain.

### 4. Supply-chain

- `anyhow 1.0.102 → 1.0.103` (Cargo.lock only; **RUSTSEC-2026-0190**,
  `Error::downcast_mut()` unsoundness). Fixed by version bump, not an ignore
  entry — `deny.toml` / CI `--ignore` lists are unchanged.

## Compatibility notes

- No CLI change, no `.qedspec` DSL change, no change to generated code / proofs
  beyond the `qedgen-macros` version-tag re-stamp (`v2.37.0 → v2.38.0`) across the
  6 codegen snapshots + 8 bundled-example `Cargo.toml` pins.
- The Leanstral default moves to a Labs model that must be enabled for the
  caller's Mistral org (same as before); `MISTRAL_API_KEY` unchanged.

## Gates (RELEASING.md)

- Version bumped in `Cargo.toml` + `package.json`; `check-version-consistency.sh` clean (2.38.0).
- Version-pinned artifacts re-stamped (`UPDATE_SNAPSHOTS codegen_snapshot` + `qedgen check --regen-drift --write`); diff is **tag-line/version-line only** across the 6 snapshots + 8 example `Cargo.toml`.
- `cargo fmt --check` ✅ · `cargo clippy --all-targets -- -D warnings` ✅ · `cargo test` ✅ (17 suites green).
- `check-readme-drift.sh` ✅ (21 commands) · `qedgen check --regen-drift` clean (8 examples) · `qedgen check --frozen` clean (no stale locks; nonzero exits are pre-existing P2/P3 lint, not lock drift).
- Zero unintended `sorry` in generated example proofs (only matches are codegen-helper DSL / docstrings in the vendored support lib — identical to v2.37.0).
- Supply-chain: `cargo audit --deny warnings` (CI `--ignore` set) and `cargo deny check` both exit 0 after the anyhow bump.
- Example `lake build` (`check-lake-build.sh`) unchanged — no `.lean` / `lean_solana` / qedsvm-pin change in this release; CI gate re-runs on the release PR.
