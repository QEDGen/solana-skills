# Release v2.20.0 — Spec-mode trust restoration + native-runtime adoption + auditor productionization

v2.20 closes three independent feedback loops that converged on the
same release window:

1. **External rewards audit feedback** (issue tracker, 2026-05-XX) —
   88 of 106 Kani harnesses verified vacuously because `forall d : T,
   P(d)` lowered to a silent `true` stub. Spec mode shipped green
   while verifying nothing. **S1.1 closes this.**
2. **Phoenix pre-audit empirical study** (2026-05-17, in-tree at
   `audits/phoenix-v1/.qed/findings/preaudit-empirical-2026-05-17.md`)
   — auditor walked past a real data-structure dep correctness bug
   (sokoban RB-tree iterator unsoundness) because §3c trust-surface
   walk only triggered on crypto verbs; the bootstrap probe returned
   empty `handlers[]` for the Shank-style native dispatcher. **S2 +
   S3.5 close these.**
3. **GitHub issue #42** (saicharanpogul, 2026-05-02) — handler
   `effect { ... }` blocks couldn't express one-instruction-N-branches
   programs; users either split into N spec handlers (breaking 1:1
   drift detection) or modeled one branch (vacuous). **S1.2 closes
   this.**

Plus a productionization pass on the auditor skill — Phase 1/2/3
workflow, thinking-budget hook, post-first-finding TUI interview,
four new catalog rules.

## What's in

### Slice 1 — spec-mode trust restoration

#### S1.1 — `forall` quantifier harness lowering

`crates/qedgen/src/quantifier.rs` (new, ~480 LOC with tests) classifies
each `Property`'s quantifier shape:

```rust
enum Shape {
    NoQuantifier,
    SingleBinderForall { binder, binder_ty, span },
}
enum Reason {
    NestedQuantifier(span),
    UnboundedVec(ty),
    UnsupportedTypeKind(ty),
}
```

For `Shape::SingleBinderForall`, Kani and proptest emitters now bind
the symbol via `kani::any::<T>()` (or `any::<T>()` in proptest
strategies) and assert via a `<prop>_at(&s, <binder>)` predicate
sibling to the bare `<prop>(&s)` stub. The legacy silent `true` stub
is gone.

```rust
// Before (v2.19) — vacuous:
fn all_slots_valid(_s: &State) -> bool { true }   // QEDGEN_UNSUPPORTED_QUANTIFIER

// After (v2.20):
fn all_slots_valid_at(s: &State, v: u64) -> bool { v >= 0 }
#[kani::proof]
fn verify_update_preserves_all_slots_valid() {
    let mut s = State { slot: kani::any() };
    let v: u64 = kani::any();
    kani::assume(all_slots_valid_at(&s, v));
    if update(&mut s) {
        assert!(all_slots_valid_at(&s, v),
            "all_slots_valid must hold after update (forall v : U64)");
    }
}
```

For unsupported shapes (nested forall, exists, unbounded `Vec<T>`
binder), `qedgen check` emits **P5 `unsupported_quantifier_shape`**
with span info and a fix pointer. The legacy `unchecked_quantifier`
warning is suppressed when P5 fires (more precise message wins).

Touched: `quantifier.rs` (new), `check.rs` (+86), `kani.rs` (+83),
`proptest_gen.rs` (+71), `rust_codegen_util.rs` (+50),
`chumsky_adapter.rs` (+30), `lean_gen.rs` (+1), `main.rs` (+1).

Fixture: `examples/regressions/issue-rewards-feedback-1/{forall-distribution,nested-forall-unsupported}.qedspec`.

#### S1.2 — conditional effects (`match` in effect blocks)

`effect { ... }` blocks now admit a `match` form with literal-integer
patterns + wildcard:

```
handler collect_fees (caller : U8) (fee_type : U8) (amount : U64)
    : State -> State {
  effect {
    match fee_type {
      0 => fees_a_withdrawn +=! amount,
      1 => fees_b_withdrawn +=! amount,
      2 => fees_c_accumulated +=! amount,
      _ => fees_d_accumulated := 0,
    }
  }
}
```

