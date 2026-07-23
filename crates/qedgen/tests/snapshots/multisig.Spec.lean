import Mathlib.Algebra.BigOperators.Fin
import QEDGen.Solana.Account
import QEDGenMathlib.IndexedState

namespace Multisig

open QEDGen.Solana
open QEDGen.Solana.IndexedState

abbrev MAX_MEMBERS : Nat := 32

abbrev AccountIdx : Type := Fin MAX_MEMBERS

inductive Status where
  | Uninitialized
  | Active
  | HasProposal
  deriving Repr, DecidableEq, BEq

structure State where
  creator : Pubkey
  threshold : Nat
  member_count : Nat
  members : Map MAX_MEMBERS Pubkey
  voted : Map MAX_MEMBERS U8
  approval_count : Nat
  rejection_count : Nat
  status : Status

def create_vaultTransition (s : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat) : Option State :=
  if signer = s.creator ∧ s.status = .Uninitialized ∧ (threshold > 0 ∧ threshold ≤ member_count) ∧ (member_count ≤ 32) then
    some { s with threshold := threshold, member_count := member_count, approval_count := 0, rejection_count := 0, status := .Active }
  else none

def proposeTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.creator ∧ s.status = .Active then
    some { s with approval_count := 0, rejection_count := 0, status := .HasProposal }
  else none

def approveTransition (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS) : Option State :=
  let approver := signer
  if s.status = .HasProposal ∧ (member_index < s.member_count) ∧ ((s.members member_index) = approver) ∧ ((s.voted member_index) = 0) then
    some { s with approval_count := s.approval_count + 1, voted := Function.update s.voted member_index (1), status := .HasProposal }
  else none

def rejectTransition (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS) : Option State :=
  let rejecter := signer
  if s.status = .HasProposal ∧ (member_index < s.member_count) ∧ ((s.members member_index) = rejecter) ∧ ((s.voted member_index) = 0) then
    some { s with rejection_count := s.rejection_count + 1, voted := Function.update s.voted member_index (1), status := .HasProposal }
  else none

def executeTransition (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS) : Option State :=
  let executor := signer
  if s.status = .HasProposal ∧ (member_index < s.member_count) ∧ ((s.members member_index) = executor) ∧ (s.approval_count ≥ s.threshold) then
    some { s with approval_count := 0, rejection_count := 0, status := .Active }
  else none

def cancel_proposalTransition (s : State) (signer : Pubkey) : Option State :=
  if s.status = .HasProposal ∧ (s.member_count - s.rejection_count < s.threshold) then
    some { s with approval_count := 0, rejection_count := 0, status := .Active }
  else none

def add_memberTransition (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS) (member_pubkey : Pubkey) : Option State :=
  if signer = s.creator ∧ s.status = .Active ∧ (member_index < s.member_count) then
    some { s with members := Function.update s.members member_index (member_pubkey), status := .Active }
  else none

def remove_memberTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.creator ∧ s.status = .Active ∧ (s.member_count > s.threshold) ∧ (s.approval_count = 0 ∧ s.rejection_count = 0) then
    some { s with member_count := s.member_count - 1, status := .Active }
  else none

inductive Operation where
  | create_vault (threshold : Nat) (member_count : Nat)
  | propose
  | approve (member_index : Fin MAX_MEMBERS)
  | reject (member_index : Fin MAX_MEMBERS)
  | execute (member_index : Fin MAX_MEMBERS)
  | cancel_proposal
  | add_member (member_index : Fin MAX_MEMBERS) (member_pubkey : Pubkey)
  | remove_member

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .create_vault threshold member_count => create_vaultTransition s signer threshold member_count
  | .propose => proposeTransition s signer
  | .approve member_index => approveTransition s signer member_index
  | .reject member_index => rejectTransition s signer member_index
  | .execute member_index => executeTransition s signer member_index
  | .cancel_proposal => cancel_proposalTransition s signer
  | .add_member member_index member_pubkey => add_memberTransition s signer member_index member_pubkey
  | .remove_member => remove_memberTransition s signer

/-- Property: threshold_bounded. -/
def threshold_bounded (s : State) : Prop :=
  s.threshold ≤ s.member_count ∧ s.threshold > 0

/-- Property: votes_bounded. -/
def votes_bounded (s : State) : Prop :=
  s.approval_count + s.rejection_count ≤ s.member_count

