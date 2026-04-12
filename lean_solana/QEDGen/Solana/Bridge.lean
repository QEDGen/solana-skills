import QEDGen.Solana.CommandBuilders
import QEDGen.Solana.Spec
import QEDGen.Solana.SBPF
import Lean.Elab.Command

/-!
# QEDGen Bridge DSL

Refinement bridge connecting qedspec (abstract transitions) to sBPF bytecode.
The `qedbridge` block generates:
  - Memory layout constants (byte offsets)
  - Status encoding/decoding (when lifecycle exists)
  - encodeState / decodeState (memory ↔ State)
  - Refinement theorem stubs (sorry) per operation
-/

open QEDGen.Solana

namespace QEDGen.Solana.BridgeDSL

-- ============================================================================
-- Syntax declarations
-- ============================================================================

/-- Layout field: `name Type at offset` — uses ident (not rawIdent)
    so the parser stops at section keywords like `operations`. -/
syntax bridgeField := ident rawIdent "at" num

/-- Status encoding map entry: `Variant value` -/
syntax bridgeStatusVariant := ident num

/-- Bridge operation parameter: `paramName Type` -/
syntax bridgeParam := ident rawIdent

/-- Operation with discriminator and optional parameters -/
syntax bridgeOp := ident "discriminator" num ("takes: " bridgeParam,*)?

/-- The top-level qedbridge command. -/
syntax (name := qedbridgeCmd)
  "qedbridge " ident " where"
    "input: " rawIdent
    ("insn: " rawIdent)?
    ("entry: " num)?
    "fuel: " num
    "layout" bridgeField*
    ("status_encoding" "at" num bridgeStatusVariant*)?
    ("operations" bridgeOp*)?
  : command

-- ============================================================================
-- Helpers
-- ============================================================================

open QEDGen.Solana.CommandBuilders in
private def quoteName := safeName

open QEDGen.Solana.CommandBuilders in
private def mapDslType := mapType

/-- Map DSL types to (encode read fn, decode fn). -/
private def typeReadFns (t : String) : String × String :=
  match t with
  | "U64"    => ("readU64", "readU64")
  | "U8"     => ("readU8", "readU8")
  | "Pubkey" => ("pubkeyAt", "readPubkey")
  | _        => ("readU64", "readU64")

