import Lake
open Lake DSL

package transferProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib TransferProg where
  roots := #[`TransferProg]

lean_lib TransferSpec where
  roots := #[`TransferSpec]

@[default_target]
lean_lib TransferProofs where
  roots := #[`TransferProofs]
