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

/-! ## Chunk mismatch → error (immediate variants)

Like `chunk_ne_mem_error` but for lddw and mov32 instruction patterns.
Used in pubkey comparison against known constant values. -/

theorem chunk_ne_imm_error (fetch : Nat → Option Insn) (s : State) (n : Nat)
    (dstA dstB srcA : Reg) (off : Int) (val : Int) (target : Nat) (errorCode : Int)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA off))
    (h_f2 : fetch (s.pc + 1) = some (.lddw dstB val))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_ne : readU64 s.mem (effectiveAddr (s.regs.get srcA) off) ≠ toU64 val)
    (h_err1 : fetch target = some (.mov32 .r0 (.imm errorCode)))
    (h_err2 : fetch (target + 1) = some .exit)
    (h_fuel : n ≥ 5) :
    (executeFn fetch s n).exitCode = some (toU64 errorCode % U32_MODULUS) := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  obtain ⟨he, hp, _⟩ := chunk_ne_imm fetch s dstA dstB srcA off val target
    h_dstA_ne_dstB h_dstA_ne_r10 h_dstB_ne_r10
    h_exit h_f1 h_f2 h_f3 h_ne
  rw [show (5 : Nat) = 3 + 2 from rfl, executeFn_compose]
  have h5 := error_exit fetch _ errorCode he (by rwa [hp]) (by rwa [hp])
  rw [executeFn_halted _ _ _ _ h5]
  exact h5

theorem chunk_ne_imm32_error (fetch : Nat → Option Insn) (s : State) (n : Nat)
    (dstA dstB srcA : Reg) (off : Int) (val : Int) (target : Nat) (errorCode : Int)
    (h_dstA_ne_dstB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10)
    (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_exit : s.exitCode = none)
    (h_f1 : fetch s.pc = some (.ldx .dword dstA srcA off))
    (h_f2 : fetch (s.pc + 1) = some (.mov32 dstB (.imm val)))
    (h_f3 : fetch (s.pc + 2) = some (.jne dstA (.reg dstB) target))
    (h_ne : readU64 s.mem (effectiveAddr (s.regs.get srcA) off) ≠ toU64 val % U32_MODULUS)
    (h_err1 : fetch target = some (.mov32 .r0 (.imm errorCode)))
    (h_err2 : fetch (target + 1) = some .exit)
    (h_fuel : n ≥ 5) :
    (executeFn fetch s n).exitCode = some (toU64 errorCode % U32_MODULUS) := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  obtain ⟨he, hp, _⟩ := chunk_ne_imm32 fetch s dstA dstB srcA off val target
    h_dstA_ne_dstB h_dstA_ne_r10 h_dstB_ne_r10
    h_exit h_f1 h_f2 h_f3 h_ne
  rw [show (5 : Nat) = 3 + 2 from rfl, executeFn_compose]
  have h5 := error_exit fetch _ errorCode he (by rwa [hp]) (by rwa [hp])
  rw [executeFn_halted _ _ _ _ h5]
  exact h5

/-! ## 4-chunk pubkey comparison cascades

These theorems prove that a 4-chunk, 14-step pubkey comparison sequence
results in error exit when at least one chunk mismatches. Each chunk is
3 instructions: ldx(src) + ldx/lddw/mov32(ref) + jne(branch on mismatch).

Two variants:
- `pubkey_compare_mem`: both sides memory reads (ldx + ldx + jne)
- `pubkey_compare_imm`: source=memory, ref=constant (3× lddw + 1× mov32) -/

