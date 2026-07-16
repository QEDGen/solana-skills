# Report and Grading

Classification rules, the severity grading procedure, output format,
latency budget, responsible disclosure, and the spec handoff. Split from
the audit handbook; `../SKILL.md` and
[severity-and-evidence.md](severity-and-evidence.md) take precedence where
they overlap. Pass references (§3a–§3h) refer to the cross-cutting passes
in [manual-review-passes.md](manual-review-passes.md).

## Classification rules

Each finding lands in one of three buckets, then gets a severity
keyed off attacker capability — not category label.

### Severity grading (attacker-capability rubric)

Severity is keyed off attacker CAPABILITY and the chain's ceiling — not the
category label. The levels fix the scale; the **procedure** fixes the
*consistency* (on cold reads the same finding gets rated a level apart, so
run the steps in order, every finding — this is the single most important
part of grading).

**Levels:** the four levels (CRITICAL/HIGH/MEDIUM/LOW) are defined
canonically in [severity and evidence](severity-and-evidence.md) — this
section does not restate them; it defines how to *apply* them.

**The procedure — apply to every finding, in order:**

1. **Rate the impact ceiling.** State the worst outcome *assuming the
   precondition holds* (a specific policy/config state, a prior privileged
   action, a particular account arrangement), then pick the level from that
   outcome — fund movement / authority escalation / DoS. That is the
   severity.
2. **Record the gate as a qualifier, not a discount.** Note the precondition
   inline ("HIGH, gated on the SpendingLimit policy being active"). Fold how
   *hard* the gate is to reach into priority / ordering — never into the
   severity level. "Hard to reach" is not "low impact"; a gated exploit is
   still a real exploit.
3. **Downgrade only for two reasons:** (a) you cannot articulate any concrete
   attacker capability at all → drop it or INFO; (b) the precondition is
   genuinely unreachable by any user → spec-gap / INFO, not a discounted
   HIGH. Never downgrade because the gate merely "felt unlikely."
4. **Special cases:**
   - **A LOW that composes to CRIT is reported as CRIT** — never let a
     chain's ceiling escape via its weakest primitive.
   - **A dead guard (§3f) inherits the unguarded path's ceiling** — rate it
     by step 1 applied to the path it *fails to protect*, not a
     "just a dead variant" floor. Only a dead variant a *different* guard
     already covers redundantly stays INFO.

Calibration (2026 bench): the failures this procedure corrects — gated
findings mis-rated a level low (the gate discounted *into* severity instead
of noted *alongside* the ceiling), and a dead-guard finding rated at the
dead-variant floor though the path it fails to protect is HIGH. Every miss
was a step-1/step-4 violation.

### Real vulnerability
The impl genuinely has the bug. Action: surface as a finding with
severity, file:line, vulnerable code excerpt, attack scenario, and
proposed fix (code edit + spec edit that would have caught it).
**Don't apply the fix yourself** — the orchestrator and user decide.

### Spec gap
The impl is safe (often because the framework's defaults caught it),
but the spec under-specifies — meaning a future refactor could
reintroduce the vuln without tripping `qedgen check`. Action: surface
as a *spec-gap suggestion*, not a vulnerability. Propose the minimal
spec edit. Lower priority in the digest.

### False positive / suppress
The category genuinely doesn't apply (e.g., `permissionless` handler
that's intentionally signer-less; CPI to `spl-associated-token-account`
which is well-known and verified; saturating-by-design on rent math).
Action: write a suppression rule to `.qed/probe-suppress.toml` so this
finding doesn't re-surface on the next run.

### Don't dismiss inconsistent accounting prematurely
If you find a program-state field whose recorded value disagrees
with the on-chain effect the program just produced (a balance
field that doesn't reflect a transfer it issued, a tracker that
doesn't include a payout it routed, a counter that doesn't tick
on an action it just performed), don't suppress the finding just
because some other guard happens to make the inconsistency
unreachable for an exploit *today*. Two reasons:

