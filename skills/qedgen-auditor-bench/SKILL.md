---
name: qedgen-auditor-bench
description: Benchmark the QEDGen auditor against sanitized Solana programs with labeled third-party or independently verified findings. Use when the user asks to benchmark, evaluate, regression-test, measure recall or precision, compare auditor versions, or compare auditor models. Produces per-run and union recall, precision, severity agreement, evidence quality, reproducer fire rate, cost, and latency without leaking ground truth to audit workers.
---

# QEDGen Auditor Benchmark

Measure the portable auditor skill without leaking labeled findings into audit
workers. Require a venue-owned benchmark directory; never assume a particular
home directory or agent harness.

## Inputs

Require:

- canonical auditor skill directory;
- corpus manifest containing repository, audited commit, program root, runtime,
  setup/test commands, sanitization rules, and labeled findings;
- explicit benchmark output directory;
- audit profile and run count;
- exact discovery and judge model identifiers when the venue permits selection.

Default to one corpus entry and two independent audit runs for a regression
check. Use three only for a high-assurance benchmark. Never use an unbounded
run-until-dry loop.

## Model policy

Read the auditor's `references/model-selection.md` before dispatch.

For OpenAI-backed venues:

- discovery worker: `gpt-5.6-sol`, `high` reasoning;
- high-assurance discovery: `gpt-5.6-sol`, `xhigh` reasoning when available;
- reconciliation judge: `gpt-5.6-terra`, `high` reasoning;
- deterministic extraction/normalization: `gpt-5.6-terra`, medium or high
  reasoning according to complexity.

Pin explicit identifiers in scored runs. Do not use aliases. Preserve older
models only in an intentionally labeled model-regression comparison.

For other providers, select the current frontier reasoning/coding tier for
discovery and the balanced frontier tier for judging. Record the mapping.

## Per-entry pipeline

### 1. Establish a live baseline

- Check out exactly the labeled commit into an isolated worktree.
- Verify remediation commits are not ancestors of the baseline.
- Spot-check at least one labeled vulnerable location.
- Exclude labels that are fixed or not present at that commit.
- Record the baseline verification evidence.

Never score recall against a tree where the expected vulnerability is absent.

### 2. Sanitize independently of the worker

Remove audit reports, findings, `.qed/`, security reviews, remediation notes,
and firm-specific artifacts. Scan remaining documentation for finding IDs,
severity headings, and descriptions. Record every removal in `sanitize.log`.

The audit worker may read only:

- its sanitized source tree;
- the canonical auditor skill and routed references;
- explicitly supplied QEDGen/test binaries.

It must not read the corpus manifest, labels, reports, prior benchmark results,
or sibling runs.

### 3. Run deterministic preflight

Invoke the auditor's `scripts/preflight.sh` against the selected program root.
Record target, runtime, spec, tool versions, compilation status, and available
reproducer lanes. A build failure is a benchmark outcome, not a recall score.

### 4. Dispatch clean audit workers

Each worker receives the same sanitized snapshot and this task only:

```text
Audit the selected Solana program using the supplied qedgen-auditor skill.
Derive findings only from the sanitized source. Follow its evidence and
reproducer contracts. Write the full report and return its digest.
```

Run workers in isolated contexts. Independent workers are a venue capability;
when unavailable, stop and report that an unbiased model benchmark cannot run.
Do not substitute a ground-truth-contaminated coordinator session.

### 5. Re-run evidence

Independently execute every reproducer reported as fired. Downgrade any result
that does not fire cleanly. Preserve source-established structural findings but
do not count hypotheses as vulnerabilities.

### 6. Normalize and reconcile

Normalize reports to:

```json
{
  "id": "string",
  "severity": "critical|high|medium|low|info",
  "evidence": "confirmed|structural|hypothesis|rejected",
  "category": "string",
  "location": "path:line",
  "root_cause": "string",
  "title": "string",
  "repro_status": "fired|inconclusive|silent|not-required"
}
```

Generate deterministic match candidates using category, path, handler, and root
cause. Give the judge only the two normalized reports. The judge may match or
reject pairs but must not invent or re-grade findings.

Deduplicate the union by root cause and affected operation, not just line
number. Preserve which worker found each item.

### 7. Score

Emit per-run and union metrics:

- recall by firm severity and overall;
- precision over confirmed and structural vulnerability findings;
- hypothesis rate;
- severity agreement on matched findings;
- reproducer fire rate for HIGH/CRITICAL candidates;
- unique-real and unique-noise counts;
- per-run variance and union gain;
- build/preflight success;
- time to first confirmed MED+;
- total wall time and, when exposed, token/cost usage;
- exact skill version, commit, model identifiers, reasoning settings, and
  QEDGen version.

Do not count fixed ground truth as a miss. Do not count hypotheses as true
positives. Report structural precision separately from confirmed precision.

### 8. Detect invalid runs

Assign a run-level status before computing recall:

- `scored`: the worker completed the authorized audit without external
  interference.
- `build-blocked`: source review completed, but executable evidence was blocked;
  score structural recall only and exclude reproducer metrics.
- `policy-interfered`: a venue safety or cybersecurity policy refused,
  truncated, redirected, or suppressed the authorized analysis. Exclude the run
  from recall, precision, severity, and model comparisons.
- `tooling-error`: preflight, source access, or required venue capability failed
  before meaningful analysis. Exclude the run from security metrics.
- `contaminated`: the worker could access labels, reports, prior results, or
  coordinator context. Exclude the run entirely.

Policy interference is not a finding miss. Preserve the refusal or moderation
signal in the run metadata so benchmark dashboards distinguish model behavior
from venue-policy behavior. When real third-party targets trigger policy
interference, move capability evaluation to intentionally vulnerable local
fixtures or another explicitly authorized test target rather than weakening
the venue's safeguards.

## Comparison modes

- Skill regression: same model, corpus, commit, prompts, and run count; change
  only the skill version.
- Model regression: same skill and corpus; change only the model identifier and
  reasoning setting.
- End-to-end release score: current default skill and model; compare against the
  prior release but label both changed dimensions.

Avoid changing model and skill simultaneously when attributing improvement.

## Output

Write under the explicit benchmark directory:

```text
<output>/<entry>/<run-id>/
  baseline.json
  sanitize.log
  run-1/report.md
  run-1/repro.log
  run-N/...
  matches.json
  score.json
  summary.md
```

Never publish or commit third-party vulnerability details automatically.
Maintain responsible-disclosure handling for unmatched HIGH/CRITICAL findings.
