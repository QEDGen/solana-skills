-- sBPF execution semantics: single-step and multi-step evaluation
--
-- Defines the machine state, register file, and the step function that gives
-- operational semantics to each sBPF instruction.

import QEDGen.Solana.SBPF.ISA
import QEDGen.Solana.SBPF.Memory

namespace QEDGen.Solana.SBPF

open Memory

/-! ## Register file as a concrete structure

Using named fields instead of a function `Reg → Nat` makes projections
trivially reducible by simp, avoiding the nested if-then-else chains that
cause timeouts in proofs. -/

structure RegFile where
  r0 : Nat := 0
  r1 : Nat := 0
  r2 : Nat := 0
  r3 : Nat := 0
  r4 : Nat := 0
  r5 : Nat := 0
  r6 : Nat := 0
  r7 : Nat := 0
  r8 : Nat := 0
  r9 : Nat := 0
  r10 : Nat := 0
  deriving Repr, Inhabited, DecidableEq, BEq

/-- Get a register value -/
@[simp] def RegFile.get (rf : RegFile) : Reg → Nat
  | .r0 => rf.r0 | .r1 => rf.r1 | .r2 => rf.r2 | .r3 => rf.r3
  | .r4 => rf.r4 | .r5 => rf.r5 | .r6 => rf.r6 | .r7 => rf.r7
  | .r8 => rf.r8 | .r9 => rf.r9 | .r10 => rf.r10

/-- Set a register value (r10 is read-only: writes are silently ignored) -/
@[simp] def RegFile.set (rf : RegFile) (r : Reg) (v : Nat) : RegFile :=
  match r with
  | .r0 => { rf with r0 := v } | .r1 => { rf with r1 := v }
  | .r2 => { rf with r2 := v } | .r3 => { rf with r3 := v }
  | .r4 => { rf with r4 := v } | .r5 => { rf with r5 := v }
  | .r6 => { rf with r6 := v } | .r7 => { rf with r7 := v }
  | .r8 => { rf with r8 := v } | .r9 => { rf with r9 := v }
  | .r10 => rf

/-- Writing to r10 is a no-op (frame pointer is read-only). -/
@[simp] theorem RegFile.set_r10 (rf : RegFile) (v : Nat) :
    rf.set .r10 v = rf := rfl

/-- Reading the register just written returns the written value (r0-r9 only). -/
theorem RegFile.get_set_self (rf : RegFile) (r : Reg) (v : Nat) (h : r ≠ .r10) :
    (rf.set r v).get r = v := by
  cases r <;> simp_all [RegFile.get, RegFile.set]

/-- Reading a different register from the one written returns the original value. -/
theorem RegFile.get_set_diff (rf : RegFile) (r1 r2 : Reg) (v : Nat) (h : r1 ≠ r2) :
    (rf.set r2 v).get r1 = rf.get r1 := by
  cases r1 <;> cases r2 <;> simp_all [RegFile.get, RegFile.set]

/-! ## Machine state -/

/-- sBPF machine state -/
structure State where
  /-- Register file -/
  regs : RegFile
  /-- Byte-addressable memory -/
  mem : Mem
  /-- Program counter: index into the instruction array -/
  pc : Nat
  /-- Exit status: None if running, Some n if exited with code n -/
  exitCode : Option Nat := none
  deriving Inhabited

/-- Is the machine still running? -/
def State.running (s : State) : Prop := s.exitCode = none

/-! ## Helpers -/

/-- Resolve a source operand to its unsigned 64-bit value -/
@[simp] def resolveSrc (rf : RegFile) (src : Src) : Nat :=
  match src with
  | .reg r => rf.get r
  | .imm v => toU64 v

/-! ## Runtime error codes

These are used when the VM encounters an unrecoverable error (not a program
exit via `exit` instruction). They must be non-zero to distinguish from
success. -/

def ERR_DIVIDE_BY_ZERO : Nat := 0xFFFFFFFFFFFFFFFE
def ERR_INVALID_PC     : Nat := 0xFFFFFFFFFFFFFFFF

/-! ## Wrapping 64-bit arithmetic -/

@[simp] def wrapAdd (a b : Nat) : Nat := (a + b) % U64_MODULUS
@[simp] def wrapSub (a b : Nat) : Nat := (a + U64_MODULUS - b % U64_MODULUS) % U64_MODULUS
@[simp] def wrapMul (a b : Nat) : Nat := (a * b) % U64_MODULUS
@[simp] def wrapNeg (a : Nat) : Nat := (U64_MODULUS - a % U64_MODULUS) % U64_MODULUS

