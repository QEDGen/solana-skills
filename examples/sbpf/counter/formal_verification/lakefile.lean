import Lake
open Lake DSL

package counterProofs

require qedgenSupport from
  "./lean_support"

lean_lib CounterProg where
  roots := #[`CounterProg]

@[default_target]
lean_lib CounterProofs where
  roots := #[`CounterProofs]
