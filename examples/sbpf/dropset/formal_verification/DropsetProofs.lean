-- Formal verification of the DASMAC dropset program (validation guards)
--
-- Source: dropset.s — a Solana on-chain order book (sBPF assembly).
-- Implements RegisterMarket: validates accounts, derives PDA, creates account via CPI.
--
-- We verify the validation prefix: discriminant dispatch, account count,
-- instruction length, and per-account duplicate/data checks.
--
-- P1-P7: Use wp_exec for one-shot proofs (simple linear paths).
-- P8-P9: Use manual executeFn_step (memory disjointness through stack writes).

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, U32_MODULUS]

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, U32_MODULUS]

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, U32_MODULUS]

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, ea_88, U32_MODULUS]

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, ea_88, ea_10344, U32_MODULUS]

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, ea_88, ea_10344, ea_10424, U32_MODULUS]

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
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_neg8, ea_disc0, ea_88, ea_10344, ea_10424, ea_20680, U32_MODULUS]

/-! ## P8: quote mint is duplicate → error 8

   Prior checks pass, base mint not dup, but the quote mint at the shifted
   input position has dup ≠ 255.

   Path: 24 → … → 39(fall) → 40-48 (pointer arith + stack writes) → 49 → 50 → 12 → 13

   Complexity: instructions 42/44 write PDA seeds to the stack (mutating mem),
   instruction 47 is and64 with -8 for 8-byte alignment, and instruction 49
   reads from a computed address. The proof requires memory disjointness
   axioms to show stack writes don't affect input buffer reads.

   Uses manual executeFn_step due to memory disjointness between steps. -/

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
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = quoteDup)
    (h_qdup_ne: quoteDup ≠ ACCT_NON_DUP_MARKER)
    -- Stack-input separation (Solana runtime guarantee)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 30).exitCode
      = some E_QUOTE_MINT_IS_DUPLICATE := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  have h_qdup' : ¬(readU8 mem
      (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
        inputAddr + 31016) = ACCT_NON_DUP_MARKER) := by rw [h_qdup]; exact h_qdup_ne
  rw [executeFn_eq_execSegment]
  -- ── Phase 1: Common prefix + pointer arith + stack writes (insns 24-44, 19 steps) ──
  iterate 19 (wp_step [progAt, progAt_0, progAt_1, writeByWidth]
    [ea_0, ea_neg8, ea_disc0, ea_88, ea_10344, ea_10424, ea_20680,
     ea_base_addr_off, ea_fm_pda_seeds_base_addr, ea_fm_pda_seeds_base_len, U32_MODULUS])
  -- ── Phase 2: Read baseDataLen through 2 stack writes (insn 45) ──
  unfold execSegment
  dsimp (config := { failIfUnchanged := false })
    [progAt, progAt_0, progAt_1, execInsn, RegFile.get, RegFile.set, resolveSrc, readByWidth]
  simp (config := { failIfUnchanged := false }) [ea_20760, *]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  rw [readU64_writeU64_disjoint _ _ _ _
    (by left; unfold STACK_START at h_sep ⊢; omega)]
  simp (config := { failIfUnchanged := false }) [h_bdl, *]
  -- ── Phase 3: Pointer arith (insns 46-48, 3 steps) ──
  iterate 3 (wp_step [progAt, progAt_0, progAt_1] [])
  -- Normalize addresses for quote dup read
  simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_qaddr h_qdup'
  -- ── Phase 4: Read quote dup through 2 stack writes (insn 49) ──
  unfold execSegment
  dsimp (config := { failIfUnchanged := false })
    [progAt, progAt_0, progAt_1, execInsn, RegFile.get, RegFile.set, resolveSrc, readByWidth]
  simp (config := { failIfUnchanged := false }) [ea_31016, *]
  rw [readU8_writeU64_outside _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  rw [readU8_writeU64_outside _ _ _ _
    (by left; unfold STACK_START at h_qaddr ⊢; omega)]
  -- ── Phase 5: Branch to error + exit (insns 50, 12, 13) ──
  iterate 3 (wp_step [progAt, progAt_0, progAt_1] [U32_MODULUS])
  rfl

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

-- Close the error exit path: mov32 r0, E_INVALID_MARKET_PUBKEY (insn 14) + exit (insn 15).
set_option hygiene false in
local macro "error_exit" : tactic => `(tactic| (
  rw [executeFn_step _ _ _ _ rfl (show progAt 14 = _ from rfl)];
  simp [step, RegFile.get, RegFile.set, resolveSrc, U32_MODULUS];
  rw [executeFn_step _ _ _ _ rfl (show progAt 15 = _ from rfl)];
  simp [step, RegFile.get]))

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
  sorry -- TODO: convert P9 from executeFn_step to monadic wp_step

end DropsetProofs
