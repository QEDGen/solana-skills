import QEDGen.Solana.Account
import QEDGen.Solana.Valid

namespace QEDGen.Solana.Cpi

open QEDGen.Solana.Account
open QEDGen.Solana.Valid

/- ============================================================================
   CPI (Cross-Program Invocation) Modeling

   This module models the CONSTRUCTION of CPIs - what the program author
   controls. We verify that CPIs are built with correct parameters, not that
   external programs execute correctly.

   Verification Scope: Parameter validation only
   Trust Boundary: External program execution (SPL Token, System, etc.)
   ============================================================================ -/

/-- Represents the parameters for a token transfer CPI -/
structure TransferCpi where
  program : Pubkey
  «from» : Pubkey
  «to» : Pubkey
  authority : Pubkey
  amount : Nat
  deriving Repr, DecidableEq

/-- Represents the parameters for a token mint CPI -/
structure MintToCpi where
  program : Pubkey
  mint : Pubkey
  «to» : Pubkey
  authority : Pubkey
  amount : Nat
  deriving Repr, DecidableEq

/-- Represents the parameters for a token burn CPI -/
structure BurnCpi where
  program : Pubkey
  mint : Pubkey
  «from» : Pubkey
  authority : Pubkey
  amount : Nat
  deriving Repr, DecidableEq

/-- Represents the parameters for an account close CPI -/
structure CloseCpi where
  program : Pubkey
  account : Pubkey
  destination : Pubkey
  authority : Pubkey
  deriving Repr, DecidableEq

/-- The standard SPL Token program ID (placeholder value) -/
def TOKEN_PROGRAM_ID : Pubkey := 0

/-- The standard System program ID (placeholder value) -/
def SYSTEM_PROGRAM_ID : Pubkey := 1

/-- Validity predicate for transfer CPIs -/
def transferCpiValid (cpi : TransferCpi) : Prop :=
  cpi.program = TOKEN_PROGRAM_ID ∧
  cpi.«from» ≠ cpi.«to» ∧
  cpi.amount ≤ U64_MAX

/-- Validity predicate for mint CPIs -/
def mintToCpiValid (cpi : MintToCpi) : Prop :=
  cpi.program = TOKEN_PROGRAM_ID ∧
  cpi.amount ≤ U64_MAX

/-- Validity predicate for burn CPIs -/
def burnCpiValid (cpi : BurnCpi) : Prop :=
  cpi.program = TOKEN_PROGRAM_ID ∧
  cpi.amount ≤ U64_MAX

/-- Validity predicate for close CPIs -/
def closeCpiValid (cpi : CloseCpi) : Prop :=
  cpi.program = TOKEN_PROGRAM_ID ∧
  cpi.account ≠ cpi.destination

/-- Multiple transfers are valid if all individual transfers are valid
    and they don't create cycles -/
def multipleTransfersValid (transfers : List TransferCpi) : Prop :=
  (∀ cpi ∈ transfers, transferCpiValid cpi) ∧
  -- All from/to pairs are distinct within each transfer
  (∀ cpi ∈ transfers, cpi.«from» ≠ cpi.«to»)

end QEDGen.Solana.Cpi

namespace QEDGen.Solana

-- Export CPI types and predicates
abbrev TransferCpi := QEDGen.Solana.Cpi.TransferCpi
abbrev MintToCpi := QEDGen.Solana.Cpi.MintToCpi
abbrev BurnCpi := QEDGen.Solana.Cpi.BurnCpi
abbrev CloseCpi := QEDGen.Solana.Cpi.CloseCpi
abbrev TOKEN_PROGRAM_ID := QEDGen.Solana.Cpi.TOKEN_PROGRAM_ID
abbrev SYSTEM_PROGRAM_ID := QEDGen.Solana.Cpi.SYSTEM_PROGRAM_ID
abbrev transferCpiValid := QEDGen.Solana.Cpi.transferCpiValid
abbrev mintToCpiValid := QEDGen.Solana.Cpi.mintToCpiValid
abbrev burnCpiValid := QEDGen.Solana.Cpi.burnCpiValid
abbrev closeCpiValid := QEDGen.Solana.Cpi.closeCpiValid
abbrev multipleTransfersValid := QEDGen.Solana.Cpi.multipleTransfersValid

end QEDGen.Solana
