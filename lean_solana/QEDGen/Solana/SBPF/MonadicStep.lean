-- Monadic instruction semantics for sBPF
--
-- Each sBPF instruction becomes a monadic action (SbpfM PUnit).
-- execInsn mirrors the existing `step` function but in monadic style.
-- execSegment provides multi-step monadic execution.

import QEDGen.Solana.SBPF.Monad

namespace QEDGen.Solana.SBPF

open Memory

/-! ## Monadic instruction execution

Each instruction is a direct function `State → PUnit × State` to keep
simp reduction fast. The structure mirrors `step` exactly. -/

@[simp] def execInsn (insn : Insn) : SbpfM PUnit := fun s =>
  let rf := s.regs
  let mem := s.mem
  let pc' := s.pc + 1
  match insn with

  | .lddw dst imm =>
    ((), { s with regs := rf.set dst (toU64 imm), pc := pc' })

  | .ldx w dst src off =>
    let addr := effectiveAddr (rf.get src) off
    let val := readByWidth mem addr w
    ((), { s with regs := rf.set dst val, pc := pc' })

  | .st w dst off imm =>
    let addr := effectiveAddr (rf.get dst) off
    let val := (toU64 imm) % (2 ^ (w.bytes * 8))
    ((), { s with mem := writeByWidth mem addr val w, pc := pc' })

  | .stx w dst off src =>
    let addr := effectiveAddr (rf.get dst) off
    let val := rf.get src % (2 ^ (w.bytes * 8))
    ((), { s with mem := writeByWidth mem addr val w, pc := pc' })

  | .add64 dst src =>
    ((), { s with regs := rf.set dst (wrapAdd (rf.get dst) (resolveSrc rf src)), pc := pc' })
  | .sub64 dst src =>
    ((), { s with regs := rf.set dst (wrapSub (rf.get dst) (resolveSrc rf src)), pc := pc' })
  | .mul64 dst src =>
    ((), { s with regs := rf.set dst (wrapMul (rf.get dst) (resolveSrc rf src)), pc := pc' })
  | .div64 dst src =>
    let b := resolveSrc rf src
    if b = 0 then ((), { s with exitCode := some ERR_DIVIDE_BY_ZERO })
    else ((), { s with regs := rf.set dst ((rf.get dst / b) % U64_MODULUS), pc := pc' })
  | .mod64 dst src =>
    let b := resolveSrc rf src
    if b = 0 then ((), { s with exitCode := some ERR_DIVIDE_BY_ZERO })
    else ((), { s with regs := rf.set dst (rf.get dst % b), pc := pc' })
  | .or64 dst src =>
    ((), { s with regs := rf.set dst ((rf.get dst ||| resolveSrc rf src) % U64_MODULUS), pc := pc' })
  | .and64 dst src =>
    ((), { s with regs := rf.set dst ((rf.get dst &&& resolveSrc rf src) % U64_MODULUS), pc := pc' })
  | .xor64 dst src =>
    ((), { s with regs := rf.set dst ((rf.get dst ^^^ resolveSrc rf src) % U64_MODULUS), pc := pc' })
  | .lsh64 dst src =>
    let shift := resolveSrc rf src % 64
    ((), { s with regs := rf.set dst ((rf.get dst <<< shift) % U64_MODULUS), pc := pc' })
  | .rsh64 dst src =>
    let shift := resolveSrc rf src % 64
    ((), { s with regs := rf.set dst (rf.get dst >>> shift), pc := pc' })
  | .arsh64 dst src =>
    let shift := resolveSrc rf src % 64
    let a := rf.get dst
    let v := if a < U64_MODULUS / 2 then a >>> shift
      else let shifted := a >>> shift
           let highBits := (U64_MODULUS - 1) - (U64_MODULUS / (2 ^ shift) - 1)
           (shifted ||| highBits) % U64_MODULUS
    ((), { s with regs := rf.set dst v, pc := pc' })
  | .mov64 dst src =>
    ((), { s with regs := rf.set dst (resolveSrc rf src), pc := pc' })
  | .neg64 dst =>
    ((), { s with regs := rf.set dst (wrapNeg (rf.get dst)), pc := pc' })

  -- 32-bit ALU
  | .add32 dst src =>
    ((), { s with regs := rf.set dst (wrapAdd32 (rf.get dst) (resolveSrc rf src)), pc := pc' })
  | .sub32 dst src =>
    ((), { s with regs := rf.set dst (wrapSub32 (rf.get dst) (resolveSrc rf src)), pc := pc' })
  | .mul32 dst src =>
    ((), { s with regs := rf.set dst (wrapMul32 (rf.get dst) (resolveSrc rf src)), pc := pc' })
  | .div32 dst src =>
    let b := resolveSrc rf src % U32_MODULUS
    if b = 0 then ((), { s with exitCode := some ERR_DIVIDE_BY_ZERO })
    else ((), { s with regs := rf.set dst ((rf.get dst % U32_MODULUS / b) % U32_MODULUS), pc := pc' })
  | .mod32 dst src =>
    let b := resolveSrc rf src % U32_MODULUS
    if b = 0 then ((), { s with exitCode := some ERR_DIVIDE_BY_ZERO })
    else ((), { s with regs := rf.set dst (rf.get dst % U32_MODULUS % b), pc := pc' })
  | .or32 dst src =>
    ((), { s with regs := rf.set dst ((rf.get dst ||| resolveSrc rf src) % U32_MODULUS), pc := pc' })
  | .and32 dst src =>
    ((), { s with regs := rf.set dst ((rf.get dst &&& resolveSrc rf src) % U32_MODULUS), pc := pc' })
  | .xor32 dst src =>
    ((), { s with regs := rf.set dst ((rf.get dst ^^^ resolveSrc rf src) % U32_MODULUS), pc := pc' })
  | .lsh32 dst src =>
    let shift := resolveSrc rf src % 32
    ((), { s with regs := rf.set dst ((rf.get dst <<< shift) % U32_MODULUS), pc := pc' })
  | .rsh32 dst src =>
    let shift := resolveSrc rf src % 32
    ((), { s with regs := rf.set dst ((rf.get dst % U32_MODULUS) >>> shift), pc := pc' })
  | .arsh32 dst src =>
    let shift := resolveSrc rf src % 32
    let a := rf.get dst % U32_MODULUS
    let v := if a < U32_MODULUS / 2 then a >>> shift
      else let shifted := a >>> shift
           let highBits := (U32_MODULUS - 1) - (U32_MODULUS / (2 ^ shift) - 1)
           (shifted ||| highBits) % U32_MODULUS
    ((), { s with regs := rf.set dst v, pc := pc' })
  | .mov32 dst src =>
    ((), { s with regs := rf.set dst (resolveSrc rf src % U32_MODULUS), pc := pc' })
  | .neg32 dst =>
    ((), { s with regs := rf.set dst (wrapNeg32 (rf.get dst)), pc := pc' })

  -- Conditional jumps
  | .jeq dst src target =>
    ((), { s with pc := if rf.get dst = resolveSrc rf src then target else pc' })
  | .jne dst src target =>
    ((), { s with pc := if rf.get dst ≠ resolveSrc rf src then target else pc' })
  | .jgt dst src target =>
    ((), { s with pc := if rf.get dst > resolveSrc rf src then target else pc' })
  | .jge dst src target =>
    ((), { s with pc := if rf.get dst ≥ resolveSrc rf src then target else pc' })
  | .jlt dst src target =>
    ((), { s with pc := if rf.get dst < resolveSrc rf src then target else pc' })
  | .jle dst src target =>
    ((), { s with pc := if rf.get dst ≤ resolveSrc rf src then target else pc' })
  | .jsgt dst src target =>
    ((), { s with pc := if toSigned64 (rf.get dst) > toSigned64 (resolveSrc rf src) then target else pc' })
  | .jsge dst src target =>
    ((), { s with pc := if toSigned64 (rf.get dst) ≥ toSigned64 (resolveSrc rf src) then target else pc' })
  | .jslt dst src target =>
    ((), { s with pc := if toSigned64 (rf.get dst) < toSigned64 (resolveSrc rf src) then target else pc' })
  | .jsle dst src target =>
    ((), { s with pc := if toSigned64 (rf.get dst) ≤ toSigned64 (resolveSrc rf src) then target else pc' })
  | .jset dst src target =>
    ((), { s with pc := if rf.get dst &&& resolveSrc rf src ≠ 0 then target else pc' })
  | .ja target =>
    ((), { s with pc := target })

  -- Syscall
  | .call syscall =>
    let s' := execSyscall syscall s
    ((), { s' with pc := pc' })

  -- Exit
  | .exit =>
    ((), { s with exitCode := some (rf.get .r0) })

/-! ## Bridge: execInsn ≡ step -/

/-- The monadic execInsn produces the same result as the pure step function. -/
theorem step_eq_execInsn (insn : Insn) (s : State) :
    step insn s = (execInsn insn s).2 := by
  cases insn <;> simp only [step, execInsn] <;> split <;> rfl

/-! ## Multi-step monadic execution -/

/-- Execute a program monadically using function-based fetch. -/
def execSegment (fetch : Nat → Option Insn) : Nat → SbpfM PUnit
  | 0 => fun s => ((), s)
  | fuel + 1 => fun s =>
    match s.exitCode with
    | some _ => ((), s)
    | none =>
      match fetch s.pc with
      | none => ((), { s with exitCode := some ERR_INVALID_PC })
      | some insn =>
        let (_, s') := execInsn insn s
        execSegment fetch fuel s'

theorem execSegment_halted (fetch : Nat → Option Insn) (n : Nat) (s : State)
    (h : s.exitCode = some c) :
    (execSegment fetch n s).2.exitCode = some c := by
  induction n with
  | zero => simp [execSegment, h]
  | succ n _ => simp [execSegment, h]

/-- When the current instruction is `exit`, execution terminates with r0 as exit code. -/
theorem execSegment_exit (fetch : Nat → Option Insn) (n : Nat) (s : State) (v : Nat)
    (h_none : s.exitCode = none)
    (h_fetch : fetch s.pc = some .exit)
    (h_r0 : s.regs.r0 = v) :
    (execSegment fetch (n + 1) s).2.exitCode = some v := by
  subst h_r0
  simp [execSegment, h_none, h_fetch, execInsn]
  exact execSegment_halted fetch n _ rfl

end QEDGen.Solana.SBPF
