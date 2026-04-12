import Lean.Elab.Command
/-!
# Compile-Time Verification Check

The `#qedgen_verify` command checks that all theorems in a given namespace
are sorry-free. Place it at the end of a proof file to get a compile-time
error if any proof obligations remain unfilled.

```lean
import Spec
import Proofs

#qedgen_verify MyProgram
-- Fails to compile if any theorem in MyProgram.* uses sorry
```
-/

namespace QEDGen.Solana.Verify

/-- Check if `name` is in namespace `ns` or any sub-namespace of it. -/
private def inNamespace (name ns : Lean.Name) : Bool :=
  match name with
  | .str parent _ => parent == ns || inNamespace parent ns
  | .num parent _ => parent == ns || inNamespace parent ns
  | .anonymous => false

/-- Check if an expression references `sorryAx` (how `sorry` is elaborated). -/
private def usesSorry (e : Lean.Expr) : Bool :=
  e.foldConsts false fun c found => found || c == ``sorryAx

end QEDGen.Solana.Verify

open Lean in
open Lean.Elab in
open Lean.Elab.Command in
open QEDGen.Solana.Verify in

/-- `#qedgen_verify Namespace` — compile-time check that all theorems
    in the given namespace (and its children) are sorry-free.
    Produces a compile error listing any theorems that still use sorry. -/
elab "#qedgen_verify " ns:ident : command => do
  let nsName := ns.getId
  let env ← getEnv

  let mut sorryNames : Array Name := #[]
  -- Check both map₁ (imported) and map₂ (current file)
  let checkConst := fun (name : Name) (ci : ConstantInfo) (acc : Array Name) => do
    if inNamespace name nsName then
      match ci.value? with
      | some val => if usesSorry val then return acc.push name
      | none => pure ()
    return acc
  for (name, ci) in env.constants.map₁.toList do
    sorryNames ← checkConst name ci sorryNames
  for (name, ci) in env.constants.map₂.toList do
    sorryNames ← checkConst name ci sorryNames

  if sorryNames.size > 0 then
    let sorted := sorryNames.qsort (Name.lt · ·)
    let nameList := sorted.foldl (fun acc n =>
      acc ++ s!"\n  - {n}") ""
    throwError m!"#qedgen_verify {nsName}: {sorted.size} theorem(s) still use sorry:{nameList}"
  else
    logInfo m!"#qedgen_verify {nsName}: all theorems verified (sorry-free)"
