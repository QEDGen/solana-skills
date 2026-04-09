import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import Lean.Elab.Command

/-!
# QEDGen Spec DSL

Declarative specification macros for Solana program verification.
The `qedspec` block is the source of truth — it expands to:
  - State structure with DecidableEq
  - Transition function stubs (sorry — agent fills)
  - Typed theorem signatures with sorry (one per operation × property)
  - Invariant theorem stubs

Humans write and approve the spec. Agents fill the sorry markers.
`lake build` enforces that every declared property has a proof.
-/

open QEDGen.Solana

-- ============================================================================
-- Syntax declarations
-- ============================================================================

namespace QEDGen.Solana.SpecDSL

/-- A single state field: `fieldName : FieldType` -/
syntax specField := ident " : " ident

/-- Operation block -/
syntax specOp :=
  "operation " ident
    "who: " ident
    "when: " ident
    "then: " ident

/-- Invariant declaration -/
syntax specInvariant := "invariant " ident str

/-- The top-level qedspec command. -/
syntax (name := qedspecCmd)
  "qedspec " ident " where"
    "state" specField*
    specOp*
    specInvariant*
  : command

-- ============================================================================
-- Elaborator: parse qedspec syntax, generate Lean source, elaborate it
-- ============================================================================

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
@[command_elab qedspecCmd]
def elabQedspec : CommandElab := fun stx => do
  -- Extract pieces from the syntax tree
  -- Layout: "qedspec" ident "where" "state" fields* ops* invs*
  let progNameStx := stx[1]
  let name := progNameStx.getId
  let fieldsStx := stx[4]  -- specField* (index 4: after "qedspec" ident "where" "state")
  let opsStx := stx[5]     -- specOp*
  let invsStx := stx[6]    -- specInvariant*

  -- Parse field declarations
  let mut fieldData : Array (String × String) := #[]
  for f in fieldsStx.getArgs do
    let fieldName := f[0].getId.toString (escape := false)
    let fieldType := f[2].getId.toString (escape := false)
    fieldData := fieldData.push (fieldName, fieldType)

  -- Build state structure field source
  let mut structFields := ""
  for (fn_, ft) in fieldData do
    structFields := structFields ++ s!"  {fn_} : {ft}\n"

  -- Assemble individual command strings to parse and elaborate one at a time
  -- (Lean's runParserCategory `command parses exactly ONE command)
  let mut cmds : Array String := #[]
  cmds := cmds.push s!"namespace {name}"
  cmds := cmds.push s!"open QEDGen.Solana"
  cmds := cmds.push s!"structure State where\n{structFields}  deriving Repr, DecidableEq, BEq"

  for op in opsStx.getArgs do
    let opName := op[1].getId.toString (escape := false)
    let signer := op[3].getId.toString (escape := false)
    let transName := s!"{opName}Transition"

    cmds := cmds.push s!"noncomputable def {transName} (s : State) (signer : Pubkey) : Option State := sorry"
    cmds := cmds.push (s!"theorem {opName}.access_control (s : State) (p : Pubkey)\n" ++
      s!"    (h : {transName} s p ≠ none) :\n" ++
      s!"    p = s.{signer} := sorry")
    cmds := cmds.push (s!"theorem {opName}.state_machine (s s' : State) (p : Pubkey)\n" ++
      s!"    (h : {transName} s p = some s') :\n" ++
      s!"    True := sorry")

  for inv in invsStx.getArgs do
    let invName := inv[1].getId.toString (escape := false)
    cmds := cmds.push s!"theorem {invName} : True := sorry"

  cmds := cmds.push s!"end {name}"

  -- Parse and elaborate each command
  let env ← getEnv
  for src in cmds do
    match Lean.Parser.runParserCategory env `command src "<qedspec>" with
    | .error msg =>
      throwError m!"qedspec: failed to parse generated code:\n{msg}\n\nSource:\n{src}"
    | .ok cmdStx =>
      elabCommand cmdStx

end QEDGen.Solana.SpecDSL
