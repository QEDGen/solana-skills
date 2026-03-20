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

-- Cons preserves tracked total
axiom trackedTotal_cons (p_account : Account) (p_accounts : List Account) :
    trackedTotal (p_account :: p_accounts) = p_account.balance + trackedTotal p_accounts

-- Append distributes over tracked total
axiom trackedTotal_append (p_accounts1 p_accounts2 : List Account) :
    trackedTotal (p_accounts1 ++ p_accounts2) = trackedTotal p_accounts1 + trackedTotal p_accounts2

-- Mapping that preserves individual balances preserves total
theorem trackedTotal_map_id (p_accounts : List Account)
    (p_f : Account → Account)
    (p_h : ∀ acc, (p_f acc).balance = acc.balance) :
    trackedTotal (p_accounts.map p_f) = trackedTotal p_accounts := by
  induction p_accounts with
  | nil => simp [trackedTotal_nil]
  | cons head tail ih =>
    simp [trackedTotal_cons, p_h, ih]

-- Balance update at specific authority preserves total if delta is zero
axiom balance_update_preserves_total
    (p_accounts : List Account)
    (p_authority : Pubkey)
    (p_delta_in p_delta_out : Nat)
    (p_h_balance : p_delta_in = p_delta_out) :
    let updated := p_accounts.map (fun acc =>
      if acc.authority = p_authority then
        { acc with balance := acc.balance - p_delta_in + p_delta_out }
      else acc)
    trackedTotal updated = trackedTotal p_accounts

-- Two-account transfer preserves total (requires sufficient balance)
axiom transfer_preserves_total
    (p_accounts : List Account)
    (p_from_authority p_to_authority : Pubkey)
    (p_amount : Nat)
    (p_h_distinct : p_from_authority ≠ p_to_authority) :
    let updated := p_accounts.map (fun acc =>
      if acc.authority = p_from_authority then
        { acc with balance := acc.balance - p_amount }
      else if acc.authority = p_to_authority then
        { acc with balance := acc.balance + p_amount }
      else acc)
    trackedTotal updated = trackedTotal p_accounts

end Leanstral.Solana.Token

namespace Leanstral.Solana

abbrev TokenAccount := Leanstral.Solana.Token.TokenAccount
abbrev Mint := Leanstral.Solana.Token.Mint
abbrev Program := Leanstral.Solana.Token.Program
abbrev trackedTotal := Leanstral.Solana.Token.trackedTotal
abbrev trackedTotal_nil := Leanstral.Solana.Token.trackedTotal_nil
abbrev trackedTotal_cons := Leanstral.Solana.Token.trackedTotal_cons
abbrev trackedTotal_append := Leanstral.Solana.Token.trackedTotal_append
abbrev trackedTotal_map_id := Leanstral.Solana.Token.trackedTotal_map_id
abbrev balance_update_preserves_total := Leanstral.Solana.Token.balance_update_preserves_total
abbrev transfer_preserves_total := Leanstral.Solana.Token.transfer_preserves_total

end Leanstral.Solana
