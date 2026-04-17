import Lake
open Lake DSL

package qedgenPercolatorProofs

-- Requires the Mathlib-extended slice (transitively pulls the base).
require qedgenSupportMathlib from
  "../../../../lean_solana_mathlib"

@[default_target]
lean_lib PercolatorSpec where
  roots := #[`Spec, `Proofs]
