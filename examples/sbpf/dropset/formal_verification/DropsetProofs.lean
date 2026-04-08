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

set_option maxRecDepth 65536

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
  wp_step [progAt, progAt_0, progAt_1] [ea_20760]
  strip_writes
  simp [*]
  -- ── Phase 3: Pointer arith (insns 46-48, 3 steps) ──
  iterate 3 (wp_step [progAt, progAt_0, progAt_1] [])
  -- Normalize addresses for quote dup read
  simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_qaddr h_qdup'
  -- ── Phase 4: Read quote dup through 2 stack writes (insn 49) ──
  wp_step [progAt, progAt_0, progAt_1] [ea_31016]
  strip_writes
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

-- P9: Split proof for chunk comparison.
-- Helper lemmas handle ldx+ldx+jne pattern in isolation (shallow proof terms),
-- avoiding kernel depth issues from wp_step_from macro expansion.

/-! ### Chunk comparison helpers

    The chunk comparison at PCs 73-84 follows a regular pattern:
    ldx r7 (market chunk), ldx r8 (PDA chunk), jne r7 r8 14.
    On mismatch, jne jumps to PC 14 (mov32 r0 9, exit).
    On match, execution falls through to the next chunk. -/

-- Chunk mismatch: ldx + ldx + jne(taken) + mov32 + exit = error in ≤n steps.
set_option maxHeartbeats 800000
private theorem p9_chunk_ne_error (inputAddr : Nat) (s : State) (n : Nat)
    (off1 off2 : Int)
    (h_exit : s.exitCode = none)
    (h_r6   : s.regs.r6 = inputAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r6 off1))
    (h_f2 : progAt (s.pc + 1) = some (.ldx .dword .r8 .r10 off2))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 14))
    (h_ne : readU64 s.mem (effectiveAddr inputAddr off1) ≠
            readU64 s.mem (effectiveAddr (STACK_START + 0x1000) off2))
    (h_fuel : n ≥ 5) :
    (executeFn progAt s n).exitCode = some E_INVALID_MARKET_PUBKEY := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  suffices h5 : (executeFn progAt s 5).exitCode = some E_INVALID_MARKET_PUBKEY by
    rw [executeFn_halted _ _ _ _ h5]; exact h5
  -- Precompute fetch values for the error handler (PCs 14-15) to avoid expensive progAt_0 reduction
  have hf14 : progAt 14 = some (.mov32 .r0 (.imm E_INVALID_MARKET_PUBKEY)) := by native_decide
  have hf15 : progAt 15 = some (.exit) := by native_decide
  rw [show (5 : Nat) = 1 + (1 + (1 + (1 + (1 + 0)))) from rfl]
  iterate 4 (rw [executeFn_compose])
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc, toU64,
    h_exit, h_r6, h_r10, h_f1, h_f2, h_f3, h_ne,
    hf14, hf15, E_INVALID_MARKET_PUBKEY, U32_MODULUS]

-- Chunk match: ldx + ldx + jne(fallthrough) advances pc by 3, preserves key state.
set_option maxHeartbeats 1600000 in
private theorem p9_chunk_eq_state (inputAddr : Nat) (s : State)
    (off1 off2 : Int)
    (h_exit : s.exitCode = none)
    (h_r6   : s.regs.r6 = inputAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r6 off1))
    (h_f2 : progAt (s.pc + 1) = some (.ldx .dword .r8 .r10 off2))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 14))
    (h_eq : readU64 s.mem (effectiveAddr inputAddr off1) =
            readU64 s.mem (effectiveAddr (STACK_START + 0x1000) off2)) :
    (executeFn progAt s 3).exitCode = none ∧
    (executeFn progAt s 3).pc = s.pc + 3 ∧
    (executeFn progAt s 3).mem = s.mem ∧
    (executeFn progAt s 3).regs.r3 = s.regs.r3 ∧
    (executeFn progAt s 3).regs.r5 = s.regs.r5 ∧
    (executeFn progAt s 3).regs.r6 = inputAddr ∧
    (executeFn progAt s 3).regs.r9 = s.regs.r9 ∧
    (executeFn progAt s 3).regs.r10 = STACK_START + 0x1000 := by
  rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
  iterate 2 (rw [executeFn_compose])
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_r6, h_r10, h_f1, h_f2, h_f3, h_eq]

/-! ### Part 3: Chunk comparison from abstract state

    At PC 73, r6 = inputAddr (market account base), r10 = frame ptr.
    Uses by_cases on each chunk: mismatch → p9_chunk_ne_error,
    match → p9_chunk_eq_state + continue. -/

set_option maxHeartbeats 4000000 in
private theorem p9_chunk_compare
    (inputAddr : Nat) (s : State)
    (mkt_c0 mkt_c1 mkt_c2 mkt_c3 : Nat)
    (pda_c0 pda_c1 pda_c2 pda_c3 : Nat)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 73)
    (h_r6   : s.regs.r6 = inputAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_mkt_c0 : readU64 s.mem (inputAddr + 10352) = mkt_c0)
    (h_mkt_c1 : readU64 s.mem (inputAddr + 10360) = mkt_c1)
    (h_mkt_c2 : readU64 s.mem (inputAddr + 10368) = mkt_c2)
    (h_mkt_c3 : readU64 s.mem (inputAddr + 10376) = mkt_c3)
    (h_pda_c0 : readU64 s.mem (STACK_START + 0x1000 - 616) = pda_c0)
    (h_pda_c1 : readU64 s.mem (STACK_START + 0x1000 - 608) = pda_c1)
    (h_pda_c2 : readU64 s.mem (STACK_START + 0x1000 - 600) = pda_c2)
    (h_pda_c3 : readU64 s.mem (STACK_START + 0x1000 - 592) = pda_c3)
    (h_ne : mkt_c0 ≠ pda_c0 ∨ mkt_c1 ≠ pda_c1 ∨ mkt_c2 ≠ pda_c2 ∨ mkt_c3 ≠ pda_c3) :
    (executeFn progAt s 14).exitCode = some E_INVALID_MARKET_PUBKEY := by
  -- Pre-compute all fetch values for chunk comparison PCs (73-84)
  have hf73 : progAt 73 = some (.ldx .dword .r7 .r6 IB_MARKET_PUBKEY_CHUNK_0_OFF) := by native_decide
  have hf74 : progAt 74 = some (.ldx .dword .r8 .r10 RM_FM_PDA_CHUNK_0_OFF) := by native_decide
  have hf75 : progAt 75 = some (.jne .r7 (.reg .r8) 14) := by native_decide
  have hf76 : progAt 76 = some (.ldx .dword .r7 .r6 IB_MARKET_PUBKEY_CHUNK_1_OFF) := by native_decide
  have hf77 : progAt 77 = some (.ldx .dword .r8 .r10 RM_FM_PDA_CHUNK_1_OFF) := by native_decide
  have hf78 : progAt 78 = some (.jne .r7 (.reg .r8) 14) := by native_decide
  have hf79 : progAt 79 = some (.ldx .dword .r7 .r6 IB_MARKET_PUBKEY_CHUNK_2_OFF) := by native_decide
  have hf80 : progAt 80 = some (.ldx .dword .r8 .r10 RM_FM_PDA_CHUNK_2_OFF) := by native_decide
  have hf81 : progAt 81 = some (.jne .r7 (.reg .r8) 14) := by native_decide
  have hf82 : progAt 82 = some (.ldx .dword .r7 .r6 IB_MARKET_PUBKEY_CHUNK_3_OFF) := by native_decide
  have hf83 : progAt 83 = some (.ldx .dword .r8 .r10 RM_FM_PDA_CHUNK_3_OFF) := by native_decide
  have hf84 : progAt 84 = some (.jne .r7 (.reg .r8) 14) := by native_decide
  by_cases h_eq0 : mkt_c0 = pda_c0
  · -- Chunk 0 matches → advance 3 steps
    simp [h_eq0] at h_ne
    rw [show (14 : Nat) = 3 + 11 from rfl, executeFn_compose]
    have hst := p9_chunk_eq_state inputAddr s
      IB_MARKET_PUBKEY_CHUNK_0_OFF RM_FM_PDA_CHUNK_0_OFF
      h_exit h_r6 h_r10
      (by rw [h_pc]; exact hf73) (by rw [h_pc]; exact hf74) (by rw [h_pc]; exact hf75)
      (by rw [ea_mkt_chunk0, ea_fm_pda_chunk0, h_mkt_c0, h_pda_c0]; exact h_eq0)
    obtain ⟨h_exit₁, h_pc₁, h_mem₁, _, _, h_r6₁, _, h_r10₁⟩ := hst
    by_cases h_eq1 : mkt_c1 = pda_c1
    · -- Chunk 1 matches
      simp [h_eq1] at h_ne
      rw [show (11 : Nat) = 3 + 8 from rfl, executeFn_compose]
      have hst := p9_chunk_eq_state inputAddr (executeFn progAt s 3)
        IB_MARKET_PUBKEY_CHUNK_1_OFF RM_FM_PDA_CHUNK_1_OFF
        h_exit₁ h_r6₁ h_r10₁
        (by rw [h_pc₁, h_pc]; exact hf76) (by rw [h_pc₁, h_pc]; exact hf77)
        (by rw [h_pc₁, h_pc]; exact hf78)
        (by rw [ea_mkt_chunk1, ea_fm_pda_chunk1, h_mem₁, h_mkt_c1, h_pda_c1]; exact h_eq1)
      obtain ⟨h_exit₂, h_pc₂, h_mem₂, _, _, h_r6₂, _, h_r10₂⟩ := hst
      by_cases h_eq2 : mkt_c2 = pda_c2
      · -- Chunk 2 matches → chunk 3 must mismatch
        simp [h_eq2] at h_ne
        rw [show (8 : Nat) = 3 + 5 from rfl, executeFn_compose]
        have hst := p9_chunk_eq_state inputAddr
          (executeFn progAt (executeFn progAt s 3) 3)
          IB_MARKET_PUBKEY_CHUNK_2_OFF RM_FM_PDA_CHUNK_2_OFF
          h_exit₂ h_r6₂ h_r10₂
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf79) (by rw [h_pc₂, h_pc₁, h_pc]; exact hf80)
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf81)
          (by rw [ea_mkt_chunk2, ea_fm_pda_chunk2, h_mem₂, h_mem₁, h_mkt_c2, h_pda_c2]; exact h_eq2)
        obtain ⟨h_exit₃, h_pc₃, h_mem₃, _, _, h_r6₃, _, h_r10₃⟩ := hst
        exact p9_chunk_ne_error inputAddr
          (executeFn progAt (executeFn progAt (executeFn progAt s 3) 3) 3)
          5 IB_MARKET_PUBKEY_CHUNK_3_OFF RM_FM_PDA_CHUNK_3_OFF
          h_exit₃ h_r6₃ h_r10₃
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf82)
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf83)
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf84)
          (by rw [ea_mkt_chunk3, ea_fm_pda_chunk3, h_mem₃, h_mem₂, h_mem₁,
                  h_mkt_c3, h_pda_c3]; exact h_ne)
          (by omega)
      · -- Chunk 2 mismatches
        exact p9_chunk_ne_error inputAddr
          (executeFn progAt (executeFn progAt s 3) 3)
          8 IB_MARKET_PUBKEY_CHUNK_2_OFF RM_FM_PDA_CHUNK_2_OFF
          h_exit₂ h_r6₂ h_r10₂
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf79) (by rw [h_pc₂, h_pc₁, h_pc]; exact hf80)
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf81)
          (by rw [ea_mkt_chunk2, ea_fm_pda_chunk2, h_mem₂, h_mem₁,
                  h_mkt_c2, h_pda_c2]; exact h_eq2)
          (by omega)
    · -- Chunk 1 mismatches
      exact p9_chunk_ne_error inputAddr (executeFn progAt s 3)
        11 IB_MARKET_PUBKEY_CHUNK_1_OFF RM_FM_PDA_CHUNK_1_OFF
        h_exit₁ h_r6₁ h_r10₁
        (by rw [h_pc₁, h_pc]; exact hf76) (by rw [h_pc₁, h_pc]; exact hf77)
        (by rw [h_pc₁, h_pc]; exact hf78)
        (by rw [ea_mkt_chunk1, ea_fm_pda_chunk1, h_mem₁,
                h_mkt_c1, h_pda_c1]; exact h_eq1)
        (by omega)
  · -- Chunk 0 mismatches
    exact p9_chunk_ne_error inputAddr s 14
      IB_MARKET_PUBKEY_CHUNK_0_OFF RM_FM_PDA_CHUNK_0_OFF
      h_exit h_r6 h_r10
      (by rw [h_pc]; exact hf73) (by rw [h_pc]; exact hf74) (by rw [h_pc]; exact hf75)
      (by rw [ea_mkt_chunk0, ea_fm_pda_chunk0, h_mkt_c0, h_pda_c0]; exact h_eq0)
      (by omega)

