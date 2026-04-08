-- Reusable sBPF instruction patterns for formal verification
--
-- Common 2-3 instruction sequences that recur across Solana sBPF programs:
-- error handlers, duplicate checks, and chunk comparisons.
--
-- All theorems are parameterized over the fetch function and registers.
-- Register disjointness hypotheses are dischargeable by `decide` at
-- call sites since the caller always has concrete register names.

import QEDGen.Solana.SBPF.Execute

namespace QEDGen.Solana.SBPF

open Memory

/-! ## Error handler: mov32 r0 errorCode; exit

Every sBPF validation program has error handlers that set r0 to an error
code and exit. This 2-step pattern is fully general — parameterized over
the fetch function and error code. -/

theorem error_exit (fetch : Nat → Option Insn) (s : State) (errorCode : Int)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.mov32 .r0 (.imm errorCode)))
    (h_f2 : fetch (s.pc + 1) = some .exit) :
    (executeFn fetch s 2).exitCode = some (toU64 errorCode % U32_MODULUS) := by
  simp only [executeFn, step, resolveSrc, RegFile.get, RegFile.set, h_exit, h_f1, h_f2]

/-! ## Duplicate marker check: ldx byte + jne

SIMD-0321 account format marks each account with a duplicate byte:
255 = non-duplicate (pass), anything else = duplicate (branch to error).

Parameters:
- `dstReg`: scratch register that receives the loaded byte
- `addrReg`: register holding the account base address
- `off`: byte offset to the duplicate marker field
- `dupImm`: the immediate value in the jne (typically 255 as Int) -/

set_option maxHeartbeats 800000 in
theorem dup_pass (fetch : Nat → Option Insn) (s : State)
    (dstReg addrReg : Reg) (off : Int) (dupImm : Int) (target : Nat)
    (h_dstA_ne_r10 : dstReg ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .byte dstReg addrReg off))
    (h_f2 : fetch (s.pc + 1) = some (.jne dstReg (.imm dupImm) target))
    (h_eq : readU8 s.mem (effectiveAddr (s.regs.get addrReg) off) = toU64 dupImm) :
    let s' := executeFn fetch s 2
    s'.exitCode = none ∧ s'.pc = s.pc + 2 ∧ s'.mem = s.mem ∧
    (∀ r, r ≠ dstReg → s'.regs.get r = s.regs.get r) := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    h_exit, h_f1, h_f2, h_eq]
  refine ⟨trivial, by simp, trivial, fun r hr => ?_⟩
  simp only [RegFile.get_set_diff _ _ _ _ hr]

set_option maxHeartbeats 800000 in
theorem dup_fail (fetch : Nat → Option Insn) (s : State)
    (dstReg addrReg : Reg) (off : Int) (dupImm : Int) (target : Nat)
    (h_dstA_ne_r10 : dstReg ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .byte dstReg addrReg off))
    (h_f2 : fetch (s.pc + 1) = some (.jne dstReg (.imm dupImm) target))
    (h_ne : readU8 s.mem (effectiveAddr (s.regs.get addrReg) off) ≠ toU64 dupImm) :
    let s' := executeFn fetch s 2
    s'.exitCode = none ∧ s'.pc = target ∧ s'.mem = s.mem ∧
    (∀ r, r ≠ dstReg → s'.regs.get r = s.regs.get r) := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    h_exit, h_f1, h_f2]
  refine ⟨trivial, if_pos h_ne, trivial, fun r hr => ?_⟩
  simp only [RegFile.get_set_diff _ _ _ _ hr]

/-! ## Chunk comparison: ldx + ldx + jne (memory vs memory)

Compares two 8-byte memory chunks via scratch registers. Used for 32-byte
pubkey validation (4 chunks × 3 instructions each).

Register disjointness hypotheses are dischargeable by `decide` at call sites
since the caller always has concrete register names from their program. -/

set_option maxHeartbeats 1600000 in
theorem chunk_eq_mem (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA srcB : Reg) (offA offB : Int) (target : Nat)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_srcB_ne_dstA : srcB ≠ dstA)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA offA))
    (h_f2 : fetch (s.pc + 1) = some (.ldx .dword dstB srcB offB))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_eq : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA) =
            readU64 s.mem (effectiveAddr (s.regs.get srcB) offB)) :
    let s' := executeFn fetch s 3
    s'.exitCode = none ∧ s'.pc = s.pc + 3 ∧ s'.mem = s.mem ∧
    (∀ r, r ≠ dstA → r ≠ dstB → s'.regs.get r = s.regs.get r) := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    RegFile.get_set_self _ _ _ h_dstB_ne_r10,
    RegFile.get_set_diff _ _ _ _ h_srcB_ne_dstA,
    RegFile.get_set_diff _ _ _ _ h_dstA_ne_dstB,
    h_exit, h_f1, h_f2, h_f3, h_eq]
  refine ⟨trivial, by simp, trivial, fun r hr1 hr2 => ?_⟩
  simp only [RegFile.get_set_diff _ _ _ _ hr2, RegFile.get_set_diff _ _ _ _ hr1]

