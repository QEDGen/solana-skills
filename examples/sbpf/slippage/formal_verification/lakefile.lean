import Lake
open Lake DSL

package slippageProofs

require qedgenSupport from
  "./lean_support"

lean_lib SlippageProg where
  roots := #[`SlippageProg]

@[default_target]
lean_lib SlippageProofs where
  roots := #[`SlippageProofs]
