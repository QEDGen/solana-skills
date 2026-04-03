-- Weakest Precondition rules for sBPF primitives
--
-- Uses Loom's MAlg.lift as the WP function:
--   wp c post s = MAlg.lift c post s
-- where c : SbpfM α, post : α → State → Prop, s : State.
--
-- Each primitive gets a @[simp] reduction rule so that wp_step
-- can unfold one instruction at a time.

import QEDGen.Solana.SBPF.Monad

namespace QEDGen.Solana.SBPF

open Memory

/-! ## WP function

For SbpfM = StateT State Id, Loom gives us:
  MAlgOrdered (StateT State Id) (State → Prop)
and MAlg.lift : SbpfM α → (α → State → Prop) → State → Prop

We define `wp` as a convenient alias. -/

/-- Weakest precondition for an sBPF monadic action.
    `wp c post s` holds iff running `c` from state `s` produces
    a result `(a, s')` satisfying `post a s'`. -/
@[simp, reducible] def wp (c : SbpfM α) (post : α → State → Prop) (s : State) : Prop :=
  let (a, s') := c s
  post a s'

/-! ## Composition laws -/

@[simp] theorem wp_pure (a : α) (post : α → State → Prop) (s : State) :
    wp (pure a : SbpfM α) post s = post a s := by
  simp [wp, pure, StateT.pure, Id.run]

@[simp] theorem wp_bind (c : SbpfM α) (f : α → SbpfM β) (post : β → State → Prop) (s : State) :
    wp (c >>= f) post s = wp c (fun a => wp (f a) post) s := by
  simp only [wp, bind, StateT.bind]
  rfl

/-! ## Primitive WP rules -/

@[simp] theorem wp_getReg (r : Reg) (post : Nat → State → Prop) (s : State) :
    wp (getReg r) post s = post (s.regs.get r) s := rfl

@[simp] theorem wp_setReg (r : Reg) (v : Nat) (post : PUnit → State → Prop) (s : State) :
    wp (setReg r v) post s = post () { s with regs := s.regs.set r v } := rfl

@[simp] theorem wp_getMem (post : Mem → State → Prop) (s : State) :
    wp getMem post s = post s.mem s := rfl

@[simp] theorem wp_setMem (m : Mem) (post : PUnit → State → Prop) (s : State) :
    wp (setMem m) post s = post () { s with mem := m } := rfl

@[simp] theorem wp_getPc (post : Nat → State → Prop) (s : State) :
    wp getPc post s = post s.pc s := rfl

@[simp] theorem wp_setPc (pc : Nat) (post : PUnit → State → Prop) (s : State) :
    wp (setPc pc) post s = post () { s with pc := pc } := rfl

@[simp] theorem wp_getExitCode (post : Option Nat → State → Prop) (s : State) :
    wp getExitCode post s = post s.exitCode s := rfl

@[simp] theorem wp_setExit (code : Nat) (post : PUnit → State → Prop) (s : State) :
    wp (setExit code) post s = post () { s with exitCode := some code } := rfl

@[simp] theorem wp_advancePc (post : PUnit → State → Prop) (s : State) :
    wp advancePc post s = post () { s with pc := s.pc + 1 } := rfl

@[simp] theorem wp_resolveSrcM (src : Src) (post : Nat → State → Prop) (s : State) :
    wp (resolveSrcM src) post s = post (resolveSrc s.regs src) s := rfl

@[simp] theorem wp_loadByWidthM (addr : Nat) (w : Width) (post : Nat → State → Prop) (s : State) :
    wp (loadByWidthM addr w) post s = post (readByWidth s.mem addr w) s := rfl

@[simp] theorem wp_storeByWidthM (addr : Nat) (val : Nat) (w : Width) (post : PUnit → State → Prop) (s : State) :
    wp (storeByWidthM addr val w) post s = post () { s with mem := writeByWidth s.mem addr val w } := rfl

/-! ## Hoare triple -/

/-- Hoare triple: if `pre` holds on the initial state, then after running `c`,
    `post` holds on the final state. -/
def triple (pre : State → Prop) (c : SbpfM PUnit) (post : State → Prop) : Prop :=
  ∀ s, pre s → wp c (fun _ => post) s

end QEDGen.Solana.SBPF
