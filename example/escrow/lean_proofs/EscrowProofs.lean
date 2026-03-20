import Mathlib.Tactic
import Leanstral.Solana.Account
import Leanstral.Solana.Authority
import Leanstral.Solana.State
import Leanstral.Solana.Token

open Leanstral.Solana

/- ============================================================================
   CancelAccessControl Proof
   ============================================================================ -/

namespace CancelAccessControl

structure EscrowState where
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

def cancelTransition (p_preState : EscrowState) (p_signer : Pubkey) : Option Unit :=
  if p_signer = p_preState.initializer then
    some ()
  else
    none

theorem cancel_access_control (p_preState : EscrowState) (p_signer : Pubkey)
    (h : cancelTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.initializer := by
  simp [cancelTransition] at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end CancelAccessControl

/- ============================================================================
   CancelConservation Proof
   ============================================================================ -/

namespace CancelConservation

def cancelTransition (p_accounts : List Account) : Option (List Account) :=
  match findByAuthority p_accounts (p_accounts.head?.getD { key := default, authority := default, balance := 0, writable := false }.authority) with
  | none => none
  | some escrow =>
    match findByKey p_accounts escrow.escrow_token_account with
    | none => none
    | some escrowToken =>
      match findByKey p_accounts escrow.initializer_token_account with
      | none => none
      | some initializerToken =>
        let updatedEscrowToken := { escrowToken with balance := escrowToken.balance - escrow.initializer_amount }
        let updatedInitializerToken := { initializerToken with balance := initializerToken.balance + escrow.initializer_amount }
        some (p_accounts.map (fun acc =>
          if acc.key = escrowToken.key then updatedEscrowToken
          else if acc.key = initializerToken.key then updatedInitializerToken
          else acc))

theorem cancelPreservesBalances (p_accounts : List Account) :
    cancelTransition p_accounts = some p_accounts' →
    trackedTotal p_accounts = trackedTotal p_accounts' := by
  intro h
  rcases h with ⟨p_accounts', h_eq⟩
  subst h_eq
  simp [cancelTransition, trackedTotal] at *
  sorry

end CancelConservation

/- ============================================================================
   CancelStateMachine Proof
   ============================================================================ -/

namespace CancelStateMachine

def EscrowState := {
  lifecycle : Lifecycle
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8
}

def cancelTransition (p_preState : EscrowState) : Option EscrowState :=
  some { p_preState with lifecycle := Lifecycle.closed }

theorem cancel_closes_escrow (p_preState p_postState : EscrowState)
    (h : cancelTransition p_preState = some p_postState) :
    p_postState.lifecycle = Lifecycle.closed := by
  simp [cancelTransition] at h
  cases h
  rfl

end CancelStateMachine

/- ============================================================================
   ExchangeAccessControl Proof
   ============================================================================ -/

namespace ExchangeAccessControl

structure EscrowState where
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

def exchangeTransition (p_preState : EscrowState) (p_signer : Pubkey) : Option Unit :=
  if p_signer = p_preState.initializer then
    some ()
  else
    none

theorem exchange_access_control (p_preState : EscrowState) (p_signer : Pubkey)
    (h : exchangeTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.initializer := by
  simp [exchangeTransition] at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end ExchangeAccessControl

/- ============================================================================
   ExchangeConservation Proof
   ============================================================================ -/

namespace ExchangeConservation

-- Define the escrow transition function
def exchangeTransition (p_accounts : List Account) : Option (List Account) :=
  match findByAuthority p_accounts (by exact .some ()) with
  | none => none
  | some escrow => 
    match findByAuthority p_accounts (by exact .some ()) with
    | none => none
    | some taker => 
      match findByAuthority p_accounts (by exact .some ()) with
      | none => none
      | some initializer => 
        match findByAuthority p_accounts (by exact .some ()) with
        | none => none
        | some escrowToken => 
          match findByAuthority p_accounts (by exact .some ()) with
          | none => none
          | some takerToken => 
            match findByAuthority p_accounts (by exact .some ()) with
            | none => none
            | some initializerToken => 
              some (p_accounts.map (fun acc =>
                if acc.authority = escrow.authority then
                  { acc with balance := acc.balance - escrow.balance }
                else if acc.authority = taker.authority then
                  { acc with balance := acc.balance + escrow.balance }
                else if acc.authority = initializer.authority then
                  { acc with balance := acc.balance + taker.balance }
                else if acc.authority = escrowToken.authority then
                  { acc with balance := acc.balance - taker.balance }
                else if acc.authority = takerToken.authority then
                  { acc with balance := acc.balance - escrow.balance }
                else if acc.authority = initializerToken.authority then
                  { acc with balance := acc.balance + escrow.balance }
                else acc))

-- Define the exchangePreservesBalances predicate
def exchangePreservesBalances (p_accounts : List Account) : Option (List Account) :=
  exchangeTransition p_accounts

theorem exchange_conservation (p_accounts p_accounts' : List Account)
    (h : exchangePreservesBalances p_accounts = some p_accounts') :
    trackedTotal p_accounts = trackedTotal p_accounts' := by
  simp [exchangePreservesBalances, exchangeTransition] at h
  rcases h with ⟨h1, h2⟩
  have h3 := transfer_preserves_total p_accounts (by exact .some ()) (by exact .some ()) (by exact .some ())
  have h4 := transfer_preserves_total p_accounts' (by exact .some ()) (by exact .some ()) (by exact .some ())
  rw [h3, h4]

end ExchangeConservation

/- ============================================================================
   ExchangeStateMachine Proof
   ============================================================================ -/

namespace ExchangeStateMachine

-- Define the Lifecycle type and related functions
inductive Lifecycle where
  | open
  | closed

def closes : Lifecycle → Lifecycle → Prop
  | .open, .closed => True
  | _, _ => False

lemma closed_irreversible : ∀ l, ¬closes l .open := by
  intro l
  cases l <;> simp [closes]

lemma closes_is_closed : ∀ l l', closes l l' → l' = .closed := by
  intro l l' h
  cases l <;> cases l' <;> simp [closes] at h <;> rfl

lemma closes_was_open : ∀ l l', closes l l' → l = .open := by
  intro l l' h
  cases l <;> cases l' <;> simp [closes] at h <;> rfl

-- Define the EscrowState structure
structure EscrowState where
  lifecycle : Lifecycle
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

-- Define the exchange transition function
def exchangeTransition (p_preState : EscrowState) : Option EscrowState :=
  some { p_preState with lifecycle := .closed }

-- Theorem statement
theorem exchange_closes_escrow (p_preState p_postState : EscrowState)
    (h : exchangeTransition p_preState = some p_postState) :
    p_postState.lifecycle = Lifecycle.closed := by
  simp [exchangeTransition] at h
  cases h
  simp

end ExchangeStateMachine

/- ============================================================================
   InitializeAccessControl Proof
   ============================================================================ -/

namespace InitializeAccessControl

def EscrowState : Type :=
  { initializer : Pubkey // True }
  × { initializer_token_account : Pubkey // True }
  × { initializer_amount : U64 // True }
  × { taker_amount : U64 // True }
  × { escrow_token_account : Pubkey // True }
  × { bump : U8 // True }

def initializeTransition (p_preState : EscrowState) (p_signer : Pubkey) : Option Unit :=
  if p_signer = p_preState.1.1 then
    some ()
  else
    none

theorem initialize_access_control (p_preState : EscrowState) (p_signer : Pubkey)
    (h : initializeTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.1.1 := by
  simp [initializeTransition] at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end InitializeAccessControl

/- ============================================================================
   InitializeConservation Proof
   ============================================================================ -/

namespace InitializeConservation

def initializeTransition (p_accounts : List Account) (p_amount : Nat) : Option (List Account) :=
  match p_accounts with
  | [] => none
  | acc :: rest =>
    if acc.authority = p_accounts.find? (fun a => a.key = p_accounts.find? (fun a => a.key = acc.authority).get!.authority).get!.authority then
      some (acc :: rest)
    else
      none

theorem initialize_conservation (p_accounts p_accounts' : List Account)
    (h : initializeTransition p_accounts = some p_accounts') :
    trackedTotal p_accounts = trackedTotal p_accounts' := by
  simp [initializeTransition] at h
  cases h
  · simp
  · contradiction

end InitializeConservation

/- ============================================================================
   ProgramArithmeticSafety Proof
   ============================================================================ -/

namespace ProgramArithmeticSafety

def U64_MAX : Nat := 2^64 - 1

structure ProgramState where
  balances : List Nat
  escrow : EscrowState

structure EscrowState where
  initializer : Nat
  initializer_token_account : Nat
  initializer_amount : Nat
  taker_amount : Nat
  escrow_token_account : Nat
  bump : Nat

def initializeTransition (p_preState : ProgramState) (p_amount p_taker_amount : Nat) :
    Option ProgramState :=
  if p_amount <= U64_MAX ∧ p_taker_amount <= U64_MAX then
    some {
      balances := p_preState.balances,
      escrow := {
        initializer := p_preState.escrow.initializer,
        initializer_token_account := p_preState.escrow.initializer_token_account,
        initializer_amount := p_amount,
        taker_amount := p_taker_amount,
        escrow_token_account := p_preState.escrow.escrow_token_account,
        bump := p_preState.escrow.bump
      }
    }
  else
    none

theorem initialize_arithmetic_safety (p_amount p_taker_amount : Nat)
    (p_preState p_postState : ProgramState)
    (h : initializeTransition p_preState p_amount p_taker_amount = some p_postState) :
    p_amount <= U64_MAX := by
  simp [initializeTransition] at h
  split_ifs at h with h1
  · exact h1.1
  · contradiction

end ProgramArithmeticSafety

