# Release v2.27.1 — CI patch

CI-only patch on top of v2.27.0. No code, no DSL, no bundled-stdlib
changes. Two workflow fixes from PRs #59–#62 land on the tag so a
fresh `git checkout v2.27.1` produces working artifacts end to end:

## What's in

- **`lake-build.yml`** — bundled-stdlib examples (`escrow-split`,
  `bundled-stdlib-demo`) now lake-build clean on CI. Their committed
  `lakefile.lean` carries an author-machine-specific
  `require tokenProofs from "<rel-path>"` directive; the workflow now
  builds qedgen, `cd`s into each affected example, strips the stale
  require line + Track-B preamble, runs `qedgen codegen --lean --spec .`
  to materialize the bundled proof package into the runner's cache
  AND rewrite the directive with a CI-correct path, then invalidates
  the stale `lake-manifest.json` so the bootstrap step's `lake update`
  picks up the new require. The `cd` was the root-cause fix —
  `--lean-output` defaults to `./formal_verification/Spec.lean`
  cwd-relative, so running from repo root previously wrote Spec.lean
  to the wrong place and the lakefile inject step short-circuited.

- **`release.yml`** — macOS targets pass `RUSTFLAGS=-C link-arg=-Wl,-ld_classic`.
  Xcode's `ld-prime` linker on `macos-latest` trips
  `name.size() <= maxLength` on long Rust-mangled symbol names; the
  classic Mach-O linker handles them without complaint. Linux targets
  unchanged. With this fix, v2.27.1's `release.yml` produces all 4
  binaries via CI; v2.27.0's macOS binaries were built locally and
  uploaded out-of-band (same content, different provenance).

## Underlying gap (still pending for v2.28)

The committed `lakefile.lean`'s `require tokenProofs from "<path>"`
directive is computed by `lean_gen::pathdiff_relative_from` against
the author's `$HOME` depth. The path depth that lands at
`~/.qedgen/cache/builtin/<key>/.qed/proofs/` depends on how deep the
repo is under `$HOME` — committed lakefiles therefore only resolve
cleanly on the machine they were generated on. v2.27.1's CI workflow
works around it by regenerating the lakefile on every CI run. v2.28
should fix it structurally: either vendor the bundled proof package
into the consumer's tree under `formal_verification/vendored/<pkg>/`
so the path is repo-relative, or use a `git source` for the require
directive so it resolves consistently across clones.

## Test plan

- [x] `cargo fmt --check` ✓
- [x] `cargo clippy --release -- -D warnings` ✓
- [x] `cargo test --release --bin qedgen` — 926 / 926 pass (no change from v2.27.0)
- [x] `scripts/check-readme-drift.sh` — 19 / 19
- [x] `scripts/check-lake-build.sh` — 11 / 11
- [x] Reproduced the original cascade locally (clean cache + repo-root
      codegen) and confirmed each of the four hotfixes is necessary
- [ ] Post-tag: `release.yml` ships all 4 binaries (Linux + macOS) via CI
- [ ] Post-tag: lake-build CI on the tag commit goes green from a fresh clone

## Upgrade notes

- Same surface as v2.27.0. Bundled stdlib, DSL, proof packages,
  CLI flags all unchanged. Only the CI workflows + version metadata
  + this RELEASE notes file land in v2.27.1.
- v2.27.0's manually-uploaded macOS release assets remain valid;
  v2.27.1's macOS assets are CI-built equivalents.
