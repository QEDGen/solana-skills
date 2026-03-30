-- Formal verification of the DASMAC counter program (validation guards)
--
-- Source: counter.s — a Solana counter program with initialize and increment
-- operations, PDA derivation, and CPI construction.
--
-- We verify the validation prefix: account count dispatch and
-- input validation checks for both branches.

import QEDGen.Solana.SBPF
import CounterProg

namespace CounterProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open CounterProg

set_option maxRecDepth 4096

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
  have : progAt 0 = some (.ldx .dword .r2 .r1 N_ACCOUNTS_OFF) := rfl
  have : progAt 1 = some (.jeq .r2 (.imm N_ACCOUNTS_INCREMENT) 116) := rfl
  have : progAt 2 = some (.jeq .r2 (.imm N_ACCOUNTS_INIT) 5) := rfl
  have : progAt 3 = some (.mov64 .r0 (.imm E_N_ACCOUNTS)) := rfl
  have : progAt 4 = some (.exit) := rfl
  simp only [ea_0] at *
  simp [*, executeFn, executeFn_halted, step, initState, RegFile.get, RegFile.set,
        resolveSrc, readByWidth, ea_0, execSyscall]

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
  -- Step-by-step execution unrolling
  -- 0: ldx r2, [r1+0]
  rw [show (10 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 0 = _ from rfl)]
  simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 1: jeq r2, 2, 116 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 1 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne2]
  -- 2: jeq r2, 3, 5 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 2 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_num]
  -- 5: ldx r2, [r1+88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 5 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  -- 6: jne r2, 0, 162 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 6 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne_dl]
  -- 162: mov32 r0, 2
  rw [executeFn_step _ _ _ _ rfl (show progAt 162 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 163: exit → r0 % U32_MODULUS = E_USER_DATA_LEN
  rw [executeFn_step _ _ _ _ rfl (show progAt 163 = _ from rfl)]
  simp [step, RegFile.get]
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
  -- 0: ldx r2, [r1+0]
  rw [show (12 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 0 = _ from rfl)]
  simp [step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 1: jeq r2, 2, 116 → fall-through
  rw [executeFn_step _ _ _ _ rfl (show progAt 1 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne2]
  -- 2: jeq r2, 3, 5 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 2 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_num]
  -- 5: ldx r2, [r1+88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 5 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  -- 6: jne r2, 0, 162 → falls through (DATA_LEN_ZERO = 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 6 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl]
  -- 7: ldx.b r2, [r1+10344]
  rw [executeFn_step _ _ _ _ rfl (show progAt 7 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10344]
  -- 8: jne r2, 0xff, 168 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 8 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne_dup]
  -- 168: mov32 r0, 5
  rw [executeFn_step _ _ _ _ rfl (show progAt 168 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 169: exit → r0 % U32_MODULUS = E_PDA_DUPLICATE
  rw [executeFn_step _ _ _ _ rfl (show progAt 169 = _ from rfl)]
  simp [step, RegFile.get]

end CounterProofs
