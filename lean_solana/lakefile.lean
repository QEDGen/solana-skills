import Lake
open Lake DSL

package qedgenSupport

require "verse-lab" / "Loom" @ git "master"

@[default_target]
lean_lib QEDGen where
  roots := #[`QEDGen]
