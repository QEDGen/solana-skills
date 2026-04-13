import QEDGen.Solana.Account
import QEDGen.Solana.CommandBuilders
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
  - Operation inductive type with `applyOp` dispatcher
  - Per-property inductive preservation theorems (one sorry per property)
  - CPI correctness theorems (structural, typically pure rfl)
  - Invariant theorem stubs

Per-operation proofs (access_control, state_machine, u64_bounds, per-op
preservation) are NOT generated — Kani and unit tests cover those.
Lean focuses on inductive/compositional properties that require reasoning
over arbitrary operation sequences.

## Clauses

- `who:` — signer field (optional: omit for permissionless operations)
- `when:` / `then:` — lifecycle state transitions (optional: omit for lifecycle-less ops)
- `takes:` — operation parameters with DSL types (U64, U128, I128, U8)
- `let:` — computed intermediates (pure `let` bindings before the guard)
- `guard:` — domain-specific constraints as Lean Prop strings
- `effect:` — structured state mutations: `field add/sub param`
- `calls:` — CPI instruction declarations
- `emits:` — event names (Quasar codegen only, ignored by Lean elaborator)
- `context:` — account context block (Quasar codegen only, ignored by Lean elaborator)
- `property` — named predicates with preservation scope
- `account` — sub-structures embedded in the main State

## Top-level Quasar-only clauses (accepted but ignored by Lean elaborator)

- `program_id:` — on-chain program address string
- `pda` — PDA seed declarations
- `event` — event structure declarations
- `errors` — error name declarations

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

/-- PDA seed: either a field reference or a string literal -/
syntax specPdaSeed := rawIdent <|> str

/-- PDA declaration: `pda name seed1, seed2, ...` -/
syntax specPdaDecl := "pda " rawIdent specPdaSeed,*

/-- Event field declaration: `fieldName : FieldType` -/
syntax specEventField := rawIdent " : " rawIdent

/-- Event declaration with braces: `event Name { field : Type, ... }`
    Braces delimit field list to prevent greedy consumption of subsequent sections. -/
syntax specEventDecl := "event " rawIdent "{" specEventField,* "}"

/-- Context attribute: bare keyword or keyword(argument).
    Examples: `mut`, `init`, `payer(maker)`, `seeds(escrow)`, `bump` -/
syntax specCtxAttr := rawIdent ("(" rawIdent ")")?

/-- Context entry: `name : Attr, Attr, ...`
    First attr is always the type (e.g., Signer, Account). -/
syntax specCtxEntry := rawIdent " : " specCtxAttr,+

/-- Operation block (rawIdent allows Lean keywords like `initialize`, `open`).
    `who:`, `when:`, `then:` are optional — omit for signer-less or lifecycle-less operations.
    `doc:` provides a human-readable intent description attached to generated theorems.
    `emits:` and `context:` are Quasar-only (accepted but ignored by Lean elaborator). -/
syntax specOp :=
  "operation " rawIdent
    ("doc: " str)?
    ("who: " rawIdent)?
    ("when: " rawIdent)?
    ("then: " rawIdent)?
    ("takes: " specParam,*)?
    ("let: " specLet,*)?
    ("guard: " str)?
    ("effect: " specEffectAssign,*)?
    ("calls: " rawIdent rawIdent "(" specCpiAcct,* ")")?
    ("emits: " rawIdent,*)?
    ("context:" "{" specCtxEntry* "}")?

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

/-- The top-level qedspec command.
    `program_id:`, `pda`, `event`, and `errors` are Quasar-only
    (accepted but ignored by the Lean elaborator). -/
syntax (name := qedspecCmd)
  "qedspec " ident " where"
    ("program_id: " str)?
    "state" specField*
    specAccount*
    specPdaDecl*
    specEventDecl*
    ("errors: " rawIdent,*)?
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

