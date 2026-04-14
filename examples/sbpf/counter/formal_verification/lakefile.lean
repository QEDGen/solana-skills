import Lake
open Lake DSL

package counterProofs

require qedgenSupport from
  "../../../../lean_solana"

lean_lib CounterProg where
  roots := #[`Program]

@[default_target]
lean_lib CounterSpec where
  roots := #[`Spec]
