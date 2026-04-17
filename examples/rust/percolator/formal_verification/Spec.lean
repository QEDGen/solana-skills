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
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 0) then
    some { s with accounts := Function.update s.accounts i { (s.accounts i) with active := 1 }, status := .Active }
  else none

def add_lpTransition (s : State) (signer : Pubkey) (i : AccountIdx) : Option State :=
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 0) then
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
  if signer = s.authority ∧ s.status = .Active ∧ ((s.accounts i).active = 1) ∧ ((s.accounts i).capital ≥ amount) then
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

theorem conservation_preserved_by_add_user (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : add_userTransition s signer i = some s') :
    conservation s' := by
  unfold add_userTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  first
    | (dsimp only; simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | (simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | sorry

theorem conservation_preserved_by_add_lp (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : add_lpTransition s signer i = some s') :
    conservation s' := by
  unfold add_lpTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  first
    | (dsimp only; simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | (simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | sorry

theorem conservation_preserved_by_reclaim_empty_account (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : reclaim_empty_accountTransition s signer i = some s') :
    conservation s' := by
  unfold reclaim_empty_accountTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  first
    | (dsimp only; simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | (simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | sorry

theorem conservation_preserved_by_close_account (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : close_accountTransition s signer i = some s') :
    conservation s' := by
  sorry

theorem conservation_preserved_by_deposit (s s' : State) (signer : Pubkey) i amount
    (h_inv : conservation s) (h : depositTransition s signer i amount = some s') :
    conservation s' := by
  sorry

theorem conservation_preserved_by_withdraw (s s' : State) (signer : Pubkey) i amount
    (h_inv : conservation s) (h : withdrawTransition s signer i amount = some s') :
    conservation s' := by
  sorry

theorem conservation_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) amount
    (h_inv : conservation s) (h : top_up_insuranceTransition s signer amount = some s') :
    conservation s' := by
  unfold top_up_insuranceTransition at h
  split_ifs at h with hg
  cases h
  simp only [conservation] at h_inv ⊢
  omega

theorem conservation_preserved_by_deposit_fee_credits (s s' : State) (signer : Pubkey) i amount
    (h_inv : conservation s) (h : deposit_fee_creditsTransition s signer i amount = some s') :
    conservation s' := by
  sorry

theorem conservation_preserved_by_convert_released_pnl (s s' : State) (signer : Pubkey) i x
    (h_inv : conservation s) (h : convert_released_pnlTransition s signer i x = some s') :
    conservation s' := by
  sorry

theorem conservation_preserved_by_execute_trade (s s' : State) (signer : Pubkey) a b size_q exec_price
    (h_inv : conservation s) (h : execute_tradeTransition s signer a b size_q exec_price = some s') :
    conservation s' := by
  unfold execute_tradeTransition at h
  split_ifs at h with hg
  cases h
  simpa [conservation] using h_inv

theorem conservation_preserved_by_liquidate_case_0 (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : liquidate_case_0Transition s signer i = some s') :
    conservation s' := by
  unfold liquidate_case_0Transition at h
  simp_all

theorem conservation_preserved_by_liquidate_case_1 (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : liquidate_case_1Transition s signer i = some s') :
    conservation s' := by
  unfold liquidate_case_1Transition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  first
    | (dsimp only; simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | (simp_rw [QEDGen.Solana.IndexedState.sum_update_proj_eq _ _ _ _ rfl]; exact h_inv)
    | sorry

theorem conservation_preserved_by_liquidate_otherwise (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : liquidate_otherwiseTransition s signer i = some s') :
    conservation s' := by
  unfold liquidate_otherwiseTransition at h
  simp_all

theorem conservation_preserved_by_settle_account (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : settle_accountTransition s signer i = some s') :
    conservation s' := by
  unfold settle_accountTransition at h
  split_ifs at h with hg
  cases h
  simpa [conservation] using h_inv

theorem conservation_preserved_by_trigger_adl (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : trigger_adlTransition s signer = some s') :
    conservation s' := by
  unfold trigger_adlTransition at h
  split_ifs at h with hg
  cases h
  simpa [conservation] using h_inv

theorem conservation_preserved_by_complete_drain (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : complete_drainTransition s signer = some s') :
    conservation s' := by
  unfold complete_drainTransition at h
  split_ifs at h with hg
  cases h
  simpa [conservation] using h_inv

theorem conservation_preserved_by_reset (s s' : State) (signer : Pubkey)
    (h_inv : conservation s) (h : resetTransition s signer = some s') :
    conservation s' := by
  unfold resetTransition at h
  split_ifs at h with hg
  cases h
  simpa [conservation] using h_inv

theorem vault_bounded_preserved_by_deposit (s s' : State) (signer : Pubkey) i amount
    (h_inv : vault_bounded s) (h : depositTransition s signer i amount = some s') :
    vault_bounded s' := by
  unfold depositTransition at h
  split_ifs at h with hg
  cases h
  simp only [vault_bounded] at h_inv ⊢
  omega

theorem vault_bounded_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) amount
    (h_inv : vault_bounded s) (h : top_up_insuranceTransition s signer amount = some s') :
    vault_bounded s' := by
  unfold top_up_insuranceTransition at h
  split_ifs at h with hg
  cases h
  simp only [vault_bounded] at h_inv ⊢
  omega

theorem vault_bounded_preserved_by_deposit_fee_credits (s s' : State) (signer : Pubkey) i amount
    (h_inv : vault_bounded s) (h : deposit_fee_creditsTransition s signer i amount = some s') :
    vault_bounded s' := by
  unfold deposit_fee_creditsTransition at h
  split_ifs at h with hg
  cases h
  simp only [vault_bounded] at h_inv ⊢
  omega

theorem account_solvent_preserved_by_add_user (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : add_userTransition s signer i = some s') :
    account_solvent s' := by
  unfold add_userTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_add_lp (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : add_lpTransition s signer i = some s') :
    account_solvent s' := by
  unfold add_lpTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_reclaim_empty_account (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : reclaim_empty_accountTransition s signer i = some s') :
    account_solvent s' := by
  unfold reclaim_empty_accountTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_close_account (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : close_accountTransition s signer i = some s') :
    account_solvent s' := by
  unfold close_accountTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_deposit (s s' : State) (signer : Pubkey) i amount
    (h_inv : account_solvent s) (h : depositTransition s signer i amount = some s') :
    account_solvent s' := by
  unfold depositTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_withdraw (s s' : State) (signer : Pubkey) i amount
    (h_inv : account_solvent s) (h : withdrawTransition s signer i amount = some s') :
    account_solvent s' := by
  unfold withdrawTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_top_up_insurance (s s' : State) (signer : Pubkey) amount
    (h_inv : account_solvent s) (h : top_up_insuranceTransition s signer amount = some s') :
    account_solvent s' := by
  unfold top_up_insuranceTransition at h
  split_ifs at h with hg
  cases h
  simpa [account_solvent] using h_inv

theorem account_solvent_preserved_by_deposit_fee_credits (s s' : State) (signer : Pubkey) i amount
    (h_inv : account_solvent s) (h : deposit_fee_creditsTransition s signer i amount = some s') :
    account_solvent s' := by
  unfold deposit_fee_creditsTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_convert_released_pnl (s s' : State) (signer : Pubkey) i x
    (h_inv : account_solvent s) (h : convert_released_pnlTransition s signer i x = some s') :
    account_solvent s' := by
  unfold convert_released_pnlTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_execute_trade (s s' : State) (signer : Pubkey) a b size_q exec_price
    (h_inv : account_solvent s) (h : execute_tradeTransition s signer a b size_q exec_price = some s') :
    account_solvent s' := by
  unfold execute_tradeTransition at h
  split_ifs at h with hg
  cases h
  simpa [account_solvent] using h_inv

theorem account_solvent_preserved_by_liquidate_case_0 (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : liquidate_case_0Transition s signer i = some s') :
    account_solvent s' := by
  unfold liquidate_case_0Transition at h
  simp_all

theorem account_solvent_preserved_by_liquidate_case_1 (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : liquidate_case_1Transition s signer i = some s') :
    account_solvent s' := by
  unfold liquidate_case_1Transition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  intro j
  by_cases hji : j = i
  · subst hji
    first
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; done)
    | (simp_all [account_solvent, Function.update_self, Function.update_of_ne]; omega)
    | sorry
  · -- j ≠ i: Map unchanged at j
    have := h_inv j
    simp [Function.update_of_ne hji]
    exact this

theorem account_solvent_preserved_by_liquidate_otherwise (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : liquidate_otherwiseTransition s signer i = some s') :
    account_solvent s' := by
  unfold liquidate_otherwiseTransition at h
  simp_all

theorem account_solvent_preserved_by_settle_account (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : settle_accountTransition s signer i = some s') :
    account_solvent s' := by
  unfold settle_accountTransition at h
  split_ifs at h with hg
  cases h
  simpa [account_solvent] using h_inv

theorem account_solvent_preserved_by_trigger_adl (s s' : State) (signer : Pubkey)
    (h_inv : account_solvent s) (h : trigger_adlTransition s signer = some s') :
    account_solvent s' := by
  unfold trigger_adlTransition at h
  split_ifs at h with hg
  cases h
  simpa [account_solvent] using h_inv

theorem account_solvent_preserved_by_complete_drain (s s' : State) (signer : Pubkey)
    (h_inv : account_solvent s) (h : complete_drainTransition s signer = some s') :
    account_solvent s' := by
  unfold complete_drainTransition at h
  split_ifs at h with hg
  cases h
  simpa [account_solvent] using h_inv

theorem account_solvent_preserved_by_reset (s s' : State) (signer : Pubkey)
    (h_inv : account_solvent s) (h : resetTransition s signer = some s') :
    account_solvent s' := by
  unfold resetTransition at h
  split_ifs at h with hg
  cases h
  simpa [account_solvent] using h_inv

/-- Liveness: Draining leads to Active via ["complete_drain", "reset"] within 2. -/
theorem liveness_drain_completes (s s' : State) (ops : List Operation)
    (h_start : s.status = .Draining)
    (h_end : s'.status = .Active) :
    ops.length ≤ 2 := by
  sorry

end Percolator