-- Part 1: 25 steps from initState2 to PC 51 (quote dup passes).
-- Proves state properties + memory form (existential avoids expensive strip_writes_goal).
-- Writes at PCs 42 (-664) and 44 (-656); exact values left existential.
set_option maxHeartbeats 40000000 in
private theorem p9_part1
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    let s := executeFn progAt (initState2 inputAddr insnAddr mem 24) 25
    s.exitCode = none ∧ s.pc = 51 ∧
    s.regs.r1 = inputAddr ∧ s.regs.r10 = STACK_START + 0x1000 ∧
    ∃ v1 v2, s.mem = writeU64 (writeU64 mem (STACK_START + 0x1000 - 664) v1) (STACK_START + 0x1000 - 656) v2 := by
  have h_ge : ¬(readU64 mem inputAddr < REGISTER_MARKET_ACCOUNTS_LEN) := by rw [h_num]; exact h_enough
  rw [executeFn_eq_execSegment]
  iterate 19 (wp_step [progAt, progAt_0, progAt_1, writeByWidth]
    [ea_0, ea_neg8, ea_disc0, ea_88, ea_10344, ea_10424, ea_20680,
     ea_base_addr_off, ea_fm_pda_seeds_base_addr, ea_fm_pda_seeds_base_len, U32_MODULUS])
  wp_step [progAt, progAt_0, progAt_1] [ea_20760]
  strip_writes
  simp [*]
  iterate 3 (wp_step [progAt, progAt_0, progAt_1] [])
  simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_qaddr h_qdup
  wp_step [progAt, progAt_0, progAt_1] [ea_31016]
  strip_writes
  wp_step [progAt, progAt_0, progAt_1] []
  exact ⟨rfl, rfl, rfl, rfl, _, _, rfl⟩

