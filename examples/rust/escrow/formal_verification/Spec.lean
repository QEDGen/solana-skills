import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

namespace Escrow

open QEDGen.Solana

inductive Status where
  | Uninitialized
  | Open
  | Closed
  deriving Repr, DecidableEq, BEq

structure State where
  initializer : Pubkey
  taker : Pubkey
  initializer_amount : Nat
  taker_amount : Nat
  escrow_token_account : Pubkey
  status : Status
  deriving Repr, DecidableEq, BEq

def initializeTransition (s : State) (signer : Pubkey) (deposit_amount : Nat) (receive_amount : Nat) : Option State :=
  if signer = s.initializer ∧ s.status = .Uninitialized ∧ deposit_amount > 0 ∧ receive_amount > 0 then
    some { s with initializer_amount := deposit_amount, taker_amount := receive_amount, status := .Open }
  else none

def exchangeTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.taker ∧ s.status = .Open then
    some { s with status := .Closed }
  else none

def cancelTransition (s : State) (signer : Pubkey) : Option State :=
  if signer = s.initializer ∧ s.status = .Open then
    some { s with status := .Closed }
  else none

structure initializeCpiContext where
  initializer_deposit : Pubkey
  escrow_token : Pubkey
  initializer : Pubkey
  deriving Repr, DecidableEq, BEq

def initialize_build_cpi (ctx : initializeCpiContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [⟨ctx.initializer_deposit, false, true⟩,
      ⟨ctx.escrow_token, false, true⟩,
      ⟨ctx.initializer, true, false⟩]
  , data := DISC_TRANSFER }

/-- initialize CPI targets TOKEN_PROGRAM_ID with correct accounts and discriminator. -/
theorem «initialize».cpi_correct (ctx : initializeCpiContext) :
    let cpi := initialize_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.initializer_deposit false true ∧
    accountAt cpi 1 ctx.escrow_token false true ∧
    accountAt cpi 2 ctx.initializer true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold initialize_build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

structure exchangeCpiContext where
  taker_deposit : Pubkey
  initializer_receive : Pubkey
  taker : Pubkey
  deriving Repr, DecidableEq, BEq

def exchange_build_cpi (ctx : exchangeCpiContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [⟨ctx.taker_deposit, false, true⟩,
      ⟨ctx.initializer_receive, false, true⟩,
      ⟨ctx.taker, true, false⟩]
  , data := DISC_TRANSFER }

/-- exchange CPI targets TOKEN_PROGRAM_ID with correct accounts and discriminator. -/
theorem exchange.cpi_correct (ctx : exchangeCpiContext) :
    let cpi := exchange_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.taker_deposit false true ∧
    accountAt cpi 1 ctx.initializer_receive false true ∧
    accountAt cpi 2 ctx.taker true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold exchange_build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

structure cancelCpiContext where
  escrow_token : Pubkey
  initializer_deposit : Pubkey
  escrow_pda : Pubkey
  deriving Repr, DecidableEq, BEq

def cancel_build_cpi (ctx : cancelCpiContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [⟨ctx.escrow_token, false, true⟩,
      ⟨ctx.initializer_deposit, false, true⟩,
      ⟨ctx.escrow_pda, true, false⟩]
  , data := DISC_TRANSFER }

/-- cancel CPI targets TOKEN_PROGRAM_ID with correct accounts and discriminator. -/
theorem cancel.cpi_correct (ctx : cancelCpiContext) :
    let cpi := cancel_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.escrow_token false true ∧
    accountAt cpi 1 ctx.initializer_deposit false true ∧
    accountAt cpi 2 ctx.escrow_pda true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold cancel_build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

/-- Invariant: conservation. -/
theorem conservation : True := sorry

inductive Operation where
  | «initialize» (deposit_amount : Nat) (receive_amount : Nat)
  | exchange
  | cancel
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .«initialize» deposit_amount receive_amount => initializeTransition s signer deposit_amount receive_amount
  | .exchange => exchangeTransition s signer
  | .cancel => cancelTransition s signer

end Escrow
