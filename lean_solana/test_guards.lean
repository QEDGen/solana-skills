-- Unit tests for the qedguards DSL
-- Tests: single-register, two-register, error constants, hypothesis accumulation

import QEDGen

open QEDGen.Solana
open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

-- ============================================================================
-- Test 1: Single-register, 2-guard chain, numeric error codes
-- ============================================================================

abbrev DISC_XFER : Nat := 1

qedguards SimpleCheck where
  prog: progAt
  r1: inputAddr

  guard rejects_bad_disc fuel 5 error 1
    hyps
      "(disc : Nat)"
      "(h_disc : readU8 mem inputAddr = disc)"
      "(h_ne : disc ≠ DISC_XFER)"
    after
      "(h_disc : readU8 mem inputAddr = DISC_XFER)"

  guard rejects_low_balance fuel 8 error 2
    hyps
      "(bal : Nat)"
      "(h_bal : readU64 mem (inputAddr + 8) = bal)"
      "(h_low : bal < 100)"

-- Verify types
#check @SimpleCheck.rejects_bad_disc
#check @SimpleCheck.rejects_low_balance

-- ============================================================================
-- Test 2: Two-register program with entry PC and error constants
-- ============================================================================

qedguards TwoReg where
  prog: progAt
  entry: 24
  r1: inputAddr
  r2: insnAddr

  errors
    E_BAD_DISC 1
    E_BAD_COUNT 3

  guard rejects_bad_disc fuel 8 error E_BAD_DISC
    hyps
      "(disc : Nat)"
      "(h_disc : readU8 mem insnAddr = disc)"
      "(h_ne : disc ≠ 0)"
    after
      "(h_disc : readU8 mem insnAddr = 0)"

  guard rejects_bad_count fuel 10 error E_BAD_COUNT
    hyps
      "(n : Nat)"
      "(h_n : readU64 mem inputAddr = n)"
      "(h_few : n < 10)"

-- Verify error constants
#check @TwoReg.E_BAD_DISC   -- Nat
#check @TwoReg.E_BAD_COUNT  -- Nat

-- Verify theorems exist and accumulate correctly
#check @TwoReg.rejects_bad_disc
#check @TwoReg.rejects_bad_count

-- ============================================================================
-- Test 3: 3-guard chain — verify full accumulation
-- ============================================================================

qedguards ThreeGuard where
  prog: progAt
  entry: 10
  r1: inputAddr
  r2: insnAddr

  guard step1 fuel 5 error 1
    hyps
      "(x : Nat)"
      "(h_x : readU64 mem inputAddr = x)"
      "(h_fail : x < 10)"
    after
      "(x : Nat)"
      "(h_x : readU64 mem inputAddr = x)"
      "(h_pass1 : ¬(x < 10))"

  guard step2 fuel 8 error 2
    hyps
      "(y : Nat)"
      "(h_y : readU64 mem (inputAddr + 8) = y)"
      "(h_fail : y ≠ 42)"
    after
      "(h_y : readU64 mem (inputAddr + 8) = 42)"

  guard step3 fuel 12 error 3
    hyps
      "(z : Nat)"
      "(h_z : readU8 mem (insnAddr + 1) = z)"
      "(h_fail : z = 0)"

-- step3 should have: params + step1.after + step2.after + step3.hyps
#check @ThreeGuard.step3