-- ============================================================================
-- Elaborator
-- ============================================================================

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
open QEDGen.Solana.CommandBuilders in
@[command_elab qedbridgeCmd]
def elabQedbridge : CommandElab := fun stx => do
  let specNameStx := stx[1]
  let specName := specNameStx.getId.toString (escape := false)

  let _inputReg := stx[4].getId.toString (escape := false)

  -- Optional insn register (index 5)
  let insnStx := stx[5]
  let hasInsn := !insnStx.isMissing && insnStx.getNumArgs > 0

  -- Optional entry PC (index 6)
  let entryStx := stx[6]
  let entryPc := if !entryStx.isMissing && entryStx.getNumArgs > 0 then
    match entryStx[1].isNatLit? with
    | some n => n
    | none => 0
  else 0

  -- Fuel (index 8)
  let fuelVal := match stx[8].isNatLit? with
    | some n => n
    | none => 100

  -- Layout fields (index 10)
  let layoutStx := stx[10]
  let mut fields : Array (String × String × Nat) := #[]
  for f in layoutStx.getArgs do
    let fname := f[0].getId.toString (escape := false)
    let ftype := f[1].getId.toString (escape := false)
    let foffset := match f[3].isNatLit? with
      | some n => n
      | none => 0
    fields := fields.push (fname, ftype, foffset)

  -- Optional status_encoding (index 11): "status_encoding" "at" num bridgeStatusVariant*
  let statusEncStx := stx[11]
  let mut statusMappings : Array (String × Nat) := #[]
  let mut statusOffset : Nat := 0
  if !statusEncStx.isMissing && statusEncStx.getNumArgs > 0 then
    statusOffset := match statusEncStx[2].isNatLit? with
      | some n => n
      | none => 0
    let mappingsStx := statusEncStx[3]  -- bridgeStatusVariant*
    for m_ in mappingsStx.getArgs do
      let variant := m_[0].getId.toString (escape := false)
      let value := match m_[1].isNatLit? with
        | some n => n
        | none => 0
      statusMappings := statusMappings.push (variant, value)

  let hasStatusEncoding := statusMappings.size > 0

  -- Optional operations (index 12)
  let opsStx := stx[12]
  -- (opName, discriminator, params: [(name, dslType)])
  let mut opsList : Array (String × Nat × Array (String × String)) := #[]
  if !opsStx.isMissing && opsStx.getNumArgs > 0 then
    let opListStx := opsStx[1]
    for o in opListStx.getArgs do
      let opName := o[0].getId.toString (escape := false)
      let disc := match o[2].isNatLit? with
        | some n => n
        | none => 0
      -- Parse optional takes: clause (index 3)
      let takesStx := o[3]
      let mut params : Array (String × String) := #[]
      if !takesStx.isMissing && takesStx.getNumArgs > 0 then
        let paramsSepStx := takesStx[1]  -- bridgeParam,*
        for i in List.range paramsSepStx.getArgs.size do
          let arg := paramsSepStx.getArgs[i]!
          if i % 2 == 0 then  -- skip comma separators
            let pName := arg[0].getId.toString (escape := false)
            let pType := arg[1].getId.toString (escape := false)
            params := params.push (pName, pType)
      opsList := opsList.push (opName, disc, params)

  -- ================================================================
  -- Generate commands
  -- ================================================================
  let mut cmds : Array String := #[]
  let nl := "\n"

  cmds := cmds.push (mkNamespace s!"{specName}.Bridge")
  cmds := cmds.push (mkOpen "QEDGen.Solana")
  cmds := cmds.push (mkOpen "QEDGen.Solana.SBPF")
  cmds := cmds.push (mkOpen "QEDGen.Solana.SBPF.Memory")

  -- 1. Offset constants
  for (fname, _, foffset) in fields do
    let constName := fname.toUpper ++ "_OFF"
    cmds := cmds.push (mkSimpleDef constName "Nat" s!"{foffset}")

  -- 2. Fuel constant
  cmds := cmds.push (mkSimpleDef "FUEL" "Nat" s!"{fuelVal}")

  -- 3. Entry PC constant
  if entryPc != 0 then
    cmds := cmds.push (mkSimpleDef "ENTRY" "Nat" s!"{entryPc}")

  -- 4. Status offset + encoding/decoding
  if hasStatusEncoding then
    cmds := cmds.push (mkSimpleDef "STATUS_OFF" "Nat" s!"{statusOffset}")
    let mut encCases := ""
    let mut decCases := ""
    for (variant, value) in statusMappings do
      encCases := encCases ++ nl ++ s!"  | .{variant} => {value}"
      decCases := decCases ++ nl ++ s!"  | {value} => some .{variant}"
    decCases := decCases ++ nl ++ "  | _ => none"

    cmds := cmds.push (s!"def encodeStatus : {specName}.Status → Nat" ++ encCases)
    cmds := cmds.push (s!"def decodeStatus : Nat → Option {specName}.Status" ++ decCases)
    cmds := cmds.push (mkSorryTheorem "decode_encode_status"
      #[s!"(st : {specName}.Status)"]
      "decodeStatus (encodeStatus st) = some st")

  -- 5. encodeState
  let mut encConjuncts : Array String := #[]
  for (fname, ftype, foffset) in fields do
    let (readFn, _) := typeReadFns ftype
    let qName := quoteName fname
    if ftype == "Pubkey" then
      encConjuncts := encConjuncts.push s!"{readFn} mem (addr + {foffset}) s.{qName}"
    else
      encConjuncts := encConjuncts.push s!"{readFn} mem (addr + {foffset}) = s.{qName}"

  -- Add status encoding conjunct if lifecycle exists
  if hasStatusEncoding then
    encConjuncts := encConjuncts.push s!"readU8 mem (addr + {statusOffset}) = encodeStatus s.status"

  let encBody := if encConjuncts.size == 0 then "True"
    else encConjuncts.foldl (fun acc c =>
      if acc.isEmpty then s!"  {c}" else acc ++ " ∧" ++ nl ++ s!"  {c}") ""

  cmds := cmds.push (
    s!"def encodeState (s : {specName}.State) (addr : Nat) (mem : Mem) : Prop :=" ++ nl ++ encBody)

  -- 6. decodeState
  let mut decFields : Array String := #[]
  for (fname, ftype, foffset) in fields do
    let (_, decodeFn) := typeReadFns ftype
    let qName := quoteName fname
    decFields := decFields.push s!"{qName} := {decodeFn} mem (addr + {foffset})"

  -- Add status decode field if lifecycle exists
  if hasStatusEncoding then
    let firstVariant := statusMappings[0]!.1
    decFields := decFields.push s!"status := (decodeStatus (readU8 mem (addr + {statusOffset}))).getD .{firstVariant}"

  let lbrace := "{"
  let rbrace := "}"
  let decBody := String.intercalate (", " ++ nl ++ "    ") (decFields.toList)

  cmds := cmds.push (
    s!"def decodeState (addr : Nat) (mem : Mem) : {specName}.State :=" ++ nl ++
    s!"  {lbrace} {decBody} {rbrace}")

  -- 7. decode_encode round-trip theorem
  cmds := cmds.push (mkSorryTheorem "decode_encode"
    #[s!"(s : {specName}.State)", "(addr : Nat)", "(mem : Mem)",
      "(h : encodeState s addr mem)"]
    "decodeState addr mem = s")

  -- 8. Refinement theorem stubs per operation
  let entryStr := if entryPc != 0 then "ENTRY" else "0"
  let initFn := if hasInsn then "initState2" else "initState"

  for (opName, disc, params) in opsList do
    let qOp := quoteName opName
    let transName := quoteName (opName ++ "Transition")

    -- Build parameter signature and argument strings
    let paramSig := mkParamSig params
    let paramArgs := mkParamArgs params

    let mut hyps := ""
    hyps := hyps ++ s!"    (h_encode : encodeState s inputAddr mem)" ++ nl
    if hasInsn then
      hyps := hyps ++ s!"    (h_disc : readU8 mem insnAddr = {disc})" ++ nl

    let initExpr := if hasInsn then
      s!"{initFn} inputAddr insnAddr mem {entryStr}"
    else
      s!"{initFn} inputAddr mem"

    let addrParams := if hasInsn then
      "(inputAddr insnAddr : Nat)"
    else
      "(inputAddr : Nat)"

    -- Success: guards hold → exits 0 → final memory encodes updated state
    -- Uses (s' : State) + hypothesis instead of .get! to avoid Inhabited requirement
    cmds := cmds.push (
      s!"theorem {qOp}.refines (progAt : Nat → Option QEDGen.Solana.SBPF.Insn)" ++ nl ++
      s!"    {addrParams} (mem : Mem) (s s' : {specName}.State) (signer : Pubkey){paramSig}" ++ nl ++
      hyps ++
      s!"    (h_guard : {transName} s signer{paramArgs} = some s') :" ++ nl ++
      s!"    let result := executeFn progAt ({initExpr}) FUEL" ++ nl ++
      s!"    result.exitCode = some 0 ∧" ++ nl ++
      s!"    encodeState s' inputAddr result.mem := sorry")

    -- Rejection: guards fail → exits nonzero
    cmds := cmds.push (
      s!"theorem {qOp}.rejects (progAt : Nat → Option QEDGen.Solana.SBPF.Insn)" ++ nl ++
      s!"    {addrParams} (mem : Mem) (s : {specName}.State) (signer : Pubkey){paramSig}" ++ nl ++
      hyps ++
      s!"    (h_fail : {transName} s signer{paramArgs} = none) :" ++ nl ++
      s!"    (executeFn progAt ({initExpr}) FUEL).exitCode ≠ some 0 := sorry")

  cmds := cmds.push (mkEnd s!"{specName}.Bridge")

  -- Parse and elaborate each command
  let env ← getEnv
  for src in cmds do
    match Lean.Parser.runParserCategory env `command src "<qedbridge>" with
    | .error msg =>
      throwError m!"qedbridge: failed to parse generated code:{nl}{msg}{nl}{nl}Source:{nl}{src}"
    | .ok cmdStx =>
      elabCommand cmdStx

end QEDGen.Solana.BridgeDSL
