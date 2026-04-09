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
  - Transition functions with signer/lifecycle guards
  - Typed theorem signatures with sorry (access_control, state_machine, cpi, bounds)
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

/-- A CPI account with access flag: `accountName writable` -/
syntax specCpiAcct := rawIdent rawIdent

/-- Operation block (rawIdent allows Lean keywords like `initialize`, `open`) -/
syntax specOp :=
  "operation " rawIdent
    "who: " rawIdent
    "when: " rawIdent
    "then: " rawIdent
    ("calls: " rawIdent rawIdent "(" specCpiAcct,* ")")?

/-- Invariant declaration (untyped — generates `theorem name : True := sorry`) -/
syntax specInvariant := "invariant " rawIdent str

/-- Property declaration with predicate body and preservation scope.
    The string is a Lean `Prop` expression using `s.field` notation.
    `preserved_by:` lists which operations must preserve it. -/
syntax specProperty :=
  "property " rawIdent str
    "preserved_by: " rawIdent,*

/-- The top-level qedspec command. -/
syntax (name := qedspecCmd)
  "qedspec " ident " where"
    "state" specField*
    specOp*
    specInvariant*
    specProperty*
  : command

-- ============================================================================
-- CPI account flag parsing
-- ============================================================================

/-- Parse an account access flag keyword to (isSigner, isWritable).
    Known flags: readonly, writable, signer, signer_writable -/
private def parseFlag (flag : String) : Option (Bool × Bool) :=
  match flag with
  | "readonly"         => some (false, false)
  | "writable"         => some (false, true)
  | "signer"           => some (true, false)
  | "signer_writable"  => some (true, true)
  | _                  => none

-- ============================================================================
-- Elaborator
-- ============================================================================

-- Lean keywords that need «» quoting when used as identifiers
private def leanKeywords : List String :=
  ["initialize", "open", "end", "where", "if", "then", "else", "do",
   "let", "def", "theorem", "structure", "inductive", "namespace",
   "section", "import", "return", "match", "with", "fun", "have",
   "show", "by", "from", "in", "at", "class", "instance", "deriving",
   "variable", "axiom", "opaque", "abbrev", "noncomputable", "partial",
   "unsafe", "private", "protected", "mutual", "set_option", "attribute"]

