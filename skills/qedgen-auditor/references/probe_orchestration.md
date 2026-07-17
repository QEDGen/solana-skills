# Probe orchestration — operating Producer A

Operational runbook for Producer A (the probe-driven discovery loop).
Read this when you're about to fan out repro emissions, choose
foreground vs background execution, or wire up the
probe→skeleton→Crucible auto-chain.

The companion document `workflow_walkthrough.md` shows the *shape* of
an end-to-end audit. This file is the engine room: which probe tier
runs when, in what mode, with what budget, and what to do when it
hangs.

---

## Time-to-fire ordering

The audit's operational metric is **time-to-first-reproducible MED+**.
Fast-fire first, slow-fire queued behind. Three tiers.

### Tier 1 — Mollusk (≈30s compile per repro, parallel)

In-process SVM driver. Each repro is a Rust integration test that
loads the program, builds an attack instruction, asserts the bug is
observable. Agent writes test files directly to
`target/qedgen-repros/audit/<finding-id>.rs` (no `--fill` verb — per
`feedback_repros_agent_authored`), then `cargo test` fans them.

~30s per test cold, but cargo parallelizes within one invocation. 14
repros warm-cache compile in ~45-60s; first fires land ~T+0:08 from
cold start including the parallel-write block.

In repro semantics, `test … ok` *is* fired (the attack assertion
held). `FAILED` is silent.

```
test qedgen_repros_F15 ... ok      <-- fired (bug observable)
test qedgen_repros_F4  ... FAILED  <-- silent (couldn't reproduce)
```

**Default tier — always run.**

### Tier 2 — Miri (3-5 min per repro, single-threaded)

Rust's MIR interpreter. Catches UB / aliasing / overflow that
survived Mollusk's coarse-grained execution (transmute soundness,
raw-pointer escape, write-through-`&` UB). Dual-execution divergence
(Miri-fail / Mollusk-pass) is itself a `Category::ExecutionDivergence`
critical finding.

```
Bash (run_in_background=true): cargo +nightly miri test \
    --release --test 'qedgen_repros_*' -- --test-threads=1 \
    2>&1 | tee .qed/audit/<ts>/miri.log
```