set_option maxHeartbeats 1600000 in
theorem chunk_ne_mem (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA srcB : Reg) (offA offB : Int) (target : Nat)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_srcB_ne_dstA : srcB ≠ dstA)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA offA))
    (h_f2 : fetch (s.pc + 1) = some (.ldx .dword dstB srcB offB))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_ne : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA) ≠
            readU64 s.mem (effectiveAddr (s.regs.get srcB) offB)) :
    let s' := executeFn fetch s 3
    s'.exitCode = none ∧ s'.pc = target ∧ s'.mem = s.mem := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    RegFile.get_set_self _ _ _ h_dstB_ne_r10,
    RegFile.get_set_diff _ _ _ _ h_srcB_ne_dstA,
    RegFile.get_set_diff _ _ _ _ h_dstA_ne_dstB,
    h_exit, h_f1, h_f2, h_f3]
  exact ⟨trivial, if_pos h_ne, trivial⟩

/-! ## Chunk comparison: ldx + lddw + jne (memory vs 64-bit immediate)

Used when comparing a memory chunk against a known constant loaded via lddw.
The `toU64` in the conclusion comes from lddw semantics: `rf.set dst (toU64 imm)`.

**Important**: when proving the `h_ne` hypothesis at call sites, avoid
`simp [toU64]` — it causes term explosion. Instead, use bridge lemmas:
  `theorem my_bridge : toU64 (↑MY_CONST : Int) = MY_CONST := by native_decide`
and rewrite with them before applying this theorem. -/

set_option maxHeartbeats 1600000 in
theorem chunk_eq_imm (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA : Reg) (off : Int) (val : Int) (target : Nat)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA off))
    (h_f2 : fetch (s.pc + 1) = some (.lddw dstB val))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_eq : readU64 s.mem (effectiveAddr (s.regs.get srcA) off) = toU64 val) :
    let s' := executeFn fetch s 3
    s'.exitCode = none ∧ s'.pc = s.pc + 3 ∧ s'.mem = s.mem ∧
    (∀ r, r ≠ dstA → r ≠ dstB → s'.regs.get r = s.regs.get r) := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    RegFile.get_set_self _ _ _ h_dstB_ne_r10,
    RegFile.get_set_diff _ _ _ _ h_dstA_ne_dstB,
    h_exit, h_f1, h_f2, h_f3, h_eq]
  refine ⟨trivial, by simp, trivial, fun r hr1 hr2 => ?_⟩
  simp only [RegFile.get_set_diff _ _ _ _ hr2, RegFile.get_set_diff _ _ _ _ hr1]

set_option maxHeartbeats 1600000 in
theorem chunk_ne_imm (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA : Reg) (off : Int) (val : Int) (target : Nat)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA off))
    (h_f2 : fetch (s.pc + 1) = some (.lddw dstB val))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_ne : readU64 s.mem (effectiveAddr (s.regs.get srcA) off) ≠ toU64 val) :
    let s' := executeFn fetch s 3
    s'.exitCode = none ∧ s'.pc = target ∧ s'.mem = s.mem := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    RegFile.get_set_self _ _ _ h_dstB_ne_r10,
    RegFile.get_set_diff _ _ _ _ h_dstA_ne_dstB,
    h_exit, h_f1, h_f2, h_f3]
  exact ⟨trivial, if_pos h_ne, trivial⟩

