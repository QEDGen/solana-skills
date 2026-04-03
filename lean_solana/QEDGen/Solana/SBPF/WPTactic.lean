-- WP tactics for sBPF proof automation
--
-- wp_exec: one-shot tactic for sBPF property proofs
-- wp_step: single instruction step (for manual proofs)

import QEDGen.Solana.SBPF.WP
import QEDGen.Solana.SBPF.MonadicStep
import QEDGen.Solana.SBPF.Bridge

namespace QEDGen.Solana.SBPF

/-! ## wp_exec — one-shot sBPF verification

Proves properties of the form:
  (executeFn progAt (initState inputAddr mem) FUEL).exitCode = some CODE

Usage:
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_88]

First bracket: fetch function + chunk defs (passed to dsimp for instruction decode).
Second bracket: effectiveAddr lemmas + extras (passed to simp for branch resolution).

The tactic:
1. Applies executeFn_eq_execSegment to switch to monadic execution
2. Iteratively unfolds execSegment one step at a time (O(1) kernel depth)
3. Uses dsimp to evaluate instruction fetch via kernel reduction
4. Uses simp with hypotheses to resolve branch conditions
5. Closes the halted-state residual via rfl

Example:
  theorem rejects_bad_input ... := by
    have h1 : ¬(readU64 mem inputAddr = EXPECTED) := by ...
    wp_exec [progAt, progAt_0] [ea_0]
-/

open Lean.Parser.Tactic in
syntax "wp_exec" "[" simpLemma,* "]" "[" simpLemma,* "]" : tactic

set_option hygiene false in
open Lean.Parser.Tactic in
macro_rules
  | `(tactic| wp_exec [$[$fetch:simpLemma],*] [$[$extras:simpLemma],*]) => `(tactic| (
      rw [executeFn_eq_execSegment];
      repeat (
        unfold execSegment;
        dsimp (config := { failIfUnchanged := false })
          [initState, execInsn,
           RegFile.get, RegFile.set, resolveSrc, readByWidth, $[$fetch],*];
        simp (config := { failIfUnchanged := false }) [*, $[$extras],*]);
      rfl))

/-! ## wp_step — single instruction step (for manual proofs)

Unfolds one level of execSegment, evaluates the instruction via dsimp,
and simplifies with hypotheses. Use when wp_exec needs manual guidance
(e.g., memory disjointness lemmas between steps). -/

open Lean.Parser.Tactic in
syntax "wp_step" "[" simpLemma,* "]" "[" simpLemma,* "]" : tactic

set_option hygiene false in
open Lean.Parser.Tactic in
macro_rules
  | `(tactic| wp_step [$[$fetch:simpLemma],*] [$[$extras:simpLemma],*]) => `(tactic| (
      unfold execSegment;
      dsimp (config := { failIfUnchanged := false })
        [initState, execInsn,
         RegFile.get, RegFile.set, resolveSrc, readByWidth, $[$fetch],*];
      simp (config := { failIfUnchanged := false }) [*, $[$extras],*]))

end QEDGen.Solana.SBPF
