-- Full integration test: qedguards for Dropset REGISTER_MARKET (P1-P13)
-- Verifies generated theorem signatures match hand-written DropsetProofs.lean

import QEDGen

open QEDGen.Solana
open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

-- ============================================================================
-- Constants (matching DropsetProg.lean)
-- ============================================================================

abbrev DISC_REGISTER_MARKET : Nat := 0
abbrev REGISTER_MARKET_ACCOUNTS_LEN : Nat := 10
abbrev REGISTER_MARKET_DATA_LEN : Nat := 1
abbrev DATA_LEN_ZERO : Nat := 0
abbrev DATA_LEN_MAX_PAD : Nat := 7
abbrev DATA_LEN_AND_MASK : Int := -8
abbrev ACCT_NON_DUP_MARKER : Nat := 255
abbrev PUBKEY_RENT_CHUNK_0 : Nat := 5862609301215225606
abbrev PUBKEY_RENT_CHUNK_1 : Nat := 9219231539345853473
abbrev PUBKEY_RENT_CHUNK_2 : Nat := 4971307250928769624
abbrev PUBKEY_RENT_CHUNK_3 : Nat := 2329533411

-- ============================================================================
-- Full qedguards spec: Dropset REGISTER_MARKET (P1-P13)
-- ============================================================================