-- Part 2a: 11 steps from abstract state at PC 51 to PC 62.
-- Uses executeFn decomposition + single simp only (avoids kernel depth from sequential wp_step_from).
-- Two stack writes at PCs 53, 61; existential form avoids expensive strip_writes.
set_option maxHeartbeats 40000000 in
private theorem p9_part2a
    (inputAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 51)
    (h_r1   : s.regs.r1 = inputAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 11
    s'.exitCode = none ∧ s'.pc = 62 ∧ s'.regs.r1 = inputAddr ∧
    s'.regs.r10 = STACK_START + 0x1000 ∧
    ∃ w1 w2, s'.mem = writeU64 (writeU64 s.mem (STACK_START + 0x1000 - 648) w1) (STACK_START + 0x1000 - 640) w2 := by
  have hf51 : progAt 51 = some (.mov64 .r8 (.reg .r9)) := by native_decide
  have hf52 : progAt 52 = some (.add64 .r8 (.imm RM_MISC_QUOTE_ADDR_OFF)) := by native_decide
  have hf53 : progAt 53 = some (.stx .dword .r10 RM_FM_PDA_SEEDS_QUOTE_ADDR_OFF .r8) := by native_decide
  have hf54 : progAt 54 = some (.ldx .dword .r8 .r9 RM_MISC_QUOTE_DATA_LEN_OFF) := by native_decide
  have hf55 : progAt 55 = some (.add64 .r8 (.imm DATA_LEN_MAX_PAD)) := by native_decide
  have hf56 : progAt 56 = some (.and64 .r8 (.imm DATA_LEN_AND_MASK)) := by native_decide
  have hf57 : progAt 57 = some (.add64 .r9 (.imm RM_MISC_QUOTE_OFF)) := by native_decide
  have hf58 : progAt 58 = some (.add64 .r9 (.reg .r8)) := by native_decide
  have hf59 : progAt 59 = some (.add64 .r9 (.imm SIZE_OF_EMPTY_ACCOUNT)) := by native_decide
  have hf60 : progAt 60 = some (.mov64 .r8 (.imm SIZE_OF_ADDRESS)) := by native_decide
  have hf61 : progAt 61 = some (.stx .dword .r10 RM_FM_PDA_SEEDS_QUOTE_LEN_OFF .r8) := by native_decide
  -- Decompose into 11 single-step calls
  rw [show (11 : Nat) = 1+1+1+1+1+1+1+1+1+1+1 from rfl]
  iterate 10 (rw [executeFn_compose])
  -- Evaluate all steps with explicit lemma list; existential avoids strip_writes
  simp only [executeFn, step,
    RegFile.get, RegFile.set, resolveSrc, readByWidth, writeByWidth,
    ea_fm_pda_seeds_quote_addr, ea_fm_pda_seeds_quote_len,
    h_exit, h_pc, h_r1, h_r10,
    hf51, hf52, hf53, hf54, hf55, hf56, hf57, hf58, hf59, hf60, hf61]
  exact ⟨trivial, trivial, trivial, trivial, _, _, rfl⟩

-- Part 2b: 11 steps from abstract state at PC 62 to PC 73.
-- Uses executeFn decomposition + single simp only (avoids kernel depth from sequential wp_step_from).
set_option maxHeartbeats 40000000 in
private theorem p9_part2b
    (inputAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 62)
    (h_r1   : s.regs.r1 = inputAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 11
    s'.exitCode = none ∧ s'.pc = 73 ∧ s'.regs.r6 = inputAddr ∧
    s'.regs.r10 = STACK_START + 0x1000 ∧ s'.mem = s.mem := by
  have hf62 : progAt 62 = some (.mov64 .r6 (.reg .r1)) := by native_decide
  have hf63 : progAt 63 = some (.mov64 .r1 (.reg .r10)) := by native_decide
  have hf64 : progAt 64 = some (.add64 .r1 (.imm RM_FM_PDA_SEEDS_OFF)) := by native_decide
  have hf65 : progAt 65 = some (.mov64 .r3 (.reg .r2)) := by native_decide
  have hf66 : progAt 66 = some (.add64 .r3 (.imm REGISTER_MARKET_DATA_LEN)) := by native_decide
  have hf67 : progAt 67 = some (.mov64 .r2 (.imm RM_MISC_TRY_FIND_PDA_SEEDS_LEN)) := by native_decide
  have hf68 : progAt 68 = some (.mov64 .r4 (.reg .r10)) := by native_decide
  have hf69 : progAt 69 = some (.add64 .r4 (.imm RM_FM_PDA_OFF)) := by native_decide
  have hf70 : progAt 70 = some (.mov64 .r5 (.reg .r10)) := by native_decide
  have hf71 : progAt 71 = some (.add64 .r5 (.imm RM_FM_BUMP_OFF)) := by native_decide
  have hf72 : progAt 72 = some (.call .sol_try_find_program_address) := by native_decide
  -- Decompose into 11 single-step calls
  rw [show (11 : Nat) = 1+1+1+1+1+1+1+1+1+1+1 from rfl]
  iterate 10 (rw [executeFn_compose])
  -- Evaluate all steps at once; explicit lemma list avoids kernel depth from simp [*]
  simp only [executeFn, step, execSyscall,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_pc, h_r1, h_r10,
    hf62, hf63, hf64, hf65, hf66, hf67, hf68, hf69, hf70, hf71, hf72]
  simp

-- Prefix: 47 steps from initState2 to chunk comparison state.
-- Composes p9_part1 (25 steps) + p9_part2a (11 steps) + p9_part2b (11 steps).
-- Returns existential memory form (4 writes) — avoids expensive strip_writes.
-- The main theorem does strip_writes to provide concrete reads to p9_chunk_compare.
private theorem p9_prefix
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    let s := executeFn progAt (initState2 inputAddr insnAddr mem 24) 47
    s.exitCode = none ∧ s.pc = 73 ∧ s.regs.r6 = inputAddr ∧
    s.regs.r10 = STACK_START + 0x1000 ∧
    ∃ v1 v2 w1 w2, s.mem = writeU64 (writeU64 (writeU64 (writeU64 mem
      (STACK_START + 0x1000 - 664) v1) (STACK_START + 0x1000 - 656) v2)
      (STACK_START + 0x1000 - 648) w1) (STACK_START + 0x1000 - 640) w2 := by
  -- Decompose 47 = 25 + 11 + 11
  rw [show (47 : Nat) = 25 + (11 + 11) from rfl, executeFn_compose, executeFn_compose]
  -- Part 1: 25 steps, state props + memory form (2 writes at -664, -656)
  obtain ⟨h1e, h1p, h1r1, h1r10, v1, v2, hmem1⟩ :=
    p9_part1 inputAddr insnAddr mem nAccounts baseDataLen
      h_disc h_num h_enough h_ilen h_udl h_mdup h_mdl h_bdup h_bdl h_qdup h_sep h_qaddr
  -- Part 2a: 11 steps, state props + memory form (2 writes at -648, -640)
  obtain ⟨h2e, h2p, h2r1, h2r10, w1, w2, hmem2⟩ :=
    p9_part2a inputAddr _ h1e h1p h1r1 h1r10
  -- Part 2b: 11 steps, state props + mem unchanged
  obtain ⟨h3e, h3p, h3r6, h3r10, h3mem⟩ :=
    p9_part2b inputAddr _ h2e h2p h2r1 h2r10
  -- State props + compose memory: s3.mem = s2.mem = writes(s1.mem) = writes(writes(mem))
  exact ⟨h3e, h3p, h3r6, h3r10, v1, v2, w1, w2, by rw [h3mem, hmem2, hmem1]⟩

-- Main theorem: compose prefix (47 steps) + chunk comparison (14 steps)
set_option maxHeartbeats 800000 in
theorem rejects_invalid_market_pubkey
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (pda_c0 pda_c1 pda_c2 pda_c3 : Nat)
    (mkt_c0 mkt_c1 mkt_c2 mkt_c3 : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    (h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = pda_c0)
    (h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = pda_c1)
    (h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = pda_c2)
    (h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = pda_c3)
    (h_mkt_c0 : readU64 mem (inputAddr + 10352) = mkt_c0)
    (h_mkt_c1 : readU64 mem (inputAddr + 10360) = mkt_c1)
    (h_mkt_c2 : readU64 mem (inputAddr + 10368) = mkt_c2)
    (h_mkt_c3 : readU64 mem (inputAddr + 10376) = mkt_c3)
    (h_ne : mkt_c0 ≠ pda_c0 ∨ mkt_c1 ≠ pda_c1 ∨ mkt_c2 ≠ pda_c2 ∨ mkt_c3 ≠ pda_c3)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 61).exitCode
      = some E_INVALID_MARKET_PUBKEY := by
  rw [show (61 : Nat) = 47 + 14 from rfl, executeFn_compose]
  obtain ⟨he, hp, hr6, hr10, v1, v2, w1, w2, hmem⟩ :=
    p9_prefix inputAddr insnAddr mem nAccounts baseDataLen
      h_disc h_num h_enough h_ilen h_udl h_mdup h_mdl h_bdup h_bdl h_qdup h_sep h_qaddr
  -- Strip 4 writes from each memory read (omega needs STACK_START unfolded)
  unfold STACK_START at h_sep
  exact p9_chunk_compare inputAddr _ mkt_c0 mkt_c1 mkt_c2 mkt_c3
    pda_c0 pda_c1 pda_c2 pda_c3 he hp hr6 hr10
    (by rw [hmem]; strip_writes_goal; exact h_mkt_c0)
    (by rw [hmem]; strip_writes_goal; exact h_mkt_c1)
    (by rw [hmem]; strip_writes_goal; exact h_mkt_c2)
    (by rw [hmem]; strip_writes_goal; exact h_mkt_c3)
    (by rw [hmem]; strip_writes_goal; exact h_pda_c0)
    (by rw [hmem]; strip_writes_goal; exact h_pda_c1)
    (by rw [hmem]; strip_writes_goal; exact h_pda_c2)
    (by rw [hmem]; strip_writes_goal; exact h_pda_c3)
    h_ne

/-! ## P10+ effectiveAddr lemmas for CPI setup (PCs 85-95) -/

private theorem ea_bump_addr (b : Nat) :
    effectiveAddr b RM_FM_PDA_SEEDS_BUMP_ADDR_OFF = b - 632 := by
  unfold effectiveAddr RM_FM_PDA_SEEDS_BUMP_ADDR_OFF; omega

private theorem ea_bump_len (b : Nat) :
    effectiveAddr b RM_FM_PDA_SEEDS_BUMP_LEN_OFF = b - 624 := by
  unfold effectiveAddr RM_FM_PDA_SEEDS_BUMP_LEN_OFF; omega

private theorem ea_pubkey_chunk0 (b : Nat) :
    effectiveAddr b PUBKEY_CHUNK_0_OFF = b := by
  unfold effectiveAddr PUBKEY_CHUNK_0_OFF; omega

private theorem ea_pubkey_chunk1 (b : Nat) :
    effectiveAddr b PUBKEY_CHUNK_1_OFF = b + 8 := by
  unfold effectiveAddr PUBKEY_CHUNK_1_OFF; omega

private theorem ea_pubkey_chunk2 (b : Nat) :
    effectiveAddr b PUBKEY_CHUNK_2_OFF = b + 16 := by
  unfold effectiveAddr PUBKEY_CHUNK_2_OFF; omega

private theorem ea_pubkey_chunk3 (b : Nat) :
    effectiveAddr b PUBKEY_CHUNK_3_OFF = b + 24 := by
  unfold effectiveAddr PUBKEY_CHUNK_3_OFF; omega

private theorem ea_owner_chunk0 (b : Nat) :
    effectiveAddr b RM_FM_CREATE_ACCT_OWNER_CHUNK_0_UOFF = b - 532 := by
  unfold effectiveAddr RM_FM_CREATE_ACCT_OWNER_CHUNK_0_UOFF; omega

private theorem ea_owner_chunk1 (b : Nat) :
    effectiveAddr b RM_FM_CREATE_ACCT_OWNER_CHUNK_1_UOFF = b - 524 := by
  unfold effectiveAddr RM_FM_CREATE_ACCT_OWNER_CHUNK_1_UOFF; omega

private theorem ea_owner_chunk2 (b : Nat) :
    effectiveAddr b RM_FM_CREATE_ACCT_OWNER_CHUNK_2_UOFF = b - 516 := by
  unfold effectiveAddr RM_FM_CREATE_ACCT_OWNER_CHUNK_2_UOFF; omega

private theorem ea_owner_chunk3 (b : Nat) :
    effectiveAddr b RM_FM_CREATE_ACCT_OWNER_CHUNK_3_UOFF = b - 508 := by
  unfold effectiveAddr RM_FM_CREATE_ACCT_OWNER_CHUNK_3_UOFF; omega

/-! ## P10: system program is duplicate → error 10

   Prior checks P1-P9 pass (including all PDA chunks matching).
   Execution continues from PC 85 (CPI setup), then at PC 96 reads
   system program duplicate marker, branches on ≠ 255.

   Path: 24 → … → 84(fall through, chunks match) → 85-95 (CPI setup) →
         96 (ldx dup) → 97(jne→16) → 16 → 17

   New sub-lemmas:
   - p10_chunk_match: PCs 73-84, all chunks match, fall through
   - p10_cpi_setup: PCs 85-95, CPI data on stack
-/

-- Chunk match: 12 steps from PC 73 to PC 85 when all 4 PDA chunks match market pubkey.
-- PCs 73-84 only modify r7, r8 (scratch for ldx/jne), everything else preserved.
set_option maxHeartbeats 40000000 in
private theorem p10_chunk_match
    (inputAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 73)
    (h_r6   : s.regs.r6 = inputAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    -- All 4 chunks match (P9 passes)
    (h_eq0 : readU64 s.mem (inputAddr + 10352) = readU64 s.mem (STACK_START + 0x1000 - 616))
    (h_eq1 : readU64 s.mem (inputAddr + 10360) = readU64 s.mem (STACK_START + 0x1000 - 608))
    (h_eq2 : readU64 s.mem (inputAddr + 10368) = readU64 s.mem (STACK_START + 0x1000 - 600))
    (h_eq3 : readU64 s.mem (inputAddr + 10376) = readU64 s.mem (STACK_START + 0x1000 - 592)) :
    let s' := executeFn progAt s 12
    s'.exitCode = none ∧ s'.pc = 85 ∧
    s'.regs.r6 = inputAddr ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    s'.regs.r3 = s.regs.r3 ∧ s'.regs.r5 = s.regs.r5 ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.mem = s.mem := by
  -- 12 = 3 + 3 + 3 + 3, one chunk comparison per triple
  rw [show (12 : Nat) = 3 + (3 + (3 + 3)) from rfl]
  iterate 3 (rw [executeFn_compose])
  -- Chunk 0
  obtain ⟨he1, hp1, hm1, hr3_1, hr5_1, hr6_1, hr9_1, hr10_1⟩ :=
    p9_chunk_eq_state inputAddr s
      IB_MARKET_PUBKEY_CHUNK_0_OFF RM_FM_PDA_CHUNK_0_OFF
      h_exit h_r6 h_r10
      (by rw [h_pc]; native_decide) (by rw [h_pc]; native_decide) (by rw [h_pc]; native_decide)
      (by rw [ea_mkt_chunk0, ea_fm_pda_chunk0]; exact h_eq0)
  -- Chunk 1
  obtain ⟨he2, hp2, hm2, hr3_2, hr5_2, hr6_2, hr9_2, hr10_2⟩ :=
    p9_chunk_eq_state inputAddr _ IB_MARKET_PUBKEY_CHUNK_1_OFF RM_FM_PDA_CHUNK_1_OFF
      he1 hr6_1 hr10_1
      (by rw [hp1, h_pc]; native_decide) (by rw [hp1, h_pc]; native_decide)
      (by rw [hp1, h_pc]; native_decide)
      (by rw [ea_mkt_chunk1, ea_fm_pda_chunk1, hm1]; exact h_eq1)
  -- Chunk 2
  obtain ⟨he3, hp3, hm3, hr3_3, hr5_3, hr6_3, hr9_3, hr10_3⟩ :=
    p9_chunk_eq_state inputAddr _ IB_MARKET_PUBKEY_CHUNK_2_OFF RM_FM_PDA_CHUNK_2_OFF
      he2 hr6_2 hr10_2
      (by rw [hp2, hp1, h_pc]; native_decide) (by rw [hp2, hp1, h_pc]; native_decide)
      (by rw [hp2, hp1, h_pc]; native_decide)
      (by rw [ea_mkt_chunk2, ea_fm_pda_chunk2, hm2, hm1]; exact h_eq2)
  -- Chunk 3
  obtain ⟨he4, hp4, hm4, hr3_4, hr5_4, hr6_4, hr9_4, hr10_4⟩ :=
    p9_chunk_eq_state inputAddr _ IB_MARKET_PUBKEY_CHUNK_3_OFF RM_FM_PDA_CHUNK_3_OFF
      he3 hr6_3 hr10_3
      (by rw [hp3, hp2, hp1, h_pc]; native_decide) (by rw [hp3, hp2, hp1, h_pc]; native_decide)
      (by rw [hp3, hp2, hp1, h_pc]; native_decide)
      (by rw [ea_mkt_chunk3, ea_fm_pda_chunk3, hm3, hm2, hm1]; exact h_eq3)
  exact ⟨he4, by rw [hp4, hp3, hp2, hp1, h_pc], by rw [hr6_4],
    hr10_4, by rw [hr3_4, hr3_3, hr3_2, hr3_1],
    by rw [hr5_4, hr5_3, hr5_2, hr5_1],
    by rw [hr9_4, hr9_3, hr9_2, hr9_1],
    by rw [hm4, hm3, hm2, hm1]⟩

-- CPI setup part A: 3 steps PCs 85-87 (bump addr + mov + bump len → 2 writes)
set_option maxHeartbeats 40000000 in
private theorem p10_cpi_setup_a
    (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 85)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 3
    s'.exitCode = none ∧ s'.pc = 88 ∧
    s'.regs.r3 = s.regs.r3 ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    ∃ w1 w2, s'.mem = writeU64 (writeU64 s.mem
      (STACK_START + 0x1000 - 632) w1) (STACK_START + 0x1000 - 624) w2 := by
  have hf85 : progAt 85 = some (.stx .dword .r10 RM_FM_PDA_SEEDS_BUMP_ADDR_OFF .r5) := by native_decide
  have hf86 : progAt 86 = some (.mov64 .r7 (.imm SIZE_OF_U8)) := by native_decide
  have hf87 : progAt 87 = some (.stx .dword .r10 RM_FM_PDA_SEEDS_BUMP_LEN_OFF .r7) := by native_decide
  rw [show (3 : Nat) = 1+1+1 from rfl]
  iterate 2 (rw [executeFn_compose])
  simp only [executeFn, step, writeByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    ea_bump_addr, ea_bump_len,
    h_exit, h_pc, h_r10,
    hf85, hf86, hf87]
  exact ⟨trivial, trivial, trivial, trivial, trivial, _, _, rfl⟩

-- CPI setup part B: 4 steps PCs 88-91 (owner chunks 0-1 → 2 writes)
-- h_r3 needed so simp can resolve ldx reads without deep state traversal
set_option maxHeartbeats 40000000 in
private theorem p10_cpi_setup_b
    (ownerAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 88)
    (h_r3   : s.regs.r3 = ownerAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 4
    s'.exitCode = none ∧ s'.pc = 92 ∧
    s'.regs.r3 = ownerAddr ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    ∃ w3 w4, s'.mem = writeU64 (writeU64 s.mem
      (STACK_START + 0x1000 - 532) w3) (STACK_START + 0x1000 - 524) w4 := by
  have hf88 : progAt 88 = some (.ldx .dword .r7 .r3 PUBKEY_CHUNK_0_OFF) := by native_decide
  have hf89 : progAt 89 = some (.stx .dword .r10 RM_FM_CREATE_ACCT_OWNER_CHUNK_0_UOFF .r7) := by native_decide
  have hf90 : progAt 90 = some (.ldx .dword .r7 .r3 PUBKEY_CHUNK_1_OFF) := by native_decide
  have hf91 : progAt 91 = some (.stx .dword .r10 RM_FM_CREATE_ACCT_OWNER_CHUNK_1_UOFF .r7) := by native_decide
  rw [show (4 : Nat) = 1+1+1+1 from rfl]
  iterate 3 (rw [executeFn_compose])
  simp only [executeFn, step, readByWidth, writeByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    ea_pubkey_chunk0, ea_pubkey_chunk1,
    ea_owner_chunk0, ea_owner_chunk1,
    h_exit, h_pc, h_r3, h_r10,
    hf88, hf89, hf90, hf91]
  exact ⟨trivial, trivial, trivial, trivial, trivial, _, _, rfl⟩

-- CPI setup part C: 4 steps PCs 92-95 (owner chunks 2-3 → 2 writes)
set_option maxHeartbeats 40000000 in
private theorem p10_cpi_setup_c
    (ownerAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 92)
    (h_r3   : s.regs.r3 = ownerAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 4
    s'.exitCode = none ∧ s'.pc = 96 ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    ∃ w5 w6, s'.mem = writeU64 (writeU64 s.mem
      (STACK_START + 0x1000 - 516) w5) (STACK_START + 0x1000 - 508) w6 := by
  have hf92 : progAt 92 = some (.ldx .dword .r7 .r3 PUBKEY_CHUNK_2_OFF) := by native_decide
  have hf93 : progAt 93 = some (.stx .dword .r10 RM_FM_CREATE_ACCT_OWNER_CHUNK_2_UOFF .r7) := by native_decide
  have hf94 : progAt 94 = some (.ldx .dword .r7 .r3 PUBKEY_CHUNK_3_OFF) := by native_decide
  have hf95 : progAt 95 = some (.stx .dword .r10 RM_FM_CREATE_ACCT_OWNER_CHUNK_3_UOFF .r7) := by native_decide
  rw [show (4 : Nat) = 1+1+1+1 from rfl]
  iterate 3 (rw [executeFn_compose])
  simp only [executeFn, step, readByWidth, writeByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    ea_pubkey_chunk2, ea_pubkey_chunk3,
    ea_owner_chunk2, ea_owner_chunk3,
    h_exit, h_pc, h_r3, h_r10,
    hf92, hf93, hf94, hf95]
  exact ⟨trivial, trivial, trivial, trivial, _, _, rfl⟩

-- CPI setup composed: 11 steps PCs 85-95
private theorem p10_cpi_setup
    (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 85)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 11
    s'.exitCode = none ∧ s'.pc = 96 ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    ∃ w1 w2 w3 w4 w5 w6, s'.mem = writeU64 (writeU64 (writeU64 (writeU64 (writeU64 (writeU64 s.mem
      (STACK_START + 0x1000 - 632) w1) (STACK_START + 0x1000 - 624) w2)
      (STACK_START + 0x1000 - 532) w3) (STACK_START + 0x1000 - 524) w4)
      (STACK_START + 0x1000 - 516) w5) (STACK_START + 0x1000 - 508) w6 := by
  rw [show (11 : Nat) = 3 + (4 + 4) from rfl, executeFn_compose, executeFn_compose]
  obtain ⟨he1, hp1, hr3_1, hr9_1, hr10_1, w1, w2, hmem1⟩ := p10_cpi_setup_a s h_exit h_pc h_r10
  obtain ⟨he2, hp2, hr3_2, hr9_2, hr10_2, w3, w4, hmem2⟩ := p10_cpi_setup_b s.regs.r3 _ he1 hp1 hr3_1 hr10_1
  obtain ⟨he3, hp3, hr9_3, hr10_3, w5, w6, hmem3⟩ := p10_cpi_setup_c s.regs.r3 _ he2 hp2 hr3_2 hr10_2
  exact ⟨he3, hp3, by rw [hr9_3, hr9_2, hr9_1], hr10_3,
    w1, w2, w3, w4, w5, w6, by rw [hmem3, hmem2, hmem1]⟩

