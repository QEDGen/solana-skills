-- Formal verification of the DASMAC tree program (validation guards)
--
-- Source: tree.s — a Solana red-black tree with initialize, insert, and
-- remove operations. 498 instructions, 162 constants.
--
-- We verify the validation prefix: discriminator dispatch, instruction
-- data length, and account count checks.

import QEDGen.Solana.SBPF
import TreeProg

namespace TreeProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open TreeProg

set_option maxRecDepth 4096

/-! ## Initial state

   The tree program reads both r1 (input accounts buffer) and r2 (instruction
   data pointer). Standard initState only sets r1. -/

@[simp] def treeInit (accts insn : Nat) (mem : Mem) : State where
  regs := { r1 := accts, r2 := insn, r10 := STACK_START + 0x1000 }
  mem := mem
  pc := 0
  exitCode := none

/-! ## effectiveAddr helpers for named Int offsets -/

private theorem ea_neg_SIZE_OF_U64 (b : Nat) (h : b ≥ 8) :
    effectiveAddr b (-SIZE_OF_U64) = b - 8 := by
  unfold effectiveAddr SIZE_OF_U64; omega

private theorem ea_OFFSET_ZERO (b : Nat) :
    effectiveAddr b OFFSET_ZERO = b := by
  unfold effectiveAddr OFFSET_ZERO; omega

private theorem ea_IB_N_ACCOUNTS_OFF (b : Nat) :
    effectiveAddr b IB_N_ACCOUNTS_OFF = b := by
  unfold effectiveAddr IB_N_ACCOUNTS_OFF; omega

/-! ## P1: invalid discriminator → error 11

   discriminator ∉ {0, 1, 2} → exit code E_INSTRUCTION_DISCRIMINATOR.
   Path: 0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 -/

