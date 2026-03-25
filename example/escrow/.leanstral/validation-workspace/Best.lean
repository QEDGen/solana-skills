import Mathlib
import QEDGen.Solana.Account
import QEDGen.Solana.State

open QEDGen.Solana

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
  unfold cancelTransition at h
  cases h
  rfl
