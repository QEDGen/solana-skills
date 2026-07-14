# Model Selection

Choose models by capability first; provider names are venue-specific mappings,
not portable requirements.

## Capability policy

| Work | Default capability | Cost-sensitive fallback |
|---|---|---|
| Security-audit discovery | Frontier reasoning/coding model, high reasoning | Balanced frontier model, high reasoning |
| High-assurance independent pass | Frontier reasoning/coding model, highest practical reasoning | Do not downgrade without recording it |
| Quick audit | Balanced frontier model, high reasoning | Smaller model only for deterministic preflight/probe triage |
| Finding reconciliation/judging | Balanced frontier model, high reasoning, structured output | Same model at medium reasoning |
| Deterministic extraction/normalization | Balanced or smaller reliable model | Venue's low-cost structured-output model |

Do not use a cheap judge to compensate for weak discovery: the judge can match
reported findings but cannot recover findings the audit worker missed. Record
the exact model identifier and reasoning setting in every benchmark result.

## Anthropic mapping

As of July 2026:

- Default audit worker: Claude Fable 5 (`claude-fable-5`) with extended
  thinking at the highest available budget. On Claude Code the bundled
  `hooks/` adapter lifts the budget automatically on audit-trigger phrases.
- Fallback audit worker: Claude Opus 4.8 (`claude-opus-4-8`) with extended
  thinking when Fable 5 is unavailable.
- Reconciliation judge: Claude Sonnet 5 (`claude-sonnet-5`) with structured
  JSON output.
- Deterministic extraction/normalization: Claude Haiku 4.5
  (`claude-haiku-4-5`).

Anthropic models are the reliability-preferred audit workers: they complete
security-audit workloads without provider-policy interruption (see the OpenAI
limitation below).

## OpenAI mapping

As of July 2026:

- Default audit worker: `gpt-5.6-sol` with `high` reasoning. Use `xhigh` for a
  high-assurance pass when the venue exposes it and budget permits.
- Quick/cost-balanced audit worker: `gpt-5.6-terra` with `high` reasoning.
- Reconciliation judge: `gpt-5.6-terra` with `high` reasoning and structured
  JSON output.
- Do not default new runs to GPT-5.5. Preserve it only for historical baseline
  comparisons that intentionally measure model drift.

The `gpt-5.6` alias currently routes to GPT-5.6 Sol, but benchmarks should pin
the explicit `gpt-5.6-sol` identifier so later alias changes do not corrupt
comparability.

Known limitation: security-audit workloads can trigger OpenAI's cybersecurity
policy mid-run, causing the worker to stop or refuse partway through
discovery. Treat any mid-run stop or refusal as policy interference: never
report that pass as complete or clean. Rerun on the Anthropic mapping, or
record the run as `policy-interfered` (excluded from benchmark metrics per the
benchmark contract).

Before changing this mapping, verify the provider's current official model
guidance. Other providers should map the capability policy to their own current
frontier and balanced tiers without editing the core workflow.
