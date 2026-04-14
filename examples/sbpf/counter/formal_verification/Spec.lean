-- Formal verification of the DASMAC counter program (validation guards)
--
-- Source: counter.s — a Solana counter program with initialize and increment
-- operations, PDA derivation, and CPI construction.
--
-- We verify the validation prefix: account count dispatch and
-- input validation checks for both branches.
--
-- Proofs use the monadic WP bridge via wp_exec:
--   1. dsimp evaluates instruction fetch via kernel reduction
--   2. simp resolves branch conditions from hypotheses
--   3. rfl closes the halted-state residual
-- Each step is O(1) kernel depth.

import QEDGen.Solana.SBPF
import Program

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
  have h1 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; exact h_ne2
  have h2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INIT) := by rw [h_num]; exact h_ne3
  wp_exec [progAt, progAt_0, progAt_1] [ea_0]

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
  have h_ne2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; decide
  have h_ne_dl : ¬(readU64 mem (inputAddr + 88) = DATA_LEN_ZERO) := by rw [h_udl]; exact h_ne
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_88, U32_MODULUS]

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
  have h_ne2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; decide
  have h_ne_dup : ¬(readU8 mem (inputAddr + 10344) = NON_DUP_MARKER) := by rw [h_pdup]; exact h_dup
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_88, ea_10344, U32_MODULUS]

end CounterProofs
