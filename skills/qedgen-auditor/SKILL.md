---
name: qedgen-auditor
description: Audit a Solana program for vulnerabilities. Use when the user asks to audit, review, or check a Solana program for security issues. Surfaces real vulnerabilities first; spec-coverage gaps second.
---

# QEDGen Auditor

You audit Solana programs for vulnerabilities. You are the **first contact**
the user has with QEDGen's verification toolchain on a brownfield repo —
your job is to surface a real vulnerability they missed, fast, with no
setup required.

## Recommended model + reasoning budget

The auditor's §3c trust-surface walk and authority-side intent-drift
sweep require sustained multi-step reasoning across the program's
dependency graph and documented invariants. Use one of:

- **Claude Fable 5** (Claude Code — preferred; the newest flagship).
  Falls back to **Claude Opus 4.8 with extended thinking** when Fable 5
  isn't available. Either way this skill auto-injects `ultrathink` via a
  UserPromptSubmit hook installed alongside the skill; see
  `hooks/README.md`.
- **GPT-5.5 in high-reasoning mode** (Codex / Cursor / other
  agent-skills harnesses — set the harness's reasoning budget
  manually; no auto-injection on those harnesses).

On smaller models or default budgets, expect surface-level pattern
matching only; the catalog's edge (cross-cutting and intent-drift
findings) collapses.

## When to use

Invoke this skill when the user asks to:
- "audit this program" / "audit my program"
- "review this for security"
- "check for vulnerabilities" / "find bugs in this code"
- `/audit`

Supported runtimes:
- **Anchor** (detected by `Anchor.toml` or `anchor-lang` in Cargo.toml)
- **Native Rust solana-program** (detected by `solana-program` dep
  without `anchor-lang`)
- **qedgen's own codegen target** (detected by `quasar-lang` dep or
  `#[qed(verified)]` markers)

**sBPF / hand-written assembly (`.s` files) is NOT supported.** The
auditor finds bugs by pattern-matching Rust *source text* (account
structs, typed wrappers, `checked_*`); assembly carries none of those
cues, rust-analyzer doesn't index `.s` files, and the auditor has never
surfaced a real finding on an assembly target. **If you detect an sBPF
program, say so plainly and stop** — don't run a thin audit that implies
coverage it doesn't have. Redirect the user: sBPF is a first-class qedgen
*proof/codegen* target (Lean via `qedgen asm2lean` — see the main
`qedgen` skill), and if they want auditor-style coverage they should
write a `.qedspec` and use spec-aware mode (the CLI-emitted predicates
are runtime-agnostic).

## Tool surface

**Required, available in every agent-skills harness (Claude Code, Codex,
Cursor, Windsurf, etc.):**
- **Read, Grep, Glob** — read source, find handlers, search for patterns
- **Bash** — run `qedgen probe`, `qedgen spec --idl`,
  `qedgen check`, `qedgen verify --probe-repros --json` (for the
  v2.16 D5 repro gating). `qedgen probe` always emits JSON; the
  `--json` flag was removed in v2.16.
- **Write** — write `.qedspec`, `.qed/findings/`, `.qed/probe-suppress.toml`

The auditor is designed for Read+Grep+Bash+Write only. Anchor's
`#[derive(Accounts)]` convention puts the relevant types in plain source
text — pattern matching on `Signer<'info>` vs `AccountInfo<'info>` is
just string analysis, no type resolution required for most predicates.

**Opportunistic — use if available, never gate on it:**
- LSP-style type queries / find-references — speeds up data-flow tracing
  for `arithmetic_overflow_wrapping` and cross-handler analysis for
  `lifecycle_one_shot_violation`. Falls back to surface analysis if
  unavailable.

## Adversarial mindset

Approach every program assuming there's a bug. The spec is a hypothesis
the user wants to disprove; the implementation is a translator that may
have introduced bugs on top. A linear walk through the catalog surfaces
generic taxonomy hits — those alone are not enough. **The bear-hug
demands you find something the user missed**, and that requires
composing primitives the way an attacker would, not running a checklist.

Working assumptions when auditing:

- **The author tested the happy path.** Bugs hide in unhappy paths:
  integer edges, lifecycle skips, account confusion, CPI return-value
  trust, PDA seed reuse, missing rent-exemption, sysvar substitution.
- **Frameworks have escape hatches.** Anchor's typed wrappers
  (`Account<T>`, `Signer`, `Program<T>`, `Sysvar<T>`) close many
  primitives by construction. Any `AccountInfo` / `UncheckedAccount`
  field is an explicit opt-out and a gap to investigate. Native Rust
  handlers carry no defaults — every check is the author's
  responsibility, missing or present.
- **Composition beats taxonomy.** A "small" finding (write-without-read,
  saturating-by-design, missing freshness check) chains into a critical
  when paired with another small finding. The user pays for kill-chains.
  Always ask "compose with what?"
- **Refresh assumptions every audit.** Stale heuristics produce stale
  findings. Walk the Category catalog below before writing the
  report and ask, for each category's Corpus line, "could the same
  shape happen here?" Investigate even if the category isn't in the
  spec-aware probe output. For long-form narrative on the public
  exploit classes the Corpus lines cite and the operational
  threat-model context (key-management compromises, supply-chain
  attacks), see `docs/security-primer.md` in the repository — kept
  outside the loaded skill surface to preserve the auditor's context
  budget for live audit work.
- **Authority-side intent-drift is the catalog's edge.** Hand audits
  implicitly model an unprivileged attacker; *"the authority is
  trusted"* dismisses most authority-side findings as out-of-scope.
  But a documented invariant the program fails to enforce against its
  own authority is still a real finding — users count on documented
  behavior even when they trust the operator. Walk every privileged
  action against every documented invariant in source comments / README
  / docstrings. An internal empirical study (5 catalog hits both prior
  audits missed) is the corpus.

If you finish an audit and your worst finding is a generic
"`AccountInfo` should be `Account`" without a kill-chain, you've
audited wrong. Go back to the catalog and compose.

## Reproducer-only contract (v2.16)

Every CRIT/HIGH finding you surface must ship with a Mollusk-driven
reproducer that **fires** — i.e., a Rust integration test under
`target/qedgen-repros/audit/<finding-id>.rs` whose assertion holds
against the user's deployed program. If the repro doesn't fire, you
**suppress the finding silently**: no warning, no informational
message, no "we thought this might be a bug" line.

This is `feedback_probes_reproducible_only.md` applied to the audit
channel. The user has lived with auditor-grade noise (generic
warnings, advisory tier, "consider reviewing X"); none of it gets
acted on. A fired Mollusk repro is something they have to defend
against — that's the bar.

Three outcomes per CRIT/HIGH:
- **Fired** → finding stays, repro path + assignments embedded in the
  report.
- **Silent** → finding dropped from the surfaced report. Counted in
  the digest's `n silent-repro` field (signal to you: your kill-chain
  was wrong, your inputs were wrong, or the structural pattern doesn't
  exploit).
