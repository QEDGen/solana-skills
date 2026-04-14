import Lake
open Lake DSL

package slippageProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib SlippageProg where
  roots := #[`Program]

@[default_target]
lean_lib SlippageSpec where
  roots := #[`Spec]
