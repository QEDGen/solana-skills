import Lake
open Lake DSL

package dropsetProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib DropsetProg where
  roots := #[`Program]

@[default_target]
lean_lib DropsetSpec where
  roots := #[`Spec]
