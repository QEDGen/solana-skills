import Lake
open Lake DSL

-- Base lean_solana package. Pure Lean 4; no Mathlib dependency.
-- Covers Account, Cpi, State, Valid, SBPF helpers — everything that
-- doesn't need `Fin → α` / BigOperators reasoning.
--
-- The `IndexedState` module and anything else that needs Mathlib
-- lives in the sibling `lean_solana_mathlib/` package; programs that
-- need it depend on that one, which transitively pulls this.
package qedgenSupport

-- sBPF semantics + both binary-proof engines (SL/lift and WP/fuel)
-- come from qedsvm's frozen `SVM` surface (see qedsvm docs/API.md).
-- Pin an exact tag pre-1.0.
require qedsvm from git
  "https://github.com/QEDGen/qedsvm.git" @ "v0.10.1"

@[default_target]
lean_lib QEDGen where
  roots := #[`QEDGen]