set_option maxHeartbeats 8000000 in
theorem pubkey_compare_mem (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA srcB : Reg) (pc : Nat)
    (offA0 offA1 offA2 offA3 offB0 offB1 offB2 offB3 : Int)
    (target : Nat) (errorCode : Int)
    (a0 a1 a2 a3 b0 b1 b2 b3 : Nat)
    (h_ne_AB : dstA ≠ dstB) (h_srcB_ne_dstA : srcB ≠ dstA)
    (h_dstA_ne_r10 : dstA ≠ .r10) (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_srcA_ne_dstA : srcA ≠ dstA) (h_srcA_ne_dstB : srcA ≠ dstB)
    (h_srcB_ne_dstB : srcB ≠ dstB)
    (h_exit : s.exitCode = none) (h_pc : s.pc = pc)
    (hf0 : fetch pc = some (.ldx .dword dstA srcA offA0))
    (hf1 : fetch (pc + 1) = some (.ldx .dword dstB srcB offB0))
    (hf2 : fetch (pc + 2) = some (.jne dstA (.reg dstB) target))
    (hf3 : fetch (pc + 3) = some (.ldx .dword dstA srcA offA1))
    (hf4 : fetch (pc + 4) = some (.ldx .dword dstB srcB offB1))
    (hf5 : fetch (pc + 5) = some (.jne dstA (.reg dstB) target))
    (hf6 : fetch (pc + 6) = some (.ldx .dword dstA srcA offA2))
    (hf7 : fetch (pc + 7) = some (.ldx .dword dstB srcB offB2))
    (hf8 : fetch (pc + 8) = some (.jne dstA (.reg dstB) target))
    (hf9 : fetch (pc + 9) = some (.ldx .dword dstA srcA offA3))
    (hf10 : fetch (pc + 10) = some (.ldx .dword dstB srcB offB3))
    (hf11 : fetch (pc + 11) = some (.jne dstA (.reg dstB) target))
    (hfe1 : fetch target = some (.mov32 .r0 (.imm errorCode)))
    (hfe2 : fetch (target + 1) = some .exit)
    (ha0 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA0) = a0)
    (hb0 : readU64 s.mem (effectiveAddr (s.regs.get srcB) offB0) = b0)
    (ha1 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA1) = a1)
    (hb1 : readU64 s.mem (effectiveAddr (s.regs.get srcB) offB1) = b1)
    (ha2 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA2) = a2)
    (hb2 : readU64 s.mem (effectiveAddr (s.regs.get srcB) offB2) = b2)
    (ha3 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA3) = a3)
    (hb3 : readU64 s.mem (effectiveAddr (s.regs.get srcB) offB3) = b3)
    (h_ne : a0 ≠ b0 ∨ a1 ≠ b1 ∨ a2 ≠ b2 ∨ a3 ≠ b3) :
    (executeFn fetch s 14).exitCode = some (toU64 errorCode % U32_MODULUS) := by
  by_cases h_eq0 : a0 = b0
  · simp [h_eq0] at h_ne
    rw [show (14 : Nat) = 3 + 11 from rfl, executeFn_compose]
    obtain ⟨he1, hp1, hm1, hreg1⟩ := chunk_eq_mem fetch s dstA dstB srcA srcB offA0 offB0 target
      h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 h_exit
      (by rw [h_pc]; exact hf0) (by rw [h_pc]; exact hf1) (by rw [h_pc]; exact hf2)
      (by rw [ha0, hb0]; exact h_eq0)
    by_cases h_eq1 : a1 = b1
    · simp [h_eq1] at h_ne
      rw [show (11 : Nat) = 3 + 8 from rfl, executeFn_compose]
      obtain ⟨he2, hp2, hm2, hreg2⟩ := chunk_eq_mem fetch _ dstA dstB srcA srcB offA1 offB1 target
        h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 he1
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf3)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf4)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf5)
        (by rw [hm1, hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha1,
                hreg1 srcB h_srcB_ne_dstA h_srcB_ne_dstB, hb1]; exact h_eq1)
      by_cases h_eq2 : a2 = b2
      · simp [h_eq2] at h_ne
        rw [show (8 : Nat) = 3 + 5 from rfl, executeFn_compose]
        obtain ⟨he3, hp3, hm3, hreg3⟩ := chunk_eq_mem fetch _ dstA dstB srcA srcB offA2 offB2 target
          h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 he2
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf6)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf7)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf8)
          (by rw [hm2, hm1,
                  hreg2 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha2,
                  hreg2 srcB h_srcB_ne_dstA h_srcB_ne_dstB,
                  hreg1 srcB h_srcB_ne_dstA h_srcB_ne_dstB, hb2]; exact h_eq2)
        exact chunk_ne_mem_error fetch _ 5 dstA dstB srcA srcB offA3 offB3 target errorCode
          h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 he3
          (by simp only [hp3, hp2, hp1, h_pc, Nat.add_assoc]; exact hf9)
          (by simp only [hp3, hp2, hp1, h_pc, Nat.add_assoc]; exact hf10)
          (by simp only [hp3, hp2, hp1, h_pc, Nat.add_assoc]; exact hf11)
          (by rw [hm3, hm2, hm1,
                  hreg3 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg2 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha3,
                  hreg3 srcB h_srcB_ne_dstA h_srcB_ne_dstB,
                  hreg2 srcB h_srcB_ne_dstA h_srcB_ne_dstB,
                  hreg1 srcB h_srcB_ne_dstA h_srcB_ne_dstB, hb3]; exact h_ne)
          hfe1 hfe2 (by omega)
      · exact chunk_ne_mem_error fetch _ 8 dstA dstB srcA srcB offA2 offB2 target errorCode
          h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 he2
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf6)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf7)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf8)
          (by rw [hm2, hm1,
                  hreg2 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha2,
                  hreg2 srcB h_srcB_ne_dstA h_srcB_ne_dstB,
                  hreg1 srcB h_srcB_ne_dstA h_srcB_ne_dstB, hb2]; exact h_eq2)
          hfe1 hfe2 (by omega)
    · exact chunk_ne_mem_error fetch _ 11 dstA dstB srcA srcB offA1 offB1 target errorCode
        h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 he1
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf3)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf4)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf5)
        (by rw [hm1, hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha1,
                hreg1 srcB h_srcB_ne_dstA h_srcB_ne_dstB, hb1]; exact h_eq1)
        hfe1 hfe2 (by omega)
  · exact chunk_ne_mem_error fetch s 14 dstA dstB srcA srcB offA0 offB0 target errorCode
      h_ne_AB h_srcB_ne_dstA h_dstA_ne_r10 h_dstB_ne_r10 h_exit
      (by rw [h_pc]; exact hf0) (by rw [h_pc]; exact hf1) (by rw [h_pc]; exact hf2)
      (by rw [ha0, hb0]; exact h_eq0) hfe1 hfe2 (by omega)

