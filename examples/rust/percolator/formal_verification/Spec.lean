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
  if signer = s.authority ∧ s.status = .Active ∧ s.V + amount ≤ 10000000000000000 then
    some { s with V := s.V + amount, C_tot := s.C_tot + amount, status := .Active }
  else none

def withdrawTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ amount ≤ s.V ∧ amount ≤ s.C_tot then
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

def conservation (s : State) : Prop := s.V ≥ s.C_tot + s.I

/-- conservation is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/
theorem conservation_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : conservation s) (h : applyOp s signer op = some s') : conservation s' := sorry

def vault_bounded (s : State) : Prop := s.V ≤ 10000000000000000

/-- vault_bounded is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/
theorem vault_bounded_inductive (s s' : State) (signer : Pubkey) (op : Operation)
    (h_inv : vault_bounded s) (h : applyOp s signer op = some s') : vault_bounded s' := sorry

end Percolator
