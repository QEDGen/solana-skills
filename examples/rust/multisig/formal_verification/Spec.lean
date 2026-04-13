import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

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
  if s.status = .HasProposal ∧ member_index < s.member_count then
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

def threshold_bounded (s : State) : Prop := s.threshold ≤ s.member_count ∧ s.threshold > 0

/-- threshold_bounded is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/
theorem threshold_bounded_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : threshold_bounded s) (h : applyOp s signer op = some s') : threshold_bounded s' := sorry

def approvals_bounded (s : State) : Prop := s.approval_count ≤ s.member_count

/-- approvals_bounded is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/
theorem approvals_bounded_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : approvals_bounded s) (h : applyOp s signer op = some s') : approvals_bounded s' := sorry

end Multisig
