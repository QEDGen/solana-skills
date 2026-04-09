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

## Clauses

- `who:` — signer field (optional: omit for permissionless operations)
- `when:` / `then:` — lifecycle state transitions (optional: omit for lifecycle-less ops)
- `takes:` — operation parameters with DSL types (U64, U128, I128, U8)
- `let:` — computed intermediates (pure `let` bindings before the guard)
- `guard:` — domain-specific constraints as Lean Prop strings
- `effect:` — structured state mutations: `field add/sub param`
- `calls:` — CPI instruction declarations
- `property` — named predicates with preservation scope
- `account` — sub-structures embedded in the main State

## Type mapping

DSL types are mapped to Lean types in generated code for omega compatibility:
  - U64, U128, U8 → Nat
  - I128 → Int
  - Other types (Pubkey, custom) pass through unchanged

## Effect syntax

Effects use structured assignments validated against the state declaration:
  - `field add param` → `field := s.field + param`
  - `field sub param` → `field := s.field - param`

`sub` effects auto-generate an underflow guard (`param ≤ s.field`)
for Nat-typed fields. Int fields (I128) skip the guard since Int
subtraction is total.

Field and param names are validated at elaboration time — typos fail fast.
Guard and property strings are also validated for `s.FIELD` references.

## Out of scope (intentionally deferred)

The following patterns cannot be expressed in the current DSL:
  - **Multi-account operations**: Creating/closing accounts (array state changes)
  - **Aggregates**: Sum/product over collections (e.g., sum of all user balances)
  - **Multi-step compositions**: Sequential transition composition with intermediate assertions
  - **Cross-program invariants**: Properties spanning multiple programs
  - **Dynamic account sets**: Variable-length account arrays in state
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

/-- Operation parameter: `paramName Type` -/
syntax specParam := rawIdent rawIdent

/-- Structured effect assignment: `field add param` or `field sub param`.
    Validated against state fields and takes parameters at elaboration time.
    `sub` auto-generates an underflow guard. -/
syntax specEffectAssign := rawIdent rawIdent rawIdent

/-- Let binding for computed intermediates: `let: varName "expression"` -/
syntax specLet := rawIdent str

/-- Operation block (rawIdent allows Lean keywords like `initialize`, `open`).
    `who:`, `when:`, `then:` are optional — omit for signer-less or lifecycle-less operations. -/
syntax specOp :=
  "operation " rawIdent
    ("who: " rawIdent)?
    ("when: " rawIdent)?
    ("then: " rawIdent)?
    ("takes: " specParam,*)?
    ("let: " specLet,*)?
    ("guard: " str)?
    ("effect: " specEffectAssign,*)?
    ("calls: " rawIdent rawIdent "(" specCpiAcct,* ")")?

/-- Invariant declaration (untyped — generates `theorem name : True := sorry`) -/
syntax specInvariant := "invariant " rawIdent str

/-- Property declaration with predicate body and preservation scope.
    The string is a Lean `Prop` expression using `s.field` notation.
    `preserved_by:` lists which operations must preserve it. -/
syntax specProperty :=
  "property " rawIdent str
    "preserved_by: " rawIdent,*

/-- Account block: generates a separate structure alongside State.
    The main State gets a field of this type automatically. -/
syntax specAccount := "account " rawIdent specField*

/-- The top-level qedspec command. -/
syntax (name := qedspecCmd)
  "qedspec " ident " where"
    "state" specField*
    specAccount*
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

/-- Map DSL types to Lean types for omega compatibility.
    U64/U128/U8 → Nat (so omega works directly), I128 → Int.
    Other types (Pubkey, custom) pass through unchanged. -/
private def mapDslType (t : String) : String :=
  match t with
  | "U64"  => "Nat"
  | "U128" => "Nat"
  | "I128" => "Int"
  | "U8"   => "Nat"
  | _      => t

/-- Validate that `s.FIELD` references in a string expression correspond to
    declared state fields. Catches typos at elaboration time. -/
