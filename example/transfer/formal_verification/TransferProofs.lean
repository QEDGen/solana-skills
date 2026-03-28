-- Formal verification of the DASMAC transfer program (validation guards)
--
-- Source: transfer.s — a SOL transfer program that validates inputs,
-- constructs a System Program Transfer CPI, and invokes it.
--
-- We verify the validation prefix: 7 input checks + balance check.

import QEDGen.Solana.SBPF.ISA
import QEDGen.Solana.SBPF.Memory
import QEDGen.Solana.SBPF.Execute

namespace TransferProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

/-! ## Program transcription (validation prefix + error handlers) -/

def prog : Program := #[
  .ldx .dword .r2 .r1 0,          -- 0:  r2 = num_accounts
  .jne .r2 (.imm 3) 16,           -- 1:  if r2 ≠ 3 → error 1
  .ldx .dword .r2 .r1 88,         -- 2:  r2 = sender_data_length
  .jne .r2 (.imm 0) 18,           -- 3:  if r2 ≠ 0 → error 2
  .ldx .byte .r2 .r1 10344,       -- 4:  r2 = recipient dup marker
  .jne .r2 (.imm 0xff) 20,        -- 5:  if r2 ≠ 0xff → error 3
  .ldx .dword .r2 .r1 10424,      -- 6:  r2 = recipient_data_length
  .jne .r2 (.imm 0) 22,           -- 7:  if r2 ≠ 0 → error 4
  .ldx .byte .r2 .r1 20680,       -- 8:  r2 = sysprog dup marker
  .jne .r2 (.imm 0xff) 24,        -- 9:  if r2 ≠ 0xff → error 5
  .ldx .dword .r4 .r1 31032,      -- 10: r4 = insn_data_length
  .jne .r4 (.imm 8) 26,           -- 11: if r4 ≠ 8 → error 6
  .ldx .dword .r4 .r1 31040,      -- 12: r4 = transfer_amount
  .ldx .dword .r2 .r1 80,         -- 13: r2 = sender_lamports
  .jlt .r2 (.reg .r4) 28,         -- 14: if lamports < amount → error 7
  .exit,                            -- 15: success (r0 = 0)
  .mov64 .r0 (.imm 1), .exit,      -- 16-17: error 1
  .mov64 .r0 (.imm 2), .exit,      -- 18-19: error 2
  .mov64 .r0 (.imm 3), .exit,      -- 20-21: error 3
  .mov64 .r0 (.imm 4), .exit,      -- 22-23: error 4
  .mov64 .r0 (.imm 5), .exit,      -- 24-25: error 5
  .mov64 .r0 (.imm 6), .exit,      -- 26-27: error 6
  .mov64 .r0 (.imm 7), .exit       -- 28-29: error 7
]

/-! ## Fetch lemmas -/

private theorem f0  : prog[0]?  = some (.ldx .dword .r2 .r1 0) := by native_decide
private theorem f1  : prog[1]?  = some (.jne .r2 (.imm 3) 16) := by native_decide
private theorem f2  : prog[2]?  = some (.ldx .dword .r2 .r1 88) := by native_decide
private theorem f3  : prog[3]?  = some (.jne .r2 (.imm 0) 18) := by native_decide
private theorem f4  : prog[4]?  = some (.ldx .byte .r2 .r1 10344) := by native_decide
private theorem f5  : prog[5]?  = some (.jne .r2 (.imm 0xff) 20) := by native_decide
private theorem f6  : prog[6]?  = some (.ldx .dword .r2 .r1 10424) := by native_decide
private theorem f7  : prog[7]?  = some (.jne .r2 (.imm 0) 22) := by native_decide
private theorem f8  : prog[8]?  = some (.ldx .byte .r2 .r1 20680) := by native_decide
private theorem f9  : prog[9]?  = some (.jne .r2 (.imm 0xff) 24) := by native_decide
private theorem f10 : prog[10]? = some (.ldx .dword .r4 .r1 31032) := by native_decide
private theorem f11 : prog[11]? = some (.jne .r4 (.imm 8) 26) := by native_decide
private theorem f12 : prog[12]? = some (.ldx .dword .r4 .r1 31040) := by native_decide
private theorem f13 : prog[13]? = some (.ldx .dword .r2 .r1 80) := by native_decide
private theorem f14 : prog[14]? = some (.jlt .r2 (.reg .r4) 28) := by native_decide
private theorem f15 : prog[15]? = some .exit := by native_decide
private theorem f16 : prog[16]? = some (.mov64 .r0 (.imm 1)) := by native_decide
private theorem f17 : prog[17]? = some .exit := by native_decide
private theorem f28 : prog[28]? = some (.mov64 .r0 (.imm 7)) := by native_decide
private theorem f29 : prog[29]? = some .exit := by native_decide