qedguards RegisterMarket where
  prog: progAt
  entry: 24
  r1: inputAddr
  r2: insnAddr

  errors
    E_INVALID_DISCRIMINANT 1
    E_INVALID_INSTRUCTION_LENGTH 2
    E_INVALID_NUMBER_OF_ACCOUNTS 3
    E_USER_HAS_DATA 4
    E_MARKET_ACCOUNT_IS_DUPLICATE 5
    E_MARKET_HAS_DATA 6
    E_BASE_MINT_IS_DUPLICATE 7
    E_QUOTE_MINT_IS_DUPLICATE 8
    E_INVALID_MARKET_PUBKEY 9
    E_SYSTEM_PROGRAM_IS_DUPLICATE 10
    E_INVALID_SYSTEM_PROGRAM_PUBKEY 11
    E_RENT_SYSVAR_IS_DUPLICATE 12
    E_INVALID_RENT_SYSVAR_PUBKEY 13

  -- ── P1: Invalid discriminant ──────────────────────────────────────────
  guard rejects_invalid_discriminant fuel 8 error E_INVALID_DISCRIMINANT
    hyps
      "(disc : Nat)"
      "(h_disc_val : readU8 mem insnAddr = disc)"
      "(h_disc_ne : disc ≠ DISC_REGISTER_MARKET)"
    after
      "(h_disc : readU8 mem insnAddr = DISC_REGISTER_MARKET)"

  -- ── P2: Invalid account count ─────────────────────────────────────────
  guard rejects_invalid_account_count fuel 10 error E_INVALID_NUMBER_OF_ACCOUNTS
    hyps
      "(nAccounts : Nat)"
      "(h_num : readU64 mem inputAddr = nAccounts)"
      "(h_few : nAccounts < REGISTER_MARKET_ACCOUNTS_LEN)"
    after
      "(nAccounts : Nat)"
      "(h_num : readU64 mem inputAddr = nAccounts)"
      "(h_enough : ¬(nAccounts < REGISTER_MARKET_ACCOUNTS_LEN))"

  -- ── P3: Invalid instruction length ────────────────────────────────────
  guard rejects_invalid_instruction_length fuel 12 error E_INVALID_INSTRUCTION_LENGTH
    hyps
      "(insnLen : Nat)"
      "(h_ilen : readU64 mem (insnAddr - 8) = insnLen)"
      "(h_ne_len : insnLen ≠ REGISTER_MARKET_DATA_LEN)"
    after
      "(h_ilen : readU64 mem (insnAddr - 8) = REGISTER_MARKET_DATA_LEN)"

  -- ── P4: User has data ─────────────────────────────────────────────────
  guard rejects_user_has_data fuel 14 error E_USER_HAS_DATA
    hyps
      "(userDataLen : Nat)"
      "(h_udl : readU64 mem (inputAddr + 88) = userDataLen)"
      "(h_udl_ne : userDataLen ≠ DATA_LEN_ZERO)"
    after
      "(h_udl : readU64 mem (inputAddr + 88) = DATA_LEN_ZERO)"

  -- ── P5: Market account is duplicate ───────────────────────────────────
  guard rejects_market_duplicate fuel 16 error E_MARKET_ACCOUNT_IS_DUPLICATE
    hyps
      "(mktDup : Nat)"
      "(h_mdup : readU8 mem (inputAddr + 10344) = mktDup)"
      "(h_mdup_ne : mktDup ≠ ACCT_NON_DUP_MARKER)"
    after
      "(h_mdup : readU8 mem (inputAddr + 10344) = ACCT_NON_DUP_MARKER)"

  -- ── P6: Market has data ───────────────────────────────────────────────
  guard rejects_market_has_data fuel 18 error E_MARKET_HAS_DATA
    hyps
      "(mktDataLen : Nat)"
      "(h_mdl : readU64 mem (inputAddr + 10424) = mktDataLen)"
      "(h_mdl_ne : mktDataLen ≠ DATA_LEN_ZERO)"
    after
      "(h_mdl : readU64 mem (inputAddr + 10424) = DATA_LEN_ZERO)"

  -- ── P7: Base mint is duplicate ────────────────────────────────────────
  guard rejects_base_mint_duplicate fuel 20 error E_BASE_MINT_IS_DUPLICATE
    hyps
      "(baseDup : Nat)"
      "(h_bdup : readU8 mem (inputAddr + 20680) = baseDup)"
      "(h_bdup_ne : baseDup ≠ ACCT_NON_DUP_MARKER)"
    after
      "(h_bdup : readU8 mem (inputAddr + 20680) = ACCT_NON_DUP_MARKER)"

  -- ── P8: Quote mint is duplicate (dynamic offset) ─────────────────────
  guard rejects_quote_mint_duplicate fuel 30 error E_QUOTE_MINT_IS_DUPLICATE
    hyps
      "(baseDataLen : Nat)"
      "(quoteDup : Nat)"
      "(h_bdl : readU64 mem (inputAddr + 20760) = baseDataLen)"
      "(h_qdup : readU8 mem (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS) inputAddr + 31016) = quoteDup)"
      "(h_qdup_ne : quoteDup ≠ ACCT_NON_DUP_MARKER)"
      "(h_sep : STACK_START + 0x1000 > inputAddr + 100000)"
      "(h_qaddr : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS) inputAddr + 31016 < STACK_START)"
    after
      "(baseDataLen : Nat)"
      "(h_bdl : readU64 mem (inputAddr + 20760) = baseDataLen)"
      "(h_qdup : readU8 mem (wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS) inputAddr + 31016) = ACCT_NON_DUP_MARKER)"
      "(h_sep : STACK_START + 0x1000 > inputAddr + 100000)"
      "(h_qaddr : wrapAdd (((wrapAdd baseDataLen (toU64 (↑DATA_LEN_MAX_PAD : Int))) &&& toU64 DATA_LEN_AND_MASK) % U64_MODULUS) inputAddr + 31016 < STACK_START)"

  -- ── P9: Invalid market pubkey (4-chunk PDA comparison) ────────────────
  guard rejects_invalid_market_pubkey fuel 61 error E_INVALID_MARKET_PUBKEY
    hyps
      "(pda_c0 pda_c1 pda_c2 pda_c3 : Nat)"
      "(mkt_c0 mkt_c1 mkt_c2 mkt_c3 : Nat)"
      "(h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = pda_c0)"
      "(h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = pda_c1)"
      "(h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = pda_c2)"
      "(h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = pda_c3)"
      "(h_mkt_c0 : readU64 mem (inputAddr + 10352) = mkt_c0)"
      "(h_mkt_c1 : readU64 mem (inputAddr + 10360) = mkt_c1)"
      "(h_mkt_c2 : readU64 mem (inputAddr + 10368) = mkt_c2)"
      "(h_mkt_c3 : readU64 mem (inputAddr + 10376) = mkt_c3)"
      "(h_ne : mkt_c0 ≠ pda_c0 ∨ mkt_c1 ≠ pda_c1 ∨ mkt_c2 ≠ pda_c2 ∨ mkt_c3 ≠ pda_c3)"
    after
      -- Pass: use shared vars (c0..c3) so PDA and market chunks are equal
      "(c0 c1 c2 c3 : Nat)"
      "(h_pda_c0 : readU64 mem (STACK_START + 0x1000 - 616) = c0)"
      "(h_pda_c1 : readU64 mem (STACK_START + 0x1000 - 608) = c1)"
      "(h_pda_c2 : readU64 mem (STACK_START + 0x1000 - 600) = c2)"
      "(h_pda_c3 : readU64 mem (STACK_START + 0x1000 - 592) = c3)"
      "(h_mkt_c0 : readU64 mem (inputAddr + 10352) = c0)"
      "(h_mkt_c1 : readU64 mem (inputAddr + 10360) = c1)"
      "(h_mkt_c2 : readU64 mem (inputAddr + 10368) = c2)"
      "(h_mkt_c3 : readU64 mem (inputAddr + 10376) = c3)"

  -- ── P10: System program is duplicate (register-computed address) ──────
  guard rejects_system_program_duplicate fuel 74 error E_SYSTEM_PROGRAM_IS_DUPLICATE
    hyps
      "(sysDup : Nat)"
      "(h_sdup : readU8 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9) = sysDup)"
      "(h_sdup_ne : sysDup ≠ ACCT_NON_DUP_MARKER)"
      "(h_r9_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 < STACK_START)"
    after
      "(h_sdup : readU8 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9) = ACCT_NON_DUP_MARKER)"
      "(h_r9_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 40 < STACK_START)"

  -- ── P11: Invalid system program pubkey (stack vs r9 chunks) ───────────
  guard rejects_invalid_system_program_pubkey fuel 86 error E_INVALID_SYSTEM_PROGRAM_PUBKEY
    hyps
      "(acct_c0 acct_c1 acct_c2 acct_c3 : Nat)"
      "(sys_c0 sys_c1 sys_c2 sys_c3 : Nat)"
      "(h_sys_c0 : readU64 mem (STACK_START + 0x1000 - 584) = sys_c0)"
      "(h_sys_c1 : readU64 mem (STACK_START + 0x1000 - 576) = sys_c1)"
      "(h_sys_c2 : readU64 mem (STACK_START + 0x1000 - 568) = sys_c2)"
      "(h_sys_c3 : readU64 mem (STACK_START + 0x1000 - 560) = sys_c3)"
      "(h_acct_c0 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 8) = acct_c0)"
      "(h_acct_c1 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 16) = acct_c1)"
      "(h_acct_c2 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 24) = acct_c2)"
      "(h_acct_c3 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 32) = acct_c3)"
      "(h_ne : acct_c0 ≠ sys_c0 ∨ acct_c1 ≠ sys_c1 ∨ acct_c2 ≠ sys_c2 ∨ acct_c3 ≠ sys_c3)"
    after
      -- Pass: shared vars (sys_c0..c3) bind both stack and account reads
      "(sys_c0 sys_c1 sys_c2 sys_c3 : Nat)"
      "(h_sys_c0 : readU64 mem (STACK_START + 0x1000 - 584) = sys_c0)"
      "(h_sys_c1 : readU64 mem (STACK_START + 0x1000 - 576) = sys_c1)"
      "(h_sys_c2 : readU64 mem (STACK_START + 0x1000 - 568) = sys_c2)"
      "(h_sys_c3 : readU64 mem (STACK_START + 0x1000 - 560) = sys_c3)"
      "(h_acct_c0 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 8) = sys_c0)"
      "(h_acct_c1 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 16) = sys_c1)"
      "(h_acct_c2 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 24) = sys_c2)"
      "(h_acct_c3 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 47).regs.r9 + 32) = sys_c3)"

  -- ── P12: Rent sysvar is duplicate (r9 at step 92) ────────────────────
  guard rejects_rent_sysvar_duplicate fuel 96 error E_RENT_SYSVAR_IS_DUPLICATE
    hyps
      "(rentDup : Nat)"
      "(h_rdup : readU8 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9) = rentDup)"
      "(h_rdup_ne : rentDup ≠ ACCT_NON_DUP_MARKER)"
      "(h_r9_rent_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 < STACK_START)"
    after
      "(h_rdup : readU8 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9) = ACCT_NON_DUP_MARKER)"
      "(h_r9_rent_sep : (executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 40 < STACK_START)"

  -- ── P13: Invalid rent sysvar pubkey (r9 chunks vs known pubkey) ───────
  guard rejects_invalid_rent_sysvar_pubkey fuel 108 error E_INVALID_RENT_SYSVAR_PUBKEY
    hyps
      "(rent_c0 rent_c1 rent_c2 rent_c3 : Nat)"
      "(h_rent_c0 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 8) = rent_c0)"
      "(h_rent_c1 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 16) = rent_c1)"
      "(h_rent_c2 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 24) = rent_c2)"
      "(h_rent_c3 : readU64 mem ((executeFn progAt (initState2 inputAddr insnAddr mem 24) 92).regs.r9 + 32) = rent_c3)"
      "(h_ne : rent_c0 ≠ PUBKEY_RENT_CHUNK_0 ∨ rent_c1 ≠ PUBKEY_RENT_CHUNK_1 ∨ rent_c2 ≠ PUBKEY_RENT_CHUNK_2 ∨ rent_c3 ≠ PUBKEY_RENT_CHUNK_3)"

