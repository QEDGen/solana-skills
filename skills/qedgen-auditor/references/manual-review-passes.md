# Manual Review Passes

The end-to-end investigation workflow: work-list construction, review
phases, the intent interview, and reproducer emission. Split from the audit
handbook; `../SKILL.md` takes precedence where they overlap. Category
predicates live in [category-catalog.md](category-catalog.md), grading and
output format in [report-and-grading.md](report-and-grading.md).

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
   - Apply the per-runtime predicate from the
     [category catalog](category-catalog.md).
   - Walk the category's Corpus line for same-shape named incidents
     and recurring audit-firm patterns — for each one, ask "could
     this shape happen here?"
   - Classify: real-vulnerability / spec-gap / suppressed.

   **Eight cross-cutting passes MUST run alongside the per-category walk.**
   These catch primitives the per-category checklist misses on a cold
   read. 3a, 3b, 3d, 3e, 3f, 3g, and 3h run on every audit; 3c runs only
   when the program leans on a small security-critical dep (see its "When
   to run it" gate).

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
   checklist](data_structure_dep_invariants.md).

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
      `trust_surface_primitives.md` (alongside this file) documents the
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

   **3d. Comparison-direction / inverted-guard sweep.** A guard can be
   present, syntactically fine, and still enforce the *opposite* of its
   intent: `<` where `>` was meant, operands swapped, `+=` where `-=`
   belonged, an accumulator advanced with the wrong sign. The per-category
   catalog and the arithmetic-symbol probe both walk PAST this class — the
   probe keys off the operator *symbol* (saturating vs checked), not
   whether the comparison *direction* matches intent. Direction-correctness
   needs intent, so no static scanner can flag it without over-firing
   (it cannot know whether `<` should be `>`); this is therefore a **read
   discipline**, not a probe.

   Run it on every guard that gates a security-relevant quantity — a
   spending / withdrawal limit, a balance or delta check, a threshold /
   quorum / vote count, an expiry or time window, a fee or allowance
   accumulation. For each such guard, do two steps in order:

   1. **State the intended direction in one sentence** from the field's
      role alone, before reading the comparison ("spending must be
      REJECTED when `spent + amount > limit`"; "activation must require
      `now >= start`").
   2. **Read the actual comparison and confirm it matches.** If flipping
      the operator (or the operand order, or the accumulation sign) would
      still compile and still look plausible, the guard is a candidate —
      derive the input that satisfies the wrong direction and hand it to a
      repro. When the intent is captured in a `.qedspec`, this becomes a
      falsifiable Kani/proptest property (the reproducible path); absent a
      spec, the fired Mollusk repro is the evidence.

   Recognition signals: a comparison or `checked_add/sub` on a field whose
   role is a limit / balance / threshold / count, especially inside an
   `evaluate_*` / `check_*` / `validate_*` accumulation loop. Corpus: an
   inverted accumulation guard that let a spending limit be bypassed
   (2026 bench) — missed by two separate audit runs precisely because the
   guard looked present and reasonable.

   **3e. Store-without-validate sweep.** A handler that persists an
   externally-supplied account or `Pubkey` into program state — add-signer,
   add-member, set-authority, register-*, whitelist / allowlist insert —
   without validating it lets later logic trust a value that was never
   checked at write time. Run this on every state write of an account- or
   `Pubkey`-typed instruction argument.

   For each such write ask: **what predicate constrained this value before
   it was stored?** Expected validations, by intent: on-curve (a real
   Ed25519 point, not an unspendable off-curve key), system-owned /
   ownership check, a signer check on the value itself, canonical
   PDA-derivation, or non-default (`!= Pubkey::default()`). A write reached
   with none of these is a candidate finding — the exploit is realized in
   whichever *later* handler reads the field and trusts it.

   This is distinct from 3b: 3b asks whether a role parameter used *in this
   handler* is anchored; 3e asks whether a value being *persisted for future
   handlers* was validated at write time. A key can pass 3b (it plays no
   role in the storing handler) yet be a live finding under 3e (a later
   handler treats it as trusted). Corpus: an add-signer path that stored a
   caller-supplied signer without any on-curve / signable check (2026 bench,
   firm-rated but repeatedly missed on cold reads).

   **3f. Dead-guard / unwired-error-variant sweep.** An error variant that
   is *defined* but wired into no guard is a named intention the code never
   enforces — the maintainer named the check (the variant often spells out
   the invariant) but no call site ever fires it, so the path it was meant
   to protect proceeds unchecked. This class is invisible to the per-category
   catalog and to 3a–3e: there IS no guard to find a coverage gap against;
   the guard exists in name only.

   **Mechanized (#240): qedgen runs the enumeration + grep for you.** The
   probe envelope now carries the candidate list — every `#[error_code]`
   variant with zero enforcement call-sites in `src/` appears as an
   `unwired_error_variant` candidate (`handler` = the variant, `spec_silent_on`
   = its definition `file:line`). Read them off `candidates[]`; do NOT
   re-derive the sweep by hand — an earlier benchmark run showed free-form
   manual review under-executes it (one run missed the class, another mis-rated
   it). Your job is step 3 below: triage each candidate and grade it.

   1. **Enumerate + grep — done by the probe.** (Historically manual:
      enumerate the `errors.rs` error enum, grep each variant for a
      `require!` / `require_*!` / `err!` / `return Err(.. Variant ..)` /
      match-arm call-site in `src/`, excluding SDK/IDL/TS/doc mirrors. The
      probe's `unwired_error_variant` candidates ARE this result.) On a tree
      the probe cannot parse, fall back to running the grep by hand.
   2. **Confirm the grep** for any candidate you intend to file — one bare-name
      reference the sweep counts as enforcement can still be a non-enforcing
      use (a log line, a display arm), so a variant the sweep left *un*flagged
      is occasionally still dead; spot-check the load-bearing ones.
   3. **Triage each candidate.** Read the variant name and the handler/path it
      was evidently meant to guard, then ask: is the missing enforcement
      exploitable? A dead variant whose invariant is load-bearing (a
      signer/authority/limit the path assumes) is a real finding; a dead
      variant that a *different* guard already covers redundantly, or a
      deprecated/placeholder variant, is INFO.

   High signal-to-noise: a named-but-unused error is a strong signal that an
   intended check was dropped or never wired. Corpus: a defined-but-never-
   referenced authority guard let a privileged CPI path sign as a global
   authority with no check (2026 bench, advisory-rated HIGH) — missed by
   every comparison-direction / store-without-validate pass precisely because
   the bug is the *absence* of the guard the enum already names.

   **Severity:** a dead guard inherits the impact ceiling of the path it
   fails to protect — grade it per the Severity procedure (Classification
   rules → step 4), NOT at a dead-variant floor. The bench mis-rated an
   unwired global-authority guard LOW/INFO when the unguarded CPI signs as a
   *global* authority (HIGH).

   **3g. State-machine / lifecycle-transition soundness sweep.** A lifecycle
   step whose precondition handling is wrong is invisible to the guard-shape
   passes: the thing that's wrong isn't a comparison, a store, or a named
   error — it's a *missing completeness or robustness check* on a transition.
   Two shapes; run both on every audit:

   1. **Premature transition (completeness).** A container or multi-part
      object (a batch, a proposal bundle, a multi-step init) can be advanced
      to an "active / finalized / executable" state before all its
      constituent parts are added — and once advanced, the parts can no
      longer be added. For every status transition (`Draft → Active`,
      `Pending → Ready`, `Building → Sealed`), find the completeness
      invariant the type implies (a `size` / `count` / `expected_total`
      field, an "all children present" relation) and confirm the transition
      is gated on it. If any privileged actor other than the creator can
      trigger the transition, the exposure is a cross-actor grief (lock the
      object incomplete) — rate on that.
   2. **Bricked creation (permissionless-create robustness).** A
      permissionless `create` / `init` that *reverts* when the target
      address already holds lamports above the expected rent-exempt minimum
      lets an attacker pre-fund the address to brick creation forever. For
      every permissionless account creation, check it *tops up to* the rent
      floor rather than asserting equality with it / `create_account`-ing an
      address an attacker can front-fund.

   Corpus: a batch container activatable before all its transactions are
   added (firm MED, missed by every guard-shape pass); a PDA an attacker
   over-funds beyond rent-exempt to permanently block its creation (firm
   HIGH). Both are lifecycle soundness, not guard direction or coverage.

   **3h. Zero / sentinel-value asymmetry sweep.** A sentinel value —
   commonly `0`, `u64::MAX`, `Pubkey::default()`, an empty `Vec` — that one
   handler *rejects* as invalid while another *accepts* as meaningful (`0` =
   "no expiry / unlimited / never", empty = "any") is a cross-handler
   contradiction that strands funds or over-permits. For every field with a
   behavioral sentinel, diff how each handler treats it:

   1. Identify sentinel-bearing fields — expiry/deadline (`0` = never),
      limit/cap (`u64::MAX` = unlimited), an allowlist/destination set
      (empty = any), an optional authority (`default()` = none).
   2. For each, list every handler that reads or validates it and record
      whether it *rejects*, *accepts-as-special*, or *treats-as-literal* the
      sentinel. Divergence across handlers on the same field is the finding:
      creation that rejects a value a downstream path honors as a valid
      sentinel (or vice-versa) is a live inconsistency.

   Also flag a **one-sided bound**: a window or range guarded on only one
   end (an expiry / upper-bound check with no symmetric start / lower-bound
   check, so an action can land before the window opens) — the missing-bound
   cousin of §3d's wrong-direction guard.

   Corpus: creation rejects a zero expiry that the transfer path honors as
   "never expires" (two firm MEDs); a recurring transfer permitted before
   its start time for lack of a lower-bound check (firm HIGH).

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
   common compositions (the cookbook in
   [category-catalog.md](category-catalog.md) has more):

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
      - `BuildError` → a tool outcome, not an evidence state. Keep the
        finding `structural` only if the source independently
        establishes reachability; otherwise downgrade it to
        `hypothesis` (see [severity and evidence](severity-and-evidence.md)).
        Either way, note the limitation in the Reproducer subsection
        so the user knows the verdict isn't confirmed.

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
     See [probe orchestration runbook](probe_orchestration.md).

   - **Producer B — read-driven discovery.** §3c trust-surface walk,
     §3d comparison-direction / inverted-guard sweep, §3e
     store-without-validate sweep, §3f dead-guard / unwired-error-variant
     sweep, §3g state-machine / lifecycle-transition soundness sweep, §3h
     zero / sentinel-value asymmetry sweep, intent-drift sweep, authority ×
     invariant matrix. Producer B also
     hypothesizes internal intent (invariants / state machine /
     authority graph / threat model) from code + comments + docstrings
     *without* prompting the user — these hypotheses feed Phase 2.
     Long-running probe phases go to background; B continues foreground.

     Producer B is independent of Producer A. Missing/stale QEDGen, a failed or
     empty probe, an unsupported adapter, or a failed build does not stop the
     domain dossier, manual review, or Phase 2 ratification. Record the blocked
     executable lanes and continue from source evidence.

   - **Event-driven surface.** The instant any MED+ repro fires:
     surface immediately ("Found <category>: <one-line>. Repro at
     <path>. Continuing."). No batching. No draft report.

   The probe→skeleton→Crucible auto-chain (closes the gap that v2.19
   couldn't fire Crucible on brownfield audits) lives in the
   orchestration runbook — agent auto-ratifies *high-confidence*
   clusters into a skeleton spec without user interaction, then
   invokes `qedgen probe --fuzz <budget> --spec <skeleton>`.

   See [workflow walkthrough](workflow_walkthrough.md) for
   a timestamped end-to-end example.

   ### Phase 2 — post-first-finding intent interview

   Triggered automatically by the first MED+ surface, OR by Phase 1
   completing dry (framed: "Phase 1 didn't find a fired vuln; deepening
   needs your input on intent." — the ranked hypotheses are then the
   trust-winning first deliverable).

   The question source is the probe envelope's `hypotheses[]` —
   evidence-anchored `InvariantHypothesis` records the binary derives
   deterministically (authorization, lifecycle_init_once) — plus any
   deeper agent-derived hypotheses: the binary owns the deterministic
   classes; the agent owns cross-procedure and ambiguous ones
   (state-machine completeness, conservation, authority graph beyond
   signer/owner, threat scenarios, intentional gaps). Render each
   hypothesis conversationally with its claim, evidence, and payoff, and
   ask in-harness in every interactive venue — one answer per hypothesis
   (accept / narrow / reject / bug).

   Record the answers as `<audit-dir>/answers.json`
   (`{"run_id": …, "answers": [{"id": "<h-… or c-…>",
   "decision": "accept|narrow|reject|bug", "note": "…"}]}`) and run
   `qedgen ratify --audit-dir <dir>`. Hypothesis IDs (`h-…`) and legacy
   cluster IDs (`c-…`) are addressed uniformly; when `answers.json` is
   present, `interview.md` is neither consulted nor required.

   There is NO user-edited `interview.md` in the interactive flow. The
   only interview-less path is truly headless CI, where hypotheses stay
   deferred in the JSON envelope and only literal, source-anchored
   high-confidence clauses may be auto-ratified. Whatever the venue, the
   content and resulting ratification must be identical.

   See [interview examples](interview_examples.md) for
   three worked transcripts.

   ### Phase 3 — refined second wave

   Producer A re-prioritizes probes against ratified invariants.
   Producer B deepens intent-drift / authority sweeps with ratified
   authority graph. Same event-driven surfacing. Stop on user signal,
   budget exhaustion, or N consecutive units of work without a new
   finding.

   Re-run Crucible in domain mode after merging the ratified invariants. If
   Crucible is blocked, translate applicable invariants into agent-authored
   Mollusk tests or manual cross-handler obligations and mark executable
   verification pending rather than dropping the invariant.

   ### High-assurance profile

   Use the bounded, venue-neutral profile in `orchestration-profiles.md`. Run
   two independent passes and a third only if the second adds a distinct MED+
   candidate. Never exceed three passes, and never treat repeated model
   agreement as evidence. Independent workers are optional; sequential passes
   are the portable fallback.

   ### Spec-aware mode (when `.qedspec` already exists)

   Skip Phase 2's interview — the spec is the ratified intent. Phase
   1's Producer B uses the spec for invariants directly; auto-chain
   step skipped (use the existing spec, not a synthesized skeleton).
   Phase 3 continues.

   ### Runtimes the extractor doesn't cover (sBPF, exotic)

   sBPF/assembly is out of scope for the auditor (see the "NOT
   supported" note in [audit-handbook.md](audit-handbook.md)) — stop and
   redirect rather than scaffold.
   For other exotic-but-Rust shapes, hand-walk the source (for Anchor
   with an IDL, the probe's IDL-enrichment overlay supplies signer
   flags / `has_one` relations / status enums; `qedgen spec --idl` is
   deprecated).

   ### Artifact emission

   - Write the full audit report to `.qed/findings/audit-<timestamp>.md`.
   - Write the source-cited domain dossier to
     `.qed/audit/<timestamp>/domain-dossier.json` plus a Markdown rendering,
     including rejected and pending candidates.
   - Write `.qed/audit/<timestamp>/run-manifest.json`; update lane status,
     artifacts, blocked reasons, and resume commands throughout the audit.
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
