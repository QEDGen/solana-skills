import Spec
import QEDGen.Solana.Verify
open QEDGen.Solana
open Percolator

/-!
# Percolator Risk Engine — Proofs

All properties proven against DSL-generated transitions in Spec.lean.
No Mathlib dependency — `simp` + `omega` handle everything.

24 of 26 expected properties proven. The 2 missing (deposit.u64_bounds,
top_up_insurance.u64_bounds) genuinely need conservation as a precondition
— the guard bounds V+amount but not C_tot+amount or I+amount individually.
-/

namespace Percolator.Proofs

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
-- Conservation: V ≥ C_tot + I (preserved by all 6 operations)
-- ============================================================================

theorem deposit_preserves_conservation (s s' : State) (p : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : depositTransition s p amount = some s') :
    conservation s' := by
  simp [depositTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [conservation] at h_inv ⊢; omega

theorem withdraw_preserves_conservation (s s' : State) (p : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : withdrawTransition s p amount = some s') :
    conservation s' := by
  simp [withdrawTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [conservation] at h_inv ⊢; omega

theorem top_up_insurance_preserves_conservation (s s' : State) (p : Pubkey) (amount : Nat)
    (h_inv : conservation s) (h : top_up_insuranceTransition s p amount = some s') :
    conservation s' := by
  simp [top_up_insuranceTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [conservation] at h_inv ⊢; omega

theorem trigger_adl_preserves_conservation (s s' : State) (p : Pubkey)
    (h_inv : conservation s) (h : trigger_adlTransition s p = some s') :
    conservation s' := by
  simp [trigger_adlTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem complete_drain_preserves_conservation (s s' : State) (p : Pubkey)
    (h_inv : conservation s) (h : complete_drainTransition s p = some s') :
    conservation s' := by
  simp [complete_drainTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem reset_preserves_conservation (s s' : State) (p : Pubkey)
    (h_inv : conservation s) (h : resetTransition s p = some s') :
    conservation s' := by
  simp [resetTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

-- ============================================================================
-- Vault bounded: V ≤ 10_000_000_000_000_000 (preserved by deposit, top_up)
-- ============================================================================

theorem deposit_preserves_vault_bounded (s s' : State) (p : Pubkey) (amount : Nat)
    (_ : vault_bounded s) (h : depositTransition s p amount = some s') :
    vault_bounded s' := by
  simp [depositTransition] at h
  obtain ⟨⟨_, _, h_guard⟩, h_eq⟩ := h
  subst h_eq; unfold vault_bounded; omega

theorem top_up_insurance_preserves_vault_bounded (s s' : State) (p : Pubkey) (amount : Nat)
    (_ : vault_bounded s) (h : top_up_insuranceTransition s p amount = some s') :
    vault_bounded s' := by
  simp [top_up_insuranceTransition] at h
  obtain ⟨⟨_, _, h_guard⟩, h_eq⟩ := h
  subst h_eq; unfold vault_bounded; omega

end Percolator.Proofs

#qedgen_verify Percolator.Proofs
