import Lake
open Lake DSL

package escrowProofs

require qedgenSupport from
  "../../../../lean_solana"

require "leanprover-community" / "mathlib" @ git "v4.24.0"

@[default_target]
lean_lib EscrowProofs where
  roots := #[`EscrowProofs]
