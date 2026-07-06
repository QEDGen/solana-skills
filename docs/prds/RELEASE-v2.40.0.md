# QEDGen v2.40.0 — typed-expression MIR + whole-transition discharge

**Status:** shipped (PR #152 + #153, tracker #150). **Theme:** the #151
typed-expression MIR (all five slices — the structural fix for the
#139/#143–#146/B12 bug class), the qedsvm v0.9.0 uptake with the #40
whole-transition obligation layer, the qedbridge `.rejects` boundary
discharged sorry-free, and #148's Lean auto bound-guards.

## What shipped

### 1. Typed-expression MIR (#151, Slices 0–4)

Expressions no longer render to per-target strings at parse time. The
adapter builds one typed, **name-resolved** tree per expression
(`mir::ExprTree`: closed enum, no `_` arms — the `Stmt` discipline extended
to expressions) with `BindingKind`-resolved paths (state field / ghost /
param / const / let / account / abstract / expression binder) and
`Ty`-annotated leaves; per-backend renderers consume it at emission time:

- **Rust** — `render_rust(tree, RustCx { binder: S | SelfAcct | PrePost |
  PreLocal, arith: Native | Checked | Widened | Wrapping, pod, acct_env,
  acct_key })` collapses the six pre-rendered string forms and the v2.39
  `*_math`/`effects_rust` seams into renderer parameters. The `Wrapping`
  mode reproduces the legacy `wrap_arithmetic`-over-widened proptest guard
  composite exactly.
- **Lean** — `render_lean(tree, LeanCx { S | SPrime, Brackets |
  Application })` with the asymmetric Nat→Int coercion, structural
  match-arm `_` binders, and application-style Map subscripts; the
  `effect_value_to_lean_mir` front-prefix heuristics are demoted to
  tree-less fallbacks.
- **Scaffold** — `mechanize_effect`'s char-whitelist "is simple" test is
  structural (`tree_bare_rhs`); guards render through `Binder::SelfAcct`
  with `AcctKeyStyle` for the Anchor/Quasar key projections.
- **Parity gates** — corpus tests render every carried tree in the bundled
  fixtures across all modes and compare against the legacy strings
  (syn-verified structural equivalence on the Rust side).

Slice 4 deleted the **parallel-array drift class**: `ParsedHandler` /
`ParsedEffectArm`'s four index-aligned effect arrays are one
`Vec<ParsedEffect>`; the `get(i)` alignment code in `lower_effects` is gone.

**Deliberate fixes shipped under the port** (each snapshot-reviewed):

- Indexed state paths in effect RHS were emitted unbound
  (`accounts[i].capital`) — now `s.accounts[(i) as usize].capital`.
- Kani conformance expected values only rebound *bare* field names to the
  `pre_<field>` snapshot locals; compound/indexed RHS leaked unbound or
  post-state reads (the perp-dex example's `verify_close_account_effect_V` never
  compiled). Expected values render under `Binder::PreLocal`.
- Negated guard conjuncts get the same wrapping policy as positive ones
  (the legacy string pass never descended into `!(…)`).
- Redundant source parens are normalized (grouping is structural).

**v3.0 tail (deliberate, documented in-source and on #151):** the six
`Expr` string fields survive — `Expr.lean` feeds the Lean requires/property
emission and the `cpi_substitute` lane; `resolve_value` /
`translate_guard_to_rust` / `wrap_arithmetic` / `split_top_level_and`
remain as production-dead fallbacks reachable only from hand-built test
fixtures; `aborts_if` is a dead legacy surface slated for deletion.

### 2. qedsvm v0.9.0 + whole-transition discharge (#150 items 1–2, #124)

- Pin bump v0.8.0 → v0.9.0 (`lean_solana` builds green; no consumer-side
  adapter changes needed).
- `qedgen discharge --transition` drives qedlift's #40 whole-transition
  mode over the same name-level descriptor seam: every program path is
  lifted from its discovered `<stem>_<path>.pcs` trace into an
  `AsmRefinesTransitionPath` (success: exit code + tracked fields
  pre→post) or `AsmRefinesTransitionFault` (typed abort/panic/OOB, no
  post) corollary plus the one bundle theorem. Verdict = sorry-free
  `<StemPascal>Transition.lean` in `--out-dir`.
- **The `.rejects` boundary is closed.** The generated qedbridge
  `.rejects` theorem was a free-`progAt` statement (only sorry-true); it
  now threads the fault-path discharge as hypotheses — the A2b treatment —
  and closes through the new proven `BridgeAdapter.faults_of_transitionFault`
  + `toSentinel_ne_zero`. `BridgeHarness` elaborates with exactly one
  `sorry` (decode_encode, qedsvm#48-gated); `#print axioms` shows
  `.refines` and `.rejects` on standard axioms only.
- #124 close-out recorded in-source: a richer (guard-cascade/multi-field/
  per-abort) descriptor is a schema v3 the current consumer refuses
  fail-closed (`DESCRIPTOR_SCHEMA_MAX = 2`; transition paths come from
  traces, not descriptor guards) — it lands in lockstep with a qedsvm-side
  bump. `read_cached_elf` explicitly re-deferred to the aggregate
  trust-report wiring. `runMir` stays parked.

### 3. Lean auto bound-guards for bare-arithmetic effect RHS (#148)

The harness lane renders effect values checked (v2.39/#146) while the Lean
model computed Nat monus / total division — the models diverged on the
rejection path. The flat-state transition guard now gains tree-derived
conjuncts: cumulative `rhs ≤ lhs` per Nat subtraction, `≠ 0` per division
(literal divisors skipped), and final-value MAX bounds on bounded targets —
emitted only for unconditionally-evaluated positions (arithmetic inside
`if`/`match` arms is checked in Rust only when taken). ADT and indexed
lanes are documented out of scope pending a binder-aware render context.

### 4. Examples re-pinned and rebuilt (7/10 full-fat)

All bundled examples re-resolved to qedsvm v0.9.0 (they had lagged at
v0.4.0) with full `lake build` validation. Green: all six Rust examples
(escrow, escrow-split, lending, multisig, the perp-dex example,
bundled-stdlib-demo — the two embedded `lean_solana` copies synced to the
v0.9.0 sources) and `sbpf/counter`. The perp-dex example's generated harnesses were
regenerated to match the shipped codegen (PR #153).

**Known debt (pre-existing, now surfaced):** `sbpf/{transfer, slippage,
tree}`'s hand-migrated `Spec.lean` proofs fail against qedsvm v0.9.0 —
their lifted programs hit the v0.9 fail-closed ISA model
(`ERR_UNSUPPORTED_INSTRUCTION` where v0.4.0 executed; `tree` needs a full
re-proof). These three had not built full-fat since the un-vendoring
(cold caches masked it — the v2.39.0 lesson cut both ways). Same posture
as the the-vendored-sbpf-example precedent: proof migration is tracked in #154, not a ship gate.
The support library itself compiles green in all three projects; only the
example proof bodies lag.

## Lessons

- The example-codegen-drift CI gate is the one surface local `cargo test`
  doesn't cover: deliberate codegen fixes must be paired with
  `check --regen-drift --write` in the same PR (#152 landed red; #153
  fixed forward within the hour).
- qedlift's transition mode consumes the **unchanged v1/v2 descriptor** —
  paths, guards, and abort codes come from `.pcs` traces. Producer-side
  descriptor growth without a consumer-side `DESCRIPTOR_SCHEMA_MAX` bump
  is a fail-closed refusal, by design.
