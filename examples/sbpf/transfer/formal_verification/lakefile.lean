import Lake
open Lake DSL

package transferProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib TransferProg where
  roots := #[`Program]

@[default_target]
lean_lib TransferSpec where
  roots := #[`Spec]
