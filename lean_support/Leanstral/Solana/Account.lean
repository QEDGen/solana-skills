namespace Leanstral.Solana.Account

abbrev Pubkey := Nat
abbrev U64 := Nat
abbrev U8 := Nat

structure Account where
  authority : Pubkey
  balance : Nat := 0
  writable : Bool := true
  deriving Repr, DecidableEq, BEq

def canWrite (actor : Pubkey) (account : Account) : Prop :=
  account.writable = true /\ account.authority = actor

end Leanstral.Solana.Account

namespace Leanstral.Solana

abbrev Pubkey := Leanstral.Solana.Account.Pubkey
abbrev U64 := Leanstral.Solana.Account.U64
abbrev U8 := Leanstral.Solana.Account.U8
abbrev Account := Leanstral.Solana.Account.Account
abbrev AccountState := Leanstral.Solana.Account.Account
abbrev canWrite := Leanstral.Solana.Account.canWrite

end Leanstral.Solana
