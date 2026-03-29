-- Automation for sBPF proof unrolling
--
-- Provides the `sbpf_steps` tactic that replaces manual execute_step
-- chains with a single tactic call.

import QEDGen.Solana.SBPF.Execute

namespace QEDGen.Solana.SBPF

open Memory

/-! ## Simplification improvements -/

/-- effectiveAddr with non-negative offset reduces to plain Nat addition.
    Eliminates the Int.toNat roundtrip for the common case. -/
@[simp] theorem effectiveAddr_nat (base off : Nat) :
    effectiveAddr base (↑off) = base + off := by
  unfold effectiveAddr; omega

-- Make readByWidth auto-simplify (dispatches to readU8/readU16/readU32/readU64)
attribute [simp] Memory.readByWidth

/-! ## Flexible execute_step -/

/-- Like execute_step but accepts any positive fuel (no n+1 form required) -/
theorem execute_step' (prog : Program) (s : State) (n : Nat) (insn : Insn)
    (h_pos : 0 < n)
    (h_running : s.exitCode = none)
    (h_fetch : prog[s.pc]? = some insn) :
    execute prog s n = execute prog (step insn s) (n - 1) := by
  cases n with
  | zero => omega
  | succ n => simp [execute, h_running, h_fetch]

/-! ## The sbpf_steps tactic -/

/-- Automated sBPF execution unrolling.

    Usage: `sbpf_steps` — automatically uses all hypotheses in scope.

    The program definition MUST be marked `@[simp]` for array access evaluation.

    The tactic:
    1. Normalizes effectiveAddr in all hypotheses
    2. Uses simp to fully unroll execute through step/initState
    3. Resolves branch conditions using hypotheses in scope -/
syntax "sbpf_steps" : tactic

macro_rules
  | `(tactic| sbpf_steps) => `(tactic| (
      try simp only [effectiveAddr, effectiveAddr_nat, Nat.add_zero] at *;
      simp [*, execute, execute_halted, step, initState, RegFile.get, RegFile.set, resolveSrc,
            readByWidth, effectiveAddr, effectiveAddr_nat, execSyscall]))

end QEDGen.Solana.SBPF
