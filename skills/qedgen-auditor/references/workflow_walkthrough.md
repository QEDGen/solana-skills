# Workflow walkthrough — concurrent producers, event-driven surfacing

End-to-end runbook for a single brownfield audit, with literal
timestamps and literal tool-call shapes. Read this when you're about to
run the audit workflow described in SKILL.md step 6 — it shows the
*shape* of Phase 1 / Phase 2 / Phase 3, not just the rules.

The scenario uses a small Pinocchio program with no `.qedspec` — the
canonical brownfield case. Anchor and native projects use the same
shape; only the `qedgen probe` flag (`--program` vs `--bootstrap
--root`) and the per-runtime probe output differ.

---

## Scene

User invokes `/qedgen-auditor` on a Pinocchio program at
`programs/fee-vault/` — single crate, 4 handlers (`initialize`,
`deposit`, `withdraw`, `update_fee_rate`), no committed `.qedspec`,
~600 LOC. Assume the thinking-budget hook has fired and the agent is
operating with extended thinking.

---

## Phase 1 — autonomous discovery (T+0:00 to first fired MED+)

No `AskUserQuestion`. No consent walls. Two producers concurrent;
findings surface event-driven as each repro fires.

### T+0:00 — agent enters the skill

Single narration line:

> Running brownfield audit on `programs/fee-vault/`. No `.qedspec`
> present — entering Phase 1 (autonomous discovery). Producer A and B
> launch concurrently.

### T+0:01 — Producer A launches (probe + spec candidates)

```
Bash: qedgen probe --program programs/fee-vault \
        --emit-spec-candidates \
        --audit-dir .qed/audit/2026-05-17-1432/ \
        --json > .qed/audit/2026-05-17-1432/probe.json
```

Probe completes in ~6s. Realistic Pinocchio output: 14 sites across
the 10 site kinds (`BorrowUnchecked`, `BytemuckCall`,
`SetLamportsArith`, `TryIntoUnwrapOnSlice`, etc.), plus 6 candidate
clusters (2 × `account_signer_check`, 2 × `arithmetic_no_overflow`,
1 × `account_owner_check`, 1 × `lifecycle_one_shot`).

### T+0:02 — Producer B launches concurrently

Same message, parallel block: handler files + shared state in one
fan-out:

```
Read: programs/fee-vault/src/lib.rs
Read: programs/fee-vault/src/instructions/initialize.rs
Read: programs/fee-vault/src/instructions/deposit.rs
Read: programs/fee-vault/src/instructions/withdraw.rs
Read: programs/fee-vault/src/instructions/update_fee_rate.rs
Read: programs/fee-vault/src/state.rs
Read: programs/fee-vault/src/errors.rs
```

Seven reads in one `<function_calls>` block — they fan inside the
harness. No prompts to the user.

### T+0:03 — Producer B begins internal intent extraction

Working draft kept *internal* (not surfaced yet — becomes Phase 2's
ratification candidates):

- **Invariants:** `vault.balance == sum(deposits) − sum(withdrawals)`;
  `vault.fee_rate ≤ MAX_FEE_BPS`; `is_initialized` monotone.
- **State-machine:** monotonic lifecycle (`Uninitialized →
  Initialized`); no close.
- **Authority graph:** `admin` (init-configured, rotates fee rate);
  `depositor` (any signer, owns shares).
- **Threats:** compromised user signer; permissionless drain.

Producer B records these with asset flows, units, candidate equations, external
assumptions, source anchors, and confidence in the canonical
`.qed/audit/2026-05-17-1432/domain-dossier.json`, then renders the paired
Markdown. It also updates `run-manifest.json` as lanes start, finish, or block.
This work continues unchanged if Producer A's ordinary probe fails or returns
no sites.

### T+0:05 — Producer A emits 14 Mollusk repros in parallel

One message, 14 Writes:

```
Write: target/qedgen-repros/audit/F1.rs    (account_signer_check / update_fee_rate)
Write: target/qedgen-repros/audit/F2.rs    (arithmetic_no_overflow / deposit)
Write: target/qedgen-repros/audit/F3.rs    (arithmetic_no_overflow / withdraw)
Write: target/qedgen-repros/audit/F4.rs    (BorrowUnchecked / initialize)
… (10 more)
```

