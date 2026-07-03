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

  STATUS: the execution-bridge core (`halts_zero_of_block_exit`) is PROVEN,
  sorry-free and standard-axiom clean (propext / Classical.choice / Quot.sound).
  Remaining for full A2b: the per-spec glue feeding it — unfold
  `AsmRefinesFieldUpdate`, map `encodeState` ↔ `codecCoarse`, derive the triple's
  `P` from the bridge's `encodeState`/`Transition` hyps — then A2b-2 wires the
  `Bridge.lean` elaborator to emit `:= by exact …` instead of `sorry`.
-/

import SVM.SBPF
import SVM.SBPF.CPSSpec
import SVM.Solana.Abstract.Refinement
import SVM.Solana.Abstract.Transition

namespace QEDGen.Solana.BridgeAdapter

open SVM.SBPF SVM.Solana.Abstract

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
  -- 1. Frame the triple with R = emp and run it to the block exit.
  have hPemp : (P ** emp).holdsFor s :=
    (holdsFor_iff_pointwise (fun h => sepConj_emp_right h)).mpr h_pre
  obtain ⟨k, hk_le, hk_pc, hk_ex, hk_cu, hk_post⟩ :=
    h emp pcFree_emp fetch h_cr s hPemp h_pc h_run h_bud h_rr
  -- 2. The block post Q holds at the exit point (drop the emp frame).
  have hQse : Q.holdsFor (executeFn fetch s k) :=
    (holdsFor_iff_pointwise (fun h => sepConj_emp_right h)).mp hk_post
  -- 3. r0 = 0 and the call stack is empty at the exit point.
  have hr0 : (executeFn fetch s k).regs.get .r0 = 0 := h_q_r0 _ hQse
  have hcs : (executeFn fetch s k).callStack = [] := h_cs k
  -- 4. The exit step's budget check passes: cuConsumed ≤ cuBudget at the exit.
  have hbud_se : (executeFn fetch s k).cuConsumed ≤ (executeFn fetch s k).cuBudget := by
    have hbudget : (executeFn fetch s k).cuBudget = s.cuBudget := by
      simp [executeFn_preserves_cuBudget]
    omega
  -- 5. One `.exit` step: exitCode = some 0; regs/mem/pc/returnData/callStack
  --    preserved (it only writes exitCode, chargeCu only cuConsumed).
  have hstep1 : executeFn fetch (executeFn fetch s k) 1
      = chargeCu (step .exit (executeFn fetch s k)) := by
    have := executeFn_step fetch (executeFn fetch s k) 0 .exit hk_ex hbud_se
      (by rw [hk_pc]; exact h_exit)
    simpa using this
  have he1_exit : (executeFn fetch (executeFn fetch s k) 1).exitCode = some 0 := by
    rw [hstep1]; simp [step, chargeCu, hcs]; exact hr0
  -- 6. Extend to the bridge's fixed FUEL: run k+1 then halt-idempotent.
  have hfuel_eq : executeFn fetch s FUEL = executeFn fetch (executeFn fetch s k) 1 := by
    have hsplit : FUEL = (k + 1) + (FUEL - (k + 1)) := by omega
    rw [hsplit, executeFn_compose, executeFn_compose]
    exact executeFn_halted fetch _ _ 0 he1_exit
  refine ⟨by rw [hfuel_eq]; exact he1_exit, ?_⟩
  -- 7. Q survives: the exit step preserves every field `CompatibleWith` checks
  --    (regs/mem/pc/returnData/callStack); `holdsFor` ignores exitCode/cuConsumed.
  rw [hfuel_eq, hstep1]
  obtain ⟨hp, hcompat, hQ⟩ := hQse
  have hregs : (chargeCu (step .exit (executeFn fetch s k))).regs
      = (executeFn fetch s k).regs := by simp [chargeCu, step, hcs]
  have hmem : (chargeCu (step .exit (executeFn fetch s k))).mem
      = (executeFn fetch s k).mem := by simp [chargeCu, step, hcs]
  have hpc' : (chargeCu (step .exit (executeFn fetch s k))).pc
      = (executeFn fetch s k).pc := by simp [chargeCu, step, hcs]
  have hrd : (chargeCu (step .exit (executeFn fetch s k))).returnData
      = (executeFn fetch s k).returnData := by simp [chargeCu, step, hcs]
  have hcsk : (chargeCu (step .exit (executeFn fetch s k))).callStack
      = (executeFn fetch s k).callStack := by simp [chargeCu, step, hcs]
  exact ⟨hp,
    { regs := by rw [hregs]; exact hcompat.regs
      mem := by rw [hmem]; exact hcompat.mem
      pc := by rw [hpc']; exact hcompat.pc
      returnData := by rw [hrd]; exact hcompat.returnData
      callStack := by rw [hcsk]; exact hcompat.callStack },
    hQ⟩

/-- Glue over qedlift's actual output type. `AsmRefinesFieldUpdate` is exactly a
    `cuTripleWithinMem` with `P = setupPre ** codecCoarse base preFields` and
    `Q = setupPost ** codecCoarse base postFields`, so it feeds
    `halts_zero_of_block_exit` directly: a discharged field update halts the run
    with `exitCode = some 0` and the account memory encodes `postFields`. The
    bridge wiring (A2b-2) supplies `h_pre` (from `encodeState`/`initState2`),
    `h_q_r0` (post pins `r0 = 0`), and `h_cs` (call-free block). -/
theorem halts_zero_of_fieldUpdate
    {cr : CodeReq} {nSteps nCu entry exitPc base : Nat}
    {preFields postFields : List (Nat × FieldVal)}
    {setupPre setupPost : Assertion} {rr : Memory.RegionTable → Prop}
    (h : AsmRefinesFieldUpdate cr nSteps nCu entry exitPc rr base
          preFields postFields setupPre setupPost)
    {fetch : Nat → Option Insn} (h_cr : cr.SatisfiedBy fetch)
    (h_exit : fetch exitPc = some .exit)
    {s : State}
    (h_pre : (setupPre ** codecCoarse base preFields).holdsFor s)
    (h_pc : s.pc = entry) (h_run : s.exitCode = none)
    (h_bud : s.cuConsumed + nSteps + nCu ≤ s.cuBudget)
    (h_rr : rr s.regions)
    (h_q_r0 : ∀ t : State,
      (setupPost ** codecCoarse base postFields).holdsFor t → t.regs.get .r0 = 0)
    (h_cs : ∀ k : Nat, (executeFn fetch s k).callStack = [])
    (FUEL : Nat) (h_fuel : nSteps + 1 ≤ FUEL) :
    (executeFn fetch s FUEL).exitCode = some 0 ∧
    (setupPost ** codecCoarse base postFields).holdsFor (executeFn fetch s FUEL) := by
  unfold AsmRefinesFieldUpdate at h
  exact halts_zero_of_block_exit h h_cr h_exit h_pre h_pc h_run h_bud h_rr h_q_r0 h_cs FUEL h_fuel

/-- Every fault sentinel is a large nonzero constant (`0xFFFFFFFFFFFFFFxx`) —
    a fault can never masquerade as the success exit code. -/
theorem toSentinel_ne_zero (e : VmError) : e.toSentinel ≠ 0 := by
  cases e <;> decide

/-- Fault-side execution bridge (qedsvm#40 / v0.9.0, the `.rejects` analog of
    `halts_zero_of_fieldUpdate`). A discharged `AsmRefinesTransitionFault` —
    one FAULT path of the whole-transition bundle, under this path's guard
    hypotheses — runs to the typed fault and stays halted for any larger
    fuel: `exitCode = some e.toSentinel` (never `some 0`) and
    `vmError = some e`. A faulted instruction is rolled back wholesale at the
    chain level, so no post-memory claim is made (or needed). -/
theorem faults_of_transitionFault
    {cr : CodeReq} {nSteps nCu entry : Nat}
    {accts : List AccountFields} {setupPre : Assertion}
    {rr : Memory.RegionTable → Prop} {e : VmError}
    (h : AsmRefinesTransitionFault cr nSteps nCu entry rr e accts setupPre)
    {fetch : Nat → Option Insn} (h_cr : cr.SatisfiedBy fetch)
    {s : State}
    (h_pre : (setupPre ** codecsPre accts).holdsFor s)
    (h_pc : s.pc = entry) (h_run : s.exitCode = none)
    (h_bud : s.cuConsumed + nSteps + nCu ≤ s.cuBudget)
    (h_rr : rr s.regions)
    (FUEL : Nat) (h_fuel : nSteps ≤ FUEL) :
    (executeFn fetch s FUEL).exitCode = some e.toSentinel ∧
    (executeFn fetch s FUEL).vmError = some e := by
  unfold AsmRefinesTransitionFault at h
  -- 1. Frame with emp and run the fault triple to its halt point.
  have hPemp : ((setupPre ** codecsPre accts) ** emp).holdsFor s :=
    (holdsFor_iff_pointwise (fun h => sepConj_emp_right h)).mpr h_pre
  obtain ⟨k, hk_le, hk_code, hk_err, _⟩ :=
    h emp pcFree_emp fetch h_cr s hPemp h_pc h_run h_bud h_rr
  -- 2. Halt idempotence extends the fault to the bridge's fixed FUEL.
  have hfe : executeFn fetch s FUEL = executeFn fetch s k := by
    have hsplit : FUEL = k + (FUEL - k) := by omega
    rw [hsplit, executeFn_compose]
    exact executeFn_halted fetch _ _ _ hk_code
  exact ⟨by rw [hfe]; exact hk_code, by rw [hfe]; exact hk_err⟩

end QEDGen.Solana.BridgeAdapter
