import Mathlib.Algebra.BigOperators.Fin
import QEDGen.Solana.Account
import QEDGen.Solana.IndexedState

namespace Percolator

open QEDGen.Solana
open QEDGen.Solana.IndexedState

abbrev MAX_ACCOUNTS : Nat := 1024
abbrev MAX_VAULT_TVL : Nat := 10000000000000000
abbrev POS_SCALE : Nat := 1000000
abbrev MAX_ACCOUNT_NOTIONAL : Nat := 100000000000000000000

abbrev AccountIdx : Type := Fin MAX_ACCOUNTS

structure Account where
  active : Nat
  capital : Nat
  reserved_pnl : Nat
  pnl : Int
  fee_credits : Nat
  deriving Repr, DecidableEq, BEq

instance : Inhabited Account := ⟨{
  active := 0,
  capital := 0,
  reserved_pnl := 0,
  pnl := 0,
  fee_credits := 0,
}⟩

inductive Status where
  | Active
  | Draining
  | Resetting
  deriving Repr, DecidableEq, BEq

structure State where
  authority : Pubkey
  V : Nat
  I : Nat
  F : Nat
  accounts : Map MAX_ACCOUNTS Account
  status : Status

def add_userTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 0) ∧ (((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((0) : Int))) then
    some { s with accounts := Function.update s.accounts i { (s.accounts i) with active := 1 }, status := .Active }
  else none

def add_lpTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 0) ∧ (((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((0) : Int))) then
    some { s with accounts := Function.update s.accounts i { (s.accounts i) with active := 1 }, status := .Active }
  else none

def reclaim_empty_accountTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ ((s.accounts i).capital = 0) ∧ ((s.accounts i).reserved_pnl = 0) ∧ ((s.accounts i).fee_credits = 0) then
    some { s with accounts := Function.update s.accounts i { (s.accounts i) with active := 0 }, status := .Active }
  else none

def close_accountTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ (s.V ≥ (s.accounts i).capital) then
    some { s with V := s.V - (s.accounts i).capital, accounts := Function.update s.accounts i { (s.accounts i) with capital := 0, active := 0 }, status := .Active }
  else none

def depositTransition (s : State) (signer : Pubkey) (i : AccountIdx) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ (s.V + amount ≤ 10000000000000000) then
    some { s with V := s.V + amount, accounts := Function.update s.accounts i { (s.accounts i) with capital := (s.accounts i).capital + amount }, status := .Active }
  else none

def withdrawTransition (s : State) (signer : Pubkey) (i : AccountIdx) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ ((s.accounts i).capital ≥ amount) ∧ (((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((amount) : Int))) then
    some { s with V := s.V - amount, accounts := Function.update s.accounts i { (s.accounts i) with capital := (s.accounts i).capital - amount }, status := .Active }
  else none

def top_up_insuranceTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ (s.V + amount ≤ 10000000000000000) then
    some { s with V := s.V + amount, I := s.I + amount, status := .Active }
  else none

def deposit_fee_creditsTransition (s : State) (signer : Pubkey) (i : AccountIdx) (amount : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ (s.V + amount ≤ 10000000000000000) then
    some { s with V := s.V + amount, F := s.F + amount, accounts := Function.update s.accounts i { (s.accounts i) with fee_credits := (s.accounts i).fee_credits + amount }, status := .Active }
  else none

def convert_released_pnlTransition (s : State) (signer : Pubkey) (i : AccountIdx) (x : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ ((s.accounts i).reserved_pnl ≥ x) ∧ (s.V ≥ x) then
    some { s with V := s.V - x, accounts := Function.update s.accounts i { (s.accounts i) with reserved_pnl := (s.accounts i).reserved_pnl - x }, status := .Active }
  else none

def execute_tradeTransition (s : State) (signer : Pubkey) (a : AccountIdx) (b : AccountIdx) (size_q : Int) (exec_price : Nat) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts a).active = 1) ∧ ((s.accounts b).active = 1) ∧ (a ≠ b) ∧ ((((size_q) * ((((exec_price) : Int)))) / (1000000)) ≤ (((100000000000000000000) : Int))) then
    some { s with status := .Active }
  else none

def liquidate_case_0Transition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ (((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((0) : Int))) ∧ (0 = 1) then
    some { s with status := .Active }
  else none

def liquidate_case_1Transition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ (¬(((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((0) : Int)))) ∧ (((((s.accounts i).capital) : Int)) + (s.accounts i).pnl + (((s.I) : Int)) ≥ (((0) : Int))) then
    some { s with accounts := Function.update s.accounts i { (s.accounts i) with active := 0 }, status := .Active }
  else none

def liquidate_otherwiseTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ (¬(((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((0) : Int)))) ∧ (¬(((((s.accounts i).capital) : Int)) + (s.accounts i).pnl + (((s.I) : Int)) ≥ (((0) : Int)))) ∧ (0 = 1) then
    some { s with status := .Active }
  else none

def settle_accountTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) then
    some { s with status := .Active }
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
  | add_user (i : AccountIdx)
  | add_lp (i : AccountIdx)
  | reclaim_empty_account (i : AccountIdx)
  | close_account (i : AccountIdx)
  | deposit (i : AccountIdx) (amount : Nat)
  | withdraw (i : AccountIdx) (amount : Nat)
  | top_up_insurance (amount : Nat)
  | deposit_fee_credits (i : AccountIdx) (amount : Nat)
  | convert_released_pnl (i : AccountIdx) (x : Nat)
  | execute_trade (a : AccountIdx) (b : AccountIdx) (size_q : Int) (exec_price : Nat)
  | liquidate_case_0 (i : AccountIdx)
  | liquidate_case_1 (i : AccountIdx)
  | liquidate_otherwise (i : AccountIdx)
  | settle_account (i : AccountIdx)
  | trigger_adl
  | complete_drain
  | reset

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .add_user i => add_userTransition s signer i
  | .add_lp i => add_lpTransition s signer i
  | .reclaim_empty_account i => reclaim_empty_accountTransition s signer i
  | .close_account i => close_accountTransition s signer i
  | .deposit i amount => depositTransition s signer i amount
  | .withdraw i amount => withdrawTransition s signer i amount
  | .top_up_insurance amount => top_up_insuranceTransition s signer amount
  | .deposit_fee_credits i amount => deposit_fee_creditsTransition s signer i amount
  | .convert_released_pnl i x => convert_released_pnlTransition s signer i x
  | .execute_trade a b size_q exec_price => execute_tradeTransition s signer a b size_q exec_price
  | .liquidate_case_0 i => liquidate_case_0Transition s signer i
  | .liquidate_case_1 i => liquidate_case_1Transition s signer i
  | .liquidate_otherwise i => liquidate_otherwiseTransition s signer i
  | .settle_account i => settle_accountTransition s signer i
  | .trigger_adl => trigger_adlTransition s signer
  | .complete_drain => complete_drainTransition s signer
  | .reset => resetTransition s signer

/-- Property: conservation. -/
def conservation (s : State) : Prop :=
  s.V ≥ ((∑ i : AccountIdx, (s.accounts i).capital)) + ((∑ i : AccountIdx, (s.accounts i).reserved_pnl)) + s.I + s.F

/-- Property: vault_bounded. -/
def vault_bounded (s : State) : Prop :=
  s.V ≤ 10000000000000000

/-- Property: account_solvent. -/
def account_solvent (s : State) : Prop :=
  ∀ i : AccountIdx, (s.accounts i).active = 1 → ((((s.accounts i).capital) : Int)) + (s.accounts i).pnl ≥ (((0) : Int))

end Percolator
