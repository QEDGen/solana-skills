import Lake
open Lake DSL

package qedgenPercolatorProofs

require qedgenSupport from
  "../../../../lean_solana"

@[default_target]
lean_lib PercolatorProofs where
  roots := #[`PercolatorProofs]
