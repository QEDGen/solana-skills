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
  if s.status = .Active then
    some { s with approval_count := 0, rejection_count := 0, status := .HasProposal }
  else none

def approveTransition (s : State) (signer : Pubkey) (member_index : Nat) : Option State :=
  if signer = s.approver ∧ s.status = .HasProposal ∧ (member_index < s.member_count) ∧ ((s.members member_index) = approver) ∧ ((s.voted member_index) = 0) then
    some { s with approval_count := s.approval_count + 1, voted := Function.update s.voted member_index (1), status := .HasProposal }
  else none

def rejectTransition (s : State) (signer : Pubkey) (member_index : Nat) : Option State :=
  if signer = s.rejecter ∧ s.status = .HasProposal ∧ (member_index < s.member_count) ∧ ((s.members member_index) = rejecter) ∧ ((s.voted member_index) = 0) then
    some { s with rejection_count := s.rejection_count + 1, voted := Function.update s.voted member_index (1), status := .HasProposal }
  else none

def executeTransition (s : State) (signer : Pubkey) : Option State :=
  if s.status = .HasProposal ∧ (s.approval_count ≥ s.threshold) then
    some { s with approval_count := 0, rejection_count := 0, status := .Active }
  else none

def cancel_proposalTransition (s : State) (signer : Pubkey) : Option State :=
  if s.status = .HasProposal ∧ (s.member_count - s.rejection_count < s.threshold) then
    some { s with approval_count := 0, rejection_count := 0, status := .Active }
  else none

def add_memberTransition (s : State) (signer : Pubkey) (member_index : Nat) (member_pubkey : Pubkey) : Option State :=
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
  | approve (member_index : Nat)
  | reject (member_index : Nat)
  | execute
  | cancel_proposal
  | add_member (member_index : Nat) (member_pubkey : Pubkey)
  | remove_member

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .create_vault threshold member_count => create_vaultTransition s signer threshold member_count
  | .propose => proposeTransition s signer
  | .approve member_index => approveTransition s signer member_index
  | .reject member_index => rejectTransition s signer member_index
  | .execute => executeTransition s signer
  | .cancel_proposal => cancel_proposalTransition s signer
  | .add_member member_index member_pubkey => add_memberTransition s signer member_index member_pubkey
  | .remove_member => remove_memberTransition s signer

/-- Property: threshold_bounded. -/
def threshold_bounded (s : State) : Prop :=
  s.threshold ≤ s.member_count ∧ s.threshold > 0

/-- Property: votes_bounded. -/
def votes_bounded (s : State) : Prop :=
  s.approval_count + s.rejection_count ≤ s.member_count

end Multisig