3-5 min per repro, single-threaded (miri isn't internally parallel).
Run on the *unsafe-touching* subset only — Pinocchio sites tagged
`BorrowUnchecked`, `BytemuckCall`, `RawPtrCastFromAccount`,
`SetLamportsArith` with raw-pointer math.

Fired output: `Undefined Behavior`, `Stacked Borrow violation`, `would
overflow`, plus the `SAFETY claim STALE` marker when a `// SAFETY:`
comment contradicts Miri's analysis. `miri_verify` parses these into
`MiriDiagnostic`s.

**Default tier — unsafe-touching subset only.**

### Tier 3 — Crucible (`--fuzz`, minutes to hours, background)

Coverage-guided fuzzing against the harness from the spec (or
auto-ratified skeleton, per the auto-chain below). Each crash →
`Reproducer::Crucible` finding.

```
Bash (run_in_background=true): qedgen probe \
    --fuzz 300 --spec .qed/audit/<ts>/skeleton.qedspec --no-smoke \
    > .qed/audit/<ts>/crucible.json
```

Budget tuning: 300s first pass; 1800s (30 min) if depth is wanted;
3600s+ overnight if the user explicitly asks.

Fired output: `findings[]` in JSON. Each has `Reproducer::Crucible`
with input bytes + reproducing call sequence.

**Scope the lane honestly.** In spec-less brownfield mode Crucible runs a
per-action protocol-invariant suite that fires only on failure modes
observable as a mechanical state diff: wallet-lamport inflation (any
fuzzer-controlled account, signer or not, gaining lamports), total-lamport
conservation, ownership takeover, discriminator/type change, close-scrub
integrity, rent-exemption loss, realloc data leak, and SPL token-balance
conservation. In-program faults (overflow, `checked_*` DoS, div-by-zero,
`unwrap`, `require!`) surface as tx-errors it cannot see, and semantic/DeFi
bugs need spec context — those always need agent-authored Mollusk
reproducers regardless of Crucible. A dry Crucible run therefore says
nothing about the classes it cannot observe; treat it as a fast win for the
state-diff-observable shapes, not a general crash lane, and never let an
empty fuzz pass downgrade or reject findings in the unobservable classes.

**Default tier — run at the earliest supported entry point and re-run when the
invariant set becomes stronger.**

Crucible has three entry points:

1. **Protocol mode, Phase 1:** start immediately when runtime metadata exposes a
   spec-less harness, including an IDL where the runtime requires one. This
   covers only the mechanical state-diff suite above.
2. **Skeleton mode, Phase 1:** start after literal, source-anchored clauses are
   merged into `skeleton.qedspec`. Do not wait for a finding or user interview.
3. **Domain mode, Phase 3:** re-run after the user ratifies derived/semantic
   invariants. Enable cross-handler sequences and domain counterexamples.

If ordinary probe failed because the QEDGen executable, build, runtime adapter,
or instruction metadata is unavailable, mark Crucible blocked. Continue the
read-driven dossier and preserve the intended command for resumption in
`run-manifest.json`.

---

## Parallel emission pattern

When the probe enumerates N sites, emit N repro prompts in a **single
message** so they fan in parallel. Then run them through one `cargo
test` invocation that cargo concurrency-fans internally.

### Repro emission (parallel Writes, one block)

```
Write: target/qedgen-repros/audit/F1.rs
Write: target/qedgen-repros/audit/F2.rs
Write: target/qedgen-repros/audit/F3.rs
… (one Write per site, all in the same <function_calls> block)
```

For 14 sites the agent's message contains 14 parallel `Write` calls.
Each is a complete Mollusk test (SKILL.md step 5 template). The
`Bash` invocation comes *after* the Writes return.

### Cargo fan (one invocation, all repros)

```
Bash (run_in_background=true): cargo test --release \
    --test 'qedgen_repros_*' \
    -- --test-threads=$(nproc) \
    2>&1 | tee .qed/audit/<ts>/mollusk.log
```

Cargo's internal concurrency saturates the box. Don't fan multiple
`cargo test` calls — they fight for the build lock and serialize
compilation.

**Anti-patterns:** one repro per message (kills the fan, ×N wait); one
`cargo test` per file (serializes compile, cache-busts); awaiting the
fan in foreground (B should be reading code while it compiles).

---

## Background choreography

Long-running phases run under Bash `run_in_background=true`. Producer
B continues foreground; notifications fire on completion.

**Background:** `cargo test` expected to take >10s (cold Mollusk
fans, Miri); `qedgen probe --fuzz` (always); `cargo +nightly miri
test` (always).

**Foreground:** `qedgen probe` without `--fuzz` (~6s pattern-match);
`qedgen probe --emit-spec-candidates` (fast); all `Read` / `Write` /
sub-5s `Bash` (grep, single-file `cargo check`).

### Hang handling

If a background job hasn't notified within 2× its expected budget:

1. **Don't kill blindly.** Foreground inspect:
   `ps -o pid,etime,cmd -p $(pgrep -f "qedgen_repros_")` or
   `tail -30 .qed/audit/<ts>/mollusk.log`.
2. **Cargo lock contention** — another `cargo` holds the build lock.
   Cargo block-waits; correct behavior, not a hang.
3. **Miri OOM** — heap grows on tight loops. Kill the PID, shorten
   the repro's loop bound, re-run.
4. **Crucible budget overrun** — by definition. `--fuzz 300` takes
   300s + setup + shutdown (~315-330s). Wait full budget + 30s.
5. **Genuinely stuck:** kill the PID, surface inconclusive for that
   tier, continue. Don't block the audit on one tier.

---

## Probe → skeleton → Crucible auto-chain

The v2.20 deliverable that closes the brownfield-to-Crucible gap. v2.21
lifted the `--fuzz requires --spec` gate for Anchor / Quasar /
qedgen-codegen brownfield (protocol-mode crash detection runs without a
skeleton). v2.22 lifted the gate for Pinocchio too, provided the program
ships a Codama / Anchor 0.30 IDL on disk (checked at `idl.json`,
`program/idl.json`, `idl/*.json`, `target/idl/*.json` — Codama IR with
`program.instructions[]` and Anchor 0.30 with top-level `instructions[]`
are both recognised). Native + sBPF still error with a v2.23-deferral
message — native will gate on Shank when it lands.

The auto-chain below still applies when richer spec-driven invariants
are wanted (or when the protocol-mode crash surface is too narrow for
the audit's threat model).

### Step 1 — emit spec candidates

```
Bash: qedgen probe --program <root> \
        --emit-spec-candidates \
        --audit-dir .qed/audit/<ts>/
```

Every spec-less probe run also carries, additively in the schema-v3
envelope on stdout: `run_id` (stable per-run identifier for funnel
joins), `hypotheses[]` (evidence-anchored `InvariantHypothesis`
records: claim + evidence + payoff + backend + confidence), and
`spec_readiness` (counts by class/confidence + lowerable). A ranked
human-readable hypothesis summary prints on stderr by default.

Writes nine files to the audit dir:
- `hypotheses.json` — `{schema_version, run_id, generated_at_unix,
  spec_readiness, hypotheses}`: the binary's evidence-anchored
  hypothesis records (authorization, lifecycle_init_once, arithmetic_bound, conservation, cpi_integrity, unwired_guard, state_machine).
- `interview.md` — markdown checkboxes, one section per cluster.
- `clusters.json` — full schema-v3 envelope.
- `skeleton.qedspec` — pre-interview structural skeleton (handler
  stubs, no `requires` / `effect` bodies yet).
- `domain-dossier.json` — canonical schema-v1 dossier seeded with stable
  structural candidates plus conservative source-derived asset-flow, quantity,
  paired-operation, and source-span hints; all inferred semantics remain pending.
- `domain-dossier.md` — human-readable rendering of the dossier.
- `domain-interview.json` — deterministic stable-ID questions and the canonical
  answer array consumed by `qedgen ratify`.
- `domain-interview.md` — readable rendering for file-driven review.
- `run-manifest.json` — initial lane status plus `run_id`,
  `spec_readiness`, and an `artifacts.hypotheses` entry; ordinary probe is
  complete and later verification lanes are resumable.

`qedgen ratify --audit-dir <dir>` consumes `<dir>/answers.json` (or
`--answers <path>`): `{"run_id": …, "answers": [{"id": "<h-… or c-…>",
"decision": "accept|narrow|reject|bug", "note": "…"}]}` — hypothesis and
legacy cluster IDs addressed uniformly. When `answers.json` is present,
`interview.md` is neither consulted nor required; the legacy interview.md
path still works when it is absent. On `accept`, ratify lowers hypotheses
to real executable clauses (auth → `auth <signer>` in the handler body;
lifecycle → a `: State.<pre> -> State.<post>` transition annotation
resolved against the skeleton's State ADT). Each lowering is committed
only if the spec still parses and adds no new Error-severity lints;
otherwise the hypothesis is reported `confirmed, not executable` and kept
in the dossier. The final spec must parse (hard gate); lint counts are
printed. `reject` → `.qed/plan/scoping.md`; `bug` →
`.qed/findings/elicitation-<id>.md`. Ratify writes
`elicitation-outcome.json` (`run_id`, per-hypothesis outcomes,
time-to-ratify, check error/warning counts) into the audit dir.
`ratify --proptest` additionally generates the spec-model proptest
harness at `<dir>/model-proptest.rs` — generation is `checking`-level
evidence only; *running* the harness is what earns `model-tested`, and
never render model-tested as "proved on your program".

`qedgen ratify` additionally writes `spec-handoff.json`, separating emitted
structural clauses, ratified domain facts that still need authoring, regression
guards, and explicit language gaps. Every ratified domain clause carries
construct names, a parser-shaped authoring template, and limitations; every
language gap records what the current language can express so supported floor,
ceiling, finite-sum, nominal-dimension, lifecycle, and authority patterns are not mislabeled as
documentary-only.
Typed external assumptions use `external object.field : Type` inside an
`environment`; keep them distinct from legacy `mutates`, which intentionally
perturbs a program-state field.
It also writes `domain-sequences.json`: paired round trips and lifecycle
setup/teardown coverage targets. Bind every unresolved account and argument
before deterministic replay; until then, use the plans to guide stateful
exploration without claiming exact sequence coverage.
Once bindings are complete, run domain mode with both
`--domain-sequences` and `--domain-sequence-bindings`. QEDGen rejects partial,
unknown, duplicate, and cross-audit bindings, writes the resolved artifact,
replays every plan exactly, then uses those seeds as the exploratory corpus.
Structured seeds encode handler choice and arguments, not fixture account
identity. QEDGen therefore compiles `fixture:<account>` bindings into the
generated harness first and records the deterministic account overlay; only
then may it omit those compiled bindings from the seed bytes. Conflicting or
unknown fixture targets stop replay rather than being silently discarded.

### Step 2 — auto-ratify *high-confidence* entries

In this headless (pre-Phase-2) mode, write `<audit-dir>/answers.json`
accepting only literal, source-anchored high-confidence entries
(hypothesis `h-…` or cluster `c-…` IDs), then run
`qedgen ratify --audit-dir <dir>` to apply them to `skeleton.qedspec`.
Everything else stays deferred in the envelope. **Do not prompt the
user here** — Phase 2 hasn't fired yet.

**High-confidence = the entry has an explicit code anchor.**
Specifically:

- `account_signer_check` — cluster names a specific account binding
  AND handler reads `<binding>.is_signer` (or `assert!(…is_signer)`)
  in source. Signer name is *literal* in code, not inferred.
- `account_owner_check` — cluster names a specific account AND handler
  reads `<binding>.owner == <PROGRAM_ID>` (or Pinocchio equivalent).
  Owner check is literal.
- `arithmetic_no_overflow` on `checked_*` / `saturating_*` — call name
  is literal and the cluster matches it exactly.
- `lifecycle_one_shot` — source contains a literal `is_initialized`
  field read + write, and the cluster's claim is monotonicity.

**Deferred to Phase 2** (low-confidence):

- Conservation invariants — require a *sum* across accounts; informed
  but not literally anchored.
- Authority-graph clauses beyond signer/owner — admin rotation,
  multi-role handoff.
- Threat model — entirely user-intent.
- Anything probe-classified `confidence: low` or `confidence: medium`.

**The check:** if you can point at a specific line of source code and
say "this line is the cluster's anchor," it's high-confidence. If
you're inferring from context, it's not.

### Step 3 — fire Crucible against the auto-ratified skeleton

```
Bash (run_in_background=true): qedgen probe \
        --fuzz 300 \
        --spec .qed/audit/<ts>/skeleton.qedspec \
        > .qed/audit/<ts>/crucible.json
```

Crucible reads the high-confidence invariants and fuzzes for
counterexamples. Each crash streams into Producer A's surface as a
fired finding — same event-driven rule as Mollusk:

> Found `<category>` [SEV]: <one-line>. Repro at <crucible.json#fN>.
> Continuing.

This is the Phase 1 skeleton-mode run. It starts as soon as Step 2 completes;
the first MED+ finding is not a prerequisite.

### Step 4 — risk and mitigation

**The risk:** auto-ratification encodes wrong invariants → Crucible
fires on wrong properties. User sees a "fired" finding that actually
tests an invariant they never agreed to.

**The mitigation:** the literal-anchor bar in step 2. Every
auto-ratified clause has a source-code anchor the agent can cite on
the surface line:

> Found `account_signer_check` [CRITICAL]: `update_fee_rate` accepts
> non-admin signers. **Auto-ratified invariant** (anchored at
> `update_fee_rate.rs:9` — `assert!(admin.is_signer)`). Repro at
> `crucible.json#f3`. Continuing.

If the citation looks wrong on the surface line, the user
correctness-checks the ratification immediately. The literal-anchor
requirement is what keeps the false-positive rate low.

**When in doubt, defer.** A cluster that doesn't meet the bar goes to
Phase 2's interview, where the user ratifies it directly. Phase 3
re-runs Crucible with the user-ratified skeleton.

### Step 5 — enrich and re-run after ratification

Merge user-ratified domain equations, lifecycle edges, authority capabilities,
external assumptions, and intentional bounds into the skeleton. Preserve
rejected candidates with rationale outside the executable spec. Launch a new
bounded background `--fuzz` run and identify it as **domain mode** in artifacts
and the report. Do not reuse a dry skeleton-mode result as evidence that the
stronger domain properties hold.

---

## What NOT to do

- **Don't serialize the producers.** A and B run concurrently. B's
  reads don't wait for A's probe to complete.
- **Don't wait for all probes to complete before reading.** B's
  intent extraction starts as soon as the files load.
- **Don't batch findings into a final report during Phase 1.** Surface
  each fired MED+ the instant it fires.
- **Don't run `cargo test` per repro.** One invocation, parallel fan.
- **Don't run Crucible synchronously.** Always
  `run_in_background=true`.
- **Don't prompt the user during Phase 1.** No `AskUserQuestion`. The
  first fired MED+ is the implicit "keep going."
- **Don't auto-ratify low-confidence clusters.** Literal anchor only;
  defer to Phase 2 when in doubt.
- **Don't read this file end-to-end during an audit.** Read the
  tier-or-step section you're about to invoke.
