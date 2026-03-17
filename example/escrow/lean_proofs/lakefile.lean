import Lake
open Lake DSL

package «leanstral-proof»

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git"

@[default_target]
lean_lib «Best» where
  roots := #[`Best]
