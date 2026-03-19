import Mathlib
import Aesop

-- Constants
def U64_MAX : Nat := 2^64 - 1

-- Account state model
structure Account where
  balance : Nat
  authority : Nat
  is_closed : Bool

-- Escrow state model
structure Escrow where
  initializer : Nat
  initializer_token_account : Nat
  initializer_amount : Nat
  taker_amount : Nat
  escrow_token_account : Nat
  bump : Nat
  is_closed : Bool

-- Program state model
structure ProgramState where
  accounts : List Account
  escrow : Escrow
  is_initialized : Bool

-- Helper functions
def get_account (p_s : ProgramState) (p_key : Nat) : Option Account :=
  p_s.accounts.find? (fun acc => acc.authority = p_key)

def get_escrow (p_s : ProgramState) : Option Escrow :=
  if p_s.is_initialized then some p_s.escrow else none

def transfer_tokens (p_from p_to : Nat) (p_amount : Nat) : Option (Nat × Nat) :=
  if h : p_amount <= U64_MAX then
    some (p_from - p_amount, p_to + p_amount)
  else
    none

-- Instruction models
def initialize (p_s : ProgramState) (p_initializer p_escrow_token_account : Nat)
    (p_amount p_taker_amount : Nat) : Option ProgramState :=
  if h1 : p_amount <= U64_MAX ∧ p_taker_amount <= U64_MAX then
    let new_escrow : Escrow := {
      initializer := p_initializer,
      initializer_token_account := p_initializer,
      initializer_amount := p_amount,
      taker_amount := p_taker_amount,
      escrow_token_account := p_escrow_token_account,
      bump := 0, -- Simplified for model
      is_closed := false
    }
    some {
      p_s with
      escrow := new_escrow,
      is_initialized := true
    }
  else
    none

def exchange (p_s : ProgramState) (p_taker : Nat) : Option ProgramState :=
  match get_escrow p_s with
  | some escrow =>
    if escrow.is_closed then none else
    match get_account p_s escrow.initializer with
    | some init_acc =>
      match get_account p_s p_taker with
      | some taker_acc =>
        match transfer_tokens escrow.initializer_token_account p_taker escrow.initializer_amount with
        | some (new_init, new_escrow) =>
          match transfer_tokens p_taker escrow.escrow_token_account escrow.taker_amount with
          | some (new_taker, new_escrow2) =>
            some {
              p_s with
              accounts := p_s.accounts.map (fun acc =>
                if acc.authority = escrow.initializer then
                  { acc with balance := new_init }
                else if acc.authority = p_taker then
                  { acc with balance := new_taker }
                else acc),
              escrow := { escrow with is_closed := true }
            }
          | none => none
        | none => none
      | none => none
    | none => none
  | none => none

def cancel (p_s : ProgramState) (p_signer : Nat) : Option ProgramState :=
  match get_escrow p_s with
  | some escrow =>
    if escrow.is_closed then none else
    if escrow.initializer ≠ p_signer then none else
    match get_account p_s escrow.initializer with
    | some init_acc =>
      match transfer_tokens escrow.escrow_token_account escrow.initializer_token_account escrow.initializer_amount with
      | some (new_init, new_escrow) =>
        some {
          p_s with
          accounts := p_s.accounts.map (fun acc =>
            if acc.authority = escrow.initializer then
              { acc with balance := new_init }
            else acc),
          escrow := { escrow with is_closed := true }
        }
      | none => none
    | none => none
  | none => none

-- Token conservation theorem
theorem token_conservation (p_s : ProgramState) (p_s' : ProgramState)
    (h : ∃ s, initialize p_s p_s.initializer p_s.escrow.escrow_token_account p_s.escrow.initializer_amount p_s.escrow.taker_amount = some s) :
    let total_before := p_s.accounts.foldl (fun acc a => acc + a.balance) 0
    let total_after := p_s'.accounts.foldl (fun acc a => acc + a.balance) 0
    total_before = total_after := by
  rcases h with ⟨s, hs⟩
  simp [initialize] at hs
  aesop

-- Access control theorem
theorem cancel_access_control (p_s : ProgramState) (p_signer : Nat)
    (h : cancel p_s p_signer = some p_s') :
    p_signer = p_s.escrow.initializer := by
  simp [cancel] at h
  aesop

-- Exchange correctness theorem
theorem exchange_correctness (p_s : ProgramState) (p_taker : Nat)
    (h : exchange p_s p_taker = some p_s') :
    let init_acc := p_s.accounts.find? (fun a => a.authority = p_s.escrow.initializer)
    let taker_acc := p_s.accounts.find? (fun a => a.authority = p_taker)
    let escrow_acc := p_s.accounts.find? (fun a => a.authority = p_s.escrow.escrow_token_account)
    let init_acc' := p_s'.accounts.find? (fun a => a.authority = p_s.escrow.initializer)
    let taker_acc' := p_s'.accounts.find? (fun a => a.authority = p_taker)
    let escrow_acc' := p_s'.accounts.find? (fun a => a.authority = p_s.escrow.escrow_token_account)
    init_acc'.bind (fun a => some a.balance) = init_acc.bind (fun a => some (a.balance + p_s.escrow.taker_amount)) ∧
    taker_acc'.bind (fun a => some a.balance) = taker_acc.bind (fun a => some (a.balance + p_s.escrow.initializer_amount)) ∧
    escrow_acc'.bind (fun a => some a.balance) = some 0 := by
  simp [exchange] at h
  aesop

-- State machine safety theorem
theorem state_machine_safety (p_s : ProgramState) (p_s' : ProgramState)
    (h : exchange p_s p_taker = some p_s' ∨ cancel p_s p_signer = some p_s') :
    p_s'.escrow.is_closed = true := by
  rcases h with (hex | hcan)
  · simp [exchange] at hex
    aesop
  · simp [cancel] at hcan
    aesop

-- Arithmetic safety theorem
theorem arithmetic_safety (p_s : ProgramState) (p_amount p_taker_amount : Nat)
    (h1 : p_amount <= U64_MAX) (h2 : p_taker_amount <= U64_MAX) :
    ∃ s, initialize p_s p_s.initializer p_s.escrow.escrow_token_account p_amount p_taker_amount = some s := by
  refine ⟨{
    p_s with
    escrow := {
      p_s.escrow with
      initializer_amount := p_amount,
      taker_amount := p_taker_amount
    },
    is_initialized := true
  }, ?_⟩
  simp [initialize, h1, h2]
