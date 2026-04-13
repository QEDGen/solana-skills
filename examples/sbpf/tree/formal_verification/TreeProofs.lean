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

/-! ## effectiveAddr helper for negative offset -/

@[simp] private theorem ea_neg_SIZE_OF_U64 (b : Nat) (h : b ≥ 8) :
    effectiveAddr b (-SIZE_OF_U64) = b - 8 := by
  unfold effectiveAddr SIZE_OF_U64; omega

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
  wp_exec [progAt, progAt_0, treeInit] [ea_neg_SIZE_OF_U64 _ h_insn, ea_IB_N_ACCOUNTS_OFF, ea_OFFSET_ZERO]

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
  wp_exec [progAt, progAt_0, progAt_4, treeInit] [ea_neg_SIZE_OF_U64 _ h_insn, ea_IB_N_ACCOUNTS_OFF, ea_OFFSET_ZERO]

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
  wp_exec [progAt, progAt_0, progAt_4, treeInit] [ea_neg_SIZE_OF_U64 _ h_insn, ea_IB_N_ACCOUNTS_OFF, ea_OFFSET_ZERO]

end TreeProofs
