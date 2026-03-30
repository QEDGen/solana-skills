import Lake
open Lake DSL

package transferProofs

require qedgenSupport from
  "./lean_support"

lean_lib TransferProg where
  roots := #[`TransferProg]

@[default_target]
lean_lib TransferProofs where
  roots := #[`TransferProofs]
