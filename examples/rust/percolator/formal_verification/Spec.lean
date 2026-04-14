import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import QEDGen.Solana.Verify

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
  if signer = s.authority ∧ s.status = .Active ∧ s.V + amount ≤ 10000000000000000 then
    some { s with V := s.V + amount, C_tot := s.C_tot + amount, status := .Active }
  else none

def withdrawTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ amount ≤ s.V ∧ amount ≤ s.C_tot ∧ s.C_tot ≥ amount then
    some { s with V := s.V - amount, C_tot := s.C_tot - amount, status := .Active }
  else none

def top_up_insuranceTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ s.V + amount ≤ 10000000000000000 then
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

-- ============================================================================
-- Access control: only authority can call operations
-- ============================================================================

theorem deposit_access_control (s : State) (p : Pubkey) (amount : Nat)
    (h : depositTransition s p amount ≠ none) : p = s.authority := by
  simp [depositTransition] at h; exact h.1

theorem withdraw_access_control (s : State) (p : Pubkey) (amount : Nat)
    (h : withdrawTransition s p amount ≠ none) : p = s.authority := by
  simp [withdrawTransition] at h; exact h.1

theorem top_up_insurance_access_control (s : State) (p : Pubkey) (amount : Nat)
    (h : top_up_insuranceTransition s p amount ≠ none) : p = s.authority := by
  simp [top_up_insuranceTransition] at h; exact h.1

theorem trigger_adl_access_control (s : State) (p : Pubkey)
    (h : trigger_adlTransition s p ≠ none) : p = s.authority := by
  simp [trigger_adlTransition] at h; exact h.1

theorem complete_drain_access_control (s : State) (p : Pubkey)
    (h : complete_drainTransition s p ≠ none) : p = s.authority := by
  simp [complete_drainTransition] at h; exact h.1

theorem reset_access_control (s : State) (p : Pubkey)
    (h : resetTransition s p ≠ none) : p = s.authority := by
  simp [resetTransition] at h; exact h.1

-- ============================================================================
-- State machine: ADL lifecycle Active → Draining → Resetting → Active
-- ============================================================================

