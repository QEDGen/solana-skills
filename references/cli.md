# CLI Reference

All commands are run via the wrapper: `$QEDGEN <command> [flags]`

## Require-git guard

`qedgen codegen`, `qedgen check`, and `qedgen reconcile` all require the
current directory to be inside a git repository (they walk upward looking for
`.git`). If no repo is found, the command prints

```
qedgen requires a git repo — run `git init` first
```

and exits 1. QEDGen relies on git for safe regeneration (three-way merge of
generated artifacts), proof preservation, and drift reconciliation; running
outside a repo would silently discard user edits to `src/instructions/*.rs`
and `Proofs.lean`.

## Project setup

### `init`
Scaffold a new formal verification project. Creates `.qed/` project state
directory and pins the spec path in `.qed/config.json` so subsequent
commands don't need `--spec`.

```bash
$QEDGEN init --name escrow   --spec escrow.qedspec
$QEDGEN init --name dropset  --spec dropset.qedspec --asm src/dropset.s
$QEDGEN init --name engine   --spec engine.qedspec --mathlib
$QEDGEN init --name counter  --spec counter.qedspec --target anchor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--name` | String | required | Project name (alphanumeric + underscores) |
| `--spec` | Path | - | Spec path (file or directory) — written into `.qed/config.json` so `check`/`codegen` can resolve it automatically |
| `--asm` | Path | - | sBPF assembly source (runs asm2lean automatically) |
| `--mathlib` | bool | false | Include Mathlib dependency |
| `--target` | enum | - | Also generate the program crate + Kani harnesses for the named framework target. Values: `anchor` (Anchor-compatible Rust), `quasar` (Blueshift Quasar — `#![no_std]`, explicit discriminators, `Ctx<X>`), `pinocchio` (reserved CLI surface; codegen not yet implemented — selecting it errors). Requires `--spec`. Omit to skip program scaffolding entirely. |
| `--output-dir` | Path | `./formal_verification` | Output directory |

The written `.qed/config.json`:

```json
{
  "name": "escrow",
  "spec": "escrow.qedspec",
  "interfaces_dir": ".qed/interfaces"
}
```

### `setup`
Set up the global validation workspace at `~/.qedgen/workspace/`.

```bash
$QEDGEN setup
$QEDGEN setup --mathlib
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--workspace` | Path | `~/.qedgen/workspace/` | Override workspace path |
| `--mathlib` | bool | false | Fetch Mathlib cache (~8GB) |

### `asm2lean`
Transpile sBPF assembly to Lean 4 program module.

```bash
$QEDGEN asm2lean --input src/program.s --output formal_verification/Prog.lean
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--input` | Path | required | sBPF assembly source file |
| `--output` | Path | required | Output Lean 4 file |
| `--namespace` | String | derived from filename | Lean namespace |

## Spec and validation

### `interface`
Generate a Tier-0 interface `.qedspec` from an Anchor IDL. Shape only —
program ID, discriminator, accounts, argument types. No `requires`/
`ensures`/`effect` (those require semantic understanding the IDL does not
carry). The `upstream` block is left as a TODO stub for the author to fill
in after running QEDGen harnesses against the deployed program.

See `docs/design/spec-composition.md` §2 for the CPI tier model.

```bash
# Print to stdout
$QEDGEN interface --idl target/idl/jupiter.json

# Write to an explicit path
$QEDGEN interface --idl target/idl/jupiter.json --out interfaces/jupiter.qedspec

# Vendor into .qed/interfaces/<program>.qedspec (canonical library location,
# resolved via the nearest .qed/config.json)
$QEDGEN interface --idl target/idl/jupiter.json --vendor
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON file |
| `--out` | Path | - | Output path (default: stdout). Conflicts with `--vendor`. |
| `--vendor` | bool | false | Drop into `.qed/interfaces/<program>.qedspec`. Requires a discoverable `.qed/` ancestor. |

### `spec`
Scaffold a `.qedspec` from an Anchor IDL JSON. (For Tier-0 interface
scaffolding from an IDL — program ID + handler signatures only — prefer
`interface`, which is more focused.) v2.10 dropped the SPEC.md
generators that previously lived behind `--from-spec` and the default
`--format md` path; `.qedspec` is QEDGen's front-door artifact and
parallel Markdown duplicates were drifting in practice.

```bash
$QEDGEN spec --idl target/idl/program.json
$QEDGEN spec --idl target/idl/program.json --output-dir ./formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON file |
| `--output-dir` | Path | `./formal_verification` | Output directory; `<idl-stem>.qedspec` is written inside |

