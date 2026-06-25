# A2b Handoff — resume the qedsvm discharge bridge

Session handoff for continuing **Slice A / A2b** of the qedsvm discharge seam.
Full design: [`qedsvm-discharge.md`](qedsvm-discharge.md) (esp. §14, §19). This doc
is the "start here" for a fresh session. Verify file:line refs before acting.

## TL;DR

The discharge pipeline turns qedgen's sBPF-bridge `sorry` into a proof against the
pinned program bytes. **The hard part is done and proven**: a discharged field
update (qedlift's `AsmRefinesFieldUpdate`) is shown to halt the whole run with
`exitCode = some 0` and the account memory encoding the post-state. The Bridge
elaborator has now been **ported** to emit that provable `.refines` shape (it
discharges via the adapter; see "Done" below). The only remaining work is a family
of byte-level codec lemmas (filed upstream as **qedsvm#48**).

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

## One remaining work item

### qedsvm#48 — the `codecCoarse ↔ encodeState` byte-level legs

The remaining `sorry`(s), both pre and post. The Bridge's `encodeState` is a flat
conjunction `readU64 mem (addr+off) = s.field ∧ …`; qedlift's codec is `codecCoarse
base fields` (recursing to `fv.coarse (base+off) ** …`, `.u64 v = memU64Is a v =
fun h => h = singletonMemU64 a v`). Need `(memU64Is a v).holdsFor s ↔ readU64
s.mem a = v` (+ `memByteIs`/`pubkeyIs` analogues) and a `holdsFor_codecCoarse`
corollary over the `**`-composition. **No ready-made qedsvm lemma** — filed as
[qedsvm#48](https://github.com/QEDGen/qedsvm/issues/48); belongs upstream next to
`account_agg`. Until it lands, prove the needed direction locally in
`BridgeAdapter.lean` (or hand the stated lemmas to Leanstral — the SL byte-decode
grind is the designated escalation).

## Recommended first action

The elaborator port is done and validated (`BridgeHarness.lean`). **Next: close
qedsvm#48** — the `codecCoarse ↔ encodeState` read-back family. The post leg is the
single remaining `sorry` in both `RefinesShape.increment_refines` and the generated
`<op>.refines`. Prove `(memU64Is a v).holdsFor s ↔ readU64 s.mem a = v` (+ byte /
pubkey analogues) and a `holdsFor_codecCoarse` corollary over the `**`-composition,
land them upstream next to `account_agg` (or locally in `BridgeAdapter.lean` until
they land), then replace the post-leg `sorry` in the generator with the read-back
application. The SL byte-decode grind is the designated Leanstral escalation.

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