/-! ## Chunk comparison: ldx + mov32 + jne (memory vs 32-bit immediate)

Used for the last chunk of a pubkey comparison when the constant fits in 32 bits.
mov32 sets `rf.set dst (resolveSrc rf src % U32_MODULUS)`. -/

set_option maxHeartbeats 1600000 in
theorem chunk_eq_imm32 (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA : Reg) (off : Int) (val : Int) (target : Nat)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA off))
    (h_f2 : fetch (s.pc + 1) = some (.mov32 dstB (.imm val)))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_eq : readU64 s.mem (effectiveAddr (s.regs.get srcA) off) = toU64 val % U32_MODULUS) :
    let s' := executeFn fetch s 3
    s'.exitCode = none ∧ s'.pc = s.pc + 3 ∧ s'.mem = s.mem ∧
    (∀ r, r ≠ dstA → r ≠ dstB → s'.regs.get r = s.regs.get r) := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    RegFile.get_set_self _ _ _ h_dstB_ne_r10,
    RegFile.get_set_diff _ _ _ _ h_dstA_ne_dstB,
    h_exit, h_f1, h_f2, h_f3, h_eq]
  refine ⟨trivial, by simp, trivial, fun r hr1 hr2 => ?_⟩
  simp only [RegFile.get_set_diff _ _ _ _ hr2, RegFile.get_set_diff _ _ _ _ hr1]

set_option maxHeartbeats 1600000 in
theorem chunk_ne_imm32 (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA : Reg) (off : Int) (val : Int) (target : Nat)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA off))
    (h_f2 : fetch (s.pc + 1) = some (.mov32 dstB (.imm val)))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_ne : readU64 s.mem (effectiveAddr (s.regs.get srcA) off) ≠ toU64 val % U32_MODULUS) :
    let s' := executeFn fetch s 3
    s'.exitCode = none ∧ s'.pc = target ∧ s'.mem = s.mem := by
  simp only [executeFn, step, readByWidth, resolveSrc,
    RegFile.get_set_self _ _ _ h_dstA_ne_r10,
    RegFile.get_set_self _ _ _ h_dstB_ne_r10,
    RegFile.get_set_diff _ _ _ _ h_dstA_ne_dstB,
    h_exit, h_f1, h_f2, h_f3]
  exact ⟨trivial, if_pos h_ne, trivial⟩

/-! ## Composition: chunk mismatch → error exit

Combines a 3-step chunk mismatch (branch taken) with a 2-step error handler
into a single 5-step theorem. Use with `executeFn_compose` to split longer
executions. -/

theorem chunk_ne_mem_error (fetch : Nat → Option Insn) (s : State) (n : Nat)
    (dstA dstB srcA srcB : Reg) (offA offB : Int) (target : Nat) (errorCode : Int)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_srcB_ne_dstA : srcB ≠ dstA)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA offA))
    (h_f2 : fetch (s.pc + 1) = some (.ldx .dword dstB srcB offB))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_ne : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA) ≠
            readU64 s.mem (effectiveAddr (s.regs.get srcB) offB))
    (h_err1 : fetch target = some (.mov32 .r0 (.imm errorCode)))
    (h_err2 : fetch (target + 1) = some .exit)
    (h_fuel : n ≥ 5) :
    (executeFn fetch s n).exitCode = some (toU64 errorCode % U32_MODULUS) := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  obtain ⟨he, hp, _⟩ := chunk_ne_mem fetch s dstA dstB srcA srcB offA offB target
    h_dstA_ne_dstB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10
    h_exit h_f1 h_f2 h_f3 h_ne
  rw [show (5 : Nat) = 3 + 2 from rfl, executeFn_compose]
  have h5 := error_exit fetch _ errorCode he (by rwa [hp]) (by rwa [hp])
  rw [executeFn_halted _ _ _ _ h5]
  exact h5

end QEDGen.Solana.SBPF