-- effectiveAddr for ACCT_DUPLICATE_OFF (= 0)
private theorem ea_acct_dup (b : Nat) : effectiveAddr b ACCT_DUPLICATE_OFF = b := by
  unfold effectiveAddr ACCT_DUPLICATE_OFF; omega

-- System program dup check: 4 steps from PC 96 → error exit.
-- ldx.byte from r9 (system program dup marker) + jne to error handler + mov32 + exit.
set_option maxHeartbeats 800000 in
private theorem p10_dup_exit
    (s : State)
    (sysDup : Nat)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 96)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_dup  : readU8 s.mem (s.regs.r9) = sysDup)
    (h_ne   : sysDup ≠ ACCT_NON_DUP_MARKER) :
    (executeFn progAt s 4).exitCode = some E_SYSTEM_PROGRAM_IS_DUPLICATE := by
  have h_ne' : ¬(readU8 s.mem (s.regs.r9) = (255 : Nat)) := by rw [h_dup]; exact h_ne
  have hf96 : progAt 96 = some (.ldx .byte .r7 .r9 ACCT_DUPLICATE_OFF) := by native_decide
  have hf97 : progAt 97 = some (.jne .r7 (.imm ACCT_NON_DUP_MARKER) 16) := by native_decide
  have hf16 : progAt 16 = some (.mov32 .r0 (.imm E_SYSTEM_PROGRAM_IS_DUPLICATE)) := by native_decide
  have hf17 : progAt 17 = some (.exit) := by native_decide
  rw [show (4 : Nat) = 1+1+1+1 from rfl]
  iterate 3 (rw [executeFn_compose])
  simp only [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_pc, h_r10, ea_acct_dup,
    hf96, hf97, hf16, hf17]
  simp [*, E_SYSTEM_PROGRAM_IS_DUPLICATE, U32_MODULUS]

-- Main P10 theorem: compose prefix + chunk match + CPI setup + dup check
set_option maxHeartbeats 4000000 in
theorem rejects_system_program_duplicate
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (c0 c1 c2 c3 : Nat)
    (sysDup : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    -- P9 pass: PDA chunks match market pubkey
    (h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = c0)
    (h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = c1)
    (h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = c2)
    (h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = c3)
    (h_mkt_c0 : readU64 mem (inputAddr + 10352) = c0)
    (h_mkt_c1 : readU64 mem (inputAddr + 10360) = c1)
    (h_mkt_c2 : readU64 mem (inputAddr + 10368) = c2)
    (h_mkt_c3 : readU64 mem (inputAddr + 10376) = c3)
    -- P10 fail: system program duplicate marker ≠ 255
    (h_sdup   : readU8 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9) = sysDup)
    (h_sdup_ne: sysDup ≠ ACCT_NON_DUP_MARKER)
    (h_r9_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 < STACK_START)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 74).exitCode
      = some E_SYSTEM_PROGRAM_IS_DUPLICATE := by
  -- Split 74 = 47 + 27
  rw [show (74 : Nat) = 47 + 27 from rfl, executeFn_compose]
  obtain ⟨he, hp, hr6, hr10, v1, v2, w1, w2, hmem⟩ := p9_prefix
    inputAddr insnAddr mem nAccounts baseDataLen
    h_disc h_num h_enough h_ilen h_udl h_mdup h_mdl h_bdup h_bdl h_qdup h_sep h_qaddr
  -- Split 27 = 12 + 15
  rw [show (27 : Nat) = 12 + 15 from rfl, executeFn_compose]
  unfold STACK_START at h_sep h_r9_sep h_pda_c0 h_pda_c1 h_pda_c2 h_pda_c3
  -- Chunk match: strip 4 prefix writes to provide chunk equalities
  obtain ⟨he2, hp2, hr6_2, hr10_2, hr3_2, hr5_2, hr9_2, hmem2⟩ :=
    p10_chunk_match inputAddr _ he hp hr6 hr10
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c0, h_pda_c0])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c1, h_pda_c1])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c2, h_pda_c2])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c3, h_pda_c3])
  -- Split 15 = 11 + 4
  rw [show (15 : Nat) = 11 + 4 from rfl, executeFn_compose]
  -- CPI setup: 6 more stack writes
  obtain ⟨he3, hp3, hr9_3, hr10_3, w3, w4, w5, w6, w7, w8, hmem3⟩ :=
    p10_cpi_setup _ he2 hp2 hr10_2
  -- Dup exit: strip 10 writes from s_96.mem to recover readU8 from original mem
  apply p10_dup_exit _ sysDup he3 hp3 hr10_3 _ h_sdup_ne
  rw [hr9_3, hr9_2, hmem3, hmem2, hmem]
  strip_writes_goal
  exact h_sdup

-- ============ P11: Invalid system program pubkey ============

-- effectiveAddr lemmas for account address chunks (r9-based reads)
private theorem ea_acct_addr_chunk0 (b : Nat) :
    effectiveAddr b ACCT_ADDRESS_CHUNK_0_OFF = b + 8 := by
  unfold effectiveAddr ACCT_ADDRESS_CHUNK_0_OFF; omega

private theorem ea_acct_addr_chunk1 (b : Nat) :
    effectiveAddr b ACCT_ADDRESS_CHUNK_1_OFF = b + 16 := by
  unfold effectiveAddr ACCT_ADDRESS_CHUNK_1_OFF; omega

private theorem ea_acct_addr_chunk2 (b : Nat) :
    effectiveAddr b ACCT_ADDRESS_CHUNK_2_OFF = b + 24 := by
  unfold effectiveAddr ACCT_ADDRESS_CHUNK_2_OFF; omega

private theorem ea_acct_addr_chunk3 (b : Nat) :
    effectiveAddr b ACCT_ADDRESS_CHUNK_3_OFF = b + 32 := by
  unfold effectiveAddr ACCT_ADDRESS_CHUNK_3_OFF; omega

-- effectiveAddr lemmas for system program pubkey on stack
private theorem ea_sys_prog_chunk0 (b : Nat) :
    effectiveAddr b RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF = b - 584 := by
  unfold effectiveAddr RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF; omega

private theorem ea_sys_prog_chunk1 (b : Nat) :
    effectiveAddr b RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF = b - 576 := by
  unfold effectiveAddr RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF; omega

private theorem ea_sys_prog_chunk2 (b : Nat) :
    effectiveAddr b RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF = b - 568 := by
  unfold effectiveAddr RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF; omega

private theorem ea_sys_prog_chunk3 (b : Nat) :
    effectiveAddr b RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF = b - 560 := by
  unfold effectiveAddr RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF; omega

-- Dup check passes: 2 steps PCs 96-97, dup marker = 255 → fall through.
private theorem p11_dup_pass
    (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 96)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_dup  : readU8 s.mem s.regs.r9 = ACCT_NON_DUP_MARKER) :
    let s' := executeFn progAt s 2
    s'.exitCode = none ∧ s'.pc = 98 ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    s'.mem = s.mem := by
  have hf96 : progAt 96 = some (.ldx .byte .r7 .r9 ACCT_DUPLICATE_OFF) := by native_decide
  have hf97 : progAt 97 = some (.jne .r7 (.imm ACCT_NON_DUP_MARKER) 16) := by native_decide
  rw [show (2 : Nat) = 1 + (1 + 0) from rfl, executeFn_compose]
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_pc, h_r10, ea_acct_dup,
    hf96, hf97, h_dup]

-- Chunk match (r9-based): ldx from r9 + ldx from r10 + jne(fallthrough) → 3 steps.
set_option maxHeartbeats 1600000 in
private theorem p11_chunk_eq_state (sysAddr : Nat) (s : State)
    (off1 off2 : Int)
    (h_exit : s.exitCode = none)
    (h_r9   : s.regs.r9 = sysAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r9 off1))
    (h_f2 : progAt (s.pc + 1) = some (.ldx .dword .r8 .r10 off2))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 18))
    (h_eq : readU64 s.mem (effectiveAddr sysAddr off1) =
            readU64 s.mem (effectiveAddr (STACK_START + 0x1000) off2)) :
    (executeFn progAt s 3).exitCode = none ∧
    (executeFn progAt s 3).pc = s.pc + 3 ∧
    (executeFn progAt s 3).mem = s.mem ∧
    (executeFn progAt s 3).regs.r9 = sysAddr ∧
    (executeFn progAt s 3).regs.r10 = STACK_START + 0x1000 := by
  rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
  iterate 2 (rw [executeFn_compose])
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_r9, h_r10, h_f1, h_f2, h_f3, h_eq]

-- Chunk mismatch (r9-based): branch to PC 18 → E_INVALID_SYSTEM_PROGRAM_PUBKEY.
private theorem p11_chunk_ne_error (sysAddr : Nat) (s : State) (n : Nat)
    (off1 off2 : Int)
    (h_exit : s.exitCode = none)
    (h_r9   : s.regs.r9 = sysAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r9 off1))
    (h_f2 : progAt (s.pc + 1) = some (.ldx .dword .r8 .r10 off2))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 18))
    (h_ne : readU64 s.mem (effectiveAddr sysAddr off1) ≠
            readU64 s.mem (effectiveAddr (STACK_START + 0x1000) off2))
    (h_fuel : n ≥ 5) :
    (executeFn progAt s n).exitCode = some E_INVALID_SYSTEM_PROGRAM_PUBKEY := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  suffices h5 : (executeFn progAt s 5).exitCode = some E_INVALID_SYSTEM_PROGRAM_PUBKEY by
    rw [executeFn_halted _ _ _ _ h5]; exact h5
  have hf18 : progAt 18 = some (.mov32 .r0 (.imm E_INVALID_SYSTEM_PROGRAM_PUBKEY)) := by native_decide
  have hf19 : progAt 19 = some (.exit) := by native_decide
  rw [show (5 : Nat) = 1 + (1 + (1 + (1 + (1 + 0)))) from rfl]
  iterate 4 (rw [executeFn_compose])
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc, toU64,
    h_exit, h_r9, h_r10, h_f1, h_f2, h_f3, h_ne,
    hf18, hf19, E_INVALID_SYSTEM_PROGRAM_PUBKEY, U32_MODULUS]