/-! ## P1: wrong account count → error 1

   Symbolic proof: numAccounts ≠ 3 → exit code 1 in 4 steps. -/

set_option maxHeartbeats 1600000 in
theorem rejects_wrong_account_count
    (inputAddr : Nat) (mem : Mem)
    (numAccounts : Nat)
    (h_num : readU64 mem inputAddr = numAccounts)
    (h_ne : numAccounts ≠ 3) :
    (execute prog (initState inputAddr mem) 6).exitCode = some 1 := by
  have h_ne3 : ¬(readU64 mem inputAddr = 3) := by rw [h_num]; exact h_ne
  -- Step 0: ldx dword r2, [r1+0]
  rw [show (6:Nat) = 5+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 0) (by rfl) (by simp [initState]; exact f0)]
  -- Step 1: jne r2, 3, 16
  rw [show (5:Nat) = 4+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 3) 16)
        (by simp [step, initState])
        (by simp [step, initState]; exact f1)]
  -- Step 2: mov64 r0, 1 (at PC=16, since numAccounts ≠ 3)
  rw [show (4:Nat) = 3+1 from rfl,
      execute_step _ _ _ (.mov64 .r0 (.imm 1))
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_ne3]; exact f16)]
  -- Step 3: exit (at PC=17)
  rw [show (3:Nat) = 2+1 from rfl,
      execute_step _ _ _ .exit
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_ne3]; exact f17)]
  -- Halted: exitCode = some 1
  simp [execute_halted, step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_ne3]

/-! ## P2: insufficient lamports → error 7

   All 7 prior checks pass (concrete values), balance check fails. -/

set_option maxHeartbeats 12800000 in
theorem rejects_insufficient_lamports
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = 3)
    (h_sdl   : readU64 mem (effectiveAddr inputAddr 88) = 0)
    (h_rdup  : readU8  mem (effectiveAddr inputAddr 10344) = 0xff)
    (h_rdl   : readU64 mem (effectiveAddr inputAddr 10424) = 0)
    (h_sdup  : readU8  mem (effectiveAddr inputAddr 20680) = 0xff)
    (h_idl   : readU64 mem (effectiveAddr inputAddr 31032) = 8)
    (h_amt   : readU64 mem (effectiveAddr inputAddr 31040) = amount)
    (h_bal   : readU64 mem (effectiveAddr inputAddr 80) = senderLamports)
    (h_insuf : senderLamports < amount) :
    (execute prog (initState inputAddr mem) 20).exitCode = some 7 := by
  -- Normalize effectiveAddr in hypotheses so addresses match step computation
  simp only [effectiveAddr] at h_sdl h_rdup h_rdl h_sdup h_idl h_amt h_bal
  -- Step 0: ldx dword r2, [r1+0] → r2 = 3
  rw [show (20:Nat) = 19+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 0) (by rfl) (by simp [initState]; exact f0)]
  -- Step 1: jne r2, 3, 16 → falls through (3=3)
  rw [show (19:Nat) = 18+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 3) 16)
        (by simp [step, initState])
        (by simp [step, initState]; exact f1)]
  -- Step 2: ldx dword r2, [r1+88] → r2 = 0
  rw [show (18:Nat) = 17+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 88)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num]; exact f2)]
  -- Step 3: jne r2, 0, 18 → falls through (0=0)
  rw [show (17:Nat) = 16+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0) 18)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl]; exact f3)]
  -- Step 4: ldx byte r2, [r1+10344] → r2 = 0xff
  rw [show (16:Nat) = 15+1 from rfl,
      execute_step _ _ _ (.ldx .byte .r2 .r1 10344)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl]; exact f4)]
  -- Step 5: jne r2, 0xff, 20 → falls through (0xff=0xff)
  rw [show (15:Nat) = 14+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0xff) 20)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup]; exact f5)]
  -- Step 6: ldx dword r2, [r1+10424] → r2 = 0
  rw [show (14:Nat) = 13+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 10424)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup]; exact f6)]
  -- Step 7: jne r2, 0, 22 → falls through (0=0)
  rw [show (13:Nat) = 12+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0) 22)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl]; exact f7)]
  -- Step 8: ldx byte r2, [r1+20680] → r2 = 0xff
  rw [show (12:Nat) = 11+1 from rfl,
      execute_step _ _ _ (.ldx .byte .r2 .r1 20680)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl]; exact f8)]
  -- Step 9: jne r2, 0xff, 24 → falls through (0xff=0xff)
  rw [show (11:Nat) = 10+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0xff) 24)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup]; exact f9)]
  -- Step 10: ldx dword r4, [r1+31032] → r4 = 8
  rw [show (10:Nat) = 9+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r4 .r1 31032)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup]; exact f10)]
  -- Step 11: jne r4, 8, 26 → falls through (8=8)
  rw [show (9:Nat) = 8+1 from rfl,
      execute_step _ _ _ (.jne .r4 (.imm 8) 26)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f11)]
  -- Step 12: ldx dword r4, [r1+31040] → r4 = amount
  rw [show (8:Nat) = 7+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r4 .r1 31040)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f12)]
  -- Step 13: ldx dword r2, [r1+80] → r2 = senderLamports
  rw [show (7:Nat) = 6+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 80)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f13)]
  -- Step 14: jlt r2, r4, 28 → jumps (senderLamports < amount)
  rw [show (6:Nat) = 5+1 from rfl,
      execute_step _ _ _ (.jlt .r2 (.reg .r4) 28)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f14)]
  -- Step 15: mov64 r0, 7 (at PC=28)
  rw [show (5:Nat) = 4+1 from rfl,
      execute_step _ _ _ (.mov64 .r0 (.imm 7))
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl, h_amt, h_bal, h_insuf]; exact f28)]
  -- Step 16: exit (at PC=29)
  rw [show (4:Nat) = 3+1 from rfl,
      execute_step _ _ _ .exit
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl, h_amt, h_bal, h_insuf]; exact f29)]
  -- Halted: exitCode = some 7
  simp [execute_halted, step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr,
        h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl, h_amt, h_bal, h_insuf]

