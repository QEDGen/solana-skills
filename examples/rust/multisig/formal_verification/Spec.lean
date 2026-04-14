import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import QEDGen.Solana.Verify

namespace Multisig

open QEDGen.Solana

inductive Status where
  | Uninitialized
  | Active
  | HasProposal
  deriving Repr, DecidableEq, BEq

structure State where
  creator : Pubkey
  threshold : Nat
  member_count : Nat
  approval_count : Nat
  status : Status
  deriving Repr, DecidableEq, BEq

def create_vaultTransition (s : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat) : Option State :=
  if signer = s.creator ∧ s.status = .Uninitialized ∧ threshold > 0 ∧ threshold ≤ member_count ∧ member_count ≤ 32 then
    some { s with threshold := threshold, member_count := member_count, approval_count := 0, status := .Active }
  else none

def proposeTransition (s : State) (signer : Pubkey) : Option State :=
  if s.status = .Active then
    some { s with approval_count := 0, status := .HasProposal }
  else none

def approveTransition (s : State) (signer : Pubkey) (member_index : Nat) : Option State :=
  if s.status = .HasProposal ∧ member_index < s.member_count ∧ s.approval_count < s.member_count then
    some { s with approval_count := s.approval_count + 1, status := .HasProposal }
  else none

def executeTransition (s : State) (signer : Pubkey) : Option State :=
  if s.status = .HasProposal ∧ s.approval_count ≥ s.threshold then
    some { s with approval_count := 0, status := .Active }
  else none

def cancel_proposalTransition (s : State) (signer : Pubkey) : Option State :=
  if s.status = .HasProposal then
    some { s with approval_count := 0, status := .Active }
  else none

def remove_memberTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.creator ∧ s.status = .Active ∧ 1 ≤ s.member_count ∧ s.member_count > s.threshold then
    some { s with member_count := s.member_count - 1, status := .Active }
  else none

inductive Operation where
  | create_vault (threshold : Nat) (member_count : Nat)
  | propose
  | approve (member_index : Nat)
  | execute
  | cancel_proposal
  | remove_member
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .create_vault threshold member_count => create_vaultTransition s signer threshold member_count
  | .propose => proposeTransition s signer
  | .approve member_index => approveTransition s signer member_index
  | .execute => executeTransition s signer
  | .cancel_proposal => cancel_proposalTransition s signer
  | .remove_member => remove_memberTransition s signer

-- ============================================================================
-- Access control
-- ============================================================================

theorem create_vault_access_control (s : State) (p : Pubkey) (threshold member_count : Nat)
    (h : create_vaultTransition s p threshold member_count ≠ none) :
    p = s.creator := by
  simp [create_vaultTransition] at h; exact h.1

theorem remove_member_access_control (s : State) (p : Pubkey)
    (h : remove_memberTransition s p ≠ none) :
    p = s.creator := by
  simp [remove_memberTransition] at h; exact h.1

-- ============================================================================
-- State machine
-- ============================================================================

