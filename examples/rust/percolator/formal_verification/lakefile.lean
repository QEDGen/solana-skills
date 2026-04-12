import Lake
open Lake DSL

package qedgenPercolatorProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib PercolatorSpec where
  roots := #[`Spec]

@[default_target]
lean_lib PercolatorProofs where
  roots := #[`PercolatorProofs]
