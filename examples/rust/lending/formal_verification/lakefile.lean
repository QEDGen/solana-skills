import Lake
open Lake DSL

package lendingProofs

require qedgenSupport from
  "../../../../lean_solana"

@[default_target]
lean_lib LendingSpec where
  roots := #[`Spec]
