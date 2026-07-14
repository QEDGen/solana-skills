# qedgen-auditor — optional Claude Code thinking-budget adapter

This is a venue-specific optional adapter, not part of the portable audit
workflow. Other skill.sh-compatible venues should ignore `hooks/` and use
their own reasoning-budget controls when available.

A Claude Code `UserPromptSubmit` hook that detects audit-trigger phrases and
appends `ultrathink` to the prompt so Fable 5 / Opus 4.8 sessions allocate
maximum thinking budget.

## Why

The auditor's §3c trust-surface walk and authority-side intent-drift sweep
require sustained multi-step reasoning across a program's dependency graph and
its documented invariants. On default thinking budgets, the catalog collapses
to surface-level pattern matching and misses exactly the cross-cutting
findings that justify the skill (project_auditor_best_models.md).

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

## Install

Two manual steps.

1. **Make the hook executable.** Adjust the path for your skill install
   location:

   ```sh
   chmod +x ~/.claude/skills/qedgen-auditor/hooks/auditor-thinking-budget.sh
   ```

2. **Merge `settings.snippet.json` into `~/.claude/settings.json` under
   `hooks.UserPromptSubmit`.** If you don't already have a
   `UserPromptSubmit` block, copy the snippet wholesale. Otherwise add the
   inner hook entry (the `{ "type": "command", "command": "..." }`) into the
   existing `hooks` array.

   The snippet uses `$HOME/.claude/skills/qedgen-auditor/...` — replace
   `$HOME` with the absolute path if your `settings.json` doesn't expand
   environment variables (most setups do).

3. **Requires `jq`.** Install via `brew install jq` / `apt install jq`.

## Verify

Echo a trigger phrase into the hook directly and confirm `ultrathink` gets
appended to the `prompt` field:

```sh
echo '{"prompt":"please run /qedgen-auditor on this repo"}' \
  | ~/.claude/skills/qedgen-auditor/hooks/auditor-thinking-budget.sh
```

Expected output: the same JSON with `prompt` ending in `\n\nultrathink`.

A non-trigger prompt should pass through unchanged:

```sh
echo '{"prompt":"what time is it"}' \
  | ~/.claude/skills/qedgen-auditor/hooks/auditor-thinking-budget.sh
```

Inside Claude Code, invoke `/qedgen-auditor` on any program and confirm the
session shows extended-thinking traces. If thinking blocks are absent or
short, the hook isn't wired in — re-check `settings.json` and the hook's
executable bit.

## Uninstall

Remove the hook entry from `~/.claude/settings.json` and (optionally) delete
this directory.
