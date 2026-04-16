import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

namespace Percolator

open QEDGen.Solana

inductive Status where
  | Active
  | Draining
  | Resetting
  deriving Repr, DecidableEq, BEq

structure State where
  authority : Pubkey
  V : Nat
  C_tot : Nat
  I : Nat
  status : Status
  deriving Repr, DecidableEq, BEq

def depositTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ s.V + amount ≤ 10000000000000000 ∧ s.C_tot + amount ≤ 340282366920938463463374607431768211455 then
    some { s with V := s.V + amount, C_tot := s.C_tot + amount, status := .Active }
  else none

def withdrawTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ amount ≤ s.V ∧ amount ≤ s.C_tot ∧ s.C_tot ≥ amount then
    some { s with V := s.V - amount, C_tot := s.C_tot - amount, status := .Active }
  else none

def top_up_insuranceTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ s.V + amount ≤ 10000000000000000 ∧ s.I + amount ≤ 340282366920938463463374607431768211455 then
    some { s with V := s.V + amount, I := s.I + amount, status := .Active }
  else none

def trigger_adlTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.authority ∧ s.status = .Active then
    some { s with status := .Draining }
  else none

def complete_drainTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.authority ∧ s.status = .Draining then
    some { s with status := .Resetting }
  else none

def resetTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.authority ∧ s.status = .Resetting then
    some { s with status := .Active }
  else none

inductive Operation where
  | deposit (amount : Nat)
  | withdraw (amount : Nat)
  | top_up_insurance (amount : Nat)
  | trigger_adl
  | complete_drain
  | reset
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .deposit amount => depositTransition s signer amount
  | .withdraw amount => withdrawTransition s signer amount
  | .top_up_insurance amount => top_up_insuranceTransition s signer amount
  | .trigger_adl => trigger_adlTransition s signer
  | .complete_drain => complete_drainTransition s signer
  | .reset => resetTransition s signer

def conservation (s : State) : Prop := s.V ≥ s.C_tot + s.I