-- System program pubkey comparison: by_cases over 4 chunks.
set_option maxHeartbeats 4000000 in
private theorem p11_pubkey_compare
    (sysAddr : Nat) (s : State)
    (acct_c0 acct_c1 acct_c2 acct_c3 : Nat)
    (sys_c0 sys_c1 sys_c2 sys_c3 : Nat)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 98)
    (h_r9   : s.regs.r9 = sysAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_acct_c0 : readU64 s.mem (sysAddr + 8) = acct_c0)
    (h_acct_c1 : readU64 s.mem (sysAddr + 16) = acct_c1)
    (h_acct_c2 : readU64 s.mem (sysAddr + 24) = acct_c2)
    (h_acct_c3 : readU64 s.mem (sysAddr + 32) = acct_c3)
    (h_sys_c0 : readU64 s.mem (STACK_START + 0x1000 - 584) = sys_c0)
    (h_sys_c1 : readU64 s.mem (STACK_START + 0x1000 - 576) = sys_c1)
    (h_sys_c2 : readU64 s.mem (STACK_START + 0x1000 - 568) = sys_c2)
    (h_sys_c3 : readU64 s.mem (STACK_START + 0x1000 - 560) = sys_c3)
    (h_ne : acct_c0 ≠ sys_c0 ∨ acct_c1 ≠ sys_c1 ∨ acct_c2 ≠ sys_c2 ∨ acct_c3 ≠ sys_c3) :
    (executeFn progAt s 14).exitCode = some E_INVALID_SYSTEM_PROGRAM_PUBKEY := by
  -- Instruction fetches for PCs 98-109
  have hf98  : progAt 98  = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_0_OFF) := by native_decide
  have hf99  : progAt 99  = some (.ldx .dword .r8 .r10 RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF) := by native_decide
  have hf100 : progAt 100 = some (.jne .r7 (.reg .r8) 18) := by native_decide
  have hf101 : progAt 101 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_1_OFF) := by native_decide
  have hf102 : progAt 102 = some (.ldx .dword .r8 .r10 RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF) := by native_decide
  have hf103 : progAt 103 = some (.jne .r7 (.reg .r8) 18) := by native_decide
  have hf104 : progAt 104 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_2_OFF) := by native_decide
  have hf105 : progAt 105 = some (.ldx .dword .r8 .r10 RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF) := by native_decide
  have hf106 : progAt 106 = some (.jne .r7 (.reg .r8) 18) := by native_decide
  have hf107 : progAt 107 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_3_OFF) := by native_decide
  have hf108 : progAt 108 = some (.ldx .dword .r8 .r10 RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF) := by native_decide
  have hf109 : progAt 109 = some (.jne .r7 (.reg .r8) 18) := by native_decide
  by_cases h_eq0 : acct_c0 = sys_c0
  · simp [h_eq0] at h_ne
    rw [show (14 : Nat) = 3 + 11 from rfl, executeFn_compose]
    have hst := p11_chunk_eq_state sysAddr s
      ACCT_ADDRESS_CHUNK_0_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF
      h_exit h_r9 h_r10
      (by rw [h_pc]; exact hf98) (by rw [h_pc]; exact hf99) (by rw [h_pc]; exact hf100)
      (by rw [ea_acct_addr_chunk0, ea_sys_prog_chunk0, h_acct_c0, h_sys_c0]; exact h_eq0)
    obtain ⟨h_exit₁, h_pc₁, h_mem₁, h_r9₁, h_r10₁⟩ := hst
    by_cases h_eq1 : acct_c1 = sys_c1
    · simp [h_eq1] at h_ne
      rw [show (11 : Nat) = 3 + 8 from rfl, executeFn_compose]
      have hst := p11_chunk_eq_state sysAddr (executeFn progAt s 3)
        ACCT_ADDRESS_CHUNK_1_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF
        h_exit₁ h_r9₁ h_r10₁
        (by rw [h_pc₁, h_pc]; exact hf101) (by rw [h_pc₁, h_pc]; exact hf102)
        (by rw [h_pc₁, h_pc]; exact hf103)
        (by rw [ea_acct_addr_chunk1, ea_sys_prog_chunk1, h_mem₁, h_acct_c1, h_sys_c1]; exact h_eq1)
      obtain ⟨h_exit₂, h_pc₂, h_mem₂, h_r9₂, h_r10₂⟩ := hst
      by_cases h_eq2 : acct_c2 = sys_c2
      · simp [h_eq2] at h_ne
        rw [show (8 : Nat) = 3 + 5 from rfl, executeFn_compose]
        have hst := p11_chunk_eq_state sysAddr
          (executeFn progAt (executeFn progAt s 3) 3)
          ACCT_ADDRESS_CHUNK_2_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF
          h_exit₂ h_r9₂ h_r10₂
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf104) (by rw [h_pc₂, h_pc₁, h_pc]; exact hf105)
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf106)
          (by rw [ea_acct_addr_chunk2, ea_sys_prog_chunk2, h_mem₂, h_mem₁, h_acct_c2, h_sys_c2]; exact h_eq2)
        obtain ⟨h_exit₃, h_pc₃, h_mem₃, h_r9₃, h_r10₃⟩ := hst
        -- Chunk 3 must mismatch
        exact p11_chunk_ne_error sysAddr
          (executeFn progAt (executeFn progAt (executeFn progAt s 3) 3) 3)
          5 ACCT_ADDRESS_CHUNK_3_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF
          h_exit₃ h_r9₃ h_r10₃
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf107)
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf108)
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf109)
          (by rw [ea_acct_addr_chunk3, ea_sys_prog_chunk3, h_mem₃, h_mem₂, h_mem₁,
                  h_acct_c3, h_sys_c3]; exact h_ne)
          (by omega)
      · -- Chunk 2 mismatches
        exact p11_chunk_ne_error sysAddr
          (executeFn progAt (executeFn progAt s 3) 3)
          8 ACCT_ADDRESS_CHUNK_2_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF
          h_exit₂ h_r9₂ h_r10₂
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf104) (by rw [h_pc₂, h_pc₁, h_pc]; exact hf105)
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf106)
          (by rw [ea_acct_addr_chunk2, ea_sys_prog_chunk2, h_mem₂, h_mem₁,
                  h_acct_c2, h_sys_c2]; exact h_eq2)
          (by omega)
    · -- Chunk 1 mismatches
      exact p11_chunk_ne_error sysAddr (executeFn progAt s 3)
        11 ACCT_ADDRESS_CHUNK_1_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF
        h_exit₁ h_r9₁ h_r10₁
        (by rw [h_pc₁, h_pc]; exact hf101) (by rw [h_pc₁, h_pc]; exact hf102)
        (by rw [h_pc₁, h_pc]; exact hf103)
        (by rw [ea_acct_addr_chunk1, ea_sys_prog_chunk1, h_mem₁,
                h_acct_c1, h_sys_c1]; exact h_eq1)
        (by omega)
  · -- Chunk 0 mismatches
    exact p11_chunk_ne_error sysAddr s 14
      ACCT_ADDRESS_CHUNK_0_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF
      h_exit h_r9 h_r10
      (by rw [h_pc]; exact hf98) (by rw [h_pc]; exact hf99) (by rw [h_pc]; exact hf100)
      (by rw [ea_acct_addr_chunk0, ea_sys_prog_chunk0, h_acct_c0, h_sys_c0]; exact h_eq0)
      (by omega)

-- Main P11 theorem: compose prefix + chunk match + CPI setup + dup pass + pubkey compare
set_option maxHeartbeats 4000000 in
theorem rejects_invalid_system_program_pubkey
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (c0 c1 c2 c3 : Nat)
    (acct_c0 acct_c1 acct_c2 acct_c3 : Nat)
    (sys_c0 sys_c1 sys_c2 sys_c3 : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    -- P9 pass: PDA chunks match market pubkey
    (h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = c0)
    (h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = c1)
    (h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = c2)
    (h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = c3)
    (h_mkt_c0 : readU64 mem (inputAddr + 10352) = c0)
    (h_mkt_c1 : readU64 mem (inputAddr + 10360) = c1)
    (h_mkt_c2 : readU64 mem (inputAddr + 10368) = c2)
    (h_mkt_c3 : readU64 mem (inputAddr + 10376) = c3)
    -- P10 pass: system program dup = 255 (not duplicate)
    (h_sdup   : readU8 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9) = ACCT_NON_DUP_MARKER)
    (h_r9_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 40 < STACK_START)
    -- System program pubkey on stack (expected)
    (h_sys_c0 : readU64 mem (STACK_START + 0x1000 - 584) = sys_c0)
    (h_sys_c1 : readU64 mem (STACK_START + 0x1000 - 576) = sys_c1)
    (h_sys_c2 : readU64 mem (STACK_START + 0x1000 - 568) = sys_c2)
    (h_sys_c3 : readU64 mem (STACK_START + 0x1000 - 560) = sys_c3)
    -- Account address at r9 (actual)
    (h_acct_c0 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 8) = acct_c0)
    (h_acct_c1 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 16) = acct_c1)
    (h_acct_c2 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 24) = acct_c2)
    (h_acct_c3 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 32) = acct_c3)
    -- Pubkey mismatch
    (h_ne : acct_c0 ≠ sys_c0 ∨ acct_c1 ≠ sys_c1 ∨ acct_c2 ≠ sys_c2 ∨ acct_c3 ≠ sys_c3)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 86).exitCode
      = some E_INVALID_SYSTEM_PROGRAM_PUBKEY := by
  -- Split 86 = 47 + 39
  rw [show (86 : Nat) = 47 + 39 from rfl, executeFn_compose]
  obtain ⟨he, hp, hr6, hr10, v1, v2, w1, w2, hmem⟩ := p9_prefix
    inputAddr insnAddr mem nAccounts baseDataLen
    h_disc h_num h_enough h_ilen h_udl h_mdup h_mdl h_bdup h_bdl h_qdup h_sep h_qaddr
  -- Split 39 = 12 + 27
  rw [show (39 : Nat) = 12 + 27 from rfl, executeFn_compose]
  unfold STACK_START at h_sep h_r9_sep h_pda_c0 h_pda_c1 h_pda_c2 h_pda_c3 h_sys_c0 h_sys_c1 h_sys_c2 h_sys_c3
  obtain ⟨he2, hp2, hr6_2, hr10_2, hr3_2, hr5_2, hr9_2, hmem2⟩ :=
    p10_chunk_match inputAddr _ he hp hr6 hr10
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c0, h_pda_c0])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c1, h_pda_c1])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c2, h_pda_c2])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c3, h_pda_c3])
  -- Split 27 = 11 + 16
  rw [show (27 : Nat) = 11 + 16 from rfl, executeFn_compose]
  obtain ⟨he3, hp3, hr9_3, hr10_3, w3, w4, w5, w6, w7, w8, hmem3⟩ :=
    p10_cpi_setup _ he2 hp2 hr10_2
  -- Split 16 = 2 + 14
  rw [show (16 : Nat) = 2 + 14 from rfl, executeFn_compose]
  -- Dup pass: strip 10 writes to recover readU8 from original mem
  have h_dup_pass := p11_dup_pass _ he3 hp3 hr10_3
    (by rw [hr9_3, hr9_2, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sdup)
  obtain ⟨he4, hp4, hr9_4, hr10_4, hmem4⟩ := h_dup_pass
  -- Pubkey compare: strip 10 writes for each chunk read
  apply p11_pubkey_compare _ _ acct_c0 acct_c1 acct_c2 acct_c3 sys_c0 sys_c1 sys_c2 sys_c3
    he4 hp4 (by rw [hr9_4, hr9_3, hr9_2]) hr10_4
    -- account address chunks (r9 below stack, all writes disjoint)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_acct_c0)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_acct_c1)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_acct_c2)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_acct_c3)
    -- system program pubkey chunks on stack (disjoint from all writes)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sys_c0)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sys_c1)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sys_c2)
    (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sys_c3)
    h_ne

