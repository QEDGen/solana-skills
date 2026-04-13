import QEDGen.Solana.CommandBuilders
import QEDGen.Solana.SBPF
import Lean.Elab.Command

/-!
# QEDGen Guards DSL

Sequential validation guard chains for sBPF programs.
The `qedguards` block generates:
1. A `Spec` structure with rejection theorem types (hypotheses accumulate)
2. `effectiveAddr` (ea_*) lemmas for listed offset constants
3. Proof bodies for simple guards marked `proof auto`
4. Phase decomposition scaffolding for complex guards marked `proof phased`

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

/-- An offset constant declaration: `NAME "value"` where value is an integer string. -/
syntax guardOffsetDecl := ident str

/-- A phase within a phased proof: `phase NAME FUEL` -/
syntax guardPhase := "phase" ident num

/-- A single guard block. -/
syntax guardBlock :=
  "guard" ident "fuel" num "error" (num <|> ident)
    ("proof" ("auto" <|> "sorry" <|> "phased"))?
    ("phases" guardPhase+)?
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
    ("chunks" ident+)?
    ("errors" guardErrorDecl*)?
    ("offsets" guardOffsetDecl*)?
    guardBlock*
  : command

-- ============================================================================
-- Offset helpers
-- ============================================================================

/-- Compute the RHS string of an ea_* lemma given the Int offset value.
    e.g., val=0 → "b", val=88 → "b + 88", val=-8 → "b - 8" -/
private def offsetRhs (val : Int) : String :=
  if val == 0 then "b"
  else if val > 0 then s!"b + {val.toNat}"
  else s!"b - {val.natAbs}"

/-- Parse an integer from a string (handles negative values). -/
private def parseIntStr (s : String) : Int :=
  let s := s.trimAscii.toString
  if s.startsWith "-" then
    let absStr := (s.drop 1).toString
    match absStr.toNat? with
    | some n => -(n : Int)
    | none => 0
  else
    match s.toNat? with
    | some n => (n : Int)
    | none => 0

-- ============================================================================
-- Hypothesis parsing helpers for auto proof generation
-- ============================================================================

/-- Parse a hypothesis string "(name : type)" into (name, type).
    Handles types containing " : " (e.g., `↑X : Int` inside expressions). -/
private def parseHypStr (s : String) : Option (String × String) :=
  let s := s.trimAscii.toString
  if s.startsWith "(" && s.endsWith ")" then
    let inner := ((s.drop 1).dropEnd 1).toString
    match inner.splitOn " : " with
    | name :: rest =>
      if rest.isEmpty then none
      else some (name.trimAscii.toString, (" : ".intercalate rest).trimAscii.toString)
    | _ => none
  else none

/-- Check if a string is a simple identifier (alphanumeric + underscore). -/
private def isSimpleIdent (s : String) : Bool :=
  s.length > 0 && s.all (fun c => c.isAlpha || c.isDigit || c == '_')

/-- Parsed read binding: `readU8 mem expr = varName` -/
private structure ReadBinding where
  hypName : String
  readFn : String
  memExpr : String
  varName : String

/-- Type of condition on a variable. -/
private inductive CondKind where
  | neq
  | lt
  | negLt

/-- Parsed condition on a variable. -/
private structure CondBinding where
  hypName : String
  varName : String
  kind : CondKind
  constExpr : String

/-- Try to parse a read binding from the type string. -/
private def tryParseRead (hypName type : String) : Option ReadBinding :=
  if type.startsWith "readU8 mem " then
    let rest := (type.drop "readU8 mem ".length).toString
    match rest.splitOn " = " with
    | [expr, rhs] => some { hypName, readFn := "readU8", memExpr := expr.trimAscii.toString, varName := rhs.trimAscii.toString }
    | _ => none
  else if type.startsWith "readU64 mem " then
    let rest := (type.drop "readU64 mem ".length).toString
    match rest.splitOn " = " with
    | [expr, rhs] => some { hypName, readFn := "readU64", memExpr := expr.trimAscii.toString, varName := rhs.trimAscii.toString }
    | _ => none
  else none

/-- Try to parse a condition from the type string. -/
private def tryParseCond (hypName type : String) : Option CondBinding :=
  -- Try ¬(VAR < CONST)
  if type.startsWith "¬(" && type.endsWith ")" then
    let inner := ((type.drop 2).dropEnd 1).toString
    match inner.splitOn " < " with
    | [var, const] =>
      if isSimpleIdent var.trimAscii.toString then
        some { hypName, varName := var.trimAscii.toString, kind := .negLt, constExpr := const.trimAscii.toString }
      else none
    | _ => none
  else
    -- Try VAR ≠ CONST
    match type.splitOn " ≠ " with
    | [var, const] =>
      if isSimpleIdent var.trimAscii.toString then
        some { hypName, varName := var.trimAscii.toString, kind := .neq, constExpr := const.trimAscii.toString }
      else none
    | _ =>
      -- Try VAR < CONST
      match type.splitOn " < " with
      | [var, const] =>
        if isSimpleIdent var.trimAscii.toString then
          some { hypName, varName := var.trimAscii.toString, kind := .lt, constExpr := const.trimAscii.toString }
        else none
      | _ => none