set_option maxHeartbeats 800000 in
theorem rejects_invalid_discriminator
    (accts insn : Nat) (mem : Mem) (h_insn : insn ≥ 8)
    (disc : Nat)
    (h_disc : readU8 mem insn = disc)
    (h_ne0 : disc ≠ INSN_DISCRIMINATOR_INITIALIZE)
    (h_ne1 : disc ≠ INSN_DISCRIMINATOR_INSERT)
    (h_ne2 : disc ≠ INSN_DISCRIMINATOR_REMOVE) :
    (executeFn progAt (treeInit accts insn mem) 11).exitCode
      = some E_INSTRUCTION_DISCRIMINATOR := by
  have h1 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_INSERT) := by rw [h_disc]; exact h_ne1
  have h2 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_REMOVE) := by rw [h_disc]; exact h_ne2
  have h3 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_INITIALIZE) := by rw [h_disc]; exact h_ne0
  -- 0: ldx r9, [r2 - 8]
  rw [show (11 : Nat) = 0+1+1+1+1+1+1+1+1+1+1+1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 0 = _ from rfl)]
  simp [step, treeInit, RegFile.get, RegFile.set, resolveSrc, readByWidth,
        ea_neg_SIZE_OF_U64 _ h_insn]
  -- 1: ldx r8, [r1 + 0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 1 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_IB_N_ACCOUNTS_OFF]
  -- 2: ldx r7, [r2 + 0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 2 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_OFFSET_ZERO]
  -- 3: jeq r7, 1, 114 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 3 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h1]
  -- 4: jeq r7, 2, 325 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 4 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h2]
  -- 5: jeq r7, 0, 8 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 5 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h3]
  -- 6: mov64 r0, 11
  rw [executeFn_step _ _ _ _ rfl (show progAt 6 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 7: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 7 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P2: initialize with wrong instruction data length → error 12

   discriminator = INITIALIZE (0), instrDataLen ≠ 1 → E_INSTRUCTION_DATA_LEN.
   Path: 0 → 1 → 2 → 3 → 4 → 5 → 8 → 476 → 477 -/

set_option maxHeartbeats 800000 in
theorem init_rejects_wrong_data_len
    (accts insn : Nat) (mem : Mem) (h_insn : insn ≥ 8)
    (instrDataLen : Nat)
    (h_disc : readU8 mem insn = INSN_DISCRIMINATOR_INITIALIZE)
    (h_dlen : readU64 mem (insn - 8) = instrDataLen)
    (h_ne   : instrDataLen ≠ SIZE_OF_INITIALIZE_INSTRUCTION) :
    (executeFn progAt (treeInit accts insn mem) 12).exitCode
      = some E_INSTRUCTION_DATA_LEN := by
  have h_ne1 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_INSERT) := by rw [h_disc]; decide
  have h_ne2 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_REMOVE) := by rw [h_disc]; decide
  have h_ne_dl : ¬(readU64 mem (insn - 8) = SIZE_OF_INITIALIZE_INSTRUCTION) := by
    rw [h_dlen]; exact h_ne
  -- 0: ldx r9, [r2 - 8]
  rw [show (12 : Nat) = 0+1+1+1+1+1+1+1+1+1+1+1+1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 0 = _ from rfl)]
  simp [step, treeInit, RegFile.get, RegFile.set, resolveSrc, readByWidth,
        ea_neg_SIZE_OF_U64 _ h_insn]
  -- 1: ldx r8, [r1 + 0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 1 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_IB_N_ACCOUNTS_OFF]
  -- 2: ldx r7, [r2 + 0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 2 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_OFFSET_ZERO]
  -- 3: jeq r7, 1, 114 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 3 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne1]
  -- 4: jeq r7, 2, 325 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 4 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne2]
  -- 5: jeq r7, 0, 8 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 5 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 8: jne r9, 1, 476 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 8 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne_dl]
  -- 476: mov64 r0, 12
  rw [executeFn_step _ _ _ _ rfl (show progAt 476 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 477: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 477 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P3: initialize with wrong account count → error 1

   discriminator = INITIALIZE (0), instrDataLen = 1, nAccounts ≠ 5
   → E_N_ACCOUNTS.
   Path: 0 → 1 → 2 → 3 → 4 → 5 → 8 → 9 → 478 → 479 -/

set_option maxHeartbeats 800000 in
theorem init_rejects_wrong_account_count
    (accts insn : Nat) (mem : Mem) (h_insn : insn ≥ 8)
    (nAccounts : Nat)
    (h_disc : readU8 mem insn = INSN_DISCRIMINATOR_INITIALIZE)
    (h_dlen : readU64 mem (insn - 8) = SIZE_OF_INITIALIZE_INSTRUCTION)
    (h_naccts : readU64 mem accts = nAccounts)
    (h_ne    : nAccounts ≠ IB_N_ACCOUNTS_INIT) :
    (executeFn progAt (treeInit accts insn mem) 13).exitCode
      = some E_N_ACCOUNTS := by
  have h_ne1 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_INSERT) := by rw [h_disc]; decide
  have h_ne2 : ¬(readU8 mem insn = INSN_DISCRIMINATOR_REMOVE) := by rw [h_disc]; decide
  have h_ne_na : ¬(readU64 mem accts = IB_N_ACCOUNTS_INIT) := by rw [h_naccts]; exact h_ne
  -- 0: ldx r9, [r2 - 8]
  rw [show (13 : Nat) = 0+1+1+1+1+1+1+1+1+1+1+1+1+1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 0 = _ from rfl)]
  simp [step, treeInit, RegFile.get, RegFile.set, resolveSrc, readByWidth,
        ea_neg_SIZE_OF_U64 _ h_insn]
  -- 1: ldx r8, [r1 + 0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 1 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_IB_N_ACCOUNTS_OFF]
  -- 2: ldx r7, [r2 + 0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 2 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_OFFSET_ZERO]
  -- 3: jeq r7, 1, 114 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 3 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne1]
  -- 4: jeq r7, 2, 325 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 4 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne2]
  -- 5: jeq r7, 0, 8 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 5 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 8: jne r9, 1, 476 → fall-through (data len correct)
  rw [executeFn_step _ _ _ _ rfl (show progAt 8 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_dlen]
  -- 9: jne r8, 5, 478 → branch taken (wrong account count)
  rw [executeFn_step _ _ _ _ rfl (show progAt 9 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne_na]
  -- 478: mov64 r0, 1
  rw [executeFn_step _ _ _ _ rfl (show progAt 478 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 479: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 479 = _ from rfl)]
  simp [step, RegFile.get]

end TreeProofs
