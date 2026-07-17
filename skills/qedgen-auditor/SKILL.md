---
name: qedgen-auditor
description: Audit Anchor, Pinocchio, native Rust, or qedgen-generated Solana programs for security vulnerabilities, validate suspected findings with reproducible tests, and identify specification gaps. Use when the user asks to audit, security-review, find vulnerabilities in, or assess the production safety of a Solana program. Prefer confirmed vulnerabilities over advisory noise.
---

# QEDGen Auditor

Audit Solana programs with explicit target resolution, bounded execution, and
evidence separate from impact. Never present an unverified pattern match as a
confirmed vulnerability.

## Required workflow

### 1. Resolve the target and run preflight

Require a concrete program root. If the repository contains multiple plausible
programs and the user did not select one, ask which program to audit.

Resolve paths relative to this skill directory (`<skill-root>`), not the user's
current working directory. Run:

```bash
<skill-root>/scripts/preflight.sh --root <program-root> [--spec <path>] [--qedgen <binary>]
```

Read [runtime detection](references/runtime-detection.md) if detection is
ambiguous or the target contains assembly. Report the resolved root, runtime,
mode, spec, QEDGen version, and available repro lanes before making findings.

Run compilation separately and retain its exit status. Do not say the target
compiles until the command has completed successfully:

```bash
<skill-root>/scripts/preflight.sh --root <program-root> [--spec <path>] --compile
```

Missing or stale QEDGen reduces the audit to read-only; it does not prevent
source review. A failed program build prevents fired Mollusk reproducers but
does not erase source-established evidence.

Record each lane independently: source review, ordinary probe, compilation,
Mollusk, Miri, and Crucible. Failure or an empty result in one lane must not
cancel invariant extraction or be reported as evidence that no bugs exist.

### 2. Select an audit profile

Use `standard` unless the user requested a quick look or high assurance.
Select the model by capability rather than venue name. Read
[model selection](references/model-selection.md) before dispatching an
independent audit worker or benchmark run.

| Profile | Independent passes | Resource ceiling |
|---|---:|---|
| `quick` | 1 focused pass | Obvious HIGH/CRIT paths and cheap repros |
| `standard` | 1 thorough pass | Full applicable catalog; HIGH/CRIT repros |
| `high-assurance` | 2–3 passes | Explicit user request; union and deduplicate |

Do not start an unbounded convergence loop. Parallel or independent workers are
an optional venue capability, never a requirement. Use them only when the venue
permits them and the user or governing instructions authorize delegation.
See [orchestration profiles](references/orchestration-profiles.md).

### 3. Build the work list

Spec-aware:

```bash
qedgen probe --spec <path>
```

Spec-less:

```bash
qedgen probe --bootstrap --root <program-root>
```

Probe output prioritizes investigation; it is not a verdict. Independently walk
the program's handlers, authority graph, state transitions, account identities,
arithmetic, CPIs, and documented invariants.

Read the probe envelope by evidence tier (schema v3, `version: 3`):

- **`findings[]`** — issues backed by a replayable reproducer. Treat as
  confirmed subject to your own review.
- **`candidates[]`** — predicate hits and static patterns that warrant
  investigation but carry **no severity and no reproducer**. These are a work
  list, never results: each is a lead to confirm or dismiss by reading the
  impl, and each `reason` says why it isn't yet a finding (usually "no
  reproducer constructor yet"). Never file a candidate as a finding without
  independently confirming it and attaching a reproducer.
- **`engine_runs[]`** — per-engine `passed | partial | blocked | failed |
  skipped`. A `partial` run lists `skipped_files` it could not read; a
  `blocked` run (e.g. budget-0 fuzz) did not execute. An empty `findings[]`
  next to a `partial`/`blocked` engine is weak evidence, not a clean pass.
