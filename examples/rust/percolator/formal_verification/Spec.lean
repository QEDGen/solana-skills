import QEDGen.Solana

open QEDGen.Solana

/-!
# Percolator Risk Engine Verification Spec

A perpetual DEX risk engine managing protected principal, junior profit claims,
and lazy A/K side indices.

This spec does NOT use the `qedspec` DSL because the percolator is a pure
computation engine without the Anchor patterns that the DSL targets:
  - No explicit signers (access control is in the wrapper layer)
  - No CPI calls (token transfers are in the wrapper layer)
  - No lifecycle states (the engine is always active)

Instead, the core properties are conservation invariants over arithmetic
state transitions. These are declared as theorem stubs below, matching
the hand-written proofs in PercolatorProofs.lean.
-/

namespace Percolator

-- ============================================================================
-- State
-- ============================================================================

structure EngineState where
  V : Nat       -- vault TVL
  C_tot : Nat   -- sum of all account capitals
  I : Nat       -- insurance fund
  deriving Repr, DecidableEq, BEq

structure AccountState where
  C_i : Nat           -- protected principal
  fee_credits_i : Int -- fee balance (always <= 0)
  deriving Repr, DecidableEq, BEq

def MAX_VAULT_TVL : Nat := 10000000000000000

-- ============================================================================
-- Conservation predicate
-- ============================================================================

def conservation (s : EngineState) : Prop := s.V >= s.C_tot + s.I

-- ============================================================================
-- Transitions
-- ============================================================================

def depositTransition (s : EngineState) (amount : Nat) : Option EngineState :=
  if s.V + amount ≤ MAX_VAULT_TVL then
    some { V := s.V + amount, C_tot := s.C_tot + amount, I := s.I }
  else
    none

def topUpInsuranceTransition (s : EngineState) (amount : Nat) : Option EngineState :=
  if s.V + amount ≤ MAX_VAULT_TVL then
    some { V := s.V + amount, C_tot := s.C_tot, I := s.I + amount }
  else
    none

def depositFeeCreditsTransition (s : EngineState) (acct : AccountState) (amount : Nat) : Option (EngineState × AccountState) :=
  if acct.fee_credits_i ≤ 0 then
    let pay := min amount (Int.toNat (-acct.fee_credits_i))
    some ({ V := s.V + pay, C_tot := s.C_tot, I := s.I + pay },
          { acct with fee_credits_i := acct.fee_credits_i + pay })
  else
    none

-- ============================================================================
-- Property declarations (theorem stubs)
-- ============================================================================

-- Conservation: V >= C_tot + I after every operation

theorem deposit_conservation (s s' : EngineState) (amount : Nat)
    (h_inv : conservation s)
    (h : depositTransition s amount = some s') :
    conservation s' := sorry

theorem top_up_insurance_conservation (s s' : EngineState) (amount : Nat)
    (h_inv : conservation s)
    (h : topUpInsuranceTransition s amount = some s') :
    conservation s' := sorry

theorem deposit_fee_credits_conservation (s s' : EngineState) (acct acct' : AccountState) (amount : Nat)
    (h_inv : conservation s)
    (h : depositFeeCreditsTransition s acct amount = some (s', acct')) :
    conservation s' := sorry

-- Arithmetic: deposit cannot exceed MAX_VAULT_TVL

theorem deposit_bounded (s s' : EngineState) (amount : Nat)
    (h : depositTransition s amount = some s') :
    s'.V ≤ MAX_VAULT_TVL := sorry

-- Fee isolation: fee_credits remain non-positive

theorem fee_credits_nonpositive (s : EngineState) (acct acct' : AccountState) (amount : Nat) (s' : EngineState)
    (h_pre : acct.fee_credits_i ≤ 0)
    (h : depositFeeCreditsTransition s acct amount = some (s', acct')) :
    acct'.fee_credits_i ≤ 0 := sorry

-- ADL lifecycle: Normal → DrainOnly → ResetPending → Normal

inductive SideMode where
  | Normal
  | DrainOnly
  | ResetPending
  deriving Repr, DecidableEq, BEq

def validAdlTransition : SideMode → SideMode → Prop
  | .Normal, .DrainOnly => True
  | .DrainOnly, .ResetPending => True
  | .ResetPending, .Normal => True
  | _, _ => False

theorem adl_lifecycle (m m' : SideMode) (h : validAdlTransition m m') :
    (m = .Normal ∧ m' = .DrainOnly) ∨
    (m = .DrainOnly ∧ m' = .ResetPending) ∨
    (m = .ResetPending ∧ m' = .Normal) := sorry

end Percolator
