import Lake
open Lake DSL

package escrow_splitProofs

require qedgenSupport from
  "./lean_solana"

@[default_target]
lean_lib Escrow_splitSpec where
  roots := #[`Spec]
