# CLI: validation

`check`, `reconcile`, `verify` — linting the spec and running the backends.

Part of the [CLI Reference](../cli.md).

### `check`
Validate a spec — lint, coverage, drift, and verification report. Default
(no flags) runs lint + coverage.

Requires a git repo (see [Require-git guard](#require-git-guard)).

`--spec` is optional — when omitted, walks up from the current directory to
the nearest `.qed/config.json` and uses its `spec` field. Explicit `--spec`
overrides.

```bash
# From inside a project initialized with `qedgen init --spec ...`
$QEDGEN check
$QEDGEN check --json

# Explicit spec path
$QEDGEN check --spec my_program.qedspec

# Coverage matrix
$QEDGEN check --coverage

# Verification report
$QEDGEN check --explain
$QEDGEN check --spec my_program.qedspec --explain --output report.md

# Drift detection
$QEDGEN check --spec my_program.qedspec --drift programs/src/
$QEDGEN check --spec my_program.qedspec --drift programs/src/ --deep
$QEDGEN check --spec my_program.qedspec --drift programs/src/ --update-hashes

# Unified code + kani drift
$QEDGEN check --spec my_program.qedspec --code programs/my_program/ --kani programs/tests/kani.rs

# sBPF verification (hash check + lake build)
$QEDGEN check --spec my_program.qedspec --asm src/program.s

# Anchor project cross-check (spec ↔ #[program] mod handler set)
$QEDGEN check --spec my_program.qedspec --anchor-project programs/my_program/

# CI freeze gate: refuse to update qed.lock and refuse network fetches.
# v2.26 Slice 4c — `--frozen` also diffs each pinned binary_hash against
# the on-chain .so. Mismatches surface as P2 warnings (exit 0); pair with
# `--strict` to escalate to CRIT and fail the check.
$QEDGEN check --spec my_program.qedspec --frozen
$QEDGEN check --spec my_program.qedspec --frozen --strict
$QEDGEN check --spec my_program.qedspec --frozen --no-cache

# Bundled example drift gate
$QEDGEN check --regen-drift
$QEDGEN check --regen-drift --examples-root examples/rust
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Spec file or directory. Defaults to `.qed/config.json spec` |
| `--proofs` | Path | `./formal_verification` | Proofs directory |
| `--coverage` | bool | false | Show operation × property matrix (spec coverage) plus the per-backend obligation rollup (backend coverage, #332): for each of kani / lean / proptest, how many requested obligations are `emitted` vs `unsupported(reason)` vs `failed`, recomputed in memory from the current spec. Under `--json` the matrix fields and the `backend_coverage` key form the `coverage` section of the single check document (#355). |
| `--explain` | bool | false | Generate Markdown verification report |
| `--output` | Path | stdout | Output file for --explain |
| `--drift` | Path | - | Rust source path for #[qed(verified)] drift detection |
| `--update-hashes` | bool | false | Auto-stamp hashes in source files |
| `--deep` | bool | false | Transitive drift detection (check callees) |
| `--code` | Path | - | Generated program source dir (code drift detection) |
| `--kani` | Path | - | Kani harness file (Kani drift detection) |
| `--asm` | Path | - | sBPF assembly source (hash check + lake build) |
| `--anchor-project` | Path | - | Anchor program crate (`Cargo.toml` + `src/lib.rs`). Cross-checks the spec's `handler` set against the `#[program]` mod's instruction set, plus an effect-coverage lint per resolved handler body. CI gate. |
| `--frozen` | bool | false | Refuse to update `qed.lock`; error if the on-disk lock is stale or missing. Used in CI to detect un-bumped imports. |
| `--strict` | bool | false | Escalate `--frozen` upstream binary-hash mismatches AND v2.27 Track D1 proof_hash drift from P2 warning to CRIT (gates exit). Use in release-blocking CI; default `--frozen` stays warning-only. Requires `--frozen`. |
| `--no-cache` | bool | false | Force-refresh the github source cache for every imported dep. Wipes `~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/` and re-clones. |
| `--regen-drift` | bool | false | Regenerate bundled examples into temporary directories and fail if committed generated support code, harnesses, or `Spec.lean` drift. Also fails when an example has `.qed/` state or generated artifacts but no `qed.toml`. |
| `--examples-root` | Path | `examples/rust` | Example root scanned by `--regen-drift` |
| `--write` | bool | false | With `--regen-drift`, also write the regenerated content into the repo so committed example outputs match current codegen. Useful for rebasing PRs across codegen-touching releases. Never touches user-owned files (handler bodies, Spec.lean proofs) — only the codegen-owned set `--regen-drift` already compares. |
| `--json` | bool | false | Machine-readable output. Stdout is exactly one JSON document (#355). Plain `check --json` prints the bare lint-findings array. When any other section prints (`--coverage`, `--explain` without `--output`, `--anchor-project`, or Proofs.lean drift), the document is one object with a key per section — `coverage` (matrix fields + `backend_coverage`), `explain`, `anchor_project`, `proofs_drift` — plus the `findings` array. |

Lints fired by `check` include `[shape_only_cpi]` for `call
Interface.handler(...)` sites whose target declares no `ensures` —
making the visible gap between "my Rust compiles" and "my program is
verified" explicit.

### `reconcile`
Emit a unified drift report comparing a `.qedspec` against both its Rust
handlers and its Lean proofs. Report-only — never modifies files.

Requires a git repo (see [Require-git guard](#require-git-guard)).

```bash
# Default paths: --code programs/ --proofs formal_verification/
$QEDGEN reconcile --spec my_program.qedspec

# Custom paths
$QEDGEN reconcile --spec my_program.qedspec --code programs/escrow/ --proofs verification/

# Machine-readable (for CI / agent consumption)
$QEDGEN reconcile --spec my_program.qedspec --json
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (.qedspec) |
| `--code` | Path | `programs/` | Root directory scanned for `#[qed(verified, ...)]` attributes (recursive) |
| `--proofs` | Path | `formal_verification/` | Directory containing `Proofs.lean` |
| `--json` | bool | false | Emit JSON instead of the human-readable report |

What it reports:

- **Rust handler drift** — handlers where the computed body hash or the
  recomputed spec-handler hash no longer matches the stamped `#[qed(...)]`
  attribute, or where the attribute references a handler that no longer
  exists in the spec.
- **Lean orphans** — `*_preserved_by_*` theorems in `Proofs.lean` that don't
  correspond to any current (property, handler) pair in the spec.
- **Lean missing** — (property, handler) pairs required by `preserved_by`
  clauses in the spec for which no `*_preserved_by_*` theorem exists in
  `Proofs.lean`.
- **Cross-spec warnings** — Rust files with `#[qed]` attributes pointing at a
  different `.qedspec` than the one passed on the CLI.

Exit codes:

- `0` — no drift; spec, code, and proofs are in sync
- `1` — drift detected (any of the categories above)

Typical use:

- After editing a `.qedspec`: `qedgen reconcile --spec x.qedspec` shows
  exactly which handlers need a hash refresh and which proofs are now
  orphans or missing.
- As a CI gate: `qedgen reconcile --spec x.qedspec --json | tee drift.json`
  plus `test $? -eq 0` ensures drift blocks merges.
- As the first step of the agent-driven reconciliation loop described in
  SKILL.md **Step 4d**.

### `verify`
Run the generated harnesses against the implementation. `check` validates
the spec; `verify` validates the code the spec produced. With no backend
flags, runs every backend whose artifact is present on disk
(`./programs/tests/proptest.rs`, `./programs/tests/kani.rs`,
`./formal_verification/`). Use `--proptest` / `--kani` / `--lean` to
target one backend. Supplying flagless `--program <crate>` also runs the
`scaffold` backend (`cargo check --tests`) in that program crate.

v2.44 — every run also records its evidence to
`<spec_dir>/.qed/verify-evidence.json` (spec hash, optional program-source
hash, per-backend status, and whether an **implementation-bound** backend
passed: miri, or Kani only when the `--kani-path` file is a
`kani_impl*.rs` harness). Plain proptest/Kani/Lean exercise the spec model,
while `--probe-repros` confirms bug findings; neither category counts. This
record is what [`stamp`](#stamp) gates
on; it is written on pass and fail alike (a failed run is still evidence
of what ran) and a failed write never turns a green verify red.

```bash
# Auto-detect: every backend whose artifact exists on disk
$QEDGEN verify --spec my_program.qedspec

# Targeted
$QEDGEN verify --spec my_program.qedspec --proptest
$QEDGEN verify --spec my_program.qedspec --kani
$QEDGEN verify --spec my_program.qedspec --lean

# Compile the generated program crate as a verify backend.
$QEDGEN verify --spec my_program.qedspec --program ./programs/my_program --scaffold

# CI gating
$QEDGEN verify --spec my_program.qedspec --fail-fast --json

# Diff every imported library's pinned upstream_binary_hash against
# the on-chain .so (requires `solana` CLI in PATH). v2.26 Slice 4c —
# mismatched pins surface as CRIT findings and gate exit. Auto-on when
# qed.lock declares any pinned `binary_hash`.
$QEDGEN verify --spec my_program.qedspec --check-upstream
$QEDGEN verify --spec my_program.qedspec --check-upstream --rpc-url https://api.devnet.solana.com
$QEDGEN verify --spec my_program.qedspec --check-upstream --offline
# Offline development — suppress the upstream check; mismatches demote
# to Info and verify exits zero. Do NOT use in CI.
$QEDGEN verify --spec my_program.qedspec --check-upstream --upstream-stale-ok
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (`.qedspec`) |
| `--program` | Path | none | Program crate selected by source-bound backends and hashed into their evidence; required before `stamp` can consume eligible implementation-bound evidence |
| `--scaffold` | bool | auto with flagless `--program` | Run `cargo check --tests` in the `--program` crate. Requires `--program`; explicit backend selection remains exact. Compilation proves buildability, not semantic conformance, and cannot authorize `stamp`. |
| `--proptest` | bool | false | Run proptest harnesses (`cargo test --release`) |
| `--proptest-path` | Path | `./programs/tests/proptest.rs` | Proptest harness file |
| `--kani` | bool | false | Run Kani BMC harnesses (`cargo kani --tests`) |
| `--kani-path` | Path | `./programs/tests/kani.rs` | Kani harness file |
| `--lean` | bool | false | Run Lean proofs (`lake build`) |
| `--lean-dir` | Path | `./formal_verification` | Lean project directory |
| `--miri` | bool | false | Run Pinocchio Miri reproducers under `.qed/probes/pinocchio/*/repro_miri.rs` via `cargo +nightly miri test`. UB / aliasing / overflow diagnostics surface as findings; dual-execution divergence against Mollusk repros surfaces as Critical. |
| `--fail-fast` | bool | false | Stop on the first failing backend |
| `--json` | bool | false | Machine-readable output for CI |
| `--check-upstream` | bool | false | Diff each pinned `upstream_binary_hash` against the on-chain `.so` via `solana program dump`. Skips deps without a pinned hash. Non-zero exit on any mismatch. |
| `--rpc-url` | String | Solana CLI default | Override RPC endpoint passed to `solana program dump --url <rpc>` |
| `--offline` | bool | false | Refuse to reach the network. Any dep that would require an on-chain fetch reports as Error. CI-gate friendly. |
| `--upstream-stale-ok` | bool | false | Suppress the upstream binary-hash check even when the lock declares pinned hashes. Mismatches demote to Info; verify exits zero. Offline-dev only — do not use in CI. Pairs with the auto-on behavior of `--check-upstream`. |
| `--probe-repros` | bool | false | Discover and run probe reproducers under `<project>/target/qedgen-repros/`, including agent-authored audit repros and mechanically generated category repros. Reports `Fired`, `Silent`, or `BuildError` per repro; emits `note: no repros found` only when the directory contains no runnable reproducers. |
| `--crucible` | u64 | none | Run the coverage-guided fuzz engine for the given wall-clock seconds. Thin alias over `probe --fuzz` — folds findings into the BackendReport so they render through the same named-trace human surface as Kani / proptest. |
| `--crucible-harness-dir` | Path | `./fuzz/<prog>/` | Harness directory for `--crucible`. |
| `--crucible-no-smoke` | bool | false | Skip the 30s smoke pre-flight. |
| `--crucible-stateful` | bool | false | Stateful action-chain mode for `--crucible`. |
| `--recursive` | bool | false | v2.27 Track D3 — DFS-walk the transitive proof-package closure (deduped by path) and run `lake build` per layer. Per-layer PASS/FAIL is reported; failed layers print the first ~10 lines of stderr/stdout. Exits non-zero on any layer failure; emits "every imported proof package built clean" when all pass. No-op success when the spec imports nothing with `verified = true` in `qed.lock`. |
| `--require-verified` | bool | false | v2.27 Track D2 — exits non-zero before any backend dispatches if any imported Tier-1+ interface (binary_hash + `ensures`) did NOT ship a `.qed/proofs/<Iface>.lean + lakefile.lean` package alongside. Tier-0 (no ensures) and sentinel-pinned natives (all-zero binary_hash) are exempt. Default-off in v2.27 because the bundled stdlib still ships Stance 1 for `import System from "system"` (no bundled proof package for Pubkey-param handlers). |
| `--strict` | bool | false | Fail when an enabled scaffold backend is skipped. Also recompute the reconciled backend-obligation manifest (kani / lean / proptest, in memory) and exit 1 on any `unsupported` or `failed` entry. A passing strict verify means no requested obligation was silently dropped by a backend. Remaining `unsupported` shapes (v2.48.0): property preservation that spans account modules (kani/lean), Lean abort predicates with multi-projection account reads, CPI ensures composition at call sites without `state_binders`, and guard-rejection tests whose guard does not survive the simplified proptest model. Specs with these shapes fail strict verify by design. |
