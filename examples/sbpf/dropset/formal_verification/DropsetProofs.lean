-- Formal verification of the DASMAC dropset program (validation guards)
--
-- Source: dropset.s — a Solana on-chain order book (sBPF assembly).
-- Implements RegisterMarket: validates accounts, derives PDA, creates account via CPI.
--
-- We verify the validation prefix: discriminant dispatch, account count,
-- instruction length, and per-account duplicate/data checks.

import QEDGen.Solana.SBPF
import DropsetProg

namespace DropsetProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open DropsetProg

set_option maxRecDepth 4096

/-! ## Proof helpers: effectiveAddr with named Int offsets -/

private theorem ea_0 (b : Nat) : effectiveAddr b IB_N_ACCTS_OFF = b := by
  unfold effectiveAddr IB_N_ACCTS_OFF; omega

private theorem ea_neg8 (b : Nat) : effectiveAddr b INSN_LEN_OFF = b - 8 := by
  unfold effectiveAddr INSN_LEN_OFF; omega

private theorem ea_disc0 (b : Nat) : effectiveAddr b INSN_DISC_OFF = b := by
  unfold effectiveAddr INSN_DISC_OFF; omega

private theorem ea_88 (b : Nat) : effectiveAddr b IB_USER_DATA_LEN_OFF = b + 88 := by
  unfold effectiveAddr IB_USER_DATA_LEN_OFF; omega

private theorem ea_10344 (b : Nat) : effectiveAddr b IB_MARKET_DUPLICATE_OFF = b + 10344 := by
  unfold effectiveAddr IB_MARKET_DUPLICATE_OFF; omega

private theorem ea_10424 (b : Nat) : effectiveAddr b IB_MARKET_DATA_LEN_OFF = b + 10424 := by
  unfold effectiveAddr IB_MARKET_DATA_LEN_OFF; omega

/-! ## P1: invalid discriminant → error 1

   If the instruction discriminant ≠ 0 (RegisterMarket), the program
   exits with E_INVALID_DISCRIMINANT (1) in 8 steps.
   Path: 24 → 25 → 26 → 27(fall) → 28 → 29 -/