theorem deposit_state_machine (s s' : State) (p : Pubkey) (amount : Nat)
    (h : depositTransition s p amount = some s') :
    s.status = .Active ∧ s'.status = .Active := by
  simp [depositTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem withdraw_state_machine (s s' : State) (p : Pubkey) (amount : Nat)
    (h : withdrawTransition s p amount = some s') :
    s.status = .Active ∧ s'.status = .Active := by
  simp [withdrawTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem top_up_insurance_state_machine (s s' : State) (p : Pubkey) (amount : Nat)
    (h : top_up_insuranceTransition s p amount = some s') :
    s.status = .Active ∧ s'.status = .Active := by
  simp [top_up_insuranceTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem trigger_adl_state_machine (s s' : State) (p : Pubkey)
    (h : trigger_adlTransition s p = some s') :
    s.status = .Active ∧ s'.status = .Draining := by
  simp [trigger_adlTransition] at h
  obtain ⟨⟨_, h_pre⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem complete_drain_state_machine (s s' : State) (p : Pubkey)
    (h : complete_drainTransition s p = some s') :
    s.status = .Draining ∧ s'.status = .Resetting := by
  simp [complete_drainTransition] at h
  obtain ⟨⟨_, h_pre⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem reset_state_machine (s s' : State) (p : Pubkey)
    (h : resetTransition s p = some s') :
    s.status = .Resetting ∧ s'.status = .Active := by
  simp [resetTransition] at h
  obtain ⟨⟨_, h_pre⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- ============================================================================
-- Conservation: V ≥ C_tot + I (preserved by all 6 operations)
-- ============================================================================

def conservation (s : State) : Prop := s.V ≥ s.C_tot + s.I

theorem conservation_preserved_by_deposit (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : depositTransition s signer amount = some s') :
    conservation s' := by
  simp [depositTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [conservation] at h_inv ⊢; omega

theorem conservation_preserved_by_withdraw (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : withdrawTransition s signer amount = some s') :
    conservation s' := by
  simp [withdrawTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [conservation] at h_inv ⊢; omega

theorem conservation_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : top_up_insuranceTransition s signer amount = some s') :
    conservation s' := by
  simp [top_up_insuranceTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [conservation] at h_inv ⊢; omega

theorem conservation_preserved_by_trigger_adl (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : trigger_adlTransition s signer = some s') :
    conservation s' := by
  simp [trigger_adlTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem conservation_preserved_by_complete_drain (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : complete_drainTransition s signer = some s') :
    conservation s' := by
  simp [complete_drainTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem conservation_preserved_by_reset (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : resetTransition s signer = some s') :
    conservation s' := by
  simp [resetTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

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

-- ============================================================================
-- Vault bounded: V ≤ 10_000_000_000_000_000 (preserved by deposit, top_up)
-- ============================================================================

def vault_bounded (s : State) : Prop := s.V ≤ 10000000000000000

theorem vault_bounded_preserved_by_deposit (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : vault_bounded s) (h : depositTransition s signer amount = some s') :
    vault_bounded s' := by
  simp [depositTransition] at h
  obtain ⟨⟨_, _, h_guard⟩, h_eq⟩ := h
  subst h_eq; unfold vault_bounded; omega

theorem vault_bounded_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_inv : vault_bounded s) (h : top_up_insuranceTransition s signer amount = some s') :
    vault_bounded s' := by
  simp [top_up_insuranceTransition] at h
  obtain ⟨⟨_, _, h_guard⟩, h_eq⟩ := h
  subst h_eq; unfold vault_bounded; omega

/-- vault_bounded is preserved by every operation. Auto-proven by case split. -/
theorem vault_bounded_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : vault_bounded s) (h : applyOp s signer op = some s') : vault_bounded s' := by
  cases op with
  | deposit amount => exact vault_bounded_preserved_by_deposit s s' signer amount h_inv h
  | withdraw amount =>
    simp [applyOp, withdrawTransition] at h
    obtain ⟨_, h_eq⟩ := h; subst h_eq
    simp [vault_bounded] at h_inv ⊢; omega
  | top_up_insurance amount => exact vault_bounded_preserved_by_top_up_insurance s s' signer amount h_inv h
  | trigger_adl =>
    simp [applyOp, trigger_adlTransition] at h
    obtain ⟨_, h_eq⟩ := h; subst h_eq; exact h_inv
  | complete_drain =>
    simp [applyOp, complete_drainTransition] at h
    obtain ⟨_, h_eq⟩ := h; subst h_eq; exact h_inv
  | reset =>
    simp [applyOp, resetTransition] at h
    obtain ⟨_, h_eq⟩ := h; subst h_eq; exact h_inv

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem withdraw_aborts_if_InsufficientFunds (s : State) (signer : Pubkey) (amount : Nat)
    (h : s.C_tot < amount) : withdrawTransition s signer amount = none := by
  simp [withdrawTransition]; intro _ _; omega

-- ============================================================================
-- Cover properties — reachability (existential proofs)
-- ============================================================================

/-- happy_path — trace [deposit, withdraw] is reachable. -/
theorem cover_happy_path : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat), ∃ (s1 : State), depositTransition s0 signer v0_0 = some s1 ∧
      ∃ (v1_0 : Nat), withdrawTransition s1 signer v1_0 ≠ none := by
  let pk : Pubkey := ⟨0, 0, 0, 0⟩
  exact ⟨⟨pk, 0, 0, 0, .Active⟩, pk, 1,
    ⟨pk, 1, 1, 0, .Active⟩, by decide,
    1, by decide⟩

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
  exact ⟨[.complete_drain, .reset], by decide, fun s' h_apply => by
    simp only [applyOps, applyOp] at h_apply
    cases hc : complete_drainTransition s signer with
    | none => simp [hc] at h_apply
    | some val =>
      simp [hc] at h_apply
      cases hr : resetTransition val signer with
      | none => simp [hr] at h_apply
      | some val2 =>
        simp [hr] at h_apply
        subst h_apply
        simp [complete_drainTransition, h] at hc
        obtain ⟨_, rfl⟩ := hc
        simp [resetTransition] at hr
        obtain ⟨_, rfl⟩ := hr
        rfl⟩

-- ============================================================================
-- U64 bounds: all U64 fields remain in bounds after each operation
-- ============================================================================

-- Note: deposit.u64_bounds and top_up_insurance.u64_bounds are NOT provable
-- without conservation as a precondition. The guard bounds V+amount ≤ MAX
-- but does not bound C_tot+amount or I+amount individually.

theorem withdraw_u64_bounds (s s' : State) (p : Pubkey) (amount : Nat)
    (h_valid : valid_u64 s.V ∧ valid_u64 s.C_tot ∧ valid_u64 s.I)
    (h : withdrawTransition s p amount = some s') :
    valid_u64 s'.V ∧ valid_u64 s'.C_tot ∧ valid_u64 s'.I := by
  simp [withdrawTransition] at h
  obtain ⟨_, h_eq⟩ := h
  obtain ⟨hv, hc, hi⟩ := h_valid
  have hv' : valid_u64 (s.V - amount) := by
    simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX] at hv ⊢; omega
  have hc' : valid_u64 (s.C_tot - amount) := by
    simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX] at hc ⊢; omega
  subst h_eq; exact ⟨hv', hc', hi⟩

theorem trigger_adl_u64_bounds (s s' : State) (p : Pubkey)
    (h_valid : valid_u64 s.V ∧ valid_u64 s.C_tot ∧ valid_u64 s.I)
    (h : trigger_adlTransition s p = some s') :
    valid_u64 s'.V ∧ valid_u64 s'.C_tot ∧ valid_u64 s'.I := by
  simp [trigger_adlTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_valid

theorem complete_drain_u64_bounds (s s' : State) (p : Pubkey)
    (h_valid : valid_u64 s.V ∧ valid_u64 s.C_tot ∧ valid_u64 s.I)
    (h : complete_drainTransition s p = some s') :
    valid_u64 s'.V ∧ valid_u64 s'.C_tot ∧ valid_u64 s'.I := by
  simp [complete_drainTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_valid

theorem reset_u64_bounds (s s' : State) (p : Pubkey)
    (h_valid : valid_u64 s.V ∧ valid_u64 s.C_tot ∧ valid_u64 s.I)
    (h : resetTransition s p = some s') :
    valid_u64 s'.V ∧ valid_u64 s'.C_tot ∧ valid_u64 s'.I := by
  simp [resetTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_valid

-- ============================================================================
-- Overflow safety obligations (auto-generated for operations with add effects)
-- ============================================================================

-- Note: deposit and top_up_insurance overflow safety requires conservation
-- (V ≥ C_tot + I) and vault_bounded (V ≤ 10^16) as preconditions, because
-- the guard only bounds V+amount but not C_tot+amount or I+amount individually.

theorem deposit_overflow_safe (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_valid : valid_u128 s.V ∧ valid_u128 s.C_tot ∧ valid_u128 s.I)
    (h_cons : conservation s) (h_vb : vault_bounded s)
    (h : depositTransition s signer amount = some s') :
    valid_u128 s'.V ∧ valid_u128 s'.C_tot ∧ valid_u128 s'.I := by
  simp [depositTransition] at h
  obtain ⟨⟨_, _, h_guard⟩, h_eq⟩ := h
  obtain ⟨_, _, hi⟩ := h_valid
  subst h_eq
  refine ⟨?_, ?_, hi⟩
  · simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]; omega
  · simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]
    unfold conservation at h_cons; unfold vault_bounded at h_vb; omega

theorem top_up_insurance_overflow_safe (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_valid : valid_u128 s.V ∧ valid_u128 s.C_tot ∧ valid_u128 s.I)
    (h_cons : conservation s) (h_vb : vault_bounded s)
    (h : top_up_insuranceTransition s signer amount = some s') :
    valid_u128 s'.V ∧ valid_u128 s'.C_tot ∧ valid_u128 s'.I := by
  simp [top_up_insuranceTransition] at h
  obtain ⟨⟨_, _, h_guard⟩, h_eq⟩ := h
  obtain ⟨_, hc, _⟩ := h_valid
  subst h_eq
  refine ⟨?_, hc, ?_⟩
  · simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]; omega
  · simp only [valid_u128, Valid.valid_u128, Valid.U128_MAX]
    unfold conservation at h_cons; unfold vault_bounded at h_vb; omega

end Percolator

#qedgen_verify Percolator