-- ============ P12: Rent sysvar is duplicate ============

-- System program pubkey match: 12 steps PCs 98-109, all chunks match → fall through.
-- Uses p11_chunk_eq_state (r9-based comparison with jne target 18).
set_option maxHeartbeats 4000000 in
private theorem p12_sys_pubkey_match
    (sysAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 98)
    (h_r9   : s.regs.r9 = sysAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_eq0 : readU64 s.mem (sysAddr + 8) = readU64 s.mem (STACK_START + 0x1000 - 584))
    (h_eq1 : readU64 s.mem (sysAddr + 16) = readU64 s.mem (STACK_START + 0x1000 - 576))
    (h_eq2 : readU64 s.mem (sysAddr + 24) = readU64 s.mem (STACK_START + 0x1000 - 568))
    (h_eq3 : readU64 s.mem (sysAddr + 32) = readU64 s.mem (STACK_START + 0x1000 - 560)) :
    let s' := executeFn progAt s 12
    s'.exitCode = none ∧ s'.pc = 110 ∧
    s'.regs.r9 = sysAddr ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    s'.mem = s.mem := by
  rw [show (12 : Nat) = 3 + (3 + (3 + 3)) from rfl]
  iterate 3 (rw [executeFn_compose])
  obtain ⟨he1, hp1, hm1, hr9_1, hr10_1⟩ :=
    p11_chunk_eq_state sysAddr s ACCT_ADDRESS_CHUNK_0_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_0_OFF
      h_exit h_r9 h_r10
      (by rw [h_pc]; native_decide) (by rw [h_pc]; native_decide) (by rw [h_pc]; native_decide)
      (by rw [ea_acct_addr_chunk0, ea_sys_prog_chunk0]; exact h_eq0)
  obtain ⟨he2, hp2, hm2, hr9_2, hr10_2⟩ :=
    p11_chunk_eq_state sysAddr _ ACCT_ADDRESS_CHUNK_1_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_1_OFF
      he1 hr9_1 hr10_1
      (by rw [hp1, h_pc]; native_decide) (by rw [hp1, h_pc]; native_decide)
      (by rw [hp1, h_pc]; native_decide)
      (by rw [ea_acct_addr_chunk1, ea_sys_prog_chunk1, hm1]; exact h_eq1)
  obtain ⟨he3, hp3, hm3, hr9_3, hr10_3⟩ :=
    p11_chunk_eq_state sysAddr _ ACCT_ADDRESS_CHUNK_2_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_2_OFF
      he2 hr9_2 hr10_2
      (by rw [hp2, hp1, h_pc]; native_decide) (by rw [hp2, hp1, h_pc]; native_decide)
      (by rw [hp2, hp1, h_pc]; native_decide)
      (by rw [ea_acct_addr_chunk2, ea_sys_prog_chunk2, hm2, hm1]; exact h_eq2)
  obtain ⟨he4, hp4, hm4, hr9_4, hr10_4⟩ :=
    p11_chunk_eq_state sysAddr _ ACCT_ADDRESS_CHUNK_3_OFF RM_FM_SYSTEM_PROGRAM_PUBKEY_CHUNK_3_OFF
      he3 hr9_3 hr10_3
      (by rw [hp3, hp2, hp1, h_pc]; native_decide) (by rw [hp3, hp2, hp1, h_pc]; native_decide)
      (by rw [hp3, hp2, hp1, h_pc]; native_decide)
      (by rw [ea_acct_addr_chunk3, ea_sys_prog_chunk3, hm3, hm2, hm1]; exact h_eq3)
  exact ⟨he4, by rw [hp4, hp3, hp2, hp1, h_pc], by rw [hr9_4], hr10_4,
    by rw [hm4, hm3, hm2, hm1]⟩

-- effectiveAddr lemmas for r9 advance
private theorem ea_sol_insn_prog_id (b : Nat) :
    effectiveAddr b RM_FM_SOL_INSN_PROGRAM_ID_UOFF = b - 48 := by
  unfold effectiveAddr RM_FM_SOL_INSN_PROGRAM_ID_UOFF; omega

private theorem ea_acct_data_len (b : Nat) :
    effectiveAddr b ACCT_DATA_LEN_OFF = b + 80 := by
  unfold effectiveAddr ACCT_DATA_LEN_OFF; omega

-- r9 advance: 8 steps PCs 110-117, writes program ID ptr, advances r9 to next account.
-- Does NOT track r9 — the new r9 value is left implicit in the state.
set_option maxHeartbeats 40000000 in
private theorem p12_r9_advance
    (sysAddr : Nat) (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 110)
    (h_r9   : s.regs.r9 = sysAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000) :
    let s' := executeFn progAt s 8
    s'.exitCode = none ∧ s'.pc = 118 ∧
    s'.regs.r10 = STACK_START + 0x1000 ∧
    ∃ w, s'.mem = writeU64 s.mem (STACK_START + 0x1000 - 48) w := by
  have hf110 : progAt 110 = some (.mov64 .r7 (.reg .r9)) := by native_decide
  have hf111 : progAt 111 = some (.add64 .r7 (.imm ACCT_ADDRESS_OFF)) := by native_decide
  have hf112 : progAt 112 = some (.stx .dword .r10 RM_FM_SOL_INSN_PROGRAM_ID_UOFF .r7) := by native_decide
  have hf113 : progAt 113 = some (.ldx .dword .r7 .r9 ACCT_DATA_LEN_OFF) := by native_decide
  have hf114 : progAt 114 = some (.add64 .r7 (.imm DATA_LEN_MAX_PAD)) := by native_decide
  have hf115 : progAt 115 = some (.and64 .r7 (.imm DATA_LEN_AND_MASK)) := by native_decide
  have hf116 : progAt 116 = some (.add64 .r9 (.reg .r7)) := by native_decide
  have hf117 : progAt 117 = some (.add64 .r9 (.imm SIZE_OF_EMPTY_ACCOUNT)) := by native_decide
  rw [show (8 : Nat) = 1+1+1+1+1+1+1+1 from rfl]
  iterate 7 (rw [executeFn_compose])
  simp only [executeFn, step, readByWidth, writeByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    ea_sol_insn_prog_id, ea_acct_data_len,
    h_exit, h_pc, h_r9, h_r10,
    hf110, hf111, hf112, hf113, hf114, hf115, hf116, hf117]
  exact ⟨trivial, trivial, trivial, _, rfl⟩

-- Rent sysvar dup check: 4 steps PCs 118-121.
-- ldx.byte from r9 (rent dup marker) + jne to error handler at PC 20 + mov32 + exit.
set_option maxHeartbeats 800000 in
private theorem p12_dup_exit
    (s : State) (rentDup : Nat)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 118)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_dup  : readU8 s.mem s.regs.r9 = rentDup)
    (h_ne   : rentDup ≠ ACCT_NON_DUP_MARKER) :
    (executeFn progAt s 4).exitCode = some E_RENT_SYSVAR_IS_DUPLICATE := by
  have h_ne' : ¬(readU8 s.mem s.regs.r9 = (255 : Nat)) := by rw [h_dup]; exact h_ne
  have hf118 : progAt 118 = some (.ldx .byte .r7 .r9 ACCT_DUPLICATE_OFF) := by native_decide
  have hf119 : progAt 119 = some (.jne .r7 (.imm ACCT_NON_DUP_MARKER) 20) := by native_decide
  have hf20 : progAt 20 = some (.mov32 .r0 (.imm E_RENT_SYSVAR_IS_DUPLICATE)) := by native_decide
  have hf21 : progAt 21 = some (.exit) := by native_decide
  rw [show (4 : Nat) = 1+1+1+1 from rfl]
  iterate 3 (rw [executeFn_compose])
  simp only [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_pc, h_r10, ea_acct_dup,
    hf118, hf119, hf20, hf21]
  simp [*, E_RENT_SYSVAR_IS_DUPLICATE, U32_MODULUS]

-- Main P12 theorem
set_option maxHeartbeats 4000000 in
theorem rejects_rent_sysvar_duplicate
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (c0 c1 c2 c3 : Nat)
    (sys_c0 sys_c1 sys_c2 sys_c3 : Nat)
    (rentDup : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    -- P9 pass: PDA chunks match market pubkey
    (h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = c0)
    (h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = c1)
    (h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = c2)
    (h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = c3)
    (h_mkt_c0 : readU64 mem (inputAddr + 10352) = c0)
    (h_mkt_c1 : readU64 mem (inputAddr + 10360) = c1)
    (h_mkt_c2 : readU64 mem (inputAddr + 10368) = c2)
    (h_mkt_c3 : readU64 mem (inputAddr + 10376) = c3)
    -- P10 pass: system program dup = 255
    (h_sdup   : readU8 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9) = ACCT_NON_DUP_MARKER)
    (h_r9_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 40 < STACK_START)
    -- P11 pass: system program pubkey matches expected
    (h_sys_c0 : readU64 mem (STACK_START + 0x1000 - 584) = sys_c0)
    (h_sys_c1 : readU64 mem (STACK_START + 0x1000 - 576) = sys_c1)
    (h_sys_c2 : readU64 mem (STACK_START + 0x1000 - 568) = sys_c2)
    (h_sys_c3 : readU64 mem (STACK_START + 0x1000 - 560) = sys_c3)
    (h_acct_c0 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 8) = sys_c0)
    (h_acct_c1 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 16) = sys_c1)
    (h_acct_c2 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 24) = sys_c2)
    (h_acct_c3 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 32) = sys_c3)
    -- P12 fail: rent sysvar dup ≠ 255
    (h_rdup   : readU8 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9) = rentDup)
    (h_rdup_ne : rentDup ≠ ACCT_NON_DUP_MARKER)
    (h_r9_rent_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 < STACK_START)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 96).exitCode
      = some E_RENT_SYSVAR_IS_DUPLICATE := by
  -- Decompose execution: 96 = 47 + 12 + 11 + 2 + 12 + 8 + 4
  rw [show (96 : Nat) = 47 + (12 + (11 + (2 + (12 + (8 + 4))))) from rfl]
  iterate 6 (rw [executeFn_compose])
  -- Decompose hypotheses referencing step 92 to match nested form
  rw [show (92 : Nat) = 47 + (12 + (11 + (2 + (12 + 8)))) from rfl] at h_rdup h_r9_rent_sep
  iterate 5 (rw [executeFn_compose] at h_rdup h_r9_rent_sep)
  -- Prefix
  obtain ⟨he, hp, hr6, hr10, v1, v2, w1, w2, hmem⟩ := p9_prefix
    inputAddr insnAddr mem nAccounts baseDataLen
    h_disc h_num h_enough h_ilen h_udl h_mdup h_mdl h_bdup h_bdl h_qdup h_sep h_qaddr
  -- Market pubkey match (12 steps)
  unfold STACK_START at h_sep h_r9_sep h_r9_rent_sep h_pda_c0 h_pda_c1 h_pda_c2 h_pda_c3 h_sys_c0 h_sys_c1 h_sys_c2 h_sys_c3
  obtain ⟨he2, hp2, _, hr10_2, _, _, hr9_2, hmem2⟩ :=
    p10_chunk_match inputAddr _ he hp hr6 hr10
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c0, h_pda_c0])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c1, h_pda_c1])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c2, h_pda_c2])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c3, h_pda_c3])
  -- CPI setup (11 steps)
  obtain ⟨he3, hp3, hr9_3, hr10_3, w3, w4, w5, w6, w7, w8, hmem3⟩ :=
    p10_cpi_setup _ he2 hp2 hr10_2
  -- System program dup pass (2 steps)
  have h_dup_pass := p11_dup_pass _ he3 hp3 hr10_3
    (by rw [hr9_3, hr9_2, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sdup)
  obtain ⟨he4, hp4, hr9_4, hr10_4, hmem4⟩ := h_dup_pass
  -- System program pubkey match (12 steps)
  obtain ⟨he5, hp5, hr9_5, hr10_5, hmem5⟩ :=
    p12_sys_pubkey_match _ _ he4 hp4 (by rw [hr9_4, hr9_3, hr9_2]) hr10_4
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c0, h_sys_c0])
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c1, h_sys_c1])
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c2, h_sys_c2])
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c3, h_sys_c3])
  -- r9 advance (8 steps)
  obtain ⟨he6, hp6, hr10_6, w_prog, hmem6⟩ :=
    p12_r9_advance _ _ he5 hp5 hr9_5 hr10_5
  -- Dup exit (4 steps)
  apply p12_dup_exit _ rentDup he6 hp6 hr10_6 _ h_rdup_ne
  rw [hmem6, hmem5, hmem4, hmem3, hmem2, hmem]
  strip_writes_goal
  exact h_rdup