-- ============================================================================
-- Verify all 13 generated theorem signatures
-- ============================================================================

-- Error constants
#check @RegisterMarket.E_INVALID_DISCRIMINANT        -- Nat = 1
#check @RegisterMarket.E_INVALID_RENT_SYSVAR_PUBKEY  -- Nat = 13

-- P1: simple — disc + fail
#check @RegisterMarket.rejects_invalid_discriminant

-- P5: accumulates P1-P4 after-blocks
#check @RegisterMarket.rejects_market_duplicate

-- P7: accumulates P1-P6 — last "simple" guard
#check @RegisterMarket.rejects_base_mint_duplicate

-- P8: dynamic offset (wrapAdd expression)
#check @RegisterMarket.rejects_quote_mint_duplicate

-- P9: 4-chunk PDA comparison with stack reads
#check @RegisterMarket.rejects_invalid_market_pubkey

-- P10: register-computed address (r9 at step 47)
#check @RegisterMarket.rejects_system_program_duplicate

-- P11: stack pubkey vs r9 account pubkey
#check @RegisterMarket.rejects_invalid_system_program_pubkey

-- P12: r9 at step 92 (different intermediate state)
#check @RegisterMarket.rejects_rent_sysvar_duplicate

-- P13: full accumulation — all 12 prior guards' after-blocks
#check @RegisterMarket.rejects_invalid_rent_sysvar_pubkey
