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
Scaffold a new formal verification project. Creates `.qed/` project state directory.

```bash
$QEDGEN init --name escrow
$QEDGEN init --name dropset --asm src/dropset.s
$QEDGEN init --name engine --mathlib
$QEDGEN init --name counter --quasar
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--name` | String | required | Project name (alphanumeric + underscores) |
| `--asm` | Path | - | sBPF assembly source (runs asm2lean automatically) |
| `--mathlib` | bool | false | Include Mathlib dependency |
| `--quasar` | bool | false | Generate Quasar program + Kani harnesses + tests |
| `--output-dir` | Path | `./formal_verification` | Output directory |

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

### `spec`
Generate SPEC.md or .qedspec from IDL or .qedspec.

```bash
$QEDGEN spec --idl target/idl/program.json
$QEDGEN spec --idl target/idl/program.json --format qedspec
$QEDGEN spec --from-spec my_program.qedspec --proofs formal_verification/
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | - | Anchor IDL JSON file |
| `--from-spec` | Path | - | .qedspec file (alternative to --idl) |
| `--proofs` | Path | - | Proofs directory (for status checking) |
| `--output-dir` | Path | `./formal_verification` | Output directory |
| `--format` | String | `md` | Output format: `md` or `qedspec` |

### `check`
Validate a spec — lint, coverage, drift, and verification report. Default (no flags) runs lint.

Requires a git repo (see [Require-git guard](#require-git-guard)).

```bash
# Lint (always runs)
$QEDGEN check --spec my_program.qedspec
$QEDGEN check --spec my_program.qedspec --json

# Coverage matrix
$QEDGEN check --spec my_program.qedspec --coverage

# Verification report
$QEDGEN check --spec my_program.qedspec --explain
$QEDGEN check --spec my_program.qedspec --explain --output report.md

# Drift detection
$QEDGEN check --spec my_program.qedspec --drift programs/src/
$QEDGEN check --spec my_program.qedspec --drift programs/src/ --deep
$QEDGEN check --spec my_program.qedspec --drift programs/src/ --update-hashes

# Unified code + kani drift
$QEDGEN check --spec my_program.qedspec --code programs/my_program/ --kani tests/kani.rs

# sBPF verification (hash check + lake build)
$QEDGEN check --spec my_program.qedspec --asm src/program.s
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (.qedspec) |
| `--proofs` | Path | `./formal_verification` | Proofs directory |
| `--coverage` | bool | false | Show operation × property matrix |
| `--explain` | bool | false | Generate Markdown verification report |
| `--output` | Path | stdout | Output file for --explain |
| `--drift` | Path | - | Rust source path for #[qed(verified)] drift detection |
| `--update-hashes` | bool | false | Auto-stamp hashes in source files |
| `--deep` | bool | false | Transitive drift detection (check callees) |
| `--code` | Path | - | Quasar program dir (code drift detection) |
| `--kani` | Path | - | Kani harness file (Kani drift detection) |
| `--asm` | Path | - | sBPF assembly source (hash check + lake build) |
| `--json` | bool | false | Machine-readable output |

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

## Code generation

### `codegen`
Generate committed artifacts from a qedspec. Default (no flags) generates
the Quasar Rust skeleton only.

Requires a git repo (see [Require-git guard](#require-git-guard)).

```bash
# Rust skeleton only
$QEDGEN codegen --spec my_program.qedspec

# Everything
$QEDGEN codegen --spec my_program.qedspec --all

# Selective
$QEDGEN codegen --spec my_program.qedspec --lean
$QEDGEN codegen --spec my_program.qedspec --kani
$QEDGEN codegen --spec my_program.qedspec --test
$QEDGEN codegen --spec my_program.qedspec --proptest
$QEDGEN codegen --spec my_program.qedspec --integration
$QEDGEN codegen --spec my_program.qedspec --ci
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (.qedspec) |
| `--output-dir` | Path | `./programs` | Output directory for Rust skeleton |
| `--all` | bool | false | Generate all artifacts |
| `--lean` | bool | false | Generate Lean 4 proofs |
| `--lean-output` | Path | `./formal_verification/Spec.lean` | Lean output path |
| `--kani` | bool | false | Generate Kani proof harnesses |
| `--kani-output` | Path | `./tests/kani.rs` | Kani output path |
| `--test` | bool | false | Generate unit tests |
| `--test-output` | Path | `./src/tests.rs` | Unit test output path |
| `--proptest` | bool | false | Generate proptest harnesses |
| `--proptest-output` | Path | `./tests/proptest.rs` | Proptest output path |
| `--integration` | bool | false | Generate QuasarSVM integration tests |
| `--integration-output` | Path | `./src/integration_tests.rs` | Integration test output path |
| `--ci` | bool | false | Generate GitHub Actions CI workflow |
| `--ci-output` | Path | `.github/workflows/verify.yml` | CI workflow output path |
| `--ci-asm` | String | - | sBPF assembly source (for CI verify step) |

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
| `tests/kani.rs` | Always regenerated |
| `tests/proptest.rs` | Always regenerated |
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
| `--poll-interval` | int (sec) | 30 | Polling interval (5-3600) |

### `aristotle status`
Check or poll project status.

```bash
$QEDGEN aristotle status <project-id>
$QEDGEN aristotle status <project-id> --wait --output-dir formal_verification
```

### `aristotle result`
Download completed result.

```bash
$QEDGEN aristotle result <project-id> --output-dir formal_verification
```

### `aristotle cancel` / `aristotle list`
Cancel a running project or list recent projects.

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
