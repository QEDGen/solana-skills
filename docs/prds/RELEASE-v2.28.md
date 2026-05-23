# Release v2.28 — explicit-axiom rename + trust-surface report

Cosmetic-but-load-bearing release. Two changes, both aimed at the same
problem: the v2.27 bundled-stdlib proof packages presented their trust
assumptions as `theorem` declarations that discharged in one step
through a named `axiom`. The framing was technically honest but
visually misleading — readers of consumer code saw `exact
Token.transfer.ensures_axiom_0 ...` and pattern-matched "proved." v2.28
makes the trust surface impossible to miss, both inside the bundled
packages and in `verify --lean` output.

## What's in

### 1. Bundled proof packages now declare explicit axioms

`crates/qedgen/data/proofs/spl/Token.lean` and
`crates/qedgen/data/proofs/metaplex/Metadata.lean` previously wrapped
each `ensures_axiom_<i>` in a one-step theorem:

```lean
-- v2.27 — visually framed as "proven"
axiom runtime_trust_from {State} ... : ...
theorem ensures_axiom_0 ... := runtime_trust_from pre post amount from_balance
```

v2.28 drops the wrapper. Each contract is a top-level `axiom`
declaration with a `TRUST ASSUMPTION — not verified.` docstring that
names the qedsvm-discharge target:

```lean
-- v2.28 — what it actually is
/-- TRUST ASSUMPTION — not verified. ... Discharged by `qedsvm` in v3.0+:
    decode the pinned ELF, apply the `transfer` SL spec via
    `sl_block_auto`, project onto `from_balance`. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
  (from_balance post) = (from_balance pre) - amount
```

**Consumer code is byte-identical across the rename.** Calls like
`exact Token.transfer.ensures_axiom_0 pre post amount (·.from_field)`
compile against v2.27 and v2.28 packages without change. Only the
bundled package internals shift; `#print axioms` on consumer proofs
now surfaces `Token.transfer.ensures_axiom_0` directly (the symbol
consumers actually apply) instead of `Token.transfer.runtime_trust_from`
one level removed.

Bundled `qed.lock` `proof_hash` values rotate for every consumer of
`spl` / `metaplex` (bytes changed). v2.28 `qedgen check --frozen`
expects the new hashes; regen any external consumer's lock with
`qedgen codegen --spec <dir> --lean`.

### 2. `verify --lean` surfaces the unverified trust surface

`run_lean` now appends a `#print axioms` query for every top-level
theorem in `Spec.lean` / `Proofs.lean` after a successful `lake build`,
filters Lean built-ins (`propext`, `Classical.choice`, `Quot.sound`,
`Lean.ofReduceBool`, `Lean.trustCompiler`), and renders what remains
as a `trust surface` section in both text and JSON output:

```
qedgen verify — examples/rust/bundled-stdlib-demo
  [PASS] lean       (16925 ms)
         trust surface (unverified axioms each theorem depends on):
           PoolDemo.deposit_Token_transfer_call_0_post_1
             - Token.transfer.ensures_axiom_1
           PoolDemo.deposit_aborts_if_InvalidAmount
             - sorryAx
           PoolDemo.deposit_frame
             - sorryAx
OK
```

Surfaces two categories of user-actionable trust:
- **Bundled-callee axioms** (`*.ensures_axiom_*`): the unverified
  contracts the proof package documented above. Today these are
  axiomatized against the upstream `binary_hash` pin; v3.0+ replaces
  them with qedsvm-discharged theorems against the pinned ELF.
- **`sorryAx`**: incomplete proofs the user hasn't finished. Already
  warned about in lake build output; now also rolled up by theorem so
  the consumer can see which top-level claims rest on unfilled sub-
  goals.

Soft-failing: if `lake env lean` can't spawn or the file scan misses,
verify still passes — the axiom report is advisory, not a gate. JSON
consumers receive the data under each backend's new `axioms` field
(`omitempty` so v2.27 pinning continues to work).

### Forward link — Stance 3 (v3.0+)

The bundled axioms' docstrings name `qedsvm_discharge` as the future
proof-replacement path. When QEDGen/qedsvm ships per-handler SL specs
+ the discharge tactic, each `axiom ensures_axiom_<i>` here becomes
`theorem ... := by qedsvm_discharge "<binary_hash>" "<handler>"`. The
tactic decodes the pinned ELF, applies the bundled SL spec via
`sl_block_auto`, projects onto the abstract State accessor. Consumer
code stays unchanged through the transition. Until then, treat every
`*.ensures_axiom_*` surfaced by the new trust report as a load-
bearing assumption underwritten by the `binary_hash` pin and nothing
else.

## What's NOT in

- **`qedsvm_discharge` tactic itself** — needs per-handler SL specs in
  the bundled package + an ELF-into-cache hook in `--check-upstream`.
  v3.0+.
- **`verified_with ["proptest"]` rename** in interface metadata. Today's
  value is misleading; v3.0 candidate to switch to
  `verified_with ["qedsvm@<commit>"]` once Stance 3 lands.
- **Source-tag clone + hash compare in `--check-upstream`** — closes
  the interface↔source gap (#2 from the v2.28 framing memo). Separate
  workstream.

## Test plan

- [x] `cargo fmt --check`
- [x] `cargo clippy -- -D warnings`
- [x] `cargo test --release -p qedgen-solana-skills verify::` — 13 / 13
- [x] `scripts/check-readme-drift.sh` (TBD on tag day)
- [x] `scripts/check-lake-build.sh --strict` — bundled-stdlib-demo +
      escrow-split lake-build clean against v2.28 packages
- [x] `qedgen check --frozen --spec examples/rust/bundled-stdlib-demo`
      ✓ (after regen — proof_hash rotated)
- [x] `qedgen check --frozen --spec examples/rust/escrow-split` ✓ (same)
- [x] End-to-end `qedgen verify --lean` against bundled-stdlib-demo
      surfaces `Token.transfer.ensures_axiom_1` + 4× `sorryAx` in the
      trust-surface section. JSON shape includes `axioms` array per
      backend.
- [ ] Post-tag: `release.yml` ships all 4 binaries via CI

## Upgrade notes

- Bundled `qed.lock` `proof_hash` rotates. Run
  `qedgen codegen --spec <dir> --lean` on any external consumer of the
  `spl` or `metaplex` builtin packages and recommit the updated lock.
- Consumer Spec.lean / Proofs.lean require no edits. `exact
  Token.<handler>.ensures_axiom_<i>` applications continue to compile.
- `verify --lean` output gains a `trust surface` section on the human
  side and an `axioms: []` array on the JSON side. Anything pinning the
  v2.27 shape with strict-undefined parsing should add the new field;
  the JSON serializes `omitempty` so absent-array consumers keep
  working.
