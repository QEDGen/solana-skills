import SVM.Pubkey

namespace QEDGen.Solana.Account

/-- A 32-byte Solana public key as four little-endian U64 chunks —
    qedsvm's `SVM.Pubkey` (the sBPF VM representation: programs compare
    pubkeys via four `ldx.dw` loads at byte offsets 0, 8, 16, 24).
    One type on both sides of the spec/binary boundary; `Pubkey.ext'`
    and `Pubkey.ne_iff` come from `SVM.Pubkey` too. -/
abbrev Pubkey := SVM.Pubkey

/-- Opaque 32-byte token (hash / digest / merkle root) — DSL `Bytes32` (#191).
    Equality-only semantics, so it shares `Pubkey`'s opaque carrier: the
    spec-level meaning is "an unforgeable token compared for equality"; the
    32-vs-64-byte width is a Rust-side layout concern the proofs never see. -/
abbrev Bytes32 := SVM.Pubkey

/-- Opaque 64-byte token (signature / recovered secp pubkey) — DSL `Bytes64`
    (#191). Same opaque carrier as `Bytes32`; see its docstring. -/
abbrev Bytes64 := SVM.Pubkey

abbrev U64 := Nat
abbrev U128 := Nat
abbrev I128 := Int
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

-- Find in a mapped list, when the map preserves the predicate
theorem find_map_pred_preserved
    (p_accounts : List Account)
    (p_pred : Account → Bool)
    (p_f : Account → Account)
    (p_h : ∀ acc, p_pred acc = p_pred (p_f acc)) :
    (p_accounts.map p_f).find? p_pred = (p_accounts.find? p_pred).map p_f := by
  induction p_accounts with
  | nil => rfl
  | cons a t ih =>
    simp only [List.map_cons, List.find?_cons, ← p_h a]
    cases p_pred a <;> simp [ih]

-- Find after updating a different account returns the same result
theorem find_map_update_other
    (p_accounts : List Account)
    (p_target_authority p_update_authority : Pubkey)
    (p_f : Account → Account)
    (p_h_distinct : p_target_authority ≠ p_update_authority)
    (p_h_preserves_auth : ∀ acc, (p_f acc).authority = acc.authority) :
    let updated := p_accounts.map (fun acc =>
      if acc.authority = p_update_authority then p_f acc else acc)
    findByAuthority updated p_target_authority = findByAuthority p_accounts p_target_authority := by
  intro updated
  simp only [updated, findByAuthority]
  induction p_accounts with
  | nil => rfl
  | cons a t ih =>
    simp only [List.map_cons, List.find?_cons]
    by_cases hu : a.authority = p_update_authority
    · have hne : p_update_authority ≠ p_target_authority := fun h => p_h_distinct h.symm
      simp [hu, p_h_preserves_auth, hne, ih]
    · simp [hu, ih]

-- Find after updating the target account returns the updated account
theorem find_map_update_same
    (p_accounts : List Account)
    (p_authority : Pubkey)
    (p_original : Account)
    (p_f : Account → Account)
    (p_h_found : findByAuthority p_accounts p_authority = some p_original)
    (p_h_preserves_auth : ∀ acc, (p_f acc).authority = acc.authority) :
    let updated := p_accounts.map (fun acc =>
      if acc.authority = p_authority then p_f acc else acc)
    findByAuthority updated p_authority = some (p_f p_original) := by
  intro updated
  simp only [updated, findByAuthority] at p_h_found ⊢
  induction p_accounts with
  | nil => simp at p_h_found
  | cons a t ih =>
    simp only [List.map_cons, List.find?_cons] at p_h_found ⊢
    by_cases hu : a.authority = p_authority
    · have ha : a = p_original := by simpa [hu] using p_h_found
      subst ha
      simp [hu, p_h_preserves_auth]
    · simp [hu] at p_h_found
      simp [hu, ih p_h_found]

-- Key-based versions: find after updating a different account (by key)
theorem find_by_key_map_update_other
    (p_accounts : List Account)
    (p_target_key p_update_key : Pubkey)
    (p_f : Account → Account)
    (p_h_distinct : p_target_key ≠ p_update_key)
    (p_h_preserves_key : ∀ acc, (p_f acc).key = acc.key) :
    let updated := p_accounts.map (fun acc =>
      if acc.key = p_update_key then p_f acc else acc)
    findByKey updated p_target_key = findByKey p_accounts p_target_key := by
  intro updated
  simp only [updated, findByKey]
  induction p_accounts with
  | nil => rfl
  | cons a t ih =>
    simp only [List.map_cons, List.find?_cons]
    by_cases hu : a.key = p_update_key
    · have hne : p_update_key ≠ p_target_key := fun h => p_h_distinct h.symm
      simp [hu, p_h_preserves_key, hne, ih]
    · simp [hu, ih]

-- Find by key after updating the target account returns the updated account
theorem find_by_key_map_update_same
    (p_accounts : List Account)
    (p_key : Pubkey)
    (p_original : Account)
    (p_f : Account → Account)
    (p_h_found : findByKey p_accounts p_key = some p_original)
    (p_h_preserves_key : ∀ acc, (p_f acc).key = acc.key) :
    let updated := p_accounts.map (fun acc =>
      if acc.key = p_key then p_f acc else acc)
    findByKey updated p_key = some (p_f p_original) := by
  intro updated
  simp only [updated, findByKey] at p_h_found ⊢
  induction p_accounts with
  | nil => simp at p_h_found
  | cons a t ih =>
    simp only [List.map_cons, List.find?_cons] at p_h_found ⊢
    by_cases hu : a.key = p_key
    · have ha : a = p_original := by simpa [hu] using p_h_found
      subst ha
      simp [hu, p_h_preserves_key]
    · simp [hu] at p_h_found
      simp [hu, ih p_h_found]

end QEDGen.Solana.Account

namespace QEDGen.Solana

abbrev Pubkey := QEDGen.Solana.Account.Pubkey
abbrev U64 := QEDGen.Solana.Account.U64
abbrev U128 := QEDGen.Solana.Account.U128
abbrev I128 := QEDGen.Solana.Account.I128
abbrev U8 := QEDGen.Solana.Account.U8
abbrev Account := QEDGen.Solana.Account.Account
abbrev canWrite := QEDGen.Solana.Account.canWrite
abbrev findByKey := QEDGen.Solana.Account.findByKey
abbrev findByAuthority := QEDGen.Solana.Account.findByAuthority
abbrev find_map_pred_preserved := QEDGen.Solana.Account.find_map_pred_preserved
abbrev find_map_update_other := QEDGen.Solana.Account.find_map_update_other
abbrev find_map_update_same := QEDGen.Solana.Account.find_map_update_same
abbrev find_by_key_map_update_other := QEDGen.Solana.Account.find_by_key_map_update_other
abbrev find_by_key_map_update_same := QEDGen.Solana.Account.find_by_key_map_update_same

end QEDGen.Solana