private def validateFieldRefs (expr : String) (fields : Array (String × String))
    (context : String) : Lean.Elab.Command.CommandElabM Unit := do
  let parts := expr.splitOn "s."
  -- Skip parts[0] (before first "s."), check each subsequent occurrence
  for i in List.range (parts.length - 1) do
    let rest := parts[i + 1]!
    let fieldRef := rest.takeWhile (fun c => c.isAlphanum || c == '_')
    if !fieldRef.isEmpty then
      let qRef := quoteName fieldRef
      if !fields.any (fun (fn_, _) => fn_ == qRef) then
        Lean.throwError m!"qedspec: {context} references unknown field 's.{fieldRef}'. Available: {fields.map (·.1)}"

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
@[command_elab qedspecCmd]
def elabQedspec : CommandElab := fun stx => do
  -- Extract pieces from the syntax tree
  -- Layout: "qedspec" ident "where" "state" fields* accounts* ops* invs* props*
  let progNameStx := stx[1]
  let name := progNameStx.getId
  let fieldsStx := stx[4]  -- specField* (index 4: after "qedspec" ident "where" "state")
  let accountsStx := stx[5] -- specAccount*
  let opsStx := stx[6]     -- specOp*
  let invsStx := stx[7]    -- specInvariant*
  let propsStx := stx[8]   -- specProperty*

  -- Parse field declarations
  let mut fieldData : Array (String × String) := #[]
  for f in fieldsStx.getArgs do
    let fieldName := quoteName (f[0].getId.toString (escape := false))
    let fieldType := f[2].getId.toString (escape := false)
    fieldData := fieldData.push (fieldName, fieldType)

  -- Parse account blocks: each generates a separate structure
  let mut accountData : Array (String × Array (String × String)) := #[]
  for acct in accountsStx.getArgs do
    let acctName := acct[1].getId.toString (escape := false)
    let mut acctFields : Array (String × String) := #[]
    -- Account fields are in the repetition node at index 2
    let acctFieldsStx := acct[2]
    for f in acctFieldsStx.getArgs do
      let fn_ := quoteName (f[0].getId.toString (escape := false))
      let ft := f[2].getId.toString (escape := false)
      acctFields := acctFields.push (fn_, ft)
    accountData := accountData.push (acctName, acctFields)
    -- Add account as a field of the main State
    fieldData := fieldData.push (acctName, acctName)

  -- Collect U64 fields for arithmetic bounds generation
  let u64Fields := fieldData.filter (fun (_, ft) => ft == "U64")

  -- Collect lifecycle states from when/then across all operations
  -- (op[3] = when?, op[4] = then? — both optional)
  let mut lifecycleStates : Array String := #[]
  for op in opsStx.getArgs do
    let whenStx := op[3]
    if !whenStx.isMissing && whenStx.getNumArgs > 0 then
      let preStatus := whenStx[1].getId.toString (escape := false)
      if !lifecycleStates.contains preStatus then
        lifecycleStates := lifecycleStates.push preStatus
    let thenStx := op[4]
    if !thenStx.isMissing && thenStx.getNumArgs > 0 then
      let postStatus := thenStx[1].getId.toString (escape := false)
      if !lifecycleStates.contains postStatus then
        lifecycleStates := lifecycleStates.push postStatus

  let hasLifecycle := lifecycleStates.size > 0

  -- Build state structure field source (map DSL types → Lean types for omega)
  let mut structFields := ""
  for (fn_, ft) in fieldData do
    let leanType := mapDslType ft
    structFields := structFields ++ s!"  {fn_} : {leanType}\n"
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

  -- Generate account structures (before State, since State references them)
  for (acctName, acctFields) in accountData do
    let mut acctStructFields := ""
    for (fn_, ft) in acctFields do
      let leanType := mapDslType ft
      acctStructFields := acctStructFields ++ s!"  {fn_} : {leanType}\n"
    cmds := cmds.push s!"structure {acctName} where\n{acctStructFields}  deriving Repr, DecidableEq, BEq"

  cmds := cmds.push s!"structure State where\n{structFields}  deriving Repr, DecidableEq, BEq"

  -- Track per-operation parameters so property preservation theorems can reference them
  let mut opParamsMap : Array (String × Array (String × String)) := #[]

  for op in opsStx.getArgs do
    let opNameRaw := op[1].getId.toString (escape := false)
    let opName := quoteName opNameRaw
    let transName := quoteName s!"{opNameRaw}Transition"

    -- ----------------------------------------------------------------
    -- Parse optional who:/when:/then: clauses (op[2], op[3], op[4])
    -- ----------------------------------------------------------------
    let whoStx := op[2]
    let hasSigner := !whoStx.isMissing && whoStx.getNumArgs > 0
    let signer := if hasSigner then quoteName (whoStx[1].getId.toString (escape := false)) else ""

    let whenStx := op[3]
    let hasWhen := !whenStx.isMissing && whenStx.getNumArgs > 0
    let preStatus := if hasWhen then whenStx[1].getId.toString (escape := false) else ""

    let thenStx := op[4]
    let hasThen := !thenStx.isMissing && thenStx.getNumArgs > 0
    let postStatus := if hasThen then thenStx[1].getId.toString (escape := false) else ""

    -- ----------------------------------------------------------------
    -- Parse optional takes: clause (op[5])
    -- ----------------------------------------------------------------
    let takesStx := op[5]
    let mut params : Array (String × String) := #[]
    if !takesStx.isMissing && takesStx.getNumArgs > 0 then
      let paramsSepStx := takesStx[1]  -- specParam,* separator node
      for i in List.range paramsSepStx.getArgs.size do
        let arg := paramsSepStx.getArgs[i]!
        if i % 2 == 0 then  -- skip comma separators
          let pName := arg[0].getId.toString (escape := false)
          let pType := arg[1].getId.toString (escape := false)
          params := params.push (pName, pType)

    -- Save params for this operation (used by property preservation theorems)
    opParamsMap := opParamsMap.push (opNameRaw, params)

    -- Build param strings for function signatures and theorem calls
    -- Map DSL types (U64 etc.) to Lean types (Nat etc.) for omega compatibility
    let paramSig := params.foldl (fun acc (pn, pt) => acc ++ s!" ({pn} : {mapDslType pt})") ""
    let paramArgs := params.foldl (fun acc (pn, _) => acc ++ s!" {pn}") ""

    -- ----------------------------------------------------------------
    -- Parse optional let: clause (op[6])
    -- ----------------------------------------------------------------
    let letStx := op[6]
    let mut letBindings : Array (String × String) := #[]
    if !letStx.isMissing && letStx.getNumArgs > 0 then
      let letsSepStx := letStx[1]  -- specLet,* separator node
      for i in List.range letsSepStx.getArgs.size do
        let arg := letsSepStx.getArgs[i]!
        if i % 2 == 0 then  -- skip comma separators
          let letName := arg[0].getId.toString (escape := false)
          let letExpr := arg[1].isStrLit?.getD ""
          letBindings := letBindings.push (letName, letExpr)

    -- ----------------------------------------------------------------
    -- Parse optional guard: clause (op[7])
    -- ----------------------------------------------------------------
    let guardStx := op[7]
    let guardStr := if !guardStx.isMissing && guardStx.getNumArgs > 0 then
      guardStx[1].isStrLit?.getD ""
    else ""

    -- Validate field references in guard string
    if !guardStr.isEmpty then
      validateFieldRefs guardStr fieldData s!"guard in operation '{opNameRaw}'"

    -- ----------------------------------------------------------------
    -- Parse optional effect: clause (op[8])
    -- Structured: `field add param` or `field sub param`
    -- ----------------------------------------------------------------
    let effectStx := op[8]
    let mut effectAssigns : Array String := #[]
    let mut autoGuards : Array String := #[]

    if !effectStx.isMissing && effectStx.getNumArgs > 0 then
      let assignsSepStx := effectStx[1]  -- specEffectAssign,* separator node
      for i in List.range assignsSepStx.getArgs.size do
        let arg := assignsSepStx.getArgs[i]!
        if i % 2 == 0 then  -- skip comma separators
          let effectField := arg[0].getId.toString (escape := false)
          let effectOp := arg[1].getId.toString (escape := false)
          let effectValue := arg[2].getId.toString (escape := false)

          -- Validate operator
          if effectOp != "add" && effectOp != "sub" then
            throwError m!"qedspec: effect operator must be 'add' or 'sub', got '{effectOp}' in operation '{opNameRaw}'"

          -- Validate field exists in state
          let qField := quoteName effectField
          if !fieldData.any (fun (fn_, _) => fn_ == qField) then
            throwError m!"qedspec: effect field '{effectField}' not found in state. Available fields: {fieldData.map (·.1)}"

          -- Validate value exists in takes params or state fields
          let qValue := quoteName effectValue
          if !params.any (fun (pn, _) => pn == effectValue) &&
             !fieldData.any (fun (fn_, _) => fn_ == qValue) then
            throwError m!"qedspec: effect value '{effectValue}' not found in 'takes:' parameters or state fields for operation '{opNameRaw}'"

          -- Look up DSL type for this field (Int subtraction is total — no guard needed)
          let fieldDslType := match fieldData.find? (fun (fn_, _) => fn_ == qField) with
            | some (_, ft) => ft
            | none => ""
          let isIntField := mapDslType fieldDslType == "Int"

          -- Generate assignment string
          if effectOp == "add" then
            effectAssigns := effectAssigns.push s!"{qField} := s.{qField} + {effectValue}"
          else
            effectAssigns := effectAssigns.push s!"{qField} := s.{qField} - {effectValue}"
            -- Auto-generate underflow guard for sub (skip for Int fields — subtraction is total)
            if !isIntField then
              autoGuards := autoGuards.push s!"{effectValue} ≤ s.{qField}"

    let hasEffect := effectAssigns.size > 0

    -- ----------------------------------------------------------------
    -- Build condition parts: signer + lifecycle + guards
    -- ----------------------------------------------------------------
    let mut condParts : Array String := #[]
    if hasSigner then
      condParts := condParts.push s!"signer = s.{signer}"
    if hasWhen then
      condParts := condParts.push s!"s.status = .{preStatus}"
    for g in autoGuards do
      condParts := condParts.push g
    if !guardStr.isEmpty then
      condParts := condParts.push guardStr

    let hasCond := condParts.size > 0
    let ifCond := condParts.foldl (fun acc p =>
      if acc.isEmpty then p else acc ++ s!" ∧ {p}") ""

    -- ----------------------------------------------------------------
    -- Build result state
    -- ----------------------------------------------------------------
    let mut withParts : Array String := #[]
    for a in effectAssigns do
      withParts := withParts.push a
    if hasThen then
      withParts := withParts.push s!"status := .{postStatus}"

    let thenBody :=
      if withParts.isEmpty then
        "some s"
      else
        let assigns := withParts.foldl (fun acc a =>
          if acc.isEmpty then a else acc ++ s!", {a}") ""
        s!"some \{ s with {assigns} }"

    -- ----------------------------------------------------------------
    -- Generate transition function
    -- ----------------------------------------------------------------
    let letPrefix := letBindings.foldl (fun acc (ln, le) =>
      acc ++ s!"  let {ln} := {le}\n") ""

    if hasCond then
      cmds := cmds.push (s!"def {transName} (s : State) (signer : Pubkey){paramSig} : Option State :=\n" ++
        letPrefix ++
        s!"  if {ifCond} then\n" ++
        s!"    {thenBody}\n" ++
        s!"  else none")
    else
      cmds := cmds.push (s!"def {transName} (s : State) (signer : Pubkey){paramSig} : Option State :=\n" ++
        letPrefix ++
        s!"  {thenBody}")

    -- ----------------------------------------------------------------
    -- Access control theorem (only when who: is specified)
    -- ----------------------------------------------------------------
    if hasSigner then
      cmds := cmds.push (s!"theorem {opName}.access_control (s : State) (p : Pubkey){paramSig}\n" ++
        s!"    (h : {transName} s p{paramArgs} ≠ none) :\n" ++
        s!"    p = s.{signer} := sorry")

    -- ----------------------------------------------------------------
    -- State machine theorem (only when when:/then: specified)
    -- ----------------------------------------------------------------
    if hasWhen || hasThen then
      let mut smParts : Array String := #[]
      if hasWhen then smParts := smParts.push s!"s.status = .{preStatus}"
      if hasThen then smParts := smParts.push s!"s'.status = .{postStatus}"
      let smConc := smParts.foldl (fun acc p =>
        if acc.isEmpty then p else acc ++ s!" ∧ {p}") ""
      cmds := cmds.push (s!"theorem {opName}.state_machine (s s' : State) (p : Pubkey){paramSig}\n" ++
        s!"    (h : {transName} s p{paramArgs} = some s') :\n" ++
        s!"    {smConc} := sorry")

    -- ----------------------------------------------------------------
    -- CPI correctness theorem (if calls: clause present)
    -- op[9]: calls clause (after operation name[1] who?[2] when?[3] then?[4]
    --         takes?[5] let?[6] guard?[7] effect?[8])
    -- ----------------------------------------------------------------
    let cpiStx := op[9]
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

    -- ----------------------------------------------------------------
    -- Arithmetic bounds preservation (for operations with U64 fields)
    -- ----------------------------------------------------------------
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
        s!"theorem {opName}.u64_bounds (s s' : State) (p : Pubkey){paramSig}\n" ++
        s!"    (h_valid : {preConj})\n" ++
        s!"    (h : {transName} s p{paramArgs} = some s') :\n" ++
        s!"    {boundsConj} := sorry")

  for inv in invsStx.getArgs do
    let invName := inv[1].getId.toString (escape := false)
    cmds := cmds.push s!"theorem {invName} : True := sorry"

  -- Typed property declarations with preservation theorems
  -- specProperty layout: "property" name predicate-string "preserved_by:" op,*
  for prop in propsStx.getArgs do
    let propName := prop[1].getId.toString (escape := false)
    let predBody := prop[2].isStrLit?.getD ""

    -- Validate field references in property predicate
    if !predBody.isEmpty then
      validateFieldRefs predBody fieldData s!"property '{propName}'"

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

        -- Look up params for this operation
        let opParams := match opParamsMap.find? (fun (n, _) => n == opNameRaw) with
          | some (_, ps) => ps
          | none => #[]
        let paramSig := opParams.foldl (fun acc (pn, pt) => acc ++ s!" ({pn} : {mapDslType pt})") ""
        let paramArgs := opParams.foldl (fun acc (pn, _) => acc ++ s!" {pn}") ""

        cmds := cmds.push (
          s!"theorem {opName}.preserves_{propName} (s s' : State) (p : Pubkey){paramSig}\n" ++
          s!"    (h_inv : {propName} s)\n" ++
          s!"    (h : {transName} s p{paramArgs} = some s') :\n" ++
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
