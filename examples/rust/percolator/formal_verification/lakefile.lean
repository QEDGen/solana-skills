import Lake
open Lake DSL

package qedgenPercolatorProofs

require qedgenSupport from
  "../../../../lean_solana"

require "leanprover-community" / "mathlib" @ git "v4.24.0"

@[default_target]
lean_lib PercolatorProofs where
  roots := #[`PercolatorProofs]
