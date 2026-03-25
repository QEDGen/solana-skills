import Leanstral.Solana.Account

namespace Leanstral.Solana.Token

open Leanstral.Solana.Account

/- ============================================================================
   Token API - Legacy Module

   DEPRECATED: This module is kept for backwards compatibility only.
   For new proofs, use Leanstral.Solana.Cpi instead.

   NEW APPROACH: We verify CPI parameter correctness, not balance preservation.
   See Leanstral.Solana.Cpi for the modern verification approach.
   ============================================================================ -/

structure Mint where
  id : Nat := 0
  deriving Repr, DecidableEq, BEq

structure Program where
  id : Nat := 0
  deriving Repr, DecidableEq, BEq

abbrev TokenAccount := Account

def trackedTotal (accounts : List Account) : Nat :=
  accounts.foldl (fun acc account => acc + account.balance) 0

theorem trackedTotal_nil : trackedTotal [] = 0 := by
  rfl

-- NOTE: The following axioms are DEPRECATED and will be removed in a future version.
-- Use Leanstral.Solana.Cpi for new proofs instead.

end Leanstral.Solana.Token

namespace Leanstral.Solana

-- Legacy exports (deprecated - use Leanstral.Solana.Cpi instead)
abbrev TokenAccount := Leanstral.Solana.Token.TokenAccount
abbrev Mint := Leanstral.Solana.Token.Mint
abbrev Program := Leanstral.Solana.Token.Program
abbrev trackedTotal := Leanstral.Solana.Token.trackedTotal
abbrev trackedTotal_nil := Leanstral.Solana.Token.trackedTotal_nil

end Leanstral.Solana