theorem conservation_preserved_by_deposit (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : depositTransition s signer amount = some s') :
    conservation s' := by
  unfold depositTransition at h; split at h
  · next hg => cases h; unfold conservation at h_inv ⊢; dsimp; omega
  · contradiction

theorem conservation_preserved_by_withdraw (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : withdrawTransition s signer amount = some s') :
    conservation s' := by
  unfold withdrawTransition at h; split at h
  · next hg => cases h; unfold conservation at h_inv ⊢; dsimp; omega
  · contradiction

theorem conservation_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : top_up_insuranceTransition s signer amount = some s') :
    conservation s' := by
  unfold top_up_insuranceTransition at h; split at h
  · next hg => cases h; unfold conservation at h_inv ⊢; dsimp; omega
  · contradiction

theorem conservation_preserved_by_trigger_adl (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : trigger_adlTransition s signer = some s') :
    conservation s' := by
  unfold trigger_adlTransition at h; split at h
  · cases h; exact h_inv
  · contradiction

theorem conservation_preserved_by_complete_drain (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : complete_drainTransition s signer = some s') :
    conservation s' := by
  unfold complete_drainTransition at h; split at h
  · cases h; exact h_inv
  · contradiction

theorem conservation_preserved_by_reset (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : resetTransition s signer = some s') :
    conservation s' := by
  unfold resetTransition at h; split at h
  · cases h; exact h_inv
  · contradiction

/-- conservation is preserved by every operation. Auto-proven by case split. -/
theorem conservation_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : conservation s) (h : applyOp s signer op = some s') : conservation s' := by
  cases op with
  | deposit amount => exact conservation_preserved_by_deposit s s' signer amount h_inv h
  | withdraw amount => exact conservation_preserved_by_withdraw s s' signer amount h_inv h
  | top_up_insurance amount => exact conservation_preserved_by_top_up_insurance s s' signer amount h_inv h
  | trigger_adl => exact conservation_preserved_by_trigger_adl s s' signer h_inv h
  | complete_drain => exact conservation_preserved_by_complete_drain s s' signer h_inv h
  | reset => exact conservation_preserved_by_reset s s' signer h_inv h

def vault_bounded (s : State) : Prop := s.V ≤ 10000000000000000

theorem vault_bounded_preserved_by_deposit (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : vault_bounded s) (h : depositTransition s signer amount = some s') :
    vault_bounded s' := by
  unfold depositTransition at h; split at h
  · next hg => cases h; unfold vault_bounded at h_inv ⊢; dsimp; omega
  · contradiction

theorem vault_bounded_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : vault_bounded s) (h : top_up_insuranceTransition s signer amount = some s') :
    vault_bounded s' := by
  unfold top_up_insuranceTransition at h; split at h
  · next hg => cases h; unfold vault_bounded at h_inv ⊢; dsimp; omega
  · contradiction

/-- vault_bounded is preserved by every operation. Auto-proven by case split. -/
theorem vault_bounded_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : vault_bounded s) (h : applyOp s signer op = some s') : vault_bounded s' := by
  cases op with
  | deposit amount => exact vault_bounded_preserved_by_deposit s s' signer amount h_inv h
  | withdraw amount =>
    simp [applyOp] at h
    unfold withdrawTransition at h; split at h
    · next hg => cases h; unfold vault_bounded at h_inv ⊢; dsimp; omega
    · contradiction
  | top_up_insurance amount => exact vault_bounded_preserved_by_top_up_insurance s s' signer amount h_inv h
  | trigger_adl =>
    simp [applyOp, trigger_adlTransition] at h
    obtain ⟨_, h_eq⟩ := h
    subst h_eq; exact h_inv
  | complete_drain =>
    simp [applyOp, complete_drainTransition] at h
    obtain ⟨_, h_eq⟩ := h
    subst h_eq; exact h_inv
  | reset =>
    simp [applyOp, resetTransition] at h
    obtain ⟨_, h_eq⟩ := h
    subst h_eq; exact h_inv

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem withdraw_aborts_if_InsufficientFunds (s : State) (signer : Pubkey) (amount : Nat)
    (h : ¬(s.C_tot ≥ amount)) : withdrawTransition s signer amount = none := by
  unfold withdrawTransition
  rw [if_neg (fun hg => h hg.2.2.2.2)]

-- ============================================================================
-- Cover properties — reachability (existential proofs)
-- ============================================================================

/-- happy_path — trace [deposit, withdraw] is reachable. -/
theorem cover_happy_path : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat), ∃ (s1 : State), depositTransition s0 signer v0_0 = some s1 ∧
      ∃ (v1_0 : Nat), withdrawTransition s1 signer v1_0 ≠ none := by
  let pk : Pubkey := ⟨0, 0, 0, 0⟩
  let s0 : State := ⟨pk, 0, 0, 0, .Active⟩
  let s1 : State := ⟨pk, 1, 1, 0, .Active⟩
  exact ⟨s0, pk, 1, s1, by decide, 1, by decide⟩

-- ============================================================================
-- Liveness properties — bounded reachability (leads-to)
-- ============================================================================

def applyOps (s : State) (signer : Pubkey) : List Operation → Option State
  | [] => some s
  | op :: ops => match applyOp s signer op with
    | some s' => applyOps s' signer ops
    | none => none

/-- drain_completes — from Draining leads to Active within 2 steps via [complete_drain, reset]. -/
theorem liveness_drain_completes (s : State) (signer : Pubkey)
    (h : s.status = .Draining) :
    ∃ ops, ops.length ≤ 2 ∧ ∀ s', applyOps s signer ops = some s' → s'.status = .Active := by
  refine ⟨[.complete_drain, .reset], by decide, fun s' h_apply => ?_⟩
  simp only [applyOps, applyOp] at h_apply
  simp only [complete_drainTransition] at h_apply
  split at h_apply
  · next heq =>
    split at heq
    · next hg =>
      simp at heq
      subst heq
      simp only [resetTransition] at h_apply
      split at h_apply
      · next heq =>
        split at heq
        · next hg => simp at heq h_apply; subst heq; subst h_apply; rfl
        · simp at heq
      · simp at h_apply
    · simp at heq
  · simp at h_apply

-- ============================================================================
-- Overflow safety obligations (auto-generated for operations with add effects)
-- ============================================================================

theorem deposit_overflow_safe (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_valid : valid_u128 s.V ∧ valid_u128 s.C_tot ∧ valid_u128 s.I)
    (h_inv_conservation : conservation s)
    (h_inv_vault_bounded : vault_bounded s)
    (h : depositTransition s signer amount = some s') :
    valid_u128 s'.V ∧ valid_u128 s'.C_tot ∧ valid_u128 s'.I := by
  unfold depositTransition at h; split at h
  · next hg =>
    cases h
    refine ⟨?_, ?_, h_valid.2.2⟩
    simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]; omega
    simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]; omega
  · contradiction

theorem top_up_insurance_overflow_safe (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_valid : valid_u128 s.V ∧ valid_u128 s.C_tot ∧ valid_u128 s.I)
    (h_inv_conservation : conservation s)
    (h_inv_vault_bounded : vault_bounded s)
    (h : top_up_insuranceTransition s signer amount = some s') :
    valid_u128 s'.V ∧ valid_u128 s'.C_tot ∧ valid_u128 s'.I := by
  unfold top_up_insuranceTransition at h; split at h
  · next hg =>
    cases h
    refine ⟨?_, h_valid.2.1, ?_⟩
    simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]; omega
    simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]; omega
  · contradiction

end Percolator
