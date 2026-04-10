import QEDGen.Solana.SBPF
import Lean.Elab.Command

/-!
# QEDGen Guards DSL

Sequential validation guard chains for sBPF programs.
The `qedguards` block generates rejection theorem stubs (with `sorry`)
where hypotheses accumulate: guard N assumes all prior guards passed.

This is designed for programs like Dropset where each validation check
exits with a specific error code on failure.
-/

namespace QEDGen.Solana.GuardsDSL

-- ============================================================================
-- Syntax declarations
-- ============================================================================

/-- A hypothesis line inside a guard block (string literal). -/
syntax guardHyp := str

/-- Error code declaration: `E_NAME value` -/
syntax guardErrorDecl := ident num

/-- A single guard block. -/
syntax guardBlock :=
  "guard" ident "fuel" num "error" (num <|> ident)
    ("hyps" guardHyp*)?
    ("after" guardHyp*)?

/-- The top-level qedguards command.
    Uses fixed `r1:` / `r2:` keywords for register bindings
    to avoid ambiguity with the `guard` keyword. -/
syntax (name := qedguardsCmd)
  "qedguards " ident " where"
    "prog: " ident
    ("entry: " num)?
    "r1: " ident
    ("r2: " ident)?
    ("errors" guardErrorDecl*)?
    guardBlock*
  : command

-- ============================================================================
-- Elaborator
-- ============================================================================

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
@[command_elab qedguardsCmd]
def elabQedguards : CommandElab := fun stx => do
  let nameStx := stx[1]
  let name := nameStx.getId.toString (escape := false)

  -- prog (index 4: "prog: " at [3], ident at [4])
  let progName := stx[4].getId.toString (escape := false)

  -- Optional entry PC (index 5)
  let entryStx := stx[5]
  let entryPc := if !entryStx.isMissing && entryStx.getNumArgs > 0 then
    match entryStx[1].isNatLit? with
    | some n => n
    | none => 0
  else 0

  -- r1 (index 7: "r1: " at [6], ident at [7])
  let r1Name := stx[7].getId.toString (escape := false)

  -- Optional r2 (index 8)
  let r2Stx := stx[8]
  let hasR2 := !r2Stx.isMissing && r2Stx.getNumArgs > 0
  let r2Name := if hasR2 then
    r2Stx[1].getId.toString (escape := false)
  else ""

  -- Build initExpr and params
  let entryStr := s!"{entryPc}"
  let initExpr := if hasR2 then
    s!"initState2 {r1Name} {r2Name} mem {entryStr}"
  else
    s!"initState {r1Name} mem"

  let params := if hasR2 then
    s!"({r1Name} {r2Name} : Nat) (mem : Mem)"
  else
    s!"({r1Name} : Nat) (mem : Mem)"

  -- Optional errors (index 9)
  let errorsStx := stx[9]
  let mut errorDecls : Array (String × Nat) := #[]
  if !errorsStx.isMissing && errorsStx.getNumArgs > 0 then
    let errorListStx := errorsStx[1]  -- guardErrorDecl*
    for e in errorListStx.getArgs do
      let eName := e[0].getId.toString (escape := false)
      let eVal := match e[1].isNatLit? with
        | some n => n
        | none => 0
      errorDecls := errorDecls.push (eName, eVal)

  -- Parse guard blocks (index 10)
  let guardsStx := stx[10]
  let mut guardList : Array (String × Nat × String × Array String × Array String) := #[]
  for g in guardsStx.getArgs do
    let gName := g[1].getId.toString (escape := false)

    let fuelN := match g[3].isNatLit? with
      | some n => n
      | none => 0

    -- error: can be num or ident
    let errorNode := g[5]
    let errStr := match errorNode.isNatLit? with
      | some n => s!"{n}"
      | none => errorNode.getId.toString (escape := false)

    -- Optional hyps (index 6)
    let hypsOpt := g[6]
    let mut gHyps : Array String := #[]
    if !hypsOpt.isMissing && hypsOpt.getNumArgs > 0 then
      let hypListStx := hypsOpt[1]  -- guardHyp*
      for hStx in hypListStx.getArgs do
        match hStx[0].isStrLit? with
        | some s => gHyps := gHyps.push s
        | none => pure ()

    -- Optional after (index 7)
    let afterOpt := g[7]
    let mut gAfter : Array String := #[]
    if !afterOpt.isMissing && afterOpt.getNumArgs > 0 then
      let afterListStx := afterOpt[1]  -- guardHyp*
      for hStx in afterListStx.getArgs do
        match hStx[0].isStrLit? with
        | some s => gAfter := gAfter.push s
        | none => pure ()

    guardList := guardList.push (gName, fuelN, errStr, gHyps, gAfter)

  -- ================================================================
  -- Generate commands
  -- ================================================================
  let mut cmds : Array String := #[]
  let nl := "\n"

  cmds := cmds.push s!"namespace {name}"
  cmds := cmds.push s!"open QEDGen.Solana"
  cmds := cmds.push s!"open QEDGen.Solana.SBPF"
  cmds := cmds.push s!"open QEDGen.Solana.SBPF.Memory"

  -- Error code constants
  for (eName, eVal) in errorDecls do
    cmds := cmds.push s!"abbrev {eName} : Nat := {eVal}"

  -- Accumulate after-blocks and generate one theorem per guard
  let mut accumulated : Array String := #[]

  for (gName, fuelN, errStr, gHyps, gAfter) in guardList do
    -- Build theorem
    let mut thmStr := s!"theorem {gName} ({progName} : Nat → Option QEDGen.Solana.SBPF.Insn)" ++ nl
    thmStr := thmStr ++ s!"    {params}" ++ nl

    -- Accumulated after from prior guards
    for hp in accumulated do
      thmStr := thmStr ++ s!"    {hp}" ++ nl

    -- This guard's hypotheses
    for hp in gHyps do
      thmStr := thmStr ++ s!"    {hp}" ++ nl

    -- Conclusion
    thmStr := thmStr ++ s!"    :" ++ nl
    thmStr := thmStr ++ s!"    (executeFn {progName} ({initExpr}) {fuelN}).exitCode" ++ nl
    thmStr := thmStr ++ s!"      = some {errStr} := sorry"

    cmds := cmds.push thmStr

    -- Add this guard's after-block to the accumulation
    for hp in gAfter do
      accumulated := accumulated.push hp

  cmds := cmds.push s!"end {name}"

  -- Parse and elaborate each command
  let env ← getEnv
  for src in cmds do
    match Lean.Parser.runParserCategory env `command src "<qedguards>" with
    | .error msg =>
      throwError m!"qedguards: failed to parse generated code:{nl}{msg}{nl}{nl}Source:{nl}{src}"
    | .ok cmdStx =>
      elabCommand cmdStx

end QEDGen.Solana.GuardsDSL
