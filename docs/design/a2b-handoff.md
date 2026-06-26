# A2b Handoff — resume the qedsvm discharge bridge

Session handoff for continuing **Slice A / A2b** of the qedsvm discharge seam.
Full design: [`qedsvm-discharge.md`](qedsvm-discharge.md) (esp. §14, §19). This doc
is the "start here" for a fresh session. Verify file:line refs before acting.

## TL;DR

The discharge pipeline turns qedgen's sBPF-bridge `sorry` into a proof against the
pinned program bytes. **A2b's success path is now COMPLETE.** A discharged field
update (qedlift's `AsmRefinesFieldUpdate`) halts the whole run with `exitCode =
some 0` and the account memory encoding the post-state (the adapter); the Bridge
elaborator emits that provable `.refines` shape; and the post `codecCoarse →
encodeState` leg now discharges via qedsvm#48's `CodecRead.lean` family (shipped
in **qedsvm v0.8.0**, pinned). The generated `Vault.Bridge.increment.refines` is
**sorry-free** (`#print axioms` = `propext / Classical.choice / Quot.sound`, no
`sorryAx`). What remains is only the **boundary** (out of A2b scope): `.rejects`
(abort path, gated on qedsvm#40) and `decode_encode` (the round-trip lemma) stay
`sorry`.

## State of the world

- **Branch:** `feat/a2b-bridge-adapter` (8 commits ahead of `main @ 2124819`). All
  WIP lives here; nothing A2b is on `main` yet.
- **Merged on `main` (the seam so far):** descriptor producer (#127), A1 ELF cache
  (#130, `verify/upstream_check.rs`), A2a discharge persist (#132,
  `descriptor.rs::run_discharge --out-dir`), qedsvm pin **v0.6.0 → v0.7.0** (#129/#133).
  qedsvm **v0.7.0 (`c38c769`)** ships `qedlift --descriptor` (the descriptor seam:
  v1 `add_const`, v2 `add_param`, arbitrary literals). Real `vault.increment`
  discharge validated end-to-end (sorry-free `AsmRefinesFieldUpdate` + `ensures`).

## What's PROVEN on the branch (`lean_solana/QEDGen/Solana/BridgeAdapter.lean`)

Both sorry-free + standard-axiom clean (`propext`/`Classical.choice`/`Quot.sound`):

1. `halts_zero_of_block_exit` — the **execution bridge** (was the flagged primary
   risk). A `cuTripleWithinMem` over a call-free block `entry → exitPc` (post `Q`
   pins `r0 = 0`) extends to a whole-run halt `exitCode = some 0` with `Q`
   surviving, once the `.exit` at `exitPc` runs. Key facts it exploits:
   `cuTripleWithinMem` is *defined* over `executeFn` (`CPSSpec.lean:379`); `step
   .exit` (empty call stack) = `{ s with exitCode := some (regs.get .r0) }`;
   `holdsFor`/`CompatibleWith` ignore `exitCode`/`cuConsumed`.
2. `halts_zero_of_fieldUpdate` — wraps qedlift's actual output type
   `AsmRefinesFieldUpdate` (= the cuTriple with `P = setupPre ** codecCoarse base
   preFields`, `Q = setupPost ** codecCoarse base postFields`) onto (1).

## What's VALIDATED (the next-step template) — `lean_solana/RefinesShape.lean`

`RefinesShape.increment_refines` is a hand-written vault analogue of the corrected
`.refines` theorem. It **elaborates and the proof closes** except ONE documented
`sorry` (the post `codecCoarse → encodeState` leg, = qedsvm#48). It proves that
the corrected statement shape is provable via the adapter. Build it standalone:
`cd lean_solana && lake env lean RefinesShape.lean` (expect only "declaration uses
sorry"). Not in the lib roots.

## Done: the corrected `.refines` is now what the elaborator emits (finding 1)

The generator (`Bridge.lean`) was rewritten to emit the
`RefinesShape.increment_refines` shape instead of the old free-`progAt` `:= sorry`:
it now takes params `(cr) (rr) (nSteps nCu exitPc) (setupPre setupPost)` and hyps
`h_prog`, `h_exit`, `h_asm : AsmRefinesFieldUpdate …`, `h_pre`, `h_cs`, `h_r0`,
`h_fuel`, `h_bud`, `h_rr`; builds the `preFields`/`postFields` `FieldVal` lists from
the layout (`U64 → .u64`, `U8 → .byte`, `Pubkey → .pubkey`, plus a `.byte
(encodeStatus …)` for a lifecycle status byte); and the body discharges via
`BridgeAdapter.halts_zero_of_fieldUpdate`, leaving exactly the one post-leg `sorry`
(qedsvm#48). The insn/`entry:` path uses `initState2` with `entry = ENTRY`; the
no-insn path uses `initState` with `entry = 0`. `Bridge.lean` now `import`s
`QEDGen.Solana.BridgeAdapter` and the generated namespace `open`s
`SVM.Solana.Abstract` + `QEDGen.Solana.BridgeAdapter`, so any importer of `Bridge`
(e.g. the harness) sees the adapter.

> **Validated** via `lean_solana/BridgeHarness.lean` (the first-ever `qedbridge`
> invocation): `cd lean_solana && lake env lean BridgeHarness.lean` →
> 3 `sorry` warnings (`decode_encode`, `increment.refines` post-leg,
> `increment.rejects`), no errors, and `#check @Vault.Bridge.increment.refines`
> now shows the corrected signature (`h_prog : cr.SatisfiedBy progAt`, `h_asm`,
> `h_pre`, …). Both the no-insn (`initState`) and insn+status+param (`initState2`)
> paths were checked.
> Gotcha: `lean_solana` is **Mathlib-free** — no `set`/Mathlib tactics. The
> bridge's `Pubkey` resolves to `QEDGen.Solana.Pubkey` (= `SVM.Pubkey.Pubkey`);
> `State` inside the `<Spec>.Bridge` namespace resolves to the abstract
> `<Spec>.State`, so the adapter's `State` is written fully-qualified
> (`SVM.SBPF.State`).

## DONE: the post leg discharges via qedsvm#48 (v0.8.0)

qedsvm#48 shipped as `SVM/SBPF/CodecRead.lean` in **qedsvm v0.8.0** (pin bumped
`v0.7.0 → v0.8.0` in `lean_solana/lakefile.lean` + manifest). The forward family:
`holdsFor_sepConj_left/right` (peel the `**` frame), `holdsFor_codecCoarse_field`
(extract a field's coarse atom from the codec list), and the per-atom bridges
`readU64_of_holdsFor_memU64Is` / `readU8_of_holdsFor_memByteIs` /
`pubkeyAt_of_holdsFor_pubkeyIs`. The post leg
(`(setupPost ** codecCoarse base postFields).holdsFor result ⟹ encodeState s'
base result.mem`) is: `holdsFor_sepConj_right` → per field `holdsFor_codecCoarse_field`
+ `simp only [FieldVal.coarse]` + the matching bridge.

**Wrinkle (load-bearing):** the forward bridges NORMALIZE — `readU64 = v % 2^64`,
`readU8 = v % 256`. So each field needs a `< width` bound (`s'.f < 2^64` / `< 256`,
pubkey limbs `< 2^64`) to land `encodeState`'s raw read. These are the state's
`Valid s'` invariant; the generator emits them as `hb_<field>` hyps and the proof
closes each via `Nat.mod_eq_of_lt`. Validated in `RefinesShape.increment_refines`
(sorry-free) then ported to the `Bridge.lean` generator; `BridgeHarness.lean`'s
`#print axioms Vault.Bridge.increment.refines` confirms no `sorryAx`.

## Recommended first action

A2b's success path is complete and merge-ready (modulo the usual review). **Next**
is the boundary, both out of current A2b scope: `.rejects` (the abort path) unparks
when **qedsvm#40** lands (the lift's per-abort arms); `decode_encode` is the codec
round-trip lemma (provable now via the same `CodecRead.lean` reverse family —
`holdsFor_codecCoarse_of_reads` — if desired). Otherwise: open the `feat/a2b-bridge-adapter`
PR.

## Pointers

- Adapter: `lean_solana/QEDGen/Solana/BridgeAdapter.lean`; template: `…/RefinesShape.lean`.
- Bridge elaborator: `lean_solana/QEDGen/Solana/Bridge.lean` (`.refines` gen
  `:307` `theorem {qOp}.refines`, `FieldVal` lists `:264` `mkFieldList`,
  encodeState gen `:223`, syntax `:40`, parse `:87`).
- qedsvm (`.lake/packages/qedsvm/`): `SVM/SBPF/CPSSpec.lean` (`cuTripleWithinMem`),
  `SVM/SBPF/Execute.lean` (`executeFn`/`step`/`initState`), `SVM/SBPF/SepLogic.lean`
  (`holdsFor`/`CompatibleWith`/`memU64Is`), `SVM/SBPF/AccountCodec.lean`
  (`codecCoarse`/`FieldVal`/`account_agg`), `SVM/Solana/Abstract/Refinement.lean`
  (`AsmRefinesFieldUpdate`).
- Persisted real discharge (reference output): rebuild via `qedgen discharge --spec
  crates/qedgen/tests/fixtures/descriptor/vault.qedspec --handler increment
  --account vault --so <qedsvm>/qedsvm-rs/tests/fixtures/vault.so --idl
  <…>/vault.codama.json --qedlift <built v0.7.0 qedlift> --out-dir <dir>`
  (qedlift built with `cargo build --features qedrecover --bin qedlift`, needs the
  package's `.lake` Lean artifacts present first).
- Boundary: `.refines` success path only; `.rejects`/abort stay `sorry` (qedsvm#40).
