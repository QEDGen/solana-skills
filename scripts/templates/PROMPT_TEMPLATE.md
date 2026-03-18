I need a single Lean 4 module that compiles under Lean 4.15 + Mathlib 4.15.

Return Lean code only.
Do not duplicate declarations.
Do not leave theorem bodies empty after `:= by`.
Do not invent helper APIs or namespaces unless you define them in the file.
If a proof is incomplete, use `sorry` inside the proof body.
Prefer a smaller explicit model that compiles over a larger broken one.

## Source Code
<paste the relevant code here>

## Property to Prove
<state exactly one property, or a very small set of closely related properties>

## Context
<account model, invariants, semantic assumptions, or proof boundaries>

## Output Requirements
1. Define the model types and executable transition functions first
2. State the theorem only after the semantics are defined
3. Use only Lean 4.15 / Mathlib 4.15 identifiers you are confident exist
4. Prefer concrete definitions over placeholders
5. If the full request is too large, prove a sound subset cleanly instead of emitting broken code