- **`outcome`** — `passed_with_coverage` | `no_findings_low_coverage` |
  `blocked_incomplete_harness` | `engine_failed` | `dry_run`. Only
  `passed_with_coverage` licenses "the probe found nothing here"; the rest
  mean the probe under-ran and you must lean on manual review.

Always extract a domain dossier from source, tests, comments, documentation,
and paired operations, even when QEDGen is missing or stale, the ordinary probe
fails or returns no sites, the runtime adapter is unsupported, or compilation
fails. Capture asset flows, quantities and units, lifecycle transitions,
authority capabilities, candidate economic equations, external assumptions,
source anchors, and confidence. Use the dossier for manual review and intent
ratification immediately; preserve it for spec drafting and later executable
verification when blocked lanes recover. Read [domain invariant extraction](references/domain-invariant-extraction.md)
before interviewing the user or drafting domain properties.

Write both `.qed/audit/<timestamp>/domain-dossier.json` and its human-readable
`.md` rendering. Validate the JSON against
`<skill-root>/schemas/domain-dossier.schema.json`; use its stable candidate IDs
in interviews, specs, findings, and fuzz results. Also maintain
`.qed/audit/<timestamp>/run-manifest.json` against
`<skill-root>/schemas/audit-run-manifest.schema.json`. Update each lane as it
starts or finishes; blocked lanes require a concrete reason and exact resume
command. After ratification, require `spec-handoff.json` against
`<skill-root>/schemas/spec-handoff.schema.json`. It separates structural,
domain, and regression layers, links clauses to stable provenance IDs, and
records language gaps instead of flattening unsupported domain semantics into
comments. For each `needs_authoring` domain clause, follow its
`authoring.constructs`, parser-shaped `authoring.template`, and
`authoring.notes`; each language gap states `current_language_support` so
finite sums, nominal dimensions, floor/ceiling/half-up rounding, typed external
fields, lifecycle transitions, and authority guards are not mistaken for
missing syntax. Also require `domain-sequences.json` against
`<skill-root>/schemas/domain-sequences.schema.json`. Use its ratified
setup/forward/reverse/teardown plans as stateful coverage targets, but do not
claim exact-sequence coverage while any `unresolved_parameters` remain or the
runner has not replayed that plan. Validate the artifacts with:

For exact replay, author `domain-sequence-bindings.json` against
`domain-sequence-bindings.schema.json`. Every plan/action/parameter key must be
explicit; never infer accounts or argument values. Domain-mode Crucible writes
`resolved-domain-sequences.json`, validates it against
`resolved-domain-sequences.schema.json`, replays each byte-exact seed before
exploratory fuzzing, and feeds the same corpus into the subsequent run. Each
exact replay appends durable evidence at
`<harness>/.qedgen/domain-replay-report.json`, validated against
`domain-replay-report.schema.json`; the report pins the resolved document,
account overlay, generated harness, seed content, native replay command, exit
status, and plan ID. A seed merely present in the exploratory corpus is not
exact-sequence coverage. Account
targets use the explicit `fixture:<account>` namespace. QEDGen verifies them
against non-PDA, non-default spec accounts, collapses them into
`account-binding-overlay.json`, rejects conflicts across plans, and compiles
that overlay into action account literals and signer selection before encoding
handler arguments. The collapse step also rejects same-action fixture aliasing
and fixture targets whose signer, writable, program, account-type,
authority/owner, or imported namespace constraints cannot satisfy the source
account. Default-address and PDA accounts remain generator-managed and must not
appear in the overlay. For PDAs, QEDGen retains Anchor IDL seed tuples and
derives each action's address from literal, account, numeric argument, and
supported state-field seeds. Dependent PDAs are ordered before their consumers;
unresolved or cyclic derivations stop codegen instead of falling back to an
empty-seed address. Non-init PDAs are materialized on demand, while init targets
remain absent for the instruction to create.

