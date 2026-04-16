# CLI Reference

All commands are run via the wrapper: `$QEDGEN <command> [flags]`

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

## Code generation

### `codegen`
Generate committed artifacts from a qedspec. Default (no flags) generates Quasar Rust skeleton only.

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

## Drift detection (`qedgen-macros`)

The `qedgen-macros` crate provides `#[qed(verified, hash = "...")]` for compile-time drift detection:

```rust
use qedgen_macros::qed;

#[qed(verified, hash = "5af369bb254368d3")]
pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    // If this body changes, compilation fails with:
    // "qed: verified function `deposit` has changed since verification"
}
```

Hash covers function signature + body, excluding attributes and comments. Use `check --drift --update-hashes` to stamp hashes, `check --drift` to detect changes.

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
| First `lake build` is slow | Without Mathlib: seconds. With `--mathlib`: 15-45 min first time, cached after. |
| `could not resolve 'HEAD' to a commit` | Remove `.lake/packages/mathlib`, run `lake update` |
| Rate limiting (429) | Built-in exponential backoff in `fill-sorry` |
