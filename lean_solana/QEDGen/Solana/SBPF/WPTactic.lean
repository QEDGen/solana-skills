-- WP tactics for sBPF proof automation
--
-- wp_exec: one-shot tactic for sBPF property proofs
-- wp_step: single instruction step (for manual proofs)

import Lean.Elab.Tactic
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

/-! ## wp_step_from — single step from abstract state

Like wp_step but takes a third (first) bracket of state-field hypotheses
that are passed to dsimp. This lets dsimp resolve `.pc`, `.exitCode`, and
register/memory accesses on a state that is not a concrete literal — e.g.
the result of `executeFn_compose` splitting.

Usage (in a suffix proof parameterized over abstract `s : State`):
  wp_step_from [h_exit, h_pc, h_r9, h_r10, h_mem]
    [progAt, progAt_0, progAt_1] [ea_extras]

The state hypotheses (h_exit : s.exitCode = none, h_pc : s.pc = 51, etc.)
are merged into the dsimp call so `match s.exitCode` and `progAt s.pc`
reduce even when `s` is abstract. After one step the pc is concrete;
exitCode stays abstract (inherited via struct `with`) so h_exit is needed
at every step until the exit instruction fires. -/

open Lean.Parser.Tactic in
syntax "wp_step_from" "[" simpLemma,* "]" "[" simpLemma,* "]" "[" simpLemma,* "]" : tactic

set_option hygiene false in
open Lean.Parser.Tactic in
macro_rules
  | `(tactic| wp_step_from [$[$state:simpLemma],*] [$[$fetch:simpLemma],*] [$[$extras:simpLemma],*]) =>
    `(tactic| (
      unfold execSegment;
      dsimp (config := { failIfUnchanged := false })
        [initState, execInsn,
         RegFile.get, RegFile.set, resolveSrc, readByWidth, $[$state],*, $[$fetch],*];
      simp (config := { failIfUnchanged := false }) [*, $[$extras],*]))

/-! ## wp_step_from_only — single step with simp only (for large proofs)

Like wp_step_from but uses `simp only` instead of `simp [*]` to avoid
exponential blowup when the context has many hypotheses (e.g., by_cases
cascade in chunk comparison proofs). Pass only the specific lemmas needed
for each step in the third bracket. -/

open Lean.Parser.Tactic in
syntax "wp_step_from_only" "[" simpLemma,* "]" "[" simpLemma,* "]" "[" simpLemma,* "]" : tactic

set_option hygiene false in
open Lean.Parser.Tactic in
macro_rules
  | `(tactic| wp_step_from_only [$[$state:simpLemma],*] [$[$fetch:simpLemma],*] [$[$extras:simpLemma],*]) =>
    `(tactic| (
      unfold execSegment;
      dsimp (config := { failIfUnchanged := false })
        [initState, execInsn,
         RegFile.get, RegFile.set, resolveSrc, readByWidth, $[$state],*, $[$fetch],*];
      simp (config := { failIfUnchanged := false }) only [$[$extras],*]))

/-! ## wp_exec_pure — evaluate register-only (memory-preserving) sections

Decomposes `executeFn fetch s n` into n single-step applications via
`executeFn_compose`, then evaluates all steps with `simp`.

Use this for instruction sequences that only modify registers (mov64, add64,
call with noop syscall, etc.). It proves register props AND `s'.mem = s.mem`
in one shot, eliminating the need for sub-lemma splitting.

Usage:
  -- After rw [executeFn_eq_execSegment] or when goal contains executeFn:
  wp_exec_pure 11 [h_exit, h_pc, h_r1] [progAt, progAt_0, progAt_1]

First bracket: state hypotheses (exitCode, pc, register values).
Second bracket: fetch function + chunk defs.

Replaces the manual pattern:
  rw [show (11:Nat) = 1+1+...+1 from rfl]
  iterate 10 (rw [executeFn_compose])
  simp only [executeFn, ..., step, execSyscall, ...]
-/

open Lean.Parser.Tactic in
syntax "wp_exec_pure" num "[" simpLemma,* "]" "[" simpLemma,* "]" : tactic

set_option hygiene false in
open Lean Elab Tactic in
open Lean.Parser.Tactic in
private def wpExecPureDecompose : Nat → TacticM Unit
  | 0 | 1 => return ()
  | n + 2 => do
    let nLit := Lean.Syntax.mkNumLit (toString (n + 2))
    let prevLit := Lean.Syntax.mkNumLit (toString (n + 1))
    evalTactic (← `(tactic| rw [show ($nLit : Nat) = 1 + $prevLit from rfl, executeFn_compose]))
    wpExecPureDecompose (n + 1)

set_option hygiene false in
open Lean Elab Tactic in
open Lean.Parser.Tactic in
elab_rules : tactic
  | `(tactic| wp_exec_pure $n:num [$[$state:simpLemma],*] [$[$fetch:simpLemma],*]) => do
    wpExecPureDecompose n.getNat
    evalTactic (← `(tactic| simp only [executeFn, executeFn_zero, step, execSyscall,
      RegFile.get, RegFile.set, resolveSrc, toU64, wrapAdd,
      $[$state],*, $[$fetch],*]))

end QEDGen.Solana.SBPF
