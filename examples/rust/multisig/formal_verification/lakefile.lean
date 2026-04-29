import Lake
open Lake DSL

package multisigProofs

-- Spec.lean uses Map-backed forall predicates over `Map[MAX_MEMBERS] _`,
-- which require `Mathlib.Algebra.BigOperators.Fin` + `QEDGenMathlib.IndexedState`.
-- Pull the Mathlib-extended slice (transitively requires the base).
require qedgenSupportMathlib from
  "../../../../lean_solana_mathlib"

@[default_target]
lean_lib MultisigSpec where
  roots := #[`Spec]
