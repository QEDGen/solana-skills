# QEDGen v2.36.0 — v3.0-prep internal reorg + `check --explain --json`

**Status:** shipped (on `release/v2.36.0`). **Theme:** the v3.0-prep cleanup
release. The flat 75-file `crates/qedgen/src/` is reorganized into directory
modules by pipeline stage, and the largest monoliths are split into
per-concern submodules — all **move-only, zero behavior change** (codegen
output is byte-identical to v2.35.0; every split was gated against the four
snapshot suites). One additive user-facing change rides along: `check
--explain` now honors `--json`. No spec-syntax or CLI-surface breaks.

## What shipped

### 1. `src/` reorg into directory modules (PR #101)

72 flat files → 9 pipeline-stage directory modules (`spec/`, `mir/`,
`codegen/`, `check/`, `probe/`, `adapt/`, `verify/`, `dispatch/`, `project/`).
Root re-exports in `main.rs` keep every `crate::<module>` path stable;
`include_str!` paths bumped one level where needed. Snapshots byte-identical.

### 2. Monolith splits into per-concern submodules (PRs #104, #105, #107, #109–#117)

Each large module was carved into a directory module with a `mod.rs` facade
that re-exports the submodule surface, so all `crate::<module>::<name>` paths
resolve unchanged. Move-only, snapshot-gated:

- `check/mod.rs` 11,294 → 36-line facade + 8 submodules (#104); `lints.rs`
  → `lints/{arithmetic,cpi,state,auth,structural}` (#107); test colocation +
  helper privatization (#109/#110/#111).
- `codegen_shared.rs` + `kani_impl.rs` → directory modules (#105).
- `spec/chumsky_adapter.rs` (#112) and `spec/chumsky_parser.rs` (#113) — the
  whole parser front-end modularized.
- `adapt/pinocchio_profile.rs` (#114).
- `main.rs` **3,526 → 71 lines** (#115): the binary crate root keeps the
  module tree + `crate::<module>` re-export hub + `fn main`; the CLI surface
  moves to `cli.rs` (clap arg defs), `run.rs` (`command_name_of` + `dispatch`),
  and `run_helpers.rs` (dispatch glue).
- `codegen/rust_codegen_util.rs` (#116) → `{guards,pubkey,expr,property,effect,emit}`.
- `codegen/kani_mir.rs` (#117) → `{driver,prefix,account,guards,preservation,conformance}`.

### 3. Shared `ProgramAdapter` traits (PR #103)

Extracted shared codegen helpers behind `ProgramModel` / `VerifyBackend` /
`FrameworkCodegen` traits with `ProgramAdapter` routing (Anchor / Pinocchio /
native). Adapter detection pins to Anchor when `anchor-lang` is in
`Cargo.toml`, so a malformed Anchor surface raises the parse error rather than
silently falling back to a native skeleton.

### 4. `check --explain --json` (PR #106) — the one additive change

`check --explain` was the only `check` sub-mode without a `--json` branch, so
verification status was Markdown-only. `Status` / `PropertyStatus` now derive
`Serialize` and `--explain --json` emits `{summary, properties}`; SKILL.md
points the agent at the structured form (Markdown kept as the human fallback).
Backward-compatible: the flag already existed, this fills the gap.

### 5. Terse-comment pass (PR #102)

A comment-only sweep (~6.15k net lines removed) across 73 files;
constraint/footgun comments cited by CLAUDE.md/references by file:line were
kept. (A comment pass is not clippy-safe — removing in-branch comments
unmasked `if_same_then_else` / `doc_lazy_continuation`; those were fixed in
the same PR.)

### 6. Release-time tooling fix

`scripts/check-readme-drift.sh` scanned `main.rs` for the `Commands` enum and
matched only a bare `enum Commands` — after the #115 split the enum lives in
`cli.rs` as `pub(crate) enum Commands`, so the gate had started passing
vacuously ("0 commands"). Repointed at `cli.rs` and taught it the visibility
prefix; it again checks all 19 commands.

## Compatibility notes

- No spec-syntax or CLI-surface breaks; codegen output is byte-identical to
  v2.35.0 (the snapshot suites prove it).
- `check --explain --json` is additive.
- Generated `Cargo.toml` pins move to `tag = "v2.36.0"`.

## Gates

`cargo fmt --check`, `cargo clippy -- -D warnings` (workspace), `cargo test`
(1006 unit + all snapshot suites byte-identical), `check-readme-drift.sh`
(19 commands, after the script fix), `qedgen check --regen-drift` (8 examples,
only the tag line drifted), `qedgen check --frozen` per bundled example, and
the `cargo audit` / `cargo deny` supply-chain gates all pass. No new RustSec
advisories (the five ignored IDs unchanged; `cargo deny` notes that
`RUSTSEC-2026-0097` no longer matches any crate — a benign stale-ignore
warning, not a failure). Zero unintended `sorry` in generated example proofs
(the support-library `QEDGen.Solana` matches are comments + Tier-0
string-templates, unchanged since v2.35.0). The strict lake gate is covered by
the green `lake-build.yml` run on `main` — this release changed no Lean
(only `Cargo.toml` tag pins + the readme-drift script), so Lean output is
identical to v2.35.0.

Checklist quirk (pre-existing, not a blocker): `qedgen check --frozen --spec
<dir>` fails on `cross-program-vault` because the sibling-glob picks up
`imports/**` fragments (`spec AdminConfig`) as siblings of `spec Vault`; the
file-form invocation passes. Identical behavior on the v2.35.0 tree (issue
#100).
