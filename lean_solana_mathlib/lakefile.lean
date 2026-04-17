import Lake
open Lake DSL

-- Mathlib-dependent slice of lean_solana. Split out so that programs
-- that don't need `Fin → α` / `BigOperators` reasoning (most of them,
-- including all sBPF proofs) can use the base `lean_solana` package
-- without paying the Mathlib download + build cost.
--
-- Percolator (the one example that does per-account sum reasoning)
-- is the sole consumer today; add new per-account DeFi specs here too.
package qedgenSupportMathlib

require qedgenSupport from "../lean_solana"

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git"

@[default_target]
lean_lib QEDGenMathlib where
  roots := #[`QEDGenMathlib.IndexedState]
