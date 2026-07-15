import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

namespace FeeSplit

open QEDGen.Solana

abbrev BPS_DENOM : Nat := 10000

structure State where
  pool : Nat
  fees : Nat
  fee_bps : Nat
  lifetime_collected : Nat
  deriving Repr, DecidableEq, BEq, Inhabited

def collectTransition (s : State) (signer : Pubkey) (total : Nat) : Option State :=
  let fee := (((total) * (s.fee_bps)) / (10000))
  let net := total - fee
  if total > 0 ∧ net > 0 ∧ s.pool + net ≤ 18446744073709551615 ∧ s.fees + fee ≤ 18446744073709551615 then
    some { s with pool := s.pool + net, fees := s.fees + fee, lifetime_collected := s.lifetime_collected + total }
  else none

inductive Operation where
  | collect (total : Nat)
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .collect total => collectTransition s signer total

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem collect_aborts_if_DustAmount_0 (s : State) (signer : Pubkey) (total : Nat)
    (h : ¬(total > 0)) : collectTransition s signer total = none := by
  unfold collectTransition
  dsimp only
  rw [if_neg (fun hg => h hg.1)]

theorem collect_aborts_if_DustAmount_1 (s : State) (signer : Pubkey) (total : Nat)
    (h : ¬(total - (((total) * (s.fee_bps)) / (10000)) > 0)) : collectTransition s signer total = none := by
  unfold collectTransition
  dsimp only
  rw [if_neg (fun hg => h hg.2.1)]

-- ============================================================================
-- Post-conditions (ensures)
-- ============================================================================

theorem collect_ensures_0 (s s' : State) (signer : Pubkey) (total : Nat)
    (h : collectTransition s signer total = some s') :
    s'.fees ≥ s.fees := sorry

-- ============================================================================
-- Overflow safety obligations (auto-generated for operations with add effects)
-- ============================================================================

theorem collect_overflow_safe (s s' : State) (signer : Pubkey) (total : Nat)
    (h_valid : valid_u64 s.pool ∧ valid_u64 s.fees ∧ valid_u64 s.fee_bps)
    (h : collectTransition s signer total = some s') :
    valid_u64 s'.pool ∧ valid_u64 s'.fees ∧ valid_u64 s'.fee_bps := by
  unfold collectTransition at h; dsimp only at h; split at h
  · next hg =>
    cases h
    refine ⟨?_, ?_, h_valid.2.2⟩
    simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX]; omega
    simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX]; omega
  · contradiction

end FeeSplit
