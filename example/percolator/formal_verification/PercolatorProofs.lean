import QEDGen.Solana
import Mathlib

open QEDGen.Solana

/- ============================================================================
   Deposit Conservation Proof

   deposit(s, amount):
     V' = V + amount, C_tot' = C_tot + amount, I' = I
   Conservation: V >= C_tot + I  →  V' >= C_tot' + I'
   ============================================================================ -/

namespace DepositConservation

structure EngineState where
  V : Nat       -- vault TVL
  C_tot : Nat   -- sum of all account capitals
  I : Nat       -- insurance fund

def conservation (p_s : EngineState) : Prop := p_s.V >= p_s.C_tot + p_s.I

def MAX_VAULT_TVL : Nat := 10000000000000000

def depositTransition (p_s : EngineState) (p_amount : Nat) : Option EngineState :=
  if p_s.V + p_amount ≤ MAX_VAULT_TVL then
    some { V := p_s.V + p_amount, C_tot := p_s.C_tot + p_amount, I := p_s.I }
  else
    none

theorem deposit_conservation (p_s p_s' : EngineState) (p_amount : Nat)
    (h_inv : conservation p_s)
    (h : depositTransition p_s p_amount = some p_s') :
    conservation p_s' := by
  unfold depositTransition at h
  split_ifs at h with h_le
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  omega

end DepositConservation

/- ============================================================================
   Top-Up Insurance Conservation Proof

   top_up_insurance(s, amount):
     V' = V + amount, I' = I + amount, C_tot' = C_tot
   Conservation: V >= C_tot + I  →  V' >= C_tot' + I'
   ============================================================================ -/

namespace TopUpInsuranceConservation

structure EngineState where
  V : Nat
  C_tot : Nat
  I : Nat

def conservation (p_s : EngineState) : Prop := p_s.V >= p_s.C_tot + p_s.I

def MAX_VAULT_TVL : Nat := 10000000000000000

def topUpTransition (p_s : EngineState) (p_amount : Nat) : Option EngineState :=
  if p_s.V + p_amount ≤ MAX_VAULT_TVL then
    some { V := p_s.V + p_amount, C_tot := p_s.C_tot, I := p_s.I + p_amount }
  else
    none

theorem top_up_insurance_conservation (p_s p_s' : EngineState) (p_amount : Nat)
    (h_inv : conservation p_s)
    (h : topUpTransition p_s p_amount = some p_s') :
    conservation p_s' := by
  unfold topUpTransition at h
  split_ifs at h with h_le
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  omega

end TopUpInsuranceConservation

/- ============================================================================
   Deposit Fee Credits Conservation Proof

   deposit_fee_credits(s, amount):
     pay = min(amount, debt)  where debt = -fee_credits (fee_credits <= 0)
     V' = V + pay, I' = I + pay, C_tot' = C_tot
   Conservation: V >= C_tot + I  →  V' >= C_tot' + I'
   ============================================================================ -/

namespace DepositFeeCreditsConservation

structure EngineState where
  V : Nat
  C_tot : Nat
  I : Nat

def conservation (p_s : EngineState) : Prop := p_s.V >= p_s.C_tot + p_s.I

def MAX_VAULT_TVL : Nat := 10000000000000000

def depositFeeCreditsTransition (p_s : EngineState) (p_pay : Nat) : Option EngineState :=
  if p_s.V + p_pay ≤ MAX_VAULT_TVL then
    some { V := p_s.V + p_pay, C_tot := p_s.C_tot, I := p_s.I + p_pay }
  else
    none

theorem deposit_fee_credits_conservation (p_s p_s' : EngineState) (p_pay : Nat)
    (h_inv : conservation p_s)
    (h : depositFeeCreditsTransition p_s p_pay = some p_s') :
    conservation p_s' := by
  unfold depositFeeCreditsTransition at h
  split_ifs at h with h_le
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  omega

end DepositFeeCreditsConservation

/- ============================================================================
   PNL Negation Safety Proof

   For all x : Int, if x > -(2^127) then -x ≤ 2^127 - 1.
   i.e., negation of a valid i128 (excluding MIN) stays in i128 range.
   ============================================================================ -/

namespace PnlNegationSafety

def I128_MIN : Int := -(2 ^ 127)
def I128_MAX : Int := 2 ^ 127 - 1

theorem pnl_negation_safety (p_x : Int)
    (h_lower : p_x ≥ I128_MIN)
    (h_upper : p_x ≤ I128_MAX)
    (h_not_min : p_x ≠ I128_MIN) :
    -p_x ≥ I128_MIN ∧ -p_x ≤ I128_MAX := by
  unfold I128_MIN at h_lower h_not_min ⊢
  unfold I128_MAX at h_upper ⊢
  omega

end PnlNegationSafety

/- ============================================================================
   Deposit Bounded Proof

   If deposit succeeds, V' ≤ MAX_VAULT_TVL.
   ============================================================================ -/

namespace DepositBounded

def MAX_VAULT_TVL : Nat := 10000000000000000

structure EngineState where
  V : Nat
  C_tot : Nat
  I : Nat

def depositTransition (p_s : EngineState) (p_amount : Nat) : Option EngineState :=
  if p_s.V + p_amount ≤ MAX_VAULT_TVL then
    some { V := p_s.V + p_amount, C_tot := p_s.C_tot + p_amount, I := p_s.I }
  else
    none

theorem deposit_bounded (p_s p_s' : EngineState) (p_amount : Nat)
    (h : depositTransition p_s p_amount = some p_s') :
    p_s'.V ≤ MAX_VAULT_TVL := by
  unfold depositTransition at h
  split_ifs at h with h_le
  cases h
  exact h_le

end DepositBounded

/- ============================================================================
   ADL Lifecycle Proof

   SideMode transitions: Normal → DrainOnly → ResetPending → Normal
   No other transitions are permitted.
   ============================================================================ -/

namespace AdlLifecycle

inductive SideMode where
  | Normal
  | DrainOnly
  | ResetPending
  deriving DecidableEq

def adlTransition (p_m : SideMode) : Option SideMode :=
  match p_m with
  | SideMode.Normal       => some SideMode.DrainOnly
  | SideMode.DrainOnly    => some SideMode.ResetPending
  | SideMode.ResetPending => some SideMode.Normal

theorem adl_lifecycle_drain (p_m p_m' : SideMode)
    (h : p_m = SideMode.Normal)
    (h_t : adlTransition p_m = some p_m') :
    p_m' = SideMode.DrainOnly := by
  subst h
  unfold adlTransition at h_t
  cases h_t
  rfl

theorem adl_lifecycle_reset (p_m p_m' : SideMode)
    (h : p_m = SideMode.DrainOnly)
    (h_t : adlTransition p_m = some p_m') :
    p_m' = SideMode.ResetPending := by
  subst h
  unfold adlTransition at h_t
  cases h_t
  rfl

theorem adl_lifecycle_normal (p_m p_m' : SideMode)
    (h : p_m = SideMode.ResetPending)
    (h_t : adlTransition p_m = some p_m') :
    p_m' = SideMode.Normal := by
  subst h
  unfold adlTransition at h_t
  cases h_t
  rfl

theorem adl_lifecycle_total (p_m : SideMode) :
    ∃ p_m', adlTransition p_m = some p_m' := by
  cases p_m
  · exact ⟨SideMode.DrainOnly, rfl⟩
  · exact ⟨SideMode.ResetPending, rfl⟩
  · exact ⟨SideMode.Normal, rfl⟩

end AdlLifecycle

/- ============================================================================
   Fee Credits Nonpositive Proof

   If fee_credits <= 0 and pay = min(amount, -fee_credits),
   then fee_credits + pay <= 0.
   ============================================================================ -/

namespace FeeCreditsNonpositive

theorem fee_credits_nonpositive (p_fee_credits : Int) (p_amount : Int)
    (_h_neg : p_fee_credits ≤ 0)
    (_h_amt_pos : p_amount ≥ 0)
    (p_pay : Int)
    (h_pay : p_pay = min p_amount (-p_fee_credits)) :
    p_fee_credits + p_pay ≤ 0 := by
  rw [h_pay]
  cases le_total p_amount (-p_fee_credits) with
  | inl h_le =>
    have h_pay_eq : min p_amount (-p_fee_credits) = p_amount := by
      apply min_eq_left
      linarith
    rw [h_pay_eq]
    linarith
  | inr h_le =>
    have h_pay_eq : min p_amount (-p_fee_credits) = -p_fee_credits := by
      apply min_eq_right
      linarith
    rw [h_pay_eq]
    linarith

end FeeCreditsNonpositive
