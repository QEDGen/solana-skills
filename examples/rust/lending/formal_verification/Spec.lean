import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

namespace Lending

open QEDGen.Solana

inductive PoolStatus where
  | Uninitialized
  | Active
  | Paused
  deriving Repr, DecidableEq, BEq

structure PoolState where
  authority : Pubkey
  total_deposits : Nat
  total_borrows : Nat
  interest_rate : Nat
  status : PoolStatus
  deriving Repr, DecidableEq, BEq

def init_poolTransition (s : PoolState) (signer : Pubkey) (rate : Nat) : Option PoolState :=
  if signer = s.authority ∧ s.status = .Uninitialized ∧ rate > 0 then
    some { s with interest_rate := rate, total_deposits := 0, total_borrows := 0, status := .Active }
  else none

def depositTransition (s : PoolState) (signer : Pubkey) (amount : Nat) : Option PoolState :=
  if s.status = .Active ∧ amount > 0 then
    some { s with total_deposits := s.total_deposits + amount, status := .Active }
  else none

inductive PoolOperation where
  | init_pool (rate : Nat)
  | deposit (amount : Nat)
  deriving Repr, DecidableEq, BEq

def applyPoolOp (s : PoolState) (signer : Pubkey) : PoolOperation → Option PoolState
  | .init_pool rate => init_poolTransition s signer rate
  | .deposit amount => depositTransition s signer amount

inductive LoanStatus where
  | Empty
  | Active
  | Liquidated
  deriving Repr, DecidableEq, BEq

structure LoanState where
  borrower : Pubkey
  pool : Pubkey
  amount : Nat
  collateral : Nat
  status : LoanStatus
  deriving Repr, DecidableEq, BEq

def borrowTransition (s : LoanState) (signer : Pubkey) (amount : Nat) (collateral : Nat) : Option LoanState :=
  if signer = s.borrower ∧ s.status = .Empty ∧ amount > 0 ∧ collateral > 0 then
    some { s with amount := amount, collateral := collateral, status := .Active }
  else none

def repayTransition (s : LoanState) (signer : Pubkey) : Option LoanState :=
  if signer = s.borrower ∧ s.status = .Active then
    some { s with amount := 0, collateral := 0, status := .Empty }
  else none

def liquidateTransition (s : LoanState) (signer : Pubkey) : Option LoanState :=
  if s.status = .Active then
    some { s with amount := 0, status := .Liquidated }
  else none

inductive LoanOperation where
  | borrow (amount : Nat) (collateral : Nat)
  | repay
  | liquidate
  deriving Repr, DecidableEq, BEq

def applyLoanOp (s : LoanState) (signer : Pubkey) : LoanOperation → Option LoanState
  | .borrow amount collateral => borrowTransition s signer amount collateral
  | .repay => repayTransition s signer
  | .liquidate => liquidateTransition s signer

/-- Invariant: collateral_backing. -/
theorem collateral_backing : True := sorry

def pool_solvency (s : PoolState) : Prop := s.total_deposits ≥ s.total_borrows

/-- pool_solvency is preserved by every operation. Prove by `cases op` with unfold/omega per case. -/
theorem pool_solvency_inductive (s s' : PoolState) (signer : Pubkey) (op : PoolOperation)
    (h_inv : pool_solvency s) (h : applyPoolOp s signer op = some s') : pool_solvency s' := sorry

end Lending
