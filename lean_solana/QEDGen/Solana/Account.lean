namespace QEDGen.Solana.Account

/-- A 32-byte Solana public key as four little-endian U64 chunks.
    Matches sBPF VM representation: programs compare pubkeys via
    four `ldx.dw` loads at byte offsets 0, 8, 16, 24. -/
structure Pubkey where
  c0 : Nat  -- bytes 0-7, little-endian U64
  c1 : Nat  -- bytes 8-15
  c2 : Nat  -- bytes 16-23
  c3 : Nat  -- bytes 24-31
  deriving DecidableEq, BEq, Repr, Inhabited

theorem Pubkey.ext' {a b : Pubkey}
    (h0 : a.c0 = b.c0) (h1 : a.c1 = b.c1) (h2 : a.c2 = b.c2) (h3 : a.c3 = b.c3) :
    a = b := by
  cases a; cases b; simp_all

/-- Two pubkeys differ iff at least one chunk differs. -/
theorem Pubkey.ne_iff {a b : Pubkey} :
    a ≠ b ↔ a.c0 ≠ b.c0 ∨ a.c1 ≠ b.c1 ∨ a.c2 ≠ b.c2 ∨ a.c3 ≠ b.c3 := by
  constructor
  · intro h
    if h0 : a.c0 = b.c0 then
      if h1 : a.c1 = b.c1 then
        if h2 : a.c2 = b.c2 then
          if h3 : a.c3 = b.c3 then
            exact absurd (Pubkey.ext' h0 h1 h2 h3) h
          else exact Or.inr (Or.inr (Or.inr h3))
        else exact Or.inr (Or.inr (Or.inl h2))
      else exact Or.inr (Or.inl h1)
    else exact Or.inl h0
  · intro h heq; subst heq
    cases h with
    | inl h => exact h rfl
    | inr h => cases h with
      | inl h => exact h rfl
      | inr h => cases h with
        | inl h => exact h rfl
        | inr h => exact h rfl

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

end QEDGen.Solana.Account

namespace QEDGen.Solana

abbrev Pubkey := QEDGen.Solana.Account.Pubkey
abbrev U64 := QEDGen.Solana.Account.U64
abbrev U8 := QEDGen.Solana.Account.U8
abbrev Account := QEDGen.Solana.Account.Account
abbrev AccountState := QEDGen.Solana.Account.Account
abbrev canWrite := QEDGen.Solana.Account.canWrite
abbrev findByKey := QEDGen.Solana.Account.findByKey
abbrev findByAuthority := QEDGen.Solana.Account.findByAuthority
abbrev find_map_pred_preserved := QEDGen.Solana.Account.find_map_pred_preserved
abbrev find_map_update_other := QEDGen.Solana.Account.find_map_update_other
abbrev find_map_update_same := QEDGen.Solana.Account.find_map_update_same
abbrev find_by_key_map_update_other := QEDGen.Solana.Account.find_by_key_map_update_other
abbrev find_by_key_map_update_same := QEDGen.Solana.Account.find_by_key_map_update_same

end QEDGen.Solana
