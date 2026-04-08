import Lake
open Lake DSL

package qedgenProof

require qedgenSupport from
  "./lean_solana"

@[default_target]
lean_lib Best where
  roots := #[`Best]
  moreLeanArgs := #["-DwarningAsError=false"]
