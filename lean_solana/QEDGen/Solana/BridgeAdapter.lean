/-
  A2b-1 — the Bridge↔qedlift execution adapter.

  qedlift emits, per handler, an `AsmRefinesFieldUpdate` (which unfolds to a
  `cuTripleWithinMem` over `executeFn`) covering the handler block `entry → exit`
  — i.e. it stops AT the `.exit` instruction with `exitCode = none`. The
  `qedbridge` `.refines` theorem (`Bridge.lean`) instead asserts the whole run
  halts: `(executeFn … FUEL).exitCode = some 0 ∧ encodeState s' … result.mem`.

  This module bridges the gap. The only delta is the epilogue, and it is exactly
  ONE instruction: run the `.exit` at `pc = exit` (with `r0 = 0`, carried in the
  triple's post `Q`) to halt with `exitCode = some 0`; `.exit` (empty call stack)
  touches neither memory nor the SL-relevant registers, and `holdsFor` ignores
  `exitCode`/`cuConsumed` (it is a predicate over `PartialState` =
  regs/mem/pc/callStack), so the triple's post `Q` survives to the halted state.

  See docs/design/qedsvm-discharge.md §19.

  STATUS: contract stated; the body is a bounded SL/execution grind (emp-framing,
  callStack invariance, `holdsFor`-under-exitCode, r0 extraction) tracked for a
  focused fill (Leanstral candidate per the escalation ladder).
-/

import SVM.SBPF
import SVM.SBPF.CPSSpec

namespace QEDGen.Solana.BridgeAdapter

open SVM.SBPF

/-- Execution bridge (A2b-1). A CU-triple covering a call-free handler block
    `entry → exitPc` (post `Q` pins `r0 = 0`) extends to a whole-run halt with
    `exitCode = some 0` once the `.exit` at `exitPc` runs; the account memory at
    halt is exactly the triple's post `Q`.

    The `h_q_r0` / `h_cs` hypotheses are discharged per-instance by the bridge
    wiring (A2b-2): `h_q_r0` from the concrete post (`… ** (.r0 ↦ᵣ toU64 0)`),
    `h_cs` from `initState2`'s empty call stack + a call-free block. -/
theorem halts_zero_of_block_exit
    {nSteps nCu entry exitPc : Nat} {cr : CodeReq} {P Q : Assertion}
    {rr : Memory.RegionTable → Prop}
    (h : cuTripleWithinMem nSteps nCu entry exitPc cr P Q rr)
    {fetch : Nat → Option Insn} (h_cr : cr.SatisfiedBy fetch)
    (h_exit : fetch exitPc = some .exit)
    {s : State}
    (h_pre : P.holdsFor s) (h_pc : s.pc = entry) (h_run : s.exitCode = none)
    (h_bud : s.cuConsumed + nSteps + nCu ≤ s.cuBudget)
    (h_rr : rr s.regions)
    (h_q_r0 : ∀ t : State, Q.holdsFor t → t.regs.get .r0 = 0)
    (h_cs : ∀ k : Nat, (executeFn fetch s k).callStack = [])
    (FUEL : Nat) (h_fuel : nSteps + 1 ≤ FUEL) :
    (executeFn fetch s FUEL).exitCode = some 0 ∧
    Q.holdsFor (executeFn fetch s FUEL) := by
  sorry

end QEDGen.Solana.BridgeAdapter
