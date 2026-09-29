import Lake
open Lake DSL

package guardProofs

require qedgenSupport from
  "../../../../../../lean_solana"

lean_lib GuardProg where
  roots := #[`Program]

@[default_target]
lean_lib GuardSpec where
  roots := #[`Spec]
