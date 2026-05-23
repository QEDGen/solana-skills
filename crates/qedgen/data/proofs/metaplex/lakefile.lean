-- QEDGen bundled proof package — Metaplex Token Metadata (Stance 2 provider).
-- See spl/lakefile.lean for the overall mechanism.

import Lake
open Lake DSL

package metadataProofs

@[default_target]
lean_lib Metadata where
  roots := #[`Metadata]
