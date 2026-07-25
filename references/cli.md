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

## Command index

Load only the page you need. Every command below is documented with its full
flag set on the linked page. For the authoritative, always-current flag list of
any single command, `qedgen <command> --help` is generated from the same clap
definitions.

| Page | Commands |
|---|---|
| [project-setup](cli/project-setup.md) | `init`, `setup`, `asm2lean` |
| [spec-authoring](cli/spec-authoring.md) | `interface`, `spec`, `adapt`, `stamp` |
| [validation](cli/validation.md) | `check`, `reconcile`, `verify` |
| [probe](cli/probe.md) | `probe`, `ratify` |
| [codegen](cli/codegen.md) | `codegen` |
| [proofs](cli/proofs.md) | `generate`, `fill-sorry`, `aristotle` (`submit`, `status`, `result`, `cancel`, `list`) |
| [deploy](cli/deploy.md) | `readiness`, `check-upgrade`, `descriptor`, `discharge` |
| [utility](cli/utility.md) | `consolidate`, `feedback` |

## Environment variables

| Variable | Required for | Description |
|---|---|---|
| `MISTRAL_API_KEY` | `generate`, `fill-sorry` | Mistral API key. Free at [console.mistral.ai](https://console.mistral.ai) |
| `ARISTOTLE_API_KEY` | `aristotle` commands | Harmonic API key. Get at [aristotle.harmonic.fun](https://aristotle.harmonic.fun) |
| `QEDGEN_HOME` | - | Override global home directory (default: `~/.qedgen/`) |
| `QEDGEN_VALIDATION_WORKSPACE` | - | Override validation workspace path |
| `QEDGEN_FEEDBACK_REPO` | `feedback` | Override the issue target (default: `QEDGen/solana-skills`) |

## Error handling

| Error | Fix |
|---|---|
| `qedgen requires a git repo` | Run `git init` in the project root |
| First `lake build` is slow | Without Mathlib: seconds. With `--mathlib`: 15-45 min first time, cached after. |
| `could not resolve 'HEAD' to a commit` | Remove `.lake/packages/mathlib`, run `lake update` |
| Rate limiting (429) | Built-in exponential backoff in `fill-sorry` |