-- ============ P13: Invalid rent sysvar pubkey ============

-- Bridge lemmas: toU64 of rent pubkey constants = Nat values
private theorem rent_bridge_0 : toU64 (↑PUBKEY_RENT_CHUNK_0 : Int) = PUBKEY_RENT_CHUNK_0 := by native_decide
private theorem rent_bridge_1 : toU64 (↑PUBKEY_RENT_CHUNK_1 : Int) = PUBKEY_RENT_CHUNK_1 := by native_decide
private theorem rent_bridge_2 : toU64 (↑PUBKEY_RENT_CHUNK_2 : Int) = PUBKEY_RENT_CHUNK_2 := by native_decide
private theorem rent_bridge_3 : toU64 PUBKEY_RENT_CHUNK_3_LO % U32_MODULUS = PUBKEY_RENT_CHUNK_3 := by native_decide

-- Rent sysvar dup pass: 2 steps PCs 118-119, dup marker = 255 → fall through to PC 120.
private theorem p13_rent_dup_pass
    (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 118)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_dup  : readU8 s.mem s.regs.r9 = ACCT_NON_DUP_MARKER) :
    let s' := executeFn progAt s 2
    s'.exitCode = none ∧ s'.pc = 120 ∧
    s'.regs.r9 = s.regs.r9 ∧ s'.regs.r10 = STACK_START + 0x1000 ∧
    s'.mem = s.mem := by
  have hf118 : progAt 118 = some (.ldx .byte .r7 .r9 ACCT_DUPLICATE_OFF) := by native_decide
  have hf119 : progAt 119 = some (.jne .r7 (.imm ACCT_NON_DUP_MARKER) 20) := by native_decide
  rw [show (2 : Nat) = 1 + (1 + 0) from rfl, executeFn_compose]
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_pc, h_r10, ea_acct_dup,
    hf118, hf119, h_dup]

-- Chunk match via lddw: 3 steps (ldx + lddw + jne fallthrough).
set_option maxHeartbeats 1600000 in
private theorem p13_chunk_eq_lddw (rentAddr : Nat) (s : State)
    (off : Int) (val : Int)
    (h_exit : s.exitCode = none)
    (h_r9   : s.regs.r9 = rentAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r9 off))
    (h_f2 : progAt (s.pc + 1) = some (.lddw .r8 val))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 22))
    (h_eq : readU64 s.mem (effectiveAddr rentAddr off) = toU64 val) :
    (executeFn progAt s 3).exitCode = none ∧
    (executeFn progAt s 3).pc = s.pc + 3 ∧
    (executeFn progAt s 3).mem = s.mem ∧
    (executeFn progAt s 3).regs.r9 = rentAddr ∧
    (executeFn progAt s 3).regs.r10 = STACK_START + 0x1000 := by
  rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
  iterate 2 (rw [executeFn_compose])
  simp [executeFn, step, readByWidth,
    RegFile.get, RegFile.set, resolveSrc,
    h_exit, h_r9, h_r10, h_f1, h_f2, h_f3, h_eq]

-- Shared error handler: PC = 22 → mov32 r0 E_INVALID_RENT_SYSVAR_PUBKEY → exit.
private theorem p13_error_at_22 (s : State)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 22) :
    (executeFn progAt s 2).exitCode = some E_INVALID_RENT_SYSVAR_PUBKEY := by
  have hf22 : progAt 22 = some (.mov32 .r0 (.imm E_INVALID_RENT_SYSVAR_PUBKEY)) := by native_decide
  have hf23 : progAt 23 = some (.exit) := by native_decide
  rw [show (2 : Nat) = 1 + (1 + 0) from rfl, executeFn_compose]
  simp [executeFn, step, RegFile.get, RegFile.set, resolveSrc, toU64,
    h_exit, h_pc, hf22, hf23, E_INVALID_RENT_SYSVAR_PUBKEY, U32_MODULUS]

-- Chunk mismatch via lddw: branch to PC 22 → E_INVALID_RENT_SYSVAR_PUBKEY.
set_option maxHeartbeats 4000000 in
private theorem p13_chunk_ne_lddw (rentAddr : Nat) (s : State) (n : Nat)
    (off : Int) (val : Int)
    (h_exit : s.exitCode = none)
    (h_r9   : s.regs.r9 = rentAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r9 off))
    (h_f2 : progAt (s.pc + 1) = some (.lddw .r8 val))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 22))
    (h_ne : readU64 s.mem (effectiveAddr rentAddr off) ≠ toU64 val)
    (h_fuel : n ≥ 5) :
    (executeFn progAt s n).exitCode = some E_INVALID_RENT_SYSVAR_PUBKEY := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  suffices h5 : (executeFn progAt s 5).exitCode = some E_INVALID_RENT_SYSVAR_PUBKEY by
    rw [executeFn_halted _ _ _ _ h5]; exact h5
  -- Split: 5 = 3 + 2 (branch + error handler)
  rw [show (5 : Nat) = 3 + 2 from rfl, executeFn_compose]
  -- Prove branch reaches PC 22
  have h3_exit : (executeFn progAt s 3).exitCode = none := by
    rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
    iterate 2 (rw [executeFn_compose])
    simp only [executeFn, step, readByWidth,
      RegFile.get, RegFile.set, resolveSrc,
      h_exit, h_r9, h_r10, h_f1, h_f2, h_f3]
  have h3_pc : (executeFn progAt s 3).pc = 22 := by
    rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
    iterate 2 (rw [executeFn_compose])
    simp only [executeFn, step, readByWidth,
      RegFile.get, RegFile.set, resolveSrc,
      h_exit, h_r9, h_r10, h_f1, h_f2, h_f3]
    exact if_pos h_ne
  exact p13_error_at_22 _ h3_exit h3_pc

-- Chunk mismatch via mov32 (chunk 3): branch to PC 22 → E_INVALID_RENT_SYSVAR_PUBKEY.
set_option maxHeartbeats 4000000 in
private theorem p13_chunk_ne_mov32 (rentAddr : Nat) (s : State) (n : Nat)
    (off : Int) (val : Int)
    (h_exit : s.exitCode = none)
    (h_r9   : s.regs.r9 = rentAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_f1 : progAt s.pc = some (.ldx .dword .r7 .r9 off))
    (h_f2 : progAt (s.pc + 1) = some (.mov32 .r8 (.imm val)))
    (h_f3 : progAt (s.pc + 2) = some (.jne .r7 (.reg .r8) 22))
    (h_ne : readU64 s.mem (effectiveAddr rentAddr off) ≠ toU64 val % U32_MODULUS)
    (h_fuel : n ≥ 5) :
    (executeFn progAt s n).exitCode = some E_INVALID_RENT_SYSVAR_PUBKEY := by
  rw [show n = 5 + (n - 5) from by omega, executeFn_compose]
  suffices h5 : (executeFn progAt s 5).exitCode = some E_INVALID_RENT_SYSVAR_PUBKEY by
    rw [executeFn_halted _ _ _ _ h5]; exact h5
  -- Split: 5 = 3 + 2 (branch + error handler)
  rw [show (5 : Nat) = 3 + 2 from rfl, executeFn_compose]
  -- Prove branch reaches PC 22
  have h3_exit : (executeFn progAt s 3).exitCode = none := by
    rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
    iterate 2 (rw [executeFn_compose])
    simp only [executeFn, step, readByWidth,
      RegFile.get, RegFile.set, resolveSrc,
      h_exit, h_r9, h_r10, h_f1, h_f2, h_f3]
  have h3_pc : (executeFn progAt s 3).pc = 22 := by
    rw [show (3 : Nat) = 1 + (1 + (1 + 0)) from rfl]
    iterate 2 (rw [executeFn_compose])
    simp only [executeFn, step, readByWidth,
      RegFile.get, RegFile.set, resolveSrc,
      h_exit, h_r9, h_r10, h_f1, h_f2, h_f3]
    exact if_pos h_ne
  exact p13_error_at_22 _ h3_exit h3_pc

