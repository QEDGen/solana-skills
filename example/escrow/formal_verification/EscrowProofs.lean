import Mathlib.Tactic
import Leanstral.Solana.Account
import Leanstral.Solana.Authority
import Leanstral.Solana.Cpi
import Leanstral.Solana.State
import Leanstral.Solana.Token
import Leanstral.Solana.Valid

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
  initializer : Pubkey
  escrow : Pubkey
  initializer_deposit : Pubkey
  escrow_token_account : Pubkey
  authority : Pubkey
  amount : U64

def cancel_build_transfer_cpi (p_ctx : CancelContext) : TransferCpi :=
  { program := TOKEN_PROGRAM_ID
  , «from» := p_ctx.escrow_token_account
  , «to» := p_ctx.initializer_deposit
  , authority := p_ctx.authority
  , amount := p_ctx.amount }

theorem cancel_cpi_valid (p_ctx : CancelContext)
    (h_distinct : p_ctx.escrow_token_account ≠ p_ctx.initializer_deposit)
    (h_amount : p_ctx.amount ≤ U64_MAX) :
    let cpi := cancel_build_transfer_cpi p_ctx
    transferCpiValid cpi ∧
    cpi.authority = p_ctx.authority ∧
    cpi.«from» ≠ cpi.«to» := by
  unfold cancel_build_transfer_cpi transferCpiValid
  exact ⟨⟨rfl, h_distinct, h_amount⟩, rfl, h_distinct⟩

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

def cancelTransition (p_preState : EscrowState) : Option EscrowState :=
  some { p_preState with lifecycle := Lifecycle.closed }

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
  taker : Pubkey
  escrow : Pubkey
  taker_deposit : Pubkey
  initializer_receive : Pubkey
  escrow_token_account : Pubkey
  taker_receive : Pubkey
  taker_amount : U64
  initializer_amount : U64

def exchange_build_transfer_cpis (p_ctx : ExchangeContext) : List TransferCpi :=
  [ { program := TOKEN_PROGRAM_ID
    , «from» := p_ctx.taker_deposit
    , «to» := p_ctx.initializer_receive
    , authority := p_ctx.taker
    , amount := p_ctx.taker_amount }
  , { program := TOKEN_PROGRAM_ID
    , «from» := p_ctx.escrow_token_account
    , «to» := p_ctx.taker_receive
    , authority := p_ctx.escrow
    , amount := p_ctx.initializer_amount } ]

theorem exchange_cpis_valid (p_ctx : ExchangeContext)
    (h_distinct1 : p_ctx.taker_deposit ≠ p_ctx.initializer_receive)
    (h_distinct2 : p_ctx.escrow_token_account ≠ p_ctx.taker_receive)
    (h_amount1 : p_ctx.taker_amount ≤ U64_MAX)
    (h_amount2 : p_ctx.initializer_amount ≤ U64_MAX) :
    let cpis := exchange_build_transfer_cpis p_ctx
    multipleTransfersValid cpis ∧
    (∀ cpi ∈ cpis, cpi.program = TOKEN_PROGRAM_ID) := by
  unfold exchange_build_transfer_cpis
  unfold multipleTransfersValid
  simp only [Leanstral.Solana.transferCpiValid, Leanstral.Solana.Cpi.transferCpiValid]
  constructor
  · constructor
    · intro cpi h
      simp [List.mem_cons, List.mem_singleton] at h
      rcases h with rfl | rfl
      · exact ⟨rfl, h_distinct1, h_amount1⟩
      · exact ⟨rfl, h_distinct2, h_amount2⟩
    · intro cpi h
      simp [List.mem_cons, List.mem_singleton] at h
      rcases h with rfl | rfl
      · exact h_distinct1
      · exact h_distinct2
  · intro cpi h
    simp [List.mem_cons, List.mem_singleton] at h
    rcases h with rfl | rfl <;> rfl

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
  initializer : Pubkey
  initializer_deposit_token_account : Pubkey
  escrow_token_account : Pubkey
  amount : U64

def initialize_build_transfer_cpi (p_ctx : InitializeContext) : TransferCpi :=
  { program := TOKEN_PROGRAM_ID
  , «from» := p_ctx.initializer_deposit_token_account
  , «to» := p_ctx.escrow_token_account
  , authority := p_ctx.initializer
  , amount := p_ctx.amount }

theorem initialize_cpi_valid (p_ctx : InitializeContext)
    (h_distinct : p_ctx.initializer_deposit_token_account ≠ p_ctx.escrow_token_account)
    (h_amount : p_ctx.amount ≤ U64_MAX) :
    let cpi := initialize_build_transfer_cpi p_ctx
    transferCpiValid cpi ∧
    cpi.authority = p_ctx.initializer ∧
    cpi.«from» ≠ cpi.«to» := by
  unfold initialize_build_transfer_cpi transferCpiValid
  exact ⟨⟨rfl, h_distinct, h_amount⟩, rfl, h_distinct⟩

end InitializeCpiCorrectness

/- ============================================================================
   ProgramArithmeticSafety Proof
   ============================================================================ -/

namespace ProgramArithmeticSafety

def U64_MAX : Nat := 18446744073709551615

structure ProgramState where
  amount : Nat
  taker_amount : Nat
  bump : Nat

def ValidState (s : ProgramState) : Prop :=
  s.amount <= U64_MAX ∧
  s.taker_amount <= U64_MAX ∧
  s.bump <= 255

def cancelTransition (p_s : ProgramState) : Option ProgramState :=
  some { p_s with amount := 0 }

theorem cancel_arithmetic_safety  (p_preState p_postState : ProgramState)
    (h : cancelTransition p_preState  = some p_postState) :
    p_postState.amount <= U64_MAX := by
  unfold cancelTransition at h
  cases h
  simp

end ProgramArithmeticSafety

