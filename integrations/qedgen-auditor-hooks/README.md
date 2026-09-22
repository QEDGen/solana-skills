# qedgen-auditor — optional Claude Code thinking-budget adapter

This is a venue-specific optional adapter, not part of the portable audit
workflow. Other skills.sh-compatible venues should ignore this adapter and use
their own reasoning-budget controls when available.

Normal skill installation does not package, install, or enable this hook and
does not modify agent settings. Activation requires the manual steps below;
removal is also manual.

A Claude Code `UserPromptSubmit` hook that detects audit-trigger phrases and
appends `ultrathink` to the prompt so Fable 5 / Opus 4.8 sessions allocate
maximum thinking budget.

## Why

The auditor's trust-surface walk and authority-side intent-drift sweep require
sustained multi-step reasoning across a program's dependency graph and its
documented invariants. On default thinking budgets, the catalog can collapse to
surface-level pattern matching and miss the cross-cutting findings that justify
the skill.

The thinking budget is decided at prompt-submit time, *before* the model
chooses to invoke the skill — so no inside-the-skill mechanism (SKILL.md text,
a tool result, a `PreToolUse` hook) can lift it. The only fix is a
harness-level hook fired on user prompt submission.

## What it does

When the submitted prompt matches one of the trigger phrases below
(case-insensitive), the hook appends `\n\nultrathink` to the prompt before it
reaches the model. On Fable 5 / Opus 4.8 this allocates the maximum thinking
budget; on Sonnet / Haiku the word is a no-op or a partial lift (no harm).

Trigger phrases:

- `/qedgen-auditor`, `qedgen-auditor`, `qedgen auditor`
- `audit my program`, `audit this program`, `audit the program`
- `audit my contract`, `audit this contract`
- `security audit`
- `review for vulnerabilities`
- `check for security issues`
- `find bugs in …`, `find vulnerabilities`

The hook is idempotent (no-op if `ultrathink` is already present) and silent
on non-matching prompts (payload passes through unchanged).

It receives the full hook JSON payload on standard input, inspects only
`.prompt`, and emits JSON on standard output. The script does not open paths
referenced by the repository or transcript, intentionally enumerate environment
variables, or make network requests. Enabling it nevertheless exposes each
submitted prompt to this local hook process, as required by Claude Code's
`UserPromptSubmit` interface.

## Install

Three manual steps from a source checkout. Keep the adapter outside the skill
installation directory so a skill update cannot delete it.

1. **Copy the hook into stable user-owned storage and make it executable:**

   ```sh
   mkdir -p "$HOME/.local/share/qedgen-auditor-hooks"
   cp integrations/qedgen-auditor-hooks/auditor-thinking-budget.sh \
     "$HOME/.local/share/qedgen-auditor-hooks/"
   chmod 700 "$HOME/.local/share/qedgen-auditor-hooks/auditor-thinking-budget.sh"
   ```

2. **Merge `settings.snippet.json` into `~/.claude/settings.json` under
   `hooks.UserPromptSubmit`.** If you don't already have a
   `UserPromptSubmit` block, copy the snippet wholesale. Otherwise add the
   inner hook entry (the `{ "type": "command", "command": "..." }`) into the
   existing `hooks` array.

   The snippet uses `$HOME/.local/share/qedgen-auditor-hooks/...` — replace
   `$HOME` with the absolute path if your `settings.json` doesn't expand
   environment variables (most setups do).

3. **Requires `jq`.** Install via `brew install jq` / `apt install jq`.

## Verify

Echo a trigger phrase into the hook directly and confirm `ultrathink` gets
appended to the `prompt` field:

```sh
echo '{"prompt":"please run /qedgen-auditor on this repo"}' \
  | ~/.local/share/qedgen-auditor-hooks/auditor-thinking-budget.sh
```

Expected output: the same JSON with `prompt` ending in `\n\nultrathink`.

A non-trigger prompt should pass through unchanged:

```sh
echo '{"prompt":"what time is it"}' \
  | ~/.local/share/qedgen-auditor-hooks/auditor-thinking-budget.sh
```

Inside Claude Code, invoke `/qedgen-auditor` on any program and confirm the
session shows extended-thinking traces. If thinking blocks are absent or
short, the hook isn't wired in — re-check `settings.json` and the hook's
executable bit.

## Uninstall

Remove the hook entry from `~/.claude/settings.json` and (optionally) delete
`~/.local/share/qedgen-auditor-hooks/`.
