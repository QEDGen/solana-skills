# Release v2.28.1 — version-metadata patch

CI-only patch on top of v2.28.0. No code, no DSL, no bundled-stdlib
changes. v2.28.0 shipped with `crates/qedgen/Cargo.toml` at `2.28.0`
but `package.json` still at `2.27.1` — the pre-release checklist only
called out the Cargo bump, so `package.json` was missed. The
`check-version-consistency.sh` CI gate caught it on the merge commit;
v2.28.1 syncs both files and amends the checklist so the next release
catches the gap before tagging.

## What's in

- **`package.json`** bumped `2.27.1` → `2.28.1` (and `Cargo.toml`
  goes `2.28.0` → `2.28.1`) so the version-consistency CI gate goes
  green on main.
- **`CLAUDE.md` pre-release checklist** step 1 now says "Bump version
  in BOTH `Cargo.toml` AND `package.json`" and points at
  `scripts/check-version-consistency.sh` as the local validator. The
  lowercase `claude.md` mirror stays byte-identical (same file on
  case-insensitive macOS filesystems).
- **Bundled examples' `qedgen-macros` tag pin** sed-bumped across 7
  Cargo.toml files (escrow, lending, multisig, percolator + their
  `programs/` sub-crates) from `tag = "v2.27.1"` → `tag = "v2.28.1"`.
  `codegen::render_qedgen_cargo_toml` embeds the value via
  `env!("CARGO_PKG_VERSION")`, so the `check --regen-drift` CI gate
  expects the pinned tag to match the current crate version. Same
  gap shipped in v2.24.1 as commit 4ee40bd; pre-release checklist
  should grow a regen step in a future release to catch this before
  tagging.

## What's NOT in

- Same surface as v2.28.0. Bundled-stdlib proof packages, the
  `verify --lean` trust-surface report, DSL, CLI flags — all
  unchanged. v2.28.0 release binaries (uploaded by `release.yml` on
  the v2.28.0 tag) remain the runtime artifacts for users who pulled
  before v2.28.1; the difference between v2.28.0 and v2.28.1
  binaries is the embedded version string and the bundled
  `package.json`.

## Test plan

- [x] `bash scripts/check-version-consistency.sh` — `Version metadata consistent: 2.28.1`
- [x] `cargo fmt --check`
- [x] `cargo clippy --release -- -D warnings` (no code change, but
      we re-validate to keep the gate-sequence honest)
- [ ] Post-tag: `release.yml` uploads v2.28.1 binaries
- [ ] Post-tag: `CI` workflow on the tag commit goes green

## Upgrade notes

- Same surface as v2.28.0. No spec, lock, or codegen changes; bundled
  examples don't need regen.
