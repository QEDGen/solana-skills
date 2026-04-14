import Lake
open Lake DSL

package qedgenPercolatorProofs

require qedgenSupport from
  "../../../../lean_solana"

@[default_target]
lean_lib PercolatorSpec where
  roots := #[`Spec]