/-- Quote a name with «» if it's a Lean keyword -/
private def quoteName (n : String) : String :=
  if leanKeywords.contains n then s!"«{n}»" else n

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
@[command_elab qedspecCmd]
def elabQedspec : CommandElab := fun stx => do
  -- Extract pieces from the syntax tree
  -- Layout: "qedspec" ident "where" "state" fields* ops* invs* props*
  let progNameStx := stx[1]
  let name := progNameStx.getId
  let fieldsStx := stx[4]  -- specField* (index 4: after "qedspec" ident "where" "state")
  let opsStx := stx[5]     -- specOp*
  let invsStx := stx[6]    -- specInvariant*
  let propsStx := stx[7]   -- specProperty*

  -- Parse field declarations
  let mut fieldData : Array (String × String) := #[]
  for f in fieldsStx.getArgs do
    let fieldName := quoteName (f[0].getId.toString (escape := false))
    let fieldType := f[2].getId.toString (escape := false)
    fieldData := fieldData.push (fieldName, fieldType)

  -- Collect U64 fields for arithmetic bounds generation
  let u64Fields := fieldData.filter (fun (_, ft) => ft == "U64")

  -- Collect lifecycle states from when/then across all operations
  let mut lifecycleStates : Array String := #[]
  for op in opsStx.getArgs do
    let preStatus := op[5].getId.toString (escape := false)
    let postStatus := op[7].getId.toString (escape := false)
    if !lifecycleStates.contains preStatus then
      lifecycleStates := lifecycleStates.push preStatus
    if !lifecycleStates.contains postStatus then
      lifecycleStates := lifecycleStates.push postStatus

  let hasLifecycle := lifecycleStates.size > 0

  -- Build state structure field source
  let mut structFields := ""
  for (fn_, ft) in fieldData do
    structFields := structFields ++ s!"  {fn_} : {ft}\n"
  if hasLifecycle then
    structFields := structFields ++ s!"  status : Status\n"

  -- Assemble individual command strings to parse and elaborate one at a time
  -- (Lean's runParserCategory `command parses exactly ONE command)
  let mut cmds : Array String := #[]
  cmds := cmds.push s!"namespace {name}"
  cmds := cmds.push s!"open QEDGen.Solana"

  -- Generate Status inductive from when/then values
  if hasLifecycle then
    let variants := lifecycleStates.foldl (fun acc s => acc ++ s!" | {s}") ""
    cmds := cmds.push s!"inductive Status where{variants}\n  deriving Repr, DecidableEq, BEq"

  cmds := cmds.push s!"structure State where\n{structFields}  deriving Repr, DecidableEq, BEq"

  for op in opsStx.getArgs do
    let opNameRaw := op[1].getId.toString (escape := false)
    let opName := quoteName opNameRaw
    let signer := quoteName (op[3].getId.toString (escape := false))
    let preStatus := op[5].getId.toString (escape := false)
    let postStatus := op[7].getId.toString (escape := false)
    let transName := quoteName s!"{opNameRaw}Transition"

    -- Transition function with signer guard + lifecycle guard
    if hasLifecycle then
      cmds := cmds.push (s!"def {transName} (s : State) (signer : Pubkey) : Option State :=\n" ++
        s!"  if signer = s.{signer} ∧ s.status = .{preStatus} then\n" ++
        s!"    some \{ s with status := .{postStatus} }\n" ++
        s!"  else none")
    else
      cmds := cmds.push (s!"def {transName} (s : State) (signer : Pubkey) : Option State :=\n" ++
        s!"  if signer = s.{signer} then sorry\n" ++
        s!"  else none")

    -- Access control theorem
    cmds := cmds.push (s!"theorem {opName}.access_control (s : State) (p : Pubkey)\n" ++
      s!"    (h : {transName} s p ≠ none) :\n" ++
      s!"    p = s.{signer} := sorry")

    -- State machine theorem — typed when lifecycle exists
    if hasLifecycle then
      cmds := cmds.push (s!"theorem {opName}.state_machine (s s' : State) (p : Pubkey)\n" ++
        s!"    (h : {transName} s p = some s') :\n" ++
        s!"    s.status = .{preStatus} ∧ s'.status = .{postStatus} := sorry")
    else
      cmds := cmds.push (s!"theorem {opName}.state_machine (s s' : State) (p : Pubkey)\n" ++
        s!"    (h : {transName} s p = some s') :\n" ++
        s!"    True := sorry")

    -- CPI correctness theorem (if calls: clause present)
    -- specOp with optional CPI: "operation" name "who:" signer "when:" pre "then:" post ("calls:" programId discriminator "(" specCpiAcct,* ")")?
    let cpiStx := op[8]
    if !cpiStx.isMissing && cpiStx.getNumArgs > 0 then
      -- cpiStx layout: "calls:" programId discriminator "(" specCpiAcct,* ")"
      let cpiProgramId := cpiStx[1].getId.toString (escape := false)
      let cpiDiscriminator := cpiStx[2].getId.toString (escape := false)

      -- Parse CPI account declarations (index 4 is the specCpiAcct,* separator node)
      let cpiAcctsStx := cpiStx[4]
      let mut cpiAccounts : Array (String × Bool × Bool) := #[]
      for i in List.range cpiAcctsStx.getArgs.size do
        let arg := cpiAcctsStx.getArgs[i]!
        -- In a separator node, even indices are specCpiAcct values, odd indices are commas
        if i % 2 == 0 then
          let acctName := arg[0].getId.toString (escape := false)
          let flagStr := arg[1].getId.toString (escape := false)
          match parseFlag flagStr with
          | some (isSigner, isWritable) =>
            cpiAccounts := cpiAccounts.push (acctName, isSigner, isWritable)
          | none =>
            throwError m!"qedspec: unknown account flag '{flagStr}' for account '{acctName}'. Use: readonly, writable, signer, signer_writable"

      -- Use raw name for compound identifiers (CpiContext, build_cpi)
      let cpiCtxName := quoteName s!"{opNameRaw}CpiContext"
      let buildCpiName := quoteName s!"{opNameRaw}_build_cpi"

      -- Generate CPI context structure
      let mut ctxFields := ""
      for (acct, _, _) in cpiAccounts do
        ctxFields := ctxFields ++ s!"  {acct} : Pubkey\n"
      cmds := cmds.push s!"structure {cpiCtxName} where\n{ctxFields}  deriving Repr, DecidableEq, BEq"

      -- Generate build_cpi function
      let mut accountsList := ""
      for i in List.range cpiAccounts.size do
        let (acct, isSigner, isWritable) := cpiAccounts[i]!
        if i > 0 then accountsList := accountsList ++ ",\n      "
        accountsList := accountsList ++
          s!"⟨ctx.{acct}, {isSigner}, {isWritable}⟩"

      cmds := cmds.push (
        s!"def {buildCpiName} (ctx : {cpiCtxName}) : CpiInstruction :=\n" ++
        s!"  \{ programId := {cpiProgramId}\n" ++
        s!"  , accounts := [{accountsList}]\n" ++
        s!"  , data := {cpiDiscriminator} }")

      -- Generate cpi_correct theorem
      let mut conjuncts := s!"    targetsProgram cpi {cpiProgramId}"
      for i in List.range cpiAccounts.size do
        let (acct, isSigner, isWritable) := cpiAccounts[i]!
        conjuncts := conjuncts ++ s!" ∧\n    accountAt cpi {i} ctx.{acct} {isSigner} {isWritable}"
      conjuncts := conjuncts ++ s!" ∧\n    hasDiscriminator cpi {cpiDiscriminator}"

      cmds := cmds.push (
        s!"theorem {opName}.cpi_correct (ctx : {cpiCtxName}) :\n" ++
        s!"    let cpi := {buildCpiName} ctx\n" ++
        conjuncts ++ " := sorry")

    -- Arithmetic bounds preservation (for operations with U64 fields)
    if u64Fields.size > 0 then
      let mut boundsConj := ""
      for i in List.range u64Fields.size do
        let (fn_, _) := u64Fields[i]!
        if i > 0 then boundsConj := boundsConj ++ " ∧\n    "
        boundsConj := boundsConj ++ s!"valid_u64 s'.{fn_}"

      let mut preConj := ""
      for i in List.range u64Fields.size do
        let (fn_, _) := u64Fields[i]!
        if i > 0 then preConj := preConj ++ " ∧ "
        preConj := preConj ++ s!"valid_u64 s.{fn_}"

      cmds := cmds.push (
        s!"theorem {opName}.u64_bounds (s s' : State) (p : Pubkey)\n" ++
        s!"    (h_valid : {preConj})\n" ++
        s!"    (h : {transName} s p = some s') :\n" ++
        s!"    {boundsConj} := sorry")

  for inv in invsStx.getArgs do
    let invName := inv[1].getId.toString (escape := false)
    cmds := cmds.push s!"theorem {invName} : True := sorry"

  -- Typed property declarations with preservation theorems
  -- specProperty layout: "property" name predicate-string "preserved_by:" op,*
  for prop in propsStx.getArgs do
    let propName := prop[1].getId.toString (escape := false)
    let predBody := prop[2].isStrLit?.getD ""

    -- Generate predicate definition: def propName (s : State) : Prop := <body>
    cmds := cmds.push s!"def {propName} (s : State) : Prop := {predBody}"

    -- Parse preserved_by operation list (index 4 is the rawIdent,* separator node)
    let preservedByStx := prop[4]
    for i in List.range preservedByStx.getArgs.size do
      let arg := preservedByStx.getArgs[i]!
      if i % 2 == 0 then  -- skip comma separators
        let opNameRaw := arg.getId.toString (escape := false)
        let opName := quoteName opNameRaw
        let transName := quoteName s!"{opNameRaw}Transition"

        cmds := cmds.push (
          s!"theorem {opName}.preserves_{propName} (s s' : State) (p : Pubkey)\n" ++
          s!"    (h_inv : {propName} s)\n" ++
          s!"    (h : {transName} s p = some s') :\n" ++
          s!"    {propName} s' := sorry")

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
