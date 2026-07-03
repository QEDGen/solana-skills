import QEDGen.Solana.CommandBuilders
import QEDGen.Solana.Spec
import SVM.SBPF
import QEDGen.Solana.BridgeAdapter
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

/-- Map a DSL type to its `SVM.SBPF.FieldVal` constructor (dot form), for the
    `codecCoarse` field list the discharge (`AsmRefinesFieldUpdate`) carries. -/
private def fieldValCtor (t : String) : String :=
  match t with
  | "U64"    => ".u64"
  | "U8"     => ".byte"
  | "Pubkey" => ".pubkey"
  | _        => ".u64"

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
  cmds := cmds.push (mkOpen "SVM.SBPF")
  cmds := cmds.push (mkOpen "SVM.SBPF.Memory")
  cmds := cmds.push (mkOpen "SVM.Solana.Abstract")          -- AsmRefinesFieldUpdate
  cmds := cmds.push (mkOpen "QEDGen.Solana.BridgeAdapter")  -- halts_zero_of_fieldUpdate

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
  -- Abstract entry pc = the init state's `pc`: `initState2` honours the entry
  -- arg, `initState` is fixed at 0.
  let entryArg := if hasInsn then entryStr else "0"

  -- `codecCoarse` field list from the account layout (U64 → .u64, U8 → .byte,
  -- Pubkey → .pubkey); the status byte (if any) appends as `.byte (encodeStatus
  -- …)`, mirroring `encodeState`. Layout-derived, so identical for every op —
  -- pre uses `s`, post uses `s'`.
  let mkFieldList := fun (subj : String) =>
    let core := fields.foldl (fun (acc : String) (f : String × String × Nat) =>
      let (fname, ftype, foffset) := f
      let entry := s!"({foffset}, {fieldValCtor ftype} {subj}.{quoteName fname})"
      if acc.isEmpty then entry else acc ++ ", " ++ entry) ""
    let full := if hasStatusEncoding then
        (if core.isEmpty then "" else core ++ ", ")
          ++ s!"({statusOffset}, .byte (encodeStatus {subj}.status))"
      else core
    "[" ++ full ++ "]"
  let preFields := mkFieldList "s"
  let postFields := mkFieldList "s'"

  -- Per-field state-validity bound hyps + the post-leg proof (also layout-
  -- derived / op-independent). The forward codec bridges in `CodecRead.lean`
  -- normalize (`readU64 = v % 2^64`, `readU8 = v % 256`), so each field carries
  -- a `< width` bound — from the spec's `Valid s'` — to land encodeState's raw
  -- read. The proof peels `setupPost` (`holdsFor_sepConj_right`), extracts each
  -- field's coarse atom (`holdsFor_codecCoarse_field`), and bridges it; one
  -- bullet per encodeState conjunct, in layout-then-status order. Mirrors the
  -- validated `RefinesShape.increment_refines`.
  let mut boundHyps : String := ""
  let mut bullets : String := ""
  for (fname, ftype, foffset) in fields do
    let q := quoteName fname
    match ftype with
    | "Pubkey" =>
      boundHyps := boundHyps ++
        s!"    (hb_{fname}_0 : s'.{q}.c0 < 2 ^ 64) (hb_{fname}_1 : s'.{q}.c1 < 2 ^ 64)" ++ nl ++
        s!"    (hb_{fname}_2 : s'.{q}.c2 < 2 ^ 64) (hb_{fname}_3 : s'.{q}.c3 < 2 ^ 64)" ++ nl
      bullets := bullets ++
        s!"  · have hc := holdsFor_codecCoarse_field _ hcodec (show ({foffset}, FieldVal.pubkey s'.{q}) ∈ _ by simp)" ++ nl ++
        s!"    simp only [FieldVal.coarse] at hc" ++ nl ++
        s!"    exact pubkeyAt_of_holdsFor_pubkeyIs hb_{fname}_0 hb_{fname}_1 hb_{fname}_2 hb_{fname}_3 hc" ++ nl
    | "U8" =>
      boundHyps := boundHyps ++ s!"    (hb_{fname} : s'.{q} < 256)" ++ nl
      bullets := bullets ++
        s!"  · have hc := holdsFor_codecCoarse_field _ hcodec (show ({foffset}, FieldVal.byte s'.{q}) ∈ _ by simp)" ++ nl ++
        s!"    simp only [FieldVal.coarse] at hc" ++ nl ++
        s!"    rw [readU8_of_holdsFor_memByteIs hc, Nat.mod_eq_of_lt hb_{fname}]" ++ nl
    | _ =>
      boundHyps := boundHyps ++ s!"    (hb_{fname} : s'.{q} < 2 ^ 64)" ++ nl
      bullets := bullets ++
        s!"  · have hc := holdsFor_codecCoarse_field _ hcodec (show ({foffset}, FieldVal.u64 s'.{q}) ∈ _ by simp)" ++ nl ++
        s!"    simp only [FieldVal.coarse] at hc" ++ nl ++
        s!"    rw [readU64_of_holdsFor_memU64Is hc, Nat.mod_eq_of_lt hb_{fname}]" ++ nl
  if hasStatusEncoding then
    boundHyps := boundHyps ++ s!"    (hb_status : encodeStatus s'.status < 256)" ++ nl
    bullets := bullets ++
      s!"  · have hc := holdsFor_codecCoarse_field _ hcodec (show ({statusOffset}, FieldVal.byte (encodeStatus s'.status)) ∈ _ by simp)" ++ nl ++
      s!"    simp only [FieldVal.coarse] at hc" ++ nl ++
      s!"    rw [readU8_of_holdsFor_memByteIs hc, Nat.mod_eq_of_lt hb_status]" ++ nl
  let nConj := fields.size + (if hasStatusEncoding then 1 else 0)
  let postLeg : String :=
    if nConj == 0 then
      s!"  unfold encodeState" ++ nl ++ "  trivial"
    else
      let refineLine := if nConj ≥ 2 then
          s!"  refine ⟨" ++ String.intercalate ", " (List.replicate nConj "?_") ++ "⟩" ++ nl
        else ""
      s!"  have hcodec := holdsFor_sepConj_right h_post" ++ nl ++
      s!"  unfold encodeState" ++ nl ++ refineLine ++ bullets

  for (opName, disc, params) in opsList do
    let qOp := quoteName opName
    let transName := quoteName (opName ++ "Transition")

    -- Build parameter signature and argument strings
    let paramSig := mkParamSig params
    let paramArgs := mkParamArgs params

    let mut hyps := ""
    hyps := hyps ++ s!"    (_h_encode : encodeState s inputAddr mem)" ++ nl
    if hasInsn then
      hyps := hyps ++ s!"    (_h_disc : readU8 mem insnAddr = {disc})" ++ nl

    let initExpr := if hasInsn then
      s!"{initFn} inputAddr insnAddr mem rt {entryStr}"
    else
      s!"{initFn} inputAddr mem rt"

    let addrParams := if hasInsn then
      "(inputAddr insnAddr : Nat) (rt : RegionTable)"
    else
      "(inputAddr : Nat) (rt : RegionTable)"

    -- Success: guards hold → exits 0 → final memory encodes updated state.
    -- The discharge (`AsmRefinesFieldUpdate`) + the program constraint
    -- (`cr.SatisfiedBy`) are threaded as hypotheses (`h_asm`, `h_prog`), so the
    -- theorem is provable via the execution adapter — vs the old free-`progAt`
    -- statement, which asserted refinement for *any* program (only `sorry`-true).
    -- The body closes through `BridgeAdapter.halts_zero_of_fieldUpdate`; the sole
    -- remaining `sorry` is the post `codecCoarse → encodeState` read-back
    -- (qedsvm#48). Mirrors the validated `RefinesShape.increment_refines`.
    cmds := cmds.push (
      s!"theorem {qOp}.refines" ++ nl ++
      s!"    (progAt : Nat → Option SVM.SBPF.Insn) (cr : CodeReq) (rr : RegionTable → Prop)" ++ nl ++
      s!"    (nSteps nCu exitPc : Nat) (setupPre setupPost : Assertion)" ++ nl ++
      s!"    {addrParams} (mem : Mem)" ++ nl ++
      s!"    (s s' : {specName}.State) (signer : Pubkey){paramSig}" ++ nl ++
      s!"    (h_prog : cr.SatisfiedBy progAt)" ++ nl ++
      s!"    (h_exit : progAt exitPc = some .exit)" ++ nl ++
      hyps ++
      s!"    (_h_guard : {transName} s signer{paramArgs} = some s')" ++ nl ++
      s!"    (h_asm : AsmRefinesFieldUpdate cr nSteps nCu {entryArg} exitPc rr inputAddr" ++ nl ++
      s!"              {preFields}" ++ nl ++
      s!"              {postFields}" ++ nl ++
      s!"              setupPre setupPost)" ++ nl ++
      s!"    (h_pre : (setupPre ** codecCoarse inputAddr" ++ nl ++
      s!"              {preFields}).holdsFor ({initExpr}))" ++ nl ++
      s!"    (h_cs : ∀ k : Nat, (executeFn progAt ({initExpr}) k).callStack = [])" ++ nl ++
      s!"    (h_r0 : ∀ t : SVM.SBPF.State," ++ nl ++
      s!"      (setupPost ** codecCoarse inputAddr" ++ nl ++
      s!"        {postFields}).holdsFor t →" ++ nl ++
      s!"      t.regs.get .r0 = 0)" ++ nl ++
      s!"    (h_fuel : nSteps + 1 ≤ FUEL)" ++ nl ++
      s!"    (h_bud : ({initExpr}).cuConsumed + nSteps + nCu" ++ nl ++
      s!"              ≤ ({initExpr}).cuBudget)" ++ nl ++
      s!"    (h_rr : rr ({initExpr}).regions)" ++ nl ++
      boundHyps ++
      s!"    :" ++ nl ++
      s!"    (executeFn progAt ({initExpr}) FUEL).exitCode = some 0 ∧" ++ nl ++
      s!"    encodeState s' inputAddr (executeFn progAt ({initExpr}) FUEL).mem := by" ++ nl ++
      s!"  have hpc : ({initExpr}).pc = {entryArg} := by simp [{initFn}]" ++ nl ++
      s!"  have hrun : ({initExpr}).exitCode = none := by simp [{initFn}]" ++ nl ++
      s!"  obtain ⟨h_halt, h_post⟩ :=" ++ nl ++
      s!"    halts_zero_of_fieldUpdate h_asm h_prog h_exit h_pre hpc hrun h_bud h_rr h_r0 h_cs FUEL h_fuel" ++ nl ++
      s!"  refine ⟨h_halt, ?_⟩" ++ nl ++
      postLeg)

    -- Rejection: guards fail → the run faults with a typed error, so it can
    -- never exit 0. Like `.refines` (A2b), the discharge is threaded as a
    -- hypothesis — `h_asm : AsmRefinesTransitionFault …` is one FAULT path of
    -- the qedsvm#40 whole-transition bundle (`qedgen discharge --transition`),
    -- under this path's guard hypotheses carried by `setupPre` — vs the old
    -- free-`progAt` statement (any failing state, ANY program → nonzero),
    -- which was only `sorry`-true. The body closes sorry-free through
    -- `BridgeAdapter.faults_of_transitionFault` + `toSentinel_ne_zero`.
    cmds := cmds.push (
      s!"theorem {qOp}.rejects" ++ nl ++
      s!"    (progAt : Nat → Option SVM.SBPF.Insn) (cr : CodeReq) (rr : RegionTable → Prop)" ++ nl ++
      s!"    (nSteps nCu : Nat) (e : SVM.SBPF.VmError)" ++ nl ++
      s!"    (accts : List AccountFields) (setupPre : Assertion)" ++ nl ++
      s!"    {addrParams} (mem : Mem) (s : {specName}.State) (signer : Pubkey){paramSig}" ++ nl ++
      s!"    (h_prog : cr.SatisfiedBy progAt)" ++ nl ++
      hyps ++
      s!"    (_h_fail : {transName} s signer{paramArgs} = none)" ++ nl ++
      s!"    (h_asm : AsmRefinesTransitionFault cr nSteps nCu {entryArg} rr e accts setupPre)" ++ nl ++
      s!"    (h_pre : (setupPre ** codecsPre accts).holdsFor ({initExpr}))" ++ nl ++
      s!"    (h_bud : ({initExpr}).cuConsumed + nSteps + nCu" ++ nl ++
      s!"              ≤ ({initExpr}).cuBudget)" ++ nl ++
      s!"    (h_rr : rr ({initExpr}).regions)" ++ nl ++
      s!"    (h_fuel : nSteps ≤ FUEL) :" ++ nl ++
      s!"    (executeFn progAt ({initExpr}) FUEL).exitCode ≠ some 0 := by" ++ nl ++
      s!"  have hpc : ({initExpr}).pc = {entryArg} := by simp [{initFn}]" ++ nl ++
      s!"  have hrun : ({initExpr}).exitCode = none := by simp [{initFn}]" ++ nl ++
      s!"  obtain ⟨h_code, _⟩ :=" ++ nl ++
      s!"    faults_of_transitionFault h_asm h_prog h_pre hpc hrun h_bud h_rr FUEL h_fuel" ++ nl ++
      s!"  intro h_zero" ++ nl ++
      s!"  rw [h_code] at h_zero" ++ nl ++
      s!"  exact toSentinel_ne_zero e (Option.some.inj h_zero)")

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