set_option maxHeartbeats 8000000 in
theorem pubkey_compare_imm (fetch : Nat → Option Insn) (s : State)
    (dstA dstB srcA : Reg) (pc : Nat)
    (offA0 offA1 offA2 offA3 : Int) (val0 val1 val2 val3 : Int)
    (target : Nat) (errorCode : Int)
    (a0 a1 a2 a3 b0 b1 b2 b3 : Nat)
    (h_ne_AB : dstA ≠ dstB)
    (h_dstA_ne_r10 : dstA ≠ .r10) (h_dstB_ne_r10 : dstB ≠ .r10)
    (h_srcA_ne_dstA : srcA ≠ dstA) (h_srcA_ne_dstB : srcA ≠ dstB)
    (h_exit : s.exitCode = none) (h_pc : s.pc = pc)
    -- Chunks 0-2: ldx + lddw + jne; chunk 3: ldx + mov32 + jne
    (hf0 : fetch pc = some (.ldx .dword dstA srcA offA0))
    (hf1 : fetch (pc + 1) = some (.lddw dstB val0))
    (hf2 : fetch (pc + 2) = some (.jne dstA (.reg dstB) target))
    (hf3 : fetch (pc + 3) = some (.ldx .dword dstA srcA offA1))
    (hf4 : fetch (pc + 4) = some (.lddw dstB val1))
    (hf5 : fetch (pc + 5) = some (.jne dstA (.reg dstB) target))
    (hf6 : fetch (pc + 6) = some (.ldx .dword dstA srcA offA2))
    (hf7 : fetch (pc + 7) = some (.lddw dstB val2))
    (hf8 : fetch (pc + 8) = some (.jne dstA (.reg dstB) target))
    (hf9 : fetch (pc + 9) = some (.ldx .dword dstA srcA offA3))
    (hf10 : fetch (pc + 10) = some (.mov32 dstB (.imm val3)))
    (hf11 : fetch (pc + 11) = some (.jne dstA (.reg dstB) target))
    (hfe1 : fetch target = some (.mov32 .r0 (.imm errorCode)))
    (hfe2 : fetch (target + 1) = some .exit)
    (ha0 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA0) = a0)
    (ha1 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA1) = a1)
    (ha2 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA2) = a2)
    (ha3 : readU64 s.mem (effectiveAddr (s.regs.get srcA) offA3) = a3)
    (hb0 : toU64 val0 = b0) (hb1 : toU64 val1 = b1)
    (hb2 : toU64 val2 = b2) (hb3 : toU64 val3 % U32_MODULUS = b3)
    (h_ne : a0 ≠ b0 ∨ a1 ≠ b1 ∨ a2 ≠ b2 ∨ a3 ≠ b3) :
    (executeFn fetch s 14).exitCode = some (toU64 errorCode % U32_MODULUS) := by
  by_cases h_eq0 : a0 = b0
  · simp [h_eq0] at h_ne
    rw [show (14 : Nat) = 3 + 11 from rfl, executeFn_compose]
    obtain ⟨he1, hp1, hm1, hreg1⟩ := chunk_eq_imm fetch s dstA dstB srcA offA0 val0 target
      h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 h_exit
      (by rw [h_pc]; exact hf0) (by rw [h_pc]; exact hf1) (by rw [h_pc]; exact hf2)
      (by rw [ha0, hb0]; exact h_eq0)
    by_cases h_eq1 : a1 = b1
    · simp [h_eq1] at h_ne
      rw [show (11 : Nat) = 3 + 8 from rfl, executeFn_compose]
      obtain ⟨he2, hp2, hm2, hreg2⟩ := chunk_eq_imm fetch _ dstA dstB srcA offA1 val1 target
        h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 he1
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf3)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf4)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf5)
        (by rw [hm1, hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha1, hb1]; exact h_eq1)
      by_cases h_eq2 : a2 = b2
      · simp [h_eq2] at h_ne
        rw [show (8 : Nat) = 3 + 5 from rfl, executeFn_compose]
        obtain ⟨he3, hp3, hm3, hreg3⟩ := chunk_eq_imm fetch _ dstA dstB srcA offA2 val2 target
          h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 he2
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf6)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf7)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf8)
          (by rw [hm2, hm1,
                  hreg2 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha2, hb2]; exact h_eq2)
        exact chunk_ne_imm32_error fetch _ 5 dstA dstB srcA offA3 val3 target errorCode
          h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 he3
          (by simp only [hp3, hp2, hp1, h_pc, Nat.add_assoc]; exact hf9)
          (by simp only [hp3, hp2, hp1, h_pc, Nat.add_assoc]; exact hf10)
          (by simp only [hp3, hp2, hp1, h_pc, Nat.add_assoc]; exact hf11)
          (by rw [hm3, hm2, hm1,
                  hreg3 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg2 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha3, hb3]; exact h_ne)
          hfe1 hfe2 (by omega)
      · exact chunk_ne_imm_error fetch _ 8 dstA dstB srcA offA2 val2 target errorCode
          h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 he2
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf6)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf7)
          (by simp only [hp2, hp1, h_pc, Nat.add_assoc]; exact hf8)
          (by rw [hm2, hm1,
                  hreg2 srcA h_srcA_ne_dstA h_srcA_ne_dstB,
                  hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha2, hb2]; exact h_eq2)
          hfe1 hfe2 (by omega)
    · exact chunk_ne_imm_error fetch _ 11 dstA dstB srcA offA1 val1 target errorCode
        h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 he1
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf3)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf4)
        (by simp only [hp1, h_pc, Nat.add_assoc]; exact hf5)
        (by rw [hm1, hreg1 srcA h_srcA_ne_dstA h_srcA_ne_dstB, ha1, hb1]; exact h_eq1)
        hfe1 hfe2 (by omega)
  · exact chunk_ne_imm_error fetch s 14 dstA dstB srcA offA0 val0 target errorCode
      h_ne_AB h_dstA_ne_r10 h_dstB_ne_r10 h_exit
      (by rw [h_pc]; exact hf0) (by rw [h_pc]; exact hf1) (by rw [h_pc]; exact hf2)
      (by rw [ha0, hb0]; exact h_eq0) hfe1 hfe2 (by omega)

end QEDGen.Solana.SBPF