Each repro follows the Mollusk pattern from SKILL.md step 5 — load
program, build attack instruction, assert observable bug. Agent
authors them directly per `feedback_repros_agent_authored` (no
`--fill` verb).

### T+0:06 — Producer B queues an extra candidate onto A's executor

While reading `withdraw.rs`, B notices a missing `checked_sub` on the
lamport accounting line the static probe matched but didn't classify
critical. B emits a 15th repro:

```
Write: target/qedgen-repros/audit/F15.rs   (lamport underflow / withdraw — B-discovered)
```

B does *not* wait for F15 to fire. Continues reading
`update_fee_rate.rs` in the same message group.

### T+0:08 — `cargo test` fires all 15 repros, background

```
Bash (run_in_background=true): cargo test --release \
    --test 'qedgen_repros_*' -- --test-threads=8 \
    2>&1 | tee .qed/audit/2026-05-17-1432/mollusk.log
```

Agent does *not* wait. Returns to Producer B's foreground: §3c
trust-surface walk on the SPL token CPI inside `deposit` (cross-
referenced against `references/trust_surface_primitives.md`).

### T+0:10 — first MED+ fires

Notification: `F15` fires —
`assert_eq!(post_state.balance, u64::MAX − 14)`, the underflow wrap is
observable. Agent surfaces *immediately*, one line, keeps working:

> Found `arithmetic_no_overflow` [HIGH]: lamport underflow in
> `withdraw` when `requested > vault.balance` — wraps to ~u64::MAX,
> drains vault on next call. Repro at
> `target/qedgen-repros/audit/F15.rs`. Continuing.

No batching. No "I'll write this up at the end." The surface line *is*
the report entry; it'll be re-aggregated in the final digest.

### T+0:11 — Phase 2 trigger fires

Per `feedback_audit_first_finding_buys_time`, the first MED+ surface
gates Phase 2:

> First fired vuln surfaced. Entering Phase 2 (post-finding
> ratification) to direct the second wave. Phase 1 still running in
> background — additional fires will surface as they land.

Background Producer A's other 14 repros are still cooking; any that
fire during Phase 2 surface in-line between question batches.

---

## Phase 2 — post-first-finding ratification (T+0:11 to T+0:18)

Four `AskUserQuestion` batches present Producer B's internal
hypotheses as ratification candidates, each with `preview` fields
showing the source excerpt the candidate was inferred from. See
`interview_examples.md` for the full transcript shape; one batch shown
below as the rhythm.

### T+0:11 — Batch 1: invariants (multi-select)

```
AskUserQuestion: "Which of these properties must always hold?"
Options:
  [1] vault.balance == sum(deposits) − sum(withdrawals)
      preview: "state.rs:34 — balance: u64"
  [2] vault.fee_rate <= MAX_FEE_BPS (10_000)
      preview: "update_fee_rate.rs:18 — if new_rate > 10_000 { ... }"
  [3] vault.is_initialized is monotone (never resets to false)
      preview: "initialize.rs:22 — vault.is_initialized = true"
  [4] vault.admin set at init, only rotated by current admin
      preview: "initialize.rs:24, update_fee_rate.rs:9"
  [5] Other (free-text)
```

User selects [1]–[4].

### T+0:13 to T+0:18 — Batches 2, 3, 4

- **Batch 2 — state-machine archetype** (single-select): one-shot init
  / monotonic lifecycle / oscillating / free-form. User: monotonic.
- **Batch 3 — authority graph** (multi-select): admin / depositor /
  operator / oracle / other. User: admin + depositor.
- **Batch 4 — threats + gaps**: attacker model (unprivileged /
  compromised depositor / compromised admin / none); acknowledged
  shortcuts (no reentrancy guard, no oracle, no close path).

### T+0:18 — agent writes ratified intent

```
Write: .qed/audit/2026-05-17-1432/ratified_intent.md
```

Records the four invariants, lifecycle archetype, authority graph,
threat model. Phase 3 reads from this file.

