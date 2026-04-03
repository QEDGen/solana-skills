-- WP tactics for sBPF proof automation
--
-- wp_step:  unfold one instruction via WP rules (O(1) kernel depth)
-- wp_steps: repeat wp_step until no more instructions

import QEDGen.Solana.SBPF.WP
import QEDGen.Solana.SBPF.MonadicStep
import QEDGen.Solana.SBPF.Bridge

namespace QEDGen.Solana.SBPF

/-! ## WP step tactic

Unfolds one instruction of execSegment and simplifies using WP rules.
Each step is O(1) kernel depth — no nested state accumulation. -/

/-- Unfold one level of execSegment and reduce the resulting instruction.
    Uses `unfold` (not simp) on execSegment to avoid recursive blowup.
    Each call is O(1) kernel depth regardless of remaining fuel. -/
syntax "wp_step" : tactic

macro_rules
  | `(tactic| wp_step) => `(tactic| (
      unfold execSegment;
      simp (config := { failIfUnchanged := false }) only [execInsn,
        RegFile.get, RegFile.set,
        resolveSrc, effectiveAddr, effectiveAddr_nat,
        readByWidth, writeByWidth,
        execSyscall,
        Nat.add_zero]))

/-- Repeatedly unfold instructions until the goal is discharged or
    no further progress can be made. -/
syntax "wp_steps" : tactic

macro_rules
  | `(tactic| wp_steps) => `(tactic| (
      try simp only [effectiveAddr, effectiveAddr_nat, Nat.add_zero] at *;
      repeat wp_step))

/-- Apply the WP bridge: convert an executeFn goal to an execSegment goal,
    then use WP tactics. -/
syntax "wp_bridge" : tactic

macro_rules
  | `(tactic| wp_bridge) => `(tactic| (
      rw [executeFn_eq_execSegment];
      wp_steps))

end QEDGen.Solana.SBPF
