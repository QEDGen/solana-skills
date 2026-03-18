import Leanstral.Solana.Account

namespace Leanstral.Solana.Token

open Leanstral.Solana.Account

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

end Leanstral.Solana.Token

namespace Leanstral.Solana

abbrev TokenAccount := Leanstral.Solana.Token.TokenAccount
abbrev Mint := Leanstral.Solana.Token.Mint
abbrev Program := Leanstral.Solana.Token.Program
abbrev trackedTotal := Leanstral.Solana.Token.trackedTotal

end Leanstral.Solana