1. **Forward-compat risk.** Future refactors that change the
   blocking guard re-arm the bug silently. The current safety is
   load-bearing on a guard the next maintainer may not realize
   exists.
2. **Cross-reader contract.** Off-chain indexers, downstream
   programs, and other handlers in the same crate read these
   fields. They have no way to know the field is "stale by
   design, currently blocked." Inconsistent on-chain accounting
   is itself the finding even when the immediate exploit is
   gated.

Surface as INFO with the framing: *"Field `<F>` records `<X>` but
the program just produced effect `<Y>`. Currently blocked by
guard `<G>`, but should be made internally consistent to prevent
future refactors from re-introducing a divergence."*

Corpus: OS-SPR-SUG-00 (`solana-program/rewards`
`RevokeMerkleClaim`, April 2026) — `NonVested` revocation paid
the claimant's vested-but-unclaimed amount without updating
`MerkleClaim.claimed_amount`. The revocation marker blocked
re-claim, so no immediate exploit, but the documented field was
inconsistent with reality. Fixed in PR #32.

## Output format

### Per-finding (in `.qed/findings/audit-<timestamp>.md`)

```markdown
## [CRIT] <handler> — <category>

**Location:** `programs/<crate>/src/<file>:<line>`
**Mode:** spec-less (no .qedspec at audit time)
**Runtime:** Anchor
**Surfaced by:** `§3d` | `§3f` | `category:unvalidated_remaining_accounts` | `probe:arithmetic_symbol`
**Standalone severity:** HIGH (chain promotes to CRIT)
**Kill-chain:** <category> + <other primitive in this codebase> = <impact>

### Vulnerable code

​```rust
<excerpt with line numbers>
​```

### Attack scenario

<concrete narrative — name the attacker action, the chained primitive,
and the resulting state / fund delta. If stand-alone, say "stand-alone,
no chain identified" explicitly so reviewers know it was checked.>

### Composes with

- <other finding in this audit, or known primitive in the codebase>
  → <amplified impact>
- <other> → <amplified impact>

### Proposed fix (impl)

​```rust
<minimal diff>
​```

### Proposed fix (spec)

​```
<minimal .qedspec edit that would have caught this in spec-aware mode>
​```

### Reproducer (CRIT/HIGH only)

**Status:** fired | inconclusive (`BuildError`/simulator limitation — evidence
state per the [reproducer contract](reproducer-contract.md))
**Test:** `target/qedgen-repros/audit/<finding-id>.rs`
**Run:** `qedgen verify --probe-repros --json | jq '.results[] | select(.finding_id == "<id>")'`

Concrete inputs (from the JSON `assignments`):

- `<var1>` = `<value1>`
- `<var2>` = `<value2>`

Observed: `<the assertion that fired — quoted from test output>`

(If `inconclusive`: state why — e.g., "Mollusk can't simulate
ExternalAccountLamportSpend; finding is structural only.")

### Corpus reference

Category `<category-name>` Corpus line — name the public incident or
recurring audit-firm pattern this finding shares a shape with.
```

**`Surfaced by:` is mandatory** — the single pass (`§3a`–`§3h`), catalog
`category:<name>`, or `probe:<name>` that actually turned up this finding.
It is the standing signal for which parts of this skill earn their keep: the
bench aggregates it into a per-pass / per-category fire rate, so the
long-tail catalog can be pruned on data rather than guesswork. One tag, the
proximate cause — not every lens that *could* have found it.

### Digest (returned to orchestrator)

