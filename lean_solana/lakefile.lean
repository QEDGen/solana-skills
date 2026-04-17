import Lake
open Lake DSL

package qedgenSupport

-- Mathlib is required for IndexedState (Fin / BigOperators / Finset).
-- The manifest already pins a version; lake will not refetch.
require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git"

@[default_target]
lean_lib QEDGen where
  roots := #[`QEDGen]