-- Rent sysvar pubkey comparison: by_cases over 4 chunks, max 14 steps.
-- Chunks 0-2 use lddw for expected value; chunk 3 uses mov32.
set_option maxHeartbeats 4000000 in
private theorem p13_pubkey_compare
    (rentAddr : Nat) (s : State)
    (rent_c0 rent_c1 rent_c2 rent_c3 : Nat)
    (h_exit : s.exitCode = none)
    (h_pc   : s.pc = 120)
    (h_r9   : s.regs.r9 = rentAddr)
    (h_r10  : s.regs.r10 = STACK_START + 0x1000)
    (h_rent_c0 : readU64 s.mem (rentAddr + 8) = rent_c0)
    (h_rent_c1 : readU64 s.mem (rentAddr + 16) = rent_c1)
    (h_rent_c2 : readU64 s.mem (rentAddr + 24) = rent_c2)
    (h_rent_c3 : readU64 s.mem (rentAddr + 32) = rent_c3)
    (h_ne : rent_c0 ≠ PUBKEY_RENT_CHUNK_0 ∨ rent_c1 ≠ PUBKEY_RENT_CHUNK_1 ∨
            rent_c2 ≠ PUBKEY_RENT_CHUNK_2 ∨ rent_c3 ≠ PUBKEY_RENT_CHUNK_3) :
    (executeFn progAt s 14).exitCode = some E_INVALID_RENT_SYSVAR_PUBKEY := by
  -- Instruction fetches for PCs 120-131
  have hf120 : progAt 120 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_0_OFF) := by native_decide
  have hf121 : progAt 121 = some (.lddw .r8 PUBKEY_RENT_CHUNK_0) := by native_decide
  have hf122 : progAt 122 = some (.jne .r7 (.reg .r8) 22) := by native_decide
  have hf123 : progAt 123 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_1_OFF) := by native_decide
  have hf124 : progAt 124 = some (.lddw .r8 PUBKEY_RENT_CHUNK_1) := by native_decide
  have hf125 : progAt 125 = some (.jne .r7 (.reg .r8) 22) := by native_decide
  have hf126 : progAt 126 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_2_OFF) := by native_decide
  have hf127 : progAt 127 = some (.lddw .r8 PUBKEY_RENT_CHUNK_2) := by native_decide
  have hf128 : progAt 128 = some (.jne .r7 (.reg .r8) 22) := by native_decide
  have hf129 : progAt 129 = some (.ldx .dword .r7 .r9 ACCT_ADDRESS_CHUNK_3_OFF) := by native_decide
  have hf130 : progAt 130 = some (.mov32 .r8 (.imm PUBKEY_RENT_CHUNK_3_LO)) := by native_decide
  have hf131 : progAt 131 = some (.jne .r7 (.reg .r8) 22) := by native_decide
  by_cases h_eq0 : rent_c0 = PUBKEY_RENT_CHUNK_0
  · simp [h_eq0] at h_ne
    rw [show (14 : Nat) = 3 + 11 from rfl, executeFn_compose]
    have hst := p13_chunk_eq_lddw rentAddr s
      ACCT_ADDRESS_CHUNK_0_OFF PUBKEY_RENT_CHUNK_0
      h_exit h_r9 h_r10
      (by rw [h_pc]; exact hf120) (by rw [h_pc]; exact hf121) (by rw [h_pc]; exact hf122)
      (by rw [ea_acct_addr_chunk0, h_rent_c0, h_eq0]; exact rent_bridge_0.symm)
    obtain ⟨h_exit₁, h_pc₁, h_mem₁, h_r9₁, h_r10₁⟩ := hst
    by_cases h_eq1 : rent_c1 = PUBKEY_RENT_CHUNK_1
    · simp [h_eq1] at h_ne
      rw [show (11 : Nat) = 3 + 8 from rfl, executeFn_compose]
      have hst := p13_chunk_eq_lddw rentAddr _
        ACCT_ADDRESS_CHUNK_1_OFF PUBKEY_RENT_CHUNK_1
        h_exit₁ h_r9₁ h_r10₁
        (by rw [h_pc₁, h_pc]; exact hf123) (by rw [h_pc₁, h_pc]; exact hf124)
        (by rw [h_pc₁, h_pc]; exact hf125)
        (by rw [ea_acct_addr_chunk1, h_mem₁, h_rent_c1, h_eq1]; exact rent_bridge_1.symm)
      obtain ⟨h_exit₂, h_pc₂, h_mem₂, h_r9₂, h_r10₂⟩ := hst
      by_cases h_eq2 : rent_c2 = PUBKEY_RENT_CHUNK_2
      · simp [h_eq2] at h_ne
        rw [show (8 : Nat) = 3 + 5 from rfl, executeFn_compose]
        have hst := p13_chunk_eq_lddw rentAddr _
          ACCT_ADDRESS_CHUNK_2_OFF PUBKEY_RENT_CHUNK_2
          h_exit₂ h_r9₂ h_r10₂
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf126) (by rw [h_pc₂, h_pc₁, h_pc]; exact hf127)
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf128)
          (by rw [ea_acct_addr_chunk2, h_mem₂, h_mem₁, h_rent_c2, h_eq2]; exact rent_bridge_2.symm)
        obtain ⟨h_exit₃, h_pc₃, h_mem₃, h_r9₃, h_r10₃⟩ := hst
        -- Chunk 3 must mismatch (mov32 pattern)
        exact p13_chunk_ne_mov32 rentAddr
          (executeFn progAt (executeFn progAt (executeFn progAt s 3) 3) 3)
          5 ACCT_ADDRESS_CHUNK_3_OFF PUBKEY_RENT_CHUNK_3_LO
          h_exit₃ h_r9₃ h_r10₃
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf129)
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf130)
          (by rw [h_pc₃, h_pc₂, h_pc₁, h_pc]; exact hf131)
          (by rw [ea_acct_addr_chunk3, h_mem₃, h_mem₂, h_mem₁, h_rent_c3, rent_bridge_3]; exact h_ne)
          (by omega)
      · -- Chunk 2 mismatches
        exact p13_chunk_ne_lddw rentAddr
          (executeFn progAt (executeFn progAt s 3) 3)
          8 ACCT_ADDRESS_CHUNK_2_OFF PUBKEY_RENT_CHUNK_2
          h_exit₂ h_r9₂ h_r10₂
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf126) (by rw [h_pc₂, h_pc₁, h_pc]; exact hf127)
          (by rw [h_pc₂, h_pc₁, h_pc]; exact hf128)
          (by rw [ea_acct_addr_chunk2, h_mem₂, h_mem₁, h_rent_c2, rent_bridge_2]; exact h_eq2)
          (by omega)
    · -- Chunk 1 mismatches
      exact p13_chunk_ne_lddw rentAddr (executeFn progAt s 3)
        11 ACCT_ADDRESS_CHUNK_1_OFF PUBKEY_RENT_CHUNK_1
        h_exit₁ h_r9₁ h_r10₁
        (by rw [h_pc₁, h_pc]; exact hf123) (by rw [h_pc₁, h_pc]; exact hf124)
        (by rw [h_pc₁, h_pc]; exact hf125)
        (by rw [ea_acct_addr_chunk1, h_mem₁, h_rent_c1, rent_bridge_1]; exact h_eq1)
        (by omega)
  · -- Chunk 0 mismatches
    exact p13_chunk_ne_lddw rentAddr s 14
      ACCT_ADDRESS_CHUNK_0_OFF PUBKEY_RENT_CHUNK_0
      h_exit h_r9 h_r10
      (by rw [h_pc]; exact hf120) (by rw [h_pc]; exact hf121) (by rw [h_pc]; exact hf122)
      (by rw [ea_acct_addr_chunk0, h_rent_c0, rent_bridge_0]; exact h_eq0)
      (by omega)

-- Main P13 theorem
set_option maxHeartbeats 4000000 in
theorem rejects_invalid_rent_sysvar_pubkey
    (inputAddr insnAddr : Nat) (mem : Mem)
    (nAccounts baseDataLen : Nat)
    (c0 c1 c2 c3 : Nat)
    (sys_c0 sys_c1 sys_c2 sys_c3 : Nat)
    (rent_c0 rent_c1 rent_c2 rent_c3 : Nat)
    (h_disc   : readU8 mem insnAddr = DISC_REGISTER_MARKET)
    (h_num    : readU64 mem inputAddr = nAccounts)
    (h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))
    (h_ilen   : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)
    (h_udl    : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)
    (h_mdup   : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)
    (h_mdl    : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)
    (h_bdup   : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)
    (h_bdl    : readU64 mem (inputAddr + 20760) = baseDataLen)
    (h_qdup   : readU8 mem
        (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
          inputAddr + 31016) = ACCT_NON_DUP_MARKER)
    -- P9 pass: PDA chunks match market pubkey
    (h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = c0)
    (h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = c1)
    (h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = c2)
    (h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = c3)
    (h_mkt_c0 : readU64 mem (inputAddr + 10352) = c0)
    (h_mkt_c1 : readU64 mem (inputAddr + 10360) = c1)
    (h_mkt_c2 : readU64 mem (inputAddr + 10368) = c2)
    (h_mkt_c3 : readU64 mem (inputAddr + 10376) = c3)
    -- P10 pass: system program dup = 255
    (h_sdup   : readU8 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9) = ACCT_NON_DUP_MARKER)
    (h_r9_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 40 < STACK_START)
    -- P11 pass: system program pubkey matches expected
    (h_sys_c0 : readU64 mem (STACK_START + 0x1000 - 584) = sys_c0)
    (h_sys_c1 : readU64 mem (STACK_START + 0x1000 - 576) = sys_c1)
    (h_sys_c2 : readU64 mem (STACK_START + 0x1000 - 568) = sys_c2)
    (h_sys_c3 : readU64 mem (STACK_START + 0x1000 - 560) = sys_c3)
    (h_acct_c0 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 8) = sys_c0)
    (h_acct_c1 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 16) = sys_c1)
    (h_acct_c2 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 24) = sys_c2)
    (h_acct_c3 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 32) = sys_c3)
    -- P12 pass: rent sysvar dup = 255
    (h_rdup   : readU8 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9) = ACCT_NON_DUP_MARKER)
    (h_r9_rent_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 40 < STACK_START)
    -- P13 fail: rent pubkey mismatch
    (h_rent_c0 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 8) = rent_c0)
    (h_rent_c1 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 16) = rent_c1)
    (h_rent_c2 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 24) = rent_c2)
    (h_rent_c3 : readU64 mem
        ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 32) = rent_c3)
    (h_ne : rent_c0 ≠ PUBKEY_RENT_CHUNK_0 ∨ rent_c1 ≠ PUBKEY_RENT_CHUNK_1 ∨
            rent_c2 ≠ PUBKEY_RENT_CHUNK_2 ∨ rent_c3 ≠ PUBKEY_RENT_CHUNK_3)
    (h_sep    : STACK_START + 0x1000 > inputAddr + 100000)
    (h_qaddr  : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS)
                  inputAddr + 31016 < STACK_START) :
    (executeFn progAt (initState2 inputAddr insnAddr mem 24) 108).exitCode
      = some E_INVALID_RENT_SYSVAR_PUBKEY := by
  -- Decompose execution: 108 = 47 + (12 + (11 + (2 + (12 + (8 + (2 + 14))))))
  rw [show (108 : Nat) = 47 + (12 + (11 + (2 + (12 + (8 + (2 + 14)))))) from rfl]
  iterate 7 (rw [executeFn_compose])
  -- Decompose hypotheses referencing step 92 to match nested form
  rw [show (92 : Nat) = 47 + (12 + (11 + (2 + (12 + 8)))) from rfl] at h_rdup h_r9_rent_sep h_rent_c0 h_rent_c1 h_rent_c2 h_rent_c3
  iterate 5 (rw [executeFn_compose] at h_rdup h_r9_rent_sep h_rent_c0 h_rent_c1 h_rent_c2 h_rent_c3)
  -- Prefix (47 steps)
  obtain ⟨he, hp, hr6, hr10, v1, v2, w1, w2, hmem⟩ := p9_prefix
    inputAddr insnAddr mem nAccounts baseDataLen
    h_disc h_num h_enough h_ilen h_udl h_mdup h_mdl h_bdup h_bdl h_qdup h_sep h_qaddr
  -- Market pubkey match (12 steps)
  unfold STACK_START at h_sep h_r9_sep h_r9_rent_sep h_pda_c0 h_pda_c1 h_pda_c2 h_pda_c3 h_sys_c0 h_sys_c1 h_sys_c2 h_sys_c3
  obtain ⟨he2, hp2, _, hr10_2, _, _, hr9_2, hmem2⟩ :=
    p10_chunk_match inputAddr _ he hp hr6 hr10
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c0, h_pda_c0])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c1, h_pda_c1])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c2, h_pda_c2])
      (by rw [hmem]; strip_writes_goal; rw [h_mkt_c3, h_pda_c3])
  -- CPI setup (11 steps)
  obtain ⟨he3, hp3, hr9_3, hr10_3, w3, w4, w5, w6, w7, w8, hmem3⟩ :=
    p10_cpi_setup _ he2 hp2 hr10_2
  -- System program dup pass (2 steps)
  have h_dup_pass := p11_dup_pass _ he3 hp3 hr10_3
    (by rw [hr9_3, hr9_2, hmem3, hmem2, hmem]; strip_writes_goal; exact h_sdup)
  obtain ⟨he4, hp4, hr9_4, hr10_4, hmem4⟩ := h_dup_pass
  -- System program pubkey match (12 steps)
  obtain ⟨he5, hp5, hr9_5, hr10_5, hmem5⟩ :=
    p12_sys_pubkey_match _ _ he4 hp4 (by rw [hr9_4, hr9_3, hr9_2]) hr10_4
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c0, h_sys_c0])
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c1, h_sys_c1])
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c2, h_sys_c2])
      (by rw [hmem4, hmem3, hmem2, hmem]; strip_writes_goal; rw [h_acct_c3, h_sys_c3])
  -- r9 advance (8 steps)
  obtain ⟨he6, hp6, hr10_6, w_prog, hmem6⟩ :=
    p12_r9_advance _ _ he5 hp5 hr9_5 hr10_5
  -- Rent sysvar dup pass (2 steps)
  have h_rent_dup := p13_rent_dup_pass _ he6 hp6 hr10_6
    (by rw [hmem6, hmem5, hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_rdup)
  obtain ⟨he7, hp7, hr9_7, hr10_7, hmem7⟩ := h_rent_dup
  -- Rent pubkey compare (14 steps)
  apply p13_pubkey_compare _ _ rent_c0 rent_c1 rent_c2 rent_c3
    he7 hp7 hr9_7 hr10_7
    (by rw [hmem7, hmem6, hmem5, hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_rent_c0)
    (by rw [hmem7, hmem6, hmem5, hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_rent_c1)
    (by rw [hmem7, hmem6, hmem5, hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_rent_c2)
    (by rw [hmem7, hmem6, hmem5, hmem4, hmem3, hmem2, hmem]; strip_writes_goal; exact h_rent_c3)
    h_ne

end DropsetProofs
