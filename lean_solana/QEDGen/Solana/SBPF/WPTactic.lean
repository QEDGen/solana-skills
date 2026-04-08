-- WP tactics for sBPF proof automation
--
-- wp_exec: one-shot tactic for sBPF property proofs
-- wp_step: single instruction step (for manual proofs)

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

/-! ## strip_writes — automatic memory write stripping

Strips nested write layers from read expressions by proving address disjointness
via omega. Pre-unfolds STACK_START so omega sees pure numerals.

Works for both cross-region (input reads through stack writes) and
within-stack (stack reads at different offsets from stack writes).

Usage (after a wp_step that left read-through-write patterns in the goal):
  wp_step [progAt, progAt_0, progAt_1, writeByWidth] [ea_offsets...]
  strip_writes
  simp [h_read_hypothesis, *]

For hypotheses containing wrapAdd/toU64, normalize them first:
  simp [wrapAdd, toU64] at h_addr
  strip_writes
-/

open QEDGen.Solana.SBPF.Memory in
syntax "strip_writes" : tactic

set_option hygiene false in
open QEDGen.Solana.SBPF.Memory in
macro_rules
  | `(tactic| strip_writes) => `(tactic| (
    try unfold STACK_START at *;
    repeat (first
      | rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]
      | rw [readU8_writeU64_outside _ _ _ _ (by omega)]
      | rw [readU64_writeU8_disjoint _ _ _ _ (by omega)]
      | rw [readU64_writeU64_same _ _ _ (by first | simp | omega)])))

/-! ## strip_writes_goal — goal-only variant for large contexts

Like strip_writes but only unfolds STACK_START in the goal, not hypotheses.
Use this when the context has many hypotheses (e.g., after 20+ wp_step calls)
and `unfold STACK_START at *` causes timeout. -/

open QEDGen.Solana.SBPF.Memory in
syntax "strip_writes_goal" : tactic

set_option hygiene false in
open QEDGen.Solana.SBPF.Memory in
macro_rules
  | `(tactic| strip_writes_goal) => `(tactic| (
    try unfold STACK_START;
    repeat (first
      | rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]
      | rw [readU8_writeU64_outside _ _ _ _ (by omega)]
      | rw [readU64_writeU8_disjoint _ _ _ _ (by omega)]
      | rw [readU64_writeU64_same _ _ _ (by first | simp | omega)])))

/-! ## rewrite_mem — rewrite memory chain + frame

Rewrites with a chain of memory hypotheses, then applies region-based
frame reasoning to strip write layers from read expressions.

Usage:
  rewrite_mem [hmem]

is equivalent to:
  rw [hmem]; mem_frame
-/

open Lean.Parser.Tactic in
syntax "rewrite_mem" "[" rwRule,* "]" : tactic

set_option hygiene false in
open Lean.Parser.Tactic in
open QEDGen.Solana.SBPF.Memory in
macro_rules
  | `(tactic| rewrite_mem [$[$ts:rwRule],*]) => `(tactic| (
      rw [$[$ts],*];
      -- Unfold STACK_START in goal only (not hypotheses — collapsed hmem can be huge)
      try unfold STACK_START;
      repeat (first
        -- Frame: read below stack, write above stack (most common in sBPF)
        | rw [readU64_writeU64_frame _ _ _ _ (by omega) (by omega)]
        | rw [readU8_writeU64_frame _ _ _ _ (by omega) (by omega)]
        -- Disjointness fallback (within same region or mixed widths)
        | rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]
        | rw [readU8_writeU64_outside _ _ _ _ (by omega)]
        | rw [readU64_writeU8_disjoint _ _ _ _ (by omega)]
        -- Same-address round-trip
        | rw [readU64_writeU64_same _ _ _ (by first | simp | omega)])))

/-! ## solve_read — one-shot memory read resolution

Rewrites with a chain of memory hypotheses, applies frame reasoning
to strip write layers, then closes the goal with `exact`.

Usage:
  solve_read [hmem] h_val
-/

open Lean.Parser.Tactic in
syntax "solve_read" "[" rwRule,* "]" term : tactic

set_option hygiene false in
open Lean.Parser.Tactic in
open QEDGen.Solana.SBPF.Memory in
macro_rules
  | `(tactic| solve_read [$[$ts:rwRule],*] $closing) => `(tactic| (
      rewrite_mem [$[$ts],*];
      exact $closing))

end QEDGen.Solana.SBPF