```bash
<skill-root>/scripts/check-domain-artifacts.sh \
  --dossier .qed/audit/<timestamp>/domain-dossier.json \
  --manifest .qed/audit/<timestamp>/run-manifest.json \
  --handoff .qed/audit/<timestamp>/spec-handoff.json \
  --sequences .qed/audit/<timestamp>/domain-sequences.json \
  --bindings .qed/audit/<timestamp>/domain-sequence-bindings.json \
  --resolved-sequences .qed/audit/<timestamp>/resolved-domain-sequences.json \
  --account-overlay .qed/audit/<timestamp>/account-binding-overlay.json \
  --replay-report fuzz/<program>/.qedgen/domain-replay-report.json
```

Read only the relevant detailed references:

- [category catalog](references/category-catalog.md): per-category
  predicates, runtime-specific patterns, and the composition cookbook.
- [manual review passes](references/manual-review-passes.md): the detailed
  investigation workflow when the compact steps here need expansion.
- [report and grading](references/report-and-grading.md): classification
  rules, the severity grading procedure, and the report format.
- [audit handbook](references/audit-handbook.md): adversarial mindset, tool
  surface, and the reproducer-only contract.
- [trust-surface primitives](references/trust_surface_primitives.md): only when
  a small dependency supplies a security-critical primitive.
- [data-structure dependency invariants](references/data_structure_dep_invariants.md):
  only when a niche collection or zero-copy dependency controls fund movement.
- [domain invariant extraction](references/domain-invariant-extraction.md): always
  for spec-less audits and whenever the existing spec is partial or only structural.
- [probe orchestration](references/probe_orchestration.md): only when using
  Mollusk, Miri, or Crucible repro lanes.
- [model selection](references/model-selection.md): before dispatching audit,
  benchmark, or reconciliation workers.

Framework wrappers are evidence, not syntax to ignore. Confirm what the active
framework and version enforce before filing missing signer, owner, rent,
discriminator, initialization, or realloc findings.

### 3a. Ratify intent and schedule fuzzing

Do not ask the user to rediscover facts already visible in code. The intent
interview's questions are the probe envelope's `hypotheses[]` records rendered
conversationally — ask in-harness, in the conversation, one answer per
hypothesis (accept / reject / bug), each presented with its claim, evidence
anchors, and payoff — plus agent-derived candidates (domain invariants,
lifecycle, authority capabilities, threats, intentional exceptions) the binary
abstained on. Time it after the first MED+ finding, or after the autonomous
pass finishes dry or probe-blocked; when there is no finding to win trust with,
the ranked hypotheses themselves become the trust-winning first deliverable.
Write the answers to `answers.json` in the audit dir and run
`qedgen ratify --audit-dir <dir>` to apply confirmed hypotheses as executable
spec clauses. Keep unratified semantic claims as hypotheses. An in-harness
confirmed invariant is `checking` until a backend runs; only impl-bound
evidence supports `#[qed(verified)]`.

Phase numbers refer to the orchestration in
[manual review passes](references/manual-review-passes.md): Phase 1 autonomous
discovery, Phase 2 intent interview, Phase 3 refined second wave.

Crucible has three bounded entry points:

1. Start spec-less protocol mode during Phase 1 as soon as supported runtime
   metadata is available. Treat it as mechanical state-diff coverage only.
2. Start skeleton mode during Phase 1 after auto-ratifying only literal,
   source-anchored high-confidence clauses. Do not wait for a finding.
3. Re-run domain mode after user ratification against the enriched spec, with
   cross-handler sequences and domain properties enabled.

If QEDGen, compilation, runtime metadata, or a fuzz harness is unavailable,
mark that entry point blocked and continue with source review and agent-authored
tests where possible. Never weaken or reject an invariant because Crucible could
not run. See [probe orchestration](references/probe_orchestration.md).

### 4. Classify evidence before severity

Assign exactly one evidence state:

- `confirmed`: an executable reproducer fired against the audited program.
- `structural`: source establishes the vulnerable and reachable path, but the
  execution environment could not run it.
