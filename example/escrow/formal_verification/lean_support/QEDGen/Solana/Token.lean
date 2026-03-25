import QEDGen.Solana.Account

namespace QEDGen.Solana.Token

open QEDGen.Solana.Account

/- ============================================================================
   Token API - Legacy Module

   DEPRECATED: This module is kept for backwards compatibility only.
   For new proofs, use QEDGen.Solana.Cpi instead.

   NEW APPROACH: We verify CPI parameter correctness, not balance preservation.
   See QEDGen.Solana.Cpi for the modern verification approach.
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
-- Use QEDGen.Solana.Cpi for new proofs instead.

end QEDGen.Solana.Token

namespace QEDGen.Solana

-- Legacy exports (deprecated - use QEDGen.Solana.Cpi instead)
abbrev TokenAccount := QEDGen.Solana.Token.TokenAccount
abbrev Mint := QEDGen.Solana.Token.Mint
abbrev Program := QEDGen.Solana.Token.Program
abbrev trackedTotal := QEDGen.Solana.Token.trackedTotal
abbrev trackedTotal_nil := QEDGen.Solana.Token.trackedTotal_nil

end QEDGen.Solana
