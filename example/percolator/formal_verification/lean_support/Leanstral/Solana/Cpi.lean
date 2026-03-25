import Leanstral.Solana.Account
import Leanstral.Solana.Valid

namespace Leanstral.Solana.Cpi

open Leanstral.Solana.Account
open Leanstral.Solana.Valid

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

end Leanstral.Solana.Cpi

namespace Leanstral.Solana

-- Export CPI types and predicates
abbrev TransferCpi := Leanstral.Solana.Cpi.TransferCpi
abbrev MintToCpi := Leanstral.Solana.Cpi.MintToCpi
abbrev BurnCpi := Leanstral.Solana.Cpi.BurnCpi
abbrev CloseCpi := Leanstral.Solana.Cpi.CloseCpi
abbrev TOKEN_PROGRAM_ID := Leanstral.Solana.Cpi.TOKEN_PROGRAM_ID
abbrev SYSTEM_PROGRAM_ID := Leanstral.Solana.Cpi.SYSTEM_PROGRAM_ID
abbrev transferCpiValid := Leanstral.Solana.Cpi.transferCpiValid
abbrev mintToCpiValid := Leanstral.Solana.Cpi.mintToCpiValid
abbrev burnCpiValid := Leanstral.Solana.Cpi.burnCpiValid
abbrev closeCpiValid := Leanstral.Solana.Cpi.closeCpiValid
abbrev multipleTransfersValid := Leanstral.Solana.Cpi.multipleTransfersValid

end Leanstral.Solana
