import Leanstral.Solana
import Leanstral.Solana.Account
import Leanstral.Solana.Authority
import Leanstral.Solana.Cpi
import Leanstral.Solana.State
import Mathlib
import Mathlib.Tactic

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
  unfold cancelTransition at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end CancelAccessControl

/- ============================================================================
   CancelCpiCorrectness Proof
   ============================================================================ -/

namespace CancelCpiCorrectness

structure CancelContext where
  escrow_token_account : Pubkey
  initializer_deposit : Pubkey
  authority : Pubkey
  amount : U64

def cancel_build_cpi (p_ctx : CancelContext) : TransferCpi :=
  { program := TOKEN_PROGRAM_ID
  , «from» := p_ctx.escrow_token_account
  , «to» := p_ctx.initializer_deposit
  , authority := p_ctx.authority
  , amount := p_ctx.amount }

theorem cancel_cpi_correct (p_ctx : CancelContext) :
    let cpi := cancel_build_cpi p_ctx
    cpi.program = TOKEN_PROGRAM_ID ∧
    cpi.«from» = p_ctx.escrow_token_account ∧
    cpi.«to» = p_ctx.initializer_deposit ∧
    cpi.authority = p_ctx.authority ∧
    cpi.amount = p_ctx.amount := by
  unfold cancel_build_cpi
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

end CancelCpiCorrectness

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

def cancelTransition (p_s : EscrowState) : Option EscrowState :=
  some { p_s with lifecycle := Lifecycle.closed }

theorem cancel_closes_escrow (p_preState p_postState : EscrowState)
    (h : cancelTransition p_preState = some p_postState) :
    p_postState.lifecycle = Lifecycle.closed := by
  unfold cancelTransition at h
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
  unfold exchangeTransition at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end ExchangeAccessControl

/- ============================================================================
   ExchangeCpiCorrectness Proof
   ============================================================================ -/

namespace ExchangeCpiCorrectness

structure ExchangeContext where
  from_account : Pubkey
  to_account : Pubkey
  authority : Pubkey
  amount : U64

def exchange_build_cpi (ctx : ExchangeContext) : TransferCpi :=
  { program := TOKEN_PROGRAM_ID
  , «from» := ctx.from_account
  , «to» := ctx.to_account
  , authority := ctx.authority
  , amount := ctx.amount }

theorem exchange_cpi_correct (ctx : ExchangeContext) :
    let cpi := exchange_build_cpi ctx
    cpi.program = TOKEN_PROGRAM_ID ∧
    cpi.«from» = ctx.from_account ∧
    cpi.«to» = ctx.to_account ∧
    cpi.authority = ctx.authority ∧
    cpi.amount = ctx.amount := by
  unfold exchange_build_cpi
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

end ExchangeCpiCorrectness

/- ============================================================================
   ExchangeStateMachine Proof
   ============================================================================ -/

namespace ExchangeStateMachine

structure EscrowState where
  lifecycle : Lifecycle
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

def exchangeTransition (p_preState : EscrowState) : Option EscrowState :=
  some { p_preState with lifecycle := Lifecycle.closed }

theorem exchange_closes_escrow (p_preState p_postState : EscrowState)
    (h : exchangeTransition p_preState = some p_postState) :
    p_postState.lifecycle = Lifecycle.closed := by
  unfold exchangeTransition at h
  cases h
  rfl

end ExchangeStateMachine

/- ============================================================================
   InitializeAccessControl Proof
   ============================================================================ -/

namespace InitializeAccessControl

structure EscrowState where
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

def initializeTransition (p_preState : EscrowState) (p_signer : Pubkey) : Option Unit :=
  if p_signer = p_preState.initializer then
    some ()
  else
    none

theorem initialize_access_control (p_preState : EscrowState) (p_signer : Pubkey)
    (h : initializeTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.initializer := by
  unfold initializeTransition at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end InitializeAccessControl

/- ============================================================================
   InitializeCpiCorrectness Proof
   ============================================================================ -/

namespace InitializeCpiCorrectness

structure InitializeContext where
  from_account : Pubkey
  to_account : Pubkey
  authority : Pubkey
  amount : U64

def initialize_build_cpi (ctx : InitializeContext) : TransferCpi :=
  { program := TOKEN_PROGRAM_ID
  , «from» := ctx.from_account
  , «to» := ctx.to_account
  , authority := ctx.authority
  , amount := ctx.amount }

theorem initialize_cpi_correct (ctx : InitializeContext) :
    let cpi := initialize_build_cpi ctx
    cpi.program = TOKEN_PROGRAM_ID ∧
    cpi.«from» = ctx.from_account ∧
    cpi.«to» = ctx.to_account ∧
    cpi.authority = ctx.authority ∧
    cpi.amount = ctx.amount := by
  unfold initialize_build_cpi
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

end InitializeCpiCorrectness

