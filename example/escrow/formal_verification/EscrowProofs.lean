import Mathlib.Tactic
import Leanstral.Solana.Account
import Leanstral.Solana.Authority
import Leanstral.Solana.State
import Leanstral.Solana.Token

open Leanstral.Solana

-- Canonical EscrowState definition
structure EscrowState where
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8
  lifecycle : Leanstral.Solana.Lifecycle

/- ============================================================================
   CancelAccessControl Proof
   ============================================================================ -/

namespace CancelAccessControl

def cancelTransition (p_preState : EscrowState) (p_signer : Pubkey) : Option Unit :=
  if h : p_signer = p_preState.initializer then
    some ()
  else
    none

theorem cancel_access_control (p_preState : EscrowState) (p_signer : Pubkey)
    (h : cancelTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.initializer := by
  simp [cancelTransition] at h
  exact h

end CancelAccessControl

/- ============================================================================
   CancelStateMachine Proof
   ============================================================================ -/

namespace CancelStateMachine

def cancelTransition (p_preState : EscrowState) : Option EscrowState :=
  some { p_preState with lifecycle := Leanstral.Solana.Lifecycle.closed }

theorem cancel_closes_escrow (p_preState p_postState : EscrowState)
    (h : cancelTransition p_preState = some p_postState) :
    p_postState.lifecycle = Leanstral.Solana.Lifecycle.closed := by
  simp [cancelTransition] at h
  cases h
  rfl

end CancelStateMachine

namespace CancelConservation

def cancelPreservesBalances (p_accounts : List Account) (p_escrow_authority p_initializer_authority : Pubkey) (p_amount : Nat) : Option (List Account) :=
  some (p_accounts.map (fun acc =>
    if acc.authority = p_escrow_authority then
      { acc with balance := acc.balance - p_amount }
    else if acc.authority = p_initializer_authority then
      { acc with balance := acc.balance + p_amount }
    else acc))

theorem cancel_conservation (p_accounts p_accounts' : List Account) (p_escrow_authority p_initializer_authority : Pubkey) (p_amount : Nat) (h_distinct : p_escrow_authority ≠ p_initializer_authority) (h : cancelPreservesBalances p_accounts p_escrow_authority p_initializer_authority p_amount = some p_accounts') : trackedTotal p_accounts = trackedTotal p_accounts' := by
  rw [cancelPreservesBalances] at h
  apply Option.some.inj at h
  rw [h]
  exact transfer_preserves_total p_accounts p_escrow_authority p_initializer_authority p_amount h_distinct

end CancelConservation

/- ============================================================================
   ExchangeConservation Proof
   ============================================================================ -/

namespace ExchangeConservation

def exchangeTransition (p_accounts : List Account) (p_taker_authority p_initializer_authority p_escrow_authority p_taker_receive_authority : Pubkey) (p_taker_amount p_initializer_amount : Nat) : Option (List Account) :=
  let accounts_after_taker_transfer := p_accounts.map (fun acc =>
    if acc.authority = p_taker_authority then
      { acc with balance := acc.balance - p_taker_amount }
    else if acc.authority = p_initializer_authority then
      { acc with balance := acc.balance + p_taker_amount }
    else acc)
  some (accounts_after_taker_transfer.map (fun acc =>
    if acc.authority = p_escrow_authority then
      { acc with balance := acc.balance - p_initializer_amount }
    else if acc.authority = p_taker_receive_authority then
      { acc with balance := acc.balance + p_initializer_amount }
    else acc))

theorem exchange_conservation (p_accounts p_accounts' : List Account) (p_taker_authority p_initializer_authority p_escrow_authority p_taker_receive_authority : Pubkey) (p_taker_amount p_initializer_amount : Nat) (h_distinct1 : p_taker_authority ≠ p_initializer_authority) (h_distinct2 : p_escrow_authority ≠ p_taker_receive_authority) (h_eq_some_accounts' : exchangeTransition p_accounts p_taker_authority p_initializer_authority p_escrow_authority p_taker_receive_authority p_taker_amount p_initializer_amount = some p_accounts') : trackedTotal p_accounts = trackedTotal p_accounts' := by
  rw [exchangeTransition] at h_eq_some_accounts'
  apply Option.some.inj at h_eq_some_accounts'
  rw [h_eq_some_accounts']

  let accounts_step1 := p_accounts.map (fun acc =>
    if acc.authority = p_taker_authority then
      { acc with balance := acc.balance - p_taker_amount }
    else if acc.authority = p_initializer_authority then
      { acc with balance := acc.balance + p_taker_amount }
    else acc)

  have h_total_step1 : trackedTotal p_accounts = trackedTotal accounts_step1 :=
    transfer_preserves_total p_accounts p_taker_authority p_initializer_authority p_taker_amount h_distinct1

  let accounts_step2 := accounts_step1.map (fun acc =>
    if acc.authority = p_escrow_authority then
      { acc with balance := acc.balance - p_initializer_amount }
    else if acc.authority = p_taker_receive_authority then
      { acc with balance := acc.balance + p_initializer_amount }
    else acc)

  have h_total_step2 : trackedTotal accounts_step1 = trackedTotal accounts_step2 :=
    transfer_preserves_total accounts_step1 p_escrow_authority p_taker_receive_authority p_initializer_amount h_distinct2

  rw [h_total_step1, h_total_step2]
  rfl

end ExchangeConservation
