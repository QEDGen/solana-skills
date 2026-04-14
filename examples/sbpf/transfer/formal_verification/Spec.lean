-- Formal verification of the DASMAC transfer program (validation guards)
--
-- Source: transfer.s — a SOL transfer program that validates inputs,
-- constructs a System Program Transfer CPI, and invokes it.
--
-- We verify the validation prefix: 7 input checks + balance check.

import QEDGen.Solana.SBPF
import Program

namespace TransferProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open TransferProg

/-! ## effectiveAddr lemmas -/

private theorem ea_0 (b : Nat) : effectiveAddr b N_ACCOUNTS_OFFSET = b := by
  unfold effectiveAddr N_ACCOUNTS_OFFSET; omega
private theorem ea_80 (b : Nat) : effectiveAddr b SENDER_LAMPORTS_OFFSET = b + 80 := by
  unfold effectiveAddr SENDER_LAMPORTS_OFFSET; omega
private theorem ea_88 (b : Nat) : effectiveAddr b SENDER_DATA_LENGTH_OFFSET = b + 88 := by
  unfold effectiveAddr SENDER_DATA_LENGTH_OFFSET; omega
private theorem ea_10344 (b : Nat) : effectiveAddr b RECIPIENT_OFFSET = b + 10344 := by
  unfold effectiveAddr RECIPIENT_OFFSET; omega
private theorem ea_10424 (b : Nat) : effectiveAddr b RECIPIENT_DATA_LENGTH_OFFSET = b + 10424 := by
  unfold effectiveAddr RECIPIENT_DATA_LENGTH_OFFSET; omega
private theorem ea_20680 (b : Nat) : effectiveAddr b SYSTEM_PROGRAM_OFFSET = b + 20680 := by
  unfold effectiveAddr SYSTEM_PROGRAM_OFFSET; omega
private theorem ea_31032 (b : Nat) : effectiveAddr b INSTRUCTION_DATA_LENGTH_OFFSET = b + 31032 := by
  unfold effectiveAddr INSTRUCTION_DATA_LENGTH_OFFSET; omega
private theorem ea_31040 (b : Nat) : effectiveAddr b INSTRUCTION_DATA_OFFSET = b + 31040 := by
  unfold effectiveAddr INSTRUCTION_DATA_OFFSET; omega

/-! ## P1: wrong account count → error 1

   Symbolic proof: numAccounts ≠ 3 → exit code 1 in 4 steps. -/

set_option maxHeartbeats 800000 in
theorem rejects_wrong_account_count
    (inputAddr : Nat) (mem : Mem)
    (numAccounts : Nat)
    (h_num : readU64 mem inputAddr = numAccounts)
    (h_ne : numAccounts ≠ N_ACCOUNTS_EXPECTED) :
    (executeFn progAt (initState inputAddr mem) 6).exitCode = some E_N_ACCOUNTS := by
  have h_ne3 : ¬(readU64 mem inputAddr = N_ACCOUNTS_EXPECTED) := by rw [h_num]; exact h_ne
  wp_exec [progAt] [ea_0, ea_88, ea_10344, ea_10424, ea_20680, ea_31032, ea_31040, ea_80]

/-! ## P2: insufficient lamports → error 7

   All 7 prior checks pass (concrete values), balance check fails. -/

set_option maxHeartbeats 16000000 in
theorem rejects_insufficient_lamports
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (inputAddr + 88) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (inputAddr + 10344) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (inputAddr + 10424) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (inputAddr + 20680) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (inputAddr + 31032) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (inputAddr + 31040) = amount)
    (h_bal   : readU64 mem (inputAddr + 80) = senderLamports)
    (h_insuf : senderLamports < amount) :
    (executeFn progAt (initState inputAddr mem) 20).exitCode = some E_INSUFFICIENT_LAMPORTS := by
  wp_exec [progAt] [ea_0, ea_88, ea_10344, ea_10424, ea_20680, ea_31032, ea_31040, ea_80]

/-! ## P3: happy path → exit 0

   All checks pass, balance sufficient → normal exit.
   The program invokes sol_invoke_signed (CPI) then exits with r0 = 0. -/

set_option maxHeartbeats 16000000 in
theorem accepts_valid_transfer
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (inputAddr + 88) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (inputAddr + 10344) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (inputAddr + 10424) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (inputAddr + 20680) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (inputAddr + 31032) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (inputAddr + 31040) = amount)
    (h_bal   : readU64 mem (inputAddr + 80) = senderLamports)
    (h_suf   : senderLamports ≥ amount) :
    (executeFn progAt (initState inputAddr mem) 20).exitCode = some 0 := by
  have h_not_lt : ¬(senderLamports < amount) := by omega
  wp_exec [progAt] [ea_0, ea_88, ea_10344, ea_10424, ea_20680, ea_31032, ea_31040, ea_80]

end TransferProofs
