# CLI: validation

`check`, `reconcile`, `build`, `verify`: linting the spec, building the verified binary, and running the backends.

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
| `--drift` | Path | - | Rust source path for #[qed(verified)] drift detection. Checks every leg a stamp carries — `hash` (body), `spec_hash` (the handler's spec block), `accounts_hash` (the accounts struct) — and names the stale leg in the report, because a stale `spec_hash` means regenerate while a stale body hash means re-verify. Before #382 only the body leg was read, so a stale `spec_hash` reported `OK`. |
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

### `build`

Build the program in Docker with `solana-verify build` (#449) and keep three
things under `<workspace>/.qed/build/<lib>/`, one directory per program, so
programs that share a workspace keep separate builds:

- `<lib>.so`: the stripped deploy binary. This is the file to deploy.
- `<lib>.debug.so`: the unstripped binary. Its debug info is the label source
  for the lift. Its `.text` is byte-identical to the deploy binary.
- `build.json`: the build record, tied to the git commit that was built.

The agent runs it before `verify` on a program it verifies. A local build is
not a fallback: it does not reproduce the Docker build byte for byte, so
labels from it could describe different bytes.

```bash
$QEDGEN build --program programs/my_program
$QEDGEN build --program . --arch v3 --tools-version v1.57
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--program` | Path | required | Program crate (the directory with the program's own `Cargo.toml`). The workspace root is the nearest ancestor whose `Cargo.toml` has `[workspace]`. |
| `--library` | String | `[lib] name`, else the package name with `-` as `_` | Name of the `.so`. Must be letters, digits and `_` only, never a path. |
| `--arch` | String | `v3` | sBPF arch passed to `cargo build-sbf`. |
| `--tools-version` | String | `v1.57` | Platform-tools version passed to `cargo build-sbf`. Debug info can change the generated code (see below), so deploy the `.so` this build produced. |

It refuses, before Docker starts, when:

- the workspace has uncommitted changes (files under `.qed/` do not count);
- `[profile.release]` has no full `debug` setting. Add `debug = 2` (or
  `"full"`) and commit it. Limited levels (`1`, `"limited"`,
  `"line-tables-only"`, `"line-directives-only"`) are refused: they have no
  types, so the concept map cannot read struct members from them.
  `[profile.release.build-override]` does not count;
- the workspace has no `Cargo.lock` (`solana-verify` builds with `--locked`);
- `solana-verify` cannot pick a build image, or would pick a Solana 1.x
  image. It reads the Solana version from `[workspace.metadata.cli] solana`
  in the workspace `Cargo.toml`, else from `solana-program`,
  `solana-program-error` or `solana-account-info` in `Cargo.lock`. Pinocchio
  programs often have none of these crates. A 1.x image's `cargo build-sbf`
  has no `--arch v3`. The fix is to set the version and commit it:

  ```toml
  [workspace.metadata.cli]
  solana = "3.0.1"
  ```
- `solana-verify` 0.5.2 or newer is missing (`cargo install solana-verify --locked`);
- Docker is not running.

The image's own cargo reads `Cargo.lock`, so the lockfile must be one that
cargo can parse. The Solana 2.x images ship cargo 1.84, which cannot read
crates on edition 2024. A fresh `cargo generate-lockfile` today pulls such
crates (for example `zeroize` 1.9), so pin `solana = "3.0.1"` when that
happens. `build` warns before Docker starts when it will use a 2.x image.

After the build it stops when the unstripped `.text` differs from the deploy
`.so`, when the unstripped binary has no `.debug_info`/`.debug_line`, when the
sBPF version is not the requested arch, or when the hash `solana-verify`
printed differs from qedgen's own hash. If `solana-verify` printed no hash,
it warns that the cross-check could not run. Each build replaces every `.so`
in its own `.qed/build/<lib>/`, so binaries from an earlier build of that
program do not stay behind. Other programs' directories are not touched.
After the build it checks that `HEAD` and the tree did not change while
Docker ran, because `solana-verify` builds the live working tree. If they
changed, it records nothing. When `solana-verify build` fails, it leaves
its Docker container running. `build` then removes only the container that
was building this workspace: a `solana-verifiable-build` container that
started during this build and mounts this workspace. Other builds, even
concurrent ones, are not touched. On a non-amd64 host it sets
`DOCKER_DEFAULT_PLATFORM=linux/amd64`, because the build images are amd64
only. Expect a few minutes per build under emulation and about 5 GB per image.

Debug info and the generated code, measured on platform-tools v1.57
(2026-10-26), V0 and v3:

| Program | `debug = 1` | `debug = 2` |
|---|---|---|
| p-token (Pinocchio) | deployed bytes unchanged | deployed bytes unchanged |
| minimal Pinocchio 0.9.3 | deployed bytes unchanged | deployed bytes unchanged |
| minimal Anchor 0.32.1 | same size, about 2.7 KB of `.text` differs | 2 more instructions, `.rodata` differs |

So debug info is not free in general. This is why the binary to deploy is
the one `build` produced, and why the labels come from the same build. On
the same Anchor program, `debug = 1` has 0 DWARF struct members and
`debug = 2` has 6,735, which is why `build` requires full debug info.

`build.json` fields: `commit`, `library`, `program_hash` (SHA-256 after
trailing zero bytes are stripped; the number `solana-verify` prints),
`file_sha256` (plain SHA-256), `solana_verify_version`, `image` (the image's
Solana version, as `solana-verify` printed it), `platform_tools`, `arch`,
`debug_setting`, `build_argv`, `workspace_path` (the workspace's path inside the repo, empty at the root), `text_matches`, `recorded_at_unix`.
`.qed/build/.gitignore` keeps the `.so` files out of git, so each
`build.json` can be committed.

Every `verify` run looks for the record of one program: `--program` when
given, else the crate the spec sits in, in that program's workspace. A spec
that is not in a program crate (for example at a workspace root) names no
program, so no build is attached; pass `--program`. It copies the record's `commit`
and `program_hash` into `.qed/verify-evidence.json` (`binary_build`) when
the record's commit is `HEAD` and the tree is clean. Otherwise it warns that
the build is stale. Files under any `.qed/` directory (build output,
evidence, the obligation manifest) never count as uncommitted changes, for
`build` or for `verify`.
[`stamp`](spec-authoring.md#stamp) prints `Verified binary: <hash> at
<commit>` when the evidence has it. A missing build never blocks `stamp`.

The build ends by printing a ready-to-run `solana-verify verify-from-repo`
command with `<PROGRAM_ID>` and `<REPO_URL>` placeholders. It adds
`--mount-path <workspace_path>` when the workspace is not the repo root. To check a
deployed program later, use `verify --check-deployed`, or rebuild
with `solana-verify verify-from-repo` and the same `--library-name`, `--arch`
and `--cargo-build-sbf-args` values. For Anchor 0.32 and newer, `anchor
verify` runs `solana-verify verify-from-repo` and passes those arguments
through.

### `verify`
Run the generated harnesses against the implementation. `check` validates
the spec; `verify` validates the code the spec produced. With no backend
flags, runs every backend whose artifact is present on disk
(`./programs/tests/proptest.rs`, `./programs/tests/kani.rs`,
`./formal_verification/`, and the program crate itself when its
dependencies already resolve). Use `--proptest` / `--kani` / `--lean` /
`--scaffold` to target one backend.

**`--scaffold` (#364)** compiles the generated program crate with
`cargo check --tests`. It is the only backend that builds what codegen
wrote: `check` is spec-level lint, and the Kani/proptest harnesses build in
isolated crates on purpose, so before this backend a codegen defect reached
the user as a red `cargo build` with no qedgen diagnostic, often right after
`check` printed `0 error(s)`. It compiles the crate named by `--program`,
defaulting to `./programs`.

Only rustc rejecting code in that crate fails the run. Dependency
resolution, a missing toolchain, and a dependency that itself fails to build
all report `skipped` with the reason, because none of them is a statement
about the generated code. A pass does **not** authorize [`stamp`](#stamp):
compiling is not verifying.

`codegen` runs the same check over what it just wrote, but only once the
dependency tree resolves, so a first generation never stalls on a cold
`anchor-lang` build. It reports and never changes the exit code; `verify
--scaffold` is the gating surface. Opt out with `codegen
--no-check-compiles`.

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

# Compare the deployed program with the `qedgen build` record. A mismatch
# is a P2 warning; the exit code stays 0.
$QEDGEN verify --spec my_program.qedspec --check-deployed <PROGRAM_ID>
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (`.qedspec`) |
| `--program` | Path | none | Program crate. Hashed into implementation-bound evidence (required before `stamp` can consume it), and the crate `--scaffold` compiles. |
| `--scaffold` | bool | false | Compile the generated program crate (`cargo check --tests`) and fail if it does not typecheck. Dependency/toolchain failures report `skipped` with the reason, not `failed`. A pass does not authorize `stamp`. |
| `--proptest` | bool | false | Run proptest harnesses (`cargo test --release`) |
| `--proptest-path` | Path | `./programs/tests/proptest.rs` | Proptest harness file |
| `--kani` | bool | false | Run Kani BMC harnesses (`cargo kani --tests`) |
| `--kani-path` | Path | `./programs/tests/kani.rs` | Kani harness file |
| `--lean` | bool | false | Run Lean proofs (`lake build`) |
| `--lean-dir` | Path | `./formal_verification` | Lean project directory |
| `--miri` | bool | false | Run Pinocchio Miri reproducers under `.qed/probes/pinocchio/*/repro_miri.rs` via `cargo +nightly miri test`. UB / aliasing / overflow diagnostics surface as findings; dual-execution divergence against Mollusk repros surfaces as Critical. |
| `--fail-fast` | bool | false | Stop on the first failing backend |
| `--json` | bool | false | Machine-readable output for CI |
| `--check-upstream` | bool | false | Diff each pinned `upstream_binary_hash` against the on-chain program via `solana program dump`, hashed the `solana-verify` way (trailing zero bytes stripped, then SHA-256; #452). A pin made the old way (plain SHA-256 of the dump) that still matches passes with a P2 warning naming the new pin. Skips deps without a pinned hash. Non-zero exit on any mismatch. |
| `--rpc-url` | String | Solana CLI default | Override RPC endpoint passed to `solana program dump --url <rpc>` |
| `--offline` | bool | false | Refuse to reach the network. Any dep that would require an on-chain fetch reports as Error. CI-gate friendly. |
| `--check-deployed` | String | none | Program id. Fetch the deployed program (`solana program dump`) and compare its hash, computed the `solana-verify` way (trailing zero bytes stripped), with the [`build`](#build) record. A match and a program that is not deployed print one info line. A mismatch prints a `P2:` line and the exit code stays 0: the stamp still covers the verified commit. Other fetch errors fail the run, including an RPC that cannot be reached. The record is the one for `--program`, else for the crate the spec sits in (`.qed/build/<lib>/build.json` in its workspace); without either it refuses and asks for `--program`. Honors `--rpc-url` and `--offline`. Like `--check-upstream`, when it is the only verb (no backend flag and no `--probe-repros`), verify returns after the check and runs no backends. |
| `--upstream-stale-ok` | bool | false | Suppress the upstream binary-hash check even when the lock declares pinned hashes. Mismatches demote to Info; verify exits zero. Offline-dev only — do not use in CI. Pairs with the auto-on behavior of `--check-upstream`. |
| `--probe-repros` | bool | false | Discover and run probe reproducers under `<project>/target/qedgen-repros/`, including agent-authored audit repros and mechanically generated category repros. Reports `Fired`, `Silent`, or `BuildError` per repro; emits `note: no repros found` only when the directory contains no runnable reproducers. |
| `--crucible` | u64 | none | Run the coverage-guided fuzz engine for the given wall-clock seconds. Thin alias over `probe --fuzz` — folds findings into the BackendReport so they render through the same named-trace human surface as Kani / proptest. |
| `--crucible-harness-dir` | Path | `./fuzz/<prog>/` | Harness directory for `--crucible`. |
| `--crucible-no-smoke` | bool | false | Skip the 30s smoke pre-flight. |
| `--crucible-stateful` | bool | false | Stateful action-chain mode for `--crucible`. |
| `--recursive` | bool | false | v2.27 Track D3 — DFS-walk the transitive proof-package closure (deduped by path) and run `lake build` per layer. Per-layer PASS/FAIL is reported; failed layers print the first ~10 lines of stderr/stdout. Exits non-zero on any layer failure; emits "every imported proof package built clean" when all pass. No-op success when the spec imports nothing with `verified = true` in `qed.lock`. |
| `--require-verified` | bool | false | v2.27 Track D2 — exits non-zero before any backend dispatches if any imported Tier-1+ interface (binary_hash + `ensures`) did NOT ship a `.qed/proofs/<Iface>.lean + lakefile.lean` package alongside. Tier-0 (no ensures) and sentinel-pinned natives (all-zero binary_hash) are exempt. Default-off in v2.27 because the bundled stdlib still ships Stance 1 for `import System from "system"` (no bundled proof package for Pubkey-param handlers). |
| `--strict` | bool | false | #332 — recompute the reconciled backend-obligation manifest (kani / lean / proptest, in memory) and exit 1 on any `unsupported` or `failed` entry. A passing strict verify means no requested obligation was silently dropped by a backend. Remaining `unsupported` shapes (v2.48.0): property preservation that spans account modules (kani/lean), Lean abort predicates with multi-projection account reads, CPI ensures composition at call sites without `state_binders`, and guard-rejection tests whose guard does not survive the simplified proptest model. Specs with these shapes fail strict verify by design. Also gates the Lean `#print axioms` report: exit 1 when a theorem depends on `sorryAx`, on the `native_decide` compiler axioms (`Lean.ofReduceBool`, `Lean.trustCompiler`), or on any axiom other than `propext` / `Classical.choice` / `Quot.sound` and the `<Iface>.<handler>.ensures_axiom_<i>` of the spec's pinned interfaces. The permitted set comes only from the spec; there is no flag or pragma that adds to it. Without `--strict` these axioms print as `[CRIT]` under the trust surface and the run passes. |