### `adapt`
Brownfield adapter for existing Anchor programs. Two modes:

- **Scaffold mode** (`--program <c>` only): parses `<c>/src/lib.rs`, finds
  the `#[program]` mod, walks each instruction to its handler body via
  forwarder classification, and emits a parseable `.qedspec` skeleton with
  TODO markers for state machine / requires / effect bodies.
- **Attribute mode** (`--program <c> --spec <s>`): given a filled-in spec,
  emits one `#[qed(verified, spec = ..., handler = ..., hash = ...,
  spec_hash = ...[, accounts = ..., accounts_file = ..., accounts_hash = ...])]`
  line per handler. Paste each above its handler `pub fn`; future body or
  spec edits trip `compile_error!` until you re-run `adapt --spec`.

Forwarder shapes the classifier handles end-to-end: Inline, free-fn
(`module::fn(args)` plus the two-stmt `<call>?; Ok(())` and `?`-tail
shapes), type-associated (`Type::method(ctx, args)` PascalCase prefix),
accounts-method (`ctx.accounts.method(args)`). Custom dispatcher patterns
fall through to `Unrecognized` — use `--handler` to point them at the real
implementation.

```bash
# Scaffold a starter spec from existing Anchor source
$QEDGEN adapt --program ./programs/my_program

# Write to disk instead of stdout
$QEDGEN adapt --program ./programs/my_program --out my_program.qedspec

# Emit #[qed] attributes for an existing spec
$QEDGEN adapt --program ./programs/my_program --spec my_program.qedspec

# Custom dispatcher handlers — point each at its actual implementation
$QEDGEN adapt --program ./programs/my_program \
  --handler dispatch=instructions::dispatch::handler \
  --handler ix2=instructions::ix2::run
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--program` | Path | required | Program crate (directory holding `Cargo.toml`, with `src/lib.rs` inside) |
| `--spec` | Path | - | Existing `.qedspec`. Switches to attribute-emit mode |
| `--out` | Path | stdout | Output path. In scaffold mode writes a `.qedspec`; in attribute mode writes a `// === handler … ===` report |
| `--handler` | `NAME=PATH` | - | Manually point an unrecognized handler at its actual implementation. Format: `<handler>=<rust_path>` where path is `module::sub::function` or just `function`. Repeatable. Wins over the classifier's choice for any outcome (Inline / FreeFn / Method / Unrecognized) |

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

# CI freeze gate: refuse to update qed.lock and refuse network fetches
$QEDGEN check --spec my_program.qedspec --frozen
$QEDGEN check --spec my_program.qedspec --frozen --no-cache

# Bundled example drift gate
$QEDGEN check --regen-drift
$QEDGEN check --regen-drift --examples-root examples/rust
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Spec file or directory. Defaults to `.qed/config.json spec` |
| `--proofs` | Path | `./formal_verification` | Proofs directory |
| `--coverage` | bool | false | Show operation × property matrix (also enabled by default) |
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
| `--no-cache` | bool | false | Force-refresh the github source cache for every imported dep. Wipes `~/.qedgen/cache/github/<org>/<repo>/<kind>/<ref>/` and re-clones. |
| `--regen-drift` | bool | false | Regenerate bundled examples into temporary directories and fail if committed generated support code, harnesses, or `Spec.lean` drift. Also fails when an example has `.qed/` state or generated artifacts but no `qed.toml`. |
| `--examples-root` | Path | `examples/rust` | Example root scanned by `--regen-drift` |
| `--json` | bool | false | Machine-readable output |

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
target one backend.

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
# the on-chain .so (requires `solana` CLI in PATH)
$QEDGEN verify --spec my_program.qedspec --check-upstream
$QEDGEN verify --spec my_program.qedspec --check-upstream --rpc-url https://api.devnet.solana.com
$QEDGEN verify --spec my_program.qedspec --check-upstream --offline
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (`.qedspec`) |
| `--proptest` | bool | false | Run proptest harnesses (`cargo test --release`) |
| `--proptest-path` | Path | `./programs/tests/proptest.rs` | Proptest harness file |
| `--kani` | bool | false | Run Kani BMC harnesses (`cargo kani --tests`) |
| `--kani-path` | Path | `./programs/tests/kani.rs` | Kani harness file |
| `--lean` | bool | false | Run Lean proofs (`lake build`) |
| `--lean-dir` | Path | `./formal_verification` | Lean project directory |
| `--fail-fast` | bool | false | Stop on the first failing backend |
| `--json` | bool | false | Machine-readable output for CI |
| `--check-upstream` | bool | false | Diff each pinned `upstream_binary_hash` against the on-chain `.so` via `solana program dump`. Skips deps without a pinned hash. Non-zero exit on any mismatch. |
| `--rpc-url` | String | Solana CLI default | Override RPC endpoint passed to `solana program dump --url <rpc>` |
| `--offline` | bool | false | Refuse to reach the network. Any dep that would require an on-chain fetch reports as Error. CI-gate friendly. |

