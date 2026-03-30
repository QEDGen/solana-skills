import Lake
open Lake DSL

package slippageProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib SlippageProg where
  roots := #[`SlippageProg]

@[default_target]
lean_lib SlippageProofs where
  roots := #[`SlippageProofs]