set_option maxHeartbeats 800000 in
theorem rejects_invalid_discriminant
    (inputAddr insnAddr : Nat) (mem : Mem)
    (disc : Nat)
    (h_disc_val : readU8 mem insnAddr = disc)
    (h_disc_ne  : disc ≠ DISC_REGISTER_MARKET) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 8).exitCode
      = some E_INVALID_DISCRIMINANT := by
  have h_ne : ¬(readU8 mem insnAddr = DISC_REGISTER_MARKET) := by rw [h_disc_val]; exact h_disc_ne
  -- 24: ldx.dw r3, [r1+0]
  rw [show (8 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 25: ldx.dw r4, [r2-8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  -- 26: ldx.b r5, [r2+0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  -- 27: jeq r5, 0, 30 → falls through (disc ≠ 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne]
  -- 28: mov32 r0, E_INVALID_DISCRIMINANT
  rw [executeFn_step _ _ _ _ rfl (show progAt 28 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 29: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 29 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P2: invalid account count → error 3

   Discriminant = 0 (RegisterMarket), but n_accounts < 10.
   Path: 24 → 25 → 26 → 27(jump) → 30 → 2 → 3 -/

set_option maxHeartbeats 800000 in
theorem rejects_invalid_account_count
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts : Nat)
    (h_disc  : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num   : readU64 mem inputAddr = nAccounts)
    (h_few   : nAccounts < REGISTER_MARKET_ACCOUNTS_LEN) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 10).exitCode
      = some E_INVALID_NUMBER_OF_ACCOUNTS := by
  have h_lt : readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN := by rw [h_num]; exact h_few
  -- 24: ldx.dw r3, [r1+0]
  rw [show (10 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 25: ldx.dw r4, [r2-8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  -- 26: ldx.b r5, [r2+0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  -- 27: jeq r5, 0, 30 → branch taken (disc = 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30: jlt r3, 10, 2 → branch taken (nAccounts < 10)
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_lt]
  -- 2: mov32 r0, E_INVALID_NUMBER_OF_ACCOUNTS
  rw [executeFn_step _ _ _ _ rfl (show progAt 2 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 3: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 3 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P3: invalid instruction length → error 2

   Discriminant = 0, n_accounts ≥ 10, but insn_len ≠ 1.
   Path: 24 → … → 30(fall) → 31 → 0 → 1 -/

set_option maxHeartbeats 800000 in
theorem rejects_invalid_instruction_length
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts insnLen : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = insnLen)
    (h_ne_len : insnLen ≠ REGISTER_MARKET_DATA_LEN) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 12).exitCode
      = some E_INVALID_INSTRUCTION_LENGTH := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_ne : ¬(readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN) := by rw [h_ilen]; exact h_ne_len
  -- 24: ldx.dw r3, [r1+0]
  rw [show (12 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 25: ldx.dw r4, [r2-8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  -- 26: ldx.b r5, [r2+0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  -- 27: jeq r5, 0, 30 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30: jlt r3, 10, 2 → falls through (n_accounts ≥ 10)
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  -- 31: jne r4, 1, 0 → branch taken (insn_len ≠ 1)
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ne]
  -- 0: mov32 r0, E_INVALID_INSTRUCTION_LENGTH
  rw [executeFn_step _ _ _ _ rfl (show progAt 0 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 1: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 1 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P4: user has data → error 4

   All prior checks pass, but user data length ≠ 0.
   Path: 24 → … → 31(fall) → 32 → 33 → 4 → 5 -/

set_option maxHeartbeats 800000 in
theorem rejects_user_has_data
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts insnLen userDataLen : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = insnLen)
    (h_ilen_ok: insnLen = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = userDataLen)
    (h_udl_ne : userDataLen ≠ DATA_LEN_ZERO) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 14).exitCode
      = some E_USER_HAS_DATA := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_ilen_eq : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN := by rw [h_ilen, h_ilen_ok]
  have h_udl_ne' : ¬(readU64 mem (inputAddr + 88) = DATA_LEN_ZERO) := by rw [h_udl]; exact h_udl_ne
  -- 24: ldx.dw r3, [r1+0]
  rw [show (14 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 25: ldx.dw r4, [r2-8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  -- 26: ldx.b r5, [r2+0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  -- 27: jeq r5, 0, 30 → branch taken
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30: jlt r3, 10, 2 → falls through
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  -- 31: jne r4, 1, 0 → falls through (insn_len = 1)
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ilen_eq]
  -- 32: ldx.dw r9, [r1+88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 32 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  -- 33: jne r9, 0, 4 → branch taken (userDataLen ≠ 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 33 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl_ne']
  -- 4: mov32 r0, E_USER_HAS_DATA
  rw [executeFn_step _ _ _ _ rfl (show progAt 4 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 5: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 5 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P5: market account is duplicate → error 5

   Prior checks pass, user data = 0, but market dup ≠ 255.
   Path: 24 → … → 33(fall) → 34 → 35 → 6 → 7 -/

set_option maxHeartbeats 800000 in
theorem rejects_market_duplicate
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts mktDup : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = mktDup)
    (h_mdup_ne: mktDup ≠ ACCT_NON_DUP_MARKER) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 16).exitCode
      = some E_MARKET_ACCOUNT_IS_DUPLICATE := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_mdup' : ¬(readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER) := by rw [h_mdup]; exact h_mdup_ne
  -- 24-27: entrypoint → register_market
  rw [show (16 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30: jlt → falls through
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  -- 31: jne → falls through (insn_len ok)
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ilen]
  -- 32: ldx r9, [r1+88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 32 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  -- 33: jne r9, 0, 4 → falls through (user data = 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 33 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl]
  -- 34: ldx.b r9, [r1+10344]
  rw [executeFn_step _ _ _ _ rfl (show progAt 34 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10344]
  -- 35: jne r9, 255, 6 → branch taken (dup ≠ 255)
  rw [executeFn_step _ _ _ _ rfl (show progAt 35 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdup']
  -- 6: mov32 r0, E_MARKET_ACCOUNT_IS_DUPLICATE
  rw [executeFn_step _ _ _ _ rfl (show progAt 6 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 7: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 7 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P6: market has data → error 6

   Prior checks pass, market not duplicate, but market data_len ≠ 0.
   Path: 24 → … → 35(fall) → 36 → 37 → 8 → 9 -/

set_option maxHeartbeats 800000 in
theorem rejects_market_has_data
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts mktDataLen : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = mktDataLen)
    (h_mdl_ne : mktDataLen ≠ DATA_LEN_ZERO) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 18).exitCode
      = some E_MARKET_HAS_DATA := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_mdl' : ¬(readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO) := by rw [h_mdl]; exact h_mdl_ne
  -- 24-27: entrypoint → register_market
  rw [show (18 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30-31: acct count + insn len pass
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ilen]
  -- 32-33: user data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 32 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 33 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl]
  -- 34-35: market dup check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 34 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10344]
  rw [executeFn_step _ _ _ _ rfl (show progAt 35 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdup]
  -- 36: ldx.dw r9, [r1+10424]
  rw [executeFn_step _ _ _ _ rfl (show progAt 36 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10424]
  -- 37: jne r9, 0, 8 → branch taken (market data ≠ 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 37 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdl']
  -- 8: mov32 r0, E_MARKET_HAS_DATA
  rw [executeFn_step _ _ _ _ rfl (show progAt 8 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 9: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 9 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P7: base mint is duplicate → error 7

   Prior checks pass, market data = 0, but base mint dup ≠ 255.
   Path: 24 → … → 37(fall) → 38 → 39 → 10 → 11 -/

private theorem ea_20680 (b : Nat) : effectiveAddr b RM_MISC_BASE_DUPLICATE_OFF = b + 20680 := by
  unfold effectiveAddr RM_MISC_BASE_DUPLICATE_OFF; omega

set_option maxHeartbeats 800000 in
theorem rejects_base_mint_duplicate
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDup : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = baseDup)
    (h_bdup_ne: baseDup ≠ ACCT_NON_DUP_MARKER) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 20).exitCode
      = some E_BASE_MINT_IS_DUPLICATE := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_bdup' : ¬(readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER) := by rw [h_bdup]; exact h_bdup_ne
  -- 24-27: entrypoint → register_market
  rw [show (20 : Nat) = 0 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 from rfl]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30-31: acct count + insn len pass
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ilen]
  -- 32-33: user data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 32 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 33 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl]
  -- 34-35: market dup check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 34 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10344]
  rw [executeFn_step _ _ _ _ rfl (show progAt 35 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdup]
  -- 36-37: market data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 36 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10424]
  rw [executeFn_step _ _ _ _ rfl (show progAt 37 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdl]
  -- 38: ldx.b r9, [r1+20680]
  rw [executeFn_step _ _ _ _ rfl (show progAt 38 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_20680]
  -- 39: jne r9, 255, 10 → branch taken (baseDup ≠ 255)
  rw [executeFn_step _ _ _ _ rfl (show progAt 39 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_bdup']
  -- 10: mov32 r0, E_BASE_MINT_IS_DUPLICATE
  rw [executeFn_step _ _ _ _ rfl (show progAt 10 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 11: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 11 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P8: quote mint is duplicate → error 8

   Prior checks pass, base mint not dup, but the quote mint at the shifted
   input position has dup ≠ 255.

   Path: 24 → … → 39(fall) → 40-48 (pointer arith + stack writes) → 49 → 50 → 12 → 13

   Complexity: instructions 42/44 write PDA seeds to the stack (mutating mem),
   instruction 47 is and64 with -8 for 8-byte alignment, and instruction 49
   reads from a computed address. The proof requires memory disjointness
   axioms to show stack writes don't affect input buffer reads. -/

private theorem ea_20760 (b : Nat) : effectiveAddr b RM_MISC_BASE_DATA_LEN_OFF = b + 20760 := by
  unfold effectiveAddr RM_MISC_BASE_DATA_LEN_OFF; omega

private theorem ea_fm_pda_seeds_base_addr (b : Nat) :
    effectiveAddr b RM_FM_PDA_SEEDS_BASE_ADDR_OFF = b - 664 := by
  unfold effectiveAddr RM_FM_PDA_SEEDS_BASE_ADDR_OFF; omega

private theorem ea_fm_pda_seeds_base_len (b : Nat) :
    effectiveAddr b RM_FM_PDA_SEEDS_BASE_LEN_OFF = b - 656 := by
  unfold effectiveAddr RM_FM_PDA_SEEDS_BASE_LEN_OFF; omega

private theorem ea_31016 (b : Nat) : effectiveAddr b RM_MISC_QUOTE_DUPLICATE_OFF = b + 31016 := by
  unfold effectiveAddr RM_MISC_QUOTE_DUPLICATE_OFF; omega

/-- Shifted input address: inputAddr offset by the padded base mint data length.
    This is the runtime-computed pointer used to access accounts after base mint. -/
def shiftedInputAddr (inputAddr baseDataLen : Nat) : Nat :=
  wrapAdd ((baseDataLen + 7) &&& toU64 DATA_LEN_AND_MASK) inputAddr

/-! ### Helpers for P8 -/

private theorem ea_base_addr_off (b : Nat) :
    effectiveAddr b RM_MISC_BASE_ADDR_OFF = b + 20688 := by
  unfold effectiveAddr RM_MISC_BASE_ADDR_OFF; omega

/-! ## P8: quote mint is duplicate → error 8

   Prior checks pass, base mint not dup, but the quote mint at the shifted
   input position has dup ≠ 255.

   Path: 24 → … → 39(fall) → 40-48 (pointer arith + stack writes) → 49 → 50 → 12 → 13

   Complexity: instructions 42/44 write PDA seeds to the stack (mutating mem),
   instruction 47 is and64 with -8 for 8-byte alignment, and instruction 49
   reads from a computed address. The proof requires memory disjointness
   axioms to show stack writes don't affect input buffer reads. -/

set_option maxHeartbeats 8000000 in
theorem rejects_quote_mint_duplicate
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen quoteDup : Nat)
    -- Common prefix
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    -- Base data length and quote dup at shifted address
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    -- Quote mint dup at the shifted address: the program computes
    --   shifted = ((baseDataLen + 7) % 2^64 &&& (2^64 - 8)) % 2^64 + inputAddr
    -- and reads the duplicate marker at shifted + 31016.
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = quoteDup)
    (h_qdup_ne: quoteDup ≠ ACCT_NON_DUP_MARKER)
    -- Stack-input separation (Solana runtime guarantee)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    -- The quote mint read address is below the stack
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 30).exitCode
      = some E_QUOTE_MINT_IS_DUPLICATE := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_qdup' : ¬(readU8 mem
      (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
        inputAddr + 31016) = ACCT_NON_DUP_MARKER) := by rw [h_qdup]; exact h_qdup_ne
  -- ── Phase 1: Common prefix (insns 24-39, 14 steps) ──
  -- All prior validation checks pass, reaching pc=40.
  rw [show (30 : Nat) = 0+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1 from rfl]
  -- 24-27: entrypoint → disc check → register_market
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30-31: acct count + insn len pass
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ilen]
  -- 32-33: user data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 32 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 33 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl]
  -- 34-35: market dup check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 34 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10344]
  rw [executeFn_step _ _ _ _ rfl (show progAt 35 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdup]
  -- 36-37: market data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 36 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10424]
  rw [executeFn_step _ _ _ _ rfl (show progAt 37 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdl]
  -- 38-39: base dup check passes (base dup = 255, fall through)
  rw [executeFn_step _ _ _ _ rfl (show progAt 38 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_20680]
  rw [executeFn_step _ _ _ _ rfl (show progAt 39 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_bdup]
  -- ── Phase 2: Pointer arithmetic (insns 40-48, 9 steps) ──
  -- Computes shifted input address; writes PDA seeds to stack.
  -- 40: mov64 r9, r1
  rw [executeFn_step _ _ _ _ rfl (show progAt 40 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 41: add64 r9, RM_MISC_BASE_ADDR_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 41 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, ea_base_addr_off]
  -- 42: stx.dw [r10-664], r9  (write PDA seed base addr to stack)
  rw [executeFn_step _ _ _ _ rfl (show progAt 42 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, writeByWidth, ea_fm_pda_seeds_base_addr]
  -- 43: mov64 r9, SIZE_OF_ADDRESS
  rw [executeFn_step _ _ _ _ rfl (show progAt 43 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 44: stx.dw [r10-656], r9  (write PDA seed length to stack)
  rw [executeFn_step _ _ _ _ rfl (show progAt 44 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, writeByWidth, ea_fm_pda_seeds_base_len]
  -- 45: ldx.dw r9, [r1+20760]  (read baseDataLen through stack writes)
  --     Memory is now: writeU64 (writeU64 mem (stack-664) ...) (stack-656) ...
  --     Read at inputAddr+20760 is disjoint from both stack writes.
  rw [executeFn_step _ _ _ _ rfl (show progAt 45 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_20760]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  simp only [h_bdl]
  -- 46: add64 r9, DATA_LEN_MAX_PAD
  rw [executeFn_step _ _ _ _ rfl (show progAt 46 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 47: and64 r9, DATA_LEN_AND_MASK  (8-byte alignment)
  rw [executeFn_step _ _ _ _ rfl (show progAt 47 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 48: add64 r9, r1  (r9 = shifted input address)
  rw [executeFn_step _ _ _ _ rfl (show progAt 48 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- ── Phase 3: Quote dup check (insns 49-50, 12-13, 4 steps) ──
  -- Read quote dup from shifted address, branch to error.
  -- Normalize h_qaddr and h_qdup' to match the goal's address form.
  -- Must use `simp` (not `simp only`) so @[simp] lemmas like the modular
  -- identity (a % m + b) % m = (a + b) % m are included — the step-level
  -- simp applied these to the goal during Phase 2 execution.
  simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_qaddr h_qdup'
  -- 49: ldx.b r8, [r9+31016]  (read quote dup through stack writes)
  rw [executeFn_step _ _ _ _ rfl (show progAt 49 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_31016]
  -- Read through the two stack writes for the byte read
  rw [readU8_writeU64_outside _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  rw [readU8_writeU64_outside _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  -- 50: jne r8, 255, 12 → branch taken (quoteDup ≠ 255)
  rw [executeFn_step _ _ _ _ rfl (show progAt 50 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_qdup']
  -- 12: mov32 r0, E_QUOTE_MINT_IS_DUPLICATE
  rw [executeFn_step _ _ _ _ rfl (show progAt 12 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS]
  -- 13: exit
  rw [executeFn_step _ _ _ _ rfl (show progAt 13 = _ from rfl)]
  simp [step, RegFile.get]

/-! ## P9: PDA integrity — invalid market pubkey → error 9

   Prior checks pass, but the derived PDA doesn't match the market pubkey
   on at least one of 4 8-byte chunks.

   Path: 24 → … → 50(fall) → 51-72 (quote seed + syscall) →
         73-84 (chunk compare, mismatch → 14 → 15)

   Noop syscall: mem universally quantified, PDA result already in memory. -/

private theorem ea_fm_pda_seeds_quote_addr (b : Nat) :
    effectiveAddr b RM_FM_PDA_SEEDS_QUOTE_ADDR_OFF = b - 648 := by
  unfold effectiveAddr RM_FM_PDA_SEEDS_QUOTE_ADDR_OFF; omega

private theorem ea_fm_pda_seeds_quote_len (b : Nat) :
    effectiveAddr b RM_FM_PDA_SEEDS_QUOTE_LEN_OFF = b - 640 := by
  unfold effectiveAddr RM_FM_PDA_SEEDS_QUOTE_LEN_OFF; omega

private theorem ea_quote_data_len (b : Nat) :
    effectiveAddr b RM_MISC_QUOTE_DATA_LEN_OFF = b + 31096 := by
  unfold effectiveAddr RM_MISC_QUOTE_DATA_LEN_OFF; omega

private theorem ea_fm_pda_off (b : Nat) :
    effectiveAddr b RM_FM_PDA_OFF = b - 616 := by
  unfold effectiveAddr RM_FM_PDA_OFF; omega

private theorem ea_fm_bump_off (b : Nat) :
    effectiveAddr b RM_FM_BUMP_OFF = b - 8 := by
  unfold effectiveAddr RM_FM_BUMP_OFF; omega

private theorem ea_fm_pda_chunk0 (b : Nat) :
    effectiveAddr b RM_FM_PDA_CHUNK_0_OFF = b - 616 := by
  unfold effectiveAddr RM_FM_PDA_CHUNK_0_OFF; omega

private theorem ea_fm_pda_chunk1 (b : Nat) :
    effectiveAddr b RM_FM_PDA_CHUNK_1_OFF = b - 608 := by
  unfold effectiveAddr RM_FM_PDA_CHUNK_1_OFF; omega

private theorem ea_fm_pda_chunk2 (b : Nat) :
    effectiveAddr b RM_FM_PDA_CHUNK_2_OFF = b - 600 := by
  unfold effectiveAddr RM_FM_PDA_CHUNK_2_OFF; omega

private theorem ea_fm_pda_chunk3 (b : Nat) :
    effectiveAddr b RM_FM_PDA_CHUNK_3_OFF = b - 592 := by
  unfold effectiveAddr RM_FM_PDA_CHUNK_3_OFF; omega

private theorem ea_mkt_chunk0 (b : Nat) :
    effectiveAddr b IB_MARKET_PUBKEY_CHUNK_0_OFF = b + 10352 := by
  unfold effectiveAddr IB_MARKET_PUBKEY_CHUNK_0_OFF; omega

private theorem ea_mkt_chunk1 (b : Nat) :
    effectiveAddr b IB_MARKET_PUBKEY_CHUNK_1_OFF = b + 10360 := by
  unfold effectiveAddr IB_MARKET_PUBKEY_CHUNK_1_OFF; omega

private theorem ea_mkt_chunk2 (b : Nat) :
    effectiveAddr b IB_MARKET_PUBKEY_CHUNK_2_OFF = b + 10368 := by
  unfold effectiveAddr IB_MARKET_PUBKEY_CHUNK_2_OFF; omega

private theorem ea_mkt_chunk3 (b : Nat) :
    effectiveAddr b IB_MARKET_PUBKEY_CHUNK_3_OFF = b + 10376 := by
  unfold effectiveAddr IB_MARKET_PUBKEY_CHUNK_3_OFF; omega

set_option maxRecDepth 8192 in
set_option maxHeartbeats 64000000 in
theorem rejects_invalid_market_pubkey
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (pda_c0 pda_c1 pda_c2 pda_c3 : Nat)
    (mkt_c0 mkt_c1 mkt_c2 mkt_c3 : Nat)
    -- Common prefix
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    -- Quote dup passes
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    -- PDA chunks on stack (universally quantified via mem)
    (h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = pda_c0)
    (h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = pda_c1)
    (h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = pda_c2)
    (h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = pda_c3)
    -- Market pubkey chunks from input buffer
    (h_mkt_c0 : readU64 mem (inputAddr + 10352) = mkt_c0)
    (h_mkt_c1 : readU64 mem (inputAddr + 10360) = mkt_c1)
    (h_mkt_c2 : readU64 mem (inputAddr + 10368) = mkt_c2)
    (h_mkt_c3 : readU64 mem (inputAddr + 10376) = mkt_c3)
    -- At least one chunk mismatches
    (h_ne : mkt_c0 ≠ pda_c0 ∨ mkt_c1 ≠ pda_c1 ∨ mkt_c2 ≠ pda_c2 ∨ mkt_c3 ≠ pda_c3)
    -- Separation (Solana runtime guarantee: input buffer sits below stack)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 61).exitCode
      = some E_INVALID_MARKET_PUBKEY := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_qeq : readU8 mem
      (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
        inputAddr + 31016) = ACCT_NON_DUP_MARKER := h_qdup
  -- ── Phase 1: Common prefix (insns 24-39, 16 steps) ──
  rw [show (61 : Nat) = 0+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1+1 from rfl]
  -- 24: ldx.dw r3, [r1+0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 24 = _ from rfl)]
  simp [step, initState2, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_0]
  -- 25: ldx.dw r4, [r2-8]
  rw [executeFn_step _ _ _ _ rfl (show progAt 25 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_neg8]
  -- 26: ldx.b r5, [r2+0]
  rw [executeFn_step _ _ _ _ rfl (show progAt 26 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_disc0]
  -- 27: jeq r5, 0, 30 → branch taken (disc = 0)
  rw [executeFn_step _ _ _ _ rfl (show progAt 27 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_disc]
  -- 30-31: acct count + insn len pass
  rw [executeFn_step _ _ _ _ rfl (show progAt 30 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ge]
  rw [executeFn_step _ _ _ _ rfl (show progAt 31 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_ilen]
  -- 32-33: user data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 32 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_88]
  rw [executeFn_step _ _ _ _ rfl (show progAt 33 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_udl]
  -- 34-35: market dup check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 34 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10344]
  rw [executeFn_step _ _ _ _ rfl (show progAt 35 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdup]
  -- 36-37: market data check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 36 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_10424]
  rw [executeFn_step _ _ _ _ rfl (show progAt 37 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_mdl]
  -- 38-39: base dup check passes
  rw [executeFn_step _ _ _ _ rfl (show progAt 38 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_20680]
  rw [executeFn_step _ _ _ _ rfl (show progAt 39 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_bdup]
  -- ── Phase 2: Pointer arithmetic + first 2 stack writes (insns 40-48) ──
  -- 40: mov64 r9, r1
  rw [executeFn_step _ _ _ _ rfl (show progAt 40 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 41: add64 r9, RM_MISC_BASE_ADDR_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 41 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, ea_base_addr_off]
  -- 42: stx.dw [r10-664], r9 (write PDA seed base addr)
  rw [executeFn_step _ _ _ _ rfl (show progAt 42 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, writeByWidth, ea_fm_pda_seeds_base_addr]
  -- 43: mov64 r9, SIZE_OF_ADDRESS
  rw [executeFn_step _ _ _ _ rfl (show progAt 43 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 44: stx.dw [r10-656], r9 (write PDA seed base len)
  rw [executeFn_step _ _ _ _ rfl (show progAt 44 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, writeByWidth, ea_fm_pda_seeds_base_len]
  -- 45: ldx.dw r9, [r1+20760] (read baseDataLen through 2 writes)
  rw [executeFn_step _ _ _ _ rfl (show progAt 45 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_20760]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  simp only [h_bdl]
  -- 46: add64 r9, DATA_LEN_MAX_PAD
  rw [executeFn_step _ _ _ _ rfl (show progAt 46 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 47: and64 r9, DATA_LEN_AND_MASK
  rw [executeFn_step _ _ _ _ rfl (show progAt 47 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 48: add64 r9, r1
  rw [executeFn_step _ _ _ _ rfl (show progAt 48 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- ── Phase 3: Quote dup passes (insns 49-50) ──
  simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_qaddr h_qeq
  -- 49: ldx.b r8, [r9+31016] (read quote dup through 2 writes)
  rw [executeFn_step _ _ _ _ rfl (show progAt 49 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_31016]
  rw [readU8_writeU64_outside _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  rw [readU8_writeU64_outside _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  -- 50: jne r8, 255, 12 → falls through (quote dup = 255)
  rw [executeFn_step _ _ _ _ rfl (show progAt 50 = _ from rfl)]
  simp [step, RegFile.get, resolveSrc, h_qeq]
  -- ── Phase 4: Quote seed setup + 2 more stack writes (insns 51-61) ──
  -- 51: mov64 r8, r9
  rw [executeFn_step _ _ _ _ rfl (show progAt 51 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 52: add64 r8, RM_MISC_QUOTE_ADDR_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 52 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 53: stx.dw [r10-648], r8 (write PDA quote seed addr)
  rw [executeFn_step _ _ _ _ rfl (show progAt 53 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, writeByWidth, ea_fm_pda_seeds_quote_addr]
  -- 54: ldx.dw r8, [r9+31096] (read quoteDataLen through 3 writes)
  rw [executeFn_step _ _ _ _ rfl (show progAt 54 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_quote_data_len]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  simp
  -- 55: add64 r8, DATA_LEN_MAX_PAD
  rw [executeFn_step _ _ _ _ rfl (show progAt 55 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 56: and64 r8, DATA_LEN_AND_MASK
  rw [executeFn_step _ _ _ _ rfl (show progAt 56 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 57: add64 r9, RM_MISC_QUOTE_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 57 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 58: add64 r9, r8
  rw [executeFn_step _ _ _ _ rfl (show progAt 58 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 59: add64 r9, SIZE_OF_EMPTY_ACCOUNT
  rw [executeFn_step _ _ _ _ rfl (show progAt 59 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 60: mov64 r8, SIZE_OF_ADDRESS
  rw [executeFn_step _ _ _ _ rfl (show progAt 60 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 61: stx.dw [r10-640], r8 (write PDA quote seed len)
  rw [executeFn_step _ _ _ _ rfl (show progAt 61 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, writeByWidth, ea_fm_pda_seeds_quote_len]
  -- ── Phase 5: Syscall setup + noop syscall (insns 62-72) ──
  -- 62: mov64 r6, r1
  rw [executeFn_step _ _ _ _ rfl (show progAt 62 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 63: mov64 r1, r10
  rw [executeFn_step _ _ _ _ rfl (show progAt 63 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 64: add64 r1, RM_FM_PDA_SEEDS_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 64 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 65: mov64 r3, r2
  rw [executeFn_step _ _ _ _ rfl (show progAt 65 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 66: add64 r3, REGISTER_MARKET_DATA_LEN
  rw [executeFn_step _ _ _ _ rfl (show progAt 66 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 67: mov64 r2, RM_MISC_TRY_FIND_PDA_SEEDS_LEN
  rw [executeFn_step _ _ _ _ rfl (show progAt 67 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 68: mov64 r4, r10
  rw [executeFn_step _ _ _ _ rfl (show progAt 68 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 69: add64 r4, RM_FM_PDA_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 69 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 70: mov64 r5, r10
  rw [executeFn_step _ _ _ _ rfl (show progAt 70 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 71: add64 r5, RM_FM_BUMP_OFF
  rw [executeFn_step _ _ _ _ rfl (show progAt 71 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc]
  -- 72: call sol_try_find_program_address (noop: r0=0, mem unchanged)
  rw [executeFn_step _ _ _ _ rfl (show progAt 72 = _ from rfl)]
  simp [step, RegFile.get, RegFile.set, resolveSrc, execSyscall]
  -- ── Phase 6: PDA chunk comparison with by_cases (insns 73-84) ──
  -- 73: ldx.dw r7, [r6+10352] (read market pubkey chunk 0 through 4 writes)
  rw [executeFn_step _ _ _ _ rfl (show progAt 73 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_mkt_chunk0]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  simp only [h_mkt_c0]
  -- 74: ldx.dw r8, [r10-616] (read PDA chunk 0 through 4 writes)
  rw [executeFn_step _ _ _ _ rfl (show progAt 74 = _ from rfl)]
  simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_fm_pda_chunk0]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by right; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by right; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by right; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by right; unfold STACK_START at h_sep ⊢; omega)]
  simp only [h_pda_c0]
  -- 75: jne r7, r8, 14 → compare chunk 0
  by_cases h_eq0 : mkt_c0 = pda_c0
  · -- Chunk 0 matches: fall through
    rw [executeFn_step _ _ _ _ rfl (show progAt 75 = _ from rfl)]
    simp [step, RegFile.get, resolveSrc, h_eq0]
    -- 76: ldx.dw r7, [r6+10360] (market chunk 1)
    rw [executeFn_step _ _ _ _ rfl (show progAt 76 = _ from rfl)]
    simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_mkt_chunk1]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by left; unfold STACK_START at h_sep ⊢; omega)]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by left; unfold STACK_START at h_sep ⊢; omega)]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by left; unfold STACK_START at h_sep ⊢; omega)]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by left; unfold STACK_START at h_sep ⊢; omega)]
    simp only [h_mkt_c1]
    -- 77: ldx.dw r8, [r10-608] (PDA chunk 1)
    rw [executeFn_step _ _ _ _ rfl (show progAt 77 = _ from rfl)]
    simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_fm_pda_chunk1]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by right; unfold STACK_START at h_sep ⊢; omega)]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by right; unfold STACK_START at h_sep ⊢; omega)]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by right; unfold STACK_START at h_sep ⊢; omega)]
    rw [readU64_writeU64_disjoint _ _ _ _
      (by right; unfold STACK_START at h_sep ⊢; omega)]
    simp only [h_pda_c1]
    -- 78: jne r7, r8, 14 → compare chunk 1
    by_cases h_eq1 : mkt_c1 = pda_c1
    · -- Chunk 1 matches
      rw [executeFn_step _ _ _ _ rfl (show progAt 78 = _ from rfl)]
      simp [step, RegFile.get, resolveSrc, h_eq1]
      -- 79: ldx.dw r7, [r6+10368] (market chunk 2)
      rw [executeFn_step _ _ _ _ rfl (show progAt 79 = _ from rfl)]
      simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_mkt_chunk2]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by left; unfold STACK_START at h_sep ⊢; omega)]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by left; unfold STACK_START at h_sep ⊢; omega)]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by left; unfold STACK_START at h_sep ⊢; omega)]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by left; unfold STACK_START at h_sep ⊢; omega)]
      simp only [h_mkt_c2]
      -- 80: ldx.dw r8, [r10-600] (PDA chunk 2)
      rw [executeFn_step _ _ _ _ rfl (show progAt 80 = _ from rfl)]
      simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_fm_pda_chunk2]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by right; unfold STACK_START at h_sep ⊢; omega)]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by right; unfold STACK_START at h_sep ⊢; omega)]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by right; unfold STACK_START at h_sep ⊢; omega)]
      rw [readU64_writeU64_disjoint _ _ _ _
        (by right; unfold STACK_START at h_sep ⊢; omega)]
      simp only [h_pda_c2]
      -- 81: jne r7, r8, 14 → compare chunk 2
      by_cases h_eq2 : mkt_c2 = pda_c2
      · -- Chunks 0-2 match → chunk 3 must mismatch
        have h_ne3 : mkt_c3 ≠ pda_c3 := by
          rcases h_ne with h | h | h | h
          · exact absurd h_eq0 h
          · exact absurd h_eq1 h
          · exact absurd h_eq2 h
          · exact h
        rw [executeFn_step _ _ _ _ rfl (show progAt 81 = _ from rfl)]
        simp [step, RegFile.get, resolveSrc, h_eq2]
        -- 82: ldx.dw r7, [r6+10376] (market chunk 3)
        rw [executeFn_step _ _ _ _ rfl (show progAt 82 = _ from rfl)]
        simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_mkt_chunk3]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by left; unfold STACK_START at h_sep ⊢; omega)]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by left; unfold STACK_START at h_sep ⊢; omega)]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by left; unfold STACK_START at h_sep ⊢; omega)]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by left; unfold STACK_START at h_sep ⊢; omega)]
        simp only [h_mkt_c3]
        -- 83: ldx.dw r8, [r10-592] (PDA chunk 3)
        rw [executeFn_step _ _ _ _ rfl (show progAt 83 = _ from rfl)]
        simp only [step, RegFile.get, RegFile.set, resolveSrc, readByWidth, ea_fm_pda_chunk3]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by right; unfold STACK_START at h_sep ⊢; omega)]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by right; unfold STACK_START at h_sep ⊢; omega)]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by right; unfold STACK_START at h_sep ⊢; omega)]
        rw [readU64_writeU64_disjoint _ _ _ _
          (by right; unfold STACK_START at h_sep ⊢; omega)]
        simp only [h_pda_c3]
        -- 84: jne r7, r8, 14 → mismatch
        rw [executeFn_step _ _ _ _ rfl (show progAt 84 = _ from rfl)]
        simp [step, RegFile.get, resolveSrc, h_ne3]
        error_exit
      · -- Chunk 2 mismatches
        rw [executeFn_step _ _ _ _ rfl (show progAt 81 = _ from rfl)]
        simp [step, RegFile.get, resolveSrc, h_eq2]
        error_exit
    · -- Chunk 1 mismatches
      rw [executeFn_step _ _ _ _ rfl (show progAt 78 = _ from rfl)]
      simp [step, RegFile.get, resolveSrc, h_eq1]
      error_exit
  · -- Chunk 0 mismatches
    rw [executeFn_step _ _ _ _ rfl (show progAt 75 = _ from rfl)]
    simp [step, RegFile.get, resolveSrc, h_eq0]
    error_exit

end DropsetProofs
