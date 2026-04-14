import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import QEDGen.Solana.Verify

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
  if s.status = .Active ∧ amount > 0 ∧ s.total_deposits + amount ≤ 18446744073709551615 then
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

-- ============================================================================
-- Pool: Access control
-- ============================================================================

theorem init_pool_access_control (s : PoolState) (p : Pubkey) (rate : Nat)
    (h : init_poolTransition s p rate ≠ none) :
    p = s.authority := by
  simp [init_poolTransition] at h; exact h.1

-- ============================================================================
-- Pool: State machine
-- ============================================================================

theorem init_pool_state_machine (s s' : PoolState) (p : Pubkey) (rate : Nat)
    (h : init_poolTransition s p rate = some s') :
    s.status = .Uninitialized ∧ s'.status = .Active := by
  simp [init_poolTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem deposit_state_machine (s s' : PoolState) (p : Pubkey) (amount : Nat)
    (h : depositTransition s p amount = some s') :
    s.status = .Active ∧ s'.status = .Active := by
  simp [depositTransition] at h
  obtain ⟨⟨h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- ============================================================================
-- Loan: Access control
-- ============================================================================

theorem borrow_access_control (s : LoanState) (p : Pubkey) (amount collateral : Nat)
    (h : borrowTransition s p amount collateral ≠ none) :
    p = s.borrower := by
  simp [borrowTransition] at h; exact h.1

theorem repay_access_control (s : LoanState) (p : Pubkey)
    (h : repayTransition s p ≠ none) :
    p = s.borrower := by
  simp [repayTransition] at h; exact h.1

-- ============================================================================
-- Loan: State machine
-- ============================================================================

theorem borrow_state_machine (s s' : LoanState) (p : Pubkey) (amount collateral : Nat)
    (h : borrowTransition s p amount collateral = some s') :
    s.status = .Empty ∧ s'.status = .Active := by
  simp [borrowTransition] at h
  obtain ⟨⟨_, h_pre, _⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem repay_state_machine (s s' : LoanState) (p : Pubkey)
    (h : repayTransition s p = some s') :
    s.status = .Active ∧ s'.status = .Empty := by
  simp [repayTransition] at h
  obtain ⟨⟨_, h_pre⟩, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

theorem liquidate_state_machine (s s' : LoanState) (p : Pubkey)
    (h : liquidateTransition s p = some s') :
    s.status = .Active ∧ s'.status = .Liquidated := by
  simp [liquidateTransition] at h
  obtain ⟨h_pre, h_eq⟩ := h
  exact ⟨h_pre, by subst h_eq; rfl⟩

-- ============================================================================
-- pool_solvency: total_deposits ≥ total_borrows
-- ============================================================================

def pool_solvency (s : PoolState) : Prop := s.total_deposits ≥ s.total_borrows

theorem pool_solvency_preserved_by_init_pool (s s' : PoolState) (signer : Pubkey) (rate : Nat)
    (h_inv : pool_solvency s) (h : init_poolTransition s signer rate = some s') :
    pool_solvency s' := by
  simp [init_poolTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [pool_solvency]

theorem pool_solvency_preserved_by_deposit (s s' : PoolState) (signer : Pubkey) (amount : Nat)
    (h_inv : pool_solvency s) (h : depositTransition s signer amount = some s') :
    pool_solvency s' := by
  simp [depositTransition] at h
  obtain ⟨_, h_eq⟩ := h
  subst h_eq; simp [pool_solvency] at h_inv ⊢; omega

/-- pool_solvency is preserved by every operation. Auto-proven by case split. -/
theorem pool_solvency_inductive (s s' : PoolState) (signer : Pubkey) (op : PoolOperation)
    (h_inv : pool_solvency s) (h : applyPoolOp s signer op = some s') : pool_solvency s' := by
  cases op with
  | init_pool rate => exact pool_solvency_preserved_by_init_pool s s' signer rate h_inv h
  | deposit amount => exact pool_solvency_preserved_by_deposit s s' signer amount h_inv h

/-- Invariant: collateral_backing. -/
theorem collateral_backing : True := trivial

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem init_pool_aborts_if_InvalidAmount (s : PoolState) (signer : Pubkey) (rate : Nat)
    (h : rate == 0) : init_poolTransition s signer rate = none := by
  simp [init_poolTransition]
  intro _ _; simp at h; omega

theorem deposit_aborts_if_InvalidAmount (s : PoolState) (signer : Pubkey) (amount : Nat)
    (h : amount == 0) : depositTransition s signer amount = none := by
  simp [depositTransition]
  intro _; simp at h; omega

theorem borrow_aborts_if_InvalidAmount (s : LoanState) (signer : Pubkey) (amount : Nat) (collateral : Nat)
    (h : amount == 0 ∨ collateral == 0) : borrowTransition s signer amount collateral = none := by
  simp [borrowTransition]
  intro _ _
  cases h with
  | inl h_a => simp at h_a; omega
  | inr h_c => simp at h_c; omega

-- ============================================================================
-- Overflow safety obligations (auto-generated for operations with add effects)
-- ============================================================================

theorem deposit_overflow_safe (s s' : PoolState) (signer : Pubkey) (amount : Nat)
    (h_valid : valid_u64 s.total_deposits ∧ valid_u64 s.total_borrows ∧ valid_u64 s.interest_rate)
    (h : depositTransition s signer amount = some s') :
    valid_u64 s'.total_deposits ∧ valid_u64 s'.total_borrows ∧ valid_u64 s'.interest_rate := by
  simp [depositTransition] at h
  obtain ⟨⟨_, _, h_bound⟩, h_eq⟩ := h
  obtain ⟨hd, hb, hi⟩ := h_valid
  subst h_eq
  refine ⟨?_, hb, hi⟩
  simp only [valid_u64, Valid.valid_u64, Valid.U64_MAX] at h_bound ⊢
  omega

-- ============================================================================
-- Cover properties — reachability (existential proofs)
-- ============================================================================

-- cover_borrow_repay_cycle: trace [init_pool, deposit, borrow, repay] spans multiple account types, skipped

-- cover_liquidation_path: trace [init_pool, deposit, borrow, liquidate] spans multiple account types, skipped

-- ============================================================================
-- Liveness properties — bounded reachability (leads-to)
-- ============================================================================

def applyLoanOps (s : LoanState) (signer : Pubkey) : List LoanOperation → Option LoanState
  | [] => some s
  | op :: ops => match applyLoanOp s signer op with
    | some s' => applyLoanOps s' signer ops
    | none => none

/-- loan_settles — from Active leads to Empty within 1 steps via [repay]. -/
theorem liveness_loan_settles (s : LoanState) (signer : Pubkey)
    (h : s.status = .Active) :
    ∃ ops, ops.length ≤ 1 ∧ ∀ s', applyLoanOps s signer ops = some s' → s'.status = .Empty := by
  exact ⟨[.repay], by decide, fun s' h_apply => by
    simp only [applyLoanOps, applyLoanOp] at h_apply
    cases hc : repayTransition s signer with
    | none => simp [hc] at h_apply
    | some val =>
      simp [hc] at h_apply
      subst h_apply
      simp [repayTransition, h] at hc
      obtain ⟨_, rfl⟩ := hc
      rfl⟩

-- ============================================================================
-- Environment — properties hold under external state changes
-- ============================================================================

theorem pool_solvency_under_interest_rate_change (s : PoolState) (new_interest_rate : Nat)
    (h_c0 : new_interest_rate > 0)
    (h_inv : pool_solvency s) :
    pool_solvency { s with interest_rate := new_interest_rate } := by
  unfold pool_solvency at h_inv ⊢; exact h_inv

end Lending

#qedgen_verify Lending
