# PRD: Spec elicitation — probe → confirmed invariants → executable `.qedspec`

Status: **decided (D1–D6 answered); Phases 0–4 implemented 2026-07-17**.
Phases 0–3: `probe/hypothesize.rs` (auth + lifecycle detectors),
`probe/elicit.rs` (structured answers + clause lowering),
`run_id`/`spec_readiness`/`hypotheses[]` on the spec-less envelope,
mandatory ratify parse+check gate, `ratify --answers/--proptest`,
auditor-skill §3a retarget. Phase 4: `stamp` verb (attribute emission
gated on `.qed/verify-evidence.json`, recorded by every `qedgen verify`
run; only implementation-bound backends — miri / `kani_impl*` — flip
`implementation_verified`, and the evidence binds the program-source hash;
Mollusk probe repros confirm findings but not conformance); `adapt` + `spec --idl`
soft-deprecated (v3.0 removal, `--fill` pattern); IDL `has_one`
relations folded in as authorization-hypothesis evidence. Phase 5: the
remaining classes — `arithmetic_bound` (held checks lifted from bodies,
lowering `requires p <= N else Err` + error-variant ensure),
`conservation` (paired ops, abstains on any supply-changing flow;
lowering stays with the agent), `cpi_integrity` (pinned SPL callee +
resolved `Transfer{}` roles, lowering `transfers { … }`),
`unwired_guard` (#240 candidates as confirm/deny questions; accept
routes to a missing-enforcement finding), and `state_machine` (the IDL
status enum lifted into `type State` via positional-rewrite lowering —
the M-01 catcher). All seven §6.1 classes ship. Remaining: v3.0
hard-removal of the deprecated verbs; conversion metrics via
hypotheses.json ⋈ elicitation-outcome.json on run_id.
Date: 2026-07-17.
Motivating evidence: the 2026-07-16 auditor bench run (probes caught 2/9; the
spec-less envelope came back `findings: []`, `candidates: []` before the
dead-guard probe #240) and the question — *"the probe should extract
invariants, hypothesize about the program, then ask the user to confirm —
spec elicitation. Why isn't this present?"*

> **How to read this doc.** Every fork where I need your call is a
> **◻ Decision** block with a blank line to write on. I've put a recommendation
> in each; overwrite or strike it. Nothing here is built yet.

---

## 1. The question, answered first

**Is it present outside probe?** Partially — and that partial-ness is the
problem. Three pieces exist:

1. **The scaffold-to-spec cluster interview** (`probe/cluster.rs` →
   `probe/prompts.rs` → `probe/ratify.rs`) — a real confirm-each loop
   (accept/narrow/reject/bug), but the clauses it offers are **generic
   security templates**, not claims about *your* program (`cluster.rs::kind_meta`
   is a static table — every program gets the same wording).
2. **The domain interview** (`probe/domain_extract.rs` →
   `probe/domain_interview.rs`) — the right shape (questions about intended
   behavior) but its behavioral arrays are **seeded empty on purpose**:
   `run_helpers.rs:130` — *"semantic domain arrays intentionally start empty
   and the auditor fills them from source review."* Hypothesis-formation is
   **delegated to the agent**, not done by the tool.
3. **Code-to-spec transcribers** (`spec/idl2spec.rs`, `adapt/*`) — turn an IDL
   or handler set into a spec *shell* with `// TODO` markers. `idl2spec` infers
   lifecycle only from instruction-name string matching.

So the *plumbing* to accept a spec exists end to end. What's missing is the
hard part you named — **hypothesize program-specific invariants and put them in
front of the user** — plus a **motivational bridge**: `qedgen probe` ends at a
JSON envelope of findings; nothing says *"here are the five promises your
program appears to make — confirm them and I'll turn each into an executable,
honestly labelled check."*

**Why isn't it present?** A deliberate bet — "Agent+LSP is the analysis
substrate; probe is glue" — put invariant inference in the agent layer and left
the binary to scaffold. That kept the binary dumb, but it means the
highest-value moment in the funnel depends on an agent doing deep work it
demonstrably skips under pressure (the bench: workers left those arrays empty).
The *objective* is in the architecture; the *conversion mechanism* is the
missing product.

---

## 2. The simplicity commitment (read this before the design)

You asked for simple. The honest starting point is that we are **already too
complex**: there are four ways to turn code into a spec and two interview
tracks, and none of them delivers the one thing that matters.

Code→spec surfaces today:

| Verb | Produces | Fate under this PRD |
|---|---|---|
| `probe --emit-spec-candidates --audit-dir` | skeleton + interview + domain dossier | **becomes the one front door** |
| `adapt --program` (scaffold) | `.qedspec` skeleton with `// TODO` | **folds into probe** (see §5) |
| `spec --idl` | `.qedspec` shell from IDL (name-heuristic lifecycle) | **folds into probe** (IDL becomes an evidence source, not its own verb) |
| `interface --idl` | Tier-0 CPI interface block (callee shape) | **stays** — different artifact (composition, not elicitation) |
| `adapt --program --spec` (attribute) | `#[qed(verified, …)]` stamps | **stays**, renamed for clarity (post-spec, not elicitation) |

The rule for every design choice below: **one front door (`probe`), one output
(a real partial spec), and each new capability must let us delete an old
surface.** Simplicity shows up as things removed, not just added. If a proposal
adds a module without collapsing a verb or a track, it is suspect.

Concretely, this PRD proposes we end with **three** user-facing spec-surface
verbs instead of five overlapping ones:

- **`probe`** — scan → hypothesize → confirm → write a partial spec (the funnel).
- **`interface`** — IDL → CPI interface block (unchanged).
- **`stamp`** (today's `adapt --program --spec`) — stamp verified handlers.

`ratify` remains a workflow verb: it applies answers from an audit working set.
It is not another code-to-spec entry point, and it never invents hypotheses.

> ### ◻ Decision D1 — the verb consolidation
> Do we commit to collapsing `adapt --program` (scaffold) and `spec --idl` into
> `probe`, leaving `probe` / `interface` / `stamp` as the only spec verbs?
> **My recommendation:** yes — it's the single biggest simplification and the
> redundant scaffolds actively confuse (three verbs, three different TODO-shells).
> Keep `adapt --program` as a deprecated thin alias for one release, then remove.
>
> **Your answer:**
> Agree to consolidate, ok to remove adapt. One thing to note is that we are still gonna rely on agent to hypothesize

---

## 3. Objective

Make `qedgen probe` end not with a list of findings but with a **ranked set of
confirmable invariant hypotheses about *this* program**, each carrying its
evidence and its payoff, so that confirming them **directly produces executable
`.qedspec` clauses** that parse, check, and generate an explicitly labelled
verification artifact.

Success: a brownfield user who ran `qedgen probe` out of curiosity leaves with a
partial-but-real, checking `.qedspec` they didn't author from scratch, a clear
reason each clause earns its keep, and no ambiguity about whether a green result
exercised the spec model or the real program.

### 3.1 The assurance contract

Elicitation creates a claim; it does not by itself prove that the implementation
honors the claim. Every hypothesis and result carries one of three assurance
levels:

| Level | Meaning | Allowed user-facing claim |
|---|---|---|
| **checking** | The emitted clause parses, lints, and generates its promised artifact. | "This is an executable specification clause." |
| **model-tested** | Generated proptests exercise the `.qedspec` state-machine model. | "The generated model passes this property test." |
| **implementation-verified** | A source-bound backend (for example impl-Kani, Mollusk, or Miri) exercised the real handler. | "The implementation enforces this property within the backend's stated boundary." |

The CLI must display the level beside every green result. It must never render
`model-tested` as "proved on your program." `stamp` accepts only
`implementation-verified` evidence; a checking or model-tested clause is not
eligible for `#[qed(verified)]`.

---

## 4. Target user journey

```
$ qedgen probe --program programs/vault

  Scanned 12 handlers. I have 6 hypotheses about what this program guarantees.
  Confirm the ones that match your intent — each becomes an executable spec
  clause with its assurance level shown explicitly.

  H1  AUTH · high    `withdraw` requires the caller to be the vault authority.
      evidence:  has_one = authority (vault.rs:41); require_keys_eq (vault.rs:88)
      payoff:    can verify that unauthorized calls are rejected
      backend:   impl-Kani when this handler shape is source-bindable
      [ ] yes   [ ] no, it's permissionless   [ ] BUG (should gate, doesn't)

  H2  CONSERVE · med Total token balance is conserved across deposit + withdraw.
      evidence:  paired transfers (vault.rs:70, :95); no mint/burn
      payoff:    generates a conservation property over the spec model
      backend:   proptest (model-tested); implementation binding unavailable
      [ ] yes   [ ] no   [ ] BUG
  ...

  Answer inline (the agent asks; you reply in the conversation). Confirmed
  hypotheses are applied on the spot:
  → vault.qedspec + labelled checking/model-tested results
```

Two design bets: (a) a hypothesis with **evidence + a concrete payoff line**
persuades where an empty questionnaire does not; (b) a **cheap first executable
check in the same session** converts "interesting" into "worth it." A green
model test is the first hook; a source-bound counterexample against the real
handler is stronger and must be labelled differently.

---

## 5. What this means for `adapt` (the thing you asked about)

`adapt` is doing two unrelated jobs under one verb, which is itself a
simplicity problem:

1. **`adapt --program` (scaffold)** walks handlers and emits a `.qedspec`
   skeleton full of `// TODO: requires / effect / state machine`. This is a
   *strictly weaker* version of what `probe → ratify` produces: probe offers
   confirmable hypotheses and ratify writes *real* clauses, where adapt only
   ever writes empty TODO stubs. **Two code paths, same intent, different
   quality.** → **Fold it in.** The skeleton becomes a byproduct of the probe
   flow (it already is — `write_audit_working_set` writes a `skeleton.qedspec`).
   A user who today runs `adapt --program` should run `probe` and get something
   better.

2. **`adapt --program --spec` (attribute/stamp)** emits `#[qed(verified, …)]`
   lines for an *existing* spec. This has nothing to do with elicitation — it's
   the post-verification stamp. Bundling it under "adapt" (which otherwise means
   "code→spec") is the confusing part. → **Split it out** as its own verb
   (proposed `stamp`), so "adapt" stops meaning two things.

Net for `adapt`: the verb **goes away**. Scaffold folds into `probe`; stamping
becomes `stamp`. One fewer verb, and the two remaining jobs each live under a
name that says what they do. This is the concrete "make it simple" move for
your question.

### 5.1 What `stamp` does (definition)

`stamp` introduces **no new hashing behavior** — it is today's
`adapt --program --spec`
path (`anchor_adapt::compute_attributes` / `render_attributes`) surfaced under
an honest name, with one new safety gate: it requires recorded
`implementation-verified` evidence. It **runs after verification** and does no
proving itself.

Given a program and its already-verified `.qedspec`, it emits one attribute per
handler to paste above the `pub fn`:

```rust
#[qed(verified, spec = "vault.qedspec", handler = "withdraw", hash = "…", spec_hash = "…")]
pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> { … }
```

`hash` is a content hash over the handler body; `spec_hash` over that handler's
spec block (it also seals the `Accounts` struct shape). At **compile time** the
`qedgen-macros` proc macro recomputes both and fires `compile_error!` if either
drifted — an edited body or a changed spec clause breaks the build until you
re-verify and re-stamp.

Precise contract: the macro checks **drift, not correctness.** It does not
re-run the proof; `verified` asserts a source-bound verification already
happened, and stamp requires that recorded evidence before emitting. So the
division of labor is: **a source-bound backend establishes the implementation
claim, stamp freezes it, the compiler guards the freeze.** It is the "keep it
fixed" ratchet — without it, `#[qed(verified)]` would silently lie the moment
someone touches the code.

Scope caveat: **Anchor-only today** (it round-trips through the Anchor project
parser to locate each handler body). Broadening to Pinocchio/native is a
separate, out-of-scope decision — flagged in D2 below.

> ### ◻ Decision D2 — `adapt`'s fate
> (a) Retire `adapt` entirely: scaffold → `probe`, stamp → new `stamp` verb.
> (b) Keep `adapt` as the stamp verb only (rename its meaning), scaffold → `probe`.
> (c) Leave `adapt` as-is, just stop developing it.
> **My recommendation:** (a) — cleanest; "adapt" currently means two things and
> that's the confusion. (b) is fine if you'd rather not churn muscle memory.
>
> **Your answer:**
> Ok to retire

---

## 6. Design — one path

The whole feature is **one pipeline**, not two interview tracks. Today's
`cluster` (structural) and `domain` (behavioral) interviews merge into a single
list of hypotheses. Fewer concepts, one artifact.

```
                                    ┌─▶ in-harness questions ─▶ structured answers   (interactive)
probe evidence ─▶ hypothesizer ─────┤                                               │
(already computed) (new, §6.1)      └─▶ deferred to JSON envelope (headless CI)      ▼
                                                                      ratify --check [--proptest]
                                                                            └─▶ .qedspec + labelled result
```

### 6.1 The hypothesizer (`probe/hypothesize.rs`, new — the one new idea)

Consumes evidence the probe **already computes** — handlers, IDL signer/writable
flags (#235 overlay), intent tags (`handler_intent`), arithmetic/paired/lifecycle
scanner hits, dead-guard candidates (#240), PDA seeds — and emits program-specific
`InvariantHypothesis` records (claim + evidence + payoff + confidence +
how-to-lower). Six evidence-anchored classes, each a thin detector over existing
signals: **Authorization, Conservation, Lifecycle/init-once, Arithmetic-safety,
CPI-integrity, and Unwired-guard-as-question** (a #240 candidate flipped into
"you named this check but never wired it — should it hold?").

Precision rule (the make-or-break): **no evidence anchor → no hypothesis.** Five
right beats thirty speculative; one wrong hypothesis burns the trust the whole
pitch runs on.

### 6.2 Confirmation is in-harness; `ratify` consumes structured answers

The interview happens **in the harness** — the agent asks, the user answers in
the conversation — not in a file the user opens. `ratify` consumes a structured
answer set (`{hypothesis_id → accept | reject | bug + note}`), not hand-edited
markdown; same accept/reject/bug semantics, minus the file round-trip. The
"BUG" answer routes to a finding — so elicitation doubles as a bug-catcher. A
written transcript of the Q&A is emitted for provenance, but it is an *output*,
never the thing the user edits.

The one case with nobody to ask is truly headless CI: there the run emits
hypotheses in the probe JSON envelope, auto-ratifies only literal/high-confidence
ones, and defers the rest (§6.5). No user-edited file exists in either path —
the previous `interview.md` interaction model is dropped.

### 6.3 Lowering to executable clauses (fold into `ratify`)

The current miss: accepts often become description-only invariants or `// TODO`
comments. Each class needs an AST-level lowering to syntax accepted by the
current parser. These are the initial contracts:

| Class | Minimum evidence | Emitted `.qedspec` shape | First assurance/backend | Abstain when |
|---|---|---|---|---|
| Authorization | One unambiguous signer plus a source/IDL binding to the stored authority (`has_one`, key equality, or equivalent). | `auth authority` plus, when needed, `requires state.authority == authority.pubkey else Unauthorized` inside the handler. | `checking`; `implementation-verified` only when impl-Kani or a runtime reproducer binds the real handler. | Multiple plausible signers, no stored-authority binding, or permissionless-intent evidence. |
| Lifecycle/init-once | An init handler, an identifiable state discriminator/status, and evidence for both pre- and post-state. | `handler initialize (...) : State.Uninitialized -> State.Active { ... }`; add `establishes <property>` only when that property has an executable body. | `checking`; the model transition may become `model-tested` under proptest. | Either endpoint is inferred only from the handler name or the state representation is ambiguous. |
| Arithmetic bound | A specific parameter, bound expression, and error path/check site. | `requires amount <= state.limit else InvalidAmount`. | `checking`; implementation level requires a source-bound reject harness. | The bound is guessed from type width or naming alone. |
| Conservation | Complete inventory of relevant transfer/mint/burn/close paths and a representable total. | `property token_conserved : state.total_tokens == old(state.total_tokens) preserved_by [deposit, withdraw]` (with the detected total expression substituted). | `model-tested` first; implementation verification is runtime-dependent. | Any value-moving CPI/path is unresolved, accounts may alias, or supply-changing operations are not ruled out. |
| CPI integrity | A resolved callee program identity and account-role mapping. | `call <Interface>.<handler>(...)` or the existing `transfers { ... }` sugar. | `checking`; implementation level requires a runtime/source-bound harness. | Target identity or account direction is unresolved. |

Ratify merges the emitted AST exactly as it merges cluster syntax today, then
must run the parser and `check` before reporting success. **User confirmation,
not detector confidence, controls activation:** any confirmed, lowerable clause
is injected live; an unconfirmed clause is never active. Confidence controls
ranking and whether a hypothesis is shown. If a confirmed hypothesis cannot be
lowered without placeholders, ratify keeps it in the dossier and reports
`confirmed, not executable` rather than inserting a misleading comment.

### 6.4 The motivational bridge (the product bet, not a module)

- **Default-on, ranked, human-readable.** The hypothesis summary is the default
  tail of every `probe` run (stderr; JSON still on stdout for agents). Today it
  hides behind `--emit-spec-candidates --audit-dir`.
- **Payoff per hypothesis** — the conversion copy names the strongest available
  assurance honestly ("if confirmed, I can check/model-test/verify …").
- **Cheap first check, same session** — `ratify --check` is mandatory;
  `--proptest` runs the generated model harness and labels it `model-tested`.
  When a source-bound backend is available, show its separate
  `implementation-verified` result inline.

### 6.5 Where elicitation runs — the audit agent is the primary surface

The hypothesizer (§6.1) is **one engine with two thin renderers**, chosen by
context — not two implementations:

| Consumer | Confirmation | When | User commitment |
|---|---|---|---|
| **Interactive** (audit agent; `probe` in a terminal) | in-harness questions — asked and answered inline | after a few real findings (agent) / at run end (CLI) | one answer per question, no file |
| Headless CI (no human) | none possible — hypotheses emitted to the JSON envelope; only literal/high-confidence auto-ratified, the rest deferred | non-interactive | none |

**The audit agent is the primary surface, not the probe funnel.** The agent's
objective is unchanged — find vulnerabilities. Elicitation rides along because
the two are the same activity seen from opposite ends: a **violated** invariant
is a finding; a **held** invariant, once the user confirms it is intended, is a
spec clause. The agent already hypothesizes invariants to hunt bugs; surfacing
the ones it could *not* violate, as confirm/deny questions, turns audit output
into a spec for free.

**Earn-then-ask.** Do not front-load an interview the user must commit to. The
agent leads with bug-finding (Phase 1 autonomous discovery); once it has
surfaced a few confirmed findings — the "first MED+ buys time" trust it has to
earn anyway — it presents the top-N invariant hypotheses as in-harness
questions. By then the user is engaged and each question is one answer, in the
conversation. There is no file to open — a file "requires more commitment"
precisely because it asks before trust is earned and forces a context-switch out
of the harness. Headless CI, with nobody to ask, is the only path without an
interview: it defers via the JSON envelope.

**What this changes in the auditor skill.** Today `SKILL.md` §3a runs an
upfront "Phase 2 intent interview" off `domain-invariant-extraction`, confirmed
via the file. The change: (a) the interview stops being an upfront gate and
becomes an opportunistic, post-first-findings in-harness pass; (b) its questions
*are* the hypothesizer's `InvariantHypothesis` records rendered conversationally;
(c) confirmed invariants do double duty — steer the refined second wave **and**
lower to spec clauses. No new phase; a lighter, better-timed version of the one
that already exists.

**Assurance carries through unchanged (§3.1).** An in-harness confirmed
invariant is `checking` until a backend runs. The audit flow already gates
HIGH/CRITICAL with reproducers (`implementation-verified`); a confirmed
invariant that then passes proptest is `model-tested`, and only an impl-bound
pass earns `#[qed(verified)]` via `stamp`.

> ### ◻ Decision D3 — how much lives in the binary vs the agent
> The binary owns the six deterministic, evidence-anchored classes; the agent
> keeps the deep cross-procedure cases (state-machine completeness, cross-path
> dataflow). The binary's job is *conversion*, not completeness.
> **My recommendation:** yes to this split — it's what keeps the binary simple.
> But tell me if you'd rather the binary stay purely mechanical and even the six
> classes live in the skill (smaller binary, more agent dependence).
>
> **Your answer:**
> Agree to keeping binary simple and push all the ambiguity to the agent. Agent is better placed to judge and also present hypothesis to user if required

> ### ◻ Decision D4 — confirmation surface (you decided: in-harness, no file)
> **Decided:** the interview is **in-harness** for every interactive run — the
> agent (or `probe` in a terminal) asks, the user answers in the conversation;
> `ratify` consumes structured answers. **No user-edited `interview.md`.** The
> only path without an interview is truly-headless CI, which defers hypotheses to
> the JSON envelope (§6.2, §6.5).
> Remaining sub-question: what triggers the in-harness pass in the *audit* flow?
> A fixed threshold (e.g. ≥1 confirmed MED+), agent judgment, or a user-invocable
> "ask me the invariant questions now"?
> **My recommendation:** agent judgment with a floor (never before ≥1 confirmed
> finding) *plus* a user-invocable trigger; avoid a rigid count.
>
> **Your answer:**
> Ok for agent judgement, thumb rule is win trust before asking for commitment. However, in genuine cases where there is not finding to win trust quickly, invariant hypothesis becomes that.

---

## 7. Phased plan

Each phase is independently useful and each **removes or subsumes** something.

- **Phase 0 — instrument the funnel (small).** Add a stable `run_id` to the
  working set and carry it from `probe` through `ratify`; add a
  `spec_readiness` block (hypothesis counts by class/confidence) and local
  outcome/timing fields. Counts measure supply; the joined run records measure
  conversion and time-to-first-check. No UX or removal yet.
- **Phase 1 — hypothesizer, auth + lifecycle first (medium).**
  `probe/hypothesize.rs` for the two highest-prior classes, replacing the
  `pending:user` domain seeds for those classes. Corpus-fixture gated.
- **Phase 2 — lowering to executable clauses (medium).** Auth + lifecycle AST
  lowerings in `ratify`; snapshot test that the ratified spec **parses and
  checks**; report `confirmed, not executable` on abstention. **Subsumes
  `adapt --program` scaffold** (now strictly worse).
- **Phase 3 — the bridge (medium, the bet).** Default-on ranked summary + payoff
  and backend lines + mandatory `ratify --check`; optional auto-proptest is
  labelled `model-tested`. Surface source-bound verification separately where
  supported. **Audit-agent surface (§6.5):** render the same hypotheses as
  in-harness questions after the first confirmed findings; `ratify` consumes the
  structured answers (no user-edited file); headless CI defers via the JSON
  envelope. This — the audit agent, not the CLI funnel — is where we measure
  conversion first.
- **Phase 4 — fold IDL in, retire verbs (small).** `spec --idl` / `idl2spec`
  become an *evidence source* for the hypothesizer, not a standalone shell
  emitter; deprecate `adapt`/`spec --idl` per D1/D2.
- **Phase 5 — remaining classes + the bench miss-set (ongoing).** Conservation,
  arithmetic, CPI classes; then state-machine-completeness as a class (which is
  exactly what would have caught M-01).

> ### ◻ Decision D5 — scope of the first cut
> Is Phases 0–3 (auth + lifecycle, file-based, proptest-only) the right first
> release to put in front of a real user, holding conservation/arithmetic/CPI
> and the verb-retirement for after we see conversion?
> **My recommendation:** yes — smallest thing that can prove the funnel works.
>
> **Your answer:**
> agree

---

## 8. Success metrics

- **Executable-clause conversion** — % of spec-less `probe` runs that yield ≥1
  confirmed clause which passes parse + check. The headline.
- **Intent precision** — `(accept + bug) / answered`; a `BUG` answer confirms
  the intended invariant while reporting missing enforcement. Track rejection
  separately as the detector's noise/non-fit rate, and calibrate both against
  the labelled auditor-bench corpus.
- **Time-to-first-check** — from the stable probe `run_id` to a ratified clause
  passing parse + check. Track time-to-first-model-test and
  time-to-first-implementation-verification as separate, stronger milestones.
- **Implementation-bug yield** — hypotheses answered `BUG`, with the number
  subsequently reproduced by a source-bound backend reported separately.
- **Implementation-verified conversion** — % of runs that reach at least one
  real-handler-backed green result; never merge this with model-tested results.
- **Spec growth** — clauses per spec over repeated probe→ratify cycles (does the
  loop keep pulling users deeper?).

---

## 9. Parking lot (things I deliberately left open)

- **Confidence calibration.** What false-hypothesis rate feels smart vs noisy?
  Needs a corpus tuning pass before default-on injection.
- **Payoff copy.** The per-hypothesis payoff line is the conversion surface and
  deserves real writing, not templated boilerplate.
- **`interface` overlap.** `interface --idl` stays, but if the hypothesizer
  learns CPI-integrity, is there a future where `interface` is also just a probe
  output? (Not now — flagged only.)

> ### ◻ Decision D6 — anything above you want promoted to a real decision, or
> any constraint I've missed (naming, backwards-compat, who the first user is)?
>
> **Your answer:**
> fine for now.