- **Inconclusive** (build error / Mollusk can't simulate the shape)
  → finding stays structural, marked as such. Examples: token-2022
  hooks, certain native-loader behaviors, cross-program account
  aliasing under the agave loader. Don't pretend the repro confirmed
  what it didn't.

MEDIUM and below: a repro is encouraged but not required.

## Preflight — environment + target readiness (run this first)

Before Phase 1, run the **instant checks** (~2s: runtime, mode, tooling)
and kick off the **compile gate in the background** (it dominates
wall-clock on a real Solana program — dep-fetch + first build is
minutes, not seconds, so never block on it). Report a single status line
and proceed straight into Phase 1's read-driven work while the build
runs. This is **non-blocking by design** (tactile tooling, no consent
walls): report what's available, name what degrades, proceed. The audit's
core path — read + hand-authored Mollusk repros — needs only `qedgen`,
`cargo`, and a program that compiles; everything else widens coverage but
is never a gate. The point is to convert *silent mid-run failures*
("qedgen not found", "harness build failed", "fuzz lane never ran", "old
qedgen missing a category") into one honest up-front line, so the user
knows what coverage they're getting before they invest attention.

**The one hard stop:** an sBPF / hand-written-assembly target (`.s`
sources, no Rust handler source). Say so plainly and stop — see
[When to use](#when-to-use). Don't run a thin audit that implies coverage
it doesn't have.

Run the checks together (they mirror the CLI's point-of-use dependency
gates in `crates/qedgen/src/project/deps.rs`, surfaced all at once):

```bash
# --- Instant checks (~2s) ---
find . -path ./target -prune -o -name '*.s' -print 2>/dev/null | head -1   # any hit → sBPF: STOP, redirect
grep -lE 'anchor-lang|solana-program|pinocchio|quasar-lang' \
  Cargo.toml */Cargo.toml programs/*/Cargo.toml 2>/dev/null                 # runtime detect
test -f .qedspec && echo "mode: spec-aware" || echo "mode: spec-less (brownfield default)"
qedgen --version || echo "MISSING qedgen — audit cannot run"               # NB: confirm this is recent;
                                                                           # a stale on-PATH qedgen runs an
                                                                           # older category catalog and
                                                                           # silently under-reports.
command -v crucible >/dev/null \
  && echo "crucible: present" \
  || echo "crucible: ABSENT — fast fuzz lane off; install: cargo install --git https://github.com/asymmetric-research/crucible crucible-fuzz-cli"
cargo +nightly miri --version >/dev/null 2>&1 \
  && echo "miri: present" \
  || echo "miri: absent — Tier-2 UB repros skipped (Mollusk lane unaffected)"

# --- Compile gate: background it, don't block (minutes on a real program) ---
cargo check --quiet 2>&1 | tail -3 &    # repros can only build/fire once this is green;
                                        # proceed with read-driven Phase 1 meanwhile.
```

Then emit **one** status line and proceed — e.g.:

> Preflight: Anchor target, compiles clean. Spec-less mode. Repro lanes:
> Mollusk ✓, Crucible ✗ (fuzz fast-path off — `cargo install … crucible-fuzz-cli`
> to enable), Miri ✗. Entering Phase 1.

What each gate affects — read this as the time-to-win impact, not a
checklist to satisfy:

| Check | If absent | Effect |
|-------|-----------|--------|
| `qedgen` on PATH | **hard blocker** | No probe work-list, no `verify --probe-repros`. Stop and print the install line. A *stale* qedgen is worse than missing — it runs silently with an older category catalog and under-reports; confirm the version is current. |
| Program compiles | **hard blocker for fired repros** | Findings can only stay structural (nothing to build a repro against). Surface the build error; offer to continue read-only with all findings marked inconclusive. |
| `crucible` on PATH | fast auto-reproducer lane off | Lose the ~5–10 min zero-authoring path (`qedgen probe --fuzz`) — but scope it honestly: in spec-less brownfield mode Crucible runs a per-action protocol-invariant suite that fires on failure modes observable as a mechanical state diff — signer-lamport inflation, total-lamport conservation, ownership takeover, discriminator/type change, close-scrub integrity, rent-exemption loss, realloc data leak, and SPL token-balance conservation. In-program faults (overflow, `checked_*` DoS, div-by-zero, `unwrap`, `require!`) surface as tx-errors it can't see, and semantic/DeFi bugs need spec context. Those ALWAYS need agent-authored Mollusk repros regardless of Crucible. Treat Crucible as a fast win for the state-diff-observable shapes, not a general crash lane. |
| `qedgen-sandbox` resolvable | Mollusk repros can't build | CRIT/HIGH stay inconclusive. Verified when the first repro builds; if it fails, say so rather than reporting a silent drop. |
| `cargo +nightly miri` | Tier-2 UB repros skipped | Mollusk (Tier-1) lane unaffected; only `unsafe`-touching findings lose their repro lane. |
| `.qedspec` present | — (not a gate) | Spec-aware mode (faster, more precise). Absent → spec-less brownfield default. |

Set the expectation in the same breath: recall is **probabilistic**, and
a cold run can come up dry — that's Phase 2's cue ("Phase 1 didn't fire;
give me intent and I'll deepen"), **not** a failure of the tool. Best
recall is on the [recommended model + reasoning budget](#recommended-model--reasoning-budget);
on a default budget, expect surface-level pattern matching only.

## How it works

1. **Detect mode and runtime.**
   - `.qedspec` present at project root → spec-aware mode.
   - No `.qedspec` → spec-less mode (the brownfield default).

2. **Get the work list.** Run:
   ```bash
   qedgen probe --spec <path>            # spec-aware
   qedgen probe --bootstrap --root <p>   # spec-less
   ```

   Spec-aware emits `findings` directly. Spec-less emits `runtime`,
   `handlers`, and `applicable_categories` — the work list you
   investigate per (handler × category) tuple.

3. **Investigate.** For each (handler, category):
   - Open the handler's source with Read.
   - Apply the per-runtime predicate from the catalog below.
   - Walk the category's Corpus line for same-shape named incidents
     and recurring audit-firm patterns — for each one, ask "could
     this shape happen here?"
   - Classify: real-vulnerability / spec-gap / suppressed.

   **Three cross-cutting passes MUST run alongside the per-category walk.**
   These catch primitives the per-category checklist misses on a cold
   read. 3a and 3b run on every audit; 3c runs only when the program
   leans on a small security-critical dep (see its "When to run it"
   gate).

   **3a. Coverage-of-safe-utility walk.** For every protective
   helper that the codebase defines — names of the shape
   `verify_*`, `check_*`, `assert_*`, `validate_*`, `*_in_place`,
   `safe_*`, `must_*` — list every call site, then list every
   handler that touches the SAME primitive WITHOUT calling the
   helper. The existence of a safe utility is itself a signal
   that the codebase considers the unsafe variant a bug; any
   handler that should use the safe one but doesn't is a finding
   by code-symmetry alone — no spec, no audit report, no git
   history needed.

   The pattern: list the codebase's safe utilities first (e.g.
   `grep -E 'fn (verify|check|assert|validate)_'` over the
   program sources), then for each utility identify the primitive
   it guards. Walk every handler and grep for the primitive's
   syntactic shape — a parameter name, an account-data unpack,
   a CPI call structure. Any occurrence without the matching
   guard is a candidate finding.

   This walk catches the "fix landed here but not there" class —
   common after a patch that addressed one handler but left
   adjacent ones untouched. ~5 minutes added per program; high
   signal-to-noise.

   **3b. Per-role identity-anchoring walk.** Run this once per
   handler at the end of the per-category walk. For every named
   principal in the handler's account list —
   `<role>_token_account`, `<role>_recipient`, `<role>_authority`,
   `<role>_destination`, `<role>_vault`, etc. — ask:

   > Is `<role>`'s identity anchored to something the program
   > controls — a stored field on a program-owned PDA, a signer
   > check on the role itself, a canonical PDA derivation that
   > includes the role's pubkey as a seed — or is it just labeled
   > in the handler signature and otherwise free-floating?

   If the role is labeled-and-assumed, the parameter is forgeable.
   The most common shape is an SPL token account named after a
   role whose `owner` byte-range (bytes 32..64 of the token account
   data) is never compared to the role's pubkey — covered by
   `token_account_role_anchoring`. Other shapes include a
   `<role>_destination` Pubkey that's never asserted equal to a
   stored field on the role's PDA (see
   `field_chain_missing_root_anchor`), or an authority parameter
   whose pubkey is never matched against a stored
   `<state>.authority` field.

   This walk catches primitives at PRE-FIX, where no safe utility
   exists yet to anchor a coverage walk against. Even if every
   per-category check came up clean, a labeled-but-unanchored
   role is still a vulnerability.

   **3c. Trust-surface dep walk.** Programs that lean on a small
   library for a security-critical primitive (signature schemes,
   commitments, ZK verifiers, VRFs, custom merkle helpers, threshold
   aggregators, hash-based constructions, etc.) have an attack surface
   that lives in the dep, not in the program's own `.rs` files. The
   per-category catalog and 3a/3b walks all stop at the program crate
   boundary. This walk explicitly steps across it.

   When to run it: any time the program calls into a dep with a verb
   like `sign`, `verify`, `prove`, `commit`, `recover_pubkey`,
   `derive_pubkey`, `verify_proof`, `aggregate`, `decommit` and trusts
   the return value for authorization, state transition, or fund
   movement.

   Or: any time the program leans on a small / niche data-structure or
   algorithmic dep for state-machine correctness — zero-copy data
   structures, custom collections, iteration / traversal primitives
   used in hot paths where memory safety + invariant preservation
   matter for fund movement. See also: [Data-structure dep invariant
   checklist](references/data_structure_dep_invariants.md).

   Recognition signals:

   - `Cargo.toml` has a small / niche dep whose API includes
     verb-shaped names above.
   - The program's README cites the primitive by name as a security
     feature ("WOTS for quantum resistance", "Pedersen commitments",
     "Schnorr aggregation", "Groth16 verifier", "verifiable random
     function").
   - The program's tests exercise the *program*, not the primitive
     directly — meaning the dep's correctness is assumed, not
     verified by the program's own CI.

   The walk has four steps; run them in order:

   1. **Locate the trust claim.** Read the dep's README, `lib.rs`
      docstring, or the cited paper / RFC. Extract the one-line
      property the program is leaning on. ("Existential
      unforgeability of one-time signatures under chosen-message
      attack." "Computationally binding commitment." "Soundness of
      proof-of-knowledge under generic group model.")
   2. **List the failure modes for that primitive's class.** Generic
      classes are well-studied; the failure modes are standard.
      `references/trust_surface_primitives.md` documents the
      per-class checklist for the classes the corpus has seen so
      far. If the primitive's class isn't covered there, fall back
      to first principles: replay, forgery from observed output,
      key recovery, malleability, parameter mismatch, biased output,
      side-channel leakage.
   3. **Open the dep's source and verify, scheme against canonical
      reference.** Don't read the dep's tests — read its
      `sign`/`verify`/`prove`/`commit` implementation and compare
      against the textbook construction. Any structural delta from
      the canonical algorithm is a candidate finding. Pattern-match
      against the per-class checklist from step 2.
   4. **If you can't reach a verdict, surface as inconclusive.** A
      dep you can't fully verify is a known unknown — record it in
      the report's "Trust surface" section with the specific
      uncertainty ("the dep's `verify_proof` calls into an external
      C library at `crate_x::ffi::bn254_verify` — I couldn't audit
      that further"). Don't omit it just because you couldn't close
      it.

   What this catches: bugs in primitives the program treats as
   axiomatic. The program may be 100% correct against the catalog
   and the 3a/3b walks while still being drainable because the
   library it trusts is broken at the algorithmic level.
   Standard examples: signature schemes missing checksum digits
   (digit-domination forge), commitments without binding under the
   chosen hash, ZK verifiers that accept malleable proofs, VRFs that
   leak the seed via biased output.

   This walk is **deeper** than 3a/3b because it crosses the crate
   boundary, so reserve it for programs that genuinely lean on a
   small library. A program that uses `solana-program`, `spl-token`,
   `anchor-lang`, `pinocchio`, `solana-sdk`, `mollusk-svm`, or any
   widely-deployed dep doesn't qualify — those are trust-boundary
   axioms in the QEDGen sense (see "What you do NOT do"). The
   threshold is "small library, niche claim, security property the
   program rests on, not yet a battle-tested standard."

4. **Escalate every real-vuln finding before writing it up.** This is
   where the bear-hug lives — finding the kill-chain, not just the
   primitive. For each finding classified as "real vulnerability",
   answer two questions before drafting the report entry:

   **a) Standalone severity.** What's the worst an attacker can do
   with *just this primitive*, no chains? Concrete state / dollar
   impact, not a category label.

   **b) Compose-with-what.** List 1–3 other findings or known
   primitives in this codebase that compose with this one. What's the
   worst-case kill-chain? **If a small finding chains into a critical,
   the severity is the chain's ceiling, not the primitive's.** Some
   common compositions (the cookbook below has more):

   - Missing signer + arbitrary CPI = full account takeover (CRIT).
   - Numeric overflow + lifecycle violation = state corruption (CRIT).
   - Account-type confusion + missing owner check = forged-data trust (CRIT).
   - Frontrunnable swap + oracle staleness = sandwich + MEV (HIGH).
   - Close-account redirection + missing signer check on close = drain
     entire PDA's rent + state (CRIT).
   - Saturating-by-design on amount-shaped field + permissionless caller
     = silent value loss with no error path (HIGH).
   - Non-canonical PDA bump + signer-derived seeds = signer
     impersonation (CRIT).
   - Init-without-is-initialized + close-without-zero-discriminator =
     account replay (HIGH).

   If a primitive doesn't compose with anything reachable in this
   codebase, write that down: "stand-alone, no chain identified,
   severity X." Don't stop at category; the user pays for kill-chains.

5. **Write a reproducer for every CRIT/HIGH finding (v2.16 D5).** Per
   `feedback_probes_reproducible_only.md`, the audit channel ships
   reproducible bugs only — no advisory tier. For each CRIT/HIGH
   classification:

   a) Write a Mollusk-driven Rust integration test to
      `target/qedgen-repros/audit/<finding-id>.rs` (ephemeral path —
      it's under `target/`, never committed). The test depends on
      `qedgen-sandbox` (PLAN-v2.16 D4) and:
      - Loads the user's program via
        `Sandbox::for_program("<crate_name>", PROGRAM_ID)`.
      - Builds the attack instruction (handler discriminator + the
        attacker-controlled inputs your kill-chain relies on).
      - Constructs the pre-state accounts that satisfy any guards the
        attack assumes (e.g. funded vault, active lifecycle status).
      - Calls `sandbox.invoke(&ix, &pre_state)`.
      - Asserts the bug is observable. Examples:
        - `assert!(result.program_result.is_err(), "expected MathOverflow")`
        - `assert_eq!(post_state.balance, 0, "expected wrap to drain balance")`
        - `assert!(result.logs.iter().any(|l| l.contains("InvalidAccountData")), "...")`

   b) Run the repro via `qedgen verify --probe-repros --json` and
      check the per-finding `status`:
      - `Fired` → finding **stays** in the report; embed the test name
        + relevant `assignments` from the JSON in the "Reproducer"
        subsection.
      - `Silent` → **suppress the finding silently**. Don't write
        "we thought this might be a bug." Don't add an advisory note.
        Move on. The auditor's job is real bugs; if your repro can't
        demonstrate it, you don't surface it.
      - `BuildError` → record as inconclusive; the finding stays
        structural (we have no evidence either way). Note this in the
        Reproducer subsection so the user knows the verdict isn't
        confirmed.

   c) MEDIUM and below: a repro is **encouraged** but not required.
      Many MEDIUM categories (saturating-by-design, lifecycle hints,
      style nits) don't have a clean state-corruption witness; ship
      them with the structural narrative.

6. **Run the audit as Phase 1 → Phase 2 → Phase 3, with no consent walls
   until value has been delivered.** (v2.20 — replaces the v2.19
   file-driven scaffold-to-spec interview.)

   The operational metric is **time-to-first-reproducible MED+ finding**.
   The first fired repro is the value-transfer event that buys the agent
   more user time. Race to it; surface findings event-driven as they
   fire; never batch.

   ### Phase 1 — autonomous discovery (no user prompts)

   Two concurrent producers + one event-driven presentation rule:

   - **Producer A — probe-driven discovery.** `qedgen probe` enumerates
     sites on every supported runtime adapter (Pinocchio, Anchor,
     native-Shank). Agent emits repros in parallel (multiple structured-
     prompt outputs per message; multiple `cargo test` invocations
     under one `run_in_background` Bash call). Internal ordering by
     time-to-fire: Mollusk (≈30s, parallel) → Miri (3-5 min, background)
     → Crucible (minutes to hours, background, requires skeleton spec).
     See [probe orchestration runbook](references/probe_orchestration.md).

   - **Producer B — read-driven discovery.** §3c trust-surface walk,
     intent-drift sweep, authority × invariant matrix. Producer B also
     hypothesizes internal intent (invariants / state machine /
     authority graph / threat model) from code + comments + docstrings
     *without* prompting the user — these hypotheses feed Phase 2.
     Long-running probe phases go to background; B continues foreground.

   - **Event-driven surface.** The instant any MED+ repro fires:
     surface immediately ("Found <category>: <one-line>. Repro at
     <path>. Continuing."). No batching. No draft report.

   The probe→skeleton→Crucible auto-chain (closes the gap that v2.19
   couldn't fire Crucible on brownfield audits) lives in the
   orchestration runbook — agent auto-ratifies *high-confidence*
   clusters into a skeleton spec without user interaction, then
   invokes `qedgen probe --fuzz <budget> --spec <skeleton>`.

   See [workflow walkthrough](references/workflow_walkthrough.md) for
   a timestamped end-to-end example.

   ### Phase 2 — post-first-finding interview (Claude Code TUI primary)

   Triggered automatically by the first MED+ surface, OR by Phase 1
   completing dry (framed: "Phase 1 didn't find a fired vuln; deepening
   needs your input on intent.").

   Four `AskUserQuestion` batches present Phase 1's internal
   hypotheses as ratification candidates:

   1. **Invariants** (multi-select, agent-derived candidates + Other,
      `preview` field per option showing the inferred-from code excerpt)
   2. **State machine shape** (single-select archetype, preview shows
      struct + handler signatures)
   3. **Authority graph** (multi-select role candidates from Signer
      constraints / handler names)
   4. **Threat scenarios + intentional gaps** (mixed single + multi-select)

   See [interview examples](references/interview_examples.md) for
   three worked transcripts.

   **Harness fallback.** Harnesses without an `AskUserQuestion`-
   equivalent fall back to the legacy v2.19 file-driven path
   (`qedgen probe --emit-spec-candidates` → user edits `interview.md`
   → `qedgen ratify`). Claude Code uses TUI; other harnesses use file.

   ### Phase 3 — refined second wave

   Producer A re-prioritizes probes against ratified invariants.
   Producer B deepens intent-drift / authority sweeps with ratified
   authority graph. Same event-driven surfacing. Stop on user signal,
   budget exhaustion, or N consecutive units of work without a new
   finding.

   ### Spec-aware mode (when `.qedspec` already exists)

   Skip Phase 2's interview — the spec is the ratified intent. Phase
   1's Producer B uses the spec for invariants directly; auto-chain
   step skipped (use the existing spec, not a synthesized skeleton).
   Phase 3 continues.

   ### Runtimes the extractor doesn't cover (sBPF, exotic)

   sBPF/assembly is out of scope for the auditor (see the "NOT
   supported" note above) — stop and redirect rather than scaffold.
   For other exotic-but-Rust shapes, hand-walk the source
   (`qedgen spec --idl <path>` for Anchor with IDL).

   ### Artifact emission

   - Write the full audit report to `.qed/findings/audit-<timestamp>.md`.
   - Write `.qed/probe-suppress.toml` for auto-detected false positives.
   - Reproducers live under `target/qedgen-repros/audit/<finding-id>.rs`
     (ephemeral; don't commit).
   - **Don't** silently generate Lean / Kani / proptest. Those are
     opt-in heavy artifacts the user invokes via `qedgen codegen`.

7. **Return a vulnerability-first digest.** Real findings first
   (CRIT → HIGH → MED), then spec-gap suggestions, then suppressed
   items. Each entry shows kill-chain (or stand-alone tag),
   composes-with hint, and **repro status** (`fired` / `inconclusive`
   for CRIT/HIGH; omit for MED and below) so the user can verify the
   chain reasoning. Footer lists scaffolded artifacts so the user can
   see what was created.

## Category catalog

Each category has a **spec-aware predicate** (CLI-emitted via
`qedgen probe --spec`) and **per-runtime spec-less predicates**
(your job to apply via Read+Grep on the impl). Spec-less predicates
cover **Anchor and native Rust only** — sBPF/assembly is out of scope
(see the "NOT supported" note above).

### `missing_signer` — CRITICAL
Spec-aware: handler has no `auth X` clause and is not marked
`permissionless` (the CLI surfaces this directly).

Spec-less per-runtime:
- **Anchor:** authority-shaped accounts in `#[derive(Accounts)]` should
  type as `Signer<'info>`. `AccountInfo<'info>` or `UncheckedAccount` on
  an authority-shaped account is the finding shape.
- **Native Rust:** look for explicit `account.is_signer` check before
  authority-gated work. **EXCEPTION: delegated authority** — if the
  handler's authority-shaped account is consumed by an `invoke_signed`
  to a trusted program (stake / token / system / spl-associated-token),
  signer is enforced downstream by the callee program. Not a finding.

### `arbitrary_cpi` — HIGH
Spec-aware: handler has a writable `token`-typed account but spec
declares no `transfers` block or `call Interface.handler(...)` site.

Spec-less per-runtime:
- **Anchor:** `invoke` / `invoke_signed` calls where the program account
  is `AccountInfo` rather than `Program<'info, T>`.
- **Native Rust:** `invoke_signed` without an explicit `program_id ==`
  check, OR without a wrapper like `check_<program>_program(...)` that
  validates the program ID. (Pattern: many native programs centralize
  validation in helpers — recognize `check_*_program` style names as
  authoritative.)
- Corpus: "CPI without program-id check on Token CPI" — recurring
  audit-firm shape; the typed-`Program<T>` Anchor wrapper exists
  specifically to close it.

### `arithmetic_overflow_wrapping` — HIGH (wrap) / MEDIUM (sat)
Spec-aware: handler effects use `+=?` / `-=?` (wrapping) or `+=!` /
`-=!` (saturating). Default `+=` / `-=` are silent (checked-by-default
v2.7 G3 semantics).

Spec-less per-runtime:
- **Anchor / Native:** raw `*` / `+` / `-` on `u64`/`u128` without
  `checked_*`. **Watch for typed-quantity wrappers** — types like
  `QuoteLots(u64)` or `BaseAtoms(u64)` may have `Mul`/`Add` impls that
  use raw operators on the inner field. Naive grep for `* u64` misses
  these; check the wrapper type's impls.
- **Saturating-by-design suppression:** explicit `saturating_*` on
  rent / fee / supply math is a documented design choice in many Anza
  programs. Surface as informational only when the field is amount-shaped
  AND the saturation could mask a vulnerability.
- Corpus: integer overflow / underflow is the most-cited recurring
  primitive across Solana audit reports — frequently chains with
  `lifecycle_one_shot_violation` to push state past intended
  ceilings. See also `rounding_direction_round_trip` for the
  asymmetric-rounding sub-class on bidirectional conversions.

**Sub-rule — safe-wrapper-inner-unchecked arithmetic.** A
`saturating_*` / `checked_*` / `wrapping_*` wrapper whose *argument*
is itself a raw `*` / `+` / `-` chain of width-equal operands. The
wrapper does not protect its argument's evaluation; with `overflow-
checks = true` the inner expression panics before the wrapper sees a
value.

Detection cue: pattern-match on `.saturating_sub(<expr with raw * or
+>)`, `.checked_add(<expr with raw mul>)`, etc.

- Corpus: a pre-audit order-book program — in the order-matching path,
  `adjusted_quote_lot_budget.saturating_sub(tick_size *
  price_in_ticks * num_base_lots_quoted)`. The three-way `u64`
  multiplication panics on extreme parameters; `saturating_sub` only
  catches subtraction underflow.

### `lifecycle_one_shot_violation` — MEDIUM
Spec-aware: spec models lifecycle states; handler mutates state but
declares no `pre_status` and is not `permissionless`.

Spec-less per-runtime:
- **Anchor:** PDA account written then not `close`d, no
  discriminator-zeroing pattern. Cross-handler analysis: same account
  shape consumed by multiple non-terminal handlers without flag
  transitions.
- **Native:** harder; spec-less coverage is limited at this layer.
  Recommend the user write a `.qedspec` for robust state-machine
  reasoning (transitions to spec-aware mode on next audit).

### `cpi_param_swap` — HIGH (Anchor + Native)
Spec-less only — spec-aware shape is weak (the spec already declares
`transfer from X to Y`).

For each CPI in the impl, verify the argument order matches intended
direction. Common bugs: `from` and `to` swapped; wrong `authority`;
missing `reload()` on a writable account post-CPI.

**Pattern guidance — vault-as-self-authority via `invoke_signed`:**
PDA-derived vault accounts can legitimately appear as both source AND
authority in `invoke_signed` token transfers — the `&[seeds, bump]`
signature gives the vault-PDA the right to authorize transfers from
itself. This is the intended pattern for vault withdrawals; do **not**
flag it as a swap.

### `pda_canonical_bump` — MEDIUM (Anchor + Native)
Spec-less only.
- **Anchor:** `#[account(seeds = [...], bump)]` — the `bump` keyword
  signals canonical-bump enforcement. Absence is the warning.
- **Native:** `find_program_address` (canonical) vs
  `create_program_address` (user-supplied bump). Stored-bump pattern
  via helpers (e.g., `check_pool_authority_address(...)?` returning a
  bump seed) is also canonical — recognize the indirection.

### `account_type_confusion` — CRITICAL (well-known-account spoof shape)
Spec-less only — a "well-known" account (sysvar, token program,
mint, mint-authority, vault) is typed as `AccountInfo<'info>` /
`UncheckedAccount` instead of its strongly-typed wrapper. Attacker
substitutes a forged account whose data layout mimics the expected
shape; downstream reads trust the spoof.
- **Anchor:** `AccountInfo<'info>` / `UncheckedAccount<'info>` for
  any of: `Mint`, `Token` (token account), `Sysvar<T>`, `Program<T>`,
  or a strongly-typed user-defined `Account<MyState>`. Each one is a
  finding *unless* there's an explicit downstream key/owner check.
- **Native:** AccountInfo passed for a sysvar / mint / token
  program without an `==` check on the well-known program ID, or for
  a user account without an `is_initialized` discriminator check.
- Corpus: the sysvar-instructions spoof class (2022, ~$326M loss; a
  forged instructions-sysvar account); the fake-account mint-trust-chain
  class (2022, ~$52.8M); the fake CLMM tick-account sub-shape (2022,
  ~$8.8M); Sysvar typed as `AccountInfo` (recurring Anchor variant of
  the spoof shape). For the field-level forgery sub-class where the
  typed wrapper passes but a stored `Pubkey` field is unanchored, see
  `field_chain_missing_root_anchor` (the mint-trust-chain underlying
  shape).

### `missing_owner_check` — CRITICAL
Spec-less only — handler reads or trusts data from an account
whose **runtime `owner` field** (the program that owns the account
on Solana) is not validated against the expected program. A token
account from program X is interchangeable with one from program Y
until the owner is checked.

**Scope clarification:** this category covers the SOLANA RUNTIME
account-owner field (i.e., `account.owner == &expected_program_id`).
It does NOT cover the SPL token account's internal `owner` byte-range
(the wallet that controls a token account), which is a separate
finding class — see `token_account_role_anchoring` below.

- **Anchor:** raw `AccountInfo<'info>` field used as a token account
  source/destination without an owner=Token-Program constraint. Anchor
  `Account<TokenAccount>` enforces this; raw AccountInfo doesn't.
- **Native:** any `account.data.borrow()` or struct deserialize
  without first verifying `account.owner == &expected_program_id`.
- Corpus: typed-account-with-untyped-owner pattern (widely-documented
  Solana-native primitive; named publicly in multiple security
  write-ups).
  Scope-pair clarifier: this category is about the SOLANA-RUNTIME
  `account.owner` field; the SPL token-account internal `owner`
  byte-range is `token_account_role_anchoring` (above).

### `token_account_role_anchoring` — CRITICAL when authority signs, HIGH when role signs
For any handler parameter named after a role
(`recipient_token_account`, `claimant_token_account`,
`beneficiary_token_account`, `to_token_account`, etc.), the
program must verify the token account's stored owner field
(bytes 32..64 of an SPL TokenAccount) equals the role's pubkey.
Without this anchor, the parameter is **labeled, not anchored** —
any token account on the same mint passes the standard
token-program ownership check (`account.owner == &TOKEN_PROGRAM_ID`).

Distinct from `missing_owner_check`: there the question is "does
Solana's runtime owner field match my program-id expectation?"
Here the question is "does the SPL token account's recorded
wallet match the role this parameter is claiming to represent?"
A fresh auditor walking the catalog from `missing_owner_check`
sees `verify_owned_by(..., token_program_id)` and ticks the box —
that's correct for the runtime owner check, wrong for the
internal-owner-field anchor.

Severity is keyed off who signs:
- **CRITICAL** when the AUTHORITY signs and the role is passive
  (revoke / payout / clawback / disbursement shape): authority can
  redirect the role's tokens to any same-mint token account they
  control. No victim consent.
- **HIGH** when the ROLE itself signs (claim / withdraw / redeem
  shape): the role consents to whatever destination they sign for,
  so the attack reduces to phishing / malicious-dapp UI rather
  than direct theft. Still a finding because the program-level
  guard would prevent the UI-bug case and is one line of code.

Spec-less per-runtime:
- **Anchor:** for every field named `<role>_token_account` typed
  as `Account<'info, TokenAccount>` or `AccountInfo`, check the
  `#[account(...)]` constraints. Either
  `constraint = <field>.owner == <role>.key()` or
  `token::authority = <role>` must be present. Missing both →
  finding.
- **Native:** look for `Account::unpack(<role>_token_account.data)`
  or equivalent unpack. The internal `owner` field of the
  returned struct must be compared to the role's pubkey before
  the account is used as a destination/source. Helper-function
  presence (e.g. names like `verify_token_account_owner`,
  `assert_token_account_owner`, `check_ata_owner`) signals the
  safe form is available; a `verify_owned_by(<account>,
  token_program_id)` call alone is NOT enough.
- **Pinocchio:** equivalent shape. `pinocchio_token::state::TokenAccount`
  or `pinocchio_token_2022::state::TokenAccount` exposes
  `.owner()`; the owner-field comparison must happen.

- Compose-with-what: see the cookbook entries below. The
  critical chain is authority-signed-payout + role-token-account-
  not-anchored = direct theft of role's funds.

- Corpus: OS-SPR-ADV-00 (`solana-program/rewards` revoke
  handlers, April 2026) — `verify_owned_by(recipient_token_account,
  token_program.address())` present, internal-owner-field
  comparison absent. Fixed by adding
  `verify_token_account_owner(recipient_token_account,
  recipient.address())` in PR #33.

### `pda_lifecycle_reuse_after_close` — MEDIUM
A close handler fully deletes a parent PDA (returns lamports to
zero, leaves the account system-owned) instead of marking it
permanently closed in place. The PDA address is deterministic
from its seeds, so a subsequent `create_program_address` /
`find_program_address` with the same seed tuple succeeds at the
same address. Dependent ("child") PDAs whose seeds include the
parent's pubkey survive the close because their addresses are
likewise deterministic and they reference the parent only by
pubkey, not by account ownership. When the parent is re-created
at the same address, the child PDAs become "live" against the
new parent — carrying forward their pre-close state.

The shape is a state-machine flaw: the program treats "PDA
closed" and "PDA freshly initialized" as the same external state
(both are system-owned + empty), so it cannot distinguish "this
is a new campaign / session / round" from "this is the old one
reopened" at the address level. Stale-state revival follows.

Three observable signals; flag the finding when all three hold:

1. The close handler fully removes the parent (zeros lamports +
   reassigns to system program), vs. a close-in-place pattern
   that retains program ownership + a permanent
   closed-discriminator byte.
2. At least one other account family has PDA seeds that include
   the closing PDA's address (the child's address is stable
   across parent reopen).
3. The close handler does NOT enumerate-and-close those
   dependents — they outlive their parent.

- **Anchor:** `#[account(mut, close = receiver)]` on the parent
  PDA + at least one child PDA whose seeds reference
  `parent.key()` + no explicit close of the children in the same
  handler or a companion. Mitigation: keep the parent
  program-owned with an explicit `closed` discriminator
  (e.g. an `AccountType::ParentClosed` variant); reject re-init
  at the same address.
- **Native / Pinocchio:** the unsafe form is an explicit close
  helper (`close_pda_account(parent, ...)` / direct
  lamport-zeroing + `assign_to(system_program)`) that leaves the
  address system-owned. The safe form is an in-place close method
  that flips the parent's first byte to a permanent
  closed-discriminator while leaving the account program-owned —
  `create_account` then errors `AccountAlreadyInitialized` on any
  re-init attempt against the same seeds.

- Compose-with-what: pairs with `init_config_field_unanchored` or
  any "authority controls re-init seeds" finding to enable a
  full re-create-with-attacker-friendly-state flow.

- Corpus: OS-SPR-ADV-01 (`solana-program/rewards`
  `CloseDirectDistribution`, April 2026) — full-delete close
  allowed re-creation with the same seeds; pre-existing
  `DirectRecipient` child PDAs (keyed on the parent's pubkey)
  revived. Fixed in PR #32 by introducing
  `DirectDistribution::close_in_place` with a
  `DirectDistributionClosed` permanent-marker discriminator.

### `token_2022_extension_arithmetic_skew` — MEDIUM
Handler records a nominal token amount into program state — e.g.,
`config.total_allocated += amount`, `position.deposit = amount`,
`vault.outstanding = amount` — before or instead of measuring what
the corresponding `TransferChecked` CPI actually delivered. For
mints with Token-2022 extensions that modify in-flight transfer
behavior, the delivered amount differs from the requested amount.
Recorded state then drifts from the actual vault balance, and any
downstream invariant that reads the state field as ground truth
breaks silently.

The canonical case is the `TransferFeeConfig` extension
(fee-on-transfer mints): the fee is deducted in-flight and the
destination receives `amount - fee`. The program records
`amount`; the vault holds `amount - fee`; the gap is the bug.
Future extensions that further alter in-flight amount (rebasing
hooks, confidential transfers, scaled-UI tokens) produce the
same shape.

Three observable signals; flag when all three hold:

1. The handler accepts a mint that is not pinned to a specific
   extension profile — the program supports `TOKEN_PROGRAM_ID`
   and/or `TOKEN_2022_PROGRAM_ID` without restricting which
   Token-2022 extensions are permitted on the mint.
2. State is updated to `amount` (the requested figure) rather
   than `actual_amount = post_balance - pre_balance` (a measured
   delta on the destination token account).
3. Downstream code reads the state field as if it were ground
   truth (solvency checks, claim limits, accounting invariants,
   payout ratios).

- **Anchor:** look for `token::transfer_checked(...)?` or
  `token_interface::transfer_checked(...)?` immediately followed
  by `state.<field> += amount` (or `= amount`) rather than a
  pre/post-balance delta. Also flag if the program accepts
  Token-2022 mints (`token_program: Program<'info, Token2022>`)
  without an explicit constraint pinning the allowed extensions.
- **Native / Pinocchio:** the same shape —
  `TransferChecked.invoke(...)?` followed by direct field
  assignment instead of:

  ```rust
  let pre = get_token_account_balance(dest)?;
  TransferChecked { ... amount, ... }.invoke()?;
  let post = get_token_account_balance(dest)?;
  let actual = post.checked_sub(pre)?;
  // record `actual`, not `amount`
  ```

- **When to suppress:** the program explicitly rejects mints with
  non-trivial extensions at entry (asserts no `TransferFeeConfig`
  / `TransferHook` / `ConfidentialTransferMint` / `InterestBearingConfig`
  configured). Catch the explicit rejection in source before
  suppressing — common forms are an early extension-walk that
  errors on any disallowed type, or pinning to legacy
  `TOKEN_PROGRAM_ID` only.

- Corpus: OS-SPR-ADV-02 (`solana-program/rewards`
  `AddDirectRecipient`, April 2026) — recorded the caller's
  `amount` directly into `total_allocated` without measuring
  post-transfer delta. Fee-on-transfer mints underfund the vault
  relative to recorded state. Fixed in PR #34 with the
  pre/post-balance pattern.

### `cleanup_incentive_mismatch` — LOW (forward-compat / griefing)
A close-style handler requires signer X but routes the recovered
rent to recipient Y, where X ≠ Y AND the signing wallet is the
only signer required. The only party authorized to invoke the
cleanup pays the tx fee but receives no rent, so they have no
economic incentive to invoke. Result: fully-claimed / fully-
revoked / fully-settled PDAs accumulate on-chain indefinitely,
stranding rent and leaving ghost state for off-chain readers to
trip over.

Not a fund-loss finding in isolation, but worth surfacing because:
- The program models cleanup as someone's responsibility but
  doesn't align responsibility with reward.
- Stranded PDAs compound with any later finding that interprets
  stale state as live (see `pda_lifecycle_reuse_after_close`).

Distinct from `close_account_redirection` (catalog above), which
catches the WRONG-destination shape (signer-controlled receiver,
unvalidated). This category catches the MISALIGNED-AGENCY shape:
destination IS validated against a stored / expected field, but
no one whose signature is required has a reason to invoke.

- **Anchor:** `#[account(mut, close = receiver)]` where
  `receiver` ≠ the signing wallet AND no other party with a
  reason to invoke can also sign. Common shape: `receiver =
  stored_payer` on a PDA whose only signer is the role
  (recipient, claimant, beneficiary) that already received the
  goods.
- **Native / Pinocchio:** `close_pda_account(pda,
  recipient_other_than_signer)` with no signer who benefits.

- Suggested-fix shapes:
  - Make cleanup permissionless once a sentinel condition holds
    (e.g., `claimed == total`, `expires_at < now`), routing rent
    to the stored beneficiary regardless of who signs.
  - OR allow the rent recipient to also initiate the cleanup
    (`close_handler` accepts either signer).
  - OR pay the cleanup-signer a small fixed fee from the
    recovered rent (incentive-aligned but more complex).

- Corpus: OS-SPR-ADV-05 (`solana-program/rewards`
  `CloseDirectRecipient`, April 2026) — recipient signs, rent
  goes to `original_payer`. Recipients have no incentive to
  invoke cleanup; claimed PDAs perpetually open in practice.
  Fixed in PR #35 by adding a permissionless path once
  `claimed == total`.

### `field_chain_missing_root_anchor` — CRITICAL (forged-collateral-chain shape)
Spec-less only. **Distinct from `missing_owner_check`** — Anchor's
typed wrappers (`Account<T>`) close the runtime-owner question for
an incoming account, but **the *fields* on that typed account
remain untrusted at the field level**. A `Pubkey` field stored on
`Account<Bank>` was written by the program, but a key passed in
the handler's accounts struct claiming "I am that bank's
crate_token" is just bytes the caller supplied, unless the
validator pins it back to the bank's stored value.

A fresh auditor walking the catalog from `missing_owner_check`
will see "Anchor types this account, owner check enforced — no
finding" and move on. That's correct for the owner check, wrong
for field-level forgery. The forged-collateral-chain class is exactly this gap.

- **Anchor:** for every `Validate::validate()` (or per-handler
  validation block) and for each passed-in account A and field F
  on a stored state account S: trust is *anchored* iff F is
  referenced (`A.key() == s.f`, `S.f == A.something`). If A is
  only checked against another passed-in account B
  (`A.key() == B.field`), the chain is *internally consistent*
  but **not anchored** — attacker forges A and B together.
  Pattern to grep for: chains of `assert_keys_eq!` /
  `==` / `has_one` that thread through passed-in accounts without
  ever touching a stored-state field on a PDA-owned `Account<T>`.
- **Native:** same shape; walk every `key()` / `pubkey ==`
  comparison. If neither side is `<trusted-state>.<field>`, the
  comparison only proves consistency, not anchoring.
- Corpus: the fake-account collateral-chain class — the canonical
  example is a stablecoin bank where the deposit-token / mint /
  collateral accounts form an internally-consistent chain that's
  never anchored to the bank's stored token/mint fields (~$52.8M,
  2022).

### `close_account_redirection` — HIGH
Anchor `close = <destination>` field, or manual close via lamport
transfer to a destination, where the destination is signer-controlled
and not validated against an expected wallet (creator, treasury, etc.).
- **Anchor:** `#[account(mut, close = receiver)]` where `receiver`
  is `AccountInfo` or `UncheckedAccount` with no constraint.
- **Native:** manual `**from.try_borrow_mut_lamports()? -= x;
  **to.try_borrow_mut_lamports()? += x;` with no destination check.
- Pair with `missing_signer` or `permissionless` marker → drain rent
  from any closable PDA.
- Corpus: a lending-protocol collateral-ratio close-account bypass
  (2022, ~$25M near-miss; publicly-discussed post-mortem);
  token-account close to wrong destination is the Anchor
  `close = receiver` variant of the same shape.

### `discriminator_collision` — HIGH
Two account types with the same first-8-bytes discriminator (Anchor
default). Attacker submits an account of type A where type B is
expected; deserialize succeeds; reads return attacker-controlled
state.
- **Anchor:** look for explicit `#[account(zero_copy)]` types or
  user-named discriminators that overlap. Default Anchor discriminator
  is `sha256("account:<TypeName>")[..8]` — generic names risk
  collision (`State`, `Vault`, `Pool` shared across crates linked in).
- **Native:** explicit discriminator bytes; check for the same
  collision shape.
- Pair with `missing_owner_check` → forged-data trust.
- Corpus: insecure-deserialization (`unpack_unchecked` and similar)
  is the recurring shape that turns a discriminator collision into
  a forged-state-trust kill-chain.

### `pda_seed_collision` — HIGH
PDA seeds insufficient to discriminate between different domains —
e.g., user-vault PDA seeded with `["vault"]` instead of
`["vault", user.key()]` lets one user's vault occupy another's.
- **Anchor:** `seeds = [...]` lacking the user-pubkey or
  resource-id-shaped seed; static seeds across handler families.
- **Native:** `find_program_address(&[seeds], &id)` with seeds
  that don't include caller-distinguishing data.
- Pair with `missing_signer` → take over another user's account.
- Corpus: "PDA sharing across authority domains" and "authority not
  stored in PDA seeds" are the two recurring audit-firm sub-shapes
  of the same root.

### `unvalidated_remaining_accounts` — HIGH
Handler iterates `ctx.remaining_accounts` (or
`accounts.iter().skip(N)`) without validating type / owner / key.
Attacker passes a malicious account that satisfies the iteration but
not the implicit type assumption.
- **Anchor:** `for acc in ctx.remaining_accounts.iter()` without
  immediate `Account::try_from` (which checks discriminator+owner)
  or explicit checks.
- **Native:** any per-iteration `account_info_iter.next()` without
  type/owner validation.
- Corpus: "permissionless account-add via remaining_accounts"
  (governance-hijack-lite sub-shape) recurs across DeFi audits;
  watch for iteration that mutates state per-account without first
  pinning the account to a stored allowlist.

### `account_not_reloaded_after_cpi` — HIGH
Handler invokes a CPI that may mutate a passed-in account, then
reads that account's state without `account.reload()` (Anchor) /
re-deserialize (native). Stale read decisions trust pre-CPI values
that the CPI just changed.
- **Anchor:** `token::transfer(...)?;` followed by reads from the
  involved token account without `account.reload()?`.
- **Native:** repeated `unpack` of the same account before/after
  `invoke_signed`.
- Corpus: recurring audit-firm primitive; pairs with
  `token_2022_extension_arithmetic_skew` when the CPI is a
  fee-on-transfer (recorded `amount` ≠ actual delta).

### `init_without_is_initialized` — HIGH
Init-style handler that doesn't check whether the target account
has already been initialized. Re-init replays state, wipes existing
balance/votes/whatever.
- **Anchor:** `init` constraint requires the account to NOT exist
  (`payer = ...` allocates fresh). `init_if_needed` opts out of this
  protection — every use is a finding *unless* the body explicitly
  guards on a discriminator/sentinel field.
- **Native:** missing `if account.is_initialized` check at the top
  of init handlers; or the init handler accepts an existing account
  and overwrites in place.
- Corpus: recurring audit-firm primitive; the canonical
  forged-collateral-chain kill-chain pairs init-without-is-initialized
  with `pda_lifecycle_reuse_after_close` for full account replay.

### `oracle_staleness` — HIGH (DeFi-specific)
Spec-less only — handler reads a price/rate-shaped field from an
oracle account without verifying freshness (timestamp window) or
confidence (deviation bound).
- **Anchor / Native:** an oracle price-load call (e.g.
  `load_price_feed(...)`) followed by immediate use without a
  `get_price_no_older_than`-style staleness gate; or an aggregator
  read (e.g. `get_result()`) with no staleness check on the
  round-open timestamp.
- Corpus: cross-margin oracle manipulation (2022, ~$114M); a stablecoin
  oracle mispricing (2022, ~$1.26M); a flash-loan oracle pump (2022,
  ~$3.5M); collateral mispricing from a stale derived rate (2025,
  ~$5.8M). For the short-TWAP-window sub-shape (fresh oracle, gameable
  in one block) see `twap_gameable_single_block`.

### `frontrunnable_no_slippage` — HIGH (DeFi-specific)
Permissionless swap-shape handler accepts no `min_amount_out` /
`max_amount_in` parameter, or accepts one but never asserts on it.
Sandwich-bot bait.
- **Spec-aware:** handler effects modify two amount-shaped fields in
  opposite directions but no `requires` clause references the
  resulting ratio.
- **Anchor / Native:** `swap`-shape handler signature with no
  `min_*` parameter, or with one that's ignored in the body.
- Corpus: sandwich / MEV against AMM swap is the recurring shape;
  the cross-margin perp-market manipulation (2022, ~$114M) is the same
  primitive applied to a thin spot oracle rather than to a swap
  router. "Frontrun the permissionless `claim` / `crank`" is the
  same primitive on rate-limited cleanup handlers.

### `lamport_write_demotion` — MEDIUM
Direct lamport mutation via `**account.try_borrow_mut_lamports()? +=
x;` instead of `system_program::transfer(...)`. Demotes an executable
or rent-exempt account silently, can also bypass ownership checks
the runtime would otherwise enforce.
- **Native / Anchor (rare):** any direct mutation of
  `*account.lamports.borrow_mut()` outside a close path.
- Corpus: the lamport-transfer-freeze class (documented in public
  security-research write-ups). Same primitive turns up across
  audit-firm reports as "manual lamport mutation freezes
  rent-exempt / executable accounts."

### `init_config_field_unanchored` — CRITICAL (DAMM-v2 shape)
Spec-less only. The **write-side companion** to
`field_chain_missing_root_anchor`. An init handler accepts a
`Pubkey` (or address-shaped arg) and stores it directly into the
config / state account that downstream handlers later trust as a
"stored authority field." Because the stored value originated
from caller-supplied bytes — not from a canonical PDA derivation
or an authenticated signer — every later handler that trusts the
field is trusting attacker input.

The classic chain: `initialize` is permissionless (or the signer
isn't the canonical authority), attacker frontruns the legitimate
init with their own ATA / pubkey, the program persists it, and
subsequent fee / yield / withdraw handlers send funds to the
attacker-controlled address.

- **Anchor:** look at every `init` (or `init_if_needed`) handler.
  For each `Pubkey` / address parameter and each `vault_config.X =
  caller_supplied_X` assignment in the body: is `caller_supplied_X`
  bound to a `Signer<'info>` (the caller authenticated as that
  authority)? Is it the result of a `find_program_address` call
  with canonical seeds? If neither, the field is unanchored on the
  write side. Pair with permissionless init (no `Signer` constraint
  matched against pre-existing trusted state) for the full
  frontrun chain.
- **Native:** same pattern; trace each `state.field = <input>`
  back to the handler's account list. If the input is from
  `accounts[i].key()` without a signer check or PDA-derivation
  proof, the write is unanchored.
- Companion to `field_chain_missing_root_anchor`: that category
  catches *read-side* trust of unanchored fields; this one catches
  the *write* that planted the unanchored value to begin with.
  Both can ship in the same program (DAMM-v2 OOD eval found
  exactly this pair).
- Corpus: `damm-v2-fee-routing` Apr 2026 OOD eval — `creator_quote_ata`
  taken as init param, stored in `vault_config`, later trusted in
  `route_fees` as the canonical fee destination.

### `bounty_intent_drift` — varies (HIGH when intent is a security invariant)
Spec-less only. The handler / program ships with stated intent
(bounty description, README, docstring, comment, mode flag) that
the implementation **doesn't enforce**. Not a structural primitive
— a *gap between declared and implemented behavior*. Severity
follows whether the stated invariant was a security claim or a
UX nicety.

Three common shapes:

1. **Constant defined, never read.** `MIN_PAYOUT_LAMPORTS_DEFAULT
   = 1_000`, but no handler references it. The minimum-payout
   guarantee exists in the constants module and nowhere else.
2. **Stored field written at init, never read in handlers.**
   `vault_config.y0_total_allocation` set in `initialize`, never
   referenced in `route_fees` / `claim_fee`. The locked/unlocked
   scaling logic is stubbed.
3. **Mode/discriminator param accepted but downstream-equivalent.**
   Bounty says "quote-only fees"; `initialize` accepts
   `collect_fee_mode: u8` and persists it; `route_fees` doesn't
   branch on the value. `BothToken` (mode 0) silently passes,
   despite the bounty's quote-only claim.

The auditor walks:
- The bounty description / README / handler docstrings for
  stated invariants (text-search for "must", "always", "only",
  rate / window / cap claims).
- `cargo check --message-format=json` for `dead_code` warnings on
  constants / fields.
- Stored config fields' read-side: `grep` for the field name
  across all handlers; if zero readers, flag it.
- Mode parameters: trace the param into the body; if no `match` /
  `if` branches on the value, the mode is decorative.

Severity:
- **HIGH** when the stated invariant is a security claim (slippage
  bound, quote-only, rate cap, time window).
- **MEDIUM** when it's an economic claim that doesn't immediately
  translate to fund loss but could (rounding direction, fee
  discount).
- **LOW** when it's UX (event payloads with stale fields, etc.).

Corpus: `damm-v2-fee-routing` Apr 2026 — quote-only intent
unenforced, 24h crank entirely absent, `y0_total_allocation`
stored-and-never-read.

### `custody_terms_retroactive_mutation` — varies (HIGH when a retroactive change can strand or seize committed funds; MEDIUM when remediation is doc-only / bounded)
Spec-less primary (no CLI predicate yet — candidate `qedgen probe`
addition). A program takes custody of user value at one handler
(deposit / lock / stake / escrow) and releases it at another
(withdraw / claim / redeem / unlock / settle). The release handler
decides admissibility — *whether*, *when*, or *how much* can leave —
by reading a parameter from **mutable** program/config state (timelock
duration, transfer-hook program, exit fee, allow/deny list, paused
flag, oracle source). Because that parameter is read **live at release
time** and is **not snapshotted onto the user's receipt/position at
custody time**, a privileged handler that mutates it **after** the
deposit retroactively changes the terms governing funds the user
already committed: extend a lock indefinitely (strand), flip a
hook/allowlist to deny (strand), raise an exit fee toward 100% (seize),
or repoint an oracle (mis-price the exit).

Distinct from `bounty_intent_drift` (a declared-vs-implemented gap —
here the behavior IS implemented: the admin *can* set the param and the
release path *does* read it) and from `flash_loan_amplified_governance`'s
snapshot note (voting-power-at-block, not custody-terms-at-deposit). The
defect is a **temporal authority invariant**: terms binding
already-custodied funds must be fixed at custody, not re-read live.

Three observable signals; flag when all three hold:

1. **Custody/release split.** One handler takes custody of user value;
   a different handler releases it (deposit→withdraw, lock→unlock,
   stake→claim, escrow→settle).
2. **Live gate, no snapshot.** The release handler's admissibility
   decision reads the gating parameter from a config/state account
   **live**, and the receipt/position struct created at custody stores
   only commit-time metadata (`deposited_at`, `amount`) — **no
   snapshot** of the gating parameter (`unlock_at`, `terms_hash`,
   `fee_bps_at_deposit`, `hook_at_deposit`).
3. **Post-custody mutability, no grandfather.** A privileged handler
   (admin setter, governance, config update) can write that parameter
   after deposits exist, with **no monotonic / grandfather guard** that
   exempts already-committed positions ("new lock applies to future
   deposits only", "fee can only decrease").

Spec-less per-runtime:
- **Anchor:** for each release handler, trace the admissibility
  expression (the `require!` / `if … return Err` gating release amount
  or timing). Does it read `config.<param>` / `pool.<param>` live, or a
  field on the user's `Account<Receipt>` written at deposit? Then check
  the receipt struct: a snapshot field, or only `deposited_at` /
  `amount`? Then find admin setters (`set_*`, `update_config`,
  `#[access_control]` handlers) writing `<param>`: monotonic/grandfather
  guard present?
- **Native / Pinocchio:** same shape. The receipt is a zeropod/state
  struct created in the deposit processor; grep its fields for a terms
  snapshot vs only `deposited_at`. The release processor's gate reads
  the config account live. The admin processor writes the config field
  with no per-position guard. Canonical tell: a `validate`-style fn
  reads the **current** config duration/hook and compares against `now`
  while the receipt carries no `unlock_at`.

When to suppress / downgrade:
- The gating parameter is **immutable after init** (no admin setter) →
  not a finding.
- The receipt **snapshots** the terms at custody (the safe pattern) →
  not a finding.
- The admin is **explicitly and intentionally trusted** and the
  centralization assumption is **documented** as a known trust boundary
  → downgrade to informational / MEDIUM (doc-only remediation), not a
  fund-loss HIGH. Catch the documented-trust statement in source/README
  before downgrading; an *undocumented* trusted-admin-can-strand stays
  HIGH.

- Compose-with-what: pairs with weak admin auth (`missing_signer` /
  unanchored admin field → *anyone* rewrites the terms → CRIT) and with
  single-key admin + no timelock (`authority_transfer_missing_nominate_accept`
  neighbor) → the retroactive change is an instant rug. Safe fix:
  **snapshot the gating terms onto the receipt at custody** (compute
  `unlock_at = now + lock_duration` at deposit; pin `hook` / `fee_bps`
  on the position) so later admin changes can't reach committed funds.

- Corpus: a Pinocchio escrow with an admin-configurable timelock — the
  release path's `validate` reads the **live** `lock_duration` and
  compares to `now`, the `Receipt` stores `deposited_at` but **no
  `unlock_at` snapshot**, and the add-timelock handler has no monotonic
  guard, so the admin can extend the lock after a user deposits and
  strand the funds. Remediation was doc-only (admin treated as trusted),
  so it was firm-rated MEDIUM; the strand-funds capability is HIGH absent
  that documented trust.

### `transfer_hook_reentrancy` — HIGH (Token-2022 only)
Token-2022 transfer hooks can call back into the calling program
during a transfer. Handler that updates state across a transfer
boundary without the new state visible to the hook is reentrancy-
vulnerable.
- **Anchor / Native:** Token-2022 transfer (`transfer_checked` with
  `mint = TOKEN_2022_PROGRAM_ID`) where program state is mutated
  *after* the transfer with the pre-transfer state still trusted.
- Corpus: first Solana-native reentrancy class; documented across
  audit-firm Token-2022 advisories. No single famous public
  incident yet — the extension shipped after the last large
  exploit window.

### `rounding_direction_round_trip` — HIGH (DeFi-specific)
Spec-less only. Two-leg conversion pair (`A → B` then `B → A`, or
`mint` + `redeem`, or `liquidity_to_collateral` + `collateral_to_liquidity`)
where both legs round in the same direction — favoring the caller on
each leg. Round-trip is unconditionally profitable; attacker packs many
swap pairs per transaction and drains the pool over hours.

- **Detect** by reading the two converse conversion functions and
  asking: does one round up and the other round down? If both use
  `ceil_div` (or both use `floor_div`) on the same denomination, the
  asymmetry is missing.
- **Anchor / Native:** look for paired functions like
  `liquidity_to_shares` / `shares_to_liquidity`, `mint` / `redeem`,
  `deposit_to_lp` / `lp_to_deposit`. Verify the deposit-side rounds
  down (caller gets fewer LP) and the redeem-side rounds down (caller
  gets fewer underlying) — the asymmetric pair.
- Compose-with-what: low-fee bulk transactions (Solana's 5000-lamport
  flat tx cost makes hundreds of round-trip swaps per tx economical).
- Corpus: the canonical stable-swap rounding class (2022, ~$700M
  at risk; public disclosure pre-exploit). Same-class
  generalization of bidirectional rounding on any stable-swap or
  two-leg conversion pair; also recurs as "loss of precision / wrong
  rounding direction" across audit-firm reports.

### `duplicate_mutable_accounts_aliasing` — HIGH
Spec-less only. A handler accepts two or more accounts of the same
type as mutable parameters (e.g. `from_token_account`,
`to_token_account`). If the program doesn't assert `from.key !=
to.key`, an attacker can pass the *same account* for both — making the
transfer a no-op while the program's accounting believes funds moved.
Often combined with a fee or supply update that fires regardless.

- **Anchor:** look for `#[derive(Accounts)]` with two same-typed mutable
  fields and no `constraint = from.key() != to.key()`. Also flag if
  `has_one` constraints could reference both fields and they're not
  asserted distinct.
- **Native:** scan handlers that take two `TokenAccountInfo` / two
  `AccountInfo` of the same role; look for an explicit `from.key !=
  to.key` or absence thereof.
- Compose-with-what: any fee accrual that fires on the no-op transfer
  (the program thinks a swap happened, charges fees, updates pool
  state — but no atoms moved).

### `twap_gameable_single_block` — HIGH (DeFi-specific)
Spec-less only. Distinct from `oracle_staleness`: the oracle is fresh,
but its TWAP window is short enough (typically ≤ 1-2 slots) that a
single attacker-controlled transaction can move the window-averaged
price. Common in AMM-based oracles where the TWAP samples the spot
pool's current `sqrt_price`.

- **Detect** by reading the oracle's window length and comparing to
  attacker affordability for a one-block price impact. Window ≤ 60
  slots (~30s) is usually game-able with a flash-loaned position;
  windows ≥ 5min are typically safe.
- **Anchor / Native:** look for `latest_confirmed_round`-style reads
  or `observe(seconds_ago)` where the `seconds_ago` parameter is small.
  Also flag if the program uses spot-price (no window) and merely
  labels it "TWAP."
- Compose-with-what: flash-loan amplifier (attacker doesn't need
  capital); single-block atomic execution (move-borrow-repay).

### `liquidation_rounding_dust_accumulation` — MEDIUM (DeFi-specific)
Spec-less only. Liquidation handler rounds collateral seizure down
("attacker only gets `floor(value)` of collateral") AND rounds debt
repayment down ("only `floor(value)` of debt cleared"). Each
liquidation leaves a dust amount of debt outstanding; attacker
liquidates the same position repeatedly via tiny slices, accumulating
dust into a self-funding strategy.

- **Detect** by reading the liquidation handler's seize and repay
  arithmetic side-by-side; both rounding-down is the asymmetry.
- Compose-with-what: low minimum-liquidation-size (no
  `min_repay_amount` floor); permissionless liquidation (any caller
  can fire it).
- Distinct from `rounding_direction_round_trip` because there's only
  one "round" — the user calls it multiple times, not two legs in one
  tx.

### `flash_loan_amplified_governance` — HIGH (DeFi-specific)
Spec-less only. Composition class: governance handler reads voting
power from a live source (current LP balance, current staked balance,
current token holdings) rather than a snapshot at proposal-creation
time. Flash-loan a large position, vote, repay — vote counted, capital
returned in same transaction.

- **Detect** by reading the governance handler's voting-power
  derivation. `vault.amount()` or `staking.user_stake()` read at vote
  time = vulnerable. `snapshot.balance_at_block(proposal.created_at)`
  or merkle-proof from snapshot = safe.
- Compose-with-what: high-leverage flash loan source available on the
  same chain (Solana has multiple lending protocols routinely used as
  flash sources); permissionless vote submission.
- Corpus: same shape as the cross-margin oracle manipulations
  (2022) when applied to governance rather than collateral —
  the live-balance read at decision time is the gap in both.

### `authority_transfer_missing_nominate_accept` — MEDIUM (operational hardening)
Spec-less only. `set_authority` (or `transfer_admin`) writes the new
authority directly in one instruction, with no two-step nominate →
accept handshake. A fat-finger or compromised key writes a wrong /
attacker pubkey; no chance to revoke before subsequent admin ops are
attacker-gated. Operational hardening, not a code exploit per se, but
high-impact when it materializes.

- **Detect:** grep for `set_authority` / `transfer_admin` /
  `change_authority` writing a single field in one ix. Missing
  `pending_authority` field on state struct is the giveaway. Missing
  `accept_authority` ix is the second giveaway.
- Compose-with-what: no time-lock; single-key admin custody; off-chain
  key-management mistakes.
- Corpus: recurring audit-firm pattern across DeFi programs; the
  two-step handshake is now the default safe form across mature
  Solana protocols.

### `missing_rent_exemption_check_on_init` — HIGH
Spec-less only. Account initialization accepts a caller-supplied
lamports amount and doesn't enforce `lamports >=
Rent::get()?.minimum_balance(size)`. Account drops below rent minimum
and gets garbage-collected mid-protocol-operation; subsequent reads
see a zeroed account; reinitialization possible by an attacker.

- **Anchor:** `space` is set on `#[account(init, ...)]` but `payer` is
  not constrained on lamport amount, or a manual `system_instruction::
  create_account(..., lamports, ...)` uses `lamports` from caller
  input.
- **Native:** init paths missing `Rent::get()?.minimum_balance(size)`
  with explicit `assert!(account.lamports() >= minimum)`.
- Compose-with-what: `init_without_is_initialized` (post-purge
  reinit); `close_account_redirection` (post-purge takeover).

### `realloc_zero_init_data_leak` — HIGH (Anchor)
Spec-less only. Anchor `realloc` grows an account's data section
without zero-initializing the new bytes. The new tail contains
whatever heap fragment was previously at that address — potentially
adjacent account data, including secrets.

- **Anchor:** look for `realloc(new_size, false)` calls. The second
  parameter is `zero_init`; passing `false` skips zeroing. Default to
  `true` unless the program explicitly initializes the tail in the
  same instruction.
- Compose-with-what: account-type confusion at the read site (a
  downstream handler reads the un-zeroed bytes as if they were a
  field). Recurs in published audit-firm checklists.

### `sentinel_null_key_array_short_circuit` — MEDIUM
Spec-less only. Program iterates a fixed-size array of pubkeys (multisig
signers, validator set, oracle providers) and short-circuits on
`Pubkey::default()` (all-zeros) as "empty slot." Attacker submits a
transaction signed by `Pubkey::default()` — if the system ever lets a
zero-keyed signer through (which it shouldn't, but corner cases exist
with `AccountInfo`-typed signer params), all the short-circuit checks
pass.

- **Detect:** grep for `if signer.key == &Pubkey::default()` / `if
  key == [0u8; 32]`, especially inside an enumerate / fold over a
  signer array. The pattern is "use default-pubkey as a sentinel."
- Compose-with-what: weak signer validation (`AccountInfo` instead of
  `Signer<'info>`).

### `permissionless_instruction_no_rate_limit` — MEDIUM (composition class)
Spec-less only. A permissionless handler does meaningful state work
(emits an event, accrues a counter, advances a state machine, writes
a log) without any rate-limit, cooldown, or proof-of-work gate. An
attacker invokes it in a tight loop, exhausting the program's
counter / log capacity / event-buffer headroom for legitimate users.
DoS via state-bloat or counter-saturation.

- **Detect:** for each `permissionless` handler (no `auth` clause / no
  signer-key match), ask: what state does it mutate, and is there a
  per-caller / per-time cap on invocation? If neither, flag.
- Compose-with-what: any other finding gated by "this never happens in
  practice" — the permissionless-no-rate-limit handler is the
  amplifier that makes it happen.

### `permissionless_create_account_dos` — MEDIUM
Spec-less only. Handler creates an account at a deterministically-
derivable PDA address using `system_instruction::create_account`
(rather than the safer transfer+allocate+assign pattern). Any caller
can grief the future creation by pre-funding the PDA address with
1 lamport — `create_account` errors when target has non-zero lamports.

- **Anchor:** `init` constraint internally uses transfer+allocate+
  assign; raw `system_instruction::create_account` in a `#[program]`
  handler is the unsafe form.
- **Native:** look for `invoke_signed(&system_instruction::create_account(...), ...)`
  with seeds derived from caller-supplied or deterministically-public
  inputs (e.g. `[b"seat", market_key, trader_key]`).
- Corpus: a pre-audit order-book program used raw
  `system_instruction::create_account` against deterministic PDA
  addresses (seat PDAs and market-vault PDAs). Subsequently fixed via a
  `create_account` helper that does transfer+allocate+assign.

### `execution_order_state_before_check` — MEDIUM
Spec-less only. A handler mutates state field X in an early branch,
then a later branch reads X to make a decision. If the early branch
always precedes the later one (no conditional gate), the check reads
post-mutation state — rarely the author's intent.

Detection cue: an early-return / early-mutation arm of an `if let` /
`match` that zeroes / freezes a field that a later condition tests
for being nonzero / unmodified.

- Corpus: a pre-audit order-book program — in the place-order path, the
  no-deposit-mode branch zeroes `num_*_lots_out` and moves the matched
  amount into trader free funds. The later FOK check compares those
  fields against `min_*_to_fill` — but they were just zeroed, so FOK in
  no-deposit mode always fails the minimum-fill check. Subsequently
  fixed by reordering the branches.

### `flag_branch_no_op` — MEDIUM
Spec-less only. A `match` / `if-else` arm distinguishes two variants
A and B, but the body's primary effect is identical for both — only
secondary bookkeeping (a counter increment, a log line) differs. The
variant is effectively decorative.

Detection cue: `A | B => { primary_effect(); if variant == B {
secondary(); } }` where `primary_effect` is load-bearing and
`secondary` is local-only.

- Corpus: a pre-audit order-book program — in the order-matching path,
  a self-trade-behavior arm calls the same full-order-reduction helper
  for two variants. The post-branch only adjusts the inflight budget
  bookkeeping, never reduces the cancellation amount. One variant is
  documented as a *partial* reduction but is implemented identically to
  the full-cancel variant.

## qedgen-codegen runtime

When the runtime is **qedgen-codegen** (detected by the
`#[qed(verified)]` markers on handler bodies, or the no-std
codegen-target dep referenced by `qedgen init --target ...`),
the program is split into codegen-owned and user-owned files.
This changes how the catalog applies:

- **Codegen-owned** (`Cargo.toml`, `state.rs`, `errors.rs`,
  `events.rs`, `instructions/<h>/guards.rs`, the `lib.rs` Anchor
  wrapping, `formal_verification/Spec.lean`,
  `tests/{kani,proptest}.rs`): auditing these is auditing the
  codegen, not the program. Bugs here are spec-gap or
  qedgen-bug, not user-vulnerability.
- **User-owned handler bodies** (`instructions/<handler>/<handler>.rs`,
  the files qedgen prints "already exists — skipping (user-owned)"):
  this is the real attack surface. Hand-written Rust that may or
  may not honor the spec.

Most existing categories collapse on qedgen-codegen because the
codegen mechanizes them by construction:

- `missing_signer`, `missing_owner_check`, `account_type_confusion`,
  `field_chain_missing_root_anchor`, `pda_canonical_bump`,
  `pda_seed_collision`, `discriminator_collision`,
  `init_without_is_initialized`: codegen mechanizes these from
  the spec's `auth` / `accounts` / `pda` / lifecycle declarations.
  Apply at the spec-aware probe level only; per-handler-body
  re-check is rarely productive unless the user added hand-written
  divergence.
- `arbitrary_cpi`, `cpi_param_swap`, `account_not_reloaded_after_cpi`,
  `transfer_hook_reentrancy`: codegen owns the CPI block (driven
  by `transfers { }` or `call Interface.handler(...)`); user-owned
  bodies typically don't write `invoke` / `invoke_signed`. If the
  user *adds* hand-written CPI to a body, that's
  `spec_impl_drift_user_owned` (below).

Categories that **still apply** at the user-owned handler-body
level: `arithmetic_overflow_wrapping`,
`lifecycle_one_shot_violation`, `bounty_intent_drift`,
`custody_terms_retroactive_mutation`,
`frontrunnable_no_slippage`, `oracle_staleness` — bodies write
math, mutate state, accept params, and read external data, all
of which can drift from the spec (the release-admissibility gate
and the admin setter that mutates it both live in bodies).

Plus four qedgen-codegen-specific categories below.

### `spec_impl_drift_user_owned` — HIGH (qedgen-codegen)
User-owned handler body deviates from the spec's `effect` block.
Three flavors:

1. **Body does *more*:** writes a state field the spec doesn't
   model. The Lean / Kani / proptest artifacts are blind to the
   extra write — formal verification stays "green" while the
   actual state machine has an unmodeled side-channel.
2. **Body does *less*:** omits a field-write the spec declares.
   Codegen, Lean, Kani all honor the spec's broken view; the
   program runs with a stale field that callers trust.
3. **Body does *differently*:** uses unchecked arithmetic where
   spec says `+=` (checked), or saturating where spec says
   wrapping. Semantics drift.

Detection: cross-reference each spec `effect` field against the
user-owned handler body's assignments. Look for `s.field = ...` /
`*field += ...` / `state.field = ...` patterns that aren't in the
spec's effect block (extra), or spec effects that have no
corresponding body assignment (missing).

Severity: HIGH because the formal-verification artifacts become
stale silently — `lake build` green ≠ "program correct."

### `generated_guard_bypass` — CRITICAL (qedgen-codegen)
User-owned handler body skips the codegen-emitted
`guards::<handler>(self, ...)?;` call (or comments it out, or
narrows it to a subset). The codegen ships with the guard call
at the top of the user-owned scaffold; an agent or human can
drop it.

- **Detect:** `grep -L "guards::<handler-name>"
  programs/*/src/instructions/<handler>/<handler>.rs`. Every
  user-owned body must invoke its corresponding generated guard.
- Pair with `arbitrary_cpi` or `arithmetic_overflow_wrapping` →
  the body now does whatever, with no spec-derived
  authorization.

### `stored_field_never_written` — CRITICAL (qedgen-codegen)
The spec's state struct (or sum-type variant) declares a field
that **no handler `effect` block writes**, but other handler
guards or effect RHSes read it. Distinct from
`init_config_field_unanchored` (which is *written from
unauthenticated input*) — this field is *not written at all*,
so reads always return the type's zero / default.

- **Detect:** for each field F in `type State | ... of { F : T,
  ... }`, walk every handler's `effect` block and check whether
  any `F := ...` / `F += ...` assignment exists. If zero, but F
  is read in any guard / effect RHS / property, flag as
  CRIT/HIGH.
- Severity: CRIT if the read controls authorization (an unwritten
  `creator` / `authority` Pubkey defaults to `0x00` — anyone
  signing as the zero address would pass, depending on guard
  shape). HIGH if it's economic but not authorization. MEDIUM if
  it's only event payload / read-only.
- Common shape: a multisig-style `create_vault` declares
  `vault.creator` in its state struct but no handler effect writes
  to it, while downstream auth guards (`signer.key() ==
  vault.creator`) read it. The zero pubkey then authorises any
  signer.

### `qed_hash_drift_or_forgery` — HIGH (qedgen-codegen)
The `#[qed(verified, hash = "...", spec_hash = "...")]` proc-macro
content-pin can drift (the body changed, the hash didn't update —
`qedgen check --frozen` catches it) or be forged (a malicious
rebuilder edits the hash to match a tampered body). Auditor must
run `qedgen check --frozen --spec <spec>` before trusting the
verification claim.

- **Detect:** `qedgen check --frozen` on the spec — if the
  proc-macro hash doesn't match the canonical token-string of
  the body, drift. If the build pipeline doesn't include the
  frozen check, forgery is undetectable to downstream consumers.
- Severity: HIGH if forged (verification claim is a lie); MED if
  drift (out-of-date but caught at the next CI run).
- Corpus: same family as the broader "trusted upstream binary not
  pinned" pattern — any out-of-band claim that "this code matches
  what was verified" needs an in-band content pin plus a CI gate
  that enforces it.

## Cluster taxonomy (scaffold-to-spec interview)

*In v2.20, this taxonomy is a **Phase-2 fallback only** — surfaced as
cluster cards for sites whose intent the four-question ratification
didn't already classify. Most sites collapse automatically once
invariants / state machine / authority graph are ratified. See
[interview examples](references/interview_examples.md) for the
primary TUI-based flow.*

The interview groups probe findings by **cluster kind** — 14 categories
that map detected site shapes to candidate spec clauses. Each kind has
a Program-scope and Handler-scope variant; the algorithm promotes
clusters to Program scope when ≥3 handlers share the kind.

| Cluster kind | Triggers from | Spec-clause target |
|---|---|---|
| `account_owner_check` | Pinocchio `_unchecked` loads with owner-claim SAFETY; Anchor `AccountInfo` for token-shaped accounts; Native handlers reading data without `owner ==` check | `invariant owner_locked_writes "..."` or per-handler `requires <acc>.owner == self_program_id` |
| `account_init_check` | `_unchecked` loads claiming init precondition; Native handlers reading account data without init guard | `invariant accounts_initialized_before_use "..."` or `requires <acc>.is_initialized` |
| `account_signer_check` | Missing-signer findings across runtimes | `invariant authority_signs_state_change "..."` or `auth <authority>` |
| `account_type_tag_check` | Discriminator-collision sites; Anchor `AccountInfo` for typed accounts; Pinocchio bytemuck / raw-cast / indexed-access | `invariant account_type_tag_checked "..."` or `requires <acc> is .<Variant>` |
| `account_distinct` | Aliasing-mutable-borrow; Anchor missing `has_one` constraint pairs | `invariant distinct_account_aliases "..."` or `requires <a> != <b>` |
| `arithmetic_no_overflow` | Raw `+ - * /` on amounts/lamports outside `checked_*` family; Pinocchio `set_amount(amount() + x)`; Native `**lamports() -= x` | `invariant checked_arithmetic "..."` or per-effect `+=`/`-=` (checked, not `+=?` wrap) |
| `arithmetic_bound_pre` | Overflow sites with implicit caller-side amount bound | `requires amount <= <bound>` |
| `pda_canonical_derivation` | `Pubkey::create_program_address` (non-canonical); Anchor missing `bump` keyword | `pda <name> [<seeds>]` with canonical derivation |
| `pda_seed_uniqueness` | Shared PDA seeds across handler families | seed list includes a distinguishing field |
| `lifecycle_one_shot` | Init-without-is-initialized; Anchor `init_if_needed` | `handler init : State.Uninit -> State.Init` + `establishes init_is_one_shot` |
| `lifecycle_monotonic` | Re-init / close-without-zero-discriminator | State ADT + per-handler `pre -> post` annotations |
| `cpi_program_pin` | Unvalidated `invoke_signed`; Anchor `AccountInfo`-typed program accounts | `transfers { ... }` or `call Interface.handler(...)` (target pinned) |
| `cpi_account_direction` | From/to swap risk; ambiguous source/destination/authority | `transfers { from <s> to <d> amount <n> authority <a> }` |
| `dispatch_caller_establishes_callee_requires` | Batch-dispatch handler that doesn't re-check callee preconditions (the cf136e7 p-token shape) | `call Interface.handler(...)` mirroring callee's `requires` |

The interview UI walks these in confidence order (High → Medium → Low),
with Program-scope clusters before Handler-scope. The user answers each
with `accept` (emit clause), `narrow` (per-handler instead of program-
wide), `reject` (drop with rationale), or `bug` (real missing-check
to file as a finding).

## Compose-with-what cookbook

The bear-hug lives in chains. Walk this cookbook when a finding
looks "small" — a chain promotes it to the ceiling severity. Not
exhaustive; use as a thinking primer, not a checklist.

| Primitive A | + | Primitive B | = | Chain ceiling |
|---|---|---|---|---|
| missing_signer | + | arbitrary_cpi | = | full account takeover via CPI authority forgery (CRIT) |
| missing_signer | + | close_account_redirection | = | drain rent + state from any closable PDA (CRIT) |
| account_type_confusion | + | missing_owner_check | = | forged-data trust → arbitrary state read (CRIT) |
| pda_seed_collision | + | missing_signer | = | take over another user's account (CRIT) |
| non_canonical_bump | + | signer-derived seeds | = | signer impersonation, sign for any address (CRIT) |
| oracle_staleness | + | frontrunnable_no_slippage | = | sandwich-amplified single-block extraction (HIGH→CRIT) |
| arithmetic_overflow_wrapping | + | lifecycle_one_shot_violation | = | state corruption past intended ceiling (CRIT) |
| init_without_is_initialized | + | close_without_zero_discriminator | = | account replay, double-spend rent / votes (HIGH) |
| account_not_reloaded_after_cpi | + | mid-handler trust on stale balance | = | CPI return-value trust → fund loss (HIGH) |
| unvalidated_remaining_accounts | + | iterator-driven state mutation | = | injected accounts mutate authorized state (HIGH) |
| discriminator_collision | + | shared deserializer between handlers | = | cross-type spoof → privileged action (HIGH) |
| transfer_hook_reentrancy | + | mid-transfer state read | = | classic reentrancy (Solana-native, HIGH→CRIT) |
| permissionless marker | + | unbounded amount param | = | griefing / draining via repeated calls (HIGH) |
| permissionless init | + | unchecked authority field on init | = | attacker bakes their own pubkey as `mint_authority` / `withdraw_authority` / `admin` at init time → privileged CPI authority on every later operation (CRIT) |
| field_chain_missing_root_anchor | + | typed-but-unanchored CPI authority field | = | forge a fake collateral chain that the validator accepts as internally-consistent → invoke privileged CPI (mint, withdraw) under the real authority (CRIT, forged-collateral-chain shape) |
| init_config_field_unanchored | + | permissionless_state_writer init | = | frontrun legitimate init, bake attacker pubkey as stored "creator" / "authority" field, capture every fee/yield/withdraw routed through it (CRIT, DAMM-v2 OOD shape) |
| bounty_intent_drift (mode flag accepted but unbranched) | + | permissionless caller | = | invoke the "forbidden" mode the bounty claimed it didn't allow, every time (HIGH→CRIT depending on what the mode controls) |
| custody_terms_retroactive_mutation | + | single-key admin, no timelock | = | admin retroactively extends a lock / flips a withdrawal gate on already-deposited funds → strand or seize user custody (HIGH; CRIT if the admin field is also unanchored or the setter is permissionless) |
| bounty_intent_drift (spec docstring claims behavior the spec body doesn't enforce) | + | qedgen-codegen mechanization | = | formal-verification artifacts (Lean / Kani / proptest) faithfully translate the broken spec — `lake build` green proves the broken behavior, **giving false confidence that the program is correct** (HIGH-CRIT depending on what the docstring claimed) |
| spec_impl_drift_user_owned (body writes a state field the spec doesn't model) | + | downstream guard reads that field | = | unmodeled side-channel that formal verification is blind to (HIGH) |
| lamport_write_demotion | + | rent-exempt PDA | = | silent rent extraction, downstream rent failure (MED→HIGH) |
| saturating_by_design (`+=!`) | + | amount-shaped field | = | silent value loss, no error path (MED→HIGH) |
| token_account_role_anchoring (`<role>_token_account.owner` field not pinned) | + | authority-signed revoke / payout handler | = | authority redirects role's vested-but-unclaimed tokens to any same-mint wallet they control, no victim consent (CRIT) |
| token_account_role_anchoring | + | claimant-signed claim handler | = | malicious dapp UI tricks the claimant into signing with attacker's ATA in the destination slot → tokens leave the program to the attacker (HIGH, requires victim interaction) |
| pda_lifecycle_reuse_after_close | + | dependent child PDAs not cascade-closed | = | re-create parent at same seeds revives stale children with carryover state (MED on its own; chains to higher when child state controls funds) |
| cleanup_incentive_mismatch (signer ≠ rent recipient) | + | program assumes cleanup happens | = | ghost state accumulates on-chain, compounding with any later finding that reads stale state (LOW alone; compounds) |

## Classification rules

Each finding lands in one of three buckets, then gets a severity
keyed off attacker capability — not category label.

### Severity grading (attacker-capability rubric)

Use the chain's ceiling, not the primitive's:

- **CRITICAL** — direct fund loss, total state takeover, unbounded
  mint, or permanent denial-of-service to all users. Attacker
  capability: any user, any tx, repeatable. No special preconditions.
- **HIGH** — conditional fund loss (requires victim action, specific
  market state, or favorable timing), griefing of all users, or
  partial state takeover. Attacker capability: any user, but bounded
  by economic preconditions, victim cooperation, or competition.
- **MEDIUM** — exploit possible but bounded by attacker's own
  economic stake or narrow precondition; partial DoS; data leak that
  doesn't immediately translate to fund loss.
- **LOW** — surface anomaly that doesn't compose into a real attack.
  Surface as informational. **A LOW that composes to CRIT is reported
  as CRIT** — never let a chain's ceiling escape.

If you can't articulate a concrete attacker capability for the
severity you assigned, downgrade.

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

**Status:** fired | inconclusive
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
  .qed/findings/audit-20260426-1715.md       (full report)
  .qed/probe-suppress.toml                   (2 false-positives)
  target/qedgen-repros/audit/<id>.rs         (5 repros — ephemeral)

Next: review vault.qedspec, refine intent, re-run /audit for
spec-aware mode (precise gap detection + ratchet integration).
```

The `n silent-repro` count tracks findings dropped because their
reproducer didn't fire — they're suppressed silently per the v2.16
contract, but the count appears in the digest so the user knows the
audit attempted them. Zero is the expected number for a clean audit;
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
— see `references/finding_to_spec.md`. Eight families cover the
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
