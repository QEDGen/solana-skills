import Lake
open Lake DSL

package treeProofs

require qedgenSupport from
  "./lean_support"

lean_lib TreeProg where
  roots := #[`TreeProg]

@[default_target]
lean_lib TreeProofs where
  roots := #[`TreeProofs]
