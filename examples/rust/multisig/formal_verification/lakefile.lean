import Lake
open Lake DSL

package multisigProofs

require qedgenSupport from
  "../../../../lean_solana"

@[default_target]
lean_lib MultisigSpec where
  roots := #[`Spec]