Codegen lowers to a Rust `match` body in `tests/proptest.rs` and
`tests/kani.rs`; the fingerprint folds `effect_match_on=<scrutinee>` +
per-arm pattern/effect into the canonical token string so swapping
arms triggers drift detection under `qedgen check --frozen`.

If the spec lacks a wildcard arm, codegen auto-appends `_ => {}` so
generated Rust always compiles; the spec hash still reflects the
spec's actual arms.

**Not shipping in v2.20** (per PRD open question #1): `if/then/else`
parsing, identifier-pattern matching on enum-shaped state (the
latter reserved for v2.21 enum-State work). Lean lowering is also
deferred to v2.21 per S1.2 scope guard — Lean sees the union of
fields-that-might-change and emits per-field obligations, which
keeps existing Lean codegen compiling.

Touched: `ast.rs` (+60), `chumsky_parser.rs` (+145),
`chumsky_adapter.rs` (+85), `check.rs` (+31),
`rust_codegen_util.rs` (+193 incl. minor refactor),
`fingerprint.rs` (+16), `crucible_gen.rs` / `lean_gen.rs` /
`probe.rs` (+1 each test-fixture literals). Closes GitHub issue #42.

Fixture: `examples/regressions/issue-42-conditional/fee_router.qedspec`.

#### S1.3 — Pubkey state-field P6 lint

`check.rs` now emits **P6 `pubkey_state_field_unsupported`** when any
state declaration carries a `Pubkey`-typed field. The lint covers all
three places state-shaped Pubkey can land: account-type fields,
sum-type variant fields, and record-type fields. A belt-and-suspenders
assert in `rust_codegen_util::emit_state_struct` panics if a Pubkey
field reaches codegen despite the lint (P6 should have caught it).

**The recommended workaround in v2.20 is to move the value to a
handler parameter**, not to use a fixed-size byte array. The spec
grammar has no array type today — `[u8; 32]` doesn't parse — so the
naive replacement isn't yet expressible. See
`docs/limitations.md#pubkey-state-fields` for the v2.20 workaround
and the v2.21 roadmap (`Bytes32` spec type OR Option B Pubkey→`[u8;
32]` lowering in codegen).

Known: `examples/rust/escrow/escrow.qedspec` has 4 Pubkey identity
fields in `State.Open` that genuinely need to persist; the lint
correctly flags them but there's no v2.20-grammar way to migrate the
spec, so `test_complete_spec_clean` filters
`pubkey_state_field_unsupported` for this fixture with a v2.21
follow-up note.

Touched: `check.rs` (+217 incl. 3 tests),
`rust_codegen_util.rs` (+16 assert), `docs/limitations.md` (+58 new
section).

Fixture: `examples/regressions/issue-rewards-feedback-2/pubkey-field.qedspec`.

### Slice 2 — native-runtime adoption

#### S2.1 — Shank dispatcher detection

`crates/qedgen/src/shank_probe.rs` (new, ~750 LOC with tests) detects
the pre-Anchor Shank-style central-match dispatcher pattern in
`lib.rs`:

1. Function signature matches
   `fn process_instruction(_: &Pubkey, _: &[AccountInfo], _: &[u8]) -> ProgramResult`.
2. Body contains a top-level `match` over a value derived from
   `instruction_data` (`try_from` / `try_from_primitive!` heuristic).
3. Each arm is `<Enum>::<Variant> { … } | <Variant>` calling a
   `process_*` function.

Uses `syn::parse_file` + `syn::visit::Visit`. Anchor / Pinocchio /
Quasar paths untouched.

Probe-output schema extension:

```json
{
  "version": 2,
  "mode": "spec_less",
  "runtime": "native",
  "dispatcher_kind": "shank_central_match",
  "handlers": [
    {
      "name": "InitializeWidget",
      "source_file": "src/lib.rs",
      "enum_variant": "WidgetInstruction::InitializeWidget",
      "entry_fn": "process_initialize_widget",
      "line": 31
    }
  ]
}
```

Fixture: `examples/native-fixtures/shank-dispatcher/` (3-arm
dispatcher exercises positive path; 3 negative unit tests cover
Anchor-shape / no-match / wrong-source-ident).

#### S2.2 — per-handler `applicable_categories` narrowing

`crates/qedgen/src/handler_intent.rs` (new, ~390 LOC with tests)
classifies each Shank handler body and emits a narrowed per-handler
category list:

| Handler shape | `intent_tag` | Drops from global list |
|---|---|---|
| Authority comparison (`.key` vs stored authority field, `assert_*authority` helpers, name pattern `authority`/`admin`/`manager`) | `authority_gated` | `permissionless_state_writer`, `permissionless_create_account_dos` |
| Signer check without authority comparison (`.is_signer`, `Signer::try_from`) | `trader_gated` | (no current admin-only category) |
| Non-trivial body without signer/authority check | `permissionless` | `missing_signer` |
| Trivial body (no branches, single-expression) | (untagged) | (no narrowing — full global list) |

Touched: `handler_intent.rs` (new), `probe.rs` (BootstrapHandler
extension), `main.rs` (`narrow_shank_handler` mirror), fixture
handler bodies updated.

### Slice 3 — auditor productionization

#### S3.1-S3.4 — four new catalog rules

Added to `skills/qedgen-auditor/SKILL.md`:

- **`permissionless_create_account_dos` (MEDIUM)** — raw
  `system_instruction::create_account` against a deterministic PDA
  is grievable by 1-lamport pre-funding. Anchor `init` does the safe
  transfer+allocate+assign internally; the unsafe form is the
  open-coded `invoke_signed(create_account(...), ...)` shape.
- **`execution_order_state_before_check` (MEDIUM)** — handler
  mutates field X early then a later branch tests X for the
  pre-mutation value. Surfaced by Phoenix's no-deposit-mode FOK
  ordering bug.
- **`flag_branch_no_op` (MEDIUM)** — `match` arm distinguishes two
  variants but the body's primary effect is identical for both;
  only secondary bookkeeping differs. Surfaced by Phoenix's
  self-trade `DecrementTake` flag-branch.
- **`safe_wrapper_inner_unchecked_arithmetic`** (sub-rule of
  `arithmetic_overflow_wrapping`) — `saturating_*` /
  `checked_*` whose argument is itself a raw `*`/`+`/`-` chain.
  With `overflow-checks = true`, the inner expression panics
  before the wrapper sees a value.

#### S3.5 — §3c data-structure dep walk extension

`skills/qedgen-auditor/SKILL.md` §3c "When to run it" extended to
catch niche data-structure / algorithmic deps the program leans on
for state-machine correctness. New reference file
`skills/qedgen-auditor/references/data_structure_dep_invariants.md`
(~190 lines): 5-axis checklist (iteration soundness, ordering
invariant preservation, re-balancing correctness, capacity-edge
behavior, shared-mutable / interior-mutability soundness) with the
sokoban RB-tree `DoubleEndedIterator` cursor-crossing corpus.

#### S3.6 — thinking-budget hook + recommended-model section

`skills/qedgen-auditor/hooks/` (new directory, 3 files):

- `auditor-thinking-budget.sh` — POSIX `UserPromptSubmit` hook
  (jq-based, ~40 LOC). Idempotent, case-insensitive trigger-phrase
  detection (`/qedgen-auditor`, `audit my program`, `security audit`,
  `find bugs in X`, etc.); appends `ultrathink` to lift Claude Code's
  thinking budget to xhigh on Opus 4.6/4.7. Silent on non-triggers.
- `settings.snippet.json` — drop-in fragment for `~/.claude/settings.json`.
- `README.md` — manual install instructions.

SKILL.md ships a new top-of-file section: *"Recommended model +
reasoning budget — Claude Opus 4.7 with extended thinking (Claude
Code; this skill auto-injects `ultrathink` via the hook) or
GPT-5.5 in high-reasoning mode (Codex / Cursor — set the harness's
reasoning budget manually)."*

#### S3.7 — authority-side intent-drift skill-body note

New bullet in SKILL.md "Adversarial mindset" section pointing at
the Phoenix empirical study finding: hand audits implicitly model
unprivileged attackers; *"the authority is trusted"* dismisses
authority-side findings as out-of-scope. A documented invariant
the program fails to enforce against its own authority is still a
real finding.

#### S3.8 — audit workflow rewrite (Phase 1/2/3)

SKILL.md step 6 fully rewritten. The legacy file-driven scaffold-to-
spec interview becomes the harness fallback; the primary flow is:

- **Phase 1 — autonomous discovery.** Producer A (probes) and
  Producer B (read pass) run concurrently. No user prompts.
  Time-to-fire ordering: Mollusk → Miri → Crucible. Event-driven
  surface — the instant a MED+ repro fires, the auditor surfaces it
  immediately.
- **Phase 2 — post-first-finding interview.** Triggered automatically
  by the first MED+ surface (or by a dry Phase 1). Four
  `AskUserQuestion` batches present Phase 1's internal hypotheses
  (invariants / state machine / authority graph / threat model) as
  ratification candidates.
- **Phase 3 — refined second wave.** Producer A re-prioritizes
  probes against ratified invariants; Producer B deepens
  intent-drift / authority sweeps with ratified authority graph.

Two new reference runbooks:

- `references/workflow_walkthrough.md` (~280 lines) — timestamped
  end-to-end example showing parallel tool-call shapes and
  background choreography.
- `references/probe_orchestration.md` (~270 lines) — probe fan-out +
  background choreography + **probe → skeleton → Crucible
  auto-chain** that finally lets brownfield audits reach
  coverage-guided fuzzing (closes the gap that `qedgen probe`'s
  `--fuzz` mode required `--spec`, which brownfield audits don't
  have).

#### S3.9 — post-first-finding TUI interview

SKILL.md step 6 interview sub-section reframed for the
`AskUserQuestion`-driven flow described in S3.8. Cluster taxonomy
(SKILL.md §"Cluster taxonomy") reframed as Phase-2 fallback only —
surfaced as cluster cards for sites whose intent the four-question
ratification didn't already classify. Most sites collapse
automatically.

New reference: `references/interview_examples.md` (~380 lines) —
three worked transcripts (authority-gated vault / token-program-like
three-authority / lifecycle-heavy multi-state position) showing the
`AskUserQuestion` JSON shapes with realistic `preview` field excerpts
and Phase-3 effect notes.

## Eval status — S3.8 + S3.9 ship-if-ready

Per PRD-v2.20.md's release-gate split, S3.8 (workflow) and S3.9
(interview) ship in this release without a completed real-program
eval against the documented exit criteria:

- **S3.8 exit criterion** — *"Eval on at least one brownfield program
  where the workflow reaches Crucible via the auto-chain and Crucible
  surfaces ≥ 1 finding that pattern-match probes missed."* Status:
  **not yet validated**; implementation is in, eval is pending.
- **S3.9 exit criterion** — *"Eval on at least one brownfield program
  where the interview fires after a real first-finding surface, and
  Phase 3 surfaces ≥ 1 additional finding the user wouldn't have
  asked for in a pre-flight intake."* Status: **not yet validated**.

These are documented as user-visible features in SKILL.md and the
references; the workflow is what the auditor agent will follow on the
next brownfield audit. If real-program eval surfaces blocking issues,
v2.20.1 will follow with the corrections; otherwise the eval is
empirical confirmation that the design holds.

## Known limitations

- **`Pubkey` state-field workaround is grammar-blocked** — P6 lint
  fires correctly but the spec grammar has no fixed-size byte-array
  type in v2.20. The lint's recommended fix is "model as handler
  parameter"; the `[u8; 32]` workaround mentioned in some prior
  drafts isn't expressible until v2.21 ships either a `Bytes32` spec
  type or Option B (Pubkey → `[u8; 32]` codegen lowering).
- **`if/then/else` in effect blocks not parsed** — only `match` lands
  in v2.20 (PRD open question #1).
- **Lean lowering for conditional effects deferred** — Lean sees the
  union of fields-that-might-change and emits per-field obligations;
  no Lean-level `match` term. Tracks as v2.21 follow-on.
- **S2.2 entry-fn source resolution is dispatch-arm-local** — each
  handler's `file` / `line` carries the dispatch-arm location, not
  the callee `process_*` definition. The auditor reads handler bodies
  after discovery; tracking source resolution for the v2.21
  `applicable_categories` deepening.
- **Spec-less Crucible still gated** — `qedgen probe --fuzz` requires
  `--spec` at the CLI level. v2.20 ships the agent-side auto-chain
  workaround (probe → skeleton → `--fuzz`); lifting the CLI gate
  itself is v2.21 (option (b) or (c) per `project_probe_crucible_gap`).

## Files changed

5 commits on `feat/v2.20`:

- `04b3505` — feat(auditor): v2.20 Slice 3 — +5 rules, hook, workflow refs
- `7897fe8` — feat(auditor+probe): v2.20 — SKILL.md workflow rewrite + Shank probe
- `4e3205e` — feat(codegen+probe): v2.20 — forall harness + per-handler categories
- `1e05183` — feat(codegen+check): v2.20 — conditional effects + Pubkey P6 lint
- (release commit, this doc + version bump) — chore(release): v2.20.0

~3500 net new lines across `crates/qedgen/src/`,
`skills/qedgen-auditor/`, `examples/regressions/`,
`examples/native-fixtures/`, `docs/limitations.md`,
`docs/prds/RELEASE-v2.20.md`.

## CI / pre-release gates

- `cargo fmt --check`: clean
- `cargo clippy --release --all-targets -- -D warnings`: clean
- `cargo test --release`: 677 tests pass (vs 624 on `main`; +53 new)
- `bash scripts/check-readme-drift.sh`: clean
- `bash scripts/check-lake-build.sh`: 8/10 OK, 2 skipped
  (`multisig` + the perp-dex example need `lake update` for cold-checkout
  state — pre-existing, not v2.20-introduced)
- Zero unintended `sorry` (verified: all hits are legitimate v2.8 G3
  ensures-as-axiom CPI theorems, comments, or string templates)
- `qedgen check --frozen --spec examples/rust/escrow-split/`:
  lock-file integrity verified clean; pre-existing P1 error on the
  `conservation` invariant (vacuous, missing `expr` body) flagged
  for v2.20.1 follow-up

## Migration notes

- **No breaking changes.** v2.20 is purely additive per
  `feedback_minor_release_completeness.md` (minors land features;
  v3.0 is reserved for breaking changes).
- **Spec authors using `forall`**: codegen output shape changes —
  the harness now binds `<binder>: T = kani::any()` and asserts via
  `<prop>_at`. Existing `forall` properties that previously
  verified vacuously may now fail on real counterexamples. This is
  the intended outcome (the v2.19 silent stub was the bug).
- **Spec authors with conditional handler logic**: switch from
  N-handler split or single-arm modeling to the new `match`
  effect-block form. Closes GitHub #42 cleanly.
- **Spec authors with Pubkey state fields**: P6 lint fires. Move
  the value to a handler parameter (see
  `docs/limitations.md#pubkey-state-fields`). If the value
  genuinely must persist in state, hold for v2.21 grammar
  additions.
- **Auditor users**: the legacy scaffold-to-spec file interview
  remains the fallback path on harnesses without
  `AskUserQuestion`-equivalent. Claude Code gets the new TUI flow
  automatically when the skill is invoked.

## Acknowledgements

- External rewards audit feedback (S1.1, S1.3 motivation).
- @saicharanpogul for issue #42 (S1.2 motivation).
- Phoenix empirical study (S2.1, S3.1-S3.5, S3.7 motivation;
  `audits/phoenix-v1/.qed/findings/preaudit-empirical-2026-05-17.md`).
- PR #45 (@0xharp, tendr.bid integration) — six complementary
  proptest codegen fixes for brownfield specs (Nat/Int coercion,
  state_binder, type-aware seed-state init, mul_div inlining,
  tuple-arity chunking, conditional recursion_limit). **Not closed
  by v2.20** — addresses orthogonal compile-shape issues. To be
  rebased onto v2.20 and merged separately.
- `project_auditor_best_models.md` — Opus 4.7 xhigh / GPT-5.5
  framing that motivated S3.6's thinking-budget hook.
