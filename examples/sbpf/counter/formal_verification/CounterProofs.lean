-- Formal verification of the DASMAC counter program (validation guards)
--
-- Source: counter.s — a Solana counter program with initialize and increment
-- operations, PDA derivation, and CPI construction.
--
-- We verify the validation prefix: account count dispatch and
-- input validation checks for both branches.
--
-- Proofs use the monadic WP bridge (executeFn_eq_execSegment)
-- with iterative step unfolding for O(1) kernel depth per step.
--
-- Pattern: pre-compute fetch values via native_decide, then
--   repeat (unfold execSegment; simp [ea_lemmas, U32_MODULUS, *])

import QEDGen.Solana.SBPF
import CounterProg

namespace CounterProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open CounterProg

/-! ## Proof helpers: effectiveAddr with named Int offsets -/

private theorem ea_0 (b : Nat) : effectiveAddr b N_ACCOUNTS_OFF = b := by
  unfold effectiveAddr N_ACCOUNTS_OFF; omega

private theorem ea_88 (b : Nat) : effectiveAddr b USER_DATA_LEN_OFF = b + 88 := by
  unfold effectiveAddr USER_DATA_LEN_OFF; omega

private theorem ea_10344 (b : Nat) : effectiveAddr b PDA_NON_DUP_MARKER_OFF = b + 10344 := by
  unfold effectiveAddr PDA_NON_DUP_MARKER_OFF; omega

/-! ## P1: wrong account count → error 1

   numAccounts ≠ 2 AND numAccounts ≠ 3 → exit code E_N_ACCOUNTS.
   Path: 0 → 1 → 2 → 3 → 4 -/

set_option maxHeartbeats 800000 in
theorem rejects_wrong_account_count
    (inputAddr : Nat) (mem : Mem)
    (numAccounts : Nat)
    (h_num : readU64 mem inputAddr = numAccounts)
    (h_ne2 : numAccounts ≠ N_ACCOUNTS_INCREMENT)
    (h_ne3 : numAccounts ≠ N_ACCOUNTS_INIT) :
    (executeFn progAt (initState inputAddr mem) 8).exitCode = some E_N_ACCOUNTS := by
  rw [executeFn_eq_execSegment]
  have h1 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; exact h_ne2
  have h2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INIT) := by rw [h_num]; exact h_ne3
  have f0 : progAt 0 = some (.ldx .dword .r2 .r1 N_ACCOUNTS_OFF) := by native_decide
  have f1 : progAt 1 = some (.jeq .r2 (.imm N_ACCOUNTS_INCREMENT) 116) := by native_decide
  have f2 : progAt 2 = some (.jeq .r2 (.imm N_ACCOUNTS_INIT) 5) := by native_decide
  have f3 : progAt 3 = some (.mov64 .r0 (.imm E_N_ACCOUNTS)) := by native_decide
  have f4 : progAt 4 = some .exit := by native_decide
  repeat (
    unfold execSegment;
    simp (config := { failIfUnchanged := false }) [ea_0, *])

/-! ## P2: user data length nonzero (initialize) → error 2

   numAccounts = 3, userData ≠ 0 → exit code E_USER_DATA_LEN.
   Path: 0 → 1 → 2 → 5 → 6 → 162 → 163 -/

set_option maxHeartbeats 800000 in
theorem init_rejects_user_data_len
    (inputAddr : Nat) (mem : Mem)
    (userDataLen : Nat)
    (h_num : readU64 mem inputAddr = N_ACCOUNTS_INIT)
    (h_udl : readU64 mem (inputAddr + 88) = userDataLen)
    (h_ne  : userDataLen ≠ DATA_LEN_ZERO) :
    (executeFn progAt (initState inputAddr mem) 10).exitCode = some E_USER_DATA_LEN := by
  rw [executeFn_eq_execSegment]
  have h_ne2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; decide
  have h_ne_dl : ¬(readU64 mem (inputAddr + 88) = DATA_LEN_ZERO) := by rw [h_udl]; exact h_ne
  have f0 : progAt 0 = some (.ldx .dword .r2 .r1 N_ACCOUNTS_OFF) := by native_decide
  have f1 : progAt 1 = some (.jeq .r2 (.imm N_ACCOUNTS_INCREMENT) 116) := by native_decide
  have f2 : progAt 2 = some (.jeq .r2 (.imm N_ACCOUNTS_INIT) 5) := by native_decide
  have f5 : progAt 5 = some (.ldx .dword .r2 .r1 USER_DATA_LEN_OFF) := by native_decide
  have f6 : progAt 6 = some (.jne .r2 (.imm DATA_LEN_ZERO) 162) := by native_decide
  have f162 : progAt 162 = some (.mov32 .r0 (.imm E_USER_DATA_LEN)) := by native_decide
  have f163 : progAt 163 = some .exit := by native_decide
  repeat (
    unfold execSegment;
    simp (config := { failIfUnchanged := false }) [ea_0, ea_88, U32_MODULUS, *])

/-! ## P3: PDA duplicate (initialize) → error 5

   numAccounts = 3, userData = 0, PDA is duplicate → exit code E_PDA_DUPLICATE.
   Path: 0 → 1 → 2 → 5 → 6 → 7 → 8 → 168 → 169 -/

set_option maxHeartbeats 800000 in
theorem init_rejects_pda_duplicate
    (inputAddr : Nat) (mem : Mem)
    (pdaDupMarker : Nat)
    (h_num  : readU64 mem inputAddr = N_ACCOUNTS_INIT)
    (h_udl  : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_pdup : readU8  mem (inputAddr + 10344) = pdaDupMarker)
    (h_dup  : pdaDupMarker ≠ NON_DUP_MARKER) :
    (executeFn progAt (initState inputAddr mem) 12).exitCode = some E_PDA_DUPLICATE := by
  rw [executeFn_eq_execSegment]
  have h_ne2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; decide
  have h_ne_dup : ¬(readU8 mem (inputAddr + 10344) = NON_DUP_MARKER) := by rw [h_pdup]; exact h_dup
  have f0 : progAt 0 = some (.ldx .dword .r2 .r1 N_ACCOUNTS_OFF) := by native_decide
  have f1 : progAt 1 = some (.jeq .r2 (.imm N_ACCOUNTS_INCREMENT) 116) := by native_decide
  have f2 : progAt 2 = some (.jeq .r2 (.imm N_ACCOUNTS_INIT) 5) := by native_decide
  have f5 : progAt 5 = some (.ldx .dword .r2 .r1 USER_DATA_LEN_OFF) := by native_decide
  have f6 : progAt 6 = some (.jne .r2 (.imm DATA_LEN_ZERO) 162) := by native_decide
  have f7 : progAt 7 = some (.ldx .byte .r2 .r1 PDA_NON_DUP_MARKER_OFF) := by native_decide
  have f8 : progAt 8 = some (.jne .r2 (.imm NON_DUP_MARKER) 168) := by native_decide
  have f168 : progAt 168 = some (.mov32 .r0 (.imm E_PDA_DUPLICATE)) := by native_decide
  have f169 : progAt 169 = some .exit := by native_decide
  repeat (
    unfold execSegment;
    simp (config := { failIfUnchanged := false }) [ea_0, ea_88, ea_10344, U32_MODULUS, *])

end CounterProofs
