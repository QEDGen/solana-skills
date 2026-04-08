import Lake
open Lake DSL

package dropsetProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib DropsetProg where
  roots := #[`DropsetProg]

@[default_target]
lean_lib DropsetProofs where
  roots := #[`DropsetProofs]
