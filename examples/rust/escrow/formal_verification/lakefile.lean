import Lake
open Lake DSL

package escrowProofs

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git" @ "v4.15.0"
require qedgenSupport from
  "./lean_support"

@[default_target]
lean_lib EscrowProofs where
  roots := #[`EscrowProofs]
