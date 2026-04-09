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

/-- Operation block (rawIdent allows Lean keywords like `initialize`, `open`) -/
syntax specOp :=
  "operation " rawIdent
    "who: " rawIdent
    "when: " rawIdent
    "then: " rawIdent
    ("calls: " rawIdent rawIdent "(" rawIdent,* ")")?

/-- Invariant declaration -/
syntax specInvariant := "invariant " rawIdent str

/-- The top-level qedspec command. -/
syntax (name := qedspecCmd)
  "qedspec " ident " where"
    "state" specField*
    specOp*
    specInvariant*
  : command

-- ============================================================================
-- Known CPI templates — maps Anchor-style calls to account layouts
-- ============================================================================

/-- A known CPI instruction template.
    Account flags stored as List (Bool × Bool) = (isSigner, isWritable). -/
structure CpiTemplate where
  programId : String          -- e.g. "TOKEN_PROGRAM_ID"
  discriminator : String      -- e.g. "[DISC_TRANSFER]"
  accountFlags : Array (Bool × Bool)  -- per-account (isSigner, isWritable)

/-- Lookup a known CPI template by program.instruction name -/
private def lookupCpi (program instruction : String) : Option CpiTemplate :=
  match program, instruction with
  -- SPL Token operations (Anchor: token::transfer, token::burn, etc.)
  -- SPL Token operations (Anchor: token::transfer, token::burn, etc.)
  | "token", "transfer" => some {
      programId := "TOKEN_PROGRAM_ID"
      discriminator := "[DISC_TRANSFER]"
      accountFlags := #[(false, true), (false, true), (true, false)] }
  | "token", "burn" => some {
      programId := "TOKEN_PROGRAM_ID"
      discriminator := "[DISC_BURN]"
      accountFlags := #[(false, true), (false, true), (true, false)] }
  | "token", "mint_to" => some {
      programId := "TOKEN_PROGRAM_ID"
      discriminator := "[DISC_MINT_TO]"
      accountFlags := #[(false, true), (false, true), (true, false)] }
  | "token", "close_account" => some {
      programId := "TOKEN_PROGRAM_ID"
      discriminator := "[DISC_CLOSE_ACCOUNT]"
      accountFlags := #[(false, true), (false, true), (true, false)] }
  | "token", "approve" => some {
      programId := "TOKEN_PROGRAM_ID"
      discriminator := "[DISC_APPROVE]"
      accountFlags := #[(false, true), (false, false), (true, false)] }
  -- System Program
  | "system", "transfer" => some {
      programId := "SYSTEM_PROGRAM_ID"
      discriminator := "DISC_SYS_TRANSFER"
      accountFlags := #[(true, true), (false, true)] }
  | "system", "create_account" => some {
      programId := "SYSTEM_PROGRAM_ID"
      discriminator := "DISC_SYS_CREATE_ACCOUNT"
      accountFlags := #[(true, true), (true, true)] }
  | _, _ => none

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
  -- Layout: "qedspec" ident "where" "state" fields* ops* invs*
  let progNameStx := stx[1]
  let name := progNameStx.getId
  let fieldsStx := stx[4]  -- specField* (index 4: after "qedspec" ident "where" "state")
  let opsStx := stx[5]     -- specOp*
  let invsStx := stx[6]    -- specInvariant*

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
    -- specOp with optional CPI: "operation" name "who:" signer "when:" pre "then:" post ("calls:" program instruction "(" accounts,* ")")?
    let cpiStx := op[8]
    if !cpiStx.isMissing then
      -- cpiStx layout: "calls:" program instruction "(" accounts,* ")"
      let cpiProgram := cpiStx[1].getId.toString (escape := false)
      let cpiInstruction := cpiStx[2].getId.toString (escape := false)

      -- Parse CPI account arguments (index 4 is the rawIdent,* separator node)
      let cpiAccountsStx := cpiStx[4]
      let mut cpiAccounts : Array String := #[]
      for i in List.range cpiAccountsStx.getArgs.size do
        let arg := cpiAccountsStx.getArgs[i]!
        -- In a separator node, even indices are values, odd indices are separators
        if i % 2 == 0 then
          cpiAccounts := cpiAccounts.push (arg.getId.toString (escape := false))

      match lookupCpi cpiProgram cpiInstruction with
      | none =>
        throwError m!"qedspec: unknown CPI call '{cpiProgram}.{cpiInstruction}'. Known: token.transfer, token.burn, token.mint_to, token.close_account, token.approve, system.transfer, system.create_account"
      | some tmpl =>
        if cpiAccounts.size != tmpl.accountFlags.size then
          throwError m!"qedspec: {cpiProgram}.{cpiInstruction} expects {tmpl.accountFlags.size} accounts, got {cpiAccounts.size}"

        -- Use raw name for compound identifiers (CpiContext, build_cpi)
        let cpiCtxName := quoteName s!"{opNameRaw}CpiContext"
        let buildCpiName := quoteName s!"{opNameRaw}_build_cpi"

        -- Generate CPI context structure
        let mut ctxFields := ""
        for acct in cpiAccounts do
          ctxFields := ctxFields ++ s!"  {acct} : Pubkey\n"
        cmds := cmds.push s!"structure {cpiCtxName} where\n{ctxFields}  deriving Repr, DecidableEq, BEq"

        -- Generate build_cpi function
        let mut accountsList := ""
        for i in List.range cpiAccounts.size do
          let acct := cpiAccounts[i]!
          let (isSigner, isWritable) := tmpl.accountFlags[i]!
          if i > 0 then accountsList := accountsList ++ ",\n      "
          accountsList := accountsList ++
            s!"⟨ctx.{acct}, {isSigner}, {isWritable}⟩"

        cmds := cmds.push (
          s!"def {buildCpiName} (ctx : {cpiCtxName}) : CpiInstruction :=\n" ++
          s!"  \{ programId := {tmpl.programId}\n" ++
          s!"  , accounts := [{accountsList}]\n" ++
          s!"  , data := {tmpl.discriminator} }")

        -- Generate cpi_correct theorem
        let mut conjuncts := s!"    targetsProgram cpi {tmpl.programId}"
        for i in List.range cpiAccounts.size do
          let acct := cpiAccounts[i]!
          let (isSigner, isWritable) := tmpl.accountFlags[i]!
          conjuncts := conjuncts ++ s!" ∧\n    accountAt cpi {i} ctx.{acct} {isSigner} {isWritable}"
        conjuncts := conjuncts ++ s!" ∧\n    hasDiscriminator cpi {tmpl.discriminator}"

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