/-- 32-bit modulus for 32-bit ALU operations -/
def U32_MODULUS : Nat := 2 ^ 32

@[simp] def wrapAdd32 (a b : Nat) : Nat := (a + b) % U32_MODULUS
@[simp] def wrapSub32 (a b : Nat) : Nat := (a + U32_MODULUS - b % U32_MODULUS) % U32_MODULUS
@[simp] def wrapMul32 (a b : Nat) : Nat := (a * b) % U32_MODULUS
@[simp] def wrapNeg32 (a : Nat) : Nat := (U32_MODULUS - a % U32_MODULUS) % U32_MODULUS

/-! ## Syscall execution -/

/-- Execute a syscall. Default: set r0 = 0 (success). -/
@[simp] def execSyscall (sc : Syscall) (s : State) : State :=
  match sc with
  | .sol_log_ | .sol_log_64_ | .sol_log_compute_units_
  | .sol_log_pubkey | .sol_log_data =>
    { s with regs := s.regs.set .r0 0 }
  | .sol_remaining_compute_units | .sol_get_stack_height =>
    s  -- r0 gets an opaque runtime value
  | _ => { s with regs := s.regs.set .r0 0 }

/-! ## Single-step semantics -/

/-- Execute one instruction, returning the new state. -/
@[simp] def step (insn : Insn) (s : State) : State :=
  let rf := s.regs
  let mem := s.mem
  let pc' := s.pc + 1
  match insn with

  | .lddw dst imm =>
    { s with regs := rf.set dst (toU64 imm), pc := pc' }

  | .ldx w dst src off =>
    let addr := effectiveAddr (rf.get src) off
    let val := readByWidth mem addr w
    { s with regs := rf.set dst val, pc := pc' }

  | .st w dst off imm =>
    let addr := effectiveAddr (rf.get dst) off
    let val := (toU64 imm) % (2 ^ (w.bytes * 8))
    { s with mem := writeByWidth mem addr val w, pc := pc' }

  | .stx w dst off src =>
    let addr := effectiveAddr (rf.get dst) off
    let val := rf.get src % (2 ^ (w.bytes * 8))
    { s with mem := writeByWidth mem addr val w, pc := pc' }

  | .add64 dst src =>
    { s with regs := rf.set dst (wrapAdd (rf.get dst) (resolveSrc rf src)), pc := pc' }
  | .sub64 dst src =>
    { s with regs := rf.set dst (wrapSub (rf.get dst) (resolveSrc rf src)), pc := pc' }
  | .mul64 dst src =>
    { s with regs := rf.set dst (wrapMul (rf.get dst) (resolveSrc rf src)), pc := pc' }
  | .div64 dst src =>
    let b := resolveSrc rf src
    if b = 0 then { s with exitCode := some ERR_DIVIDE_BY_ZERO }
    else { s with regs := rf.set dst ((rf.get dst / b) % U64_MODULUS), pc := pc' }
  | .mod64 dst src =>
    let b := resolveSrc rf src
    if b = 0 then { s with exitCode := some ERR_DIVIDE_BY_ZERO }
    else { s with regs := rf.set dst (rf.get dst % b), pc := pc' }
  | .or64 dst src =>
    { s with regs := rf.set dst ((rf.get dst ||| resolveSrc rf src) % U64_MODULUS), pc := pc' }
  | .and64 dst src =>
    { s with regs := rf.set dst ((rf.get dst &&& resolveSrc rf src) % U64_MODULUS), pc := pc' }
  | .xor64 dst src =>
    { s with regs := rf.set dst ((rf.get dst ^^^ resolveSrc rf src) % U64_MODULUS), pc := pc' }
  | .lsh64 dst src =>
    let shift := resolveSrc rf src % 64
    { s with regs := rf.set dst ((rf.get dst <<< shift) % U64_MODULUS), pc := pc' }
  | .rsh64 dst src =>
    let shift := resolveSrc rf src % 64
    { s with regs := rf.set dst (rf.get dst >>> shift), pc := pc' }
  | .arsh64 dst src =>
    let shift := resolveSrc rf src % 64
    let a := rf.get dst
    let v := if a < U64_MODULUS / 2 then a >>> shift
      else let shifted := a >>> shift
           let highBits := (U64_MODULUS - 1) - (U64_MODULUS / (2 ^ shift) - 1)
           (shifted ||| highBits) % U64_MODULUS
    { s with regs := rf.set dst v, pc := pc' }
  | .mov64 dst src =>
    { s with regs := rf.set dst (resolveSrc rf src), pc := pc' }
  | .neg64 dst =>
    { s with regs := rf.set dst (wrapNeg (rf.get dst)), pc := pc' }

  -- 32-bit ALU: result zero-extended to 64 bits
  | .add32 dst src =>
    { s with regs := rf.set dst (wrapAdd32 (rf.get dst) (resolveSrc rf src)), pc := pc' }
  | .sub32 dst src =>
    { s with regs := rf.set dst (wrapSub32 (rf.get dst) (resolveSrc rf src)), pc := pc' }
  | .mul32 dst src =>
    { s with regs := rf.set dst (wrapMul32 (rf.get dst) (resolveSrc rf src)), pc := pc' }
  | .div32 dst src =>
    let b := resolveSrc rf src % U32_MODULUS
    if b = 0 then { s with exitCode := some ERR_DIVIDE_BY_ZERO }
    else { s with regs := rf.set dst ((rf.get dst % U32_MODULUS / b) % U32_MODULUS), pc := pc' }
  | .mod32 dst src =>
    let b := resolveSrc rf src % U32_MODULUS
    if b = 0 then { s with exitCode := some ERR_DIVIDE_BY_ZERO }
    else { s with regs := rf.set dst (rf.get dst % U32_MODULUS % b), pc := pc' }
  | .or32 dst src =>
    { s with regs := rf.set dst ((rf.get dst ||| resolveSrc rf src) % U32_MODULUS), pc := pc' }
  | .and32 dst src =>
    { s with regs := rf.set dst ((rf.get dst &&& resolveSrc rf src) % U32_MODULUS), pc := pc' }
  | .xor32 dst src =>
    { s with regs := rf.set dst ((rf.get dst ^^^ resolveSrc rf src) % U32_MODULUS), pc := pc' }
  | .lsh32 dst src =>
    let shift := resolveSrc rf src % 32
    { s with regs := rf.set dst ((rf.get dst <<< shift) % U32_MODULUS), pc := pc' }
  | .rsh32 dst src =>
    let shift := resolveSrc rf src % 32
    { s with regs := rf.set dst ((rf.get dst % U32_MODULUS) >>> shift), pc := pc' }
  | .arsh32 dst src =>
    let shift := resolveSrc rf src % 32
    let a := rf.get dst % U32_MODULUS
    let v := if a < U32_MODULUS / 2 then a >>> shift
      else let shifted := a >>> shift
           let highBits := (U32_MODULUS - 1) - (U32_MODULUS / (2 ^ shift) - 1)
           (shifted ||| highBits) % U32_MODULUS
    { s with regs := rf.set dst v, pc := pc' }
  | .mov32 dst src =>
    { s with regs := rf.set dst (resolveSrc rf src % U32_MODULUS), pc := pc' }
  | .neg32 dst =>
    { s with regs := rf.set dst (wrapNeg32 (rf.get dst)), pc := pc' }

  | .jeq dst src target =>
    { s with pc := if rf.get dst = resolveSrc rf src then target else pc' }
  | .jne dst src target =>
    { s with pc := if rf.get dst ≠ resolveSrc rf src then target else pc' }
  | .jgt dst src target =>
    { s with pc := if rf.get dst > resolveSrc rf src then target else pc' }
  | .jge dst src target =>
    { s with pc := if rf.get dst ≥ resolveSrc rf src then target else pc' }
  | .jlt dst src target =>
    { s with pc := if rf.get dst < resolveSrc rf src then target else pc' }
  | .jle dst src target =>
    { s with pc := if rf.get dst ≤ resolveSrc rf src then target else pc' }
  | .jsgt dst src target =>
    { s with pc := if toSigned64 (rf.get dst) > toSigned64 (resolveSrc rf src) then target else pc' }
  | .jsge dst src target =>
    { s with pc := if toSigned64 (rf.get dst) ≥ toSigned64 (resolveSrc rf src) then target else pc' }
  | .jslt dst src target =>
    { s with pc := if toSigned64 (rf.get dst) < toSigned64 (resolveSrc rf src) then target else pc' }
  | .jsle dst src target =>
    { s with pc := if toSigned64 (rf.get dst) ≤ toSigned64 (resolveSrc rf src) then target else pc' }
  | .jset dst src target =>
    { s with pc := if rf.get dst &&& resolveSrc rf src ≠ 0 then target else pc' }
  | .ja target =>
    { s with pc := target }

  | .call syscall =>
    let s' := execSyscall syscall s
    { s' with pc := pc' }

  | .exit =>
    { s with exitCode := some (rf.get .r0) }

/-! ## Multi-step execution -/

abbrev Program := Array Insn

/-- Execute a program from the current state, consuming up to `fuel` steps. -/
def execute (prog : Program) (s : State) (fuel : Nat) : State :=
  match fuel with
  | 0 => s
  | fuel' + 1 =>
    match s.exitCode with
    | some _ => s
    | none =>
      match prog[s.pc]? with
      | none => { s with exitCode := some ERR_INVALID_PC }
      | some insn => execute prog (step insn s) fuel'

/-- Execute using a function-based instruction fetch (O(1) per step).
    Use this for large programs where Array.get? is too expensive for simp. -/
def executeFn (fetch : Nat → Option Insn) (s : State) (fuel : Nat) : State :=
  match fuel with
  | 0 => s
  | fuel' + 1 =>
    match s.exitCode with
    | some _ => s
    | none =>
      match fetch s.pc with
      | none => { s with exitCode := some ERR_INVALID_PC }
      | some insn => executeFn fetch (step insn s) fuel'

/-- Create an initial machine state with r1 pointing to the input buffer -/
@[simp] def initState (inputAddr : Nat) (mem : Mem) : State where
  regs := { r1 := inputAddr, r10 := STACK_START + 0x1000 }
  mem := mem
  pc := 0

/-- Two-pointer initial state for SIMD-0321 programs.
    r1 = input buffer, r2 = instruction data pointer.
    `entryPc` allows starting at a non-zero entrypoint (e.g. when error
    handlers are laid out before the entrypoint). -/
@[simp] def initState2 (inputAddr insnAddr : Nat) (mem : Mem) (entryPc : Nat := 0) : State where
  regs := { r1 := inputAddr, r2 := insnAddr, r10 := STACK_START + 0x1000 }
  mem := mem
  pc := entryPc

/-! ## Execution unrolling lemmas -/

@[simp] theorem execute_halted (prog : Program) (s : State) (n : Nat) (code : Nat)
    (h : s.exitCode = some code) :
    execute prog s n = s := by
  cases n with
  | zero => simp [execute]
  | succ n => simp [execute, h]

theorem execute_step (prog : Program) (s : State) (n : Nat) (insn : Insn)
    (h_running : s.exitCode = none)
    (h_fetch : prog[s.pc]? = some insn) :
    execute prog s (n + 1) = execute prog (step insn s) n := by
  simp [execute, h_running, h_fetch]

@[simp] theorem execute_zero (prog : Program) (s : State) :
    execute prog s 0 = s := by
  simp [execute]

-- Function-based execution lemmas (for large programs)

@[simp] theorem executeFn_halted (fetch : Nat → Option Insn) (s : State) (n : Nat) (code : Nat)
    (h : s.exitCode = some code) :
    executeFn fetch s n = s := by
  cases n with
  | zero => simp [executeFn]
  | succ n => simp [executeFn, h]

@[simp] theorem executeFn_zero (fetch : Nat → Option Insn) (s : State) :
    executeFn fetch s 0 = s := by
  simp [executeFn]

theorem executeFn_step (fetch : Nat → Option Insn) (s : State) (n : Nat) (insn : Insn)
    (h_running : s.exitCode = none)
    (h_fetch : fetch s.pc = some insn) :
    executeFn fetch s (n + 1) = executeFn fetch (step insn s) n := by
  simp [executeFn, h_running, h_fetch]

/-- Composability of deterministic execution: running n+m steps is the same as
    running n steps then running m steps from the resulting state. -/
theorem executeFn_compose (fetch : Nat → Option Insn) (s : State) (n m : Nat) :
    executeFn fetch s (n + m) = executeFn fetch (executeFn fetch s n) m := by
  induction n generalizing s with
  | zero => simp [executeFn]
  | succ n ih =>
    rw [Nat.succ_add]
    simp only [executeFn]
    split
    · -- halted: exitCode = some _
      rename_i h_halted
      simp [executeFn_halted, h_halted]
    · -- running: exitCode = none
      split
      · -- invalid PC: fetch returns none → sets exitCode, then halted for m steps
        simp [executeFn_halted]
      · -- valid instruction
        rename_i insn h_fetch
        exact ih (step insn s)

/-- Step N times from a state, applying instructions from a list -/
def stepN : List Insn → State → State
  | [], s => s
  | i :: is, s => stepN is (step i s)

end QEDGen.Solana.SBPF