## Code generation

### `codegen`
Generate committed artifacts from a qedspec. Default (no flags) generates
the program Rust skeleton only (Anchor-compatible; see the generated
`Cargo.toml` for dependency configuration).

Requires a git repo (see [Require-git guard](#require-git-guard)).

`--spec` is optional — when omitted, resolved via the nearest
`.qed/config.json`'s `spec` field. Explicit `--spec` overrides.

```bash
# From inside a project initialized with `qedgen init --spec ...`
$QEDGEN codegen
$QEDGEN codegen --all

# Explicit spec path
$QEDGEN codegen --spec my_program.qedspec --all

# Selective
$QEDGEN codegen --lean
$QEDGEN codegen --kani
$QEDGEN codegen --test
$QEDGEN codegen --proptest
$QEDGEN codegen --integration
$QEDGEN codegen --ci
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | optional | Spec file or directory. Defaults to `.qed/config.json spec` |
| `--output-dir` | Path | `./programs` | Output directory for Rust skeleton |
| `--all` | bool | false | Generate all artifacts |
| `--lean` | bool | false | Generate Lean 4 proofs |
| `--lean-output` | Path | `./formal_verification/Spec.lean` | Lean output path |
| `--kani` | bool | false | Generate Kani proof harnesses |
| `--kani-output` | Path | `./programs/tests/kani.rs` | Kani output path. Lives **inside the program package** so `cargo kani --tests` resolves `programs/Cargo.toml` without a hand-authored root shim. |
| `--test` | bool | false | Generate unit tests |
| `--test-output` | Path | `./programs/src/tests.rs` | Unit test output path |
| `--proptest` | bool | false | Generate proptest harnesses |
| `--proptest-output` | Path | `./programs/tests/proptest.rs` | Proptest output path. Lives inside the program package (see `--kani-output`). |
| `--integration` | bool | false | Generate in-process SVM integration tests |
| `--integration-output` | Path | `./src/integration_tests.rs` | Integration test output path |
| `--ci` | bool | false | Generate GitHub Actions CI workflow |
| `--ci-output` | Path | `.github/workflows/verify.yml` | CI workflow output path |
| `--ci-asm` | String | - | sBPF assembly source (for CI verify step) |
| `--ci-ratchet` | Path | - | Anchor IDL the generated CI should lint with `qedgen readiness`. When set, the emitted `verify.yml` runs ratchet after the verification jobs — any breaking / unsafe finding fails the build. Path is repo-root-relative (e.g. `target/idl/escrow.json`) |
| `--fill` | bool | false | After scaffolding, emit one stdout prompt block per handler whose generated body still contains a `todo!()`. The in-session agent (Claude / Codex) reads the prompts and edits the files. |
| `--handler` | String | - | Restrict `--fill` to one handler by name (default: every handler that needs filling) |
| `--fill-tests` | bool | false | After scaffolding, emit prompt blocks for every `todo!()` site in the generated integration test file. Same stdout-for-agent flow as `--fill`, but for `tests/integration_tests.rs`. |

#### Scaffold-once vs. always-regenerate

`codegen` distinguishes files that are **always regenerated** from the spec
(pure derived artifacts) from files that are **scaffolded once** and then
become user-owned (business logic, tactic bodies, integration glue). On the
second run, scaffold-once files are detected as present and skipped with an
advisory line on stderr; their always-regenerated siblings next to them are
refreshed.

| Path | Policy |
|---|---|
| `programs/<name>/src/instructions/mod.rs` | Always regenerated (pure `pub mod` declarations) |
| `programs/<name>/src/instructions/<handler>.rs` | Scaffolded once (user-owned body; `#[qed]` tied to spec) |
| `programs/<name>/src/lib.rs` | Scaffolded once (user-owned crate root) |
| `programs/<name>/src/guards.rs` | Always regenerated |
| `programs/<name>/src/errors.rs` | Always regenerated |
| `tests/integration/*.rs` | Scaffolded once (user-owned integration tests) |
| `programs/tests/kani.rs` | Always regenerated |
| `programs/tests/proptest.rs` | Always regenerated |
| `formal_verification/Spec.lean` | Always regenerated |
| `formal_verification/Proofs.lean` | Scaffolded once (user-owned preservation proofs) |
| `.github/workflows/verify.yml` | Always regenerated |

`Proofs.lean` bootstrapping uses `proofs_bootstrap::bootstrap_if_missing` —
it never overwrites. Once a user-owned file exists, the only way to pick up
new theorems from a changed spec is to add them by hand (or delete the file
and re-run). `qedgen reconcile` flags the delta.

#### `#[qed]` drift attributes

Every scaffolded handler function is stamped with

```rust
#[qed(verified,
      spec      = "../../program.qedspec",
      handler   = "deposit",
      spec_hash = "7e1a48d93b2c0f65")]
pub fn deposit(...) -> Result<()> { ... }
```

and the `hash = "..."` body-hash field is filled in by
`qedgen check --drift --update-hashes` (or manually) once the handler body
stabilises. At compile time the `qedgen-macros` proc macro:

1. Reads the spec file referenced by `spec`
2. Extracts the `handler <handler> { ... }` block verbatim
3. Hashes it (SHA-256, first 16 hex chars)
4. Compares against the `spec_hash` literal — `compile_error!` on mismatch
5. Hashes the function signature + body and compares against `hash` — same

This turns "edit the spec, forget to regen" into a compile error and
"edit a verified function, forget to re-verify" into a compile error.

`#[qed]` attribute arguments (all strings, all optional after `verified`):

| Arg | Purpose |
|---|---|
| `verified` | Marker keyword (required first) |
| `spec` | Path to the `.qedspec` file, relative to the `.rs` source |
| `handler` | Name of the `handler { ... }` block in that spec |
| `hash` | SHA-256-hex16 of the fn signature + body; omit to get a `compile_error` with the computed value |
| `spec_hash` | SHA-256-hex16 of the spec-side handler block text |

See SKILL.md **Step 4d — drift reconciliation** for the full agent-driven
workflow; this page is the flag reference only.

## Proof generation

### `generate`
Generate Lean 4 proofs via Leanstral API (pass@N sampling).

```bash
$QEDGEN generate --prompt-file /tmp/prompt.txt --output-dir /tmp/proof --passes 4 --validate
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--prompt-file` | Path | required | Path to prompt file |
| `--output-dir` | Path | required | Output directory |
| `--passes` | int | 4 | Number of independent completions |
| `--temperature` | float | 0.6 | Sampling temperature |
| `--max-tokens` | int | 16384 | Max tokens per completion |
| `--validate` | bool | false | Validate with `lake build` |
| `--mathlib` | bool | false | Include Mathlib in validation workspace |

### `fill-sorry`
Fill sorry markers in a Lean file using Leanstral.

```bash
$QEDGEN fill-sorry --file formal_verification/Spec.lean --validate
$QEDGEN fill-sorry --file formal_verification/Spec.lean --escalate
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--file` | Path | required | Lean file with sorry markers |
| `--output` | Path | overwrites input | Output path |
| `--passes` | int | 3 | Attempts per sorry |
| `--temperature` | float | 0.3 | Sampling temperature |
| `--max-tokens` | int | 16384 | Max tokens |
| `--validate` | bool | false | Validate with `lake build` |
| `--escalate` | bool | false | Auto-escalate to Aristotle if sorry remains |

## Aristotle (Harmonic theorem prover)

### `aristotle submit`
Submit a Lean project for long-running sorry-filling.

```bash
$QEDGEN aristotle submit --project-dir formal_verification --wait
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--project-dir` | Path | required | Lean project directory |
| `--prompt` | String | "Fill in all sorry..." | Custom prompt |
| `--output-dir` | Path | same as project-dir | Output directory |
| `--wait` | bool | false | Block until completion |
| `--poll-interval` | int (sec) | 30 | Polling interval; clamped to [5, 3600] |

### `aristotle status`
Check project status; with `--wait`, poll until terminal and download the result.

```bash
$QEDGEN aristotle status <project-id>
$QEDGEN aristotle status <project-id> --wait --output-dir formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `<project-id>` | String | required | Project ID returned by `aristotle submit` |
| `--wait` | bool | false | Poll until terminal status, then download |
| `--poll-interval` | int (sec) | 30 | Polling interval; clamped to [5, 3600]. Requires `--wait` |
| `--output-dir` | Path | `.` | Where to extract the result. Requires `--wait` |

### `aristotle result`
Download a completed project's solution archive.

```bash
$QEDGEN aristotle result <project-id> --output-dir formal_verification
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `<project-id>` | String | required | Project ID |
| `--output-dir` | Path | `.` | Where to extract the result |

### `aristotle cancel`
Cancel a running project.

```bash
$QEDGEN aristotle cancel <project-id>
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `<project-id>` | String | required | Project ID to cancel |

### `aristotle list`
List recent projects.

```bash
$QEDGEN aristotle list
$QEDGEN aristotle list --limit 25 --status IN_PROGRESS
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--limit` | int | 10 | Maximum number of projects to show |
| `--status` | String | none | Filter by status (e.g. `IN_PROGRESS`, `COMPLETE`, `FAILED`) |

## Mainnet readiness

QEDGen embeds the ratchet rule engine for upgrade-safety lints over
Anchor IDLs — separate from the spec/proof gates above. `readiness`
runs the **P-rule preflight** (one IDL); `check-upgrade` runs the
**R-rule diff** (old vs new IDL). Both exit `0` for additive/safe,
`1` for breaking, `2` for unsafe. Both are linked in as a library —
no standalone `ratchet` CLI on PATH after `install.sh` /
`npx skills add`; use these wrappers instead.

### `readiness`
Lint one Anchor IDL for mainnet-readiness before first deploy. Catches
upgrade landmines before the program ever ships: missing `version: u8`
prefix, no `_reserved` trailing padding, unpinned discriminators, name
collisions, writable accounts with no signer.

```bash
# Standard preflight
$QEDGEN readiness --idl target/idl/my_program.json

# JSON for CI
$QEDGEN readiness --idl target/idl/my_program.json --json

# Print the rule catalog and exit
$QEDGEN readiness --list-rules
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | required | Anchor IDL JSON (typically `target/idl/<program>.json`) |
| `--list-rules` | bool | false | Print the catalog of P-rules applied and exit |
| `--json` | bool | false | Machine-readable output |

### `check-upgrade`
Diff an old vs new Anchor IDL and flag every upgrade-unsafe change.
Catches the failure modes `solana program upgrade` won't — field
reorders, discriminator changes, orphaned accounts, PDA seed drift,
signer/writable tightening.

```bash
# Standard upgrade diff
$QEDGEN check-upgrade --baseline old.json --candidate new.json

# Acknowledge a specific finding so it reports as Additive
$QEDGEN check-upgrade --baseline old.json --candidate new.json \
  --ack R007=ProgramId

# Declare a migration / realloc was added in source
$QEDGEN check-upgrade --baseline old.json --candidate new.json \
  --has-migration TreasuryV2 --has-realloc UserConfig

# Print the rule catalog and exit
$QEDGEN check-upgrade --list-rules
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--baseline` | Path | required | Baseline IDL (the one on-chain today) |
| `--candidate` | Path | required | Candidate IDL (the one the upgrade would ship) |
| `--ack` | String | - | Acknowledge a specific finding so it reports as Additive (repeatable). Pass `--list-rules` to see the full flag catalog. |
| `--has-migration` | String | - | Declare an account as having a migration in source; demotes R003/R004 findings for that account to Additive (repeatable) |
| `--has-realloc` | String | - | Declare an account as having `realloc = ...` in source; demotes R005 for that account to Additive (repeatable) |
| `--list-rules` | bool | false | Print the catalog of R-rules applied and exit |
| `--json` | bool | false | Machine-readable output |

## Utility

### `consolidate`
Merge multiple proof projects into a single Lean project.

```bash
$QEDGEN consolidate --input-dir /tmp/proofs --output-dir formal_verification
```

## Environment variables

| Variable | Required for | Description |
|---|---|---|
| `MISTRAL_API_KEY` | `generate`, `fill-sorry` | Mistral API key. Free at [console.mistral.ai](https://console.mistral.ai) |
| `ARISTOTLE_API_KEY` | `aristotle` commands | Harmonic API key. Get at [aristotle.harmonic.fun](https://aristotle.harmonic.fun) |
| `QEDGEN_HOME` | - | Override global home directory (default: `~/.qedgen/`) |
| `QEDGEN_VALIDATION_WORKSPACE` | - | Override validation workspace path |

## Error handling

| Error | Fix |
|---|---|
| `qedgen requires a git repo` | Run `git init` in the project root |
| First `lake build` is slow | Without Mathlib: seconds. With `--mathlib`: 15-45 min first time, cached after. |
| `could not resolve 'HEAD' to a commit` | Remove `.lake/packages/mathlib`, run `lake update` |
| Rate limiting (429) | Built-in exponential backoff in `fill-sorry` |
