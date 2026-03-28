-- Formal verification of the asm-slippage program
--
-- Source: asm-slippage.s — a slippage guard that rejects transactions
-- when the token balance drops below a minimum threshold.

import QEDGen.Solana.SBPF.ISA
import QEDGen.Solana.SBPF.Memory
import QEDGen.Solana.SBPF.Execute

namespace SlippageProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

/-! ## Program transcription

Translate asm-slippage.s into a Program array. Jump targets are absolute
instruction indices: `end` label maps to index 4. -/

def prog : Program := #[
  .ldx .dword .r3 .r1 0x2918,   -- 0: r3 = minimum_balance
  .ldx .dword .r4 .r1 0x00a0,   -- 1: r4 = token_account_balance
  .jge .r3 (.reg .r4) 4,        -- 2: if min >= bal, jump to error (index 4)
  .exit,                          -- 3: success (r0 = 0)
  .lddw .r1 0,                   -- 4: error msg addr
  .lddw .r2 17,                  -- 5: error msg len
  .call .sol_log_,                -- 6: log error
  .lddw .r0 1,                   -- 7: set error code
  .exit                           -- 8: error exit
]

/-! ## Instruction fetch lemmas

Pre-computed for each instruction index. These are closed terms so
native_decide can evaluate them. -/

private theorem f0 : prog[0]? = some (.ldx .dword .r3 .r1 0x2918) := by native_decide
private theorem f1 : prog[1]? = some (.ldx .dword .r4 .r1 0x00a0) := by native_decide
private theorem f2 : prog[2]? = some (.jge .r3 (.reg .r4) 4) := by native_decide
private theorem f3 : prog[3]? = some .exit := by native_decide
private theorem f4 : prog[4]? = some (.lddw .r1 0) := by native_decide
private theorem f5 : prog[5]? = some (.lddw .r2 17) := by native_decide
private theorem f6 : prog[6]? = some (.call .sol_log_) := by native_decide
private theorem f7 : prog[7]? = some (.lddw .r0 1) := by native_decide
private theorem f8 : prog[8]? = some .exit := by native_decide

/-! ## Property P1: slippage rejection

SPEC.md §3.1 P1: When minimum_balance >= token_account_balance,
the program MUST exit with code 1. -/

set_option maxHeartbeats 3200000 in
theorem rejects_insufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (effectiveAddr inputAddr 0x2918) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr 0x00a0) = tokenBal)
    (h_slip : minBal ≥ tokenBal) :
    (execute prog (initState inputAddr mem) 10).exitCode = some 1 := by
  simp only [effectiveAddr] at h_min h_tok
  -- Step 0: ldxdw r3 — PC:0→1
  rw [show (10:Nat) = 9+1 from rfl, execute_step _ _ _ (.ldx .dword .r3 .r1 0x2918)
    (by rfl) (by simp [initState]; exact f0)]
  -- Step 1: ldxdw r4 — PC:1→2
  rw [show (9:Nat) = 8+1 from rfl, execute_step _ _ _ (.ldx .dword .r4 .r1 0x00a0)
    (by simp [step, initState]) (by simp [step, initState]; exact f1)]
  -- Step 2: jge r3, r4, 4 — branch taken, PC:2→4
  rw [show (8:Nat) = 7+1 from rfl, execute_step _ _ _ (.jge .r3 (.reg .r4) 4)
    (by simp [step, initState]) (by simp [step, initState]; exact f2)]
  -- Step 3: lddw r1, 0 — PC:4→5
  rw [show (7:Nat) = 6+1 from rfl, execute_step _ _ _ (.lddw .r1 0)
    (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc])
    (by simp [step, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]; exact f4)]
  -- Step 4: lddw r2, 17 — PC:5→6
  rw [show (6:Nat) = 5+1 from rfl, execute_step _ _ _ (.lddw .r2 17)
    (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte])
    (by simp [step, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]; exact f5)]
  -- Step 5: call sol_log_ — PC:6→7
  rw [show (5:Nat) = 4+1 from rfl, execute_step _ _ _ (.call .sol_log_)
    (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte])
    (by simp [step, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]; exact f6)]
  -- Step 6: lddw r0, 1 — PC:7→8
  rw [show (4:Nat) = 3+1 from rfl, execute_step _ _ _ (.lddw .r0 1)
    (by simp [step, execSyscall, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte])
    (by simp [step, execSyscall, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]; exact f7)]
  -- Step 7: exit — exitCode = some 1
  rw [show (3:Nat) = 2+1 from rfl, execute_step _ _ _ .exit
    (by simp [step, execSyscall, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte])
    (by simp [step, execSyscall, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
              h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]; exact f8)]
  -- Halted with exit code 1
  simp [execute_halted, step, execSyscall, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth,
        effectiveAddr, h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]

/-! ## Property P2: slippage acceptance

SPEC.md §3.1 P2: When minimum_balance < token_account_balance,
the program MUST exit with code 0. -/

set_option maxHeartbeats 1600000 in
theorem accepts_sufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (effectiveAddr inputAddr 0x2918) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr 0x00a0) = tokenBal)
    (h_ok : minBal < tokenBal) :
    (execute prog (initState inputAddr mem) 10).exitCode = some 0 := by
  simp only [effectiveAddr] at h_min h_tok
  have h_not_ge : ¬(minBal ≥ tokenBal) := by omega
  -- Step 0: ldxdw r3
  rw [show (10:Nat) = 9+1 from rfl, execute_step _ _ _ (.ldx .dword .r3 .r1 0x2918)
    (by rfl) (by simp [initState]; exact f0)]
  -- Step 1: ldxdw r4
  rw [show (9:Nat) = 8+1 from rfl, execute_step _ _ _ (.ldx .dword .r4 .r1 0x00a0)
    (by simp [step, initState]) (by simp [step, initState]; exact f1)]
  -- Step 2: jge — branch NOT taken, PC:2→3
  rw [show (8:Nat) = 7+1 from rfl, execute_step _ _ _ (.jge .r3 (.reg .r4) 4)
    (by simp [step, initState]) (by simp [step, initState]; exact f2)]
  -- Step 3: exit — exitCode = some 0
  rw [show (7:Nat) = 6+1 from rfl, execute_step _ _ _ .exit
    (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc])
    (by simp [step, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
              h_min, h_tok, ge_iff_le, h_not_ge, ↓reduceIte]; exact f3)]
  -- Halted with exit code 0
  simp [execute_halted, step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth,
        effectiveAddr, h_min, h_tok, ge_iff_le, h_not_ge, ↓reduceIte]

end SlippageProofs