/-- Generate a `have` lifting statement from a read binding + condition pair. -/
private def genLiftHave (rb : ReadBinding) (cond : CondBinding) (idx : Nat) : String :=
  let readExpr := s!"{rb.readFn} mem {rb.memExpr}"
  match cond.kind with
  | .neq =>
    s!"  have h_lift_{idx} : ¬({readExpr} = {cond.constExpr}) := by rw [{rb.hypName}]; exact {cond.hypName}"
  | .lt =>
    s!"  have h_lift_{idx} : {readExpr} < {cond.constExpr} := by rw [{rb.hypName}]; exact {cond.hypName}"
  | .negLt =>
    s!"  have h_lift_{idx} : ¬({readExpr} < {cond.constExpr}) := by rw [{rb.hypName}]; exact {cond.hypName}"

/-- Given all hypotheses, produce lifting `have` lines and wp_exec call. -/
def genAutoBody
    (allHyps : Array String)
    (progName : String)
    (chunkDefs : Array String)
    (eaNames : Array String) : Array String := Id.run do
  -- Phase 1: identify variables, read bindings, conditions
  let mut varDecls : Array String := #[]
  let mut readBindings : Array ReadBinding := #[]
  let mut conditions : Array CondBinding := #[]

  for h in allHyps do
    if let some (name, type) := parseHypStr h then
      if type == "Nat" then
        varDecls := varDecls.push name
      else if let some rb := tryParseRead name type then
        if varDecls.contains rb.varName then
          readBindings := readBindings.push rb
      else if let some cond := tryParseCond name type then
        if varDecls.contains cond.varName then
          conditions := conditions.push cond

  -- Phase 2: match conditions with read bindings by variable name
  let mut lines : Array String := #[]
  let mut liftIdx : Nat := 0
  for cond in conditions do
    let mut found := false
    for rb in readBindings do
      if !found && rb.varName == cond.varName then
        lines := lines.push (genLiftHave rb cond liftIdx)
        liftIdx := liftIdx + 1
        found := true

  -- Phase 3: wp_exec call
  let defsStr := ", ".intercalate ((#[progName] ++ chunkDefs).toList)
  let eaStr := ", ".intercalate ((eaNames ++ #["U32_MODULUS"]).toList)
  lines := lines.push s!"  wp_exec [{defsStr}] [{eaStr}]"
  lines

-- ============================================================================
-- Proof mode
-- ============================================================================

/-- Proof generation mode for a guard. -/
private inductive ProofMode where
  | none                                      -- no proof generated (default / `proof sorry`)
  | auto                                      -- full wp_exec proof
  | phased (phs : Array (String × Nat))       -- per-phase sorry stubs + composition

-- ============================================================================
-- Phased proof generation
-- ============================================================================

/-- Build a right-associated fuel sum string: [47, 14] → "47 + 14",
    [25, 11, 11] → "25 + (11 + 11)" -/
private def buildFuelSumList : List Nat → String
  | [] => "0"
  | [n] => s!"{n}"
  | [a, b] => s!"{a} + {b}"
  | n :: rest => s!"{n} + ({buildFuelSumList rest})"

private def buildFuelSum (fuels : Array Nat) : String :=
  buildFuelSumList fuels.toList

-- ============================================================================
-- Elaborator
-- ============================================================================

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
open QEDGen.Solana.CommandBuilders in
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

  -- Optional chunks (index 9)
  let chunksStx := stx[9]
  let mut chunkDefs : Array String := #[]
  if !chunksStx.isMissing && chunksStx.getNumArgs > 0 then
    let chunkListStx := chunksStx[1]  -- ident+
    for cStx in chunkListStx.getArgs do
      chunkDefs := chunkDefs.push (cStx.getId.toString (escape := false))

  -- Optional errors (index 10)
  let errorsStx := stx[10]
  let mut errorDecls : Array (String × Nat) := #[]
  if !errorsStx.isMissing && errorsStx.getNumArgs > 0 then
    let errorListStx := errorsStx[1]  -- guardErrorDecl*
    for e in errorListStx.getArgs do
      let eName := e[0].getId.toString (escape := false)
      let eVal := match e[1].isNatLit? with
        | some n => n
        | none => 0
      errorDecls := errorDecls.push (eName, eVal)

  -- Optional offsets (index 11) — parse string values and prepare ea_* theorems
  let offsetsStx := stx[11]
  let mut eaNames : Array String := #[]
  let mut eaCmds : Array String := #[]
  if !offsetsStx.isMissing && offsetsStx.getNumArgs > 0 then
    let offsetListStx := offsetsStx[1]  -- guardOffsetDecl*
    for oStx in offsetListStx.getArgs do
      let oNameStr := oStx[0].getId.toString (escape := false)
      let valStr := match oStx[1].isStrLit? with
        | some s => s
        | none => "0"
      let intVal := parseIntStr valStr
      let rhs := offsetRhs intVal
      eaNames := eaNames.push s!"ea_{oNameStr}"
      eaCmds := eaCmds.push (mkTacticTheorem s!"ea_{oNameStr}" #["(b : Nat)"]
        s!"effectiveAddr b {oNameStr} = {rhs}" s!"unfold effectiveAddr {oNameStr}; omega")

  -- Parse guard blocks (index 12)
  let guardsStx := stx[12]
  let mut guardData : Array (String × Nat × String × ProofMode × Array String × Array String) := #[]
  for g in guardsStx.getArgs do
    let gName := g[1].getId.toString (escape := false)

    let fuelN : Nat := match g[3].isNatLit? with
      | some n => n
      | none => 0

    -- error: can be num or ident
    let errorNode := g[5]
    let errStr := match errorNode.isNatLit? with
      | some n => s!"{n}"
      | none => errorNode.getId.toString (escape := false)

    -- Optional proof annotation (index 6)
    let proofStx := g[6]
    let mut proofMode : ProofMode := .none
    if !proofStx.isMissing && proofStx.getNumArgs > 0 then
      let modeStx := proofStx[1]
      let kindStr := modeStx.getKind.toString
      if kindStr == "token.auto" || (modeStx.isAtom && modeStx.getAtomVal == "auto") then
        proofMode := .auto
      else if kindStr == "token.phased" || (modeStx.isAtom && modeStx.getAtomVal == "phased") then
        -- Parse phases from index 7
        let phasesStx := g[7]
        let mut phaseArr : Array (String × Nat) := #[]
        if !phasesStx.isMissing && phasesStx.getNumArgs > 0 then
          let phaseListStx := phasesStx[1]  -- guardPhase+
          for pStx in phaseListStx.getArgs do
            let pName := pStx[1].getId.toString (escape := false)
            let pFuel := match pStx[2].isNatLit? with
              | some n => n
              | none => 0
            phaseArr := phaseArr.push (pName, pFuel)
        -- Validate fuel sum
        let phaseSum := phaseArr.foldl (fun acc (_, f) => acc + f) 0
        if phaseSum != fuelN then
          throwError m!"qedguards: phase fuels sum to {phaseSum}, expected {fuelN} for guard '{gName}'"
        if phaseArr.size < 2 then
          throwError m!"qedguards: guard '{gName}' needs at least 2 phases"
        proofMode := .phased phaseArr

    -- Optional hyps (index 8, shifted from 7)
    let hypsOpt := g[8]
    let mut gHyps : Array String := #[]
    if !hypsOpt.isMissing && hypsOpt.getNumArgs > 0 then
      let hypListStx := hypsOpt[1]  -- guardHyp*
      for hStx in hypListStx.getArgs do
        match hStx[0].isStrLit? with
        | some s => gHyps := gHyps.push s
        | none => pure ()

    -- Optional after (index 9, shifted from 8)
    let afterOpt := g[9]
    let mut gAfter : Array String := #[]
    if !afterOpt.isMissing && afterOpt.getNumArgs > 0 then
      let afterListStx := afterOpt[1]  -- guardHyp*
      for hStx in afterListStx.getArgs do
        match hStx[0].isStrLit? with
        | some s => gAfter := gAfter.push s
        | none => pure ()

    guardData := guardData.push (gName, fuelN, errStr, proofMode, gHyps, gAfter)

  -- ================================================================
  -- Generate commands
  -- ================================================================
  let mut cmds : Array String := #[]
  let nl := "\n"

  cmds := cmds.push (mkNamespace name)
  cmds := cmds.push (mkOpen "QEDGen.Solana")
  cmds := cmds.push (mkOpen "QEDGen.Solana.SBPF")
  cmds := cmds.push (mkOpen "QEDGen.Solana.SBPF.Memory")

  -- Error code constants
  for (eName, eVal) in errorDecls do
    cmds := cmds.push (mkAbbrev eName "Nat" s!"{eVal}")

  -- Build structure with one field per guard (proof obligations)
  let mut structStr := s!"structure Spec ({progName} : Nat → Option QEDGen.Solana.SBPF.Insn) where"
  let mut accumulated : Array String := #[]

  -- Also collect proof commands (auto + phased)
  let mut proofCmds : Array String := #[]

  for (gName, fuelN, errStr, proofMode, gHyps, gAfter) in guardData do
    -- Collect all binders: params + accumulated after + this guard's hyps
    let mut binderLines : Array String := #[]
    binderLines := binderLines.push s!"∀ {params}"
    for hp in accumulated do
      binderLines := binderLines.push hp
    for hp in gHyps do
      binderLines := binderLines.push hp

    -- Field declaration
    structStr := structStr ++ nl ++ s!"  {gName} :"
    let lastIdx := binderLines.size - 1
    for i in [:binderLines.size] do
      let line := binderLines[i]!
      if i == lastIdx then
        structStr := structStr ++ nl ++ s!"    {line},"
      else
        structStr := structStr ++ nl ++ s!"    {line}"

    -- Conclusion
    structStr := structStr ++ nl ++ s!"    (executeFn {progName} ({initExpr}) {fuelN}).exitCode"
    structStr := structStr ++ nl ++ s!"      = some {errStr}"

    match proofMode with
    | .auto =>
      -- Generate auto proof (wp_exec with hypothesis lifting)
      let allHyps := accumulated ++ gHyps
      let bodyLines := genAutoBody allHyps progName chunkDefs eaNames

      let mut proofStr := s!"set_option maxHeartbeats 800000 in{nl}"
      proofStr := proofStr ++ s!"theorem {gName}"
      proofStr := proofStr ++ nl ++ s!"    {params}"
      for hp in accumulated do
        proofStr := proofStr ++ nl ++ s!"    {hp}"
      for hp in gHyps do
        proofStr := proofStr ++ nl ++ s!"    {hp}"
      proofStr := proofStr ++ s!" :"
      proofStr := proofStr ++ nl ++ s!"    (executeFn {progName} ({initExpr}) {fuelN}).exitCode"
      proofStr := proofStr ++ nl ++ s!"      = some {errStr} := by"
      let bodyStr := nl.intercalate bodyLines.toList
      proofStr := proofStr ++ nl ++ bodyStr

      proofCmds := proofCmds.push proofStr

    | .phased phs =>
      let phaseFuels := phs.map (·.2)
      let fuelSum := buildFuelSum phaseFuels
      let numComposes := phs.size - 1

      -- Build phase decomposition comment
      let phaseDesc := ", ".intercalate (phs.toList.map fun (n, f) => s!"{n} ({f})")

      -- Main composition theorem with executeFn_compose rewrite
      let composeList := List.replicate numComposes "executeFn_compose"
      let composeStr := ", ".intercalate composeList
      let mut mainStr := s!"set_option maxHeartbeats 800000 in{nl}"
      mainStr := mainStr ++ s!"/-- Phased: {phaseDesc} -/{nl}"
      mainStr := mainStr ++ s!"theorem {gName}"
      mainStr := mainStr ++ nl ++ s!"    {params}"
      for hp in accumulated do
        mainStr := mainStr ++ nl ++ s!"    {hp}"
      for hp in gHyps do
        mainStr := mainStr ++ nl ++ s!"    {hp}"
      mainStr := mainStr ++ s!" :"
      mainStr := mainStr ++ nl ++ s!"    (executeFn {progName} ({initExpr}) {fuelN}).exitCode"
      mainStr := mainStr ++ nl ++ s!"      = some {errStr} := by"
      mainStr := mainStr ++ nl ++ s!"  rw [show ({fuelN} : Nat) = {fuelSum} from rfl, {composeStr}]"
      mainStr := mainStr ++ nl ++ s!"  sorry"

      proofCmds := proofCmds.push mainStr

    | .none => pure ()

    -- Add this guard's after-block to the accumulation
    for hp in gAfter do
      accumulated := accumulated.push hp

  cmds := cmds.push structStr

  -- ea_* lemmas
  for eaCmd in eaCmds do
    cmds := cmds.push eaCmd

  -- Proof theorems (auto + phased)
  for proofCmd in proofCmds do
    cmds := cmds.push proofCmd

  cmds := cmds.push (mkEnd name)

  -- Parse and elaborate each command
  let env ← getEnv
  for src in cmds do
    match Lean.Parser.runParserCategory env `command src "<qedguards>" with
    | .error msg =>
      throwError m!"qedguards: failed to parse generated code:{nl}{msg}{nl}{nl}Source:{nl}{src}"
    | .ok cmdStx =>
      elabCommand cmdStx

end QEDGen.Solana.GuardsDSL