```
Audit complete: 3 critical, 2 high, 7 medium, 4 spec-gap suggestions
                4 of 5 CRIT/HIGH repros fired (1 inconclusive); 0 silent

[CRIT] withdraw — arbitrary CPI         programs/vault/src/lib.rs:142  [fired]
[CRIT] cancel — missing post-CPI reload programs/vault/src/lib.rs:201  [fired]
[CRIT] init — discriminator collision   programs/vault/src/lib.rs:55   [inconclusive: Mollusk can't simulate cross-program account aliasing]
[HIGH] initialize — non-canonical PDA   programs/vault/src/lib.rs:30   [fired]
[HIGH] redeem — fee computation overflow programs/vault/src/lib.rs:177 [fired]
[MED]  ... (7 more — repros not required)

Spec-gap suggestions (4): impl safe, spec under-specifies — see report.
Suppressed (2 + 0 silent-repro): rules in .qed/probe-suppress.toml

Scaffolded:
  vault.qedspec                              (12 handlers, 5 invariants)
  .qed/audit/20260426-1715/domain-dossier.json (canonical intent candidates)
  .qed/audit/20260426-1715/domain-dossier.md (source-cited intent + lane status)
  .qed/audit/20260426-1715/run-manifest.json (lane status + resume commands)
  .qed/findings/audit-20260426-1715.md       (full report)
  .qed/probe-suppress.toml                   (2 false-positives)
  target/qedgen-repros/audit/<id>.rs         (5 repros — ephemeral)

Next: review vault.qedspec, refine intent, re-run /audit for
spec-aware mode (precise gap detection + ratchet integration).
```

The `n silent-repro` count tracks candidate claims rejected because their
reproducer did not fire. Their details are omitted from the report; only the
aggregate count appears in the digest. This is distinct from schema-v3
`candidates[]`, which remain an internal work list and are never vulnerability
findings. Zero is the expected number for a clean audit;
non-zero is a signal that either the auditor wrote a too-narrow
attack or the structural pattern doesn't actually exploit (in which
case the pattern shouldn't have been flagged at CRIT/HIGH).

## What you do NOT do

- **Don't apply fixes to user source.** Propose; the orchestrator and
  user decide. Editing source crosses the destructive line.
- **Don't run Lean / Kani / proptest.** Those are heavy, opinionated
  artifacts that the user opts into via `qedgen codegen`. Audit is the
  cheap front door. Mollusk repros under
  `target/qedgen-repros/audit/` are a different beast — ephemeral
  test files that exist *only* to gate findings (fired vs silent), not
  long-lived verification artifacts. Generating those is required for
  CRIT/HIGH (v2.16 D5); the prohibition is specifically about Lean
  proofs, Kani harnesses, and full proptest harnesses.
- **Don't ask consent for the audit's named side-effects.** `.qedspec`,
  `.qed/findings/`, `.qed/probe-suppress.toml` are all expected
  artifacts of the named operation. Show them in the digest footer.
- **Don't refuse a native-Rust audit.** Reduced category coverage vs
  Anchor is OK; surface what categories apply, mark the others "not
  applicable to this runtime."
- **Do decline an sBPF/assembly audit.** It's not supported (the
  auditor has never surfaced a real finding on bytecode). Say so
  plainly, don't run a thin audit that implies coverage, and redirect
  to the qedgen proof path (`asm2lean`) or spec-aware mode.
- **Don't dispatch to dylint / anchor-lints / external static analyzers.**
  You're in author position via the user's harness; you have strictly
  more info than dylint's HIR/MIR analysis can recover.
- **Don't surface findings on third-party / dependency code.** Audit
  the user's program source, not the SPL Token program or other
  dependencies; those are trust-boundary axioms.
- **Don't do an audit on a program with active uncommitted changes
  without flagging it.** The audit may produce findings tied to in-
  flight code that won't reflect committed reality. Note this in the
  digest header.

## Latency budget

- Sub-15s for small Anchor programs (1–4 handlers, ~500 LOC). Bias
  toward fewer Read/Grep roundtrips: do one handler-sweep then revisit
  specific lines for confirmation, not back-and-forth.
- 30–60s for native-Rust programs of similar size — multi-file call
  chains (e.g., `try_deposit` → `maybe_invoke_deposit` →
  `spl_token::instruction::transfer`) cost more roundtrips.
- For large programs (multi-thousand-line, many-handler scale), warn
  the user up front that a full audit may take several minutes; offer a
  `programs/` subset cut.

