-- Formal verification of the DASMAC transfer program (validation guards)
--
-- Source: transfer.s — a SOL transfer program that validates inputs,
-- constructs a System Program Transfer CPI, and invokes it.
--
-- We verify the validation prefix: 7 input checks + balance check.

import QEDGen.Solana.SBPF
import TransferProg

namespace TransferProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open TransferProg

/-! ## P1: wrong account count → error 1

   Symbolic proof: numAccounts ≠ 3 → exit code 1 in 4 steps. -/

set_option maxHeartbeats 8000000 in
theorem rejects_wrong_account_count
    (inputAddr : Nat) (mem : Mem)
    (numAccounts : Nat)
    (h_num : readU64 mem inputAddr = numAccounts)
    (h_ne : numAccounts ≠ N_ACCOUNTS_EXPECTED) :
    (execute prog (initState inputAddr mem) 6).exitCode = some E_N_ACCOUNTS := by
  have h_ne3 : ¬(readU64 mem inputAddr = N_ACCOUNTS_EXPECTED) := by rw [h_num]; exact h_ne
  sbpf_steps

/-! ## P2: insufficient lamports → error 7

   All 7 prior checks pass (concrete values), balance check fails. -/

set_option maxHeartbeats 16000000 in
theorem rejects_insufficient_lamports
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (effectiveAddr inputAddr SENDER_DATA_LENGTH_OFFSET) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (effectiveAddr inputAddr RECIPIENT_OFFSET) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (effectiveAddr inputAddr RECIPIENT_DATA_LENGTH_OFFSET) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (effectiveAddr inputAddr SYSTEM_PROGRAM_OFFSET) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_LENGTH_OFFSET) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_OFFSET) = amount)
    (h_bal   : readU64 mem (effectiveAddr inputAddr SENDER_LAMPORTS_OFFSET) = senderLamports)
    (h_insuf : senderLamports < amount) :
    (execute prog (initState inputAddr mem) 20).exitCode = some E_INSUFFICIENT_LAMPORTS := by
  sbpf_steps

/-! ## P3: happy path → exit 0

   All checks pass, balance sufficient → normal exit.
   The program invokes sol_invoke_signed (CPI) then exits with r0 = 0. -/

set_option maxHeartbeats 16000000 in
theorem accepts_valid_transfer
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (effectiveAddr inputAddr SENDER_DATA_LENGTH_OFFSET) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (effectiveAddr inputAddr RECIPIENT_OFFSET) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (effectiveAddr inputAddr RECIPIENT_DATA_LENGTH_OFFSET) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (effectiveAddr inputAddr SYSTEM_PROGRAM_OFFSET) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_LENGTH_OFFSET) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_OFFSET) = amount)
    (h_bal   : readU64 mem (effectiveAddr inputAddr SENDER_LAMPORTS_OFFSET) = senderLamports)
    (h_suf   : senderLamports ≥ amount) :
    (execute prog (initState inputAddr mem) 20).exitCode = some 0 := by
  have h_not_lt : ¬(senderLamports < amount) := by omega
  sbpf_steps

end TransferProofs