---

## Phase 3 — refined second wave (T+0:18 to user stop)

Producer A re-prioritizes against ratified invariants. Producer B
deepens the read-driven walk with the ratified authority graph.

### T+0:19 — Producer A re-prioritization

The `account_signer_check` cluster on `update_fee_rate` emitted F1 in
Phase 1. Now that "admin must sign fee updates" is **ratified**, F1 is
fired-or-silent — no ambiguity:

```
Bash: grep "F1 " .qed/audit/2026-05-17-1432/mollusk.log
```

F1 fired — a non-admin signer successfully called `update_fee_rate`.
Surfaces:

> Found `account_signer_check` [CRITICAL]: `update_fee_rate` accepts
> any signer, not just `vault.admin`. Ratified invariant #4 makes
> this load-bearing. Repro at `target/qedgen-repros/audit/F1.rs`.
> Continuing.

### T+0:22 — Producer B authority × invariant matrix

With the ratified authority graph, B walks each
(authority × invariant) cell — which authority must sign to mutate
the invariant? Finds: `vault.fee_rate` mutation must require `admin`
(ratification says so; F1 just showed it doesn't). Also flags a
**cleanup-incentive** mismatch in `withdraw` — no fee on sub-rent
withdrawals → free-storage abuse path. Emits F16 + cargo test
(background).

### T+0:25 — F16 surfaces

> Found `cleanup_incentive_mismatch` [MEDIUM]: `withdraw` permits
> sub-rent partial withdrawals with no fee — free-storage griefing
> across many accounts. Repro at `target/qedgen-repros/audit/F16.rs`.
> Continuing.

### T+0:31 — N consecutive units without new finding → stop

Background notifications: 13 remaining repros silent. Producer B's
intent-drift sweep on `deposit` / `initialize` yields nothing new.
Agent has gone 6 min without a new surface. Per stop conditions:

```
Write: .qed/findings/audit-2026-05-17-1432.md
```

Final digest aggregates F1, F15, F16 (vulnerability-first order) plus
suppressed-silent appendix and ratified-intent reference.

> Wrapping. Three fired findings: F1 (CRIT, signer bypass on
> `update_fee_rate`), F15 (HIGH, lamport underflow in `withdraw`),
> F16 (MED, free-storage griefing in `withdraw`). Twelve repros
> silent — suppressed per no-advisory-tier rule. Digest at
> `.qed/findings/audit-2026-05-17-1432.md`.

User can extend (`continue`, `look at X handler again`) or accept and
move to spec-writing.

The "move to spec-writing" path is covered by the
**"Handoff to `/qedgen` for spec scaffold"** section in the
auditor SKILL.md and by the family-by-family conversion table at
`references/finding_to_spec.md` (v2.23 Slice 8). Once at least one
MED+ repro has fired, the agent offers the user the audit→specify
handoff with the pitch *"I helped find so many bugs, now let's get
you to specify them so they never come back."*

The handoff authors structural contracts first, ratified domain properties
second, and finding-derived regression guards third. Finding coverage alone is
not treated as protocol-spec completeness.

---

## What this walkthrough shows (pattern-match literally)

- **Producers A and B start in the same minute**, not back-to-back.
- **Repros emitted in parallel** — one message, N Writes.
- **Long-running phases use `run_in_background=true`** so B doesn't
  block on cargo fans.
- **Findings surface the instant a repro fires** — no batched report
  during Phase 1.
- **Phase 2 fires on the first MED+**, not on probe completion. If
  Phase 1 finishes dry, Phase 2 still fires with the "deepening needs
  your input" framing.
- **Phase 3 re-prioritizes by ratified intent**, not by re-running the ordinary
  pattern probe. It does launch a new Crucible domain-mode fuzz run against the
  enriched spec; a dry skeleton run cannot stand in for this stronger pass.
- **Stop is event-driven**: user / budget / N consecutive units
  without a fire. Not a category checklist.

Copy the literal tool-call shapes: parallel-Read block at T+0:02;
parallel-Write block at T+0:05; `run_in_background=true` cargo test
at T+0:08; immediate single-line surface at T+0:10.
