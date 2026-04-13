# CLI Reference

All commands are run via the wrapper: `$QEDGEN <command> [flags]`

## Project setup

### `init`
Scaffold a new formal verification project.

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
| `--temperature` | float | 0.6 | Sampling temperature (0.0-2.0) |
| `--max-tokens` | int | 16384 | Max tokens per completion |
| `--validate` | bool | false | Validate with `lake build` |
| `--mathlib` | bool | false | Include Mathlib in validation workspace |

### `fill-sorry`
Fill sorry markers in a Lean file using Leanstral.

```bash
$QEDGEN fill-sorry --file formal_verification/Proofs/Hard.lean --validate
$QEDGEN fill-sorry --file formal_verification/Proofs/Hard.lean --escalate
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

| Flag | Type | Default | Description |
|---|---|---|---|
| `--wait` | bool | false | Poll until terminal state + auto-download |
| `--poll-interval` | int (sec) | 30 | Polling interval (5-3600) |
| `--output-dir` | Path | `.` | Download destination |

### `aristotle result`
Download completed result.

```bash
$QEDGEN aristotle result <project-id> --output-dir formal_verification
```

### `aristotle cancel`
Cancel a running project.

### `aristotle list`
List recent projects.

| Flag | Type | Default | Description |
|---|---|---|---|
| `--limit` | int | 10 | Max projects to show |
| `--status` | String | - | Filter by status (IN_PROGRESS, COMPLETE, FAILED) |

## Spec and inspection

### `spec`
Generate SPEC.md or .qedspec from IDL or .qedspec.

```bash
$QEDGEN spec --idl target/idl/program.json
$QEDGEN spec --idl target/idl/program.json --format qedspec
$QEDGEN spec --from-spec my_program.qedspec --proofs formal_verification/Proofs/
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--idl` | Path | - | Anchor IDL JSON file |
| `--from-spec` | Path | - | .qedspec file (alternative to --idl) |
| `--proofs` | Path | - | Proofs directory (for status checking) |
| `--output-dir` | Path | `./formal_verification` | Output directory |
| `--format` | String | `md` | Output format: `md` (SPEC.md) or `qedspec` (.qedspec scaffold) |

### `check`
Check spec coverage and drift detection.

```bash
$QEDGEN check --spec formal_verification/Spec.lean --proofs formal_verification/Proofs/
$QEDGEN check --spec Spec.lean --proofs Proofs/ --code programs/my_program/ --kani tests/kani.rs
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file (Spec.lean) |
| `--proofs` | Path | `./formal_verification/Proofs` | Proofs directory |
| `--code` | Path | - | Quasar program dir (enables code drift detection) |
| `--kani` | Path | - | Kani harness file (enables Kani drift detection) |

### `explain`
Generate human-readable verification report.

```bash
$QEDGEN explain --spec Spec.lean --proofs formal_verification/
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file |
| `--proofs` | Path | `./formal_verification` | Proofs directory |
| `--output` | Path | stdout | Output file |

### `lint`
Lint a qedspec for completeness. Returns priority-ordered findings with concrete fix suggestions.

```bash
$QEDGEN lint --spec my_program.qedspec
$QEDGEN lint --spec my_program.qedspec --json
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--spec` | Path | required | Spec file |
| `--json` | bool | false | Output as JSON (includes `priority` field: 1=security, 2=correctness, 3=completeness, 4=quality, 5=polish) |

### `verify`
Verify sBPF proofs: check source hash, regenerate if stale, run lake build.

```bash
$QEDGEN verify --asm src/program.s --proofs formal_verification/
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--asm` | Path | required | sBPF assembly source |
| `--proofs` | Path | `./formal_verification` | Proofs directory |

## Code generation

### `codegen`
Generate Quasar program skeleton from spec.

```bash
$QEDGEN codegen --spec Spec.lean --output-dir programs/my_program/
```

### `kani`
Generate Kani proof harnesses from spec.

```bash
$QEDGEN kani --spec Spec.lean --output tests/kani.rs
```

### `test`
Generate unit tests (plain Rust, cargo test).

```bash
$QEDGEN test --spec Spec.lean --output src/tests.rs
```

### `integration-test`
Generate QuasarSVM integration test scaffolds.

```bash
$QEDGEN integration-test --spec my_program.qedspec --output src/integration_tests.rs
```

### `lean-gen`
Generate Lean 4 file from .qedspec format.

```bash
$QEDGEN lean-gen --spec my_program.qedspec --output formal_verification/Spec.lean
```

## CI

### `ci`
Generate GitHub Actions workflow for verification CI.

```bash
$QEDGEN ci --output .github/workflows/verify.yml
$QEDGEN ci --output .github/workflows/verify.yml --asm src/program.s
```

| Flag | Type | Default | Description |
|---|---|---|---|
| `--output` | Path | `.github/workflows/verify.yml` | Workflow file |
| `--asm` | String | - | sBPF assembly source (adds verify step) |

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
| `QEDGEN_VALIDATION_WORKSPACE` | - | Override validation workspace path (default: `~/.qedgen/workspace/`) |

## Error handling

| Error | Fix |
|---|---|
| First `lake build` is slow | Without Mathlib: seconds. With `--mathlib`: 15-45 min first time, cached after. |
| `could not resolve 'HEAD' to a commit` | Remove `.lake/packages/mathlib`, run `lake update` |
| Rate limiting (429) | Built-in exponential backoff in `fill-sorry` |
