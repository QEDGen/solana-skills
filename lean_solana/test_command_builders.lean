import QEDGen.Solana.CommandBuilders
/-!
# CommandBuilders unit tests

Run: `cd lean_solana && lake build && lake env lean test_command_builders.lean`

Tests verify that each builder produces correct strings by checking
against expected output. Since these strings are consumed by
`runParserCategory`, correctness here means "parses as valid Lean".
-/

open QEDGen.Solana.CommandBuilders

-- ============================================================================
-- safeName
-- ============================================================================

#guard safeName "maker" == "maker"
#guard safeName "initialize" == "«initialize»"
#guard safeName "open" == "«open»"
#guard safeName "myField" == "myField"
#guard safeName "then" == "«then»"

-- ============================================================================
-- mapType
-- ============================================================================

#guard mapType "U64" == "Nat"
#guard mapType "U128" == "Nat"
#guard mapType "I128" == "Int"
#guard mapType "U8" == "Nat"
#guard mapType "Pubkey" == "Pubkey"
#guard mapType "CustomType" == "CustomType"

-- ============================================================================
-- mkBinder
-- ============================================================================

#guard mkBinder "s" "State" == "(s : State)"
#guard mkBinder "amount" "Nat" == "(amount : Nat)"

-- ============================================================================
-- mkParamSig / mkParamArgs
-- ============================================================================

#guard mkParamSig #[] == ""
#guard mkParamSig #[("amount", "U64")] == " (amount : Nat)"
#guard mkParamSig #[("amount", "U64"), ("fee", "U64")] == " (amount : Nat) (fee : Nat)"
#guard mkParamSig #[("delta", "I128")] == " (delta : Int)"

#guard mkParamArgs #[] == ""
#guard mkParamArgs #[("amount", "U64")] == " amount"
#guard mkParamArgs #[("amount", "U64"), ("fee", "U64")] == " amount fee"

-- ============================================================================
-- mkConj
-- ============================================================================

#guard mkConj #[] == ""
#guard mkConj #["p = s.maker"] == "p = s.maker"
#guard mkConj #["p = s.maker", "s.status = .Open"] == "p = s.maker ∧ s.status = .Open"
#guard mkConj #["a", "b", "c"] == "a ∧ b ∧ c"

-- ============================================================================
-- mkSomeUpdate
-- ============================================================================

#guard mkSomeUpdate "s" #[] == "some s"
#guard mkSomeUpdate "s" #["status := .Closed"] == "some { s with status := .Closed }"
#guard mkSomeUpdate "s" #["balance := s.balance + amount", "status := .Active"]
  == "some { s with balance := s.balance + amount, status := .Active }"

-- ============================================================================
-- mkNamespace / mkEnd / mkOpen
-- ============================================================================

#guard mkNamespace "Escrow" == "namespace Escrow"
#guard mkEnd "Escrow" == "end Escrow"
#guard mkOpen "QEDGen.Solana" == "open QEDGen.Solana"

-- ============================================================================
-- mkAbbrev
-- ============================================================================

#guard mkAbbrev "E_BAD_DISC" "Nat" "1" == "abbrev E_BAD_DISC : Nat := 1"
#guard mkAbbrev "open" "Nat" "42" == "abbrev «open» : Nat := 42"

-- ============================================================================
-- mkSimpleDef
-- ============================================================================

#guard mkSimpleDef "FUEL" "Nat" "20" == "def FUEL : Nat := 20"

-- ============================================================================
-- mkStructure
-- ============================================================================

#guard mkStructure "State" #[("maker", "Pubkey"), ("balance", "U64")]
  == "structure State where\n  maker : Pubkey\n  balance : Nat\n  deriving Repr, DecidableEq, BEq"

-- With keyword field name
#guard mkStructure "State" #[("open", "U64")]
  == "structure State where\n  «open» : Nat\n  deriving Repr, DecidableEq, BEq"

-- No deriving
#guard mkStructure "Pair" #[("x", "Nat"), ("y", "Nat")] (deriving_ := #[])
  == "structure Pair where\n  x : Nat\n  y : Nat\n"

-- ============================================================================
-- mkInductive
-- ============================================================================

#guard mkInductive "Status" #["Open", "Closed"]
  == "inductive Status where | Open | Closed\n  deriving Repr, DecidableEq, BEq"

#guard mkInductive "Status" #["Active", "Completed", "Cancelled"]
  == "inductive Status where | Active | Completed | Cancelled\n  deriving Repr, DecidableEq, BEq"

-- ============================================================================
-- mkSorryTheorem
-- ============================================================================

#guard mkSorryTheorem "cancel.access_control"
    #["(s : State)", "(p : Pubkey)", "(h : cancelTransition s p ≠ none)"]
    "p = s.maker"
  == "theorem cancel.access_control (s : State) (p : Pubkey) (h : cancelTransition s p ≠ none) :\n    p = s.maker := sorry"

-- Empty binders
#guard mkSorryTheorem "trivial" #[] "True"
  == "theorem trivial :\n    True := sorry"

-- ============================================================================
-- mkDocSorryTheorem
-- ============================================================================

#guard mkDocSorryTheorem "withdraw.access_control"
    #["(s : State)", "(p : Pubkey)", "(h : withdrawTransition s p ≠ none)"]
    "p = s.owner"
    "Only the owner can withdraw funds."
  == "/-- Only the owner can withdraw funds. -/\ntheorem withdraw.access_control (s : State) (p : Pubkey) (h : withdrawTransition s p ≠ none) :\n    p = s.owner := sorry"

-- ============================================================================
-- mkModuleDoc
-- ============================================================================

#guard mkModuleDoc "## Assumptions\n- Balance tracked for U64 bounds"
  == "/-!\n## Assumptions\n- Balance tracked for U64 bounds\n-/"

-- ============================================================================
-- mkTacticTheorem
-- ============================================================================

#guard mkTacticTheorem "ea_FOO" #["(b : Nat)"] "effectiveAddr b FOO = b + 8" "unfold effectiveAddr FOO; omega"
  == "theorem ea_FOO (b : Nat) :\n    effectiveAddr b FOO = b + 8 := by\n  unfold effectiveAddr FOO; omega"

-- With set_option prefix
#guard mkTacticTheorem "big_proof" #["(s : State)"] "True"
    "trivial"
    (options := "set_option maxHeartbeats 800000 in")
  == "set_option maxHeartbeats 800000 in\ntheorem big_proof (s : State) :\n    True := by\n  trivial"

-- ============================================================================
-- mkDef
-- ============================================================================

#guard mkDef "cancelTransition" "(s : State) (signer : Pubkey) : Option State"
    "  if signer = s.maker then\n    some { s with status := .Closed }\n  else none"
  == "def cancelTransition (s : State) (signer : Pubkey) : Option State :=\n  if signer = s.maker then\n    some { s with status := .Closed }\n  else none"