- `hypothesis`: intent, reachability, or a required precondition is unresolved.
- `rejected`: disproved, framework-protected, unreachable, or suppressed.

Then grade impact using [severity and evidence](references/severity-and-evidence.md).
Hypotheses are not vulnerability findings and must appear in a separate
open-questions section, if useful at all.

For every candidate, record:

1. Attacker capability and required preconditions.
2. Exact source path and line.
3. Framework guarantees already in force.
4. Standalone impact.
5. Reachable compositions with other primitives.
6. Evidence state and reproducer status.

### 5. Gate HIGH and CRITICAL findings with reproducers

Write Mollusk reproducers under:

```text
target/qedgen-repros/audit/<finding-id>.rs
```

Run:

```bash
qedgen verify --probe-repros --json
```

Apply these outcomes:

- Fired: evidence is `confirmed`; surface the finding.
- Build or simulator limitation: retain only if source establishes reachability;
  mark `structural` and name the limitation.
- Did not fire: mark `rejected` and omit it from vulnerability totals.
- Ambiguous result: mark `hypothesis`, not `structural`.

MEDIUM and LOW findings may be structural without a repro, but still require a
concrete reachable path. See [reproducer contract](references/reproducer-contract.md).

### 6. Write artifacts and report

Write the full report to `.qed/findings/audit-<timestamp>.md`. Reproducers stay
ephemeral under `target/`. Write suppression rules only for stable,
machine-detectable false positives.

Give the report header a run status: `completed`, `build-blocked` (the program
did not compile), `tooling-blocked` (QEDGen, metadata, simulator, or another
verification lane was unavailable while source review continued), or
`policy-interfered` (a venue or provider policy stopped, truncated, or
redirected the analysis). If an audit worker halts mid-run — refusal,
truncation, or silent stop — the run is `policy-interfered`: report what was
covered before the stop, switch to a worker that completes audit workloads
(see [model selection](references/model-selection.md)), and never present a
partial pass as complete coverage.

Order the digest:

1. Confirmed CRITICAL/HIGH/MEDIUM findings.
2. Structural findings.
3. Specification gaps.
4. Open hypotheses, clearly excluded from vulnerability totals.
5. Suppressed count and emitted artifacts.

Include the dossier JSON/Markdown and run manifest in the artifact footer. A
blocked run must point to the manifest's resume commands.

Every surfaced finding must include:

- severity and evidence state;
- location;
- attacker action and resulting state or fund delta;
- preconditions and reachability;
- standalone impact and any kill-chain;
- minimal implementation fix;
- minimal `.qedspec` regression guard when applicable;
- repro path and result for HIGH/CRITICAL findings.

When handing findings into specification, author three layers in order:
structural contracts, ratified domain properties, then finding-derived
regression guards. An audit is not fully specified merely because every finding
has a guard. See [finding to spec](references/finding_to_spec.md).

Do not edit the audited program unless the user separately asks for fixes. Do
not publish third-party HIGH/CRITICAL findings; recommend responsible disclosure.

## Reliability rules

- Treat the packaged repository copy of this skill as canonical. Maintainers run
  `scripts/check-auditor-skill.sh` when changing it. Installation tooling must
  receive an explicit venue-owned destination; the skill does not assume a
  venue-specific home directory.
- Include the skill package version and, when available, its source commit in
  the audit header so results can be traced to the exact catalog and workflow.
- Never infer spec-aware mode from a literal file named `.qedspec`; resolve an
  explicit spec or a unique `*.qedspec` file.
- Never reject a mixed repository because an unrelated `.s` file exists.
- Assembly-only source-pattern audits are unsupported. Spec-aware analysis is
  allowed when the probe operates from the spec without claiming source-level
  assembly coverage.
- Never claim “clean.” State what was examined, which lanes ran, and remaining
  coverage limits. Recall is probabilistic: a single empty pass is
  under-sampling, not evidence of absence.
