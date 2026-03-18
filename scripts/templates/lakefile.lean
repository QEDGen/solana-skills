import Lake
open Lake DSL

package leanstralProof

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git" @ "v4.15.0"
require leanstralSupport from
  "./lean_support"

@[default_target]
lean_lib Best where
  roots := #[`Best]
