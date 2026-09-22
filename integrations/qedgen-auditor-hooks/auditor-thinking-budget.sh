#!/bin/sh
# auditor-thinking-budget.sh — UserPromptSubmit hook for qedgen-auditor.
#
# Reads a Claude Code UserPromptSubmit hook JSON payload on stdin, detects
# audit-trigger phrases in the prompt, and appends "ultrathink" to the prompt
# so supported Claude sessions request an extended thinking budget. The auditor's
# §3c trust-surface walk and authority-side intent-drift sweep collapse to
# surface-level pattern matching without sustained reasoning.
#
# Contract:
#   - Idempotent: no-op if the prompt already contains "ultrathink".
#   - Silent on non-matching prompts: payload passes through unchanged.
#   - Requires jq.
#
# Exit codes: 0 always (emit payload on stdout); 2 only if jq is missing.

set -eu

if ! command -v jq >/dev/null 2>&1; then
  echo "auditor-thinking-budget.sh: jq not found in PATH" >&2
  exit 2
fi

payload=$(cat)
prompt=$(printf '%s' "$payload" | jq -r '.prompt // ""')

# Idempotent: already lifted, pass through.
if printf '%s' "$prompt" | grep -qF 'ultrathink'; then
  printf '%s' "$payload"
  exit 0
fi

# Trigger phrases — case-insensitive, word-boundary aware where it matters.
trigger_regex='(/?qedgen[- ]auditor)|(audit (my|this|the) (program|contract))|(security audit)|(review for vulnerabilities)|(check for security issues)|(find (bugs in|vulnerabilities))'

if printf '%s' "$prompt" | grep -iqE "$trigger_regex"; then
  printf '%s' "$payload" | jq -c '.prompt = (.prompt + "\n\nultrathink")'
else
  printf '%s' "$payload"
fi
