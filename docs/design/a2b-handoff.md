# A2b Handoff — resume the qedsvm discharge bridge

Session handoff for continuing **Slice A / A2b** of the qedsvm discharge seam.
Full design: [`qedsvm-discharge.md`](qedsvm-discharge.md) (esp. §14, §19). This doc
is the "start here" for a fresh session. Verify file:line refs before acting.

## TL;DR

The discharge pipeline turns qedgen's sBPF-bridge `sorry` into a proof against the
pinned program bytes. **The hard part is done and proven**: a discharged field
update (qedlift's `AsmRefinesFieldUpdate`) is shown to halt the whole run with
`exitCode = some 0` and the account memory encoding the post-state. What remains
is **assembly**: port a validated `.refines` statement into the Bridge elaborator,
and prove a family of byte-level codec lemmas (filed upstream as **qedsvm#48**).

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

## Two remaining work items

### 1. Port the corrected `.refines` into the Bridge elaborator (finding 1)

The **currently generated** `.refines` (`Bridge.lean:269`) is **not provable**: it
quantifies over a free `progAt` with no `cr.SatisfiedBy progAt` hypothesis, so it
asserts refinement for *any* program. Rewrite the generator to emit the
`RefinesShape.increment_refines` shape: add params `(cr) (rr) (nSteps nCu exitPc)
(setupPre setupPost)` and hyps `h_prog`, `h_exit`, `h_asm : AsmRefinesFieldUpdate
…`, `h_pre`, `h_cs`, `h_r0`, `h_fuel`, `h_bud`, `h_rr`; build the `preFields` /
`postFields` `FieldVal` lists from the layout (`U64 → .u64`, `U8 → .byte`,
`Pubkey → .pubkey`, value from `s.field` / `s'.field`); body = the adapter
application (copy from `RefinesShape`), leaving the post-leg `sorry` for #48.

> **Test harness now exists:** `lean_solana/BridgeHarness.lean` — the first-ever
> `qedbridge` invocation (`Vault` over {owner: Pubkey, total: u64, bump: u8} +
> `increment`). Build it standalone: `cd lean_solana && lake env lean
> BridgeHarness.lean` (3 expected `sorry` warnings = generated bodies, no errors).
> Its `#check @Vault.Bridge.increment.refines` shows the current **bad** signature
> (free `progAt`, no `cr.SatisfiedBy`). Use it to validate the port: after porting,
> that signature should gain the `h_prog`/`h_exit`/`h_asm`/… hyps and the body
> should close via the adapter (modulo #48).
> Gotcha: `lean_solana` is **Mathlib-free** — no `set`/Mathlib tactics. Also the
> bridge's `Pubkey` resolves to `QEDGen.Solana.Pubkey` (= `SVM.Pubkey.Pubkey`).

### 2. qedsvm#48 — the `codecCoarse ↔ encodeState` byte-level legs

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

The `qedbridge` test harness is built (`BridgeHarness.lean`). **Next: port the
`.refines` generator** (`Bridge.lean:268-275`) to emit the `RefinesShape` shape,
re-running `lake env lean BridgeHarness.lean` after each change until
`@Vault.Bridge.increment.refines` matches the corrected signature and its body
closes via `BridgeAdapter.halts_zero_of_fieldUpdate` (leaving only the #48 post
leg). Build the `FieldVal` lists from the layout (`U64 → .u64`, `U8 → .byte`,
`Pubkey → .pubkey`).

## Pointers

- Adapter: `lean_solana/QEDGen/Solana/BridgeAdapter.lean`; template: `…/RefinesShape.lean`.
- Bridge elaborator: `lean_solana/QEDGen/Solana/Bridge.lean` (`.refines` gen `:268-275`,
  encodeState gen `:192-211`, syntax `:27-48`, parse `:76-151`).
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
