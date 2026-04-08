import QEDGen.Solana
import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import Mathlib.Tactic
open QEDGen.Solana

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

structure ProgramState where
  escrow : EscrowState
  initializer : Pubkey

def cancelTransition (p_preState : ProgramState) (p_signer : Pubkey) : Option Unit :=
  if h : p_signer = p_preState.escrow.initializer then
    some ()
  else
    none

theorem cancel_access_control (p_preState : ProgramState) (p_signer : Pubkey)
    (h : cancelTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.escrow.initializer := by
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

def cancel_build_cpi (p_ctx : CancelContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [
      ⟨p_ctx.escrow_token_account, false, true⟩,   -- source: writable
      ⟨p_ctx.initializer_deposit, false, true⟩,     -- dest: writable
      ⟨p_ctx.authority, true, false⟩                  -- authority: signer
    ]
  , data := [DISC_TRANSFER]
  }

theorem cancel_cpi_correct (p_ctx : CancelContext) :
    let cpi := cancel_build_cpi p_ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 p_ctx.escrow_token_account false true ∧
    accountAt cpi 1 p_ctx.initializer_deposit false true ∧
    accountAt cpi 2 p_ctx.authority true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold cancel_build_cpi targetsProgram accountAt hasDiscriminator
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

structure ProgramState where
  escrow : EscrowState

def cancelTransition (p_s : ProgramState) : Option ProgramState :=
  if p_s.escrow.lifecycle = Lifecycle.open then
    some {
      escrow := {
        p_s.escrow with
        lifecycle := Lifecycle.closed
      }
    }
  else
    none

theorem cancel_closes_escrow (p_preState p_postState : ProgramState)
    (h : cancelTransition p_preState = some p_postState) :
    p_postState.escrow.lifecycle = Lifecycle.closed := by
  unfold cancelTransition at h
  split_ifs at h with h_open
  cases h
  rfl

end CancelStateMachine

/- ============================================================================
   ExchangeAccessControl Proof
   ============================================================================ -/

namespace ExchangeAccessControl

structure ProgramState where
  initializer : Pubkey
  taker : Pubkey
  initializer_amount : Nat
  taker_amount : Nat
  is_active : Bool

def exchangeTransition (p_preState : ProgramState) (p_signer : Pubkey) : Option Unit :=
  if h : p_signer = p_preState.taker then
    some ()
  else
    none

theorem exchange_access_control (p_preState : ProgramState) (p_signer : Pubkey)
    (h : exchangeTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.taker := by
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

def exchange_build_cpi (ctx : ExchangeContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [
      ⟨ctx.from_account, false, true⟩,   -- source: writable
      ⟨ctx.to_account, false, true⟩,      -- dest: writable
      ⟨ctx.authority, true, false⟩         -- authority: signer
    ]
  , data := [DISC_TRANSFER]
  }

theorem exchange_cpi_correct (ctx : ExchangeContext) :
    let cpi := exchange_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.from_account false true ∧
    accountAt cpi 1 ctx.to_account false true ∧
    accountAt cpi 2 ctx.authority true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold exchange_build_cpi targetsProgram accountAt hasDiscriminator
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

structure ProgramState where
  escrow : EscrowState

def exchangeTransition (p_s : ProgramState) : Option ProgramState :=
  if p_s.escrow.lifecycle = Lifecycle.open then
    some {
      escrow := {
        p_s.escrow with
        lifecycle := Lifecycle.closed
      }
    }
  else
    none

theorem exchange_closes_escrow (p_preState p_postState : ProgramState)
    (h : exchangeTransition p_preState = some p_postState) :
    p_postState.escrow.lifecycle = Lifecycle.closed := by
  unfold exchangeTransition at h
  split_ifs at h with h_open
  cases h
  rfl

end ExchangeStateMachine

/- ============================================================================
   InitializeAccessControl Proof
   ============================================================================ -/

namespace InitializeAccessControl

structure ProgramState where
  initializer : Pubkey
  initializer_token_account : Pubkey
  initializer_amount : U64
  taker_amount : U64
  escrow_token_account : Pubkey
  bump : U8

def initializeTransition (p_preState : ProgramState) (p_signer : Pubkey) : Option Unit :=
  if p_signer = p_preState.initializer then
    some ()
  else
    none

theorem initialize_access_control (p_preState : ProgramState) (p_signer : Pubkey)
    (h : initializeTransition p_preState p_signer ≠ none) :
    p_signer = p_preState.initializer := by
  unfold initializeTransition at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction

end InitializeAccessControl

/- ============================================================================
   InitializeArithmeticSafety Proof
   ============================================================================ -/

namespace InitializeArithmeticSafety

def U64_MAX : Nat := 2^64 - 1

structure ProgramState where
  initializer_amount : Nat
  taker_amount : Nat

def initializeTransition (p_amount p_taker_amount : Nat) : Option ProgramState :=
  if p_amount > 0 ∧ p_amount ≤ U64_MAX ∧ p_taker_amount > 0 ∧ p_taker_amount ≤ U64_MAX then
    some { initializer_amount := p_amount, taker_amount := p_taker_amount }
  else
    none

theorem initialize_arithmetic_safety (p_amount p_taker_amount : Nat) (p_postState : ProgramState)
    (h : initializeTransition p_amount p_taker_amount = some p_postState) :
    p_postState.initializer_amount ≤ U64_MAX ∧ p_postState.taker_amount ≤ U64_MAX := by
  unfold initializeTransition at h
  split_ifs at h with h_bounds
  cases h
  exact ⟨h_bounds.2.1, h_bounds.2.2.2⟩

end InitializeArithmeticSafety

/- ============================================================================
   InitializeCpiCorrectness Proof
   ============================================================================ -/

namespace InitializeCpiCorrectness

structure InitializeContext where
  from_account : Pubkey
  to_account : Pubkey
  authority : Pubkey
  amount : U64

def initialize_build_cpi (ctx : InitializeContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [
      ⟨ctx.from_account, false, true⟩,   -- source: writable
      ⟨ctx.to_account, false, true⟩,      -- dest: writable
      ⟨ctx.authority, true, false⟩         -- authority: signer
    ]
  , data := [DISC_TRANSFER]
  }

theorem initialize_cpi_correct (ctx : InitializeContext) :
    let cpi := initialize_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.from_account false true ∧
    accountAt cpi 1 ctx.to_account false true ∧
    accountAt cpi 2 ctx.authority true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold initialize_build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

end InitializeCpiCorrectness

