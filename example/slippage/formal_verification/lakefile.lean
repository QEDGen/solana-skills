import Lake
open Lake DSL

package slippageProofs

require qedgenSupport from
  "./lean_support"

@[default_target]
lean_lib SlippageProofs where
  roots := #[`SlippageProofs]
