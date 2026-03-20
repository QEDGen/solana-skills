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
  if h : p_signer = p_preState.initializer then
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
   CancelStateMachine Proof
   ============================================================================ -/

namespace CancelStateMachine

structure EscrowState where
  lifecycle : Lifecycle
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

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

structure EscrowState where
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : Nat
  taker_amount : Nat
  escrow_token_account : Pubkey
  bump : U8

def exchangeTransition (p_accounts : List Account) (p_taker_authority p_initializer_receive_authority p_escrow_authority p_taker_receive_authority : Pubkey) (p_taker_amount p_initializer_amount : Nat) : Option (List Account) :=
  some (p_accounts.map (fun acc =>
    if acc.authority = p_taker_authority then
      { acc with balance := acc.balance - p_taker_amount }
    else if acc.authority = p_initializer_receive_authority then
      { acc with balance := acc.balance + p_taker_amount }
    else if acc.authority = p_escrow_authority then
      { acc with balance := acc.balance - p_initializer_amount }
    else if acc.authority = p_taker_receive_authority then
      { acc with balance := acc.balance + p_initializer_amount }
    else
      acc))

theorem exchange_conservation (p_accounts p_accounts' : List Account) (p_taker_authority p_initializer_receive_authority p_escrow_authority p_taker_receive_authority : Pubkey) (p_taker_amount p_initializer_amount : Nat) (h_distinct1 : p_taker_authority ≠ p_initializer_receive_authority) (h_distinct2 : p_escrow_authority ≠ p_taker_receive_authority) (h_distinct3 : p_taker_authority ≠ p_escrow_authority) (h : exchangeTransition p_accounts p_taker_authority p_initializer_receive_authority p_escrow_authority p_taker_receive_authority p_taker_amount p_initializer_amount = some p_accounts') : trackedTotal p_accounts = trackedTotal p_accounts' := by
  rcases h with rfl
  have h1 := transfer_preserves_total p_accounts p_taker_authority p_initializer_receive_authority p_taker_amount h_distinct1
  have h2 := transfer_preserves_total (p_accounts.map (fun acc =>
    if acc.authority = p_taker_authority then
      { acc with balance := acc.balance - p_taker_amount }
    else if acc.authority = p_initializer_receive_authority then
      { acc with balance := acc.balance + p_taker_amount }
    else acc)) p_escrow_authority p_taker_receive_authority p_initializer_amount h_distinct2
  rw [← h1, ← h2]
  simp [trackedTotal_map_id]

end ExchangeConservation

/- ============================================================================
   ProgramArithmeticSafety Proof
   ============================================================================ -/

namespace ProgramArithmeticSafety

def U64_MAX : Nat := 2^64 - 1

structure EscrowState where
  initializer : Nat
  initializer_token_account : Nat
  initializer_amount : Nat
  taker_amount : Nat
  escrow_token_account : Nat
  bump : Nat

structure ProgramState where
  escrow : EscrowState
  counter : Nat

def cancelTransition (p_s : ProgramState) : Option ProgramState :=
  some { p_s with escrow := { p_s.escrow with bump := 0 } }

theorem cancel_arithmetic_safety (p_preState p_postState : ProgramState)
    (h : cancelTransition p_preState = some p_postState) :
    p_preState.escrow.initializer_amount <= U64_MAX := by
  simp [cancelTransition] at h
  cases h
  simp

end ProgramArithmeticSafety

