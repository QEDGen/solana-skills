import Lake
open Lake DSL

package transferProofs

require qedgenSupport from
  "./lean_support"

@[default_target]
lean_lib TransferProofs where
  roots := #[`TransferProofs]
