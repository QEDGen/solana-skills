import Lake
open Lake DSL

package slippageProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib SlippageProg where
  roots := #[`SlippageProg]

lean_lib SlippageSpec where
  roots := #[`SlippageSpec]

@[default_target]
lean_lib SlippageProofs where
  roots := #[`SlippageProofs]
