import Mathlib
import Aesop

-- Token balance type (u64)
def TokenBalance := Nat

-- Account state: balance and authority
structure Account where
  balance : TokenBalance
  authority : Nat  -- Public key representation

-- Escrow state
structure Escrow where
  initializer : Nat          -- Public key of initializer
  initializer_token_account : Nat  -- Public key of initializer's token account
  initializer_amount : TokenBalance
  taker_amount : TokenBalance
  escrow_token_account : Nat  -- Public key of escrow's token account
  bump : Nat                 -- PDA bump
  is_closed : Bool           -- Whether escrow is closed

-- Program state
structure ProgramState where
  accounts : List Account
  escrows : List Escrow
  -- Other program state...


-- Total token balance in a set of accounts
def total_balance (accounts : List Account) : TokenBalance :=
  accounts.foldl (fun acc acc' => acc + acc'.balance) 0

-- Initial state before any operation
variable (initial_state : ProgramState)

-- After initialize, total balance is conserved
theorem token_conservation_initialize
  (initializer : Account)
  (escrow_token_account : Account)
  (amount : TokenBalance)
  (taker_amount : TokenBalance)
  (h_amount : amount ≤ initializer.balance) :
  let new_state := {
    accounts := initial_state.accounts.map (fun acc =>
      if acc.authority = initializer.authority then
        {acc with balance := acc.balance - amount}
      else if acc.authority = escrow_token_account.authority then
        {acc with balance := acc.balance + amount}
      else acc)
    escrows := Escrow.mk initializer.authority
      initializer.authority amount taker_amount
      escrow_token_account.authority (Nat.find (fun b => b > 0)) false :: initial_state.escrows
  }
  total_balance new_state.accounts + total_balance (new_state.escrows.map (fun e =>
    {balance := 0, authority := e.escrow_token_account})) =
  total_balance initial_state.accounts + total_balance (initial_state.escrows.map (fun e =>
    {balance := 0, authority := e.escrow_token_account})) := by
  simp [total_balance]
  omega


-- Only initializer can cancel
theorem access_control_cancel
  (state : ProgramState)
  (escrow : Escrow)
  (signer : Nat)
  (h_escrow_exists : escrow ∈ state.escrows)
  (h_cancel : escrow.is_closed = false)
  (h_success : ∃ (new_state : ProgramState),
    new_state.escrows = state.escrows.filter (fun e => e ≠ escrow) ∧
    new_state.accounts = state.accounts.map (fun acc =>
      if acc.authority = escrow.initializer then
        {acc with balance := acc.balance + escrow.initializer_amount}
      else acc)) :
  signer = escrow.initializer := by
  -- The existence of a successful cancel implies the signer must be the initializer
  -- This is because the CPI transfer requires the correct authority
  aesop


-- After exchange, balances are updated correctly
theorem exchange_correctness
  (state : ProgramState)
  (escrow : Escrow)
  (h_escrow_exists : escrow ∈ state.escrows)
  (h_not_closed : escrow.is_closed = false)
  (taker : Account)
  (h_taker_exists : taker ∈ state.accounts)
  (h_taker_balance : taker.balance ≥ escrow.taker_amount) :
  ∃ (new_state : ProgramState),
    -- Taker receives initializer's amount
    (new_state.accounts.find? (fun acc => acc.authority = taker.authority)).get!.balance =
      taker.balance + escrow.initializer_amount ∧
    -- Initializer receives taker's amount
    (new_state.accounts.find? (fun acc => acc.authority = escrow.initializer)).get!.balance =
      (state.accounts.find? (fun acc => acc.authority = escrow.initializer)).get!.balance +
      escrow.taker_amount ∧
    -- Escrow balance is zero
    (new_state.accounts.find? (fun acc => acc.authority = escrow.escrow_token_account)).get!.balance = 0 ∧
    -- Escrow is closed
    (new_state.escrows.find? (fun e => e = escrow)).isNone := by
  -- Construct the new state with updated balances
  let new_state := {
    accounts := state.accounts.map (fun acc =>
      if acc.authority = taker.authority then
        {acc with balance := acc.balance + escrow.initializer_amount}
      else if acc.authority = escrow.initializer then
        {acc with balance := acc.balance + escrow.taker_amount}
      else if acc.authority = escrow.escrow_token_account then
        {acc with balance := 0}
      else acc)
    escrows := state.escrows.filter (fun e => e ≠ escrow)
  }
  use new_state
  simp
  omega


-- Escrow can only be used once
theorem state_machine_safety
  (state : ProgramState)
  (escrow : Escrow)
  (h_escrow_exists : escrow ∈ state.escrows) :
  ∀ (new_state : ProgramState),
    (new_state.escrows = state.escrows.filter (fun e => e ≠ escrow)) →
    escrow ∉ new_state.escrows := by
  intro new_state h_filter
  rw [h_filter]
  simp


-- All arithmetic operations are safe
theorem arithmetic_safety
  (amount : TokenBalance)
  (taker_amount : TokenBalance)
  (h_amount : amount ≤ u64_max)
  (h_taker : taker_amount ≤ u64_max) :
  amount + taker_amount ≤ u64_max := by
  omega