## Responsible disclosure (third-party programs)

If the user runs audit against a third-party / mainnet-deployed
program AND you surface a real critical or high-severity finding, do
**not** publish the finding in any artifact that may leak (no commits
to public repos, no posts to Discord/Slack). Surface in the digest
only. Recommend the user follow the program's responsible-disclosure
channel (`SECURITY.md`, security advisory link, etc.) before any
broader sharing.

## Handoff to `/qedgen` for spec scaffold (v2.23 Slice 8)

Once you've fired at least one MED+ repro (per
`[[feedback_audit_first_finding_buys_time]]`), the next operational
move is to convert the findings into a `.qedspec` so they become
**permanent regression guards**. The audit found the bugs; the spec
ensures they never come back. This is the brownfield onboarding
wedge — the user feels value first (real bugs surfaced from their
existing code), then commits to specification with motivation that
isn't cold.

### When to offer the handoff

The audit "feels complete enough to specify" when **any** of:

- A CRIT or HIGH finding has fired (`repro_status = fired` in the
  digest).
- ≥ 2 MED findings have fired across distinct categories.
- The user signals stop (`/done`, "that's enough", "let's lock this
  in").

Don't gate on the full latency budget — the bear hug requires
incremental value, not a complete sweep
([[feedback_audit_first_finding_buys_time]]).

### The pitch

Carry this framing verbatim:

> "I helped find so many bugs, now let's get you to specify them so
> they never come back. For each finding I've written under
> `.qed/findings/`, I have a `.qedspec` construct that locks the
> finding in as a permanent regression guard. Want me to draft a
> `.qedspec` (or extend yours) and walk you through verification?"

If the user agrees, the next step is to **re-enter the `/qedgen`
skill** for the scaffolding (the cross-skill switch is harness-
handled per `[[feedback_audit_as_subagent]]` — issue a recommendation,
don't programmatic-spawn). The auditor's job ends at "findings
written + handoff offered."

### Operating reference

For the conversion table — probe category → spec construct shape →
why it locks the finding in → what the harness asserts on regression
— see [`finding_to_spec.md`](finding_to_spec.md). Eight families cover the
high-yield categories (authorization, arithmetic, lifecycle / PDA,
data-structure dep invariants, paired validators, intent drift,
external-state revocation, out-of-band documentation invariants).

Pre-conversion checklist (the agent owns each, per
`[[feedback_audit_interview_intent_not_sites]]`):

1. **Read the finding's category and citation.** Both from
   `.qed/findings/<id>.md` (markdown header + cited fields) and from
   `.qed/probes/*.json` (structured fields).
2. **Look up the family in `finding_to_spec.md`.**
3. **Draft the spec snippet** with placeholder slots filled from
   code-derivable facts (handler name, field name, error code symbol).
4. **Ask the user only for intent decisions** when multiple families
   could apply (e.g. `PermissionlessStateWriter` — remove
   `permissionless`, add a bound, or split into two handlers?).
5. **Run `qedgen check`** to validate the snippet — iterate to lint-
   clean before moving on.
6. **Run `qedgen codegen --all` + `qedgen verify`** to confirm the
   harness fires red against the buggy code and green against the
   fix. This is the user-visible payoff.

The conversion is **agent-authored**, not CLI-emitted per
`[[feedback_repros_agent_authored]]`. The data layer
(`.qed/probes/*.json`, `.qed/findings/*.md`) gives you everything you
need; no `qedgen scaffold-spec --from-findings` verb exists or should
in v2.x.

### When the spec already exists

The audit may have run on a brownfield repo that already carries a
partial `.qedspec` (spec-aware mode). Don't draft a parallel spec —
extend the existing one. For each finding:

- If the relevant handler is already in the spec, add the missing
  guard / property / effect inline.
- If the handler is missing, add it (and note "[from audit finding
  <id>]" in the doc-comment).
- Diff the resulting `.qedspec` against the original at the end so
  the user can see what the audit drove.
