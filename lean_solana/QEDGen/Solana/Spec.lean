import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

/-!
# QEDGen Spec DSL (v1.5.0 — initial scaffold)

Declarative specification macros for Solana program verification.
The `qedspec` block is the source of truth — it expands to:
  - Theorem signatures with sorry (one per operation × property)
  - Transition function stubs (agent fills these)

Humans write and approve the spec. Agents fill the sorry markers.
`lake build` enforces that every declared property has a proof.

## Current status
First iteration. The syntax will evolve through prototyping
against real Anchor programs (escrow, AMMs, vaults).
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
-- Macro expansion
-- ============================================================================

open Lean in
macro_rules
  | `(command| qedspec $progName where
        state $[$fields:specField]*
        $[$ops:specOp]*
        $[$invs:specInvariant]*) => do
    let name := progName.getId
    let ns := mkIdent name

    -- Collect field info for documentation
    let mut fieldStrs : Array String := #[]
    for f in fields do
      match f with
      | `(specField| $fn : $ft) =>
        fieldStrs := fieldStrs.push s!"{fn.getId} : {ft.getId}"
      | _ => Macro.throwError "invalid field declaration"

    -- Generate per-operation theorem stubs
    let mut opCmds : Array Syntax := #[]
    for op in ops do
      match op with
      | `(specOp| operation $opName
            who: $signer
            when: $preStatus
            then: $postStatus) => do
        -- Access control theorem stub
        let acName := mkIdent (opName.getId ++ `access_control)
        let acDoc := s!"Only {signer.getId} can execute {opName.getId}. \
                        Requires {preStatus.getId} → {postStatus.getId}."
        let acCmd ← `(
          /-- $(Lean.mkDocStringFromStr acDoc) -/
          theorem $acName : True := sorry)
        opCmds := opCmds.push acCmd

        -- State machine theorem stub
        let smName := mkIdent (opName.getId ++ `state_machine)
        let smDoc := s!"{opName.getId} transitions from {preStatus.getId} to {postStatus.getId}."
        let smCmd ← `(
          /-- $(Lean.mkDocStringFromStr smDoc) -/
          theorem $smName : True := sorry)
        opCmds := opCmds.push smCmd
      | _ => Macro.throwError "invalid operation declaration"

    -- Generate invariant theorem stubs
    let mut invCmds : Array Syntax := #[]
    for inv in invs do
      match inv with
      | `(specInvariant| invariant $invName $desc) => do
        let invCmd ← `(
          theorem $invName : True := sorry)
        invCmds := invCmds.push invCmd
      | _ => Macro.throwError "invalid invariant declaration"

    let nsOpen ← `(command| namespace $ns)
    let nsClose ← `(command| end $ns)
    let allCmds := #[nsOpen] ++ opCmds ++ invCmds ++ #[nsClose]
    return Lean.mkNullNode allCmds

end QEDGen.Solana.SpecDSL
