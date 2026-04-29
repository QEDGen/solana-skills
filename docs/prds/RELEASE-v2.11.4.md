# QEDGen v2.11.4 — `verify` / `reconcile` config-walking + doc cleanup

**Date:** 2026-04-29
**Type:** Patch release
**Predecessor:** v2.11.3 (Lean-side hotfix + drift-hash alignment)

## Headline

A README-accuracy sweep after v2.11.3 surfaced two subcommands whose
required `--spec <SPEC>` made the documented "works from anywhere
inside a project" promise a lie, plus a handful of stale milestone
markers and undocumented-but-shipping features. v2.11.4 closes those
gaps and starts tracking release notes in-repo.

## What changed (user-facing)

### `verify` and `reconcile` now walk `.qed/config.json`

`qedgen check` and `qedgen codegen` already fall back to the spec path
written by `qedgen init --spec`. `verify` and `reconcile` did not —
their `--spec` was a required clap arg. Per memory `feedback_…` and
the README's quick-start sequence, the spec-driven pipeline is meant
to be uniform across the four subcommands. v2.11.4 adds the same
`init::resolve_spec_path` walk to both, so:

```bash
cd examples/rust/escrow
qedgen verify --lean        # works — resolves spec from .qed/config.json
qedgen reconcile --json     # works — same resolution
```

Explicit `--spec` still takes precedence.

### Stale milestone markers removed

Several user-visible strings still pointed at future-tense versions
that have since shipped:

- **`qedgen verify --kani` help** said *"cargo kani — lands in v2.4-M2"*. Kani has shipped since v2.4. Tail dropped.
- **`qedgen adapt`** error message on non-Anchor programs said *"non-Anchor (raw / native) Solana program, support lands in v2.10+"* — already past. Replaced with a timeless statement that `qedgen adapt` is Anchor-only by design.
- **Internal `codegen.rs` doc comments** referenced "lands in v2.9", "v2.5 (slice 2)", and similar artifacts of past release planning. Rewritten to describe current behavior without version pointers.
- **`references/cli.md`** carried "Changed from `./tests/kani.rs` in v2.6" / "Changed from `./tests/proptest.rs` in v2.6" change-log fossils on the path columns. Dropped — the current path is the only path that matters today.

### Undocumented features now in README

`qedgen verify --check-upstream` is a real shipping verification stage
that diffs each imported library's pinned `upstream_binary_hash`
against the on-chain `.so` via `solana program dump`. Pre-v2.11.4 it
was reachable only via `--help`; now it has a *Upstream binary
pinning* section in the README with the three flags that govern it
(`--check-upstream`, `--rpc-url`, `--offline`) and a callout for the
[Solana CLI](https://docs.solana.com/cli/install-solana-cli-tools)
dependency.

The Requirements section also names the Solana CLI as a per-feature
optional dependency (alongside Lean and the API keys).

### CI: `lake-build.yml` workflow

A new `.github/workflows/lake-build.yml` provisions elan, caches the
toolchain store + per-example `.lake/` directories (keyed on
`lean-toolchain` and `lakefile.lean` content hashes), bootstraps any
missing manifests, and runs `bash scripts/check-lake-build.sh
--strict` over all 10 bundled examples. Triggers on push to `main`
and manual dispatch only (skips PRs to avoid the 15–45 min Mathlib
cold-build tax). The Rust-side `ci.yml` workflow is unchanged.

This closes the "v2.11.3 added the gate but it's manual-only"
follow-up from v2.11.3's release notes.

### Release notes are now in-repo

`docs/prds/` was ignored wholesale; the pre-release-checklist
referenced `RELEASE-v<version>.md` as a tracked artifact but there
was no such tracking. v2.11.4 splits the gitignore: design PRDs stay
local, `RELEASE-*.md` files commit alongside the version bump.
Backfilled with v2.8 / v2.9 / v2.10 / v2.11 / v2.11.2 / v2.11.3 /
v2.11.4 release notes. (v2.11.1 was a one-line patch with no notes.)

## Compatibility

- DSL surface, parser, and `.qedspec` file format are unchanged.
- All CLI surface changes are additive: `--spec` becoming optional on
  `verify` and `reconcile` doesn't break any existing invocation.
- Internal codegen doc-comment edits change no behavior.
- Stale error messages on `qedgen adapt` and `--kani` help reword
  without changing exit codes or behavior.

## Pre-release gates (all clean)

- `cargo fmt --check` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — 457 / 457 pass (anchor_project's stale-string test
  updated to assert the new wording)
- `bash scripts/check-version-consistency.sh` — `2.11.4` consistent
- `bash scripts/check-readme-drift.sh` — clean
- `bash scripts/check-lake-build.sh` — **10 / 10 examples** (and now
  also exercised via the new GH Actions workflow on push-to-main)
- `qedgen check --regen-drift` — 5 / 5 examples in sync
- `cargo check` per bundled example — clean

## Carried forward

- Lean coverage status flowing into `UnifiedReport::issue_count`
  causes `qedgen check --code` to exit 1 even on a clean spec when
  the bundled `Spec.lean` has stub `:= trivial` theorems with names
  that don't match `generate_properties` output. v3.0 cleanup item.
- `Target::Pinocchio` codegen still not implemented; reserved CLI
  surface only.