-- ============================================================================
-- Obligation statements (#336) — machine-owned `Prop`s.
--
-- Prove each in Proofs.lean as
--   theorem <name> : <name>_stmt := by intro … 
-- The statement is generated from the spec; only the proof is yours.
-- ============================================================================

def threshold_bounded_preserved_by_create_vault_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat),
    threshold_bounded s → create_vaultTransition s signer threshold member_count = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_propose_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey),
    threshold_bounded s → proposeTransition s signer = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_approve_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    threshold_bounded s → approveTransition s signer member_index = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_reject_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    threshold_bounded s → rejectTransition s signer member_index = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_execute_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    threshold_bounded s → executeTransition s signer member_index = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_cancel_proposal_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey),
    threshold_bounded s → cancel_proposalTransition s signer = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_add_member_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS) (member_pubkey : Pubkey),
    threshold_bounded s → add_memberTransition s signer member_index member_pubkey = some s' → threshold_bounded s'

def threshold_bounded_preserved_by_remove_member_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey),
    threshold_bounded s → remove_memberTransition s signer = some s' → threshold_bounded s'

def votes_bounded_preserved_by_create_vault_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat),
    votes_bounded s → create_vaultTransition s signer threshold member_count = some s' → votes_bounded s'

def votes_bounded_preserved_by_propose_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey),
    votes_bounded s → proposeTransition s signer = some s' → votes_bounded s'

def votes_bounded_preserved_by_execute_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    votes_bounded s → executeTransition s signer member_index = some s' → votes_bounded s'

def votes_bounded_preserved_by_cancel_proposal_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey),
    votes_bounded s → cancel_proposalTransition s signer = some s' → votes_bounded s'

def votes_bounded_preserved_by_remove_member_stmt : Prop :=
  ∀ (s s' : State) (signer : Pubkey),
    votes_bounded s → remove_memberTransition s signer = some s' → votes_bounded s'

def create_vault_aborts_if_InvalidThreshold_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat),
    ¬(threshold > 0 ∧ threshold ≤ member_count) → create_vaultTransition s signer threshold member_count = none

def create_vault_aborts_if_TooManyMembers_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (threshold : Nat) (member_count : Nat),
    ¬(member_count ≤ 32) → create_vaultTransition s signer threshold member_count = none

def approve_aborts_if_NotAMember_0_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬(member_index < s.member_count) → approveTransition s signer member_index = none

def approve_aborts_if_NotAMember_1_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬((s.members member_index) = signer) → approveTransition s signer member_index = none

def approve_aborts_if_AlreadyVoted_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬((s.voted member_index) = 0) → approveTransition s signer member_index = none

def reject_aborts_if_NotAMember_0_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬(member_index < s.member_count) → rejectTransition s signer member_index = none

def reject_aborts_if_NotAMember_1_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬((s.members member_index) = signer) → rejectTransition s signer member_index = none

def reject_aborts_if_AlreadyVoted_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬((s.voted member_index) = 0) → rejectTransition s signer member_index = none

def execute_aborts_if_NotAMember_0_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬(member_index < s.member_count) → executeTransition s signer member_index = none

def execute_aborts_if_NotAMember_1_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬((s.members member_index) = signer) → executeTransition s signer member_index = none

def execute_aborts_if_ThresholdNotMet_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS),
    ¬(s.approval_count ≥ s.threshold) → executeTransition s signer member_index = none

def cancel_proposal_aborts_if_ThresholdUnreachable_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey),
    ¬(s.member_count - s.rejection_count < s.threshold) → cancel_proposalTransition s signer = none

def add_member_aborts_if_NotAMember_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS) (member_pubkey : Pubkey),
    ¬(member_index < s.member_count) → add_memberTransition s signer member_index member_pubkey = none

def cover_proposal_lifecycle_stmt : Prop :=
  ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), create_vaultTransition s0 signer v0_0 v0_1 = some s1 ∧
∃ (s2 : State), proposeTransition s1 signer = some s2 ∧
        ∃ (v2_0 : Fin MAX_MEMBERS), ∃ (s3 : State), approveTransition s2 signer v2_0 = some s3 ∧
          ∃ (v3_0 : Fin MAX_MEMBERS), executeTransition s3 signer v3_0 ≠ none

def cover_rejection_flow_stmt : Prop :=
  ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), create_vaultTransition s0 signer v0_0 v0_1 = some s1 ∧
∃ (s2 : State), proposeTransition s1 signer = some s2 ∧
        ∃ (v2_0 : Fin MAX_MEMBERS), ∃ (s3 : State), rejectTransition s2 signer v2_0 = some s3 ∧
cancel_proposalTransition s3 signer ≠ none

def applyOps (s : State) (signer : Pubkey) : List Operation → Option State
  | [] => some s
  | op :: ops => match applyOp s signer op with
    | some s' => applyOps s' signer ops
    | none => none

def liveness_proposal_resolves_stmt : Prop :=
  ∀ (s : State) (signer : Pubkey), s.status = .HasProposal →
    ∃ ops s', ops.length ≤ 1 ∧ applyOps s signer ops = some s' ∧ s'.status = .Active

end Multisig