/-! ## P3: happy path → exit 0

   All checks pass, balance sufficient → normal exit. -/

set_option maxHeartbeats 12800000 in
theorem accepts_valid_transfer
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = 3)
    (h_sdl   : readU64 mem (effectiveAddr inputAddr 88) = 0)
    (h_rdup  : readU8  mem (effectiveAddr inputAddr 10344) = 0xff)
    (h_rdl   : readU64 mem (effectiveAddr inputAddr 10424) = 0)
    (h_sdup  : readU8  mem (effectiveAddr inputAddr 20680) = 0xff)
    (h_idl   : readU64 mem (effectiveAddr inputAddr 31032) = 8)
    (h_amt   : readU64 mem (effectiveAddr inputAddr 31040) = amount)
    (h_bal   : readU64 mem (effectiveAddr inputAddr 80) = senderLamports)
    (h_suf   : senderLamports ≥ amount) :
    (execute prog (initState inputAddr mem) 20).exitCode = some 0 := by
  simp only [effectiveAddr] at h_sdl h_rdup h_rdl h_sdup h_idl h_amt h_bal
  have h_not_lt : ¬(senderLamports < amount) := by omega
  -- Steps 0-13: identical validation prefix (all checks pass)
  rw [show (20:Nat) = 19+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 0) (by rfl) (by simp [initState]; exact f0)]
  rw [show (19:Nat) = 18+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 3) 16)
        (by simp [step, initState])
        (by simp [step, initState]; exact f1)]
  rw [show (18:Nat) = 17+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 88)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num]; exact f2)]
  rw [show (17:Nat) = 16+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0) 18)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl]; exact f3)]
  rw [show (16:Nat) = 15+1 from rfl,
      execute_step _ _ _ (.ldx .byte .r2 .r1 10344)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl]; exact f4)]
  rw [show (15:Nat) = 14+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0xff) 20)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup]; exact f5)]
  rw [show (14:Nat) = 13+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 10424)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup]; exact f6)]
  rw [show (13:Nat) = 12+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0) 22)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl]; exact f7)]
  rw [show (12:Nat) = 11+1 from rfl,
      execute_step _ _ _ (.ldx .byte .r2 .r1 20680)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl]; exact f8)]
  rw [show (11:Nat) = 10+1 from rfl,
      execute_step _ _ _ (.jne .r2 (.imm 0xff) 24)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup]; exact f9)]
  rw [show (10:Nat) = 9+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r4 .r1 31032)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup]; exact f10)]
  rw [show (9:Nat) = 8+1 from rfl,
      execute_step _ _ _ (.jne .r4 (.imm 8) 26)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f11)]
  rw [show (8:Nat) = 7+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r4 .r1 31040)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f12)]
  rw [show (7:Nat) = 6+1 from rfl,
      execute_step _ _ _ (.ldx .dword .r2 .r1 80)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f13)]
  -- Step 14: jlt r2, r4, 28 → falls through (senderLamports ≥ amount)
  rw [show (6:Nat) = 5+1 from rfl,
      execute_step _ _ _ (.jlt .r2 (.reg .r4) 28)
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl]; exact f14)]
  -- Step 15: exit (at PC=15, r0=0)
  rw [show (5:Nat) = 4+1 from rfl,
      execute_step _ _ _ .exit
        (by simp [step, initState])
        (by simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr, h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl, h_amt, h_bal, h_not_lt]; exact f15)]
  -- Halted: exitCode = some 0
  simp [execute_halted, step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, effectiveAddr,
        h_num, h_sdl, h_rdup, h_rdl, h_sdup, h_idl, h_amt, h_bal, h_not_lt]

end TransferProofs
