-- Bridge between executeFn (pure) and execSegment (monadic)
--
-- Proves that executeFn and execSegment produce the same final state,
-- enabling theorem statements to use executeFn while proofs use WP.

import QEDGen.Solana.SBPF.MonadicStep

namespace QEDGen.Solana.SBPF

/-! ## Core equivalence -/

/-- executeFn and execSegment produce the same final state. -/
theorem executeFn_eq_execSegment (fetch : Nat → Option Insn) (s : State) (fuel : Nat) :
    executeFn fetch s fuel = (execSegment fetch fuel s).2 := by
  induction fuel generalizing s with
  | zero => rfl
  | succ n ih =>
    unfold executeFn execSegment
    cases h_exit : s.exitCode with
    | some _ => rfl
    | none =>
      cases h_fetch : fetch s.pc with
      | none => rfl
      | some insn =>
        simp (config := { failIfUnchanged := false }) only [h_exit]
        have heq : step insn s = (execInsn insn s).2 := step_eq_execInsn insn s
        rw [heq]
        exact ih (execInsn insn s).2

end QEDGen.Solana.SBPF
