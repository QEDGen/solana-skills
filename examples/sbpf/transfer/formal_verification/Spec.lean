-- Formal verification of the DASMAC transfer program (validation guards)
--
-- Source: transfer.s — a SOL transfer program that validates inputs,
-- constructs a System Program Transfer CPI, and invokes it.
--
-- We verify the validation prefix: 7 input checks + balance check.

import SVM.SBPF
import Program

namespace TransferProofs

open SVM.SBPF
open SVM.SBPF.Memory
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
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (numAccounts : Nat)
    (h_rt_num : rt.containsRange inputAddr 8 = true)
    (h_num : readU64 mem inputAddr = numAccounts)
    (h_ne : numAccounts ≠ N_ACCOUNTS_EXPECTED) :
    (executeFn progAt (initState inputAddr mem rt) 6).exitCode = some E_N_ACCOUNTS := by
  have h_ne3 : ¬(readU64 mem inputAddr = N_ACCOUNTS_EXPECTED) := by rw [h_num]; exact h_ne
  wp_exec [progAt] [ea_0, ea_88, ea_10344, ea_10424, ea_20680, ea_31032, ea_31040, ea_80]

/-! ## P2: insufficient lamports → error 7

   All 7 prior checks pass (concrete values), balance check fails. -/

set_option maxHeartbeats 16000000 in
theorem rejects_insufficient_lamports
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (amount senderLamports : Nat)
    (h_rt_num  : rt.containsRange inputAddr 8 = true)
    (h_rt_sdl  : rt.containsRange (inputAddr + 88) 8 = true)
    (h_rt_rdup : rt.containsRange (inputAddr + 10344) 1 = true)
    (h_rt_rdl  : rt.containsRange (inputAddr + 10424) 8 = true)
    (h_rt_sdup : rt.containsRange (inputAddr + 20680) 1 = true)
    (h_rt_idl  : rt.containsRange (inputAddr + 31032) 8 = true)
    (h_rt_amt  : rt.containsRange (inputAddr + 31040) 8 = true)
    (h_rt_bal  : rt.containsRange (inputAddr + 80) 8 = true)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (inputAddr + 88) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (inputAddr + 10344) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (inputAddr + 10424) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (inputAddr + 20680) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (inputAddr + 31032) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (inputAddr + 31040) = amount)
    (h_bal   : readU64 mem (inputAddr + 80) = senderLamports)
    (h_insuf : senderLamports < amount) :
    (executeFn progAt (initState inputAddr mem rt) 20).exitCode = some E_INSUFFICIENT_LAMPORTS := by
  wp_exec [progAt] [ea_0, ea_88, ea_10344, ea_10424, ea_20680, ea_31032, ea_31040, ea_80]

/-! ## P3: happy path → reaches the CPI boundary

   All checks pass, balance sufficient → after the 15-instruction
   validation prefix, control sits AT the `sol_invoke_signed` call
   (pc 15) with no error, r2 = sender balance, r4 = transfer amount.

   qedsvm ≥ v0.9.0 models the proof-facing CPI fail-closed (`Cpi.exec`
   aborts rather than fabricate a successful no-effect invoke — audit
   C4/C5), so the pre-v0.9 "exit 0 through the CPI" statement is no
   longer provable, and was never sound. The boundary statement is the
   honest claim this file's scope (the validation prefix) supports;
   what the invoke is HANDED can be pinned with `SVM.Solana.cpiEnvelope`
   once the CPI construction is lifted (the .s elides it). -/

set_option maxHeartbeats 16000000 in
theorem accepts_valid_transfer
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (amount senderLamports : Nat)
    (h_rt_num  : rt.containsRange inputAddr 8 = true)
    (h_rt_sdl  : rt.containsRange (inputAddr + 88) 8 = true)
    (h_rt_rdup : rt.containsRange (inputAddr + 10344) 1 = true)
    (h_rt_rdl  : rt.containsRange (inputAddr + 10424) 8 = true)
    (h_rt_sdup : rt.containsRange (inputAddr + 20680) 1 = true)
    (h_rt_idl  : rt.containsRange (inputAddr + 31032) 8 = true)
    (h_rt_amt  : rt.containsRange (inputAddr + 31040) 8 = true)
    (h_rt_bal  : rt.containsRange (inputAddr + 80) 8 = true)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (inputAddr + 88) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (inputAddr + 10344) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (inputAddr + 10424) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (inputAddr + 20680) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (inputAddr + 31032) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (inputAddr + 31040) = amount)
    (h_bal   : readU64 mem (inputAddr + 80) = senderLamports)
    (h_suf   : senderLamports ≥ amount) :
    (executeFn progAt (initState inputAddr mem rt) 15).pc = 15 ∧
    (executeFn progAt (initState inputAddr mem rt) 15).exitCode = none ∧
    (executeFn progAt (initState inputAddr mem rt) 15).regs.r2 = senderLamports ∧
    (executeFn progAt (initState inputAddr mem rt) 15).regs.r4 = amount := by
  have h_not_lt : ¬(senderLamports < amount) := by omega
  refine ⟨?_, ?_, ?_, ?_⟩ <;>
    wp_exec [progAt] [ea_0, ea_88, ea_10344, ea_10424, ea_20680, ea_31032, ea_31040, ea_80]

end TransferProofs
