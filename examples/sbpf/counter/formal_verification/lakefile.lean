import Lake
open Lake DSL

package counterProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib CounterProg where
  roots := #[`CounterProg]

lean_lib CounterSpec where
  roots := #[`CounterSpec]

@[default_target]
lean_lib CounterProofs where
  roots := #[`CounterProofs]
