# CLI: utility

`consolidate`, `feedback`.

Part of the [CLI Reference](../cli.md).

## Utility

### `consolidate`
Merge multiple proof projects into a single Lean project.

```bash
$QEDGEN consolidate --input-dir /tmp/proofs --output-dir formal_verification
```

### `feedback`
File a GitHub issue with the last command's failure context.

```bash
# Walk through the most recent failure (reads `.qed/last-error.log`).
$QEDGEN feedback --note "lint flags X but my spec declares it"

# Print the title and body without filing anything.
$QEDGEN feedback --dry-run

# Skip the interactive confirmation (CI / scripts).
$QEDGEN feedback --yes
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--note <text>` | string | — | Free-form description of what happened. Top of the issue body. |
| `--title <text>` | string | auto | Override the derived title (`[qedgen <version>] <command> failed: <line>`). |
| `--spec <path>` | path | auto | Override the auto-resolved `.qedspec` path used for the excerpt. |
| `--dry-run` | bool | false | Print to stdout; no local artifact, no remote submission. |
| `--yes` | bool | false | Skip the interactive y/N prompt. Required in non-interactive shells. |
| `--no-open` | bool | false | Suppress the browser open on the pre-filled-URL fallback path. |

Submission order: local copy to `.qed/feedback/<timestamp>.md` (silent) → preview → confirmation → `gh issue create` → pre-filled GitHub URL fallback if `gh` is unavailable. Override the target repo with `QEDGEN_FEEDBACK_REPO=owner/repo`.

The bundled context is the most recent command's stderr (captured automatically into `.qed/last-error.{log,json}` by `main()`'s error path), the qedgen version, OS/arch, detected runtime, and a `.qedspec` excerpt centered on the error's line hint when one is parseable.

