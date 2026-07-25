# CLI: proof generation

`generate`, `fill-sorry`, and the `aristotle` subcommands.

Part of the [CLI Reference](../cli.md).

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