-- Use CommandBuilders for safe string construction
open QEDGen.Solana.CommandBuilders in
private def quoteName := safeName
open QEDGen.Solana.CommandBuilders in
private def mapDslType := mapType

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
open QEDGen.Solana.CommandBuilders in
@[command_elab qedspecCmd]
def elabQedspec : CommandElab := fun stx => do
  -- Extract pieces from the syntax tree
  -- Layout: "qedspec" ident "where" program_id? "state" fields* accounts*
  --         pdas* events* errors? ops* invs* props*
  -- Indices: 0       1     2      3            4       5       6
  --          7     8       9       10   11    12
  let progNameStx := stx[1]
  let name := progNameStx.getId
  -- stx[3] = ("program_id:" str)? — Quasar-only, ignored
  let _programIdStx := stx[3]
  let fieldsStx := stx[5]      -- specField* (index 5: after "qedspec" ident "where" program_id? "state")
  let accountsStx := stx[6]    -- specAccount*
  -- stx[7] = specPdaDecl*     — Quasar-only, ignored
  let _pdasStx := stx[7]
  -- stx[8] = specEventDecl*   — Quasar-only, ignored
  let _eventsStx := stx[8]
  -- stx[9] = ("errors:" rawIdent,*)?   — Quasar-only, ignored
  let _errorsStx := stx[9]
  let opsStx := stx[10]        -- specOp*
  let invsStx := stx[11]       -- specInvariant*
  let propsStx := stx[12]      -- specProperty*

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
  -- (op[2] = doc?, op[3] = who?, op[4] = when?, op[5] = then?)
  let mut lifecycleStates : Array String := #[]
  for op in opsStx.getArgs do
    let whenStx := op[4]
    if !whenStx.isMissing && whenStx.getNumArgs > 0 then
      let preStatus := whenStx[1].getId.toString (escape := false)
      if !lifecycleStates.contains preStatus then
        lifecycleStates := lifecycleStates.push preStatus
    let thenStx := op[5]
    if !thenStx.isMissing && thenStx.getNumArgs > 0 then
      let postStatus := thenStx[1].getId.toString (escape := false)
      if !lifecycleStates.contains postStatus then
        lifecycleStates := lifecycleStates.push postStatus

  let hasLifecycle := lifecycleStates.size > 0

  -- Build state field list (add lifecycle status if any when/then clauses exist)
  let mut stateFields := fieldData
  if hasLifecycle then
    stateFields := stateFields.push ("status", "Status")

  -- Assemble individual command strings to parse and elaborate one at a time
  -- (Lean's runParserCategory `command parses exactly ONE command)
  let mut cmds : Array String := #[]
  cmds := cmds.push (mkNamespace s!"{name}")
  cmds := cmds.push (mkOpen "QEDGen.Solana")

  -- Generate Status inductive from when/then values
  if hasLifecycle then
    cmds := cmds.push (mkInductive "Status" lifecycleStates)

  -- Generate account structures (before State, since State references them)
  for (acctName, acctFields) in accountData do
    cmds := cmds.push (mkStructure acctName acctFields)

  cmds := cmds.push (mkStructure "State" stateFields)

  -- Track per-operation parameters so property preservation theorems can reference them
  let mut opParamsMap : Array (String × Array (String × String)) := #[]

  -- Collect assumptions for the module doc block
  let mut assumptions : Array String := #[]
  if u64Fields.size > 0 then
    let u64Names := ", ".intercalate (u64Fields.map (·.1)).toList
    assumptions := assumptions.push s!"**U64 bounds tracking**: Fields [{u64Names}] are tracked for overflow/underflow safety."
  if hasLifecycle then
    let states := ", ".intercalate lifecycleStates.toList
    assumptions := assumptions.push s!"**Lifecycle states**: {states}."

  for op in opsStx.getArgs do
    let opNameRaw := op[1].getId.toString (escape := false)
    let opName := quoteName opNameRaw
    let transName := quoteName s!"{opNameRaw}Transition"

    -- ----------------------------------------------------------------
    -- Parse optional doc: clause (op[2])
    -- ----------------------------------------------------------------
    let docStx := op[2]
    let docStr := if !docStx.isMissing && docStx.getNumArgs > 0 then
      docStx[1].isStrLit?.getD ""
    else ""

    -- ----------------------------------------------------------------
    -- Parse optional who:/when:/then: clauses (op[3], op[4], op[5])
    -- ----------------------------------------------------------------
    let whoStx := op[3]
    let hasSigner := !whoStx.isMissing && whoStx.getNumArgs > 0
    let signer := if hasSigner then quoteName (whoStx[1].getId.toString (escape := false)) else ""

    let whenStx := op[4]
    let hasWhen := !whenStx.isMissing && whenStx.getNumArgs > 0
    let preStatus := if hasWhen then whenStx[1].getId.toString (escape := false) else ""

    let thenStx := op[5]
    let hasThen := !thenStx.isMissing && thenStx.getNumArgs > 0
    let postStatus := if hasThen then thenStx[1].getId.toString (escape := false) else ""

    -- ----------------------------------------------------------------
    -- Parse optional takes: clause (op[6])
    -- ----------------------------------------------------------------
    let takesStx := op[6]
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
    let paramSig := mkParamSig params
    let paramArgs := mkParamArgs params

    -- ----------------------------------------------------------------
    -- Parse optional let: clause (op[7])
    -- ----------------------------------------------------------------
    let letStx := op[7]
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
    -- Parse optional guard: clause (op[8])
    -- ----------------------------------------------------------------
    let guardStx := op[8]
    let guardStr := if !guardStx.isMissing && guardStx.getNumArgs > 0 then
      guardStx[1].isStrLit?.getD ""
    else ""

    -- Validate field references in guard string
    if !guardStr.isEmpty then
      validateFieldRefs guardStr fieldData s!"guard in operation '{opNameRaw}'"

    -- ----------------------------------------------------------------
    -- Parse optional effect: clause (op[9])
    -- Structured: `field add param` or `field sub param`
    -- ----------------------------------------------------------------
    let effectStx := op[9]
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
    let ifCond := mkConj condParts

    -- Collect per-operation assumptions
    if hasSigner then
      assumptions := assumptions.push s!"**Signer**: `{opNameRaw}` checks `signer = s.{signer}` (from `who: {signer}`)."
    if hasWhen then
      assumptions := assumptions.push s!"**Lifecycle**: `{opNameRaw}` requires `s.status = .{preStatus}` → `.{postStatus}`."
    for g in autoGuards do
      assumptions := assumptions.push s!"**Auto-guard**: `{opNameRaw}` has auto-generated underflow guard `{g}` from `effect: sub`."

    -- ----------------------------------------------------------------
    -- Build result state
    -- ----------------------------------------------------------------
    let mut withParts : Array String := #[]
    for a in effectAssigns do
      withParts := withParts.push a
    if hasThen then
      withParts := withParts.push s!"status := .{postStatus}"

    let thenBody := mkSomeUpdate "s" withParts

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
    -- CPI correctness theorem (if calls: clause present)
    -- Structural proof — unique to Lean, typically pure rfl.
    -- op[10]: calls clause (after operation name[1] doc?[2] who?[3] when?[4]
    --          then?[5] takes?[6] let?[7] guard?[8] effect?[9])
    -- ----------------------------------------------------------------
    let cpiStx := op[10]
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
      let ctxFieldPairs := cpiAccounts.map fun (acct, _, _) => (acct, "Pubkey")
      cmds := cmds.push (mkStructure cpiCtxName ctxFieldPairs)

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
      let mut cpiParts : Array String := #[s!"targetsProgram cpi {cpiProgramId}"]
      for i in List.range cpiAccounts.size do
        let (acct, isSigner, isWritable) := cpiAccounts[i]!
        cpiParts := cpiParts.push s!"accountAt cpi {i} ctx.{acct} {isSigner} {isWritable}"
      cpiParts := cpiParts.push s!"hasDiscriminator cpi {cpiDiscriminator}"
      let cpiConc := s!"let cpi := {buildCpiName} ctx\n    " ++ mkConj cpiParts
      let cpiDoc := s!"{opNameRaw} CPI targets {cpiProgramId} with correct accounts and discriminator."
      cmds := cmds.push (mkDocSorryTheorem s!"{opName}.cpi_correct" #[s!"(ctx : {cpiCtxName})"] cpiConc cpiDoc)

    -- op[11] = emits? — Quasar-only, ignored by Lean elaborator
    -- op[12] = context? — Quasar-only, ignored by Lean elaborator

  for inv in invsStx.getArgs do
    let invName := inv[1].getId.toString (escape := false)
    let invDoc := s!"Invariant: {invName}."
    cmds := cmds.push (mkDocSorryTheorem invName #[] "True" invDoc)

  -- ================================================================
  -- Operation inductive + applyOp dispatcher
  -- Embeds per-operation parameters in constructors so the inductive
  -- theorem quantifies over all operations uniformly.
  -- ================================================================
  if opParamsMap.size > 0 then
    -- Build Operation inductive: each constructor carries its takes: params
    let mut ctors := ""
    for (opNameRaw, params) in opParamsMap do
      let ctorName := quoteName opNameRaw
      if params.isEmpty then
        ctors := ctors ++ s!" | {ctorName}"
      else
        let paramFields := params.foldl (fun acc (pn, pt) =>
          acc ++ s!" ({pn} : {mapType pt})") ""
        ctors := ctors ++ s!" | {ctorName}{paramFields}"
    cmds := cmds.push s!"inductive Operation where{ctors}\n  deriving Repr, DecidableEq, BEq"

    -- Build applyOp dispatcher: matches Operation constructors to transition functions
    let mut matchArms := ""
    for (opNameRaw, params) in opParamsMap do
      let ctorName := quoteName opNameRaw
      let transName := quoteName s!"{opNameRaw}Transition"
      let paramNames := params.foldl (fun acc (pn, _) => acc ++ s!" {pn}") ""
      let paramArgs := mkParamArgs params
      matchArms := matchArms ++ s!"\n  | .{ctorName}{paramNames} => {transName} s signer{paramArgs}"
    cmds := cmds.push (s!"def applyOp (s : State) (signer : Pubkey) : Operation → Option State" ++ matchArms)

  -- ================================================================
  -- Property predicates + inductive preservation theorems
  -- One sorry per property (not N×M per operation×property).
  -- The agent proves by `cases op` then unfold/omega per case.
  -- ================================================================
  for prop in propsStx.getArgs do
    let propName := prop[1].getId.toString (escape := false)
    let predBody := prop[2].isStrLit?.getD ""

    -- Validate field references in property predicate
    if !predBody.isEmpty then
      validateFieldRefs predBody fieldData s!"property '{propName}'"

    -- Generate predicate definition: def propName (s : State) : Prop := <body>
    cmds := cmds.push s!"def {propName} (s : State) : Prop := {predBody}"

    -- Generate inductive preservation theorem (one sorry for all operations)
    if opParamsMap.size > 0 then
      let pvBinders : Array String := #[
        "(s s' : State)", "(signer : Pubkey)", "(op : Operation)",
        s!"(h_inv : {propName} s)",
        "(h : applyOp s signer op = some s')"
      ]
      let pvDoc := s!"{propName} is preserved by every operation. Prove by `cases op` with unfold/omega per case."
      cmds := cmds.push (mkDocSorryTheorem s!"{propName}_inductive" pvBinders s!"{propName} s'" pvDoc)

  -- Emit assumptions summary as module doc
  if assumptions.size > 0 then
    let assumptionBody := assumptions.foldl (fun acc a => acc ++ s!"- {a}\n") ""
    cmds := cmds.push (mkModuleDoc s!"## Assumptions made by qedspec\n\n{assumptionBody}")

  cmds := cmds.push (mkEnd s!"{name}")

  -- Parse and elaborate each command
  let env ← getEnv
  for src in cmds do
    match Lean.Parser.runParserCategory env `command src "<qedspec>" with
    | .error msg =>
      throwError m!"qedspec: failed to parse generated code:\n{msg}\n\nSource:\n{src}"
    | .ok cmdStx =>
      elabCommand cmdStx

end QEDGen.Solana.SpecDSL
