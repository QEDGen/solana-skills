import Lake
open Lake DSL

package escrowProofs

require qedgenSupport from
  "../../../../lean_solana"

@[default_target]
lean_lib EscrowProofs where
  roots := #[`EscrowProofs]