theorem create_vault_state_machine (s s' : State) (p : Pubkey) (threshold member_count : Nat)
    (h : create_vaultTransition s p threshold member_count = some s') :
    s.status = .Uninitialized ∧ s'.status = .Active := by
  simp [create_vaultTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem propose_state_machine (s s' : State) (p : Pubkey)
    (h : proposeTransition s p = some s') :
    s.status = .Active ∧ s'.status = .HasProposal := by
  simp [proposeTransition] at h
  obtain ⟨h_pre, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem approve_state_machine (s s' : State) (p : Pubkey) (member_index : Nat)
    (h : approveTransition s p member_index = some s') :
    s.status = .HasProposal ∧ s'.status = .HasProposal := by
  simp [approveTransition] at h
  obtain ⟨⟨h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem execute_state_machine (s s' : State) (p : Pubkey)
    (h : executeTransition s p = some s') :
    s.status = .HasProposal ∧ s'.status = .Active := by
  simp [executeTransition] at h
  obtain ⟨⟨h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem cancel_proposal_state_machine (s s' : State) (p : Pubkey)
    (h : cancel_proposalTransition s p = some s') :
    s.status = .HasProposal ∧ s'.status = .Active := by
  simp [cancel_proposalTransition] at h
  obtain ⟨h_pre, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem remove_member_state_machine (s s' : State) (p : Pubkey)
    (h : remove_memberTransition s p = some s') :
    s.status = .Active ∧ s'.status = .Active := by
  simp [remove_memberTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- ============================================================================
-- threshold_bounded: threshold ≤ member_count ∧ threshold > 0
-- ============================================================================

def threshold_bounded (s : State) : Prop := s.threshold ≤ s.member_count ∧ s.threshold > 0

theorem threshold_bounded_preserved_by_create_vault (s s' : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat)
    (h_inv : threshold_bounded s) (h : create_vaultTransition s signer threshold member_count = some s') :
    threshold_bounded s' := by
  simp [create_vaultTransition] at h
  obtain ⟨⟨_, _, h_gt, h_le, _⟩, h_eq⟩ := h
  subst h_eq; simp [threshold_bounded]; omega

theorem threshold_bounded_preserved_by_propose (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s) (h : proposeTransition s signer = some s') :
    threshold_bounded s' := by
  simp [proposeTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem threshold_bounded_preserved_by_approve (s s' : State) (signer : Pubkey) (member_index : Nat)
    (h_inv : threshold_bounded s) (h : approveTransition s signer member_index = some s') :
    threshold_bounded s' := by
  simp [approveTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem threshold_bounded_preserved_by_execute (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s) (h : executeTransition s signer = some s') :
    threshold_bounded s' := by
  simp [executeTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem threshold_bounded_preserved_by_cancel_proposal (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s) (h : cancel_proposalTransition s signer = some s') :
    threshold_bounded s' := by
  simp [cancel_proposalTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; exact h_inv

theorem threshold_bounded_preserved_by_remove_member (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s) (h : remove_memberTransition s signer = some s') :
    threshold_bounded s' := by
  simp [remove_memberTransition] at h
  obtain ⟨⟨_, _, _, h_gt⟩, h_eq⟩ := h
  subst h_eq; simp [threshold_bounded] at h_inv ⊢; omega

/-- threshold_bounded is preserved by every operation. Auto-proven by case split. -/
theorem threshold_bounded_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : threshold_bounded s) (h : applyOp s signer op = some s') : threshold_bounded s' := by
  cases op with
  | create_vault threshold member_count => exact threshold_bounded_preserved_by_create_vault s s' signer threshold member_count h_inv h
  | propose => exact threshold_bounded_preserved_by_propose s s' signer h_inv h
  | approve member_index => exact threshold_bounded_preserved_by_approve s s' signer member_index h_inv h
  | execute => exact threshold_bounded_preserved_by_execute s s' signer h_inv h
  | cancel_proposal => exact threshold_bounded_preserved_by_cancel_proposal s s' signer h_inv h
  | remove_member => exact threshold_bounded_preserved_by_remove_member s s' signer h_inv h

-- ============================================================================
-- approvals_bounded: approval_count ≤ member_count
-- ============================================================================

def approvals_bounded (s : State) : Prop := s.approval_count ≤ s.member_count

theorem approvals_bounded_preserved_by_create_vault (s s' : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat)
    (h_inv : approvals_bounded s) (h : create_vaultTransition s signer threshold member_count = some s') :
    approvals_bounded s' := by
  simp [create_vaultTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [approvals_bounded]

theorem approvals_bounded_preserved_by_propose (s s' : State) (signer : Pubkey)
    (h_inv : approvals_bounded s) (h : proposeTransition s signer = some s') :
    approvals_bounded s' := by
  simp [proposeTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [approvals_bounded]

theorem approvals_bounded_preserved_by_approve (s s' : State) (signer : Pubkey) (member_index : Nat)
    (h_inv : approvals_bounded s) (h : approveTransition s signer member_index = some s') :
    approvals_bounded s' := by
  simp [approveTransition] at h
  obtain ⟨⟨_, h_lt⟩, h_eq⟩ := h
  subst h_eq; simp [approvals_bounded]; omega

theorem approvals_bounded_preserved_by_execute (s s' : State) (signer : Pubkey)
    (h_inv : approvals_bounded s) (h : executeTransition s signer = some s') :
    approvals_bounded s' := by
  simp [executeTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [approvals_bounded]

theorem approvals_bounded_preserved_by_cancel_proposal (s s' : State) (signer : Pubkey)
    (h_inv : approvals_bounded s) (h : cancel_proposalTransition s signer = some s') :
    approvals_bounded s' := by
  simp [cancel_proposalTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [approvals_bounded]

-- Note: approvals_bounded_inductive is not emitted because remove_member is not
-- in the preserved_by list and doesn't preserve approvals_bounded without a
-- lifecycle invariant (status = Active → approval_count = 0).

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem create_vault_aborts_if_InvalidThreshold (s : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat)
    (h : threshold == 0 ∨ threshold > member_count) : create_vaultTransition s signer threshold member_count = none := by
  simp [create_vaultTransition]
  intro _ _
  cases h with
  | inl h_z => simp at h_z; omega
  | inr h_big => omega

theorem create_vault_aborts_if_TooManyMembers (s : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat)
    (h : member_count > 32) : create_vaultTransition s signer threshold member_count = none := by
  simp [create_vaultTransition]
  intro _ _ _ _; omega

theorem approve_aborts_if_NotAMember (s : State) (signer : Pubkey) (member_index : Nat)
    (h : member_index ≥ s.member_count) : approveTransition s signer member_index = none := by
  simp [approveTransition]
  intro _; omega

theorem execute_aborts_if_ThresholdNotMet (s : State) (signer : Pubkey)
    (h : s.approval_count < s.threshold) : executeTransition s signer = none := by
  simp [executeTransition]
  intro _; omega

-- ============================================================================
-- Cover properties — reachability (existential proofs)
-- ============================================================================

/-- proposal_lifecycle — trace [create_vault, propose, approve, execute] is reachable. -/
theorem cover_proposal_lifecycle : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), create_vaultTransition s0 signer v0_0 v0_1 = some s1 ∧
∃ (s2 : State), proposeTransition s1 signer = some s2 ∧
        ∃ (v2_0 : Nat), ∃ (s3 : State), approveTransition s2 signer v2_0 = some s3 ∧
executeTransition s3 signer ≠ none := by
  let pk : Pubkey := ⟨0, 0, 0, 0⟩
  exact ⟨⟨pk, 0, 0, 0, .Uninitialized⟩, pk, 1, 1,
    ⟨pk, 1, 1, 0, .Active⟩, by decide,
    ⟨pk, 1, 1, 0, .HasProposal⟩, by decide,
    0, ⟨pk, 1, 1, 1, .HasProposal⟩, by decide, by decide⟩

/-- cancel_flow — trace [create_vault, propose, cancel_proposal] is reachable. -/
theorem cover_cancel_flow : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), create_vaultTransition s0 signer v0_0 v0_1 = some s1 ∧
∃ (s2 : State), proposeTransition s1 signer = some s2 ∧
cancel_proposalTransition s2 signer ≠ none := by
  let pk : Pubkey := ⟨0, 0, 0, 0⟩
  exact ⟨⟨pk, 0, 0, 0, .Uninitialized⟩, pk, 1, 1,
    ⟨pk, 1, 1, 0, .Active⟩, by decide,
    ⟨pk, 1, 1, 0, .HasProposal⟩, by decide, by decide⟩

-- ============================================================================
-- Liveness properties — bounded reachability (leads-to)
-- ============================================================================

def applyOps (s : State) (signer : Pubkey) : List Operation → Option State
  | [] => some s
  | op :: ops => match applyOp s signer op with
    | some s' => applyOps s' signer ops
    | none => none

/-- proposal_resolves — from HasProposal leads to Active within 1 steps via [execute, cancel_proposal]. -/
theorem liveness_proposal_resolves (s : State) (signer : Pubkey)
    (h : s.status = .HasProposal) :
    ∃ ops, ops.length ≤ 1 ∧ ∀ s', applyOps s signer ops = some s' → s'.status = .Active := by
  exact ⟨[.cancel_proposal], by decide, fun s' h_apply => by
    simp only [applyOps, applyOp] at h_apply
    cases hc : cancel_proposalTransition s signer with
    | none => simp [hc] at h_apply
    | some val =>
      simp [hc] at h_apply
      subst h_apply
      simp [cancel_proposalTransition, h] at hc
      obtain ⟨_, rfl⟩ := hc
      rfl⟩

-- ============================================================================
-- Overflow safety obligations (auto-generated for operations with add effects)
-- ============================================================================

theorem approve_overflow_safe (s s' : State) (signer : Pubkey) (member_index : Nat)
    (h_valid : valid_u8 s.threshold ∧ valid_u8 s.member_count ∧ valid_u8 s.approval_count)
    (h : approveTransition s signer member_index = some s') :
    valid_u8 s'.threshold ∧ valid_u8 s'.member_count ∧ valid_u8 s'.approval_count := by
  simp [approveTransition] at h
  obtain ⟨⟨_, h_lt⟩, h_eq⟩ := h
  obtain ⟨ht, hm, ha⟩ := h_valid
  subst h_eq
  refine ⟨ht, hm, ?_⟩
  simp only [valid_u8, Valid.valid_u8, Valid.U8_MAX] at hm ha ⊢
  omega

end Multisig

#qedgen_verify Multisig
