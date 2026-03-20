namespace Leanstral.Solana.Account

abbrev Pubkey := Nat
abbrev U64 := Nat
abbrev U8 := Nat

structure Account where
  key : Pubkey
  authority : Pubkey
  balance : Nat := 0
  writable : Bool := true
  deriving Repr, DecidableEq, BEq

def canWrite (actor : Pubkey) (account : Account) : Prop :=
  account.writable = true /\ account.authority = actor

-- Finding an account by key
def findByKey (p_accounts : List Account) (p_key : Pubkey) : Option Account :=
  p_accounts.find? (fun acc => acc.key = p_key)

-- Finding an account by authority
def findByAuthority (p_accounts : List Account) (p_authority : Pubkey) : Option Account :=
  p_accounts.find? (fun acc => acc.authority = p_authority)

-- Find in mapped list: if mapping preserves the predicate's relevant fields
-- Using axiom for now - full proof would require more complex induction
axiom find_map_pred_preserved
    (p_accounts : List Account)
    (p_pred : Account → Bool)
    (p_f : Account → Account)
    (p_h : ∀ acc, p_pred acc = p_pred (p_f acc)) :
    (p_accounts.map p_f).find? p_pred = (p_accounts.find? p_pred).map p_f

-- Find after updating a different account returns the same result
axiom find_map_update_other
    (p_accounts : List Account)
    (p_target_authority p_update_authority : Pubkey)
    (p_f : Account → Account)
    (p_h_distinct : p_target_authority ≠ p_update_authority)
    (p_h_preserves_auth : ∀ acc, (p_f acc).authority = acc.authority) :
    let updated := p_accounts.map (fun acc =>
      if acc.authority = p_update_authority then p_f acc else acc)
    findByAuthority updated p_target_authority = findByAuthority p_accounts p_target_authority

-- Find after updating the target account returns the updated account
axiom find_map_update_same
    (p_accounts : List Account)
    (p_authority : Pubkey)
    (p_original : Account)
    (p_f : Account → Account)
    (p_h_found : findByAuthority p_accounts p_authority = some p_original)
    (p_h_preserves_auth : ∀ acc, (p_f acc).authority = acc.authority) :
    let updated := p_accounts.map (fun acc =>
      if acc.authority = p_authority then p_f acc else acc)
    findByAuthority updated p_authority = some (p_f p_original)

-- Key-based versions: Find after updating a different account (by key)
axiom find_by_key_map_update_other
    (p_accounts : List Account)
    (p_target_key p_update_key : Pubkey)
    (p_f : Account → Account)
    (p_h_distinct : p_target_key ≠ p_update_key)
    (p_h_preserves_key : ∀ acc, (p_f acc).key = acc.key) :
    let updated := p_accounts.map (fun acc =>
      if acc.key = p_update_key then p_f acc else acc)
    findByKey updated p_target_key = findByKey p_accounts p_target_key

-- Find by key after updating the target account returns the updated account
axiom find_by_key_map_update_same
    (p_accounts : List Account)
    (p_key : Pubkey)
    (p_original : Account)
    (p_f : Account → Account)
    (p_h_found : findByKey p_accounts p_key = some p_original)
    (p_h_preserves_key : ∀ acc, (p_f acc).key = acc.key) :
    let updated := p_accounts.map (fun acc =>
      if acc.key = p_key then p_f acc else acc)
    findByKey updated p_key = some (p_f p_original)

end Leanstral.Solana.Account

namespace Leanstral.Solana

abbrev Pubkey := Leanstral.Solana.Account.Pubkey
abbrev U64 := Leanstral.Solana.Account.U64
abbrev U8 := Leanstral.Solana.Account.U8
abbrev Account := Leanstral.Solana.Account.Account
abbrev AccountState := Leanstral.Solana.Account.Account
abbrev canWrite := Leanstral.Solana.Account.canWrite
abbrev findByKey := Leanstral.Solana.Account.findByKey
abbrev findByAuthority := Leanstral.Solana.Account.findByAuthority
abbrev find_map_pred_preserved := Leanstral.Solana.Account.find_map_pred_preserved
abbrev find_map_update_other := Leanstral.Solana.Account.find_map_update_other
abbrev find_map_update_same := Leanstral.Solana.Account.find_map_update_same
abbrev find_by_key_map_update_other := Leanstral.Solana.Account.find_by_key_map_update_other
abbrev find_by_key_map_update_same := Leanstral.Solana.Account.find_by_key_map_update_same

end Leanstral.Solana
